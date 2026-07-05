use crate::{eprintln_verbose, ResourceType};
use aghub_core::errors::ConfigError;
use aghub_core::manager::ConfigManager;
use aghub_core::skills::removal::{PruneStatus, RemovalOutcome};
use anyhow::Result;
use serde_json::json;

pub struct DeleteOptions {
	pub all_agents: bool,
	pub dry_run: bool,
	pub yes: bool,
}

/// Delete a resource.
///
/// Skills use the layout-aware planner with a **default dry-run**: without
/// `--yes` (or with `--dry-run`) it only reports the paths that would be
/// removed. `--all-agents` extends a copy-layout removal across every agent and
/// is destructive, so it too requires `--yes`. The emitted JSON carries
/// `dry_run`, the exact `paths`, and any `skipped` (out-of-allowlist) paths
/// (snake_case, from the shared `aghub_core::dto::RemovalView`).
pub fn execute(
	manager: &mut ConfigManager,
	resource: ResourceType,
	name: String,
	options: DeleteOptions,
) -> Result<()> {
	match resource {
		ResourceType::Skills => {
			// Default is a dry-run; --yes performs the removal, --dry-run forces
			// a preview even if --yes was also passed.
			let is_dry_run = options.dry_run || !options.yes;
			eprintln_verbose!(
				"Removing skill '{}' (all_agents={}, dry_run={})",
				name,
				options.all_agents,
				is_dry_run
			);
			// The lock prune happens inside remove_skill_planned on execute;
			// its result is reported via outcome.prune. A missing config or a
			// missing skill is an idempotent no-op (matches the API), not an
			// error — see `plan_or_noop`.
			let outcome = plan_or_noop(manager, |m| {
				m.remove_skill_planned(
					&name,
					options.all_agents,
					is_dry_run,
					options.yes,
				)
			})?;
			// Serialize the shared core builder so the removal fields
			// (success/dry_run/executed/needs_confirm/paths/skipped/
			// deleted_path) live once and stay snake_case, matching the API +
			// desktop DeleteSkillByPathResponse. Then layer the CLI-only
			// {type,name} envelope and the prune status on top.
			let view = aghub_core::dto::RemovalView::from(&outcome);
			let mut payload = serde_json::to_value(&view)?;
			payload["type"] = json!("skill");
			payload["name"] = json!(name);
			apply_prune_fields(&mut payload, &outcome.prune);
			println!("{}", serde_json::to_string_pretty(&payload)?);
		}
		ResourceType::Mcps => {
			// Same gate as skills: default dry-run, --yes executes, --dry-run
			// forces a preview even alongside --yes.
			let is_dry_run = options.dry_run || !options.yes;
			eprintln_verbose!(
				"Removing MCP server '{}' (dry_run={})",
				name,
				is_dry_run
			);
			// Missing config / missing MCP is an idempotent no-op (matches the
			// API), not an error — see `plan_or_noop`.
			let outcome = plan_or_noop(manager, |m| {
				m.remove_mcp_planned(&name, is_dry_run, options.yes)
			})?;
			// Reuse the shared core RemovalView so the removal fields stay
			// snake_case and byte-identical to the skills branch + the API +
			// desktop DeleteSkillByPathResponse; layer the CLI {type,name}
			// envelope on top. MCP removal has no lock prune.
			let view = aghub_core::dto::RemovalView::from(&outcome);
			let mut payload = serde_json::to_value(&view)?;
			payload["type"] = json!("mcp");
			payload["name"] = json!(name);
			println!("{}", serde_json::to_string_pretty(&payload)?);
		}
	}

	Ok(())
}

