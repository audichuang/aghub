use super::guard::{mutation_guard, MutationScope};
use super::types::SkillLockFile;
use std::io::Write;
use std::path::{Path, PathBuf};

/// The interprocess mutation lock for the global lock file, so concurrent
/// writers never interleave or observe a partially written file — and, unlike
/// the process mutex this replaced, so two aghub PROCESSES cannot both be told
/// they created the same entry. Reentrant, so a core flow holding it for a whole
/// transaction composes with these writers. Combined with temp+rename, readers
/// always see either the old or the fully written new file.
fn global_guard(op: &str) -> std::io::Result<super::guard::MutationGuard> {
	mutation_guard(op, &[MutationScope::Global])
}

/// Basename of the global lock, for errors that must not carry its path.
const GLOBAL_LOCK_FILE: &str = ".skill-lock.json";

/// Get the path to the global skill lock file.
/// Use $XDG_STATE_HOME/skills/.skill-lock.json if set.
/// otherwise fall back to ~/.agents/.skill-lock.json
pub fn get_skill_lock_path() -> PathBuf {
	if let Ok(xdg_state_home) = std::env::var("XDG_STATE_HOME") {
		PathBuf::from(xdg_state_home)
			.join("skills")
			.join(".skill-lock.json")
	} else {
		dirs::home_dir()
			.unwrap_or_else(|| PathBuf::from("."))
			.join(".agents")
			.join(".skill-lock.json")
	}
}

/// Read the skill lock file.
/// Returns an empty lock file structure if the file doesn't exist.
/// Wipes the lock file if it's an old format (version < CURRENT_VERSION).
pub fn read_skill_lock() -> SkillLockFile {
	read_skill_lock_locked()
}

fn read_skill_lock_locked() -> SkillLockFile {
	let lock_path = get_skill_lock_path();

	match std::fs::read_to_string(&lock_path) {
		Ok(content) => {
			match serde_json::from_str::<SkillLockFile>(&content) {
				Ok(lock) => {
					// If old version, wipe and start fresh (backwards incompatible change)
					// v3 adds skillFolderHash - we want fresh installs to populate it
					if lock.version < SkillLockFile::current_version() {
						SkillLockFile::new()
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
						"global skill lock {} is not valid JSON ({error}); \
						 reading it as empty",
						lock_path.display()
					);
					SkillLockFile::new()
				}
			}
		}
		Err(_) => SkillLockFile::new(),
	}
}

/// Read a lock for a MODIFY funnel, failing CLOSED on anything unprovable.
///
/// `read_*_lock` fails open to an empty lock so read paths keep answering. A
/// modify funnel must not: from that empty view it rewrites the file and drops
/// every entry the unreadable one still holds — an unresolved merge conflict in
/// a VCS-tracked `skills-lock.json` is the everyday way in. Only `NotFound` and
/// an empty file are genuinely "start fresh"; a permissions error or invalid
/// UTF-8 is NOT, because the entries are still there.
///
/// The parsed value is RETURNED rather than left for a second read. Two reads
/// are a window another writer slips through, and the mutation lock serializes
/// aghub against aghub only — `npx skills` and editors take none of it.
///
/// The error names the FILE, never its path: it reaches API clients verbatim,
/// and root AGENTS.md forbids internal lock paths in API errors. The path goes
/// to the log instead.
pub(crate) fn read_lock_for_modify<T>(
	path: &Path,
	file_label: &str,
) -> std::io::Result<T>
where
	T: serde::de::DeserializeOwned + Default,
{
	let content = match std::fs::read_to_string(path) {
		Ok(content) => content,
		Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
			return Ok(T::default())
		}
		Err(error) => {
			// Neutral wording: this predicate now also backs the read-only
			// `*_lock_readable` probes, where "rewrite" would be a lie.
			log::warn!("skill lock {} is unreadable: {error}", path.display());
			return Err(std::io::Error::new(
				error.kind(),
				format!(
					"the skill lock file {file_label} exists but could not be \
					 read ({}); refusing to overwrite it",
					error.kind()
				),
			));
		}
	};
	// An empty file holds no entries to lose. It is also a state users reach on
	// purpose: `skills-lock.json` is one of the project-root markers, so
	// `touch`ing it to mark a root must not dead-end every later write.
	if content.trim().is_empty() {
		return Ok(T::default());
	}
	serde_json::from_str::<T>(&content).map_err(|error| {
		log::warn!("skill lock {} is not valid JSON: {error}", path.display());
		std::io::Error::new(
			std::io::ErrorKind::InvalidData,
			format!(
				"the skill lock file {file_label} is not valid JSON; refusing \
				 to overwrite it. Resolve it (unresolved merge conflict?) and \
				 retry."
			),
		)
	})
}

