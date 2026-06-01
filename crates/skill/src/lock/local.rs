use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

const LOCAL_LOCK_FILE: &str = "skills-lock.json";
const CURRENT_VERSION: u32 = 1;

/// Process-wide guard so concurrent writers never interleave or observe a
/// partially written lock file. Combined with temp+rename, readers always see
/// either the old or the fully written new file.
static WRITE_LOCK: Mutex<()> = Mutex::new(());

/// Represents a single skill entry in the local (project) lock file.
///
/// Intentionally minimal and timestamp-free to minimize merge conflicts.
/// Two branches adding different skills produce non-overlapping JSON keys
/// that git can auto-merge cleanly.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LocalSkillLockEntry {
	/// Where the skill came from: npm package name, owner/repo, local path, etc.
	pub source: String,
	/// Branch or tag ref used for installation, when known.
	#[serde(rename = "ref", skip_serializing_if = "Option::is_none")]
	pub ref_name: Option<String>,
	/// The provider/source type (e.g., "github", "node_modules", "local")
	#[serde(rename = "sourceType")]
	pub source_type: String,
	/// npx-style skill path: POSIX `<repo-relative-dir>/SKILL.md`, used by
	/// npx `experimental_sync` to locate the skill within its source repo.
	#[serde(
		rename = "skillPath",
		skip_serializing_if = "Option::is_none",
		default
	)]
	pub skill_path: Option<String>,
	/// SHA-256 hash computed from all files in the skill folder.
	/// Unlike the global lock which uses GitHub tree SHA, the local lock
	/// computes the hash from actual file contents on disk.
	#[serde(rename = "computedHash")]
	pub computed_hash: String,
}

/// The structure of the local (project-scoped) skill lock file.
/// This file is meant to be checked into version control.
///
/// Skills are sorted alphabetically by name when written to produce
/// deterministic output and minimize merge conflicts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalSkillLockFile {
	/// Schema version for future migrations
	pub version: u32,
	/// Map of skill name to its lock entry (sorted alphabetically)
	pub skills: BTreeMap<String, LocalSkillLockEntry>,
}

impl Default for LocalSkillLockFile {
	fn default() -> Self {
		Self {
			version: CURRENT_VERSION,
			skills: BTreeMap::new(),
		}
	}
}

impl LocalSkillLockFile {
	/// Create a new empty lock file.
	pub fn new() -> Self {
		Self::default()
	}
}

/// Get the path to the local skill lock file for a project.
pub fn get_local_lock_path(cwd: Option<&Path>) -> PathBuf {
	let dir = cwd
		.map(|p| p.to_path_buf())
		.or_else(|| std::env::current_dir().ok())
		.unwrap_or_else(|| PathBuf::from("."));
	dir.join(LOCAL_LOCK_FILE)
}

/// Read the local skill lock file.
/// Returns an empty lock file structure if the file doesn't exist
/// or is corrupted (e.g., merge conflict markers).
pub fn read_local_lock(cwd: Option<&Path>) -> LocalSkillLockFile {
	read_local_lock_locked(cwd)
}

fn read_local_lock_locked(cwd: Option<&Path>) -> LocalSkillLockFile {
	let lock_path = get_local_lock_path(cwd);

	match std::fs::read_to_string(&lock_path) {
		Ok(content) => {
			// Try to parse, return empty on any error
			match serde_json::from_str::<LocalSkillLockFile>(&content) {
				Ok(lock) => {
					// Check version
					if lock.version < CURRENT_VERSION {
						LocalSkillLockFile::new()
					} else {
						lock
					}
				}
				Err(_) => {
					// Corrupted JSON (merge conflict markers, etc.)
					LocalSkillLockFile::new()
				}
			}
		}
		Err(_) => {
			// File doesn't exist
			LocalSkillLockFile::new()
		}
	}
}

/// Write the local skill lock file.
/// Skills are sorted alphabetically by name for deterministic output.
pub fn write_local_lock(
	lock: &LocalSkillLockFile,
	cwd: Option<&Path>,
) -> std::io::Result<()> {
	let _guard = WRITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
	write_local_lock_locked(lock, cwd)
}

fn write_local_lock_locked(
	lock: &LocalSkillLockFile,
	cwd: Option<&Path>,
) -> std::io::Result<()> {
	let lock_path = get_local_lock_path(cwd);

	// BTreeMap is already sorted by key. Preserve existing formatting:
	// 2-space pretty + trailing newline.
	let content = serde_json::to_string_pretty(lock)? + "\n";
	super::io::atomic_write_json(&lock_path, &content)
}

