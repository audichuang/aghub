use aghub_core::{
	errors::ConfigError, load_all_agents, models::McpServer, transfer,
};
use rocket::http::Status;
use rocket::serde::json::Json;

use crate::{
	dto::mcp::{
		AgentBatchResponse, BatchCreateMcpRequest, CreateMcpRequest,
		McpResponse, UpdateMcpRequest,
	},
	dto::skill::DeleteSkillByPathResponse,
	dto::transfer::{
		OperationBatchResponse, ReconcileRequest, TransferRequest,
	},
	error::{ApiCreated, ApiError, ApiResult},
	extractors::{AgentParam, ScopeParams, TrustedLocalOrigin},
	routes::{
		build_manager_from_resolved, require_writable_scope,
		resolved_to_resource_scope,
	},
};

fn check_mcp_supported(
	agent: &AgentParam,
	scope: aghub_core::models::ResourceScope,
) -> Result<(), ApiError> {
	let descriptor = aghub_core::registry::get(agent.0);
	if !descriptor.supports_mcp_scope(scope) {
		return Err(ApiError::new(
			Status::UnprocessableEntity,
			format!(
				"Agent '{}' does not support MCP servers in {:?} scope",
				descriptor.id, scope
			),
			"UNSUPPORTED_OPERATION",
		));
	}
	Ok(())
}

#[get("/agents/<agent>/mcps?<scope..>")]
pub fn list_mcps(
	_origin: TrustedLocalOrigin,
	agent: AgentParam,
	scope: ScopeParams,
) -> ApiResult<Vec<McpResponse>> {
	let resolved = scope.resolve()?;
	let (resource_scope, _) = resolved_to_resource_scope(&resolved);
	check_mcp_supported(&agent, resource_scope)?;
	let mut manager = build_manager_from_resolved(&agent, &resolved)?;

	if resolved.is_all() {
		let (_, mcps, _) =
			manager.load_both_annotated().map_err(ApiError::from)?;
		let items = mcps.iter().map(McpResponse::from).collect();
		return Ok(Json(items));
	}

	let config = manager.load().map_err(ApiError::from)?;
	let mcps = config.mcps.iter().map(McpResponse::from).collect();
	Ok(Json(mcps))
}

#[post("/mcps/transfer", data = "<body>")]
pub fn transfer_mcp_route(
	_origin: TrustedLocalOrigin,
	body: Json<TransferRequest>,
) -> ApiResult<OperationBatchResponse> {
	let req = body.into_inner();
	let source = req.source.to_core()?;
	let destinations = req
		.destinations
		.iter()
		.map(|target| target.to_core())
		.collect::<Result<Vec<_>, _>>()?;
	let result =
		transfer::transfer_mcp(source, destinations).map_err(ApiError::from)?;
	Ok(Json(result.into()))
}

#[post("/mcps/reconcile", data = "<body>")]
pub fn reconcile_mcp_route(
	_origin: TrustedLocalOrigin,
	body: Json<ReconcileRequest>,
) -> ApiResult<OperationBatchResponse> {
	let req = body.into_inner();
	// Read the gate BEFORE the vec fields below move out of `req`.
	let confirm = req.confirmed();
	let source = req.source.to_core()?;

	let added: Vec<_> = req
		.added
		.unwrap_or_default()
		.iter()
		.map(|agent_str| {
			agent_str.parse().map_err(|_| {
				ApiError::new(
					rocket::http::Status::BadRequest,
					format!("Unknown agent '{agent_str}'"),
					"INVALID_PARAM",
				)
			})
		})
		.collect::<Result<Vec<_>, _>>()?;

	let removed: Vec<_> = req
		.removed
		.unwrap_or_default()
		.iter()
		.map(|agent_str| {
			agent_str.parse().map_err(|_| {
				ApiError::new(
					rocket::http::Status::BadRequest,
					format!("Unknown agent '{agent_str}'"),
					"INVALID_PARAM",
				)
			})
		})
		.collect::<Result<Vec<_>, _>>()?;

	let result = transfer::reconcile_mcp(source, added, removed, confirm)
		.map_err(ApiError::from)?;
	Ok(Json(result.into()))
}

