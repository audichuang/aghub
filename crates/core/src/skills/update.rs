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

/// Structured reason a transactional skill swap/rename could not complete, so
/// callers render a reason + one actionable next step instead of a bare
/// "may need manual recovery" string. The universal-rename path
/// (`manager::skill::rollback_master_rename`) maps onto the same enum.
#[derive(Debug, Clone)]
pub enum RecoveryHint {
	/// EBUSY/EACCES/PermissionDenied on a rename — something holds the path.
	LockHeld { path: PathBuf },
	/// A path vanished mid-operation (NotFound).
	MissingDir { path: PathBuf },
	/// A dangling/foreign symlink is blocking a relink.
	BrokenSymlink { link: PathBuf },
	/// The rollback itself failed — `recover_from` is the only surviving copy
	/// of the original contents and must be moved back to `restore_to` by hand.
	ManualRestore {
		recover_from: PathBuf,
		restore_to: PathBuf,
	},
}

impl RecoveryHint {
	/// One actionable line. The only raw interpolation is the path(s).
	pub fn next_step(&self) -> String {
		match self {
			RecoveryHint::LockHeld { path } => format!(
				"close any process holding {} and retry",
				path.display()
			),
			RecoveryHint::MissingDir { path } => format!(
				"{} disappeared mid-operation; retry the install",
				path.display()
			),
			RecoveryHint::BrokenSymlink { link } => {
				format!("remove the broken link {} and retry", link.display())
			}
			RecoveryHint::ManualRestore {
				recover_from,
				restore_to,
			} => format!(
				"original contents retained at {}; move them back to {} \
				 to recover",
				recover_from.display(),
				restore_to.display()
			),
		}
	}
}

/// Result of a SUCCESSFUL [`stage_and_swap_dir`]: the target now holds the
/// new contents. `cleanup_warning` is `Some` when the post-swap temp sweep
/// failed — the swap itself is done, so callers MUST proceed (update locks,
/// continue remaining targets) and surface the warning, never treat it as a
/// swap failure: aborting here leaves new content on disk with a stale lock
/// and an error message claiming nothing was replaced.
#[must_use]
#[derive(Debug)]
pub struct SwapOutcome {
	pub cleanup_warning: Option<String>,
}

pub fn stage_and_swap_dir(
	source_dir: &Path,
	target_dir: &Path,
) -> std::io::Result<SwapOutcome> {
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
		handle_failed_swap(
			error,
			had_target,
			&backup,
			target_dir,
			&staging_root,
			&backup_root,
		)?;
		unreachable!("handle_failed_swap always returns Err");
	}

	// The swap succeeded — target_dir now holds the new contents — but the
	// staging/backup temps must still be swept. A failure here means we'd be
	// claiming success while leaking a .aghub-stage-*/.aghub-backup-* orphan,
	// so surface it as a WARNING on the outcome (never as an Err: the swap is
	// done, and callers aborting on it would skip the lock update and lie).
	Ok(SwapOutcome {
		cleanup_warning: cleanup_swap_temps(&staging_root, &backup_root)
			.err()
			.map(|e| e.to_string()),
	})
}

/// Sweep the post-swap temp roots. The swap itself already succeeded, so this
/// is best-effort cleanup — but a failure must not be silently swallowed: it
/// would leave an orphan while the caller is told everything is fine. Returns
/// the first removal error (with the offending path named), or `Ok(())`.
fn cleanup_swap_temps(
	staging_root: &Path,
	backup_root: &Path,
) -> std::io::Result<()> {
	let mut first_error: Option<std::io::Error> = None;
	for root in [backup_root, staging_root] {
		if let Err(e) = remove_path_any(root) {
			log::warn!(
				"failed to remove skill-swap temp {}: {e}",
				root.display()
			);
			let e = std::io::Error::new(
				e.kind(),
				format!(
					"swap succeeded but cleanup of {} failed: {e}",
					root.display()
				),
			);
			first_error.get_or_insert(e);
		}
	}
	match first_error {
		Some(e) => Err(e),
		None => Ok(()),
	}
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
	// Fresh install (no prior target): the staged copy is the only orphan and
	// it is swept above; nothing was backed up. Sweep the empty backup root too
	// so no .aghub-stage-*/.aghub-backup-* survives the failure. The swap error
	// is the real failure we must return, so a cleanup error here only warns —
	// it must not mask `error`.
	warn_on_cleanup_failure(staging_root);
	if !had_target {
		warn_on_cleanup_failure(backup_root);
		return Err(error);
	}

	if let Err(rollback_error) = rollback(backup, target_dir) {
		// Rollback failed: the backup is now the ONLY copy of the original
		// contents, so keep it and report WHERE + the next step structurally.
		let hint = RecoveryHint::ManualRestore {
			recover_from: backup.to_path_buf(),
			restore_to: target_dir.to_path_buf(),
		};
		return Err(std::io::Error::new(
			error.kind(),
			format!(
				"failed to replace {}; rollback also failed ({}); {}",
				target_dir.display(),
				rollback_error,
				hint.next_step(),
			),
		));
	}

	// Rollback succeeded: original contents are back at target_dir, so neither
	// the staging nor the backup root is needed — sweep both, leaving no orphan.
	// As above, a cleanup error only warns; `error` is the failure to report.
	warn_on_cleanup_failure(backup_root);
	Err(error)
}

