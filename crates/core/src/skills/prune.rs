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
//! - The lock write is atomic (temp + rename; see `skill::lock`), and the SCAN
//!   plus the rewrite are held under the interprocess mutation lock, so a skill
//!   another aghub process installs in between can no longer be pruned by a disk
//!   set that predates it.
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
	/// The interprocess mutation lock could not be taken; nothing was scanned
	/// and NOTHING was written. Distinct from [`PruneError::Io`] precisely so the
	/// message cannot claim a write that never happened.
	#[error("{0}")]
	Locked(String),
	/// A project-scope prune was requested without a project root.
	#[error("project prune requires a project root")]
	MissingProjectRoot,
	/// The lock itself could not be read, so nothing about it can be reported.
	///
	/// Distinct from [`PruneError::Scan`]: reusing that made the message blame
	/// the disk scan and claim a permissions problem for a file that is merely
	/// unparseable — the exact kind of lie this module keeps having to remove.
	#[error(
		"the skill lock could not be read, so no prune can be planned: \
		 {0}; resolve it (unresolved merge conflict?) and retry"
	)]
	UnreadableLock(String),
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
		match dir.try_exists() {
			// Genuinely absent: a missing agent dir simply holds no skills.
			Ok(false) => continue,
			Ok(true) => {}
			// Inaccessible (EACCES on a parent, ENOTDIR, a dropped network
			// mount, …): we CANNOT prove the dir holds no skills, so abort the
			// whole scan rather than treat it as empty. `Path::exists()` would
			// collapse this into `false` and let a confirmed prune wipe the lock
			// for skills that merely live in a currently-unreadable location.
			Err(_) => return Err(ScanError::PermissionDenied(dir.clone())),
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
	// Hold the interprocess mutation lock across scan AND rewrite (window 3 in
	// the module docs): otherwise a skill another process installs in between is
	// pruned from the lock by a disk set that predates it.
	let _mutation_guard = crate::skills::lock::mutation_guard(
		"prune lock",
		match scope {
			PruneScope::Global => ResourceScope::GlobalOnly,
			PruneScope::Project => ResourceScope::ProjectOnly,
		},
		project_root,
	)
	.map_err(|e| PruneError::Locked(e.to_string()))?;
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

/// Map a single prune result into a [`PruneStatus`]: success → `Pruned(keys)`,
/// error → `Failed` with an empty `pruned` (a single-scope prune leaves its
/// lock unchanged on error).
fn prune_status(
	result: Result<Vec<String>, PruneError>,
) -> crate::skills::removal::PruneStatus {
	use crate::skills::removal::PruneStatus;
	match result {
		Ok(keys) => PruneStatus::Pruned(keys),
		Err(e) => PruneStatus::Failed {
			reason: e.to_string(),
			pruned: Vec::new(),
		},
	}
}

/// Fold the global prune result with a LAZY project prune (the `Both` scope)
/// into one [`PruneStatus`]. The project prune is a closure that runs ONLY when
/// the global prune succeeded — a global failure short-circuits before the
/// project lock is touched at all (no side effect, `pruned` empty), so a failed
/// global prune can never silently mutate the project lock. The two locks are
/// independent and pruned in sequence, so a project failure AFTER a global
/// success is a partial mutation — reported as
/// `Failed { reason, pruned: <global keys already dropped> }`, never as a
/// `Pruned` (the project lock errored) nor as a bare `Failed` that falsely
/// claims nothing changed.
fn combine_prune(
	global: Result<Vec<String>, PruneError>,
	project: Option<impl FnOnce() -> Result<Vec<String>, PruneError>>,
) -> crate::skills::removal::PruneStatus {
	use crate::skills::removal::PruneStatus;
	let mut keys = match global {
		Ok(k) => k,
		Err(e) => {
			// Short-circuit: do NOT run the project prune, leaving its lock
			// untouched.
			return PruneStatus::Failed {
				reason: e.to_string(),
				pruned: Vec::new(),
			};
		}
	};
	if let Some(project) = project {
		match project() {
			Ok(k) => keys.extend(k),
			Err(e) => {
				// Global already pruned `keys`; do not lose that fact.
				return PruneStatus::Failed {
					reason: e.to_string(),
					pruned: keys,
				};
			}
		}
	}
	PruneStatus::Pruned(keys)
}

/// Reconcile the per-scope lock(s) against disk after a removal and report the
/// outcome as a [`PruneStatus`]. This is the single core-owned seam every
/// delete path routes through (the manager's `remove_skill_planned` AND the
/// API by-path copy branch) so the prune logic lives in exactly one place.
///
/// Scope mapping: `GlobalOnly` → Global lock; `ProjectOnly` → Project lock
/// (`NotRun` without a root, since there is no project lock to reconcile);
/// `Both` → global then a LAZY project prune ([`combine_prune`]), so a global
/// failure never mutates the project lock. Non-fatal: a single-scope failure
/// leaves that lock unchanged (`Failed { pruned: [] }`); a `Both` failure after
/// the global succeeded records the partial mutation in `Failed.pruned`.
pub fn prune_lock_for_scope(
	scope: ResourceScope,
	project_root: Option<&Path>,
) -> crate::skills::removal::PruneStatus {
	use crate::skills::removal::PruneStatus;
	match scope {
		ResourceScope::GlobalOnly => {
			prune_status(prune_lock_scanning(PruneScope::Global, None))
		}
		ResourceScope::ProjectOnly => match project_root {
			Some(r) => {
				prune_status(prune_lock_scanning(PruneScope::Project, Some(r)))
			}
			None => PruneStatus::NotRun,
		},
		ResourceScope::Both => {
			let global = prune_lock_scanning(PruneScope::Global, None);
			// Lazy: the project prune runs ONLY if `global` succeeded, so a
			// global failure never mutates the project lock.
			let project = project_root.map(|r| {
				move || prune_lock_scanning(PruneScope::Project, Some(r))
			});
			combine_prune(global, project)
		}
	}
}

/// What the post-delete lock prune WOULD drop, for a removal that has not run
/// yet.
///
/// A committed `delete` reconciles the WHOLE scope's lock against disk, so it
/// also drops entries for OTHER skills that are already gone. The preview did
/// not disclose that: `pruned_lock_entries` appeared only on the committed
/// payload, so the caller could not see which other skills' provenance the
/// commit was about to discard — while `prune-lock`, which performs the SAME
/// GC, gates it behind its own `--yes`.
///
/// `removing` is the paths this delete will take. They must be EXCLUDED from
/// the disk scan: the preview runs BEFORE the deletion, so those folders are
/// still present, and a plain `preview_prune` would omit exactly the key the
/// commit is most certain to drop — the target's own.
///
/// Read-only. `preview_prune_from_dirs` deliberately takes no mutation guard
/// (unlike `prune_lock_from_dirs`), and `locked_keys` only reads, so this is
/// safe on the dry-run path, which holds no guard.
///
/// Returns [`PruneStatus::NotRun`] rather than an error when anything is
/// unprovable — an unreadable dir, an unreadable lock, a project scope with no
/// root. A preview that cannot see the whole picture must promise NOTHING; the
/// alternative is claiming "no other entries will be dropped" on the strength
/// of a scan that failed. That also keeps preview and commit from diverging on
/// a corrupt lock: the commit path reads through the fail-CLOSED modify seam
/// and reports `Failed`, whereas these readers fail OPEN to an empty lock and
/// would otherwise quietly answer "nothing".
pub fn preview_prune_for_removal(
	scope: ResourceScope,
	project_root: Option<&Path>,
	removing: &[PathBuf],
) -> crate::skills::removal::PruneStatus {
	use crate::skills::removal::PruneStatus;

	// Excluded by PATH, not by folder name: a single-agent delete removes only
	// some referrers and KEEPS the shared Master, so the skill is still on disk
	// through that Master and its key must NOT be listed.
	//
	// ponytail: path equality. If `canonical_path` was canonicalized while the
	// descriptor dirs were not (macOS /private/var, a symlinked HOME), a path
	// fails to match and its key is simply omitted — i.e. today's behaviour,
	// never a fabricated key. Upgrade path: compare canonicalized pairs.
	let excluded: Vec<PathBuf> = removing.to_vec();
	let scan = move |dir: &Path| -> Result<Vec<PathBuf>, ScanError> {
		let dirs = top_level_skill_dirs(dir)?;
		Ok(dirs
			.into_iter()
			.filter(|found| !excluded.iter().any(|gone| gone == found))
			.collect())
	};

	// Mirrors `prune_lock_for_scope`'s mapping exactly — including `None` as
	// the root for the Global half of `Both`. A different mapping here would
	// scan different dirs from the commit it is previewing.
	let preview_one = |prune_scope: PruneScope, root: Option<&Path>| {
		// Fail-CLOSED on the LOCK as well as on the scan: an unreadable lock
		// must degrade the whole preview to `NotRun`, not to an empty list that
		// promises nothing else will be dropped.
		let keys = locked_keys_checked(prune_scope, root)?;
		let dirs = scope_skill_dirs(prune_scope, root);
		let disk = collect_disk_dir_names(&dirs, &scan).ok()?;
		Some(
			keys.into_iter()
				.filter(|k| !skill::skill_present_on_disk(k, &disk))
				.collect::<Vec<String>>(),
		)
	};

	let keys = match scope {
		ResourceScope::GlobalOnly => preview_one(PruneScope::Global, None),
		ResourceScope::ProjectOnly => match project_root {
			Some(root) => preview_one(PruneScope::Project, Some(root)),
			None => None,
		},
		ResourceScope::Both => {
			let global = preview_one(PruneScope::Global, None);
			match (global, project_root) {
				(Some(mut g), Some(root)) => {
					match preview_one(PruneScope::Project, Some(root)) {
						Some(p) => {
							g.extend(p);
							Some(g)
						}
						// Half a picture is not a picture.
						None => None,
					}
				}
				(g, None) => g,
				(None, _) => None,
			}
		}
	};

	match keys {
		Some(keys) => PruneStatus::WouldPrune(keys),
		None => PruneStatus::NotRun,
	}
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
	// Fail CLOSED on the lock, like the commit this previews. `locked_keys`'s
	// fail-OPEN readers turned an unreadable lock into an empty key set, and an
	// empty result here means "the scan ran and found no orphans" — a clean
	// bill of health from a scan that saw nothing, while `--yes` on the same
	// file refuses outright. A preview whose whole job is predicting the commit
	// must not disagree with it about whether the file is readable.
	let keys = locked_keys_checked(scope, project_root).ok_or_else(|| {
		PruneError::UnreadableLock(match scope {
			PruneScope::Global => "global skill lock".to_string(),
			PruneScope::Project => "skills-lock.json".to_string(),
		})
	})?;
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
/// Lock keys for a PREVIEW, failing CLOSED.
///
/// `locked_keys` uses the fail-OPEN readers, so an unreadable lock yields an
/// empty key set — and an empty `would_prune_lock_entries` reads, by this
/// module's own convention, as "the scan ran and found no orphans": a clean
/// bill of health from a scan that saw nothing. The commit path prunes through
/// the fail-CLOSED modify seam and reports `Failed` for the same file, so the
/// two diverged exactly where the preview is supposed to predict the commit.
fn locked_keys_checked(
	scope: PruneScope,
	project_root: Option<&Path>,
) -> Option<Vec<String>> {
	match scope {
		PruneScope::Global => skill::lock::read_global_lock_checked()
			.ok()
			.map(|lock| lock.skills.keys().cloned().collect()),
		PruneScope::Project => project_root.and_then(|root| {
			skill::lock::local::read_local_lock_checked(Some(root))
				.ok()
				.map(|lock| lock.skills.keys().cloned().collect())
		}),
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
		if file_type.is_dir() && entry.path().join("SKILL.md").is_file() {
			dirs.push(entry.path());
		}
	}
	Ok(dirs)
}

/// Test-only global-lock isolation, shared across every core test mod that
/// touches the global skill lock (here + `manager::skill`). It MUST be a single
/// definition so all callers serialize on the SAME mutex — two separate `static
/// LOCK`s would race on the shared `XDG_STATE_HOME` global lock.
#[cfg(test)]
pub(crate) mod test_lock {
	use std::sync::{Mutex, MutexGuard, OnceLock};
	use tempfile::TempDir;

	/// Serializes + isolates the GLOBAL lock by pointing `XDG_STATE_HOME` at a
	/// fresh temp dir (core cannot import skill's `pub(crate)` TestLockGuard).
	pub(crate) struct GlobalLockGuard {
		_temp: TempDir,
		old: Option<String>,
		_lock: MutexGuard<'static, ()>,
	}

	impl GlobalLockGuard {
		pub(crate) fn new() -> Self {
			static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
			let guard = LOCK
				.get_or_init(|| Mutex::new(()))
				.lock()
				.unwrap_or_else(|e| e.into_inner());
			let temp = tempfile::tempdir().unwrap();
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
}

#[cfg(test)]
mod tests {
	use super::test_lock::GlobalLockGuard;
	use super::*;
	use tempfile::tempdir;

	fn global_entry() -> skill::SkillLockEntry {
		skill::SkillLockEntry {
			source: "o/r".to_string(),
			source_type: "github".to_string(),
			source_url: "https://github.com/o/r".to_string(),
			ref_name: None,
			skill_path: None,
			skill_folder_hash: "h".to_string(),
			content_hash: None,
			ref_commit: None,
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

	// combine_prune / prune_status: the per-scope fold that
	// `prune_lock_for_scope` builds on. `Both` folds two INDEPENDENT lock
	// prunes; a project failure after a global success is a partial mutation
	// and must NOT masquerade as a bare Failed (which contractually means
	// "lock unchanged") — the already-dropped global keys are reported.
	use crate::skills::removal::PruneStatus;

	#[test]
	fn combine_prune_both_success_concatenates_keys() {
		let out = combine_prune(
			Ok(vec!["g1".into()]),
			Some(|| Ok(vec!["p1".into()])),
		);
		assert_eq!(out, PruneStatus::Pruned(vec!["g1".into(), "p1".into()]));
	}

	#[test]
	fn combine_prune_project_failure_after_global_success_reports_partial() {
		// Global pruned g1 (lock already mutated); project errored. The outcome
		// must surface BOTH the error and the global keys that were dropped —
		// not a Failed that pretends the lock is untouched.
		let out = combine_prune(
			Ok(vec!["g1".into()]),
			Some(|| Err(PruneError::MissingProjectRoot)),
		);
		match out {
			PruneStatus::Failed { reason, pruned } => {
				assert!(!reason.is_empty());
				assert_eq!(
					pruned,
					vec!["g1".to_string()],
					"global keys pruned before project failure must be reported"
				);
			}
			other => {
				panic!("expected Failed with partial pruned, got {other:?}")
			}
		}
	}

	#[test]
	fn combine_prune_global_failure_short_circuits_with_empty_pruned() {
		// Global errored: the project closure must NEVER run (it would mutate
		// the project lock). Assert both the reported status AND that the
		// closure was not invoked.
		let mut project_ran = false;
		let out = combine_prune(
			Err(PruneError::MissingProjectRoot),
			Some(|| {
				project_ran = true;
				Ok(vec![])
			}),
		);
		assert!(
			!project_ran,
			"global failure must short-circuit before the project prune"
		);
		assert_eq!(
			out,
			PruneStatus::Failed {
				reason: PruneError::MissingProjectRoot.to_string(),
				pruned: Vec::new(),
			}
		);
	}

	#[test]
	fn prune_status_failure_reports_empty_pruned() {
		// Single-scope failure leaves the lock unchanged: pruned is empty.
		let out = prune_status(Err(PruneError::MissingProjectRoot));
		assert_eq!(
			out,
			PruneStatus::Failed {
				reason: PruneError::MissingProjectRoot.to_string(),
				pruned: Vec::new(),
			}
		);
	}

	#[test]
	fn prune_lock_for_scope_project_without_root_is_not_run() {
		// ProjectOnly with no root: no project lock to reconcile, so the seam
		// reports NotRun rather than attempting (and failing) a prune. (Must be
		// ProjectOnly — `Both` would prune the shared global lock here, and
		// this test holds no GlobalLockGuard. Both+None is covered below.)
		let out = prune_lock_for_scope(ResourceScope::ProjectOnly, None);
		assert_eq!(out, PruneStatus::NotRun);
	}

	#[test]
	fn prune_lock_for_scope_global_drops_orphan() {
		// End-to-end through the core seam: an orphan global lock entry (no dir
		// on disk) is dropped and surfaced as Pruned.
		let _g = GlobalLockGuard::new();
		skill::lock::add_skill_to_lock("orphan", global_entry()).unwrap();

		let out = prune_lock_for_scope(ResourceScope::GlobalOnly, None);
		match out {
			PruneStatus::Pruned(keys) => assert!(
				keys.contains(&"orphan".to_string()),
				"orphan must be dropped, got {keys:?}"
			),
			other => panic!("expected Pruned, got {other:?}"),
		}
		assert!(!skill::read_skill_lock().skills.contains_key("orphan"));
	}

	#[test]
	fn prune_lock_for_scope_both_without_root_prunes_global_only() {
		// Both scope with NO project root: the global lock IS reconciled, but
		// the project branch is skipped entirely (no root to locate a project
		// lock). Result is a plain Pruned of the global keys — no NotRun, no
		// panic, no attempt to touch a project lock.
		let _g = GlobalLockGuard::new();
		skill::lock::add_skill_to_lock("orphan-global", global_entry())
			.unwrap();

		let out = prune_lock_for_scope(ResourceScope::Both, None);
		assert_eq!(out, PruneStatus::Pruned(vec!["orphan-global".to_string()]));
		assert!(!skill::read_skill_lock()
			.skills
			.contains_key("orphan-global"));
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
				source_url: None,
				ref_commit: None,
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
				source_url: None,
				ref_commit: None,
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
	fn collect_disk_dir_names_skips_dirs_without_skill_md() {
		let root = tempdir().unwrap();
		let skills = root.path().join("skills");
		write_skill_md(&skills.join("alpha"), "alpha");
		std::fs::create_dir_all(skills.join("not-a-skill")).unwrap();
		std::fs::write(skills.join("not-a-skill/README.md"), "x").unwrap();

		let got = collect_disk_dir_names(
			std::slice::from_ref(&skills),
			top_level_skill_dirs,
		)
		.unwrap();

		assert!(got.contains("alpha"));
		assert!(!got.contains("not-a-skill"));
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

	// Unix-only: the deterministic "inaccessible dir" proxy here is a path whose
	// ancestor is a regular file, which yields ENOTDIR (-> try_exists Err) on
	// Unix. Windows maps `<file>\sub` to Ok(false) (genuinely-absent), so the
	// abort cannot be triggered this way there; the real Windows inaccessibility
	// case (ACL/EACCES) needs privileged setup. The PRODUCTION try_exists() guard
	// still applies cross-platform — only this test's simulation is Unix-specific.
	#[cfg(unix)]
	#[test]
	fn collect_disk_dir_names_aborts_on_inaccessible_dir() {
		// A path whose ancestor is a regular file is INACCESSIBLE (ENOTDIR), as
		// opposed to genuinely absent. It must abort the scan (so the caller
		// never prunes), not be silently treated as "this dir holds no skills".
		// `Path::exists()` collapses both to `false`; `try_exists()` distinguishes
		// them. (ENOTDIR stands in deterministically for the real-world EACCES of
		// an unreadable parent / dropped network mount, with no chmod needed.)
		let root = tempdir().unwrap();
		let file = root.path().join("not-a-dir");
		std::fs::write(&file, "x").unwrap();
		let inaccessible = file.join("subdir");

		let res = collect_disk_dir_names(
			std::slice::from_ref(&inaccessible),
			top_level_skill_dirs,
		);

		assert!(
			matches!(res, Err(ScanError::PermissionDenied(_))),
			"inaccessible dir must abort the scan, got {res:?}"
		);
	}

	// Unix-only for the same reason as the previous test (ENOTDIR proxy).
	#[cfg(unix)]
	#[test]
	fn prune_does_not_wipe_lock_when_a_scope_dir_is_inaccessible() {
		let _g = GlobalLockGuard::new();
		skill::lock::add_skill_to_lock("keep", global_entry()).unwrap();
		let before = std::fs::read(skill::get_skill_lock_path()).unwrap();

		// Every configured scope dir is inaccessible (ENOTDIR via a file ancestor).
		let root = tempdir().unwrap();
		let file = root.path().join("not-a-dir");
		std::fs::write(&file, "x").unwrap();
		let inaccessible = file.join("subdir");

		let res = prune_lock_from_dirs(
			PruneScope::Global,
			std::slice::from_ref(&inaccessible),
			None,
			top_level_skill_dirs,
		);

		assert!(matches!(res, Err(PruneError::Scan(_))));
		let after = std::fs::read(skill::get_skill_lock_path()).unwrap();
		assert_eq!(
			before, after,
			"an inaccessible scan must not mutate the lock"
		);
		assert!(skill::read_skill_lock().skills.contains_key("keep"));
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
