//! Skill usage counts, read from Claude Code's `skillUsage` map.
//!
//! Claude Code records `{ <name>: { usageCount, lastUsedAt } }` in
//! `~/.claude.json`, incrementing on every real dispatch of a skill. We read
//! that map and left-join it against the installed skills so a skill that has
//! never been dispatched (and is therefore absent from the map) surfaces as
//! zero — those are the prune candidates.
//!
//! Claude-only: no other agent keeps a comparable counter, so there is no
//! cross-agent generalization here (see
//! `docs/specs/2026-07-09-skill-usage-counts.md`).

use crate::adapters::create_adapter;
use crate::manager::ConfigManager;
use crate::models::{AgentType, ResourceScope, Skill};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// A raw `skillUsage` entry as stored by Claude Code.
#[derive(Debug, Clone, Deserialize)]
pub struct RawUsage {
	#[serde(rename = "usageCount", default)]
	pub usage_count: u64,
	/// Epoch milliseconds of the last dispatch. Absent on some entries.
	#[serde(rename = "lastUsedAt")]
	pub last_used_at: Option<i64>,
}

/// One installed skill joined with its usage count.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SkillUsage {
	pub name: String,
	pub usage_count: u64,
	/// Epoch milliseconds of the last dispatch, or `None` if never used.
	pub last_used_at: Option<i64>,
}

/// `~/.claude.json` — the file that holds `skillUsage`. `None` if the home
/// directory cannot be resolved. This mirrors the Claude descriptor's own
/// `~/.claude.json` path (its private `mcp_global_path`); the counter is a
/// Claude-home concern with no generic accessor.
pub fn default_claude_json_path() -> Option<PathBuf> {
	dirs::home_dir().map(|home| home.join(".claude.json"))
}

/// Read the `skillUsage` map from a `~/.claude.json`-shaped file.
///
/// Tolerant by design: a missing file, missing `skillUsage` key, or malformed
/// entries yield an empty map rather than an error — the feature is read-only
/// and best-effort.
pub fn read_skill_usage(claude_json: &Path) -> HashMap<String, RawUsage> {
	let Ok(bytes) = std::fs::read(claude_json) else {
		return HashMap::new();
	};
	let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
		return HashMap::new();
	};
	value
		.get("skillUsage")
		.and_then(|v| {
			serde_json::from_value::<HashMap<String, RawUsage>>(v.clone()).ok()
		})
		.unwrap_or_default()
}

/// Left-join installed skills against a usage map.
///
/// Every installed skill appears exactly once; skills absent from the map
/// default to zero uses / never. Sorted least-used first (ties broken by name
/// ascending) so prune candidates rise to the top. This is the pure core.
pub fn join_skill_usage(
	installed: &[Skill],
	usage: &HashMap<String, RawUsage>,
) -> Vec<SkillUsage> {
	let mut rows: Vec<SkillUsage> = installed
		.iter()
		.map(|skill| match usage.get(&skill.name) {
			Some(u) => SkillUsage {
				name: skill.name.clone(),
				usage_count: u.usage_count,
				last_used_at: u.last_used_at,
			},
			None => SkillUsage {
				name: skill.name.clone(),
				usage_count: 0,
				last_used_at: None,
			},
		})
		.collect();
	rows.sort_by(|a, b| {
		a.usage_count
			.cmp(&b.usage_count)
			.then_with(|| a.name.cmp(&b.name))
	});
	rows
}

/// List usage counts for the installed **global** Claude skills.
///
/// Discovers skills through the normal Claude adapter (so test path overrides
/// apply), reads `skillUsage` from `~/.claude.json`, and joins them. Returns an
/// empty list if the config cannot be loaded. `skillUsage` is user-global, not
/// per-project, so this intentionally covers only global-scope skills.
///
/// Note: plugin-managed skills (whose usage Claude keys as `plugin:name`) are
/// joined by bare name and would read as 0 — but they live in the plugin cache,
/// not `~/.claude/skills`, so this does not manifest in practice. The API list
/// route's `ClaudePluginManager` filter is API-only and deliberately not wired
/// through core for this edge case (see the spec's Risks).
pub fn list_claude_skill_usage() -> Vec<SkillUsage> {
	let mut manager = ConfigManager::with_scope(
		create_adapter(AgentType::Claude),
		true,
		None,
		ResourceScope::GlobalOnly,
	);
	let installed = manager
		.load()
		.map(|config| config.skills.clone())
		.unwrap_or_default();
	let usage = default_claude_json_path()
		.map(|path| read_skill_usage(&path))
		.unwrap_or_default();
	join_skill_usage(&installed, &usage)
}

#[cfg(test)]
mod tests {
	use super::*;

	fn raw(count: u64, last: Option<i64>) -> RawUsage {
		RawUsage {
			usage_count: count,
			last_used_at: last,
		}
	}

	#[test]
	fn join_defaults_absent_skill_to_zero_and_sorts_ascending() {
		let installed = vec![
			Skill::new("heavy"),
			Skill::new("never-used"),
			Skill::new("light"),
		];
		let mut usage = HashMap::new();
		usage.insert("heavy".to_string(), raw(42, Some(1000)));
		usage.insert("light".to_string(), raw(3, Some(500)));
		// "never-used" deliberately absent from the map.

		let rows = join_skill_usage(&installed, &usage);

		// Least-used first: never-used (0) < light (3) < heavy (42).
		assert_eq!(
			rows.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
			["never-used", "light", "heavy"]
		);
		// Absent skill defaults to zero / never.
		assert_eq!(rows[0].usage_count, 0);
		assert_eq!(rows[0].last_used_at, None);
		assert_eq!(rows[2].usage_count, 42);
		assert_eq!(rows[2].last_used_at, Some(1000));
	}

	#[test]
	fn ties_broken_by_name() {
		let installed = vec![Skill::new("b"), Skill::new("a")];
		let rows = join_skill_usage(&installed, &HashMap::new());
		assert_eq!(
			rows.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
			["a", "b"]
		);
	}

	#[test]
	fn read_missing_file_is_empty() {
		let path = Path::new("/nonexistent/definitely/.claude.json");
		assert!(read_skill_usage(path).is_empty());
	}

	#[test]
	fn read_parses_skill_usage_and_tolerates_missing_last_used() {
		let dir = tempfile::tempdir().unwrap();
		let path = dir.path().join(".claude.json");
		std::fs::write(
			&path,
			r#"{
				"numStartups": 5,
				"skillUsage": {
					"init": { "usageCount": 6, "lastUsedAt": 1779929056499 },
					"orphan": { "usageCount": 2 }
				}
			}"#,
		)
		.unwrap();

		let usage = read_skill_usage(&path);
		assert_eq!(usage.len(), 2);
		assert_eq!(usage["init"].usage_count, 6);
		assert_eq!(usage["init"].last_used_at, Some(1779929056499));
		assert_eq!(usage["orphan"].usage_count, 2);
		assert_eq!(usage["orphan"].last_used_at, None);
	}

	#[test]
	fn read_without_skill_usage_key_is_empty() {
		let dir = tempfile::tempdir().unwrap();
		let path = dir.path().join(".claude.json");
		std::fs::write(&path, r#"{"numStartups": 5}"#).unwrap();
		assert!(read_skill_usage(&path).is_empty());
	}
}