/// The single-agent create MUTATION, shared by the per-agent route and the
/// batch route so the two write paths cannot drift. Capability is the
/// CALLER's contract: the per-agent route checks `check_mcp_supported`
/// first (preserving its error precedence: capability → validate →
/// writable), and the batch route preflights every agent via
/// `aghub_core::batch::run_mcp_agent_mutation` before any write.
fn create_mcp_for_agent(
	agent: &AgentParam,
	resolved: &crate::extractors::ResolvedScope,
	req: CreateMcpRequest,
) -> Result<McpResponse, ApiError> {
	let mut manager = build_manager_from_resolved(agent, resolved)?;
	match manager.load() {
		Ok(_) => {}
		Err(ConfigError::NotFound { .. }) => manager.init_empty_config(),
		Err(e) => return Err(ApiError::from(e)),
	}
	let mcp = McpServer::from(req);
	let response = McpResponse::from(&mcp);
	manager.add_mcp(mcp).map_err(ApiError::from)?;
	Ok(response)
}

#[post("/agents/<agent>/mcps?<scope..>", data = "<body>")]
pub fn create_mcp(
	_origin: TrustedLocalOrigin,
	agent: AgentParam,
	scope: ScopeParams,
	body: Json<CreateMcpRequest>,
) -> ApiCreated<McpResponse> {
	let resolved = scope.resolve()?;
	// Error precedence is public contract: capability → validate → writable
	// (an unsupported agent must answer UNSUPPORTED_OPERATION even when the
	// body is invalid or the scope is read-only).
	let (resource_scope, _) = resolved_to_resource_scope(&resolved);
	check_mcp_supported(&agent, resource_scope)?;
	body.validate()?;
	require_writable_scope(&resolved)?;
	let response = create_mcp_for_agent(&agent, &resolved, body.into_inner())?;
	Ok((Status::Created, Json(response)))
}

/// Multi-agent MCP create — the desktop's multi-select mapped onto the
/// SHARED core batch policy (`aghub_core::batch`): capability preflight for
/// ALL agents before any write (422 on a predictable failure, nothing
/// written), then attempt every agent and return per-agent attribution.
/// A partial failure is a 200 with `failed_count > 0` — the caller decides
/// how to surface it.
#[post("/mcps/batch?<scope..>", data = "<body>")]
pub fn batch_create_mcp(
	_origin: TrustedLocalOrigin,
	scope: ScopeParams,
	body: Json<BatchCreateMcpRequest>,
) -> ApiResult<AgentBatchResponse> {
	let req = body.into_inner();
	let resolved = scope.resolve()?;
	let (resource_scope, _) = resolved_to_resource_scope(&resolved);
	req.mcp.validate()?;
	require_writable_scope(&resolved)?;
	if req.agents.is_empty() {
		return Err(ApiError::new(
			Status::BadRequest,
			"agents must not be empty",
			"INVALID_PARAM",
		));
	}
	// Stable-dedup after parsing (aliases included), matching the CLI's
	// comma-list semantics — a duplicate must not turn into a second write
	// that fails RESOURCE_EXISTS.
	let mut agents: Vec<aghub_core::models::AgentType> = Vec::new();
	for s in &req.agents {
		let agent = s.parse().map_err(|_| {
			ApiError::new(
				Status::BadRequest,
				format!("Unknown agent '{s}'"),
				"INVALID_PARAM",
			)
		})?;
		if !agents.contains(&agent) {
			agents.push(agent);
		}
	}
	// Preflight also has to know the transport: a dialect with no word for it
	// refuses the write, and finding that out mid-batch leaves the agents that
	// already succeeded holding the server.
	let probe_transport =
		aghub_core::models::McpTransport::from(req.mcp.transport.clone());
	let view = aghub_core::batch::run_mcp_agent_mutation(
		&agents,
		resource_scope,
		false,
		Some(&probe_transport),
		|agent| {
			create_mcp_for_agent(&AgentParam(agent), &resolved, req.mcp.clone())
				.map(|resp| {
					serde_json::to_value(&resp)
						.unwrap_or(serde_json::Value::Null)
				})
				.map_err(|e| e.body.error)
		},
	)
	.map_err(|e| {
		ApiError::new(
			Status::UnprocessableEntity,
			e.to_string(),
			"UNSUPPORTED_OPERATION",
		)
	})?;
	Ok(Json(view.into()))
}

