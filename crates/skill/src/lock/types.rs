use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

const CURRENT_VERSION: u32 = 3;

/// Represents a single installed skill entry in the lock file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SkillLockEntry {
	/// Normalized source identifier (e.g., "owner/repo", "mintlify/bun.com")
	pub source: String,
	/// The provider/source type (e.g., "github", "mintlify", "huggingface", "local")
	#[serde(rename = "sourceType")]
	pub source_type: String,
	/// The original URL used to install the skill (for re-fetching updates)
	#[serde(rename = "sourceUrl")]
	pub source_url: String,
	/// Branch or tag ref used for installation, when known.
	#[serde(rename = "ref", skip_serializing_if = "Option::is_none")]
	pub ref_name: Option<String>,
	/// Subpath within the source repo, if applicable
	#[serde(rename = "skillPath", skip_serializing_if = "Option::is_none")]
	pub skill_path: Option<String>,
	/// GitHub tree SHA for the entire skill folder.
	/// This hash changes when ANY file in the skill folder changes.
	/// Fetched via GitHub Trees API by the telemetry server.
	#[serde(rename = "skillFolderHash")]
	pub skill_folder_hash: String,
	/// aghub source SHA-256 folder hash. npx leaves this absent; aghub stores
	/// the real hash here and keeps `skill_folder_hash` empty. Missing →
	/// recompute (never an error).
	#[serde(
		rename = "contentHash",
		skip_serializing_if = "Option::is_none",
		default
	)]
	pub content_hash: Option<String>,
	/// ISO timestamp when the skill was first installed
	#[serde(rename = "installedAt")]
	pub installed_at: String,
	/// ISO timestamp when the skill was last updated
	#[serde(rename = "updatedAt")]
	pub updated_at: String,
	/// Name of the plugin this skill belongs to (if any)
	#[serde(rename = "pluginName", skip_serializing_if = "Option::is_none")]
	pub plugin_name: Option<String>,
}

/// Tracks dismissed prompts so they're not shown again.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct DismissedPrompts {
	/// Dismissed the find-skills skill installation prompt
	#[serde(
		rename = "findSkillsPrompt",
		skip_serializing_if = "Option::is_none"
	)]
	pub find_skills_prompt: Option<bool>,
}

/// The structure of the skill lock file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SkillLockFile {
	/// Schema version for future migrations
	pub version: u32,
	/// Map of skill name to its lock entry
	pub skills: BTreeMap<String, SkillLockEntry>,
	/// Tracks dismissed prompts
	#[serde(skip_serializing_if = "Option::is_none")]
	pub dismissed: Option<DismissedPrompts>,
	/// Last selected agents for installation
	#[serde(
		rename = "lastSelectedAgents",
		skip_serializing_if = "Option::is_none"
	)]
	pub last_selected_agents: Option<Vec<String>>,
}

impl Default for SkillLockFile {
	fn default() -> Self {
		Self {
			version: CURRENT_VERSION,
			skills: BTreeMap::new(),
			dismissed: None,
			last_selected_agents: None,
		}
	}
}

impl SkillLockFile {
	/// Create a new empty lock file.
	pub fn new() -> Self {
		Self::default()
	}

	/// Get current schema version
	pub fn current_version() -> u32 {
		CURRENT_VERSION
	}
}

impl SkillLockEntry {
	/// Create a new skill lock entry with timestamps
	pub fn new(
		source: String,
		source_type: String,
		source_url: String,
		ref_name: Option<String>,
		skill_path: Option<String>,
		skill_folder_hash: String,
		plugin_name: Option<String>,
	) -> Self {
		let now = Utc::now().to_rfc3339();
		Self {
			source,
			source_type,
			source_url,
			ref_name,
			skill_path,
			skill_folder_hash,
			installed_at: now.clone(),
			updated_at: now,
			plugin_name,
			content_hash: None,
		}
	}

