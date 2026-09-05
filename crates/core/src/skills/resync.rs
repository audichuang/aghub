//! Re-sync an already-installed skill from a freshly-fetched source folder: the
//! single deep transaction shared by the CLI `apply-update` / `source sync` paths
//! and the API apply-update / git-sync routes.
//!
//! Callers own everything I/O-shaped up to a resolved on-disk source dir (fetch,
//! session, credential, sanitize, request validation); this owns the transaction:
//! discover installed targets → rename guard → hash → strict containment → stage
//! every target → swap every target → lock re-stamp. Old contents stay in sibling
//! backups until the lock commits; any swap or lock failure rolls every replaced
//! target back, so disk content and the lock advance together.

use crate::models::ResourceScope;
use std::path::{Path, PathBuf};

/// Inputs for [`resync_installed_skill`]. `source_dir` is an already-sanitized
/// skill folder (containing `SKILL.md`); `name` is the locked name the source
/// must still match.
pub struct ResyncRequest<'a> {
	pub source_dir: &'a Path,
	pub name: &'a str,
	pub scope: ResourceScope,
	pub project_root: Option<&'a Path>,
	pub ref_commit: Option<&'a str>,
	/// The entry's identity as [`captured`](crate::skills::lock::EntryIdentity::capture)
	/// BEFORE the fetch, re-verified under the mutation lock before anything is
	/// swapped — the unlocked read → fetch → mutate window is seconds wide and the
	/// lock alone cannot cover it.
	///
	/// Required, with no opt-out: a caller whose capture returned `None` never saw
	/// the entry and has no mandate to overwrite it, so it must refuse rather than
	/// pass "nothing to compare" down here.
	pub expected: crate::skills::lock::EntryIdentity,
}

/// Outcome of a successful resync: every installed target was swapped to the new
/// content and `updated_hash` (the source folder hash) was recorded in the lock.
#[derive(Debug)]
pub struct ResyncReport {
	pub swapped: Vec<PathBuf>,
	pub updated_hash: String,
}

/// Why a resync could not complete. Surfaces map these onto their own
/// codes/messages (HTTP status codes, anyhow text).
#[derive(Debug)]
pub enum ResyncError {
	/// The interprocess mutation lock could not be taken (nothing was mutated).
	Locked(String),
	/// The lock entry changed source/skillPath while this resync was fetching, so
	/// it is no longer the entry that was fetched (nothing was mutated).
	StaleFetch(String),
	/// The skill has no installed copy on disk in this scope.
	NotInstalled,
	/// The fetched source's frontmatter `name` no longer matches the locked name.
	Renamed { new_name: String },
	/// The fetched `SKILL.md` could not be parsed.
	Parse(String),
	/// The source folder hash could not be computed.
	Hash(String),
	/// An installed target resolved outside the allow-listed skill roots.
	OutOfTree(String),
	/// One or more installed targets failed to swap; completed swaps were rolled
	/// back and the lock was left unchanged.
	Swap(String),
	/// The lock could not be re-stamped after the swap; installed targets were
	/// rolled back to their prior contents.
	LockUpdate(String),
}

impl std::fmt::Display for ResyncError {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			Self::Locked(e) => write!(f, "{e}"),
			Self::StaleFetch(e) => write!(f, "{e}"),
			Self::NotInstalled => {
				write!(f, "skill is locked but no installed copy was found")
			}
			Self::Renamed { new_name } => {
				write!(f, "source skill was renamed to '{new_name}'")
			}
			Self::Parse(e) => write!(f, "failed to parse fetched skill: {e}"),
			Self::Hash(e) => write!(f, "failed to hash fetched skill: {e}"),
			Self::OutOfTree(e) => write!(f, "{e}"),
			Self::Swap(e) => {
				write!(f, "failed to replace installed skill: {e}")
			}
			Self::LockUpdate(e) => write!(f, "{e}"),
		}
	}
}