/// Read the GLOBAL skill lock, failing CLOSED. `Ok` when it parses, is absent,
/// or is empty; `Err` naming the problem otherwise. Takes no mutation lock.
///
/// Returns the PARSED LOCK, not a verdict. A predicate-only probe left the
/// caller to read the file a second time through the fail-open reader, and
/// between those two reads a non-aghub writer (an editor, `npx skills`) can
/// truncate it — the second read then falls open to an empty lock and the
/// command answers `[]` on exit 0, which is exactly the failure the probe was
/// added to prevent. One read, no window.
///
/// The read paths above fail OPEN so a corrupt lock does not break every query.
/// A caller that presents the lock's CONTENTS as its answer needs the
/// difference, or it reports "nothing installed" for "I could not read the
/// file": `check skills`, `source list` and `doctor` each did exactly that, on
/// exit 0 with an empty stderr, and `doctor` went on to recommend deleting the
/// skills it had just failed to see.
pub fn read_global_lock_checked() -> std::io::Result<SkillLockFile> {
	// `_versioned`, NOT the bare `read_lock_for_modify`: the bare one skips the
	// old-format wipe that `read_skill_lock` applies. While this only answered
	// yes/no that difference was invisible; now that the parsed value is handed
	// to the caller, using the bare reader would resurrect v2 entries the
	// fail-open reader treats as an empty lock — a silent behaviour change from
	// a function that exists to REMOVE a silent behaviour change.
	read_lock_for_modify_versioned()
		.map_err(|error| unreadable_for_reading(GLOBAL_LOCK_FILE, &error))
}

/// Restate a fail-closed read error for a READ-ONLY caller.
///
/// `read_lock_for_modify`'s own wording ends in "refusing to overwrite it",
/// which is right for a writer and actively misleading for `check` / `doctor` /
/// `source list` — an agent reading it would think the command had tried to
/// write. Same predicate, caller-appropriate sentence.
pub(crate) fn unreadable_for_reading(
	file_label: &str,
	error: &std::io::Error,
) -> std::io::Error {
	std::io::Error::new(
		error.kind(),
		format!(
			"the skill lock file {file_label} exists but could not be read \
			 ({}); its contents cannot be reported. Resolve it (unresolved \
			 merge conflict?) and retry.",
			error.kind()
		),
	)
}

/// [`read_lock_for_modify`] plus the same old-format wipe the read path does.
fn read_lock_for_modify_versioned() -> std::io::Result<SkillLockFile> {
	let lock: SkillLockFile =
		read_lock_for_modify(&get_skill_lock_path(), GLOBAL_LOCK_FILE)?;
	// Old format: wipe and start fresh, exactly as `read_skill_lock_locked`
	// does — v3 added skillFolderHash and fresh installs must populate it.
	if lock.version < SkillLockFile::current_version() {
		return Ok(SkillLockFile::new());
	}
	Ok(lock)
}

/// Preflight for a caller that writes files BEFORE it writes the lock.
///
/// `install_fetched` materializes the Master and Referrers first, so a lock
/// funnel that refuses mid-transaction would leave an untracked partial
/// install behind. Prove the locks are writable while there is still nothing
/// to roll back — the project-AGENTS rule is preflight before any write.
pub fn ensure_locks_writable(
	global: bool,
	project_root: Option<&Path>,
) -> std::io::Result<()> {
	if global {
		read_lock_for_modify::<SkillLockFile>(
			&get_skill_lock_path(),
			GLOBAL_LOCK_FILE,
		)?;
	}
	if let Some(root) = project_root {
		read_lock_for_modify::<super::local::LocalSkillLockFile>(
			&super::local::get_local_lock_path(Some(root)),
			super::local::LOCAL_LOCK_FILE,
		)?;
	}
	Ok(())
}

/// Write the skill lock file.
/// Creates the directory if it doesn't exist.
pub fn write_skill_lock(lock: &SkillLockFile) -> std::io::Result<()> {
	let _guard = global_guard("write global lock")?;
	write_skill_lock_locked(lock)
}

fn write_skill_lock_locked(lock: &SkillLockFile) -> std::io::Result<()> {
	let lock_path = get_skill_lock_path();

	// Preserve existing aghub formatting: 2-space pretty + trailing newline.
	let content = serde_json::to_string_pretty(lock)? + "\n";
	atomic_write_json(&lock_path, &content)
}

