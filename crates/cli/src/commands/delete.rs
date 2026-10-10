use crate::{eprintln_verbose, ResourceType};
use aghub_core::errors::ConfigError;
use aghub_core::manager::ConfigManager;
use aghub_core::skills::removal::{
	self, apply_prune_fields, RemovalOutcome, SkillRemovalRequest,
	SkillRemovalTarget,
};
use anyhow::Result;
use serde_json::json;

pub struct DeleteOptions {
	pub all_agents: bool,
	pub dry_run: bool,
	pub yes: bool,
	/// Every agent this command deletes from; always contains the manager's.
	pub requested_agents: Vec<aghub_core::models::AgentType>,
	pub scope: aghub_core::WriteScope,
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
) -> Result<serde_json::Value> {
	// The caller prints the payload (single-agent) or wraps it in the batch
	// envelope (multi-agent) — command logic stays print-free.
	let payload = match resource {
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
			let request = SkillRemovalRequest {
				target: SkillRemovalTarget::ByName(name.clone()),
				scope: options.scope,
				agents: options.requested_agents.clone(),
				dry_run: is_dry_run,
				all_agents: options.all_agents,
				prior_removed_paths: Vec::new(),
				keeps_master: false,
				plugin_roots: super::plugin_roots(),
			};
			let resp = removal::remove_skill_batch(&request)
				.map_err(anyhow::Error::from)?;
			let single = resp
				.to_single_view(is_dry_run)
				.map_err(anyhow::Error::from)?;
			let mut payload = serde_json::to_value(&single.removal_view)?;
			payload["type"] = json!("skill");
			payload["name"] = json!(name);
			apply_prune_fields(&mut payload, &single.prune);
			payload
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
			let outcome = plan_or_noop(
				manager,
				|_| RemovalOutcome::noop(false),
				|m| {
					m.remove_mcp_planned_single_guarded(
						&name,
						is_dry_run,
						options.yes,
						&options.requested_agents,
					)
				},
			)?;
			// Reuse the shared core RemovalView so the removal fields stay
			// snake_case and byte-identical to the skills branch + the API +
			// desktop DeleteSkillByPathResponse; layer the CLI {type,name}
			// envelope on top. MCP removal has no lock prune.
			let view = aghub_core::dto::RemovalView::from_outcome(
				&outcome, is_dry_run,
			);
			let mut payload = serde_json::to_value(&view)?;
			payload["type"] = json!("mcp");
			payload["name"] = json!(name);
			payload
		}
		ResourceType::SubAgents => {
			if options.all_agents {
				anyhow::bail!("--all-agents is not valid for sub-agents");
			}
			let is_dry_run = options.dry_run || !options.yes;
			eprintln_verbose!(
				"Removing sub-agent '{}' (dry_run={})",
				name,
				is_dry_run
			);
			let outcome = plan_or_noop(
				manager,
				|_| RemovalOutcome::noop(false),
				|m| m.remove_sub_agent_planned(&name, is_dry_run, options.yes),
			)?;
			let view = aghub_core::dto::RemovalView::from_outcome(
				&outcome, is_dry_run,
			);
			let mut payload = serde_json::to_value(&view)?;
			payload["type"] = json!("sub-agent");
			payload["name"] = json!(name);
			payload
		}
	};

	Ok(payload)
}

/// Run a planned-removal closure, mapping the two "already gone" cases to a
/// shared no-op [`RemovalOutcome`] (`success:true, executed:false`) so the CLI
/// matches the API's idempotent-delete contract instead of erroring:
///
/// - **No config loaded** (the file never existed; `main.rs` tolerated the
///   missing config for `delete`): nothing to remove.
/// - **`ResourceNotFound`**: the config loaded but has no such resource.
///
/// All other errors propagate. The no-op shape comes from `noop`, so the CLI
/// and API serialize byte-identically.
fn plan_or_noop(
	manager: &mut ConfigManager,
	noop: impl FnOnce(&ConfigManager) -> RemovalOutcome,
	plan: impl FnOnce(
		&mut ConfigManager,
	) -> aghub_core::errors::Result<RemovalOutcome>,
) -> Result<RemovalOutcome> {
	if manager.config().is_none() {
		return Ok(noop(manager));
	}
	match plan(manager) {
		Ok(outcome) => Ok(outcome),
		Err(ConfigError::ResourceNotFound { .. }) => Ok(noop(manager)),
		Err(e) => Err(e.into()),
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use aghub_core::skills::removal::PruneStatus;

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