/// The stable machine code for a resync failure — ONE classification, read by
/// every surface.
///
/// Surfaces own their WORDING (the API must stay path-free; the CLI names the
/// skill), but not the classification: the CLI's `source sync` used to render a
/// `StaleFetch` as free text with no code at all, while the API answered 409 +
/// `SOURCE_CHANGED_DURING_FETCH` for the very same condition — and the API's own
/// comment claimed that was "the same answer the CLI's sync gives".
pub fn resync_error_code(error: &ResyncError) -> &'static str {
	match error {
		ResyncError::Locked(_) => crate::skills::lock::MUTATION_LOCK_BUSY_CODE,
		ResyncError::StaleFetch(_) => {
			crate::skills::lock::SOURCE_CHANGED_DURING_FETCH_CODE
		}
		ResyncError::NotInstalled => "SKILL_NOT_INSTALLED",
		ResyncError::Renamed { .. } => {
			crate::skills::update::SKILL_RENAMED_CODE
		}
		ResyncError::Parse(_) => "SKILL_PARSE_FAILED",
		ResyncError::OutOfTree(_) => "SKILL_TARGET_OUT_OF_TREE",
		ResyncError::Hash(_) | ResyncError::Swap(_) => "SKILL_SYNC_ERROR",
		ResyncError::LockUpdate(_) => "SKILL_LOCK_ERROR",
	}
}

/// Replace every installed copy of `name` with the content at `source_dir` and
/// re-stamp the lock. See the module docs for the seam and the ordering.
pub fn resync_installed_skill(
	req: ResyncRequest,
) -> Result<ResyncReport, ResyncError> {
	// Hold the interprocess mutation lock across stage-and-swap AND the lock
	// re-stamp, so a concurrent aghub cannot swap content under the hash we are
	// about to record (nor be rolled back over by ours).
	let _mutation_guard = crate::skills::lock::mutation_guard(
		"resync skill",
		req.scope,
		req.project_root,
	)
	.map_err(|e| ResyncError::Locked(e.to_string()))?;

	let targets = crate::skills::removal::installed_skill_roots(
		req.name,
		req.scope,
		req.project_root,
	);
	if targets.is_empty() {
		return Err(ResyncError::NotInstalled);
	}

	// Compare-after-fetch: the caller read these coordinates, then fetched over
	// the network (seconds), so prove under the lock that the entry is still the
	// one it fetched before overwriting installed content and stamping a hash.
	// After the disk check, which is cheaper and a more specific answer when there
	// is simply nothing installed to resync.
	req.expected
		.ensure_unchanged(req.name, req.scope, req.project_root)
		.map_err(ResyncError::StaleFetch)?;

	// Rename guard: refuse to overwrite when the upstream frontmatter `name`
	// diverged from the locked name (same skillPath, renamed skill).
	let parsed = skill::parse(&req.source_dir.join("SKILL.md"))
		.map_err(|e| ResyncError::Parse(e.to_string()))?;
	if let Some(new_name) =
		crate::skills::update::detect_rename(&parsed.name, req.name)
	{
		return Err(ResyncError::Renamed { new_name });
	}

	let updated_hash = skill::compute_skill_folder_hash(req.source_dir)
		.map_err(|e| ResyncError::Hash(e.to_string()))?;

	let agent_dirs = crate::skills::removal::agent_skill_dirs_in_scope(
		req.scope,
		req.project_root,
	);
	crate::skills::removal::assert_targets_strictly_contained(
		&targets,
		&agent_dirs,
		req.project_root,
	)
	.map_err(|e| ResyncError::OutOfTree(e.to_string()))?;

	// Stage every target before the first destructive rename, then retain every
	// old directory until the lock write commits. Universal installs resolve to
	// one canonical Master target (Referrers remain symlinks); N>1 covers legacy
	// isolated copies. Either way a late failure rolls every replaced root back.
	let mut transaction = crate::skills::update::DirSwapTransaction::prepare(
		req.source_dir,
		&targets,
	)
	.map_err(|e| ResyncError::Swap(e.to_string()))?;
	if let Err(error) = transaction.commit_all() {
		let detail = match transaction.rollback() {
			Ok(()) => error.to_string(),
			Err(rollback) => {
				format!("{error}; rollback also failed: {rollback}")
			}
		};
		return Err(ResyncError::Swap(detail));
	}

	if let Err(error) = crate::skills::lock::update_lock_hash(
		req.name,
		req.scope,
		req.project_root,
		&updated_hash,
		req.ref_commit,
	) {
		let detail = match transaction.rollback() {
			Ok(()) => error,
			Err(rollback) => {
				format!("{error}; rollback also failed: {rollback}")
			}
		};
		return Err(ResyncError::LockUpdate(detail));
	}
	transaction.finish();

	Ok(ResyncReport {
		swapped: targets,
		updated_hash,
	})
}

