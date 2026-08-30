//! `aghub-cli skill-usage` — read-only view of Claude skill usage counts.
//!
//! A pure passthrough over `aghub_core::skills::usage`: reads Claude Code's own
//! `skillUsage` counter (`~/.claude.json`), left-joins it against the installed
//! global Claude skills so never-dispatched skills show 0, and prints them
//! least-used first. Claude-only — no other agent keeps such a counter. Adds no
//! write path. `--json` serializes the core `SkillUsage` rows directly, the
//! same data the HTTP API's `SkillUsageResponse` wraps.

use aghub_core::skills::usage::list_claude_skill_usage;
use anyhow::Result;
use tabled::builder::Builder;
use tabled::settings::Style;

/// Format an epoch-milliseconds timestamp as a local `YYYY-MM-DD HH:MM`, or
/// "never" when the skill has never been dispatched.
fn format_last_used(last_used_at: Option<i64>) -> String {
	match last_used_at.and_then(chrono::DateTime::from_timestamp_millis) {
		Some(dt) => dt
			.with_timezone(&chrono::Local)
			.format("%Y-%m-%d %H:%M")
			.to_string(),
		None => "never".to_string(),
	}
}

/// Dispatch the `skill-usage` subcommand.
///
/// `skillUsage` is user-global, so this command is inherently global-scope;
/// `-p`/`--project` and `--all` are rejected rather than silently ignored — by
/// `CLAUDE_GLOBAL_ONLY_SCOPE` in `main`'s ONE policy table.
///
/// The resolved scope is taken and ignored ON PURPOSE: it is the only thing
/// making the dispatch site's call to the resolver load-bearing. Drop the
/// parameter and `main` can stop consulting the policy table for this command
/// without a compile error — which is exactly how `-p skill-usage` would go
/// from "rejected" to "silently accepted".
pub fn execute(_scope: &crate::Scope, json: bool) -> Result<()> {
	let rows = list_claude_skill_usage();

	if json {
		println!("{}", serde_json::to_string_pretty(&rows)?);
		return Ok(());
	}

	if rows.is_empty() {
		println!("No Claude skills found.");
		return Ok(());
	}

	let mut builder = Builder::default();
	builder.push_record(["SKILL", "USES", "LAST USED"]);
	for row in &rows {
		builder.push_record([
			row.name.clone(),
			row.usage_count.to_string(),
			format_last_used(row.last_used_at),
		]);
	}
	let mut table = builder.build();
	table.with(Style::sharp());
	println!("{table}");
	Ok(())
}