#[get("/agents/<agent>/mcps/<name>?<scope..>")]
pub fn get_mcp(
	_origin: TrustedLocalOrigin,
	agent: AgentParam,
	name: &str,
	scope: ScopeParams,
) -> ApiResult<McpResponse> {
	let resolved = scope.resolve()?;
	let (resource_scope, _) = resolved_to_resource_scope(&resolved);
	check_mcp_supported(&agent, resource_scope)?;
	let mut manager = build_manager_from_resolved(&agent, &resolved)?;

	if resolved.is_all() {
		let (_, mcps, _) =
			manager.load_both_annotated().map_err(ApiError::from)?;
		let mcp = mcps.iter().find(|m| m.name == name).ok_or_else(|| {
			ApiError::from(ConfigError::resource_not_found("mcp", name))
		})?;
		return Ok(Json(McpResponse::from(mcp)));
	}

	manager.load().map_err(ApiError::from)?;
	let mcp = manager.get_mcp(name).ok_or_else(|| {
		ApiError::from(ConfigError::resource_not_found("mcp", name))
	})?;
	Ok(Json(McpResponse::from(mcp)))
}

#[put("/agents/<agent>/mcps/<name>?<scope..>", data = "<body>")]
pub fn update_mcp(
	_origin: TrustedLocalOrigin,
	agent: AgentParam,
	name: &str,
	scope: ScopeParams,
	body: Json<UpdateMcpRequest>,
) -> ApiResult<McpResponse> {
	let resolved = scope.resolve()?;
	let (resource_scope, _) = resolved_to_resource_scope(&resolved);
	check_mcp_supported(&agent, resource_scope)?;
	body.validate()?;
	require_writable_scope(&resolved)?;
	let mut manager = build_manager_from_resolved(&agent, &resolved)?;
	manager.load().map_err(ApiError::from)?;
	let existing = manager
		.get_mcp(name)
		.ok_or_else(|| {
			ApiError::from(ConfigError::resource_not_found("mcp", name))
		})?
		.clone();
	let updated = body.into_inner().apply_to(existing);
	let response = McpResponse::from(&updated);
	manager.update_mcp(name, updated).map_err(ApiError::from)?;
	Ok(Json(response))
}

/// Query params for `delete_mcp`. Mirrors the skill `DeleteSkillParams`
/// dry-run/confirm gate but without `all_agents` (MCP removal is single-scope).
#[derive(rocket::FromForm)]
pub struct DeleteMcpParams {
	scope: Option<String>,
	project_root: Option<String>,
	confirm: Option<bool>,
}

#[delete("/agents/<agent>/mcps/<name>?<params..>")]
pub fn delete_mcp(
	_origin: TrustedLocalOrigin,
	agent: AgentParam,
	name: &str,
	params: DeleteMcpParams,
) -> ApiResult<DeleteSkillByPathResponse> {
	let resolved = ScopeParams {
		scope: params.scope.clone(),
		project_root: params.project_root.clone(),
	}
	.resolve()?;
	let (resource_scope, _) = resolved_to_resource_scope(&resolved);
	check_mcp_supported(&agent, resource_scope)?;
	require_writable_scope(&resolved)?;
	let confirm = params.confirm.unwrap_or(false);
	let dry_run = !confirm;
	let mut manager = build_manager_from_resolved(&agent, &resolved)?;
	match manager.load() {
		Ok(_) => {}
		// No config file: nothing to remove. Return a dry-run-shaped Ok body
		// (success:true, executed:false) so the wire shape is uniform across
		// the skill/MCP/sub-agent delete routes.
		Err(ConfigError::NotFound { .. }) => {
			return Ok(Json(crate::routes::noop_removal_response(
				vec![],
				vec![],
				dry_run,
			)));
		}
		Err(e) => return Err(ApiError::from(e)),
	}
	// Idempotent-delete contract (a missing MCP is a success no-op, any other
	// error propagates) is owned once in `routes::removal_or_noop`.
	crate::routes::removal_or_noop(
		manager.remove_mcp_planned(name, dry_run, confirm),
		dry_run,
	)
}

