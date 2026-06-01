//! Disk-reconciled lock pruning.
//!
//! After a removal (or on demand), re-scan a scope's disk: if NO agent in that
//! scope still has a skill on disk, its lock entry is pruned. Guarded so a
//! VCS-committed lock is never corrupted:
//!
//! - Pruning runs ONLY on a provably successful scan (error-returning
//!   [`skill::scan_skills`], never the error-swallowing discovery collector).
//!   Any [`ScanError`] aborts ALL pruning — the lock is left untouched.
//! - Per-scope disk sets are disjoint: a global prune scans the union of every
//!   agent's *global* skill dirs; a project prune scans ONLY the project's skill
//!   dirs. A project prune never touches the global lock and vice versa.
//! - A project prune requires a project root.
//! - The lock write is atomic (temp + rename under a process mutex; see
//!   `skill::lock`).
//!
//! [`prune_lock`] is pure given a pre-scanned name set; [`prune_lock_from_dirs`]
//! adds the scan (with an injectable scanner for deterministic tests), and
//! [`prune_lock_scanning`] is the production entry point that derives the
//! per-scope dirs from the agent descriptors.

use crate::models::ResourceScope;
use skill::ScanError;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Which lock + disk set a prune operates on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PruneScope {
	/// Global lock (`.skill-lock.json`) reconciled against all agents' global dirs.
	Global,
	/// Project lock (`skills-lock.json`) reconciled against the project's dirs only.
	Project,
}