#[cfg(test)]
mod tests {
	use super::*;

	/// Every variant has a code, they are distinct where the surfaces treat them
	/// differently, and the two that carry a documented wire contract are
	/// spelled exactly as the HTTP API already publishes them.
	#[test]
	fn every_resync_error_has_its_published_code() {
		let cases = [
			(ResyncError::Locked("x".into()), "SKILL_MUTATION_LOCK_BUSY"),
			(
				ResyncError::StaleFetch("x".into()),
				"SKILL_SOURCE_CHANGED_DURING_FETCH",
			),
			(ResyncError::NotInstalled, "SKILL_NOT_INSTALLED"),
			(
				ResyncError::Renamed {
					new_name: "n".into(),
				},
				"SKILL_RENAMED_IN_SOURCE",
			),
			(ResyncError::Parse("x".into()), "SKILL_PARSE_FAILED"),
			(
				ResyncError::OutOfTree("x".into()),
				"SKILL_TARGET_OUT_OF_TREE",
			),
			(ResyncError::Hash("x".into()), "SKILL_SYNC_ERROR"),
			(ResyncError::Swap("x".into()), "SKILL_SYNC_ERROR"),
			(ResyncError::LockUpdate("x".into()), "SKILL_LOCK_ERROR"),
		];
		for (error, expected) in cases {
			assert_eq!(
				resync_error_code(&error),
				expected,
				"{error:?} must keep the code both surfaces publish"
			);
		}
	}

	/// A code that leaked the error's own text would be neither stable nor
	/// machine-readable, and a fetch race must never read as a lock-busy retry.
	#[test]
	fn a_stale_fetch_is_not_confused_with_a_busy_lock() {
		assert_ne!(
			resync_error_code(&ResyncError::StaleFetch("x".into())),
			resync_error_code(&ResyncError::Locked("x".into())),
			"one is retryable by waiting, the other needs a re-fetch"
		);
	}

	fn write_skill(dir: &Path, name: &str, desc: &str) {
		std::fs::create_dir_all(dir).unwrap();
		std::fs::write(
			dir.join("SKILL.md"),
			format!("---\nname: {name}\ndescription: {desc}\n---\n\n{desc}\n"),
		)
		.unwrap();
	}

	/// The coordinates `lock_entry()` describes — what a caller would have
	/// fetched from. `source_url` is None there, so the effective source is
	/// `source`.
	/// The identity a caller's pre-fetch capture would have returned for the
	/// fixture entry. Captured, never hand-built — the same rule production obeys.
	fn captured(
		name: &str,
		project: &Path,
	) -> crate::skills::lock::EntryIdentity {
		crate::skills::lock::EntryIdentity::capture(
			name,
			ResourceScope::ProjectOnly,
			Some(project),
		)
		.expect("fixture entry must exist before capture")
	}

	fn lock_entry() -> skill::LocalSkillLockEntry {
		skill::LocalSkillLockEntry {
			source_url: None,
			source: "owner/repo".to_string(),
			ref_name: Some("main".to_string()),
			source_type: "github".to_string(),
			computed_hash: "old".to_string(),
			skill_path: Some("s/SKILL.md".to_string()),
			ref_commit: None,
		}
	}

