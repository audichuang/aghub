pub mod agents;
pub mod catchers;
pub mod coverage;
pub mod credentials;
pub mod inference;
pub mod integrations;
pub mod market;
pub mod mcps;
pub mod plugins;
pub mod skills;
pub mod skills_update;
pub mod sources;
pub mod sub_agents;

use aghub_core::{
	create_adapter,
	manager::ConfigManager,
	models::ResourceScope,
	skills::removal::{PruneStatus, RemovalOutcome},
};
use rocket::http::Status;
use rocket::response::status::NoContent;
use std::path::PathBuf;

use crate::dto::skill::DeleteSkillByPathResponse;
use crate::error::ApiError;
use crate::extractors::{AgentParam, ResolvedScope};

/// Map a [`RemovalOutcome`] to the shared [`DeleteSkillByPathResponse`] wire
/// shape. Owned ONCE here so the skill, MCP and sub-agent delete routes all
/// serialize identically. The 7 core removal-outcome fields (success/dry_run/
/// executed/needs_confirm/paths/skipped/deleted_path) and the PathBuf->String
/// derivation live in `aghub_core::dto::RemovalView`; this layers the api-only
/// lock-prune fields on top (always None for MCP/sub-agent, which never prune).
pub(crate) fn removal_response(
	outcome: RemovalOutcome,
) -> DeleteSkillByPathResponse {
	let mut response = DeleteSkillByPathResponse::from(
		&aghub_core::dto::RemovalView::from(&outcome),
	);
	let (pruned_lock_entries, prune_error) = match outcome.prune {
		PruneStatus::NotRun => (None, None),
		PruneStatus::Pruned(keys) => (Some(keys), None),
		PruneStatus::Failed { reason, pruned } => (Some(pruned), Some(reason)),
	};
	response.pruned_lock_entries = pruned_lock_entries;
	response.prune_error = prune_error;
	response
}

/// Removal-shaped success body for a **no-op** delete (nothing on disk to
/// remove, or the targeted dir is a shared master kept for another agent). Goes
/// through the same `RemovalView` seam as a real removal so these edge branches
/// can't drift from the wire shape — in particular `deleted_path` stays `null`
/// because `executed` is false.
pub(crate) fn noop_removal_response(
	paths: Vec<PathBuf>,
	skipped: Vec<PathBuf>,
) -> DeleteSkillByPathResponse {
	removal_response(RemovalOutcome {
		plan: aghub_core::skills::removal::RemovalPlan {
			layout: aghub_core::skills::removal::Layout::Copy,
			paths,
			skipped,
			needs_confirm: false,
		},
		executed: false,
		prune: PruneStatus::NotRun,
	})
}

/// The API's **idempotent-delete contract**, owned in ONE place so the skill,
/// MCP and sub-agent delete routes apply it identically instead of each
/// open-coding an `Err(ResourceNotFound) => noop` arm.
///
/// Deleting a resource that is already absent is a **success no-op**
/// (`success:true, executed:false, deleted_path:null`), NOT an error — DELETE is
/// idempotent, the post-condition ("resource gone") already holds. This matches
/// the by-path skill route and the CLI's `plan_or_noop`, so all delete surfaces
/// agree. Only `ResourceNotFound` is absorbed; every other error (IO, save
/// failure, unsupported scope, …) propagates as an actionable API error so a
/// genuine failure is never swallowed as success.
///
/// `outcome` is the result of a planned-removal call (already gated for
/// dry-run/confirm by the manager).
pub(crate) fn removal_or_noop(
	outcome: aghub_core::errors::Result<RemovalOutcome>,
) -> Result<rocket::serde::json::Json<DeleteSkillByPathResponse>, ApiError> {
	use aghub_core::errors::ConfigError;
	match outcome {
		Ok(outcome) => Ok(rocket::serde::json::Json(removal_response(outcome))),
		Err(ConfigError::ResourceNotFound { .. }) => Ok(
			rocket::serde::json::Json(noop_removal_response(vec![], vec![])),
		),
		Err(e) => Err(ApiError::from(e)),
	}
}