pub fn modify_local_lock<R>(
	cwd: Option<&Path>,
	f: impl FnOnce(&mut LocalSkillLockFile) -> R,
) -> std::io::Result<R> {
	let _guard = WRITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
	let mut lock = read_local_lock_locked(cwd);
	let result = f(&mut lock);
	write_local_lock_locked(&lock, cwd)?;
	Ok(result)
}

pub fn modify_local_lock_changed<R>(
	cwd: Option<&Path>,
	f: impl FnOnce(&mut LocalSkillLockFile) -> (R, bool),
) -> std::io::Result<R> {
	let _guard = WRITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
	let mut lock = read_local_lock_locked(cwd);
	let (result, changed) = f(&mut lock);
	if changed {
		write_local_lock_locked(&lock, cwd)?;
	}
	Ok(result)
}

/// Add or update a skill entry in the local lock file.
pub fn add_skill_to_local_lock(
	skill_name: &str,
	entry: LocalSkillLockEntry,
	cwd: Option<&Path>,
) -> std::io::Result<()> {
	modify_local_lock(cwd, |lock| {
		lock.skills.insert(skill_name.to_string(), entry);
	})
}

/// Remove a skill from the local lock file.
/// Returns true if the skill was removed, false if it didn't exist.
pub fn remove_skill_from_local_lock(
	skill_name: &str,
	cwd: Option<&Path>,
) -> std::io::Result<bool> {
	modify_local_lock_changed(cwd, |lock| {
		let removed = lock.skills.remove(skill_name).is_some();
		(removed, removed)
	})
}