	#[test]
	fn swaps_installed_skill_and_restamps_lock() {
		let tmp = tempfile::tempdir().unwrap();
		let project = tmp.path().join("project");
		let installed = project.join(".claude/skills/sync-me");
		write_skill(&installed, "sync-me", "old");
		skill::add_skill_to_local_lock("sync-me", lock_entry(), Some(&project))
			.unwrap();

		let source = tmp.path().join("src/sync-me");
		write_skill(&source, "sync-me", "new");

		let report = resync_installed_skill(ResyncRequest {
			source_dir: &source,
			name: "sync-me",
			scope: ResourceScope::ProjectOnly,
			project_root: Some(&project),
			ref_commit: Some("deadbeefcafef00d"),
			expected: captured("sync-me", &project),
		})
		.unwrap();

		assert!(report.swapped.iter().any(|p| p.ends_with("sync-me")));
		assert!(std::fs::read_to_string(installed.join("SKILL.md"))
			.unwrap()
			.contains("new"));
		let lock = skill::lock::local::read_local_lock(Some(&project));
		assert_eq!(
			lock.skills["sync-me"].ref_commit.as_deref(),
			Some("deadbeefcafef00d")
		);
	}

	#[test]
	fn errors_when_not_installed() {
		let tmp = tempfile::tempdir().unwrap();
		let source = tmp.path().join("src/ghost");
		write_skill(&source, "ghost", "x");
		// Locked but never installed: the disk check must answer first, so the
		// identity capture below is satisfiable yet irrelevant.
		let project = tmp.path().join("project");
		skill::add_skill_to_local_lock("ghost", lock_entry(), Some(&project))
			.unwrap();

		let err = resync_installed_skill(ResyncRequest {
			source_dir: &source,
			name: "ghost",
			scope: ResourceScope::ProjectOnly,
			project_root: Some(&project),
			ref_commit: None,
			expected: captured("ghost", &project),
		})
		.unwrap_err();
		assert!(matches!(err, ResyncError::NotInstalled));
	}

	#[test]
	fn refuses_renamed_source_without_touching_disk() {
		let tmp = tempfile::tempdir().unwrap();
		let project = tmp.path().join("project");
		let installed = project.join(".claude/skills/keep");
		write_skill(&installed, "keep", "old");
		skill::add_skill_to_local_lock("keep", lock_entry(), Some(&project))
			.unwrap();

		let source = tmp.path().join("src/keep");
		write_skill(&source, "renamed-upstream", "new");

		let err = resync_installed_skill(ResyncRequest {
			source_dir: &source,
			name: "keep",
			scope: ResourceScope::ProjectOnly,
			project_root: Some(&project),
			ref_commit: None,
			expected: captured("keep", &project),
		})
		.unwrap_err();
		assert!(matches!(err, ResyncError::Renamed { .. }));
		assert!(std::fs::read_to_string(installed.join("SKILL.md"))
			.unwrap()
			.contains("old"));
	}

	/// Compare-after-fetch, the widest window in the subsystem: the caller read
	/// the entry, then spent SECONDS fetching over the network while another aghub
	/// process repointed that entry at a different source. The resync must refuse
	/// and leave the replacement's installed content untouched — overwriting it
	/// would destroy the other process's work and stamp a hash that disagrees with
	/// the lock's own coordinates.
	#[test]
	fn refuses_when_the_entry_was_repointed_during_the_fetch() {
		let tmp = tempfile::tempdir().unwrap();
		let project = tmp.path().join("project");
		let installed = project.join(".claude/skills/sync-me");
		write_skill(&installed, "sync-me", "the other process's content");

		// Capture the identity our fetch started from, THEN let "another process"
		// repoint the entry at a different source — the real ordering.
		skill::add_skill_to_local_lock("sync-me", lock_entry(), Some(&project))
			.unwrap();
		let expected = captured("sync-me", &project);
		let mut repointed = lock_entry();
		repointed.source = "someone-else/repo".to_string();
		repointed.source_url =
			Some("https://example.com/someone-else/repo".to_string());
		skill::add_skill_to_local_lock("sync-me", repointed, Some(&project))
			.unwrap();
		let lock_before = skill::lock::local::read_local_lock(Some(&project));

		let source = tmp.path().join("src/sync-me");
		write_skill(&source, "sync-me", "our stale fetch");

		let err = resync_installed_skill(ResyncRequest {
			source_dir: &source,
			name: "sync-me",
			scope: ResourceScope::ProjectOnly,
			project_root: Some(&project),
			ref_commit: Some("newoid"),
			// What WE fetched, captured before the entry was repointed.
			expected,
		})
		.unwrap_err();

		assert!(
			matches!(err, ResyncError::StaleFetch(_)),
			"a repointed entry must be refused, got {err:?}"
		);
		assert!(
			std::fs::read_to_string(installed.join("SKILL.md"))
				.unwrap()
				.contains("the other process's content"),
			"the other process's installed content must survive untouched"
		);
		assert_eq!(
			skill::lock::local::read_local_lock(Some(&project)).skills,
			lock_before.skills,
			"a refused resync must not advance the lock"
		);
	}