#[post("/agents/<agent>/mcps/<name>/enable?<scope..>")]
pub fn enable_mcp(
	_origin: TrustedLocalOrigin,
	agent: AgentParam,
	name: &str,
	scope: ScopeParams,
) -> ApiResult<McpResponse> {
	let resolved = scope.resolve()?;
	let (resource_scope, _) = resolved_to_resource_scope(&resolved);
	check_mcp_supported(&agent, resource_scope)?;
	require_writable_scope(&resolved)?;
	let mut manager = build_manager_from_resolved(&agent, &resolved)?;
	manager.load().map_err(ApiError::from)?;
	manager.enable_mcp(name).map_err(ApiError::from)?;
	let mcp = manager.get_mcp(name).expect("mcp present after enable");
	Ok(Json(McpResponse::from(mcp)))
}

#[post("/agents/<agent>/mcps/<name>/disable?<scope..>")]
pub fn disable_mcp(
	_origin: TrustedLocalOrigin,
	agent: AgentParam,
	name: &str,
	scope: ScopeParams,
) -> ApiResult<McpResponse> {
	let resolved = scope.resolve()?;
	let (resource_scope, _) = resolved_to_resource_scope(&resolved);
	check_mcp_supported(&agent, resource_scope)?;
	require_writable_scope(&resolved)?;
	let mut manager = build_manager_from_resolved(&agent, &resolved)?;
	manager.load().map_err(ApiError::from)?;
	manager.disable_mcp(name).map_err(ApiError::from)?;
	let mcp = manager.get_mcp(name).expect("mcp present after disable");
	Ok(Json(McpResponse::from(mcp)))
}

#[get("/agents/all/mcps?<scope..>")]
pub fn list_all_agents_mcps(
	_origin: TrustedLocalOrigin,
	scope: ScopeParams,
) -> ApiResult<Vec<McpResponse>> {
	let resolved = scope.resolve()?;
	let (resource_scope, project_root) = resolved_to_resource_scope(&resolved);
	let items = load_all_agents(resource_scope, project_root.as_deref())
		.into_iter()
		.flat_map(|ar| {
			let id = ar.agent_id;
			ar.mcps.into_iter().map(move |m| McpResponse::from((m, id)))
		})
		.collect();
	Ok(Json(items))
}

#[cfg(test)]
mod tests {
	use super::*;
	use aghub_core::models::AgentType;

	use crate::{
		dto::mcp::{CreateMcpRequest, TransportDto},
		extractors::AgentParam,
	};

	#[test]
	fn test_create_mcp_rejects_pi_agent() {
		let result = create_mcp(
			TrustedLocalOrigin,
			AgentParam(AgentType::Pi),
			ScopeParams {
				scope: Some("global".to_string()),
				project_root: None,
			},
			Json(CreateMcpRequest {
				name: "pi-mcp".to_string(),
				transport: TransportDto::Stdio {
					command: "echo".to_string(),
					args: vec!["hello".to_string()],
					env: None,
					timeout: None,
				},
				timeout: None,
			}),
		);

		let err = result.expect_err("pi should reject MCP creation");
		assert_eq!(err.status, Status::UnprocessableEntity);
		assert_eq!(err.body.code, "UNSUPPORTED_OPERATION");
		assert!(err.body.error.contains("does not support MCP servers"));
		assert!(err.body.error.contains("pi"));
	}

	#[test]
	fn test_create_mcp_rejects_zero_timeout() {
		let result = create_mcp(
			TrustedLocalOrigin,
			AgentParam(AgentType::Claude),
			ScopeParams {
				scope: Some("global".to_string()),
				project_root: None,
			},
			Json(CreateMcpRequest {
				name: "zero".to_string(),
				transport: TransportDto::Stdio {
					command: "echo".to_string(),
					args: vec![],
					env: None,
					timeout: None,
				},
				timeout: Some(0),
			}),
		);

		let err = result.expect_err("zero timeout should reject");
		assert_eq!(err.status, Status::UnprocessableEntity);
		assert_eq!(err.body.code, "VALIDATION_FAILED");
		assert!(err.body.error.contains("timeout"));
	}