/// Atomically prune the project lock down to the skills present on disk.
///
/// Mirror of the global `retain_locked_skills`: drops any entry whose
/// `sanitize_name(key)` is absent from `present_dir_names`, preserves survivors
/// (including `computedHash`/`skillPath`) byte-for-byte, keeps `version = 1` and
/// the trailing newline, and does NOT rewrite the file when nothing is pruned.
pub fn retain_local_locked_skills(
	present_dir_names: &std::collections::BTreeSet<String>,
	cwd: Option<&Path>,
) -> std::io::Result<Vec<String>> {
	modify_local_lock_changed(cwd, |lock| {
		let removed: Vec<String> = lock
			.skills
			.keys()
			.filter(|key| {
				!crate::sanitize::skill_present_on_disk(key, present_dir_names)
			})
			.cloned()
			.collect();
		for key in &removed {
			lock.skills.remove(key);
		}
		let changed = !removed.is_empty();
		(removed, changed)
	})
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::collections::BTreeSet;
	use std::fs;
	use tempfile::TempDir;

	fn present(names: &[&str]) -> BTreeSet<String> {
		names.iter().map(|s| s.to_string()).collect()
	}

	fn sample_local_entry() -> LocalSkillLockEntry {
		LocalSkillLockEntry {
			source: "org/repo".to_string(),
			ref_name: None,
			source_type: "github".to_string(),
			computed_hash: "abc123".to_string(),
			skill_path: None,
		}
	}

	#[test]
	fn local_entry_serializes_skill_path_camel_case() {
		let mut e = sample_local_entry();
		e.skill_path = Some("skills/pdf/SKILL.md".to_string());
		let json = serde_json::to_string(&e).unwrap();
		assert!(json.contains("\"skillPath\":\"skills/pdf/SKILL.md\""));
	}

	#[test]
	fn local_entry_key_order_matches_npx() {
		let entry = LocalSkillLockEntry {
			source: "org/repo".to_string(),
			ref_name: Some("main".to_string()),
			source_type: "github".to_string(),
			skill_path: Some("skills/pdf/SKILL.md".to_string()),
			computed_hash: "abc123".to_string(),
		};
		let json = serde_json::to_string(&entry).unwrap();
		let source = json.find("\"source\"").unwrap();
		let ref_name = json.find("\"ref\"").unwrap();
		let source_type = json.find("\"sourceType\"").unwrap();
		let skill_path = json.find("\"skillPath\"").unwrap();
		let computed_hash = json.find("\"computedHash\"").unwrap();
		assert!(source < ref_name);
		assert!(ref_name < source_type);
		assert!(source_type < skill_path);
		assert!(skill_path < computed_hash);
	}

	#[test]
	fn local_entry_omits_skill_path_when_none() {
		let mut e = sample_local_entry();
		e.skill_path = None;
		assert!(!serde_json::to_string(&e).unwrap().contains("skillPath"));
	}

	#[test]
	fn write_local_lock_has_trailing_newline_and_sorted_keys() {
		let _g = crate::lock::test_utils::TestLockGuard::new();
		let tmp = tempfile::tempdir().unwrap();
		let mut lock = super::LocalSkillLockFile::default();
		lock.skills.insert("z".into(), sample_local_entry());
		lock.skills.insert("a".into(), sample_local_entry());
		super::write_local_lock(&lock, Some(tmp.path())).unwrap();
		let raw = std::fs::read_to_string(tmp.path().join("skills-lock.json"))
			.unwrap();
		assert!(raw.ends_with("\n"));
		assert!(!raw.ends_with("\n\n"));
		let a = raw.find("\"a\"").unwrap();
		let z = raw.find("\"z\"").unwrap();
		assert!(a < z, "keys must be sorted (BTreeMap)");
	}

	#[test]
	fn write_local_lock_uses_unique_tmp_no_fixed_collision() {
		let tmp = tempfile::tempdir().unwrap();
		let legacy_tmp = tmp.path().join("skills-lock.json.tmp");
		std::fs::create_dir_all(&legacy_tmp).unwrap();
		let lock = super::LocalSkillLockFile::default();

		super::write_local_lock(&lock, Some(tmp.path())).unwrap();

		assert!(legacy_tmp.is_dir(), "legacy fixed tmp path was untouched");
		assert!(tmp.path().join("skills-lock.json").exists());
	}

	#[test]
	fn test_get_local_lock_path_with_cwd() {
		let result = get_local_lock_path(Some(Path::new("/some/project")));
		assert_eq!(result, PathBuf::from("/some/project/skills-lock.json"));
	}

	#[test]
	fn test_get_local_lock_path_without_cwd() {
		let result = get_local_lock_path(None);
		assert!(result.ends_with("skills-lock.json"));
	}

	#[test]
	fn test_read_local_lock_missing_file() {
		let dir = TempDir::new().unwrap();
		let lock = read_local_lock(Some(dir.path()));
		assert_eq!(lock.version, 1);
		assert!(lock.skills.is_empty());
	}

	#[test]
	fn test_read_local_lock_valid_file() {
		let dir = TempDir::new().unwrap();
		let content = r#"{
  "version": 1,
  "skills": {
    "my-skill": {
      "source": "vercel-labs/skills",
      "sourceType": "github",
      "computedHash": "abc123"
    }
  }
}"#;
		fs::write(dir.path().join("skills-lock.json"), content).unwrap();

		let lock = read_local_lock(Some(dir.path()));
		assert_eq!(lock.version, 1);
		assert!(lock.skills.contains_key("my-skill"));
		let entry = lock.skills.get("my-skill").unwrap();
		assert_eq!(entry.source, "vercel-labs/skills");
		assert_eq!(entry.source_type, "github");
		assert_eq!(entry.computed_hash, "abc123");
	}

	#[test]
	fn test_read_local_lock_corrupted_json_merge_conflict() {
		let dir = TempDir::new().unwrap();
		let conflicted = [
			r#"{"#,
			r#"  "version": 1,"#,
			r#"  "skills": {"#,
			"<<<<<<< HEAD",
			r#"    "skill-a": { "source": "org/repo-a", "sourceType": "github", "computedHash": "aaa" }"#,
			"=======",
			r#"    "skill-b": { "source": "org/repo-b", "sourceType": "github", "computedHash": "bbb" }"#,
			">>>>>>> feature-branch",
			r#"  }"#,
			r#"}"#,
		]
		.join("\n");
		fs::write(dir.path().join("skills-lock.json"), conflicted).unwrap();

		let lock = read_local_lock(Some(dir.path()));
		assert_eq!(lock.version, 1);
		assert!(lock.skills.is_empty());
	}

	#[test]
	fn test_read_local_lock_invalid_structure() {
		let dir = TempDir::new().unwrap();
		fs::write(dir.path().join("skills-lock.json"), r#"{"version": 1}"#)
			.unwrap();

		let lock = read_local_lock(Some(dir.path()));
		assert_eq!(lock.version, 1);
		assert!(lock.skills.is_empty());
	}

	#[test]
	fn test_write_local_lock_sorted_with_newline() {
		let dir = TempDir::new().unwrap();
		let mut lock = LocalSkillLockFile::new();
		lock.skills.insert(
			"zebra-skill".to_string(),
			LocalSkillLockEntry {
				source: "org/z".to_string(),
				ref_name: None,
				source_type: "github".to_string(),
				computed_hash: "zzz".to_string(),
				skill_path: None,
			},
		);
		lock.skills.insert(
			"alpha-skill".to_string(),
			LocalSkillLockEntry {
				source: "org/a".to_string(),
				ref_name: None,
				source_type: "github".to_string(),
				computed_hash: "aaa".to_string(),
				skill_path: None,
			},
		);
		lock.skills.insert(
			"middle-skill".to_string(),
			LocalSkillLockEntry {
				source: "org/m".to_string(),
				ref_name: None,
				source_type: "github".to_string(),
				computed_hash: "mmm".to_string(),
				skill_path: None,
			},
		);

		write_local_lock(&lock, Some(dir.path())).unwrap();

		let raw =
			fs::read_to_string(dir.path().join("skills-lock.json")).unwrap();
		assert!(raw.ends_with('\n'));

		let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap();
		let keys: Vec<_> =
			parsed["skills"].as_object().unwrap().keys().collect();
		assert_eq!(keys, vec!["alpha-skill", "middle-skill", "zebra-skill"]);
	}

	#[test]
	fn test_add_skill_to_local_lock_new() {
		let dir = TempDir::new().unwrap();
		add_skill_to_local_lock(
			"new-skill",
			LocalSkillLockEntry {
				source: "org/repo".to_string(),
				ref_name: None,
				source_type: "github".to_string(),
				computed_hash: "hash123".to_string(),
				skill_path: None,
			},
			Some(dir.path()),
		)
		.unwrap();

		let lock = read_local_lock(Some(dir.path()));
		assert!(lock.skills.contains_key("new-skill"));
		let entry = lock.skills.get("new-skill").unwrap();
		assert_eq!(entry.computed_hash, "hash123");
	}

	#[test]
	fn test_add_skill_to_local_lock_update_hash() {
		let dir = TempDir::new().unwrap();
		add_skill_to_local_lock(
			"my-skill",
			LocalSkillLockEntry {
				source: "org/repo".to_string(),
				ref_name: None,
				source_type: "github".to_string(),
				computed_hash: "old-hash".to_string(),
				skill_path: None,
			},
			Some(dir.path()),
		)
		.unwrap();

		add_skill_to_local_lock(
			"my-skill",
			LocalSkillLockEntry {
				source: "org/repo".to_string(),
				ref_name: None,
				source_type: "github".to_string(),
				computed_hash: "new-hash".to_string(),
				skill_path: None,
			},
			Some(dir.path()),
		)
		.unwrap();

		let lock = read_local_lock(Some(dir.path()));
		assert_eq!(
			lock.skills.get("my-skill").unwrap().computed_hash,
			"new-hash"
		);
	}

	#[test]
	fn test_add_skill_to_local_lock_preserves_others() {
		let dir = TempDir::new().unwrap();
		add_skill_to_local_lock(
			"skill-a",
			LocalSkillLockEntry {
				source: "org/a".to_string(),
				ref_name: None,
				source_type: "github".to_string(),
				computed_hash: "aaa".to_string(),
				skill_path: None,
			},
			Some(dir.path()),
		)
		.unwrap();

		add_skill_to_local_lock(
			"skill-b",
			LocalSkillLockEntry {
				source: "org/b".to_string(),
				ref_name: None,
				source_type: "github".to_string(),
				computed_hash: "bbb".to_string(),
				skill_path: None,
			},
			Some(dir.path()),
		)
		.unwrap();

		let lock = read_local_lock(Some(dir.path()));
		assert_eq!(lock.skills.len(), 2);
		assert_eq!(lock.skills.get("skill-a").unwrap().computed_hash, "aaa");
		assert_eq!(lock.skills.get("skill-b").unwrap().computed_hash, "bbb");
	}

	#[test]
	fn test_remove_skill_from_local_lock_existing() {
		let dir = TempDir::new().unwrap();
		add_skill_to_local_lock(
			"my-skill",
			LocalSkillLockEntry {
				source: "org/repo".to_string(),
				ref_name: None,
				source_type: "github".to_string(),
				computed_hash: "hash".to_string(),
				skill_path: None,
			},
			Some(dir.path()),
		)
		.unwrap();

		let removed =
			remove_skill_from_local_lock("my-skill", Some(dir.path())).unwrap();
		assert!(removed);

		let lock = read_local_lock(Some(dir.path()));
		assert!(!lock.skills.contains_key("my-skill"));
	}

	#[test]
	fn test_remove_skill_from_local_lock_nonexistent() {
		let dir = TempDir::new().unwrap();
		let removed =
			remove_skill_from_local_lock("no-such-skill", Some(dir.path()))
				.unwrap();
		assert!(!removed);
	}

	#[test]
	fn retain_local_drops_absent_keeps_present_and_preserves_fields() {
		let dir = TempDir::new().unwrap();
		let mut keep = sample_local_entry();
		keep.computed_hash = "abc123".to_string();
		keep.skill_path = Some("skills/keep/SKILL.md".to_string());
		add_skill_to_local_lock("keep", keep, Some(dir.path())).unwrap();
		add_skill_to_local_lock("gone", sample_local_entry(), Some(dir.path()))
			.unwrap();

		let removed =
			retain_local_locked_skills(&present(&["keep"]), Some(dir.path()))
				.unwrap();

		assert_eq!(removed, vec!["gone".to_string()]);
		let lock = read_local_lock(Some(dir.path()));
		assert!(!lock.skills.contains_key("gone"));
		let k = lock.skills.get("keep").unwrap();
		assert_eq!(k.computed_hash, "abc123");
		assert_eq!(k.skill_path, Some("skills/keep/SKILL.md".to_string()));
		assert_eq!(lock.version, 1, "version stays 1");
		let raw =
			fs::read_to_string(dir.path().join("skills-lock.json")).unwrap();
		assert!(raw.ends_with('\n') && !raw.ends_with("\n\n"));
	}

	#[test]
	fn retain_local_noop_when_all_present_keeps_exact_bytes() {
		let dir = TempDir::new().unwrap();
		add_skill_to_local_lock("a", sample_local_entry(), Some(dir.path()))
			.unwrap();
		let path = dir.path().join("skills-lock.json");
		let before = fs::read(&path).unwrap();
		let removed =
			retain_local_locked_skills(&present(&["a"]), Some(dir.path()))
				.unwrap();
		assert!(removed.is_empty());
		assert_eq!(fs::read(&path).unwrap(), before);
	}

	#[test]
	fn retain_local_matches_legacy_sanitized_key() {
		let dir = TempDir::new().unwrap();
		add_skill_to_local_lock(
			"İstanbul",
			sample_local_entry(),
			Some(dir.path()),
		)
		.unwrap();

		let removed = retain_local_locked_skills(
			&present(&["stanbul"]),
			Some(dir.path()),
		)
		.unwrap();

		assert!(removed.is_empty());
		assert!(read_local_lock(Some(dir.path()))
			.skills
			.contains_key("İstanbul"));
	}

	#[test]
	fn test_merge_conflict_friendliness() {
		let dir = TempDir::new().unwrap();

		// Simulate branch A adding skill-a
		add_skill_to_local_lock(
			"skill-a",
			LocalSkillLockEntry {
				source: "org/a".to_string(),
				ref_name: None,
				source_type: "github".to_string(),
				computed_hash: "aaa".to_string(),
				skill_path: None,
			},
			Some(dir.path()),
		)
		.unwrap();
		let branch_a =
			fs::read_to_string(dir.path().join("skills-lock.json")).unwrap();

		// Reset to empty
		fs::remove_file(dir.path().join("skills-lock.json")).unwrap();

		// Simulate branch B adding skill-b
		add_skill_to_local_lock(
			"skill-b",
			LocalSkillLockEntry {
				source: "org/b".to_string(),
				ref_name: None,
				source_type: "github".to_string(),
				computed_hash: "bbb".to_string(),
				skill_path: None,
			},
			Some(dir.path()),
		)
		.unwrap();
		let branch_b =
			fs::read_to_string(dir.path().join("skills-lock.json")).unwrap();

		// Both branches produce valid JSON with no timestamps to conflict on
		let parsed_a: serde_json::Value =
			serde_json::from_str(&branch_a).unwrap();
		let parsed_b: serde_json::Value =
			serde_json::from_str(&branch_b).unwrap();

		assert!(parsed_a["skills"]["skill-a"].is_object());
		assert!(parsed_a["skills"]["skill-a"]["computedHash"].is_string());
		assert!(parsed_b["skills"]["skill-b"].is_object());
		assert!(parsed_b["skills"]["skill-b"]["computedHash"].is_string());

		// No timestamps present
		assert!(parsed_a["skills"]["skill-a"]["installedAt"].is_null());
		assert!(parsed_a["skills"]["skill-a"]["updatedAt"].is_null());
	}
}
