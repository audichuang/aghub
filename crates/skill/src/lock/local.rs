use super::guard::{mutation_guard, MutationGuard, MutationScope};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub(super) const LOCAL_LOCK_FILE: &str = "skills-lock.json";
const CURRENT_VERSION: u32 = 1;

/// The interprocess mutation lock for THIS project's lock file — see
/// [`super::io`]'s `global_guard` for why the process mutex this replaced was
/// not enough. Resolves the project dir exactly like [`get_local_lock_path`], so
/// the guard and the file it guards can never disagree.
fn project_guard(
	op: &str,
	cwd: Option<&Path>,
) -> std::io::Result<MutationGuard> {
	mutation_guard(op, &[MutationScope::Project(local_dir(cwd))])
}

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
	/// aghub-only: repo-level commit OID (SHA-1 hex) of the branch/tag tip at
	/// install/update time, for the cheap ls-refs update preflight. Identical
	/// across members of the same source+ref group. Never read/written by npx.
	#[serde(
		rename = "refCommit",
		skip_serializing_if = "Option::is_none",
		default
	)]
	pub ref_commit: Option<String>,
	/// The full clone URL recorded at install time, so check/diff can rebuild a
	/// NON-github host (TFS/Azure DevOps/on-prem GitLab) that the host-stripped
	/// `source` (`owner/repo`) alone can only reconstruct as `github.com`.
	/// `None` for github shorthand and for legacy locks that predate the field —
	/// consumers fall back to `skill_update::sources::reconstruct_source_url`,
	/// which can only recover a host for `github`/`gitlab` types.
	///
	/// NOT aghub-only: current npx `skills` writes `sourceUrl` for `git`/`gitlab`
	/// installs too. What it does not do is backfill — its v1 reader accepts an
	/// older entry as-is and its writer re-emits the whole lock, so an absent
	/// field stays absent across npx round-trips.
	#[serde(
		rename = "sourceUrl",
		skip_serializing_if = "Option::is_none",
		default
	)]
	pub source_url: Option<String>,
}

impl LocalSkillLockEntry {
	/// Record aghub's freshly computed Source hash into the v1 project lock's
	/// `computed_hash`. The project lock is intentionally timestamp-free, so
	/// there is nothing else to update. Returns whether anything changed.
	pub fn apply_computed_hash(&mut self, hash: &str) -> bool {
		if self.computed_hash == hash {
			return false;
		}
		self.computed_hash = hash.to_string();
		true
	}
}

/// The structure of the local (project-scoped) skill lock file.
/// This file is meant to be checked into version control.
///
/// Skills are sorted alphabetically by name when written to produce
/// deterministic output and minimize merge conflicts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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

/// The project directory a `cwd: Option<&Path>` resolves to.
fn local_dir(cwd: Option<&Path>) -> PathBuf {
	cwd.map(|p| p.to_path_buf())
		.or_else(|| std::env::current_dir().ok())
		.unwrap_or_else(|| PathBuf::from("."))
}

