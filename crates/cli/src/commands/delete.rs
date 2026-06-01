use crate::{eprintln_verbose, ResourceType};
use aghub_core::manager::ConfigManager;
use aghub_core::models::ResourceScope;
use aghub_core::skills::prune::{prune_lock_scanning, PruneScope};
use anyhow::Result;
use serde_json::json;
use std::path::Path;

pub struct DeleteOptions<'a> {
	pub scope: ResourceScope,
	pub project_root: Option<&'a Path>,
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
/// `dryRun`, the exact `paths`, and any `skipped` (out-of-allowlist) paths.
pub fn execute(
	manager: &mut ConfigManager,
	resource: ResourceType,
	name: String,
	options: DeleteOptions<'_>,
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
			let outcome = manager.remove_skill_planned(
				&name,
				options.all_agents,
				is_dry_run,
				options.yes,
			)?;
			if outcome.executed {
				match options.scope {
					ResourceScope::GlobalOnly => {
						let _ = prune_lock_scanning(PruneScope::Global, None);
					}
					ResourceScope::ProjectOnly => {
						if let Some(root) = options.project_root {
							let _ = prune_lock_scanning(
								PruneScope::Project,
								Some(root),
							);
						}
					}
					ResourceScope::Both => {
						let _ = prune_lock_scanning(PruneScope::Global, None);
						if let Some(root) = options.project_root {
							let _ = prune_lock_scanning(
								PruneScope::Project,
								Some(root),
							);
						}
					}
				}
			}
			let paths: Vec<String> = outcome
				.plan
				.paths
				.iter()
				.map(|p| p.display().to_string())
				.collect();
			let skipped: Vec<String> = outcome
				.plan
				.skipped
				.iter()
				.map(|p| p.display().to_string())
				.collect();
			println!(
				"{}",
				serde_json::to_string_pretty(&json!({
					"type": "skill",
					"name": name,
					"dryRun": !outcome.executed,
					"executed": outcome.executed,
					"needsConfirm": outcome.plan.needs_confirm,
					"paths": paths,
					"skipped": skipped,
				}))?
			);
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