	#[test]
	fn test_create_mcp_rejects_zero_transport_timeout() {
		let result = create_mcp(
			TrustedLocalOrigin,
			AgentParam(AgentType::Claude),
			ScopeParams {
				scope: Some("global".to_string()),
				project_root: None,
			},
			Json(CreateMcpRequest {
				name: "zero-transport".to_string(),
				transport: TransportDto::Stdio {
					command: "echo".to_string(),
					args: vec![],
					env: None,
					timeout: Some(0),
				},
				timeout: None,
			}),
		);

		let err = result.expect_err("zero transport timeout should reject");
		assert_eq!(err.status, Status::UnprocessableEntity);
		assert_eq!(err.body.code, "VALIDATION_FAILED");
		assert!(err.body.error.contains("timeout"));
	}

	#[test]
	fn test_update_mcp_rejects_zero_timeout() {
		use crate::dto::mcp::UpdateMcpRequest;

		let result = update_mcp(
			TrustedLocalOrigin,
			AgentParam(AgentType::Claude),
			"any",
			ScopeParams {
				scope: Some("global".to_string()),
				project_root: None,
			},
			Json(UpdateMcpRequest {
				name: None,
				transport: None,
				enabled: None,
				timeout: Some(0),
			}),
		);

		let err = result.expect_err("zero timeout should reject");
		assert_eq!(err.status, Status::UnprocessableEntity);
		assert_eq!(err.body.code, "VALIDATION_FAILED");
		assert!(err.body.error.contains("timeout"));
	}

	#[test]
	fn test_update_mcp_rejects_zero_transport_timeout() {
		use crate::dto::mcp::UpdateMcpRequest;

		let result = update_mcp(
			TrustedLocalOrigin,
			AgentParam(AgentType::Claude),
			"any",
			ScopeParams {
				scope: Some("global".to_string()),
				project_root: None,
			},
			Json(UpdateMcpRequest {
				name: None,
				transport: Some(TransportDto::Stdio {
					command: "echo".to_string(),
					args: vec![],
					env: None,
					timeout: Some(0),
				}),
				enabled: None,
				timeout: None,
			}),
		);

		let err = result.expect_err("zero transport timeout should reject");
		assert_eq!(err.status, Status::UnprocessableEntity);
		assert_eq!(err.body.code, "VALIDATION_FAILED");
		assert!(err.body.error.contains("timeout"));
	}

	// --- delete_mcp dry-run/confirm gate (Phase 3 #5) -----------------------

	/// Seed one Claude MCP in a project-scoped temp root so delete tests have
	/// real on-disk state without touching the real home dir.
	fn seed_mcp(root: &std::path::Path, name: &str) {
		create_mcp(
			TrustedLocalOrigin,
			AgentParam(AgentType::Claude),
			ScopeParams {
				scope: Some("project".to_string()),
				project_root: Some(root.display().to_string()),
			},
			Json(CreateMcpRequest {
				name: name.to_string(),
				transport: TransportDto::Stdio {
					command: "echo".to_string(),
					args: vec!["hi".to_string()],
					env: None,
					timeout: None,
				},
				timeout: None,
			}),
		)
		.ok()
		.expect("seed mcp");
	}

	fn mcp_exists(root: &std::path::Path, name: &str) -> bool {
		list_mcps(
			TrustedLocalOrigin,
			AgentParam(AgentType::Claude),
			ScopeParams {
				scope: Some("project".to_string()),
				project_root: Some(root.display().to_string()),
			},
		)
		.ok()
		.expect("list mcps")
		.into_inner()
		.iter()
		.any(|m| m.name == name)
	}

	fn delete_params(
		root: &std::path::Path,
		confirm: Option<bool>,
	) -> DeleteMcpParams {
		DeleteMcpParams {
			scope: Some("project".to_string()),
			project_root: Some(root.display().to_string()),
			confirm,
		}
	}

