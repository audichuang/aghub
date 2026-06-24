//! Re-sync an already-installed skill from a freshly-fetched source folder: the
//! single deep transaction shared by the CLI `apply-update` / `source sync` paths
//! and the API apply-update / git-sync routes.
//!
//! Callers own everything I/O-shaped up to a resolved on-disk source dir (fetch,
//! session, credential, sanitize, request validation); this owns the transaction:
//! discover installed targets → rename guard → hash → strict containment → swap
//! every target → lock re-stamp. Any swap failure aborts before the lock advances
//! (a partial swap with an advanced lock would let a later `check` read a stale
//! target as up-to-date), so the lock moves only once every target took the new
//! content.

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
	/// One or more installed targets failed to swap; the lock was left unchanged.
	Swap(String),
	/// The lock could not be re-stamped after the swap.
	LockUpdate(String),
}

impl std::fmt::Display for ResyncError {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
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

/// Replace every installed copy of `name` with the content at `source_dir` and
/// re-stamp the lock. See the module docs for the seam and the ordering.
pub fn resync_installed_skill(
	req: ResyncRequest,
) -> Result<ResyncReport, ResyncError> {
	let targets = crate::skills::removal::installed_skill_roots(
		req.name,
		req.scope,
		req.project_root,
	);
	if targets.is_empty() {
		return Err(ResyncError::NotInstalled);
	}

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

	// Swap every installed target. ANY failure aborts before the lock advances: a
	// partial swap with an advanced lock would let a later `check` read a failed
	// target as up-to-date (differing per-agent hashes are dropped as ambiguous,
	// leaving the lock as the sole baseline). Universal installs resolve to one
	// canonical Master target (N=1); N>1 only for isolated-copy installs.
	let mut swapped = Vec::new();
	let mut failures = Vec::new();
	for target in &targets {
		match crate::skills::update::stage_and_swap_dir(req.source_dir, target)
		{
			Ok(()) => swapped.push(target.clone()),
			Err(e) => failures.push(format!("{}: {e}", target.display())),
		}
	}
	if !failures.is_empty() {
		return Err(ResyncError::Swap(format!(
			"{} of {} target(s) failed: {}",
			failures.len(),
			targets.len(),
			failures.join("; ")
		)));
	}

	crate::skills::lock::update_lock_hash(
		req.name,
		req.scope,
		req.project_root,
		&updated_hash,
		req.ref_commit,
	)
	.map_err(ResyncError::LockUpdate)?;

	Ok(ResyncReport {
		swapped,
		updated_hash,
	})
}

#[cfg(test)]
mod tests {
	use super::*;

	fn write_skill(dir: &Path, name: &str, desc: &str) {
		std::fs::create_dir_all(dir).unwrap();
		std::fs::write(
			dir.join("SKILL.md"),
			format!("---\nname: {name}\ndescription: {desc}\n---\n\n{desc}\n"),
		)
		.unwrap();
	}

	fn lock_entry() -> skill::LocalSkillLockEntry {
		skill::LocalSkillLockEntry {
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

		let err = resync_installed_skill(ResyncRequest {
			source_dir: &source,
			name: "ghost",
			scope: ResourceScope::ProjectOnly,
			project_root: Some(&tmp.path().join("empty")),
			ref_commit: None,
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
		})
		.unwrap_err();
		assert!(matches!(err, ResyncError::Renamed { .. }));
		assert!(std::fs::read_to_string(installed.join("SKILL.md"))
			.unwrap()
			.contains("old"));
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