	/// Record aghub's freshly computed Source hash, in the v3 lock's native
	/// representation: store it in `content_hash`, empty `skill_folder_hash`
	/// (the two are never both populated), and bump `updated_at` to `now`.
	/// Returns whether anything changed.
	pub fn apply_content_hash(&mut self, hash: &str, now: &str) -> bool {
		let already_set = self.content_hash.as_deref() == Some(hash);
		let folder_clean = self.skill_folder_hash.is_empty();
		if already_set && folder_clean {
			return false;
		}
		self.content_hash = Some(hash.to_string());
		self.skill_folder_hash.clear();
		self.updated_at = now.to_string();
		true
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn sample_entry() -> SkillLockEntry {
		SkillLockEntry {
			source: "o/r".to_string(),
			source_type: "github".to_string(),
			source_url: "https://github.com/o/r".to_string(),
			ref_name: None,
			skill_path: None,
			skill_folder_hash: String::new(),
			installed_at: "t".to_string(),
			updated_at: "t".to_string(),
			plugin_name: None,
			content_hash: None,
		}
	}

	#[test]
	fn entry_serializes_content_hash_as_camel_case() {
		let mut e = sample_entry();
		e.content_hash = Some("abc123".to_string());
		let json = serde_json::to_string(&e).unwrap();
		assert!(json.contains("\"contentHash\":\"abc123\""));
	}

	#[test]
	fn entry_omits_content_hash_when_none() {
		let mut e = sample_entry();
		e.content_hash = None;
		let json = serde_json::to_string(&e).unwrap();
		assert!(!json.contains("contentHash"));
	}

	#[test]
	fn entry_deserializes_without_content_hash_to_none() {
		// npx-written entry has no contentHash; must not error.
		let json = r#"{"source":"o/r","sourceType":"github","sourceUrl":"https://github.com/o/r","skillFolderHash":"","installedAt":"t","updatedAt":"t"}"#;
		let e: super::SkillLockEntry = serde_json::from_str(json).unwrap();
		assert_eq!(e.content_hash, None);
		assert_eq!(e.skill_folder_hash, "");
	}

	#[test]
	fn apply_content_hash_sets_content_clears_folder_bumps_time() {
		// An npx-written entry carries a GitHub tree SHA in skill_folder_hash.
		// Applying aghub's source hash must store it in content_hash, empty the
		// folder hash (the v3 mutual-exclusion invariant), and bump updated_at.
		let mut e = sample_entry();
		e.skill_folder_hash = "gh-tree-sha".to_string();
		e.content_hash = None;
		e.installed_at = "first".to_string();
		e.updated_at = "old".to_string();

		let changed =
			e.apply_content_hash("source-sha", "2026-01-01T00:00:00Z");

		assert!(changed);
		assert_eq!(e.content_hash.as_deref(), Some("source-sha"));
		assert_eq!(e.skill_folder_hash, "");
		assert_eq!(e.updated_at, "2026-01-01T00:00:00Z");
		assert_eq!(e.installed_at, "first", "install time is preserved");
	}

	#[test]
	fn apply_content_hash_is_idempotent_when_already_in_desired_state() {
		// Already carrying the same source hash with an empty folder hash: a
		// re-apply must be a no-op and must NOT bump updated_at (auto-heal relies
		// on this to avoid rewriting an unchanged lock file).
		let mut e = sample_entry();
		e.skill_folder_hash = String::new();
		e.content_hash = Some("source-sha".to_string());
		e.updated_at = "old".to_string();

		let changed =
			e.apply_content_hash("source-sha", "2026-02-02T00:00:00Z");

		assert!(!changed);
		assert_eq!(e.updated_at, "old", "no bump when nothing changed");
	}

	#[test]
	fn apply_content_hash_changes_when_only_folder_hash_dirty() {
		// Same content hash, but a stale folder hash still present → must clear it
		// and report changed (the npx→aghub heal case).
		let mut e = sample_entry();
		e.skill_folder_hash = "stale".to_string();
		e.content_hash = Some("source-sha".to_string());

		let changed =
			e.apply_content_hash("source-sha", "2026-03-03T00:00:00Z");

		assert!(changed);
		assert_eq!(e.skill_folder_hash, "");
		assert_eq!(e.updated_at, "2026-03-03T00:00:00Z");
	}
}
