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
//! repaired, `1` when something was refused.
//!
//! Dry-run unless `--yes`, per the house rule for layout-changing verbs. The
//! preview is the same code path with the writes withheld — `execute_repair`
//! takes a `dry_run` flag rather than the caller running a second, parallel
//! "what would happen" implementation that drifts.

use aghub_core::models::ResourceScope;
use aghub_core::skills::repair::{repair_skill, RepairOutcome, RepairReport};
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
	// Fail CLOSED. The lock IS this command's worklist (D5) and it decides
	// which directories may be adopted as a Master, so an unreadable lock must
	// not read as "nothing to repair" — that answer looks like success.
	let want_global = matches!(scope, ResourceScope::GlobalOnly);
	let locks = crate::commands::read_locks_checked(want_global, project_root)?;
	let in_lock: std::collections::BTreeSet<String> = locks
		.global
		.iter()
		.flat_map(|l| l.skills.keys().cloned())
		.chain(locks.project.iter().flat_map(|l| l.skills.keys().cloned()))
		.collect();

	// A named skill is repaired whether or not the lock knows it — the lock
	// only decides ADOPTION, and refusing to look at an unlocked skill would
	// leave the user with no way to diagnose it.
	let worklist: Vec<String> = match name {
		Some(one) => vec![one.to_string()],
		None => in_lock.iter().cloned().collect(),
	};

	// ONE guard around the whole bulk run, not one per skill. `repair_skill`
	// takes its own (reentrant per thread, so the inner acquire is free), but
	// without this outer one a 20-skill migration would be twenty independently
	// racing mutations that another aghub could interleave halfway through.
	// Dry runs take none, matching the seam.
	let _bulk_guard = if dry_run {
		None
	} else {
		Some(aghub_core::skills::lock::mutation_guard(
			"skill repair (bulk)",
			scope,
			project_root,
		)?)
	};

	let mut reports: Vec<RepairReport> = Vec::new();
	for skill_name in &worklist {
		// Through the core seam, which takes the interprocess mutation lock
		// across plan AND write. Calling `execute_repair` here instead would
		// leave the hash-compare → rename window open to a concurrent aghub.
		let Some(report) = repair_skill(
			scope,
			project_root,
			skill_name,
			in_lock.contains(skill_name),
			dry_run,
		)?
		else {
			continue;
		};
		// In a bulk run, silence about the already-correct skills is the point;
		// a named one still reports so the user learns it was fine.
		if report.outcome == RepairOutcome::Conformant && name.is_none() {
			continue;
		}
		reports.push(report);
	}

	let refused = reports
		.iter()
		.any(|r| matches!(r.outcome, RepairOutcome::Refused { .. }));

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

	if refused {
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