	#[test]
	fn delete_mcp_dry_run_default_keeps_entry() {
		let tmp = tempfile::tempdir().unwrap();
		let root = tmp.path();
		seed_mcp(root, "keepme");

		let resp = delete_mcp(
			TrustedLocalOrigin,
			AgentParam(AgentType::Claude),
			"keepme",
			delete_params(root, None),
		)
		.ok()
		.expect("dry-run ok")
		.into_inner();

		assert!(resp.success);
		assert!(resp.dry_run, "default (confirm=None) must be a dry-run");
		assert!(!resp.executed, "dry-run must not execute");
		assert!(
			resp.paths.is_empty(),
			"MCP removal deletes no on-disk path (only rewrites a config entry)"
		);
		assert!(resp.deleted_path.is_none(), "nothing deleted on dry-run");
		assert!(mcp_exists(root, "keepme"), "dry-run must leave the mcp");
	}

	#[test]
	fn delete_mcp_confirm_removes_entry() {
		let tmp = tempfile::tempdir().unwrap();
		let root = tmp.path();
		seed_mcp(root, "goner");

		let resp = delete_mcp(
			TrustedLocalOrigin,
			AgentParam(AgentType::Claude),
			"goner",
			delete_params(root, Some(true)),
		)
		.ok()
		.expect("confirm ok")
		.into_inner();

		assert!(resp.success);
		assert!(!resp.dry_run);
		assert!(resp.executed, "confirm=true must execute");
		assert!(!mcp_exists(root, "goner"), "confirm deletes the mcp");
	}

	#[test]
	fn delete_mcp_missing_is_dry_run_shaped_ok() {
		// Missing name is not an error: it returns a dry-run-shaped success body
		// (success:true, executed:false), matching delete_skill's NotFound path.
		let tmp = tempfile::tempdir().unwrap();
		let root = tmp.path();
		seed_mcp(root, "present");

		let resp = delete_mcp(
			TrustedLocalOrigin,
			AgentParam(AgentType::Claude),
			"absent",
			delete_params(root, Some(true)),
		)
		.ok()
		.expect("missing is ok")
		.into_inner();

		assert!(resp.success);
		assert!(!resp.executed, "nothing to remove");
		assert!(
			resp.deleted_path.is_none(),
			"no-op missing delete must leave deleted_path null"
		);
		assert!(mcp_exists(root, "present"), "the real mcp is untouched");
	}

	#[test]
	fn delete_mcp_no_config_is_dry_run_shaped_ok() {
		// No config file on disk at all: the NotFound-config early return must
		// produce a dry-run-shaped Ok body, not a 204/error.
		let tmp = tempfile::tempdir().unwrap();
		let root = tmp.path();

		let resp = delete_mcp(
			TrustedLocalOrigin,
			AgentParam(AgentType::Claude),
			"anything",
			delete_params(root, None),
		)
		.ok()
		.expect("no-config is ok")
		.into_inner();

		assert!(resp.success);
		assert!(resp.dry_run);
		assert!(!resp.executed);
		assert!(
			resp.deleted_path.is_none(),
			"no-op leaves deleted_path null"
		);
	}

	#[test]
	fn delete_mcp_rejects_unsupported_agent() {
		// The supports-mcp guard still fires before any planning.
		let tmp = tempfile::tempdir().unwrap();
		let root = tmp.path();
		let err = delete_mcp(
			TrustedLocalOrigin,
			AgentParam(AgentType::Pi),
			"x",
			delete_params(root, Some(true)),
		)
		.expect_err("pi rejects mcp delete");
		assert_eq!(err.status, Status::UnprocessableEntity);
		assert_eq!(err.body.code, "UNSUPPORTED_OPERATION");
	}

	fn stdio_req(name: &str) -> CreateMcpRequest {
		CreateMcpRequest {
			name: name.to_string(),
			transport: TransportDto::Stdio {
				command: "echo".to_string(),
				args: vec![],
				env: None,
				timeout: None,
			},
			timeout: None,
		}
	}

