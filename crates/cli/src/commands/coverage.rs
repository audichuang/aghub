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
use aghub_core::skills::linker::{
	universal_canonical_dir, AgentSkillCoverageView,
};
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

	let master = universal_canonical_dir(project_root).ok_or_else(|| {
		anyhow::anyhow!(
			"could not resolve the universal master skills directory"
		)
	})?;

	let plans = classify_all(scope, project_root, &master);

	if json {
		let rows: Vec<_> = plans
			.iter()
			.map(|p| AgentSkillCoverageView::from_plan(p, scope_str))
			.collect();
		println!("{}", serde_json::to_string_pretty(&rows)?);
		return Ok(());
	}

	let mut builder = Builder::default();
	builder.push_record([
		"AGENT",
		"READS MASTER",
		"WRITES MASTER",
		"NEEDS LINK",
		"AUTO COVERED",
		"SUPPORTED",
	]);
	for p in &plans {
		let yn = |b: bool| if b { "yes" } else { "no" }.to_string();
		builder.push_record([
			p.agent_id.to_string(),
			yn(p.reads_master),
			yn(p.writes_master),
			yn(matches!(p.need, LinkNeed::NeedsLink { .. })),
			yn(matches!(p.need, LinkNeed::NativeReader)),
			yn(!matches!(p.need, LinkNeed::Unsupported)),
		]);
	}
	let mut table = builder.build();
	table.with(Style::sharp());
	println!("{table}");
	Ok(())
}
