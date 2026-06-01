//! `prune-lock` subcommand — disk-reconciled, lock-only skill pruning.
//!
//! Removes lock entries whose skill is no longer present on disk in the selected
//! scope. Lock-only: it never deletes skill files or edits agent config. Default
//! is a dry-run (reports what would be pruned). Any disk-scan error aborts the
//! prune with a non-zero exit and leaves the lock untouched.

use crate::eprintln_verbose;
use aghub_core::models::ResourceScope;
use aghub_core::skills::prune::{
	preview_prune, prune_lock_scanning, PruneScope,
};
use anyhow::{bail, Result};
use serde_json::json;
use std::path::Path;

pub fn execute(
	scope: ResourceScope,
	project_root: Option<&Path>,
	dry_run: bool,
	_json: bool,
) -> Result<()> {
	let want_global =
		matches!(scope, ResourceScope::GlobalOnly | ResourceScope::Both);
	let want_project =
		matches!(scope, ResourceScope::ProjectOnly | ResourceScope::Both);

	let mut pruned: Vec<String> = Vec::new();

	if want_global {
		eprintln_verbose!("Pruning global skill lock (dry_run={dry_run})");
		let names = if dry_run {
			preview_prune(PruneScope::Global, None)?
		} else {
			prune_lock_scanning(PruneScope::Global, None)?
		};
		pruned.extend(names);
	}

	if want_project {
		let Some(root) = project_root else {
			if matches!(scope, ResourceScope::ProjectOnly) {
				bail!(
					"project root is required for project skill lock pruning"
				);
			}
			eprintln_verbose!(
				"Skipping project skill lock prune: no project root"
			);
			println!(
				"{}",
				serde_json::to_string_pretty(&json!({
					"pruned": pruned,
					"dryRun": dry_run,
				}))?
			);
			return Ok(());
		};
		eprintln_verbose!("Pruning project skill lock (dry_run={dry_run})");
		let names = if dry_run {
			preview_prune(PruneScope::Project, Some(root))?
		} else {
			prune_lock_scanning(PruneScope::Project, Some(root))?
		};
		pruned.extend(names);
	}

	println!(
		"{}",
		serde_json::to_string_pretty(&json!({
			"pruned": pruned,
			"dryRun": dry_run,
		}))?
	);
	Ok(())
}
