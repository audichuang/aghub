//! `repair` subcommand (alias `migrate`) — fix a skill's on-disk layout.
//!
//! **One verb for every non-conformant shape.** From inside an agent session the
//! user does not know WHICH shape they hit — they know the skill is
//! misbehaving. Migration is what repair does to an un-migrated skill; it is not
//! a separate command.
//!
//! Designed to be driven by an agent, which is a real constraint on the output
//! and not a nicety: `--json` carries the shape found, what was done, every path
//! involved, and for a refusal a `fix` string that reads as an instruction
//! rather than a diagnosis. Exit `0` when nothing needed doing or everything was
//! repaired, `1` when something was refused OR failed — both mean a skill the
//! user still has to deal with.
//!
//! Dry-run unless `--yes`, per the house rule for layout-changing verbs. The
//! preview is the same code path with the writes withheld — `execute_repair`
//! takes a `dry_run` flag rather than the caller running a second, parallel
//! "what would happen" implementation that drifts.

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
		if let Some(q) = &r.quarantined {
			println!("            kept:  {}", q.display());
		}
		if !r.fused.is_empty() {
			// Say it plainly: these agents did NOT become individually
			// revocable, which is the thing the migration is sold on.
			println!(
				"            still shared by: {} (no private skills dir)",
				r.fused.join(", ")
			);
		}
	}
}
