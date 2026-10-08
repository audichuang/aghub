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
#[cfg(all(test, unix))]
mod skills_test_git;
pub mod skills_update;
pub mod sources;
pub mod sub_agents;

use aghub_core::{
	create_adapter,
	manager::ConfigManager,
	models::ResourceScope,
	skills::removal::{PruneStatus, RemovalOutcome},
	WriteScope,
};
use rocket::http::Status;
use rocket::response::status::NoContent;
use std::path::PathBuf;

use crate::dto::skill::DeleteSkillByPathResponse;
use crate::error::ApiError;
use crate::extractors::{AgentParam, ResolvedScope};

pub(crate) fn project_prune_status(
	prune: PruneStatus,
) -> (Option<Vec<String>>, Option<Vec<String>>, Option<String>) {
	match prune {
		PruneStatus::NotRun => (None, None, None),
		// A preview's disclosure goes under its OWN key — a preview cannot
		// claim entries were dropped.
		PruneStatus::WouldPrune(keys) => (None, Some(keys), None),
		PruneStatus::Pruned(keys) => (Some(keys), None, None),
		PruneStatus::Failed { reason, pruned } => {
			(Some(pruned), None, Some(reason))
		}
	}
}

/// Map a [`RemovalOutcome`] to the shared [`DeleteSkillByPathResponse`] wire
/// shape, owned ONCE so the skill, MCP and sub-agent delete routes serialize
/// identically. The core fields come from `aghub_core::dto::RemovalView`; this
/// adds the api-only lock-prune fields (always None for MCP/sub-agent).
///
/// `requested_dry_run` is the CALLER's intent (`!confirm`), never inferred from
/// `!outcome.executed` — a confirmed delete of an absent target is not a
/// preview. See docs/history/api.md#removal-response-dry-run-inference
pub(crate) fn removal_response(
	outcome: RemovalOutcome,
	requested_dry_run: bool,
) -> DeleteSkillByPathResponse {
	let mut response = DeleteSkillByPathResponse::from(
		&aghub_core::dto::RemovalView::from_outcome(
			&outcome,
			requested_dry_run,
		),
	);
	let (pruned_lock_entries, would_prune_lock_entries, prune_error) =
		project_prune_status(outcome.prune);
	response.pruned_lock_entries = pruned_lock_entries;
	response.would_prune_lock_entries = would_prune_lock_entries;
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
	requested_dry_run: bool,
) -> DeleteSkillByPathResponse {
	let mut outcome = aghub_core::skills::removal::RemovalOutcome::noop(false);
	outcome.plan.paths = paths;
	outcome.plan.skipped = skipped;
	removal_response(outcome, requested_dry_run)
}

/// Parse a delete route's optional `agents` query (comma list): every agent
/// ONE user action removes the resource from, so a shared file or Referrer
/// may go when all of its readers are in the set. Absent means only the path
/// agent; the path agent is always included; an unknown id is a 400.
pub(crate) fn requested_delete_agents(
	agent: aghub_core::models::AgentType,
	agents: Option<&str>,
) -> Result<Vec<aghub_core::models::AgentType>, ApiError> {
	let mut requested = vec![agent];
	if let Some(s) = agents {
		for parsed in crate::extractors::resolve_agent_list(s)? {
			if !requested.contains(&parsed) {
				requested.push(parsed);
			}
		}
	}
	Ok(requested)
}

/// The API's **idempotent-delete contract**, owned in ONE place so the skill,
/// MCP and sub-agent delete routes apply it identically instead of each
/// open-coding an `Err(ResourceNotFound) => noop` arm.
///
/// Deleting an already-absent resource is a **success no-op**
/// (`success:true, executed:false, deleted_path:null`), matching the by-path
/// route and the CLI's `plan_or_noop`. Only `ResourceNotFound` is absorbed;
/// every other error propagates so a real failure is never reported as success.
/// `outcome` is a planned-removal result (already dry-run/confirm gated).
pub(crate) fn removal_or_noop(
	outcome: aghub_core::errors::Result<RemovalOutcome>,
	requested_dry_run: bool,
	noop: impl FnOnce() -> RemovalOutcome,
) -> Result<rocket::serde::json::Json<DeleteSkillByPathResponse>, ApiError> {
	use aghub_core::errors::ConfigError;
	match outcome {
		Ok(outcome) => Ok(rocket::serde::json::Json(removal_response(
			outcome,
			requested_dry_run,
		))),
		Err(ConfigError::ResourceNotFound { .. }) => {
			Ok(rocket::serde::json::Json(removal_response(
				noop(),
				requested_dry_run,
			)))
		}
		Err(e) => Err(ApiError::from(e)),
	}
}

#[cfg(test)]
pub(crate) fn test_env_lock() -> &'static std::sync::Mutex<()> {
	static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> =
		std::sync::OnceLock::new();
	LOCK.get_or_init(|| std::sync::Mutex::new(()))
}