/// Get the path to the local skill lock file for a project.
pub fn get_local_lock_path(cwd: Option<&Path>) -> PathBuf {
	local_dir(cwd).join(LOCAL_LOCK_FILE)
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
				Err(error) => {
					// Unparseable (merge conflict markers, truncation, …).
					// Fail open so read paths still answer, but say so —
					// silently reading as "no skills installed" is how a
					// corrupt lock gets mistaken for an empty one.
					log::warn!(
						"project skill lock {} is not valid JSON ({error}); \
						 reading it as empty",
						lock_path.display()
					);
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

/// [`super::io::read_lock_for_modify`] plus the same old-format wipe the read
/// path does.
fn read_local_lock_for_modify(
	cwd: Option<&Path>,
) -> std::io::Result<LocalSkillLockFile> {
	let lock: LocalSkillLockFile = super::io::read_lock_for_modify(
		&get_local_lock_path(cwd),
		LOCAL_LOCK_FILE,
	)?;
	if lock.version < CURRENT_VERSION {
		return Ok(LocalSkillLockFile::new());
	}
	Ok(lock)
}

/// Write the local skill lock file.
/// Skills are sorted alphabetically by name for deterministic output.
pub fn write_local_lock(
	lock: &LocalSkillLockFile,
	cwd: Option<&Path>,
) -> std::io::Result<()> {
	let _guard = project_guard("write project lock", cwd)?;
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
	let _guard = project_guard("modify project lock", cwd)?;
	let mut lock = read_local_lock_for_modify(cwd)?;
	let before = lock.clone();
	let result = f(&mut lock);
	if lock != before {
		write_local_lock_locked(&lock, cwd)?;
	}
	Ok(result)
}

pub fn modify_local_lock_changed<R>(
	cwd: Option<&Path>,
	f: impl FnOnce(&mut LocalSkillLockFile) -> (R, bool),
) -> std::io::Result<R> {
	let _guard = project_guard("modify project lock", cwd)?;
	let mut lock = read_local_lock_for_modify(cwd)?;
	let (result, changed) = f(&mut lock);
	if changed {
		write_local_lock_locked(&lock, cwd)?;
	}
	Ok(result)
}

/// Add or update a skill entry in the local lock file.
/// Add or update a skill entry in the project lock.
///
/// Returns the entry this write REPLACED, or `None` when it created a new one.
/// See [`crate::lock::global::add_skill_to_lock`] for why the receipt matters.
pub fn add_skill_to_local_lock(
	skill_name: &str,
	entry: LocalSkillLockEntry,
	cwd: Option<&Path>,
) -> std::io::Result<Option<LocalSkillLockEntry>> {
	modify_local_lock(cwd, |lock| {
		lock.skills.insert(skill_name.to_string(), entry)
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
			source_url: None,
			ref_commit: None,
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
	fn local_entry_source_url_round_trips_and_is_npx_invisible() {
		// None → the key is omitted entirely (npx never sees it, byte-identical
		// to a pre-field lock).
		let none = sample_local_entry();
		assert!(none.source_url.is_none());
		let json = serde_json::to_string(&none).unwrap();
		assert!(!json.contains("sourceUrl"), "None must not serialize a key");

		// Some(non-github url) → serialized as camelCase `sourceUrl` and reads
		// back identically.
		let mut e = sample_local_entry();
		e.source_url =
			Some("https://dev.azure.example/org/_git/repo".to_string());
		let json = serde_json::to_string(&e).unwrap();
		assert!(json.contains(
			"\"sourceUrl\":\"https://dev.azure.example/org/_git/repo\""
		));
		let back: LocalSkillLockEntry = serde_json::from_str(&json).unwrap();
		assert_eq!(back.source_url.as_deref(), e.source_url.as_deref());

		// A legacy/npx lock without the key deserializes to None (serde default).
		let legacy =
			r#"{"source":"o/r","sourceType":"github","computedHash":"h"}"#;
		let parsed: LocalSkillLockEntry = serde_json::from_str(legacy).unwrap();
		assert!(parsed.source_url.is_none());
	}

	#[test]
	fn local_entry_key_order_matches_npx() {
		let entry = LocalSkillLockEntry {
			source_url: None,
			source: "org/repo".to_string(),
			ref_name: Some("main".to_string()),
			source_type: "github".to_string(),
			skill_path: Some("skills/pdf/SKILL.md".to_string()),
			computed_hash: "abc123".to_string(),
			ref_commit: None,
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
	fn apply_computed_hash_sets_hash_and_reports_changed() {
		let mut e = sample_local_entry();
		e.computed_hash = "old".to_string();

		let changed = e.apply_computed_hash("new-source-sha");

		assert!(changed);
		assert_eq!(e.computed_hash, "new-source-sha");
	}

	#[test]
	fn apply_computed_hash_is_idempotent() {
		let mut e = sample_local_entry();
		e.computed_hash = "same".to_string();

		let changed = e.apply_computed_hash("same");

		assert!(!changed);
		assert_eq!(e.computed_hash, "same");
	}

	#[test]
	fn local_entry_serializes_ref_commit_as_camel_case() {
		let mut e = sample_local_entry();
		e.ref_commit = Some("deadbeef".to_string());
		let json = serde_json::to_string(&e).unwrap();
		assert!(json.contains("\"refCommit\":\"deadbeef\""));
	}

	#[test]
	fn local_entry_omits_ref_commit_when_none() {
		let mut e = sample_local_entry();
		e.ref_commit = None;
		assert!(!serde_json::to_string(&e).unwrap().contains("refCommit"));
	}

	#[test]
	fn local_entry_deserializes_without_ref_commit_to_none() {
		// npx-written entry has no refCommit; must default to None, not error.
		let json =
			r#"{"source":"o/r","sourceType":"github","computedHash":"abc"}"#;
		let e: super::LocalSkillLockEntry = serde_json::from_str(json).unwrap();
		assert_eq!(e.ref_commit, None);
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
	fn modify_local_lock_noop_does_not_rewrite() {
		let dir = TempDir::new().unwrap();
		let lock = super::LocalSkillLockFile::default();
		write_local_lock(&lock, Some(dir.path())).unwrap();
		let path = dir.path().join("skills-lock.json");
		let before = fs::read(&path).unwrap();

		modify_local_lock(Some(dir.path()), |_lock| ()).unwrap();

		assert_eq!(fs::read(&path).unwrap(), before);
	}

	// An unresolved merge conflict in a VCS-tracked `skills-lock.json` used to
	// read as an EMPTY lock, so the next install rewrote the file from that
	// empty view and silently dropped every other entry. The write side must
	// fail closed. Asserting only the error would pass even if the file had
	// already been clobbered, so this reads the bytes back.
	#[test]
	fn modify_local_lock_refuses_to_overwrite_an_unparseable_lock() {
		let dir = TempDir::new().unwrap();
		let path = dir.path().join("skills-lock.json");
		// What an unresolved merge of two branches actually leaves behind.
		write_local_lock(
			&LocalSkillLockFile {
				version: CURRENT_VERSION,
				skills: [("keep-me".to_string(), sample_local_entry())]
					.into_iter()
					.collect(),
			},
			Some(dir.path()),
		)
		.unwrap();
		let corrupt =
			format!("<<<<<<< HEAD\n{}", fs::read_to_string(&path).unwrap());
		fs::write(&path, &corrupt).unwrap();

		let error = modify_local_lock(Some(dir.path()), |lock| {
			lock.skills
				.insert("newcomer".to_string(), sample_local_entry());
		})
		.expect_err("an unparseable lock must not be overwritten");

		assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
		assert_eq!(
			fs::read_to_string(&path).unwrap(),
			corrupt,
			"the corrupt file must be left exactly as found"
		);
	}

	// `skills-lock.json` is a project-root marker, so `touch`ing one to mark a
	// root is a deliberate state. It holds nothing to lose, so writes proceed.
	#[test]
	fn modify_local_lock_treats_an_empty_lock_as_fresh() {
		let dir = TempDir::new().unwrap();
		let path = dir.path().join("skills-lock.json");
		fs::write(&path, "").unwrap();

		modify_local_lock(Some(dir.path()), |lock| {
			lock.skills
				.insert("newcomer".to_string(), sample_local_entry());
		})
		.expect("an empty lock file must not block writes");

		assert!(read_local_lock(Some(dir.path()))
			.skills
			.contains_key("newcomer"));
	}

	// The read side stays fail-open on purpose (a corrupt lock must not brick
	// `get`/`check`), so pin that too — otherwise "fix" the write side by
	// making reads throw and every read command breaks instead.
	#[test]
	fn read_local_lock_still_falls_open_on_an_unparseable_lock() {
		let dir = TempDir::new().unwrap();
		fs::write(dir.path().join("skills-lock.json"), "<<<<<<< HEAD\n")
			.unwrap();

		assert!(read_local_lock(Some(dir.path())).skills.is_empty());
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
				source_url: None,
				ref_commit: None,
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
				source_url: None,
				ref_commit: None,
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
				source_url: None,
				ref_commit: None,
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
				source_url: None,
				ref_commit: None,
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
				source_url: None,
				ref_commit: None,
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
				source_url: None,
				ref_commit: None,
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
				source_url: None,
				ref_commit: None,
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
				source_url: None,
				ref_commit: None,
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
				source_url: None,
				ref_commit: None,
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
				source_url: None,
				ref_commit: None,
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
				source_url: None,
				ref_commit: None,
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