	/// The batch preflight must reject the WHOLE batch on one unsupported
	/// agent (pi holds no MCPs) before any manager is built — nothing
	/// written, 422 with the shared core reason string.
	#[test]
	fn batch_create_mcp_preflight_rejects_and_writes_nothing() {
		let result = batch_create_mcp(
			TrustedLocalOrigin,
			ScopeParams {
				scope: Some("global".to_string()),
				project_root: None,
			},
			Json(BatchCreateMcpRequest {
				agents: vec!["claude".to_string(), "pi".to_string()],
				mcp: stdio_req("never"),
			}),
		);
		let err = result.expect_err("pi must fail the whole batch");
		assert_eq!(err.status, Status::UnprocessableEntity);
		assert_eq!(err.body.code, "UNSUPPORTED_OPERATION");
		assert!(err.body.error.contains("pi"), "{}", err.body.error);
		assert!(
			err.body.error.contains("nothing was written"),
			"{}",
			err.body.error
		);
	}

	#[test]
	fn batch_create_mcp_rejects_unknown_agent() {
		let result = batch_create_mcp(
			TrustedLocalOrigin,
			ScopeParams {
				scope: Some("global".to_string()),
				project_root: None,
			},
			Json(BatchCreateMcpRequest {
				agents: vec!["claude".to_string(), "nonesuch".to_string()],
				mcp: stdio_req("never"),
			}),
		);
		let err = result.expect_err("unknown agent must 400");
		assert_eq!(err.status, Status::BadRequest);
		assert_eq!(err.body.code, "INVALID_PARAM");
		assert!(err.body.error.contains("nonesuch"), "{}", err.body.error);
	}

	/// Error precedence is public contract: an unsupported agent answers
	/// UNSUPPORTED_OPERATION even when the scope is read-only (`all`) —
	/// capability is checked BEFORE the writable-scope gate.
	#[test]
	fn create_mcp_capability_beats_readonly_scope() {
		let result = create_mcp(
			TrustedLocalOrigin,
			AgentParam(AgentType::Pi),
			ScopeParams {
				scope: Some("all".to_string()),
				project_root: None,
			},
			Json(stdio_req("pi-mcp")),
		);
		let err = result.expect_err("pi must fail on capability first");
		assert_eq!(err.status, Status::UnprocessableEntity);
		assert_eq!(err.body.code, "UNSUPPORTED_OPERATION");
	}

	/// …and BEFORE body validation: pi + invalid timeout must still answer
	/// UNSUPPORTED_OPERATION, not VALIDATION_FAILED.
	#[test]
	fn create_mcp_capability_beats_validation() {
		let mut req = stdio_req("pi-mcp");
		req.timeout = Some(0);
		let result = create_mcp(
			TrustedLocalOrigin,
			AgentParam(AgentType::Pi),
			ScopeParams {
				scope: Some("global".to_string()),
				project_root: None,
			},
			Json(req),
		);
		let err = result.expect_err("pi must fail on capability first");
		assert_eq!(err.body.code, "UNSUPPORTED_OPERATION");
	}

	/// Duplicate agents stable-dedup to ONE attempt (CLI parity): the
	/// preflight rejection names pi exactly once, proving the roster was
	/// deduped before any downstream step.
	#[test]
	fn batch_create_mcp_dedups_duplicate_agents() {
		let result = batch_create_mcp(
			TrustedLocalOrigin,
			ScopeParams {
				scope: Some("global".to_string()),
				project_root: None,
			},
			Json(BatchCreateMcpRequest {
				agents: vec!["pi".to_string(), "pi".to_string()],
				mcp: stdio_req("never"),
			}),
		);
		let err = result.expect_err("pi must fail preflight");
		assert_eq!(err.status, Status::UnprocessableEntity);
		assert_eq!(
			err.body.error.matches("pi").count(),
			1,
			"duplicates must collapse to one roster entry: {}",
			err.body.error
		);
	}

	#[test]
	fn batch_create_mcp_rejects_empty_agent_list() {
		let result = batch_create_mcp(
			TrustedLocalOrigin,
			ScopeParams {
				scope: Some("global".to_string()),
				project_root: None,
			},
			Json(BatchCreateMcpRequest {
				agents: vec![],
				mcp: stdio_req("never"),
			}),
		);
		let err = result.expect_err("empty agent list must 400");
		assert_eq!(err.status, Status::BadRequest);
		assert_eq!(err.body.code, "INVALID_PARAM");
	}
}