/// Best-effort temp removal whose ONLY job is to never silently swallow a
/// cleanup failure: it logs at warn level and otherwise ignores the result.
/// Used on the swap-failure paths, where the swap error itself is the value the
/// caller must see — so a cleanup error here is surfaced via the log, not by
/// masking the original error.
fn warn_on_cleanup_failure(path: &Path) {
	if let Err(e) = remove_path_any(path) {
		log::warn!("failed to remove skill-swap temp {}: {e}", path.display());
	}
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

		let outcome = stage_and_swap_dir(&source, &target).unwrap();

		assert!(outcome.cleanup_warning.is_none());
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

		let outcome = stage_and_swap_dir(&source, &target).unwrap();

		assert!(outcome.cleanup_warning.is_none());
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

		// The message is now built from RecoveryHint::ManualRestore so it names
		// both the recover-from (backup) and restore-to (target) paths plus an
		// actionable next step.
		let msg = err.to_string();
		assert!(
			msg.contains(&backup.display().to_string()),
			"recover_from path missing from: {msg}"
		);
		assert!(
			msg.contains(&target.display().to_string()),
			"restore_to path missing from: {msg}"
		);
		assert!(
			msg.contains("move"),
			"missing actionable next step in: {msg}"
		);
		assert!(backup.join("old.txt").exists());
		assert!(backup_root.exists());
		assert!(!staging_root.exists());
	}

	#[test]
	fn recovered_swap_failure_leaves_no_orphans() {
		let tmp = tempdir().unwrap();
		let staging_root = tmp.path().join("stage");
		let backup_root = tmp.path().join("backup-root");
		let backup = backup_root.join("target");
		let target = tmp.path().join("target");
		fs::create_dir(&staging_root).unwrap();
		fs::create_dir_all(&backup).unwrap();
		fs::write(backup.join("old.txt"), "old").unwrap();

		// Rollback SUCCEEDS: the original contents are restored to target.
		let err = handle_failed_swap_with_rollback(
			std::io::Error::other("swap failed"),
			true,
			&backup,
			&target,
			&staging_root,
			&backup_root,
			|from, to| std::fs::rename(from, to),
		)
		.unwrap_err();

		assert_eq!(err.to_string(), "swap failed");
		assert!(target.join("old.txt").exists());
		// No .aghub-stage-* / .aghub-backup-* survives a recovered failure.
		assert!(!staging_root.exists(), "staging orphan survived recovery");
		assert!(!backup_root.exists(), "backup orphan survived recovery");
	}

	#[cfg(unix)]
	#[test]
	fn failed_swap_no_prior_target_sweeps_staging() {
		use std::os::unix::fs::PermissionsExt;

		let tmp = tempdir().unwrap();

		// Root bypasses 0o555 perms — skip there, otherwise the swap rename
		// would succeed and the error path under test never runs.
		if !perms_enforced(tmp.path()) {
			eprintln!("skip: running as root, 0o555 not enforced");
			return;
		}

		let source = tmp.path().join("source");
		fs::create_dir_all(&source).unwrap();
		fs::write(source.join("SKILL.md"), "new").unwrap();

		// target's parent exists but is read-only, so create_dir_all of the
		// parent is a no-op (already there) yet the final swap rename INTO it
		// fails — fresh install (had_target = false).
		let parent = tmp.path().join("ro-parent");
		fs::create_dir_all(&parent).unwrap();
		let target = parent.join("target");
		let orig = fs::metadata(&parent).unwrap().permissions();
		fs::set_permissions(&parent, fs::Permissions::from_mode(0o555))
			.unwrap();

		let res = stage_and_swap_dir(&source, &target);

		// Restore perms BEFORE asserting so a failure can't leak the temp dir.
		fs::set_permissions(&parent, orig).unwrap();

		assert!(res.is_err(), "expected swap into read-only parent to fail");
		assert!(!target.exists(), "target must not exist on failed install");
		// No .aghub-stage-* orphan survives the fresh-install failure path.
		let leftovers: Vec<_> = fs::read_dir(&parent)
			.unwrap()
			.filter_map(|e| e.ok())
			.map(|e| e.file_name().to_string_lossy().into_owned())
			.filter(|n| n.starts_with(".aghub-stage"))
			.collect();
		assert!(leftovers.is_empty(), "staging orphans left: {leftovers:?}");
	}

	/// Frozen npx round-trip: after a successful GLOBAL apply-update, the lock
	/// entry must carry the REAL updated folder hash in `contentHash`, leave the
	/// legacy `skillFolderHash` empty (the v3 mutual-exclusion invariant), and
	/// record `refCommit`. Guards against a half-migrated entry that would make
	/// an aghub-written lock unreadable by `npx skills`.
	#[test]
	fn global_apply_update_pins_frozen_lock_contract() {
		use crate::skills::prune::test_lock::GlobalLockGuard;

		let _guard = GlobalLockGuard::new();

		// A real skill folder + its real Source hash — the value apply-update
		// recomputes from the freshly swapped target. Asserting against this
		// (not just `is_some()`) catches a stub/empty hash being written.
		let folder = tempdir().unwrap();
		fs::write(
			folder.path().join("SKILL.md"),
			"---\nname: my-skill\ndescription: d\n---\nbody\n",
		)
		.unwrap();
		let updated_hash =
			skill::compute_skill_folder_hash(folder.path()).unwrap();

		// Seed an npx-written entry: legacy folder hash populated, no contentHash.
		skill::lock::global::add_skill_to_lock(
			"my-skill",
			skill::SkillLockEntry {
				source: "owner/repo".to_string(),
				source_type: "github".to_string(),
				source_url: "https://github.com/owner/repo".to_string(),
				ref_name: Some("main".to_string()),
				skill_path: Some("SKILL.md".to_string()),
				skill_folder_hash: "stale-gh-tree-sha".to_string(),
				content_hash: None,
				ref_commit: None,
				installed_at: "t".to_string(),
				updated_at: "t".to_string(),
				plugin_name: None,
			},
		)
		.unwrap();

		// The exact frozen-contract mutation the global apply-update performs:
		// apply_content_hash (sets contentHash + clears skillFolderHash) plus the
		// refCommit write.
		let ref_commit = "deadbeefcafef00ddeadbeefcafef00ddeadbeef";
		skill::lock::global::modify_skill_lock(|lock| {
			let entry = lock.skills.get_mut("my-skill").unwrap();
			entry.apply_content_hash(&updated_hash, "2026-06-28T00:00:00Z");
			entry.ref_commit = Some(ref_commit.to_string());
		})
		.unwrap();

		let lock = skill::lock::global::read_skill_lock();
		let entry = &lock.skills["my-skill"];
		assert_eq!(
			entry.content_hash.as_deref(),
			Some(updated_hash.as_str()),
			"contentHash must equal the recomputed folder hash"
		);
		assert_eq!(
			entry.skill_folder_hash, "",
			"legacy skillFolderHash must be cleared (v3 invariant)"
		);
		assert_eq!(
			entry.ref_commit.as_deref(),
			Some(ref_commit),
			"refCommit must be recorded for the next ls-refs preflight"
		);
	}

	#[cfg(unix)]
	#[test]
	fn successful_swap_surfaces_cleanup_failure_not_silently() {
		use std::os::unix::fs::PermissionsExt;

		let tmp = tempdir().unwrap();

		// Root bypasses 0o555 perms — the cleanup would succeed and the path
		// under test never runs.
		if !perms_enforced(tmp.path()) {
			eprintln!("skip: running as root, 0o555 not enforced");
			return;
		}

		// A backup root that cannot be removed (read-only dir with a child):
		// remove_dir_all needs write on the dir to unlink the child, so the
		// post-swap cleanup of this leftover fails.
		let backup_root = tmp.path().join("backup-root");
		fs::create_dir_all(backup_root.join("target")).unwrap();
		fs::write(backup_root.join("target/old.txt"), "old").unwrap();
		fs::set_permissions(&backup_root, fs::Permissions::from_mode(0o555))
			.unwrap();

		let staging_root = tmp.path().join("stage");
		fs::create_dir(&staging_root).unwrap();

		let res = cleanup_swap_temps(&staging_root, &backup_root);

		// Restore perms BEFORE asserting so a failure can't leak the temp dir.
		fs::set_permissions(&backup_root, fs::Permissions::from_mode(0o755))
			.unwrap();

		// A cleanup failure on the success path must NOT be swallowed: the
		// caller's Ok(()) would otherwise hide the orphan it just leaked.
		assert!(
			res.is_err(),
			"cleanup failure on the success path was silently swallowed"
		);
		// The leftover the caller was told nothing about must be named.
		let msg = res.unwrap_err().to_string();
		assert!(
			msg.contains(&backup_root.display().to_string()),
			"orphan path missing from error: {msg}"
		);
	}

	#[cfg(unix)]
	fn perms_enforced(under: &Path) -> bool {
		use std::os::unix::fs::PermissionsExt;
		let p = under.join(format!(".perm-probe-{}", std::process::id()));
		let _ = fs::remove_dir_all(&p);
		fs::create_dir(&p).unwrap();
		fs::set_permissions(&p, fs::Permissions::from_mode(0o555)).unwrap();
		let blocked = fs::write(p.join("x"), b"x").is_err();
		fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
		let _ = fs::remove_dir_all(&p);
		blocked
	}
}