	/// Every coordinate binds, not just the source. Without the `ref_name` and
	/// `skillPath` comparisons these two cases pass and the swap proceeds: the ref
	/// case overwrites `stable` content with `main` and stamps only the hash/OID,
	/// so disk and lock disagree; the path case overwrites from a folder the entry
	/// no longer names.
	#[test]
	fn refuses_when_only_the_ref_or_only_the_path_changed() {
		for (label, mutate) in [
			(
				"ref",
				(|e: &mut skill::LocalSkillLockEntry| {
					e.ref_name = Some("stable".to_string());
				}) as fn(&mut skill::LocalSkillLockEntry),
			),
			("skillPath", |e: &mut skill::LocalSkillLockEntry| {
				e.skill_path = Some("moved/SKILL.md".to_string());
			}),
		] {
			let tmp = tempfile::tempdir().unwrap();
			let project = tmp.path().join("project");
			let installed = project.join(".claude/skills/sync-me");
			write_skill(&installed, "sync-me", "content to protect");
			skill::add_skill_to_local_lock(
				"sync-me",
				lock_entry(),
				Some(&project),
			)
			.unwrap();
			// Capture FIRST, then let "another process" change one coordinate.
			let expected = captured("sync-me", &project);
			let mut changed = lock_entry();
			mutate(&mut changed);
			skill::add_skill_to_local_lock("sync-me", changed, Some(&project))
				.unwrap();

			let source = tmp.path().join("src/sync-me");
			write_skill(&source, "sync-me", "stale fetch");
			let err = resync_installed_skill(ResyncRequest {
				source_dir: &source,
				name: "sync-me",
				scope: ResourceScope::ProjectOnly,
				project_root: Some(&project),
				ref_commit: Some("newoid"),
				expected,
			})
			.unwrap_err();

			assert!(
				matches!(err, ResyncError::StaleFetch(_)),
				"a changed {label} must be refused, got {err:?}"
			);
			assert!(
				std::fs::read_to_string(installed.join("SKILL.md"))
					.unwrap()
					.contains("content to protect"),
				"installed content must survive a changed {label}"
			);
		}
	}

