use aghub_core::{
	errors::ConfigError, load_all_agents, models::SubAgent, transfer,
};
use rocket::http::Status;
use rocket::serde::json::Json;

use crate::{
	dto::skill::DeleteSkillByPathResponse,
	dto::sub_agent::{
		CreateSubAgentRequest, SubAgentResponse, UpdateSubAgentRequest,
	},
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

fn check_sub_agent_supported(
	agent: &AgentParam,
	scope: aghub_core::models::ResourceScope,
) -> Result<(), ApiError> {
	let descriptor = aghub_core::registry::get(agent.0);
	if !descriptor.supports_sub_agent_scope(scope) {
		return Err(ApiError::new(
			Status::UnprocessableEntity,
			format!(
				"Agent '{}' does not support sub-agents in {:?} scope",
				descriptor.id, scope
			),
			"UNSUPPORTED_OPERATION",
		));
	}
	Ok(())
}

#[post("/sub-agents/transfer", data = "<body>")]
pub fn transfer_sub_agent_route(
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
	let result = transfer::transfer_sub_agent(source, destinations)
		.map_err(ApiError::from)?;
	Ok(Json(result.into()))
}

#[post("/sub-agents/reconcile", data = "<body>")]
pub fn reconcile_sub_agent_route(
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

	let result = transfer::reconcile_sub_agent(source, added, removed, confirm)
		.map_err(ApiError::from)?;
	Ok(Json(result.into()))
}

#[get("/agents/<agent>/sub-agents?<scope..>")]
pub fn list_sub_agents(
	_origin: TrustedLocalOrigin,
	agent: AgentParam,
	scope: ScopeParams,
) -> ApiResult<Vec<SubAgentResponse>> {
	let resolved = scope.resolve()?;
	let (resource_scope, _) = resolved_to_resource_scope(&resolved);
	check_sub_agent_supported(&agent, resource_scope)?;
	let mut manager = build_manager_from_resolved(&agent, &resolved)?;

	if resolved.is_all() {
		let (_, _, sub_agents) =
			manager.load_both_annotated().map_err(ApiError::from)?;
		let items = sub_agents.iter().map(SubAgentResponse::from).collect();
		return Ok(Json(items));
	}

	let config = manager.load().map_err(ApiError::from)?;
	let items = config
		.sub_agents
		.iter()
		.map(SubAgentResponse::from)
		.collect();
	Ok(Json(items))
}

#[get("/agents/all/sub-agents?<scope..>")]
pub fn list_all_agents_sub_agents(
	_origin: TrustedLocalOrigin,
	scope: ScopeParams,
) -> ApiResult<Vec<SubAgentResponse>> {
	let resolved = scope.resolve()?;
	let (resource_scope, project_root) = resolved_to_resource_scope(&resolved);
	let all = load_all_agents(resource_scope, project_root.as_deref());
	let items = all
		.into_iter()
		.flat_map(|r| {
			r.sub_agents
				.into_iter()
				.map(|a| SubAgentResponse::from((a, r.agent_id)))
				.collect::<Vec<_>>()
		})
		.collect();
	Ok(Json(items))
}

#[get("/agents/<agent>/sub-agents/<name>?<scope..>")]
pub fn get_sub_agent(
	_origin: TrustedLocalOrigin,
	agent: AgentParam,
	name: String,
	scope: ScopeParams,
) -> ApiResult<SubAgentResponse> {
	let resolved = scope.resolve()?;
	let (resource_scope, _) = resolved_to_resource_scope(&resolved);
	check_sub_agent_supported(&agent, resource_scope)?;
	let mut manager = build_manager_from_resolved(&agent, &resolved)?;

	if resolved.is_all() {
		let (_, _, sub_agents) =
			manager.load_both_annotated().map_err(ApiError::from)?;
		return sub_agents
			.into_iter()
			.find(|a| a.name == name)
			.map(SubAgentResponse::from)
			.map(Json)
			.ok_or_else(|| {
				ApiError::from(ConfigError::resource_not_found(
					"sub_agent",
					&name,
				))
			});
	}

	let config = manager.load().map_err(ApiError::from)?;
	config
		.sub_agents
		.iter()
		.find(|a| a.name == name)
		.map(SubAgentResponse::from)
		.map(Json)
		.ok_or_else(|| {
			ApiError::from(ConfigError::resource_not_found("sub_agent", &name))
		})
}

#[post("/agents/<agent>/sub-agents?<scope..>", data = "<body>")]
pub fn create_sub_agent(
	_origin: TrustedLocalOrigin,
	agent: AgentParam,
	scope: ScopeParams,
	body: Json<CreateSubAgentRequest>,
) -> ApiCreated<SubAgentResponse> {
	let resolved = scope.resolve()?;
	require_writable_scope(&resolved)?;
	let (resource_scope, _) = resolved_to_resource_scope(&resolved);
	check_sub_agent_supported(&agent, resource_scope)?;
	let mut manager = build_manager_from_resolved(&agent, &resolved)?;
	manager.load().map_err(ApiError::from)?;

	let new_agent = SubAgent::from(body.into_inner());
	let response = SubAgentResponse::from(&new_agent);
	manager.add_sub_agent(new_agent).map_err(ApiError::from)?;
	Ok((Status::Created, Json(response)))
}

#[put("/agents/<agent>/sub-agents/<name>?<scope..>", data = "<body>")]
pub fn update_sub_agent(
	_origin: TrustedLocalOrigin,
	agent: AgentParam,
	name: String,
	scope: ScopeParams,
	body: Json<UpdateSubAgentRequest>,
) -> ApiResult<SubAgentResponse> {
	let resolved = scope.resolve()?;
	require_writable_scope(&resolved)?;
	let (resource_scope, _) = resolved_to_resource_scope(&resolved);
	check_sub_agent_supported(&agent, resource_scope)?;
	let mut manager = build_manager_from_resolved(&agent, &resolved)?;
	manager.load().map_err(ApiError::from)?;

	let body = body.into_inner();
	// Capture the effective name after the patch so we can look it up
	// after a potential rename (patch.name takes precedence over route name).
	let effective_name = body.name.clone().unwrap_or_else(|| name.clone());
	let patch = body.into();
	manager
		.update_sub_agent(&name, patch)
		.map_err(ApiError::from)?;

	let config = manager.config().unwrap();
	let updated = config
		.sub_agents
		.iter()
		.find(|a| a.name == effective_name)
		.map(SubAgentResponse::from)
		.ok_or_else(|| {
			ApiError::from(ConfigError::resource_not_found(
				"sub_agent",
				&effective_name,
			))
		})?;
	Ok(Json(updated))
}

/// Query params for `delete_sub_agent`. Mirrors `DeleteMcpParams`: the same
/// dry-run/confirm gate (no `all_agents` — sub-agent removal is single-scope).
#[derive(rocket::FromForm)]
pub struct DeleteSubAgentParams {
	scope: Option<String>,
	project_root: Option<String>,
	confirm: Option<bool>,
}

#[delete("/agents/<agent>/sub-agents/<name>?<params..>")]
pub fn delete_sub_agent(
	_origin: TrustedLocalOrigin,
	agent: AgentParam,
	name: String,
	params: DeleteSubAgentParams,
) -> ApiResult<DeleteSkillByPathResponse> {
	let resolved = ScopeParams {
		scope: params.scope.clone(),
		project_root: params.project_root.clone(),
	}
	.resolve()?;
	require_writable_scope(&resolved)?;
	let (resource_scope, _) = resolved_to_resource_scope(&resolved);
	check_sub_agent_supported(&agent, resource_scope)?;
	let confirm = params.confirm.unwrap_or(false);
	let dry_run = !confirm;
	let mut manager = build_manager_from_resolved(&agent, &resolved)?;
	manager.load().map_err(ApiError::from)?;
	// Idempotent-delete contract (a missing sub-agent is a success no-op, any
	// other error propagates) is owned once in `routes::removal_or_noop`,
	// mirroring the skill/MCP delete routes.
	crate::routes::removal_or_noop(
		manager.remove_sub_agent_planned(&name, dry_run, confirm),
		dry_run,
	)
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::dto::sub_agent::CreateSubAgentRequest;
	use aghub_core::models::AgentType;

	/// Seed one Claude sub-agent in a project-scoped temp root so delete tests
	/// have real on-disk state without touching the real home dir.
	fn seed_sub_agent(root: &std::path::Path, name: &str) {
		create_sub_agent(
			TrustedLocalOrigin,
			AgentParam(AgentType::Claude),
			ScopeParams {
				scope: Some("project".to_string()),
				project_root: Some(root.display().to_string()),
			},
			Json(CreateSubAgentRequest {
				name: name.to_string(),
				description: "d".to_string(),
				instruction: "do things".to_string(),
			}),
		)
		.ok()
		.expect("seed sub-agent");
	}

	fn agent_file(root: &std::path::Path, name: &str) -> std::path::PathBuf {
		root.join(".claude/agents").join(format!("{name}.md"))
	}

	fn sub_agent_exists(root: &std::path::Path, name: &str) -> bool {
		list_sub_agents(
			TrustedLocalOrigin,
			AgentParam(AgentType::Claude),
			ScopeParams {
				scope: Some("project".to_string()),
				project_root: Some(root.display().to_string()),
			},
		)
		.ok()
		.expect("list sub-agents")
		.into_inner()
		.iter()
		.any(|a| a.name == name)
	}

	fn del_params(
		root: &std::path::Path,
		confirm: Option<bool>,
	) -> DeleteSubAgentParams {
		DeleteSubAgentParams {
			scope: Some("project".to_string()),
			project_root: Some(root.display().to_string()),
			confirm,
		}
	}

	#[test]
	fn delete_sub_agent_dry_run_default_keeps_agent_and_file() {
		let tmp = tempfile::tempdir().unwrap();
		let root = tmp.path();
		seed_sub_agent(root, "reviewer");
		let file = agent_file(root, "reviewer");
		assert!(file.exists(), "precondition: backing file written");

		let resp = delete_sub_agent(
			TrustedLocalOrigin,
			AgentParam(AgentType::Claude),
			"reviewer".to_string(),
			del_params(root, None),
		)
		.ok()
		.expect("dry-run ok")
		.into_inner();

		assert!(resp.success);
		assert!(resp.dry_run, "default (confirm=None) must be a dry-run");
		assert!(!resp.executed, "dry-run must not execute");
		assert_eq!(resp.paths.len(), 1, "plan names the backing file");
		assert!(resp.deleted_path.is_none(), "nothing deleted on dry-run");
		assert!(file.exists(), "dry-run must leave the file on disk");
		assert!(sub_agent_exists(root, "reviewer"), "agent still present");
	}

	#[test]
	fn delete_sub_agent_confirm_removes_agent_and_file() {
		let tmp = tempfile::tempdir().unwrap();
		let root = tmp.path();
		seed_sub_agent(root, "goner");
		let file = agent_file(root, "goner");
		assert!(file.exists());

		let resp = delete_sub_agent(
			TrustedLocalOrigin,
			AgentParam(AgentType::Claude),
			"goner".to_string(),
			del_params(root, Some(true)),
		)
		.ok()
		.expect("confirm ok")
		.into_inner();

		assert!(resp.success);
		assert!(!resp.dry_run);
		assert!(resp.executed, "confirm=true must execute");
		assert_eq!(
			resp.deleted_path.as_deref(),
			Some(file.display().to_string().as_str()),
			"deleted_path is the backing file"
		);
		assert!(!file.exists(), "confirm deletes the backing file");
		assert!(!sub_agent_exists(root, "goner"), "agent gone");
	}

	#[test]
	fn delete_sub_agent_missing_is_dry_run_shaped_ok() {
		// Missing name is not an error: dry-run-shaped success body, matching
		// the skill/MCP delete routes.
		let tmp = tempfile::tempdir().unwrap();
		let root = tmp.path();
		seed_sub_agent(root, "present");

		let resp = delete_sub_agent(
			TrustedLocalOrigin,
			AgentParam(AgentType::Claude),
			"absent".to_string(),
			del_params(root, Some(true)),
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
		assert!(sub_agent_exists(root, "present"), "real agent untouched");
	}

	#[test]
	fn delete_sub_agent_rejects_unsupported_scope() {
		// 'all' is read-only — require_writable_scope rejects before planning.
		let tmp = tempfile::tempdir().unwrap();
		let root = tmp.path();
		let err = delete_sub_agent(
			TrustedLocalOrigin,
			AgentParam(AgentType::Claude),
			"x".to_string(),
			DeleteSubAgentParams {
				scope: Some("all".to_string()),
				project_root: Some(root.display().to_string()),
				confirm: Some(true),
			},
		)
		.expect_err("all scope rejects write");
		assert_eq!(err.status, Status::MethodNotAllowed);
		assert_eq!(err.body.code, "READ_ONLY_SCOPE");
	}
}
