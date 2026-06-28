use aghub_core::{
	errors::ConfigError, load_all_agents, models::McpServer, transfer,
};
use rocket::http::Status;
use rocket::serde::json::Json;

use crate::{
	dto::mcp::{CreateMcpRequest, McpResponse, UpdateMcpRequest},
	dto::skill::DeleteSkillByPathResponse,
	dto::transfer::{
		OperationBatchResponse, ReconcileRequest, TransferRequest,
	},
	error::{ApiCreated, ApiError, ApiResult},
	extractors::{AgentParam, ScopeParams},
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
	body: Json<ReconcileRequest>,
) -> ApiResult<OperationBatchResponse> {
	let req = body.into_inner();
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

	let result = transfer::reconcile_mcp(source, added, removed)
		.map_err(ApiError::from)?;
	Ok(Json(result.into()))
}

#[post("/agents/<agent>/mcps?<scope..>", data = "<body>")]
pub fn create_mcp(
	agent: AgentParam,
	scope: ScopeParams,
	body: Json<CreateMcpRequest>,
) -> ApiCreated<McpResponse> {
	let resolved = scope.resolve()?;
	let (resource_scope, _) = resolved_to_resource_scope(&resolved);
	check_mcp_supported(&agent, resource_scope)?;
	body.validate()?;
	require_writable_scope(&resolved)?;
	let mut manager = build_manager_from_resolved(&agent, &resolved)?;
	match manager.load() {
		Ok(_) => {}
		Err(ConfigError::NotFound { .. }) => manager.init_empty_config(),
		Err(e) => return Err(ApiError::from(e)),
	}
	let mcp = McpServer::from(body.into_inner());
	let response = McpResponse::from(&mcp);
	manager.add_mcp(mcp).map_err(ApiError::from)?;
	Ok((Status::Created, Json(response)))
}

#[get("/agents/<agent>/mcps/<name>?<scope..>")]
pub fn get_mcp(
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
			return Ok(Json(DeleteSkillByPathResponse {
				success: true,
				dry_run,
				executed: false,
				..Default::default()
			}));
		}
		Err(e) => return Err(ApiError::from(e)),
	}
	match manager.remove_mcp_planned(name, dry_run, confirm) {
		Ok(outcome) => Ok(Json(crate::routes::removal_response(outcome))),
		Err(ConfigError::ResourceNotFound { .. }) => {
			Ok(Json(DeleteSkillByPathResponse {
				success: true,
				dry_run,
				executed: false,
				..Default::default()
			}))
		}
		Err(e) => Err(ApiError::from(e)),
	}
}

#[post("/agents/<agent>/mcps/<name>/enable?<scope..>")]
pub fn enable_mcp(
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
pub fn list_all_agents_mcps(scope: ScopeParams) -> ApiResult<Vec<McpResponse>> {
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
		assert_eq!(resp.paths.len(), 1, "plan names the config file path");
		assert!(resp.deleted_path.is_none(), "nothing deleted on dry-run");
		assert!(mcp_exists(root, "keepme"), "dry-run must leave the mcp");
	}

	#[test]
	fn delete_mcp_confirm_removes_entry() {
		let tmp = tempfile::tempdir().unwrap();
		let root = tmp.path();
		seed_mcp(root, "goner");

		let resp = delete_mcp(
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
			AgentParam(AgentType::Claude),
			"absent",
			delete_params(root, Some(true)),
		)
		.ok()
		.expect("missing is ok")
		.into_inner();

		assert!(resp.success);
		assert!(!resp.executed, "nothing to remove");
		assert!(mcp_exists(root, "present"), "the real mcp is untouched");
	}

	#[test]
	fn delete_mcp_no_config_is_dry_run_shaped_ok() {
		// No config file on disk at all: the NotFound-config early return must
		// produce a dry-run-shaped Ok body, not a 204/error.
		let tmp = tempfile::tempdir().unwrap();
		let root = tmp.path();

		let resp = delete_mcp(
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
	}

	#[test]
	fn delete_mcp_rejects_unsupported_agent() {
		// The supports-mcp guard still fires before any planning.
		let tmp = tempfile::tempdir().unwrap();
		let root = tmp.path();
		let err = delete_mcp(
			AgentParam(AgentType::Pi),
			"x",
			delete_params(root, Some(true)),
		)
		.expect_err("pi rejects mcp delete");
		assert_eq!(err.status, Status::UnprocessableEntity);
		assert_eq!(err.body.code, "UNSUPPORTED_OPERATION");
	}
}
