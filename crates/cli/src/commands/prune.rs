//! `prune-lock` subcommand — disk-reconciled, lock-only skill pruning.
//!
//! Removes lock entries whose skill is no longer present on disk in the selected
//! scope. Lock-only: it never deletes skill files or edits agent config. Default
//! is a dry-run (reports what would be pruned). A disk-scan error aborts before
//! that scope's lock is written. For a `Both`-scope commit, the global and
//! project scans are preflighted before either lock is touched — but that is
//! not full atomicity. Three windows remain:
//!
//! 1. TOCTOU — a permission/mount change between the preflight scan and the
//!    real commit-time scan can still fail the project scan AFTER the global
//!    lock was already rewritten.
//! 2. The project lock WRITE itself (after its scan passes) can fail on its
//!    own, e.g. a read-only project root.
//! 3. Concurrency — a skill installed by another process between this scan and
//!    the lock rewrite is pruned from the lock anyway, because the rewrite
//!    applies a stale disk set. There is no interprocess mutation lock spanning
//!    install and prune (core's own prune path has the same window), so a
//!    concurrent install + prune is not safe in either surface.
//!
//! Windows 1 and 2 leave a partial mutation; when either happens and the global
//! scope had already pruned something, the reported JSON carries those keys
//! tagged with an `error` field, and the command still exits non-zero.

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

	// Committing (`!dry_run`) BOTH scopes: scan-check both up front, before
	// mutating either lock. `preview_prune` runs the exact same disk scan
	// `prune_lock_scanning` does, minus the write, so a scan error on either
	// side surfaces here instead of after the global lock below is already
	// committed. This buys back the documented all-or-nothing-on-scan-error
	// contract at the cost of scanning disk twice. See the module doc for the
	// three residual non-atomic windows this does NOT close: the scan-to-commit
	// TOCTOU, the lock WRITE itself, and a concurrent installer whose fresh
	// lock entry is pruned by a stale disk set. The first two are handled below
	// by reporting whatever the global scope already pruned alongside an
	// `error` field instead of bailing with empty stdout; the third needs an
	// interprocess lock that no surface has.
	if !dry_run && want_global && want_project {
		if let Some(root) = project_root {
			preview_prune(PruneScope::Global, None)?;
			preview_prune(PruneScope::Project, Some(root))?;
		}
	}

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
			match prune_lock_scanning(PruneScope::Project, Some(root)) {
				Ok(names) => names,
				Err(e) => {
					// Disclose ONLY a real partial mutation: a non-empty
					// `pruned` means the global scope above already
					// committed those keys, and bailing with empty
					// stdout would hide that. With nothing committed
					// (single-scope run, or a `Both` run that pruned no
					// global key) there is no partial state to report,
					// so stdout stays empty as before -- a caller that
					// treats "stdout parses as JSON" as success must not
					// start reading a failed prune as a clean one.
					if !pruned.is_empty() {
						println!(
							"{}",
							serde_json::to_string_pretty(&json!({
								"pruned": pruned,
								"dryRun": dry_run,
								"error": e.to_string(),
							}))?
						);
					}
					return Err(e.into());
				}
			}
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
