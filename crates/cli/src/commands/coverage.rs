//! `aghub-cli coverage` — read-only projection of `classify_all`.
//!
//! Classifies every registered agent against the canonical `.agents/skills`
//! master SKILLS-DIR for the requested scope and prints a per-agent coverage
//! table (or JSON). A pure passthrough over `aghub_core::skills::linker`: it
//! adds no write path. The `--json` shape comes from the SHARED
//! `AgentSkillCoverageView` (core), the exact type the HTTP API's
//! `AgentSkillCoverageDto` mirrors — so the two surfaces emit one wire shape
//! defined in one place and can never drift.

use aghub_core::skills::linker::classify::{classify_all, LinkNeed};
use aghub_core::skills::linker::{master_store_dir, AgentSkillCoverageView};
use anyhow::Result;
use tabled::builder::Builder;
use tabled::settings::Style;

/// Dispatch the `coverage` subcommand.
///
/// Coverage supports only `global` or `project` scope (the master is resolved
/// per-scope); `--all`/`Both` has no single master to classify against and is
/// rejected — by `COVERAGE_SCOPE` in `main`'s ONE policy table, mirroring the
/// API route, which rejects `scope=all`. So is `-p` with no project root.
pub fn execute(resolved: &crate::Scope, json: bool) -> Result<()> {
	let scope = resolved.resource_scope();
	let project_root = resolved.project_root();
	let scope_str = resolved.label();

	let plans = classify_all(scope, project_root);

	if json {
		let rows: Vec<_> = plans
			.iter()
			.map(|p| AgentSkillCoverageView::from_plan(p, scope_str))
			.collect();
		println!("{}", serde_json::to_string_pretty(&rows)?);
		return Ok(());
	}

	// "READS MASTER" / "WRITES MASTER" / "AUTO COVERED" are gone with
	// `LinkNeed::NativeReader`: against a store nothing reads they would print
	// three constants. SHARES WITH replaces them with the fact a user acts on —
	// granting here grants to those agents too, and revoking here revokes from
	// all of them.
	let mut builder = Builder::default();
	builder.push_record(["AGENT", "SUPPORTED", "REFERRER DIR", "SHARES WITH"]);
	for p in &plans {
		let (supported, dir) = match &p.need {
			LinkNeed::NeedsLink { referrer_dir } => {
				("yes".to_string(), referrer_dir.display().to_string())
			}
			LinkNeed::Unsupported => ("no".to_string(), "-".to_string()),
		};
		let shares = if p.shared_with.is_empty() {
			"-".to_string()
		} else {
			p.shared_with.join(", ")
		};
		builder.push_record([p.agent_id.to_string(), supported, dir, shares]);
	}
	let mut table = builder.build();
	table.with(Style::sharp());
	println!("{table}");
	Ok(())
}