/// Run a planned-removal closure, mapping the two "already gone" cases to a
/// shared no-op [`RemovalOutcome`] (`success:true, executed:false`) so the CLI
/// matches the API's idempotent-delete contract instead of erroring (#5 audit):
///
/// - **No config loaded** (the file never existed; `main.rs` tolerated the
///   missing config for `delete`): nothing to remove.
/// - **`ResourceNotFound`**: the config loaded but has no such resource.
///
/// All other errors propagate. The no-op shape comes from the same
/// `RemovalOutcome::noop()` the API's `noop_removal_response` uses, so the two
/// surfaces serialize byte-identically.
fn plan_or_noop(
	manager: &mut ConfigManager,
	plan: impl FnOnce(
		&mut ConfigManager,
	) -> aghub_core::errors::Result<RemovalOutcome>,
) -> Result<RemovalOutcome> {
	if manager.config().is_none() {
		return Ok(RemovalOutcome::noop());
	}
	match plan(manager) {
		Ok(outcome) => Ok(outcome),
		Err(ConfigError::ResourceNotFound { .. }) => Ok(RemovalOutcome::noop()),
		Err(e) => Err(e.into()),
	}
}

/// Render a skill removal's [`PruneStatus`] onto the JSON `payload`, matching
/// the API's `DeleteSkillByPathResponse` fields. Keys are **snake_case**
/// (`pruned_lock_entries`/`prune_error`) so the CLI and the API/desktop DTO are
/// one wire shape — the same convention the shared `RemovalView` uses:
///
/// - `NotRun` → no keys (no prune was attempted).
/// - `Pruned(keys)` → `pruned_lock_entries` (empty = ran, nothing orphaned).
/// - `Failed { reason, pruned }` → `prune_error` plus `pruned_lock_entries` for
///   the keys dropped BEFORE the failure. The list is ALWAYS emitted (even
///   when empty) so the CLI and API agree on the `Failed` shape; a `Both`-scope
///   prune can leave a non-empty partial here.
fn apply_prune_fields(payload: &mut serde_json::Value, prune: &PruneStatus) {
	match prune {
		PruneStatus::NotRun => {}
		PruneStatus::Pruned(keys) => {
			payload["pruned_lock_entries"] = json!(keys);
		}
		PruneStatus::Failed { reason, pruned } => {
			payload["prune_error"] = json!(reason);
			payload["pruned_lock_entries"] = json!(pruned);
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn prune_not_run_adds_no_keys() {
		let mut payload = json!({});
		apply_prune_fields(&mut payload, &PruneStatus::NotRun);
		assert!(payload.get("pruned_lock_entries").is_none());
		assert!(payload.get("prune_error").is_none());
	}

	#[test]
	fn prune_pruned_sets_lock_entries_only() {
		let mut payload = json!({});
		let prune = PruneStatus::Pruned(vec!["a".into(), "b".into()]);
		apply_prune_fields(&mut payload, &prune);
		assert_eq!(payload["pruned_lock_entries"], json!(["a", "b"]));
		assert!(payload.get("prune_error").is_none());
		// One wire shape with the API: never emit the legacy camelCase keys.
		assert!(payload.get("prunedLockEntries").is_none());
	}

	#[test]
	fn prune_failed_with_partial_keys_sets_both() {
		// Regression: a `Both`-scope failure after the global lock pruned must
		// surface BOTH the error and the already-dropped keys.
		let mut payload = json!({});
		let prune = PruneStatus::Failed {
			reason: "boom".into(),
			pruned: vec!["g1".into()],
		};
		apply_prune_fields(&mut payload, &prune);
		assert_eq!(payload["prune_error"], json!("boom"));
		assert_eq!(payload["pruned_lock_entries"], json!(["g1"]));
		assert!(payload.get("pruneError").is_none());
		assert!(payload.get("prunedLockEntries").is_none());
	}

	#[test]
	fn prune_failed_empty_keys_still_emits_empty_lock_entries() {
		// Consistency with the API: `Failed { pruned: [] }` emits an empty
		// `pruned_lock_entries` plus the error, not a missing field.
		let mut payload = json!({});
		let prune = PruneStatus::Failed {
			reason: "boom".into(),
			pruned: vec![],
		};
		apply_prune_fields(&mut payload, &prune);
		assert_eq!(payload["prune_error"], json!("boom"));
		assert_eq!(payload["pruned_lock_entries"], json!([]));
	}
}
