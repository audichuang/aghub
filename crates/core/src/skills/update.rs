//! PURE update comparison. No network, no keyring — callers pass a resolved token
//! and a fetched source folder. (Fetch + creds live in crates/api.)

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UncheckableReason {
	Auth,
	Network,
	Local,
	Ssh,
	UnsupportedScheme,
	NoPath,
	Timeout,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillUpdateStatus {
	UpToDate,
	UpdateAvailable { current: String, available: String },
	Renamed { new_name: String },
	Uncheckable { reason: UncheckableReason },
}

/// Shared rename-detection contract used by every update/apply/sync path
/// (CLI `apply-update`, the API apply/sync routes, and the update-check
/// pipeline). Centralising the predicate, the user-facing message, and the
/// error code keeps all surfaces in lock-step: a skill renamed upstream is
/// refused identically whether detected at check time, apply time, or sync
/// time, and consumers branch on a single stable code.
///
/// Returns `Some(parsed_name)` when the upstream-parsed name differs from the
/// expected (locked) name — i.e. the skill was renamed in the source — or
/// `None` when the names match. Intentionally a cheap, exact comparison: the
/// `SKILL.md` frontmatter parser is the authority on canonical form, so this
/// does not trim or case-fold.
pub fn detect_rename(parsed_name: &str, expected: &str) -> Option<String> {
	if parsed_name == expected {
		None
	} else {
		Some(parsed_name.to_string())
	}
}

/// Canonical, user-facing rename message: the lock owner should delete the old
/// skill and install under the new name.
pub fn skill_renamed_message(old_name: &str, new_name: &str) -> String {
	format!(
		"Skill '{old_name}' was renamed to '{new_name}' in the source. \
		 Delete the old skill and install '{new_name}' instead."
	)
}

/// API error code returned for both apply-time and sync-time renames. Distinct
/// from the legacy `SKILL_NAME_MISMATCH` so consumers can branch on one code.
pub const SKILL_RENAMED_CODE: &str = "SKILL_RENAMED_IN_SOURCE";

/// Compare two known content hashes.
///
/// Legacy/missing hashes are intentionally not handled here: callers must first
/// compute a local baseline hash when the lock has no trustworthy value.
pub fn compare_known_hashes(stored: &str, fresh: &str) -> SkillUpdateStatus {
	if stored == fresh {
		SkillUpdateStatus::UpToDate
	} else {
		SkillUpdateStatus::UpdateAvailable {
			current: stored.to_string(),
			available: fresh.to_string(),
		}
	}
}

/// Pre-fetch source classification. Returns `Some(reason)` for a source that
/// cannot be update-checked via an HTTPS fetch — so the caller skips fetching
/// and reports `Uncheckable{reason}` directly — or `None` for HTTPS / GitHub
/// `owner/repo` shorthand that should proceed to a normal fetch.
///
/// `source_type` is the authoritative signal for local installs (set to
/// `"local"` at install time, and what the CLI `check` already keys on);
/// `source` is inspected for its URL scheme to distinguish SSH from other
/// unsupported (non-HTTPS) schemes. This makes the spec-mandated
/// `Uncheckable{ssh|local|unsupportedScheme}` reasons reachable instead of
/// every non-HTTPS source collapsing to a misleading `network` error.
pub fn precheck_source(
	source_type: &str,
	source: &str,
) -> Option<UncheckableReason> {
	if source_type.eq_ignore_ascii_case("local") {
		return Some(UncheckableReason::Local);
	}
	let s = source.trim();
	// scp-like SSH shorthand, e.g. `git@github.com:owner/repo`.
	if s.starts_with("git@") {
		return Some(UncheckableReason::Ssh);
	}
	// Explicit URL scheme.
	if let Some((scheme, _rest)) = s.split_once("://") {
		return match scheme.to_ascii_lowercase().as_str() {
			"https" => None,
			"ssh" => Some(UncheckableReason::Ssh),
			"file" => Some(UncheckableReason::Local),
			// http, git, ftp, … — not fetchable over the HTTPS-only path.
			_ => Some(UncheckableReason::UnsupportedScheme),
		};
	}
	// No scheme: GitHub `owner/repo` (or `host/owner/repo`) shorthand resolves
	// to HTTPS, so proceed to fetch.
	None
}

/// Reject absolute paths and any `..`; join under `root`; canonicalize; verify the
/// result stays under `root`. Returns the safe absolute skill dir, or None to reject.
pub fn sanitize_skill_path(root: &Path, skill_path: &str) -> Option<PathBuf> {
	if skill_path.is_empty() {
		return None;
	}
	let p = Path::new(skill_path);
	if p.is_absolute() {
		return None;
	}
	if p.components()
		.any(|c| matches!(c, std::path::Component::ParentDir))
	{
		return None;
	}
	let joined = root.join(p);
	let canon_root = root.canonicalize().ok()?;
	let canon = joined.canonicalize().ok()?; // also resolves symlinks
	if canon.starts_with(&canon_root) {
		Some(canon)
	} else {
		None
	}
}

pub fn stage_and_swap_dir(
	source_dir: &Path,
	target_dir: &Path,
) -> std::io::Result<()> {
	let parent = target_dir.parent().ok_or_else(|| {
		std::io::Error::new(
			std::io::ErrorKind::InvalidInput,
			format!("target has no parent: {}", target_dir.display()),
		)
	})?;
	std::fs::create_dir_all(parent)?;

	let staging_root = unique_temp_dir(parent, ".aghub-stage")?;
	let staged = staging_root.join("skill");
	copy_dir_recursive_skip_symlinks(source_dir, &staged)?;

	let backup_root = unique_temp_dir(parent, ".aghub-backup")?;
	let backup = backup_root.join("target");
	let had_target = std::fs::symlink_metadata(target_dir).is_ok();
	if had_target {
		std::fs::rename(target_dir, &backup)?;
	}

	let swap_result = std::fs::rename(&staged, target_dir);
	if let Err(error) = swap_result {
		return handle_failed_swap(
			error,
			had_target,
			&backup,
			target_dir,
			&staging_root,
			&backup_root,
		);
	}

	let _ = remove_path_any(&backup_root);
	let _ = remove_path_any(&staging_root);
	Ok(())
}

fn handle_failed_swap(
	error: std::io::Error,
	had_target: bool,
	backup: &Path,
	target_dir: &Path,
	staging_root: &Path,
	backup_root: &Path,
) -> std::io::Result<()> {
	handle_failed_swap_with_rollback(
		error,
		had_target,
		backup,
		target_dir,
		staging_root,
		backup_root,
		|from, to| std::fs::rename(from, to),
	)
}

fn handle_failed_swap_with_rollback(
	error: std::io::Error,
	had_target: bool,
	backup: &Path,
	target_dir: &Path,
	staging_root: &Path,
	backup_root: &Path,
	rollback: impl FnOnce(&Path, &Path) -> std::io::Result<()>,
) -> std::io::Result<()> {
	let _ = remove_path_any(staging_root);
	if !had_target {
		let _ = remove_path_any(backup_root);
		return Err(error);
	}

	if let Err(rollback_error) = rollback(backup, target_dir) {
		return Err(std::io::Error::new(
			error.kind(),
			format!(
				"failed to replace {}; rollback also failed, original \
				 contents retained at {}: swap error: {}; rollback error: {}",
				target_dir.display(),
				backup.display(),
				error,
				rollback_error
			),
		));
	}

	let _ = remove_path_any(backup_root);
	Err(error)
}

fn copy_dir_recursive_skip_symlinks(
	from: &Path,
	to: &Path,
) -> std::io::Result<()> {
	std::fs::create_dir_all(to)?;
	for entry in std::fs::read_dir(from)? {
		let entry = entry?;
		let file_type = entry.file_type()?;
		if file_type.is_symlink() {
			continue;
		}
		let from_path = entry.path();
		let to_path = to.join(entry.file_name());
		if file_type.is_dir() {
			copy_dir_recursive_skip_symlinks(&from_path, &to_path)?;
		} else if file_type.is_file() {
			std::fs::copy(&from_path, &to_path)?;
		}
	}
	Ok(())
}

fn unique_temp_dir(parent: &Path, prefix: &str) -> std::io::Result<PathBuf> {
	static COUNTER: AtomicU64 = AtomicU64::new(0);
	for _ in 0..100 {
		let id = COUNTER.fetch_add(1, Ordering::Relaxed);
		let path = parent.join(format!("{prefix}-{}-{id}", std::process::id()));
		match std::fs::create_dir(&path) {
			Ok(()) => return Ok(path),
			Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
			Err(e) => return Err(e),
		}
	}
	Err(std::io::Error::new(
		std::io::ErrorKind::AlreadyExists,
		"could not create unique staging directory",
	))
}

fn remove_path_any(path: &Path) -> std::io::Result<()> {
	let meta = match std::fs::symlink_metadata(path) {
		Ok(meta) => meta,
		Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
		Err(e) => return Err(e),
	};
	if meta.file_type().is_dir() && !meta.file_type().is_symlink() {
		std::fs::remove_dir_all(path)
	} else {
		std::fs::remove_file(path)
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::fs;
	use tempfile::tempdir;

	#[test]
	fn detect_rename_returns_none_when_names_match() {
		assert_eq!(detect_rename("foo", "foo"), None);
	}

	#[test]
	fn detect_rename_returns_some_new_name_when_names_differ() {
		assert_eq!(
			detect_rename("new-name", "old-name"),
			Some("new-name".to_string())
		);
	}

	#[test]
	fn skill_renamed_message_names_both_and_advises_delete_install() {
		let msg = skill_renamed_message("old", "new");
		assert!(msg.contains("old"));
		assert!(msg.contains("new"));
		assert!(msg.contains("Delete"));
		assert!(msg.contains("install"));
	}

	#[test]
	fn skill_renamed_code_is_the_stable_contract() {
		assert_eq!(SKILL_RENAMED_CODE, "SKILL_RENAMED_IN_SOURCE");
	}

	#[test]
	fn rejects_absolute_skill_path() {
		let root = tempdir().unwrap();
		assert_eq!(sanitize_skill_path(root.path(), "/etc/passwd"), None);
	}
	#[test]
	fn rejects_dotdot_skill_path() {
		let root = tempdir().unwrap();
		assert_eq!(
			sanitize_skill_path(root.path(), "../../secret/SKILL.md"),
			None
		);
	}
	#[test]
	fn accepts_contained_skill_path() {
		let root = tempdir().unwrap();
		fs::create_dir_all(root.path().join("skills/a")).unwrap();
		fs::write(root.path().join("skills/a/SKILL.md"), b"x").unwrap();
		let got =
			sanitize_skill_path(root.path(), "skills/a/SKILL.md").unwrap();
		assert!(got.starts_with(root.path().canonicalize().unwrap()));
	}
	#[cfg(unix)]
	#[test]
	fn rejects_symlink_escape() {
		use std::os::unix::fs::symlink;
		let root = tempdir().unwrap();
		let outside = tempdir().unwrap();
		fs::write(outside.path().join("SKILL.md"), b"x").unwrap();
		symlink(outside.path(), root.path().join("escape")).unwrap();
		assert_eq!(sanitize_skill_path(root.path(), "escape/SKILL.md"), None);
	}

	#[test]
	fn precheck_local_source_type_is_local() {
		// `source_type == "local"` is authoritative regardless of the string.
		assert_eq!(
			precheck_source("local", "/home/u/my-skill"),
			Some(UncheckableReason::Local)
		);
		assert_eq!(
			precheck_source("LOCAL", "anything"),
			Some(UncheckableReason::Local)
		);
	}
	#[test]
	fn precheck_scp_ssh_shorthand_is_ssh() {
		assert_eq!(
			precheck_source("github", "git@github.com:owner/repo.git"),
			Some(UncheckableReason::Ssh)
		);
	}
	#[test]
	fn precheck_ssh_scheme_is_ssh() {
		assert_eq!(
			precheck_source("git", "ssh://git@github.com/owner/repo"),
			Some(UncheckableReason::Ssh)
		);
	}
	#[test]
	fn precheck_file_scheme_is_local() {
		assert_eq!(
			precheck_source("git", "file:///home/u/repo"),
			Some(UncheckableReason::Local)
		);
	}
	#[test]
	fn precheck_non_https_scheme_is_unsupported() {
		for src in ["http://example.com/o/r", "git://h/o/r", "ftp://h/x"] {
			assert_eq!(
				precheck_source("github", src),
				Some(UncheckableReason::UnsupportedScheme),
				"{src}"
			);
		}
	}
	#[test]
	fn precheck_https_and_shorthand_proceed() {
		assert_eq!(precheck_source("github", "https://github.com/o/r"), None);
		assert_eq!(precheck_source("github", "owner/repo"), None);
		assert_eq!(precheck_source("github", "github.com/owner/repo"), None);
	}

	#[test]
	fn stage_and_swap_dir_replaces_target_and_skips_symlinks() {
		let parent = tempdir().unwrap();
		let source = parent.path().join("source");
		let target = parent.path().join("target");
		fs::create_dir_all(source.join("nested")).unwrap();
		fs::write(source.join("SKILL.md"), "new").unwrap();
		fs::write(source.join("nested/file.txt"), "nested").unwrap();
		#[cfg(unix)]
		std::os::unix::fs::symlink(
			parent.path().join("outside"),
			source.join("link"),
		)
		.unwrap();
		fs::create_dir_all(&target).unwrap();
		fs::write(target.join("old.txt"), "old").unwrap();

		stage_and_swap_dir(&source, &target).unwrap();

		assert_eq!(fs::read_to_string(target.join("SKILL.md")).unwrap(), "new");
		assert!(target.join("nested/file.txt").exists());
		assert!(!target.join("old.txt").exists());
		#[cfg(unix)]
		assert!(!target.join("link").exists());
	}

	#[test]
	fn stage_and_swap_dir_creates_fresh_parent() {
		let tmp = tempdir().unwrap();
		let source = tmp.path().join("source");
		let target = tmp.path().join("missing-parent/target");
		fs::create_dir_all(&source).unwrap();
		fs::write(source.join("SKILL.md"), "new").unwrap();

		stage_and_swap_dir(&source, &target).unwrap();

		assert_eq!(fs::read_to_string(target.join("SKILL.md")).unwrap(), "new");
	}

	#[test]
	fn failed_swap_keeps_backup_when_rollback_fails() {
		let tmp = tempdir().unwrap();
		let staging_root = tmp.path().join("stage");
		let backup_root = tmp.path().join("backup-root");
		let backup = backup_root.join("target");
		let target = tmp.path().join("target");
		fs::create_dir(&staging_root).unwrap();
		fs::create_dir_all(&backup).unwrap();
		fs::write(backup.join("old.txt"), "old").unwrap();

		let err = handle_failed_swap_with_rollback(
			std::io::Error::other("swap failed"),
			true,
			&backup,
			&target,
			&staging_root,
			&backup_root,
			|_, _| Err(std::io::Error::other("rollback failed")),
		)
		.unwrap_err();

		let msg = err.to_string();
		assert!(msg.contains("rollback also failed"));
		assert!(msg.contains(&backup.display().to_string()));
		assert!(backup.join("old.txt").exists());
		assert!(backup_root.exists());
		assert!(!staging_root.exists());
	}
}
