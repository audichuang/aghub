//! `prune-lock` subcommand — disk-reconciled, lock-only skill pruning.
//!
//! Removes lock entries whose skill is no longer present on disk in the selected
//! scope. Lock-only: it never deletes skill files or edits agent config. Default
//! is a dry-run (reports what would be pruned). A disk-scan error aborts before
//! that scope's lock is written. For a `Both`-scope commit, the global and
//! project scans are preflighted before either lock is touched — but that is
//! not full atomicity. Two windows remain:
//!
//! 1. TOCTOU — a permission/mount change between the preflight scan and the
//!    real commit-time scan can still fail the project scan AFTER the global
//!    lock was already rewritten.
//! 2. The project lock WRITE itself (after its scan passes) can fail on its
//!    own, e.g. a read-only project root.
//!
//! Both leave a partial mutation; when either happens and the global scope had
//! already pruned something, the reported JSON carries those keys tagged with an
//! `error` field, and the command still exits non-zero.
//!
//! The concurrency window that used to be third here is CLOSED: core's
//! `prune_lock_from_dirs` holds the interprocess mutation lock across its scan
//! AND its rewrite, so a skill another aghub process installs can no longer be
//! pruned by a disk set that predates it. `npx skills` still takes no lock of
//! ours. Windows 1/2 are per-scope-sequential, not concurrency — one lock per
//! scope cannot make two independent lock files commit atomically.

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
	json: bool,
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
	// two residual non-atomic windows this does NOT close: the scan-to-commit
	// TOCTOU and the lock WRITE itself. Both are handled below by reporting
	// whatever the global scope already pruned alongside an `error` field
	// instead of bailing with empty stdout. Neither is concurrency — core's
	// prune holds the interprocess mutation lock over scan+rewrite — so this
	// preflight stays: the lock cannot make two lock files commit atomically.
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
			report(&pruned, dry_run, None, json)?;
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
						report(&pruned, dry_run, Some(&e.to_string()), json)?;
					}
					return Err(e.into());
				}
			}
		};
		pruned.extend(names);
	}

	report(&pruned, dry_run, None, json)
}

/// Emit the prune result: a human summary by default, the unchanged
/// `{pruned, dryRun[, error]}` object under `--json`.
///
/// A dry run MUST say how to commit it — the JSON encoded that in `dryRun`,
/// which a human reading a bare key list never saw, so "nothing happened" and
/// "entries were dropped" printed identically.
fn report(
	pruned: &[String],
	dry_run: bool,
	error: Option<&str>,
	json: bool,
) -> Result<()> {
	if json {
		let mut payload = json!({ "pruned": pruned, "dryRun": dry_run });
		if let Some(error) = error {
			payload["error"] = json!(error);
		}
		println!("{}", serde_json::to_string_pretty(&payload)?);
		return Ok(());
	}
	if pruned.is_empty() {
		println!("No orphaned lock entries.");
	} else {
		let verb = if dry_run { "would prune" } else { "pruned" };
		println!("{verb} {} lock entry(ies):", pruned.len());
		for key in pruned {
			println!("  {key}");
		}
		if dry_run {
			println!("re-run with --yes to write the pruned lock");
		}
	}
	if let Some(error) = error {
		println!("error after a partial prune: {error}");
	}
	Ok(())
}
