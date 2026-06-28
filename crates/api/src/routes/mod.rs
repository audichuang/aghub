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
