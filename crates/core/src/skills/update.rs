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
	UpdateAvailable {
		current: String,
		available: String,
		/// RFC 3339 author-time of the upstream tip commit. Best-effort.
		upstream_commit_time: Option<String>,
	},
	Renamed {
		new_name: String,
	},
	Uncheckable {
		reason: UncheckableReason,
	},
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
pub fn compare_known_hashes(
	stored: &str,
	fresh: &str,
	upstream_commit_time: Option<String>,
) -> SkillUpdateStatus {
	if stored == fresh {
		SkillUpdateStatus::UpToDate
	} else {
		SkillUpdateStatus::UpdateAvailable {
			current: stored.to_string(),
			available: fresh.to_string(),
			upstream_commit_time,
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

	// The swap succeeded — target_dir now holds the new contents. The temp
	// roots are best-effort cleanup, but a failure must not be silent: it
	// leaks a .aghub-stage-*/.aghub-backup-* orphan while the caller is told
	// everything is fine, and nothing else ever sweeps them (prune.rs is
	// lock-only). Warn instead of `let _ =` so the orphan has a signal.
	warn_on_cleanup_failure(&backup_root);
	warn_on_cleanup_failure(&staging_root);
	Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SwapState {
	Prepared,
	OriginalMoved,
	Committed,
	RolledBack,
}

#[derive(Debug)]
struct PreparedDirSwap {
	target: PathBuf,
	staging_root: PathBuf,
	staged: PathBuf,
	backup_root: PathBuf,
	backup: PathBuf,
	had_target: bool,
	state: SwapState,
}

impl PreparedDirSwap {
	fn prepare(source_dir: &Path, target: &Path) -> std::io::Result<Self> {
		let parent = target.parent().ok_or_else(|| {
			std::io::Error::new(
				std::io::ErrorKind::InvalidInput,
				format!("target has no parent: {}", target.display()),
			)
		})?;
		std::fs::create_dir_all(parent)?;

		let staging_root = unique_temp_dir(parent, ".aghub-stage")?;
		let staged = staging_root.join("skill");
		if let Err(error) =
			copy_dir_recursive_skip_symlinks(source_dir, &staged)
		{
			warn_on_cleanup_failure(&staging_root);
			return Err(error);
		}

		let backup_root = match unique_temp_dir(parent, ".aghub-backup") {
			Ok(path) => path,
			Err(error) => {
				warn_on_cleanup_failure(&staging_root);
				return Err(error);
			}
		};
		let backup = backup_root.join("target");
		let had_target = std::fs::symlink_metadata(target).is_ok();

		Ok(Self {
			target: target.to_path_buf(),
			staging_root,
			staged,
			backup_root,
			backup,
			had_target,
			state: SwapState::Prepared,
		})
	}

	fn commit(&mut self) -> std::io::Result<()> {
		if self.had_target {
			std::fs::rename(&self.target, &self.backup)?;
			self.state = SwapState::OriginalMoved;
		}
		std::fs::rename(&self.staged, &self.target)?;
		self.state = SwapState::Committed;
		Ok(())
	}

	fn rollback(&mut self) -> std::io::Result<()> {
		match self.state {
			SwapState::Prepared | SwapState::RolledBack => return Ok(()),
			SwapState::OriginalMoved | SwapState::Committed => {}
		}

		remove_path_any(&self.target)?;
		if self.had_target {
			std::fs::rename(&self.backup, &self.target)?;
		}
		self.state = SwapState::RolledBack;
		Ok(())
	}

	fn cleanup_after_rollback(&self) {
		warn_on_cleanup_failure(&self.staging_root);
		// A failed rollback leaves the only retained original in `backup`. Never
		// erase that recovery copy during best-effort cleanup.
		if std::fs::symlink_metadata(&self.backup).is_err() {
			warn_on_cleanup_failure(&self.backup_root);
		}
	}

	fn finish(self) {
		warn_on_cleanup_failure(&self.backup_root);
		warn_on_cleanup_failure(&self.staging_root);
	}
}

/// A staged multi-target directory rewrite whose old contents remain available
/// until the caller commits its accompanying metadata write. This is the
/// transaction primitive used by skill resync so a late lock-write failure can
/// restore every Master/copy that was already replaced.
pub(super) struct DirSwapTransaction {
	entries: Vec<PreparedDirSwap>,
}

impl DirSwapTransaction {
	pub(super) fn prepare(
		source_dir: &Path,
		targets: &[PathBuf],
	) -> std::io::Result<Self> {
		let mut entries = Vec::with_capacity(targets.len());
		for target in targets {
			match PreparedDirSwap::prepare(source_dir, target) {
				Ok(entry) => entries.push(entry),
				Err(error) => {
					for entry in &entries {
						entry.cleanup_after_rollback();
					}
					return Err(error);
				}
			}
		}
		Ok(Self { entries })
	}

	pub(super) fn commit_all(&mut self) -> std::io::Result<()> {
		for entry in &mut self.entries {
			if let Err(error) = entry.commit() {
				return Err(std::io::Error::new(
					error.kind(),
					format!("{}: {error}", entry.target.display()),
				));
			}
		}
		Ok(())
	}

	pub(super) fn rollback(&mut self) -> Result<(), String> {
		let mut failures = Vec::new();
		for entry in self.entries.iter_mut().rev() {
			if let Err(error) = entry.rollback() {
				let recovery = if std::fs::symlink_metadata(&entry.backup)
					.is_ok()
				{
					format!("; original retained at {}", entry.backup.display())
				} else {
					String::new()
				};
				failures.push(format!(
					"{}: {error}{recovery}",
					entry.target.display()
				));
			}
		}
		for entry in &self.entries {
			entry.cleanup_after_rollback();
		}
		if failures.is_empty() {
			Ok(())
		} else {
			Err(failures.join("; "))
		}
	}

	pub(super) fn finish(self) {
		for entry in self.entries {
			entry.finish();
		}
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
	// The swap error is the failure the caller must see, so a cleanup error
	// here only warns — it must never mask `error`.
	warn_on_cleanup_failure(staging_root);
	if !had_target {
		warn_on_cleanup_failure(backup_root);
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

	warn_on_cleanup_failure(backup_root);
	Err(error)
}

/// Best-effort temp removal whose ONLY job is to never silently swallow a
/// cleanup failure: it logs at warn level and otherwise ignores the result.
/// Used after a successful swap AND on the swap-failure paths — in both cases
/// the orphan temp is not fatal, but leaving it with no signal is what let a
/// `.aghub-stage-*`/`.aghub-backup-*` build up unnoticed.
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
	fn multi_target_commit_failure_rolls_back_an_earlier_swap() {
		let tmp = tempdir().unwrap();
		let source = tmp.path().join("source");
		let first = tmp.path().join("first/skill");
		let second = tmp.path().join("second/skill");
		fs::create_dir_all(&source).unwrap();
		fs::write(source.join("SKILL.md"), "new").unwrap();
		for target in [&first, &second] {
			fs::create_dir_all(target).unwrap();
			fs::write(target.join("SKILL.md"), "old").unwrap();
		}

		let mut transaction = DirSwapTransaction::prepare(
			&source,
			&[first.clone(), second.clone()],
		)
		.unwrap();
		// Force the second commit to fail only after the first target has been
		// replaced: its reserved backup destination is now non-empty.
		fs::create_dir_all(&transaction.entries[1].backup).unwrap();
		fs::write(
			transaction.entries[1].backup.join("occupied"),
			"block rename",
		)
		.unwrap();

		transaction.commit_all().unwrap_err();
		assert_eq!(
			fs::read_to_string(first.join("SKILL.md")).unwrap(),
			"new",
			"the test must fail after an earlier destructive swap",
		);
		transaction.rollback().unwrap();

		assert_eq!(fs::read_to_string(first.join("SKILL.md")).unwrap(), "old");
		assert_eq!(fs::read_to_string(second.join("SKILL.md")).unwrap(), "old");
	}

	#[cfg(unix)]
	#[test]
	fn transaction_rollback_failure_retains_and_reports_original_backup() {
		use std::os::unix::ffi::OsStringExt;

		let tmp = tempdir().unwrap();
		let source = tmp.path().join("source");
		let target = tmp.path().join("installed/skill");
		fs::create_dir_all(&source).unwrap();
		fs::write(source.join("SKILL.md"), "new").unwrap();
		fs::create_dir_all(&target).unwrap();
		fs::write(target.join("SKILL.md"), "old").unwrap();

		let mut transaction =
			DirSwapTransaction::prepare(&source, std::slice::from_ref(&target))
				.unwrap();
		transaction.commit_all().unwrap();
		let backup = transaction.entries[0].backup.clone();
		let backup_root = transaction.entries[0].backup_root.clone();
		assert_eq!(fs::read_to_string(backup.join("SKILL.md")).unwrap(), "old");

		// A NUL-containing path is rejected by every Unix filesystem API. Pointing
		// the private test fixture at one deterministically exercises rollback's
		// own failure branch even when the suite runs as root.
		transaction.entries[0].target = PathBuf::from(
			std::ffi::OsString::from_vec(b"invalid\0target".to_vec()),
		);
		let error = transaction.rollback().unwrap_err();

		assert!(error.contains(&backup.display().to_string()));
		assert!(backup_root.exists(), "the recovery container must survive");
		assert_eq!(
			fs::read_to_string(backup.join("SKILL.md")).unwrap(),
			"old",
			"the only original copy must survive a failed rollback"
		);
		assert_eq!(
			fs::read_to_string(target.join("SKILL.md")).unwrap(),
			"new",
			"the fixture must prove commit happened before rollback failed"
		);
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
