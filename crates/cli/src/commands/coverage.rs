//! `aghub-cli coverage` — read-only projection of `classify_all`.
//!
//! Classifies every registered agent against the canonical `.agents/skills`
//! master SKILLS-DIR for the requested scope and prints a per-agent coverage
//! table (or JSON). A pure passthrough over `aghub_core::skills::linker`: it
//! adds no write path. The `--json` shape comes from the SHARED
//! `AgentSkillCoverageView` (core), the exact type the HTTP API's
//! `AgentSkillCoverageDto` mirrors — so the two surfaces emit one wire shape
//! defined in one place and can never drift.

use aghub_core::models::ResourceScope;
use aghub_core::paths::find_project_root;
use aghub_core::skills::linker::classify::{classify_all, LinkNeed};
use aghub_core::skills::linker::{
	universal_canonical_dir, AgentSkillCoverageView,
};
use anyhow::{bail, Result};
use tabled::builder::Builder;
use tabled::settings::Style;

/// Coverage supports only `global` or `project` scope (the master is resolved
/// per-scope); `--all`/`Both` has no single master to classify against and is
/// rejected — mirroring the API route, which rejects `scope=all`.
fn resolve_scope(
	global: bool,
	project: bool,
	all: bool,
) -> Result<(ResourceScope, Option<std::path::PathBuf>, &'static str)> {
	if all {
		bail!(
			"coverage supports only 'global' or 'project' scope, not 'all'; \
			 pass -g/--global or -p/--project"
		);
	}
	if global && project {
		bail!("pass at most one of -g/--global or -p/--project");
	}
	if project {
		let cwd = std::env::current_dir()?;
		let Some(root) = find_project_root(&cwd) else {
			bail!(
				"project scope requires a project root, but none was found from \
				 the current directory"
			);
		};
		return Ok((ResourceScope::ProjectOnly, Some(root), "project"));
	}
	// Default (neither flag) is global, matching the single-agent default.
	Ok((ResourceScope::GlobalOnly, None, "global"))
}

/// Dispatch the `coverage` subcommand.
pub fn execute(
	global: bool,
	project: bool,
	all: bool,
	json: bool,
) -> Result<()> {
	let (scope, project_root, scope_str) = resolve_scope(global, project, all)?;

	let master =
		universal_canonical_dir(project_root.as_deref()).ok_or_else(|| {
			anyhow::anyhow!(
				"could not resolve the universal master skills directory"
			)
		})?;

	let plans = classify_all(scope, project_root.as_deref(), &master);

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