#[cfg(test)]
pub(crate) fn test_env_lock() -> &'static std::sync::Mutex<()> {
	static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> =
		std::sync::OnceLock::new();
	LOCK.get_or_init(|| std::sync::Mutex::new(()))
}

pub fn build_manager_from_resolved(
	agent: &AgentParam,
	scope: &ResolvedScope,
) -> Result<ConfigManager, ApiError> {
	let adapter = create_adapter(agent.0);
	match scope {
		ResolvedScope::Global => Ok(ConfigManager::new(adapter, true, None)),
		ResolvedScope::Project { root } => {
			Ok(ConfigManager::new(adapter, false, Some(root)))
		}
		ResolvedScope::All {
			project_root: Some(root),
		} => Ok(ConfigManager::with_scope(
			adapter,
			false,
			Some(root),
			ResourceScope::Both,
		)),
		ResolvedScope::All { project_root: None } => {
			Ok(ConfigManager::new(adapter, true, None))
		}
	}
}

pub fn require_writable_scope(scope: &ResolvedScope) -> Result<(), ApiError> {
	if scope.is_all() {
		return Err(ApiError::new(
            Status::MethodNotAllowed,
            "scope 'all' is read-only; use 'global' or 'project' for write operations",
            "READ_ONLY_SCOPE",
        ));
	}
	Ok(())
}

/// Map a resolved scope to the (ResourceScope, project_root) pair used by load_all_agents.
pub fn resolved_to_resource_scope(
	scope: &ResolvedScope,
) -> (ResourceScope, Option<PathBuf>) {
	match scope {
		ResolvedScope::Global => (ResourceScope::GlobalOnly, None),
		ResolvedScope::Project { root } => {
			(ResourceScope::ProjectOnly, Some(root.clone()))
		}
		ResolvedScope::All { project_root } => {
			(ResourceScope::Both, project_root.clone())
		}
	}
}

#[options("/<_path..>")]
pub fn preflight(_path: PathBuf) -> NoContent {
	NoContent
}

#[cfg(test)]
mod removal_or_noop_tests {
	use super::*;
	use aghub_core::errors::ConfigError;
	use aghub_core::skills::removal::{Layout, RemovalPlan};

	fn outcome(executed: bool) -> RemovalOutcome {
		RemovalOutcome {
			plan: RemovalPlan {
				layout: Layout::Copy,
				paths: vec![PathBuf::from("/x")],
				skipped: vec![],
				needs_confirm: false,
			},
			executed,
			prune: PruneStatus::NotRun,
		}
	}

	#[test]
	fn ok_outcome_passes_through() {
		let resp = removal_or_noop(Ok(outcome(true)))
			.ok()
			.expect("ok")
			.into_inner();
		assert!(resp.success);
		assert!(resp.executed);
		assert_eq!(resp.paths, vec!["/x".to_string()]);
	}

	#[test]
	fn resource_not_found_is_success_noop_not_error() {
		// The idempotent-delete contract: deleting an absent resource succeeds
		// as a no-op, never an error. Owned here so all delete routes agree.
		let resp = removal_or_noop(Err(ConfigError::resource_not_found(
			"mcp", "ghost",
		)))
		.ok()
		.expect("missing resource must be a success no-op")
		.into_inner();
		assert!(resp.success, "missing delete is success");
		assert!(!resp.executed, "no-op did not execute");
		assert!(resp.paths.is_empty());
		assert!(
			resp.deleted_path.is_none(),
			"no-op must not report a deleted path"
		);
	}

	#[test]
	fn other_errors_propagate_not_swallowed() {
		// Regression (#5 audit blocking): only ResourceNotFound is absorbed. A
		// genuine failure (e.g. an IO/save error) MUST surface as an API error
		// instead of being swallowed as a success no-op.
		let io = ConfigError::Io(std::io::Error::other("disk full"));
		assert!(
			removal_or_noop(Err(io)).is_err(),
			"a non-ResourceNotFound error must propagate, not be swallowed"
		);
	}
}