/// Why a prune did not run (the lock is always left unchanged on error).
#[derive(Debug, thiserror::Error)]
pub enum PruneError {
	/// A disk scan failed; no pruning was performed.
	#[error("disk scan failed, no pruning performed: {0}")]
	Scan(#[from] ScanError),
	/// Writing the lock failed.
	#[error("failed to write lock: {0}")]
	Io(#[from] std::io::Error),
	/// A project-scope prune was requested without a project root.
	#[error("project prune requires a project root")]
	MissingProjectRoot,
}

/// Pure prune: drop lock entries whose sanitized name is absent from
/// `disk_names`. Returns the pruned keys. `disk_names` MUST come from a provably
/// successful scan (see [`prune_lock_from_dirs`]).
pub fn prune_lock(
	scope: PruneScope,
	disk_names: &BTreeSet<String>,
	project_root: Option<&Path>,
) -> Result<Vec<String>, PruneError> {
	match scope {
		PruneScope::Global => Ok(skill::retain_locked_skills(disk_names)?),
		PruneScope::Project => {
			let root = project_root.ok_or(PruneError::MissingProjectRoot)?;
			Ok(skill::retain_local_locked_skills(disk_names, Some(root))?)
		}
	}
}

/// Gather the set of skill *folder* names present on disk under `dirs`, using
/// the supplied scanner. Non-existent dirs contribute nothing (not an error);
/// any scanner error aborts the whole collection so the caller never prunes
/// against a partial view.
pub fn collect_disk_dir_names<F>(
	dirs: &[PathBuf],
	scan: F,
) -> Result<BTreeSet<String>, ScanError>
where
	F: Fn(&Path) -> Result<Vec<PathBuf>, ScanError>,
{
	let mut names = BTreeSet::new();
	for dir in dirs {
		if !dir.exists() {
			continue; // a missing agent dir simply holds no skills
		}
		for path in scan(dir)? {
			if let Some(name) = path.file_name().and_then(|s| s.to_str()) {
				names.insert(name.to_string());
			}
		}
	}
	Ok(names)
}

/// Scan `dirs` with `scan`, then prune. Any scan error aborts before the lock is
/// touched. A `Project` scope requires `project_root`.
pub fn prune_lock_from_dirs<F>(
	scope: PruneScope,
	dirs: &[PathBuf],
	project_root: Option<&Path>,
	scan: F,
) -> Result<Vec<String>, PruneError>
where
	F: Fn(&Path) -> Result<Vec<PathBuf>, ScanError>,
{
	if scope == PruneScope::Project && project_root.is_none() {
		return Err(PruneError::MissingProjectRoot);
	}
	let disk = collect_disk_dir_names(dirs, scan)?;
	prune_lock(scope, &disk, project_root)
}

/// Production entry point: derive the per-scope skill dirs from the agent
/// descriptors and prune against them with the real scanner.
pub fn prune_lock_scanning(
	scope: PruneScope,
	project_root: Option<&Path>,
) -> Result<Vec<String>, PruneError> {
	if scope == PruneScope::Project && project_root.is_none() {
		return Err(PruneError::MissingProjectRoot);
	}
	let dirs = scope_skill_dirs(scope, project_root);
	prune_lock_from_dirs(scope, &dirs, project_root, top_level_skill_dirs)
}

/// Dry-run: report which lock entries WOULD be pruned, without mutating the lock.
/// Uses an injectable scanner for deterministic tests.
pub fn preview_prune_from_dirs<F>(
	scope: PruneScope,
	dirs: &[PathBuf],
	project_root: Option<&Path>,
	scan: F,
) -> Result<Vec<String>, PruneError>
where
	F: Fn(&Path) -> Result<Vec<PathBuf>, ScanError>,
{
	if scope == PruneScope::Project && project_root.is_none() {
		return Err(PruneError::MissingProjectRoot);
	}
	let disk = collect_disk_dir_names(dirs, scan)?;
	let keys = locked_keys(scope, project_root);
	Ok(keys
		.into_iter()
		.filter(|k| !skill::skill_present_on_disk(k, &disk))
		.collect())
}

/// Dry-run preview against the real per-scope dirs + scanner.
pub fn preview_prune(
	scope: PruneScope,
	project_root: Option<&Path>,
) -> Result<Vec<String>, PruneError> {
	if scope == PruneScope::Project && project_root.is_none() {
		return Err(PruneError::MissingProjectRoot);
	}
	let dirs = scope_skill_dirs(scope, project_root);
	preview_prune_from_dirs(scope, &dirs, project_root, top_level_skill_dirs)
}

/// Current lock entry keys for a scope (no mutation).
fn locked_keys(scope: PruneScope, project_root: Option<&Path>) -> Vec<String> {
	match scope {
		PruneScope::Global => {
			skill::get_all_locked_skills().keys().cloned().collect()
		}
		PruneScope::Project => project_root
			.map(|r| {
				skill::read_local_lock(Some(r))
					.skills
					.keys()
					.cloned()
					.collect()
			})
			.unwrap_or_default(),
	}
}

/// The union of every agent's skill read dirs for `scope`.
fn scope_skill_dirs(
	scope: PruneScope,
	project_root: Option<&Path>,
) -> Vec<PathBuf> {
	let resource_scope = match scope {
		PruneScope::Global => ResourceScope::GlobalOnly,
		PruneScope::Project => ResourceScope::ProjectOnly,
	};
	super::removal::agent_skill_dirs_in_scope(resource_scope, project_root)
}

fn top_level_skill_dirs(dir: &Path) -> Result<Vec<PathBuf>, ScanError> {
	if !dir.exists() {
		return Err(ScanError::PathNotFound(dir.to_path_buf()));
	}
	let mut dirs = Vec::new();
	let entries = std::fs::read_dir(dir)
		.map_err(|_| ScanError::PermissionDenied(dir.to_path_buf()))?;
	for entry in entries {
		let entry = entry
			.map_err(|_| ScanError::PermissionDenied(dir.to_path_buf()))?;
		let file_type = entry
			.file_type()
			.map_err(|_| ScanError::PermissionDenied(entry.path()))?;
		if file_type.is_dir() {
			dirs.push(entry.path());
		}
	}
	Ok(dirs)
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::sync::{Mutex, MutexGuard, OnceLock};
	use tempfile::{tempdir, TempDir};

	/// Serializes + isolates the GLOBAL lock by pointing `XDG_STATE_HOME` at a
	/// fresh temp dir (core cannot import skill's `pub(crate)` TestLockGuard).
	struct GlobalLockGuard {
		_temp: TempDir,
		old: Option<String>,
		_lock: MutexGuard<'static, ()>,
	}

	impl GlobalLockGuard {
		fn new() -> Self {
			static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
			let guard = LOCK
				.get_or_init(|| Mutex::new(()))
				.lock()
				.unwrap_or_else(|e| e.into_inner());
			let temp = tempdir().unwrap();
			let old = std::env::var("XDG_STATE_HOME").ok();
			std::env::set_var("XDG_STATE_HOME", temp.path());
			Self {
				_temp: temp,
				old,
				_lock: guard,
			}
		}
	}

	impl Drop for GlobalLockGuard {
		fn drop(&mut self) {
			match &self.old {
				Some(v) => std::env::set_var("XDG_STATE_HOME", v),
				None => std::env::remove_var("XDG_STATE_HOME"),
			}
		}
	}

	fn global_entry() -> skill::SkillLockEntry {
		skill::SkillLockEntry {
			source: "o/r".to_string(),
			source_type: "github".to_string(),
			source_url: "https://github.com/o/r".to_string(),
			ref_name: None,
			skill_path: None,
			skill_folder_hash: "h".to_string(),
			content_hash: None,
			installed_at: "t".to_string(),
			updated_at: "t".to_string(),
			plugin_name: None,
		}
	}

	fn names(items: &[&str]) -> BTreeSet<String> {
		items.iter().map(|s| s.to_string()).collect()
	}

	fn write_skill_md(dir: &Path, name: &str) {
		std::fs::create_dir_all(dir).unwrap();
		std::fs::write(
			dir.join("SKILL.md"),
			format!("---\nname: {name}\ndescription: d\n---\n"),
		)
		.unwrap();
	}

	#[test]
	fn prune_lock_global_drops_orphan_keeps_present() {
		let _g = GlobalLockGuard::new();
		skill::lock::add_skill_to_lock("keep", global_entry()).unwrap();
		skill::lock::add_skill_to_lock("gone", global_entry()).unwrap();

		let pruned =
			prune_lock(PruneScope::Global, &names(&["keep"]), None).unwrap();

		assert_eq!(pruned, vec!["gone".to_string()]);
		let lock = skill::read_skill_lock();
		assert!(lock.skills.contains_key("keep"));
		assert!(!lock.skills.contains_key("gone"));
	}

	#[test]
	fn prune_lock_lock_independent_orphan_on_disk_keeps_entry() {
		let _g = GlobalLockGuard::new();
		skill::lock::add_skill_to_lock("x", global_entry()).unwrap();
		// disk has "x" (present) → kept even though scan never consulted the lock.
		let pruned =
			prune_lock(PruneScope::Global, &names(&["x"]), None).unwrap();
		assert!(pruned.is_empty());
		assert!(skill::read_skill_lock().skills.contains_key("x"));
	}

	#[test]
	fn prune_lock_global_keeps_npx_unicode_sanitized_folder() {
		let _g = GlobalLockGuard::new();
		skill::lock::add_skill_to_lock("İstanbul", global_entry()).unwrap();

		let pruned =
			prune_lock(PruneScope::Global, &names(&["i-stanbul"]), None)
				.unwrap();

		assert!(pruned.is_empty());
		assert!(skill::read_skill_lock().skills.contains_key("İstanbul"));
	}

	#[test]
	fn prune_lock_project_requires_project_root() {
		let _g = GlobalLockGuard::new();
		let err = prune_lock(PruneScope::Project, &names(&[]), None);
		assert!(matches!(err, Err(PruneError::MissingProjectRoot)));
	}

	#[test]
	fn prune_lock_global_never_touches_project_lock() {
		let _g = GlobalLockGuard::new();
		let project = tempdir().unwrap();
		skill::add_skill_to_local_lock(
			"proj-only",
			skill::LocalSkillLockEntry {
				source: "o/r".to_string(),
				ref_name: None,
				source_type: "github".to_string(),
				computed_hash: "h".to_string(),
				skill_path: None,
			},
			Some(project.path()),
		)
		.unwrap();
		// global prune with an empty disk set must not touch the project lock.
		prune_lock(PruneScope::Global, &names(&[]), None).unwrap();
		let local = skill::read_local_lock(Some(project.path()));
		assert!(local.skills.contains_key("proj-only"));
	}

	#[test]
	fn prune_lock_project_prunes_local_lock_only() {
		let _g = GlobalLockGuard::new();
		let project = tempdir().unwrap();
		skill::lock::add_skill_to_lock("global-keep", global_entry()).unwrap();
		skill::add_skill_to_local_lock(
			"proj-gone",
			skill::LocalSkillLockEntry {
				source: "o/r".to_string(),
				ref_name: None,
				source_type: "github".to_string(),
				computed_hash: "h".to_string(),
				skill_path: None,
			},
			Some(project.path()),
		)
		.unwrap();

		let pruned =
			prune_lock(PruneScope::Project, &names(&[]), Some(project.path()))
				.unwrap();

		assert_eq!(pruned, vec!["proj-gone".to_string()]);
		assert!(skill::read_local_lock(Some(project.path()))
			.skills
			.is_empty());
		// global lock untouched
		assert!(skill::read_skill_lock().skills.contains_key("global-keep"));
	}

	#[test]
	fn collect_disk_dir_names_returns_folder_basenames() {
		let root = tempdir().unwrap();
		let skills = root.path().join("skills");
		write_skill_md(&skills.join("alpha"), "alpha");
		write_skill_md(&skills.join("beta"), "beta");

		let got = collect_disk_dir_names(
			std::slice::from_ref(&skills),
			top_level_skill_dirs,
		)
		.unwrap();

		assert!(got.contains("alpha"));
		assert!(got.contains("beta"));
	}

	#[test]
	fn prune_disk_set_excludes_bundled_nested_subskill() {
		let _g = GlobalLockGuard::new();
		skill::lock::add_skill_to_lock("foo", global_entry()).unwrap();
		skill::lock::add_skill_to_lock("bundled", global_entry()).unwrap();
		let root = tempdir().unwrap();
		let skills = root.path().join("skills");
		write_skill_md(&skills.join("foo"), "foo");
		write_skill_md(&skills.join("foo/bundled"), "bundled");

		let pruned = prune_lock_from_dirs(
			PruneScope::Global,
			std::slice::from_ref(&skills),
			None,
			top_level_skill_dirs,
		)
		.unwrap();

		assert_eq!(pruned, vec!["bundled".to_string()]);
		let lock = skill::read_skill_lock();
		assert!(lock.skills.contains_key("foo"));
		assert!(!lock.skills.contains_key("bundled"));
	}

	#[test]
	fn collect_disk_dir_names_skips_nonexistent_dirs() {
		let root = tempdir().unwrap();
		let missing = root.path().join("nope");
		let got =
			collect_disk_dir_names(&[missing], top_level_skill_dirs).unwrap();
		assert!(got.is_empty());
	}

	#[test]
	fn prune_lock_from_dirs_aborts_on_scan_error_lock_unchanged() {
		let _g = GlobalLockGuard::new();
		skill::lock::add_skill_to_lock("gone", global_entry()).unwrap();
		let before = std::fs::read(skill::get_skill_lock_path()).unwrap();

		let existing = tempdir().unwrap();
		// Injected scanner errors deterministically (no chmod, root-CI safe).
		let res = prune_lock_from_dirs(
			PruneScope::Global,
			&[existing.path().to_path_buf()],
			None,
			|d: &Path| Err(ScanError::PermissionDenied(d.to_path_buf())),
		);

		assert!(matches!(res, Err(PruneError::Scan(_))));
		let after = std::fs::read(skill::get_skill_lock_path()).unwrap();
		assert_eq!(before, after, "scan error must not mutate the lock");
		assert!(skill::read_skill_lock().skills.contains_key("gone"));
	}

	#[test]
	fn preview_prune_reports_orphans_without_mutating_lock() {
		let _g = GlobalLockGuard::new();
		skill::lock::add_skill_to_lock("present", global_entry()).unwrap();
		skill::lock::add_skill_to_lock("orphan", global_entry()).unwrap();
		let before = std::fs::read(skill::get_skill_lock_path()).unwrap();

		let dir = tempdir().unwrap();
		let would = preview_prune_from_dirs(
			PruneScope::Global,
			&[dir.path().to_path_buf()],
			None,
			|_d: &Path| Ok(vec![PathBuf::from("present")]),
		)
		.unwrap();

		assert_eq!(would, vec!["orphan".to_string()]);
		let after = std::fs::read(skill::get_skill_lock_path()).unwrap();
		assert_eq!(before, after, "preview must not mutate the lock");
	}

	#[test]
	fn prune_lock_from_dirs_prunes_when_scan_succeeds() {
		let _g = GlobalLockGuard::new();
		skill::lock::add_skill_to_lock("present", global_entry()).unwrap();
		skill::lock::add_skill_to_lock("orphan", global_entry()).unwrap();

		let dir = tempdir().unwrap();
		// scanner reports only "present" on disk.
		let pruned = prune_lock_from_dirs(
			PruneScope::Global,
			&[dir.path().to_path_buf()],
			None,
			|_d: &Path| Ok(vec![PathBuf::from("present")]),
		)
		.unwrap();

		assert_eq!(pruned, vec!["orphan".to_string()]);
		let lock = skill::read_skill_lock();
		assert!(lock.skills.contains_key("present"));
		assert!(!lock.skills.contains_key("orphan"));
	}
}
