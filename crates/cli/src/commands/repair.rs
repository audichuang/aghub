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
use aghub_core::skills::repair::{execute_repair, RepairOutcome, RepairReport};
use aghub_core::skills::shape::{plan_repair, readers_of};
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

	let mut reports: Vec<RepairReport> = Vec::new();
	for skill_name in &worklist {
		let Some(plan) = plan_repair(
			scope,
			project_root,
			skill_name,
			in_lock.contains(skill_name),
			// Answered against the CURRENT layout, before anything moves: an
			// agent that reads the skill today is owed an explicit Referrer
			// once the Master leaves the slot it was reading.
			&readers_of(scope, project_root, skill_name),
		) else {
			continue;
		};
		// In a bulk run, silence about the already-correct skills is the point;
		// a named one still reports so the user learns it was fine.
		if plan.is_noop() && name.is_none() {
			continue;
		}
		reports.push(execute_repair(&plan, dry_run)?);
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
	}
}
