use crate::{eprintln_verbose, ResourceType};
use aghub_core::manager::ConfigManager;
use aghub_core::skills::removal::PruneStatus;
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
			// its result is reported via outcome.prune.
			let outcome = manager.remove_skill_planned(
				&name,
				options.all_agents,
				is_dry_run,
				options.yes,
			)?;
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
			eprintln_verbose!("Deleting MCP server: {}", name);
			manager.remove_mcp(&name)?;
			eprintln_verbose!("MCP server deleted successfully");
			println!(
				"{}",
				serde_json::to_string_pretty(
					&json!({"deleted": true, "name": name, "type": "mcp" })
				)?
			);
		}
	}

	Ok(())
}

/// Render a skill removal's [`PruneStatus`] onto the JSON `payload`, matching
/// the API's `DeleteSkillByPathResponse` fields:
///
/// - `NotRun` → no keys (no prune was attempted).
/// - `Pruned(keys)` → `prunedLockEntries` (empty = ran, nothing orphaned).
/// - `Failed { reason, pruned }` → `pruneError` plus `prunedLockEntries` for
///   the keys dropped BEFORE the failure. The list is ALWAYS emitted (even
///   when empty) so the CLI and API agree on the `Failed` shape; a `Both`-scope
///   prune can leave a non-empty partial here.
fn apply_prune_fields(payload: &mut serde_json::Value, prune: &PruneStatus) {
	match prune {
		PruneStatus::NotRun => {}
		PruneStatus::Pruned(keys) => {
			payload["prunedLockEntries"] = json!(keys);
		}
		PruneStatus::Failed { reason, pruned } => {
			payload["pruneError"] = json!(reason);
			payload["prunedLockEntries"] = json!(pruned);
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
		assert!(payload.get("prunedLockEntries").is_none());
		assert!(payload.get("pruneError").is_none());
	}

	#[test]
	fn prune_pruned_sets_lock_entries_only() {
		let mut payload = json!({});
		let prune = PruneStatus::Pruned(vec!["a".into(), "b".into()]);
		apply_prune_fields(&mut payload, &prune);
		assert_eq!(payload["prunedLockEntries"], json!(["a", "b"]));
		assert!(payload.get("pruneError").is_none());
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
		assert_eq!(payload["pruneError"], json!("boom"));
		assert_eq!(payload["prunedLockEntries"], json!(["g1"]));
	}

	#[test]
	fn prune_failed_empty_keys_still_emits_empty_lock_entries() {
		// Consistency with the API: `Failed { pruned: [] }` emits an empty
		// `prunedLockEntries` plus the error, not a missing field.
		let mut payload = json!({});
		let prune = PruneStatus::Failed {
			reason: "boom".into(),
			pruned: vec![],
		};
		apply_prune_fields(&mut payload, &prune);
		assert_eq!(payload["pruneError"], json!("boom"));
		assert_eq!(payload["prunedLockEntries"], json!([]));
	}
}
