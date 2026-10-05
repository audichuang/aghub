//! `repair` subcommand (alias `migrate`) — fix a skill's on-disk layout.
//!
//! **One verb for every non-conformant shape** — the user knows the skill
//! misbehaves, not which shape they hit; migration is just one of them.
//!
//! Built to be driven by an agent: `--json` carries the shape found, what was
//! done, every path involved, and for a refusal a `fix` that reads as an
//! instruction. Exit `1` when something was refused OR failed.
//!
//! Dry-run unless `--yes`; the preview is the same code path with writes
//! withheld (`dry_run`), never a parallel implementation.

use aghub_core::models::ResourceScope;
use aghub_core::skills::repair::{repair_all, RepairOutcome, RepairReport};
use anyhow::Result;
use serde_json::json;
use std::path::Path;

pub fn execute(
	scope: ResourceScope,
	project_root: Option<&Path>,
	name: Option<&str>,
	dry_run: bool,
	json_out: bool,
) -> Result<()> {
	// The worklist, the fail-closed lock read, the bulk mutation guard and the
	// per-skill error capture all live in `repair_all`. This used to be a
	// hand-written copy of the loop the API route also carried, and the two had
	// already drifted apart — see that function's docs.
	let reports = repair_all(scope, project_root, name, dry_run)?;

	let unresolved = reports.iter().any(|r| {
		matches!(
			r.outcome,
			RepairOutcome::Refused { .. } | RepairOutcome::Failed { .. }
		)
	});

	if json_out {
		println!(
			"{}",
			serde_json::to_string_pretty(&json!({
				"dry_run": dry_run,
				"scope": match scope {
					ResourceScope::ProjectOnly => "project",
					_ => "global",
				},
				"skills": reports,
			}))?
		);
	} else {
		render(&reports, dry_run);
	}

	if unresolved {
		// The JSON already said what and why; a second prose error on stderr
		// would be noise for the agent parsing stdout.
		std::process::exit(1);
	}
	Ok(())
}

fn render(reports: &[RepairReport], dry_run: bool) {
	if reports.is_empty() {
		println!("Nothing to repair.");
		return;
	}
	if dry_run {
		println!("Preview only — re-run with --yes to apply.\n");
	}
	for r in reports {
		match &r.outcome {
			RepairOutcome::Conformant => {
				println!("  ok        {}", r.name);
			}
			RepairOutcome::Migrated => {
				println!("  migrated  {}  -> {}", r.name, r.master.display());
			}
			RepairOutcome::Relinked => {
				println!("  relinked  {}", r.name);
			}
			RepairOutcome::Reconciled => {
				println!("  reconciled {}", r.name);
			}
			RepairOutcome::Tidied => {
				println!("  tidied    {}", r.name);
			}
			RepairOutcome::Refused { reason, fix } => {
				println!("  REFUSED   {}", r.name);
				println!("            why: {reason}");
				println!("            fix: {fix}");
			}
			RepairOutcome::Failed { reason, fix } => {
				// Distinct from REFUSED in the prose too: this one is worth
				// re-running, and a user who cannot tell them apart re-runs the
				// refusals forever instead of acting on their fix.
				println!("  FAILED    {}", r.name);
				println!("            why: {reason}");
				println!("            fix: {fix}");
			}
		}
		for referrer in &r.referrers {
			println!("            link: {}", referrer.display());
		}
		for stale in &r.unlinked {
			// A DETACH, spelled differently from `link:` — these lines are the
			// only notice the user gets that a path they may have created by
			// hand went away.
			println!("            unlinked: {}", stale.display());
		}
		if let Some(q) = &r.quarantined {
			println!("            kept:  {}", q.display());
		}
		// A refusal changed nothing, so "still shared" describes no outcome and
		// read as if those agents were involved in the refused row.
		let refused = matches!(r.outcome, RepairOutcome::Refused { .. });
		if !r.fused.is_empty() && !refused {
			// Say it plainly: these agents did NOT become individually
			// revocable, which is the thing the migration is sold on.
			println!(
				"            still shared by: {} (no private skills dir)",
				r.fused.join(", ")
			);
		}
	}
}