/// Atomic JSON write: unique temp file in the destination directory, fsync,
/// then `persist` (which REPLACES an existing destination on every platform).
/// Public so other surfaces that must not hand-roll this — the CLI's
/// `check --write-result` sidecar — reuse the one implementation instead of a
/// fixed `.tmp` name and a bare rename.
pub fn atomic_write_json(path: &Path, content: &str) -> std::io::Result<()> {
	if let Some(parent) = path.parent() {
		std::fs::create_dir_all(parent)?;
		let mut tmp = tempfile::Builder::new()
			.prefix(".lock.")
			.tempfile_in(parent)?;
		tmp.write_all(content.as_bytes())?;
		apply_json_file_mode(path, tmp.as_file())?;
		tmp.as_file().sync_all()?;
		tmp.persist(path).map_err(|e| e.error)?;
	} else {
		let mut tmp = tempfile::Builder::new().prefix(".lock.").tempfile()?;
		tmp.write_all(content.as_bytes())?;
		apply_json_file_mode(path, tmp.as_file())?;
		tmp.as_file().sync_all()?;
		tmp.persist(path).map_err(|e| e.error)?;
	}
	Ok(())
}

#[cfg(unix)]
fn apply_json_file_mode(
	path: &Path,
	file: &std::fs::File,
) -> std::io::Result<()> {
	use std::os::unix::fs::PermissionsExt;

	let mode = std::fs::metadata(path)
		.map(|meta| meta.permissions().mode() & 0o777)
		.unwrap_or(0o644);
	file.set_permissions(std::fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn apply_json_file_mode(
	_path: &Path,
	_file: &std::fs::File,
) -> std::io::Result<()> {
	Ok(())
}

pub fn modify_skill_lock<R>(
	f: impl FnOnce(&mut SkillLockFile) -> R,
) -> std::io::Result<R> {
	let _guard = global_guard("modify global lock")?;
	let mut lock = read_lock_for_modify_versioned()?;
	let before = lock.clone();
	let result = f(&mut lock);
	if lock != before {
		write_skill_lock_locked(&lock)?;
	}
	Ok(result)
}

pub fn modify_skill_lock_changed<R>(
	f: impl FnOnce(&mut SkillLockFile) -> (R, bool),
) -> std::io::Result<R> {
	let _guard = global_guard("modify global lock")?;
	let mut lock = read_lock_for_modify_versioned()?;
	let (result, changed) = f(&mut lock);
	if changed {
		write_skill_lock_locked(&lock)?;
	}
	Ok(result)
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::lock::test_utils::TestLockGuard;

	fn sample_entry() -> super::super::types::SkillLockEntry {
		super::super::types::SkillLockEntry {
			source: "o/r".to_string(),
			source_type: "github".to_string(),
			source_url: "https://github.com/o/r".to_string(),
			ref_name: None,
			skill_path: None,
			skill_folder_hash: String::new(),
			content_hash: None,
			ref_commit: None,
			installed_at: "t".to_string(),
			updated_at: "t".to_string(),
			plugin_name: None,
		}
	}

	#[test]
	fn checked_read_applies_the_old_format_wipe() {
		// THE trap in turning a predicate probe into a reader. The probe was
		// built on the BARE `read_lock_for_modify`, which does NOT apply the
		// old-format wipe that `read_skill_lock` does. While it only answered
		// yes/no that difference was invisible. Handing the parsed value to the
		// caller with the bare reader would resurrect pre-v3 entries that every
		// other read path treats as an empty lock — a silent behaviour change
		// introduced by a function whose entire purpose is removing one.
		let _g = crate::lock::test_utils::TestLockGuard::new();
		let path = get_skill_lock_path();
		std::fs::create_dir_all(path.parent().unwrap()).unwrap();
		// A structurally VALID lock, in an old format: build it with the real
		// entry type, then downgrade only the version field, so the test cannot
		// pass for the wrong reason (a hand-written fixture that simply fails
		// to parse would also come back empty).
		let mut old = super::super::types::SkillLockFile::default();
		old.skills.insert("stale".into(), sample_entry());
		let mut raw: serde_json::Value =
			serde_json::to_value(&old).expect("lock serializes");
		raw["version"] = serde_json::json!(
			super::super::types::SkillLockFile::current_version() - 1
		);
		std::fs::write(&path, serde_json::to_string(&raw).unwrap()).unwrap();

		let checked = read_global_lock_checked()
			.expect("an old-format lock parses; it is not unreadable");
		assert!(
			checked.skills.is_empty(),
			"a pre-v3 lock must read as empty, exactly as `read_skill_lock` \
			 reports it — otherwise the checked reader resurrects entries no \
			 other path can see: {:?}",
			checked.skills.keys().collect::<Vec<_>>()
		);
		assert_eq!(
			checked.skills.len(),
			read_skill_lock().skills.len(),
			"the checked reader and the fail-open reader must agree on what \
			 this file contains"
		);
	}

	#[test]
	fn write_skill_lock_is_atomic_no_partial() {
		let _g = crate::lock::test_utils::TestLockGuard::new();
		let mut lock = super::super::types::SkillLockFile::default();
		lock.skills.insert("a".into(), sample_entry());
		super::write_skill_lock(&lock).unwrap();
		// file is valid JSON immediately after write (no truncated state)
		let path = super::get_skill_lock_path();
		let raw = std::fs::read_to_string(&path).unwrap();
		let _: super::super::types::SkillLockFile =
			serde_json::from_str(&raw).unwrap();
	}

	#[test]
	fn write_skill_lock_uses_unique_tmp_no_fixed_collision() {
		let _g = crate::lock::test_utils::TestLockGuard::new();
		let lock_path = get_skill_lock_path();
		std::fs::create_dir_all(lock_path.parent().unwrap()).unwrap();
		let legacy_tmp = lock_path.with_extension("json.tmp");
		std::fs::create_dir_all(&legacy_tmp).unwrap();

		let lock = super::super::types::SkillLockFile::default();
		write_skill_lock(&lock).unwrap();

		assert!(legacy_tmp.is_dir(), "legacy fixed tmp path was untouched");
		assert!(lock_path.exists());
	}

	#[test]
	fn modify_skill_lock_changed_writes_only_when_changed() {
		let _g = crate::lock::test_utils::TestLockGuard::new();
		let lock = super::super::types::SkillLockFile::default();
		write_skill_lock(&lock).unwrap();
		let path = get_skill_lock_path();
		let before = std::fs::read(&path).unwrap();

		modify_skill_lock_changed(|_lock| ((), false)).unwrap();

		let after = std::fs::read(&path).unwrap();
		assert_eq!(before, after);
	}

	#[test]
	fn modify_skill_lock_noop_does_not_rewrite() {
		let _g = crate::lock::test_utils::TestLockGuard::new();
		let lock = super::super::types::SkillLockFile::default();
		write_skill_lock(&lock).unwrap();
		let path = get_skill_lock_path();
		let before = std::fs::read(&path).unwrap();

		modify_skill_lock(|_lock| ()).unwrap();

		let after = std::fs::read(&path).unwrap();
		assert_eq!(before, after);
	}

	#[cfg(unix)]
	#[test]
	fn atomic_write_json_preserves_existing_mode() {
		use std::os::unix::fs::PermissionsExt;

		let _g = crate::lock::test_utils::TestLockGuard::new();
		let path = get_skill_lock_path();
		std::fs::create_dir_all(path.parent().unwrap()).unwrap();
		std::fs::write(&path, b"{}\n").unwrap();
		std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640))
			.unwrap();

		atomic_write_json(&path, "{\"version\":3,\"skills\":{}}\n").unwrap();

		let mode =
			std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
		assert_eq!(mode, 0o640);
	}

	#[test]
	fn test_get_skill_lock_path_with_xdg() {
		let _guard = TestLockGuard::new();
		let path = get_skill_lock_path();
		assert!(path.ends_with(".skill-lock.json"));
	}

	#[test]
	fn test_get_skill_lock_path_without_xdg() {
		let _guard = TestLockGuard::new();
		let old_xdg = std::env::var("XDG_STATE_HOME").ok();
		std::env::remove_var("XDG_STATE_HOME");

		let path = get_skill_lock_path();
		assert!(path.ends_with(".skill-lock.json"));
		assert!(path.to_string_lossy().contains(".agents"));

		if let Some(old) = old_xdg {
			std::env::set_var("XDG_STATE_HOME", old);
		}
	}

	#[test]
	fn test_read_skill_lock_missing_file() {
		let _guard = TestLockGuard::new();
		let lock = read_skill_lock();
		assert_eq!(lock.version, 3);
		assert!(lock.skills.is_empty());
	}

	#[test]
	fn test_read_skill_lock_old_version_wipes() {
		let _guard = TestLockGuard::new();
		let old_lock = r#"{
  "version": 2,
  "skills": {
    "old-skill": {
      "source": "org/repo",
      "sourceType": "github",
      "sourceUrl": "https://github.com/org/repo",
      "skillFolderHash": "old",
      "installedAt": "2024-01-01T00:00:00Z",
      "updatedAt": "2024-01-01T00:00:00Z"
    }
  }
}"#;

		let lock_path = get_skill_lock_path();
		std::fs::create_dir_all(lock_path.parent().unwrap()).unwrap();
		std::fs::write(&lock_path, old_lock).unwrap();

		let lock = read_skill_lock();
		assert_eq!(lock.version, 3);
		assert!(lock.skills.is_empty()); // Old version should be wiped
	}

	#[test]
	fn test_write_skill_lock_creates_directory() {
		let _guard = TestLockGuard::new();
		let lock = SkillLockFile::new();
		write_skill_lock(&lock).unwrap();

		let lock_path = get_skill_lock_path();
		assert!(lock_path.exists());
	}
}