	/// The data-safety heart: a lock failure AFTER the destructive swap must
	/// restore every replaced target.
	///
	/// The failure used to be injected by removing the tracked lock entry, which
	/// compare-after-fetch now refuses BEFORE any swap (a better outcome, but it
	/// would leave this rollback path uncovered). So the entry stays intact and
	/// matching, and the lock WRITE is what fails: the project root is made
	/// read-only, so the atomic temp+rename cannot create its temp file there,
	/// while `.agents/skills` underneath stays writable so the swap still happens.
	#[cfg(unix)]
	#[test]
	fn lock_failure_rolls_back_master_and_existing_referrer() {
		use std::os::unix::fs::{symlink, PermissionsExt};
		// Root ignores the read-only bit, so there would be no failure to observe.
		if unsafe { libc::geteuid() } == 0 {
			return;
		}

		let tmp = tempfile::tempdir().unwrap();
		let project = tmp.path().join("project");
		let master = project.join(".aghub/sync-me");
		let referrers = [
			project.join(".claude/skills/sync-me"),
			project.join(".cursor/skills/sync-me"),
		];
		write_skill(&master, "sync-me", "old");
		for referrer in &referrers {
			std::fs::create_dir_all(referrer.parent().unwrap()).unwrap();
			symlink(&master, referrer).unwrap();
		}

		// The tracked entry is present and matches what was "fetched", so the
		// transaction runs all the way to the lock write.
		skill::add_skill_to_local_lock("sync-me", lock_entry(), Some(&project))
			.unwrap();
		// Pre-create the mutation lock file while the root is still writable, so
		// the guard can open it after the freeze (it is the WRITE that must fail).
		std::fs::write(project.join(".agents/.aghub-mutation.lock"), b"")
			.unwrap();
		let lock_before = skill::lock::local::read_local_lock(Some(&project));

		let source = tmp.path().join("src/sync-me");
		write_skill(&source, "sync-me", "new");

		let original = std::fs::metadata(&project).unwrap().permissions();
		std::fs::set_permissions(
			&project,
			std::fs::Permissions::from_mode(0o500),
		)
		.unwrap();
		let err = resync_installed_skill(ResyncRequest {
			source_dir: &source,
			name: "sync-me",
			scope: ResourceScope::ProjectOnly,
			project_root: Some(&project),
			ref_commit: Some("newoid"),
			expected: captured("sync-me", &project),
		})
		.unwrap_err();
		// Restore before asserting, so a failed assert cannot leak a read-only dir.
		std::fs::set_permissions(&project, original).unwrap();

		assert!(
			matches!(err, ResyncError::LockUpdate(_)),
			"expected a post-swap lock write failure, got {err:?}"
		);
		assert!(
			std::fs::read_to_string(master.join("SKILL.md"))
				.unwrap()
				.contains("old"),
			"Master content must be restored after the post-swap lock failure",
		);
		for referrer in &referrers {
			assert!(
				std::fs::read_to_string(referrer.join("SKILL.md"))
					.unwrap()
					.contains("old"),
				"every existing Referrer must resolve to the restored Master",
			);
			assert!(
				std::fs::symlink_metadata(referrer)
					.unwrap()
					.file_type()
					.is_symlink(),
				"rollback must not replace an existing Referrer",
			);
		}
		let lock_after = skill::lock::local::read_local_lock(Some(&project));
		assert_eq!(
			lock_after.skills, lock_before.skills,
			"a failed lock write must leave every entry unchanged"
		);
		assert_eq!(
			lock_after.skills["sync-me"].computed_hash, "old",
			"the un-advanced hash is what makes a later `check` see the truth"
		);
	}

	// A swap failure must NOT advance the lock — else a later `check` would read
	// the un-swapped target as up-to-date. Forced via a read-only install parent
	// (skipped under root, which ignores file permissions).
	#[cfg(unix)]
	#[test]
	fn swap_failure_leaves_lock_unchanged() {
		use std::os::unix::fs::PermissionsExt;
		if unsafe { libc::geteuid() } == 0 {
			return;
		}
		let tmp = tempfile::tempdir().unwrap();
		let project = tmp.path().join("project");
		let skills_dir = project.join(".claude/skills");
		let installed = skills_dir.join("locked");
		write_skill(&installed, "locked", "old");
		skill::add_skill_to_local_lock("locked", lock_entry(), Some(&project))
			.unwrap();

		let source = tmp.path().join("src/locked");
		write_skill(&source, "locked", "new");

		// Read+exec but no write: discovery still lists the skill, but the swap
		// cannot create its staging dir under the parent.
		let ro = std::fs::Permissions::from_mode(0o500);
		std::fs::set_permissions(&skills_dir, ro).unwrap();

		let err = resync_installed_skill(ResyncRequest {
			source_dir: &source,
			name: "locked",
			scope: ResourceScope::ProjectOnly,
			project_root: Some(&project),
			ref_commit: Some("newoid"),
			expected: captured("locked", &project),
		})
		.unwrap_err();

		std::fs::set_permissions(
			&skills_dir,
			std::fs::Permissions::from_mode(0o700),
		)
		.unwrap();

		assert!(matches!(err, ResyncError::Swap(_)));
		let lock = skill::lock::local::read_local_lock(Some(&project));
		let entry = &lock.skills["locked"];
		assert_eq!(entry.computed_hash, "old", "lock hash must not advance");
		assert_eq!(entry.ref_commit, None, "ref_commit must not advance");
	}
}