/// Restores the process CWD on drop, so a mid-test panic can never leave
/// the process standing in a soon-deleted temp dir.
#[cfg(all(test, unix))]
pub(crate) struct CwdGuard(std::path::PathBuf);

#[cfg(all(test, unix))]
impl CwdGuard {
	pub(crate) fn change_to(dir: &std::path::Path) -> Self {
		let prev = std::env::current_dir().unwrap();
		std::env::set_current_dir(dir).unwrap();
		Self(prev)
	}
}

#[cfg(all(test, unix))]
impl Drop for CwdGuard {
	fn drop(&mut self) {
		let _ = std::env::set_current_dir(&self.0);
	}
}

pub fn build_manager_from_resolved(
	agent: &AgentParam,
	scope: &ResolvedScope,
) -> Result<ConfigManager, ApiError> {
	let adapter = create_adapter(agent.0);
	match scope {
		ResolvedScope::Global => {
			Ok(ConfigManager::for_write(adapter, WriteScope::Global))
		}
		ResolvedScope::Project { root } => Ok(ConfigManager::for_write(
			adapter,
			WriteScope::project(root.clone()),
		)),
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

/// Map a resolved scope to the (ResourceScope, project_root) pair used by
/// load_all_agents.
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
	use crate::dto::skill::RemovalOutcomeKind;
	use aghub_core::errors::ConfigError;
	use aghub_core::skills::removal::{Layout, RemovalPlan, Verdict};

	fn outcome(executed: bool) -> RemovalOutcome {
		RemovalOutcome {
			plan: RemovalPlan {
				layout: Layout::Copy,
				paths: vec![PathBuf::from("/x")],
				skipped: vec![],
				needs_confirm: false,
				shared_master_kept: false,
				still_read_from: Vec::new(),
				incomplete: false,
			},
			executed,
			prune: PruneStatus::NotRun,
			failed_paths: vec![],
			absent: false,
			verdict: Verdict::Removed,
		}
	}

	#[test]
	fn ok_outcome_passes_through() {
		let resp = removal_or_noop(Ok(outcome(true)), false, || {
			RemovalOutcome::noop(false)
		})
		.ok()
		.expect("ok")
		.into_inner();
		assert!(resp.success);
		assert!(resp.executed);
		assert_eq!(resp.paths, vec!["/x".to_string()]);
		assert_eq!(resp.outcome, RemovalOutcomeKind::Removed);
	}

	#[test]
	fn resource_not_found_is_success_noop_not_error() {
		// The idempotent-delete contract: deleting an absent resource succeeds
		// as a no-op, never an error. Owned here so all delete routes agree.
		let resp = removal_or_noop(
			Err(ConfigError::resource_not_found("mcp", "ghost")),
			// CONFIRMED: the caller asked for a real delete and the resource
			// was already gone. That is `absent`, not a dry-run.
			false,
			|| RemovalOutcome::noop(false),
		)
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
		assert!(
			!resp.dry_run,
			"a confirmed delete of an absent resource is not a dry-run"
		);
		assert_eq!(
			resp.outcome,
			RemovalOutcomeKind::Absent,
			"an already-gone resource must be distinguishable from a preview"
		);
	}

	#[test]
	fn other_errors_propagate_not_swallowed() {
		// Regression (#5 audit blocking): only ResourceNotFound is absorbed. A
		// genuine failure (e.g. an IO/save error) MUST surface as an API error
		// instead of being swallowed as a success no-op.
		let io = ConfigError::Io(std::io::Error::other("disk full"));
		assert!(
			removal_or_noop(Err(io), false, || RemovalOutcome::noop(false))
				.is_err(),
			"a non-ResourceNotFound error must propagate, not be swallowed"
		);
	}

	#[test]
	fn requested_delete_agents_matches_cli_a_empty_token_and_duplicate_semantics(
	) {
		use aghub_core::models::AgentType;

		// Absent: returns only path agent
		let single = match requested_delete_agents(AgentType::Claude, None) {
			Ok(agents) => agents,
			Err(err) => panic!("expected Ok, got Err({})", err.body.code),
		};
		assert_eq!(single, vec![AgentType::Claude]);

		// Duplicates in list and with path agent: deduped preserving order, path agent first
		let deduped = match requested_delete_agents(
			AgentType::Claude,
			Some("copilot,claude,copilot,cursor"),
		) {
			Ok(agents) => agents,
			Err(err) => panic!("expected Ok, got Err({})", err.body.code),
		};
		assert_eq!(
			deduped,
			vec![AgentType::Claude, AgentType::Copilot, AgentType::Cursor]
		);

		// Empty token (trailing comma, double comma, empty string): rejected with INVALID_PARAM
		for bad in ["claude,", "claude,,cursor", "", "  "] {
			let err =
				match requested_delete_agents(AgentType::Claude, Some(bad)) {
					Ok(_) => panic!("expected Err for {bad:?}, got Ok"),
					Err(err) => err,
				};
			assert_eq!(err.status, Status::BadRequest);
			assert_eq!(err.body.code, "INVALID_PARAM");
		}
	}
}
