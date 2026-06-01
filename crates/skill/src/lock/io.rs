use super::types::SkillLockFile;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Process-wide guard so concurrent writers never interleave or observe a
/// partially written lock file. Combined with temp+rename, readers always see
/// either the old or the fully written new file.
static WRITE_LOCK: Mutex<()> = Mutex::new(());

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
				Err(_) => {
					// File doesn't exist or is invalid - return empty
					SkillLockFile::new()
				}
			}
		}
		Err(_) => SkillLockFile::new(),
	}
}

/// Write the skill lock file.
/// Creates the directory if it doesn't exist.
pub fn write_skill_lock(lock: &SkillLockFile) -> std::io::Result<()> {
	let _guard = WRITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
	write_skill_lock_locked(lock)
}

fn write_skill_lock_locked(lock: &SkillLockFile) -> std::io::Result<()> {
	let lock_path = get_skill_lock_path();

	// Preserve existing aghub formatting: 2-space pretty + trailing newline.
	let content = serde_json::to_string_pretty(lock)? + "\n";
	atomic_write_json(&lock_path, &content)
}

pub(crate) fn atomic_write_json(
	path: &Path,
	content: &str,
) -> std::io::Result<()> {
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
	let _guard = WRITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
	let mut lock = read_skill_lock_locked();
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
	let _guard = WRITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
	let mut lock = read_skill_lock_locked();
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
			installed_at: "t".to_string(),
			updated_at: "t".to_string(),
			plugin_name: None,
		}
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
