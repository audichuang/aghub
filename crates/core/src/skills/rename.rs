//! The transactional skill-rename: install the new name, remove the old name,
//! and transition both lock entries as ONE transaction that rolls back to the
//! pre-mutation state on any failure. Extracted from the CLI `source
//! accept-rename` and the API `accept-rename` route, which mirrored this logic
//! by hand (candidate A). ADR-0001 fixes the rollback scope (rename + relink
//! only); `docs/specs/2026-07-15-skill-rename-transaction-deepening.md` records
//! the extraction.
//!
//! The git FETCH is deliberately NOT here: `skill-update` depends on this crate,
//! so this module cannot fetch. The adapter fetches (with its own auth strategy)
//! and hands us an already-fetched `repo_root` + `oid` via [`FetchedRename`];
//! everything the transaction mutates (`install_fetched`, `removal`, `linker`,
//! the lock) is core-level. That also makes the transaction testable with a
//! tempdir `repo_root` — no git, no network.

use crate::models::ResourceScope;
use crate::skills::linker::{universal_canonical_dir, LinkTarget, Linker};
use std::path::{Path, PathBuf};

/// Machine code surfaced when a rename target already exists (lock entry or
/// on-disk dir) OR the rename is degenerate (old/new sanitize to one dir).
pub const RENAME_TARGET_EXISTS_CODE: &str = "RENAME_TARGET_EXISTS";

/// Exactly one scope a rename targets. Unlike [`ResourceScope`] this makes the
/// illegal states unrepresentable: there is no `Both`, and a project rename
/// always carries its root.
#[derive(Debug, Clone)]
pub enum RenameScope {
	Global,
	Project { root: PathBuf },
}

impl RenameScope {
	fn resource_scope(&self) -> ResourceScope {
		match self {
			RenameScope::Global => ResourceScope::GlobalOnly,
			RenameScope::Project { .. } => ResourceScope::ProjectOnly,
		}
	}

	fn project_root(&self) -> Option<&Path> {
		match self {
			RenameScope::Global => None,
			RenameScope::Project { root } => Some(root),
		}
	}
}

/// Source coordinates of the OLD-name lock entry plus the fields needed to
/// re-install under the new name. The adapter reads this (via
/// [`rename_source_from_lock`]) to fetch, applies any `--ref` override to
/// `ref_name`, and hands it back inside [`FetchedRename`].
#[derive(Debug, Clone)]
pub struct RenameLockSource {
	pub source: String,
	pub source_type: String,
	pub source_url: String,
	pub ref_name: Option<String>,
	pub skill_path: String,
}

/// What the adapter learned by fetching, in types core can name.
pub struct FetchedRename<'a> {
	/// Root of the fetched source tree.
	pub repo_root: &'a Path,
	/// The fetched commit id, written to the new lock entry's `ref_commit`.
	pub oid: &'a str,
	/// The (effective) source coordinates for the new lock entry.
	pub source: &'a RenameLockSource,
}

/// The rename to perform. `old_name`/`new_name` are the skill names; `scope`
/// carries the target scope (and project root when project-scoped).
pub struct RenameRequest<'a> {
	pub old_name: &'a str,
	pub new_name: &'a str,
	pub scope: RenameScope,
}

/// The result of a committed rename.
#[derive(Debug)]
pub struct RenameSuccess {
	pub installed_hash: String,
	pub paths: Vec<String>,
}

/// Surface-agnostic rename failures. Each adapter maps these to its own channel;
/// [`RenameError::message`] / [`RenameError::code`] give the canonical wording
/// and machine code so both surfaces stay consistent.
#[derive(Debug)]
pub enum RenameError {
	/// The old name is not in the lock, or its entry has no `skillPath`.
	NotLocked(String),
	/// The old name is locked but no installed copy was found.
	NoInstalledCopy,
	/// `old_name` and `new_name` sanitize to the same on-disk directory.
	SameSanitizedName,
	/// The new name already exists (lock entry or on-disk dir) in the scope.
	TargetExists,
	/// The locked `skillPath` was not found in the fetched tree.
	SkillPathNotFound,
	/// The fetched `SKILL.md` declares a name other than `new_name`.
	NameMismatch { declared: String, expected: String },
	/// The fetched `SKILL.md` failed to parse.
	ParseFailed(String),
	/// The pre-mutation snapshot failed (nothing was mutated).
	Snapshot(String),
	/// Installing the new name failed (rolled back).
	InstallFailed(String),
	/// Removing the old name failed (rolled back).
	RemovalFailed(String),
	/// Removing the old lock entry failed (rolled back).
	LockRemovalFailed(String),
}

impl RenameError {
	/// The machine code for surfaces that expose one, else `None`.
	pub fn code(&self) -> Option<&'static str> {
		match self {
			RenameError::SameSanitizedName | RenameError::TargetExists => {
				Some(RENAME_TARGET_EXISTS_CODE)
			}
			_ => None,
		}
	}

	/// The canonical, safe, user-facing message (no internal paths).
	pub fn message(&self) -> String {
		match self {
			RenameError::NotLocked(m) => m.clone(),
			RenameError::NoInstalledCopy => {
				"Skill is locked but no installed copy was found".to_string()
			}
			RenameError::SameSanitizedName => {
				"old_name and new_name resolve to the same on-disk skill \
				 directory; choose a distinct rename target"
					.to_string()
			}
			RenameError::TargetExists => {
				"A skill with the new name already exists in this scope (lock \
				 entry or on-disk directory); pick a rename target that does \
				 not already exist"
					.to_string()
			}
			RenameError::SkillPathNotFound => {
				"Locked skillPath was not found in fetched source".to_string()
			}
			RenameError::NameMismatch { declared, expected } => format!(
				"Fetched SKILL.md declares name '{declared}', expected \
				 '{expected}'. Verify the new_name matches the upstream source."
			),
			RenameError::ParseFailed(e) => {
				format!("Failed to parse fetched skill: {e}")
			}
			RenameError::Snapshot(e) => e.clone(),
			RenameError::InstallFailed(e) => {
				format!("Failed to install renamed skill: {e}")
			}
			RenameError::RemovalFailed(e) => e.clone(),
			RenameError::LockRemovalFailed(e) => e.clone(),
		}
	}
}

/// Read the OLD-name lock entry for the fetch coordinates. The adapter calls
/// this BEFORE it fetches, then applies any `--ref` override to `ref_name` and
/// passes the result back through [`FetchedRename`].
pub fn rename_source_from_lock(
	old_name: &str,
	scope: &RenameScope,
) -> Result<RenameLockSource, RenameError> {
	match scope {
		RenameScope::Global => {
			let lock = skill::lock::global::read_skill_lock();
			let entry = lock.skills.get(old_name).ok_or_else(|| {
				RenameError::NotLocked(
					"Skill is not in global lock".to_string(),
				)
			})?;
			let skill_path = entry.skill_path.clone().ok_or_else(|| {
				RenameError::NotLocked(
					"Locked skill has no skillPath".to_string(),
				)
			})?;
			Ok(RenameLockSource {
				source: entry.source.clone(),
				source_type: entry.source_type.clone(),
				source_url: entry.source_url.clone(),
				ref_name: entry.ref_name.clone(),
				skill_path,
			})
		}
		RenameScope::Project { root } => {
			let lock = skill::lock::local::read_local_lock(Some(root));
			let entry = lock.skills.get(old_name).ok_or_else(|| {
				RenameError::NotLocked(
					"Skill is not in project lock".to_string(),
				)
			})?;
			let skill_path = entry.skill_path.clone().ok_or_else(|| {
				RenameError::NotLocked(
					"Locked skill has no skillPath".to_string(),
				)
			})?;
			Ok(RenameLockSource {
				source: entry.source.clone(),
				source_type: entry.source_type.clone(),
				// Prefer the recorded clone URL so a non-github host is fetched
				// correctly; fall back to `source` for github/legacy locks.
				source_url: entry
					.source_url
					.clone()
					.unwrap_or_else(|| entry.source.clone()),
				ref_name: entry.ref_name.clone(),
				skill_path,
			})
		}
	}
}

/// P0-2 guard (a): reject a degenerate rename whose old/new names sanitize to
/// the same on-disk directory — the install would write the very dir the
/// removal then deletes. Exposed so an adapter can refuse BEFORE it fetches (the
/// original before-fetch UX); [`accept_rename`] also calls it as defense in
/// depth for direct callers.
pub fn ensure_distinct_names(
	old_name: &str,
	new_name: &str,
) -> Result<(), RenameError> {
	if skill::sanitize::sanitize_name(old_name)
		== skill::sanitize::sanitize_name(new_name)
	{
		Err(RenameError::SameSanitizedName)
	} else {
		Ok(())
	}
}

/// Run the rename transaction. Steps 2/4/5/6/7/8/9 of the flow plus the P0-1/2/3
/// data-loss guards and a best-effort rollback to the pre-mutation state on any
/// post-snapshot failure. Assumes the caller has confirmed the operation and
/// already fetched the source.
pub fn accept_rename(
	req: RenameRequest,
	fetched: FetchedRename,
) -> Result<RenameSuccess, RenameError> {
	let resource_scope = req.scope.resource_scope();
	let project_root = req.scope.project_root();

	// P0-2 guard (a) — also enforced by adapters before fetch.
	ensure_distinct_names(req.old_name, req.new_name)?;

	// Step 2: target agents = those that ACTUALLY have the old name installed
	// (never every agent). Mirrors apply-update only touching installed roots.
	let target_agents: Vec<crate::models::AgentType> =
		crate::load_all_agents(resource_scope, project_root)
			.into_iter()
			.filter(|r| r.skills.iter().any(|s| s.name == req.old_name))
			.filter_map(|r| r.agent_id.parse().ok())
			.collect();
	if target_agents.is_empty() {
		return Err(RenameError::NoInstalledCopy);
	}

	// Step 4: locate the skill file in the fetched tree (containment check).
	let skill_file = crate::skills::update::sanitize_skill_path(
		fetched.repo_root,
		&fetched.source.skill_path,
	)
	.ok_or(RenameError::SkillPathNotFound)?;

	// Step 5: verify the fetched name matches new_name (confirms this rename).
	let parsed = skill::parser::parse(&skill_file)
		.map_err(|e| RenameError::ParseFailed(e.to_string()))?;
	if parsed.name != req.new_name {
		return Err(RenameError::NameMismatch {
			declared: parsed.name,
			expected: req.new_name.to_string(),
		});
	}

	let agent_dirs = crate::skills::removal::agent_skill_dirs_in_scope(
		resource_scope,
		project_root,
	);

	// P0-2 guard (b): refuse if the new name ALREADY exists (lock entry or
	// on-disk dir). The rollback deletes EVERY new_name path; requiring new_name
	// to be absent makes that cleanup safe.
	if new_name_exists_in_scope(
		req.new_name,
		resource_scope,
		project_root,
		&agent_dirs,
	) {
		return Err(RenameError::TargetExists);
	}

	// Step 6: SNAPSHOT the old-name dirs + clone the old lock entry BEFORE
	// mutating. A snapshot failure (P0-3) aborts before install — nothing
	// mutated.
	let snapshot = snapshot_old_skill(
		req.old_name,
		resource_scope,
		project_root,
		&agent_dirs,
	)
	.map_err(RenameError::Snapshot)?;
	let old_global_entry: Option<skill::SkillLockEntry> = match &req.scope {
		RenameScope::Global => skill::lock::global::read_skill_lock()
			.skills
			.get(req.old_name)
			.cloned(),
		RenameScope::Project { .. } => None,
	};
	let old_local_entry: Option<skill::LocalSkillLockEntry> = match &req.scope {
		RenameScope::Project { root } => {
			skill::lock::local::read_local_lock(Some(root))
				.skills
				.get(req.old_name)
				.cloned()
		}
		RenameScope::Global => None,
	};

	// Roll the WHOLE transaction back to its pre-mutation state. Defined BEFORE
	// install so every post-snapshot failure path (P0-1) runs the SAME rollback.
	let rollback_all = || {
		rollback_rename_install(
			req.new_name,
			resource_scope,
			project_root,
			&agent_dirs,
		);
		let _ = remove_lock_entry(req.new_name, &req.scope);
		restore_snapshot(&snapshot);
		let _ = restore_lock_entry(
			req.old_name,
			&req.scope,
			old_global_entry.as_ref(),
			old_local_entry.as_ref(),
		);
	};

	// Step 7: install the new-named skill. A failure AFTER this point rolls back
	// (install writes the master/link before the lock, so an Err may have left a
	// half-installed new_name — P0-1).
	let install_source = skill::InstallLockSource {
		source: fetched.source.source.clone(),
		source_type: fetched.source.source_type.clone(),
		source_url: fetched.source.source_url.clone(),
		ref_name: fetched.source.ref_name.clone(),
	};
	let install_req =
		crate::skills::install_fetched::FetchedSkillInstallRequest {
			skill_file: &skill_file,
			source: &install_source,
			lock_skill_path: fetched.source.skill_path.clone(),
			ref_commit: Some(fetched.oid.to_string()),
			scope: resource_scope,
			project_root,
			target_agents: &target_agents,
			expected_name: Some(req.new_name),
			target: if matches!(resource_scope, ResourceScope::ProjectOnly) {
				LinkTarget::Relative
			} else {
				LinkTarget::Absolute
			},
		};
	let install_report =
		match crate::skills::install_fetched::install_fetched_skill_and_lock(
			install_req,
		) {
			Ok(r) => r,
			Err(e) => {
				rollback_all();
				return Err(RenameError::InstallFailed(e.to_string()));
			}
		};
	if !install_report.agent_results.iter().any(|r| r.installed) {
		let detail = install_report
			.agent_results
			.iter()
			.find_map(|r| r.error.clone())
			.unwrap_or_else(|| "no agent received the skill".to_string());
		rollback_all();
		return Err(RenameError::InstallFailed(detail));
	}

	let installed_paths: Vec<String> = install_report
		.agent_results
		.iter()
		.filter(|r| r.installed)
		.filter_map(|r| {
			crate::create_adapter(r.agent)
				.get_skills_paths(project_root, resource_scope)
				.first()
				.map(|p| p.join(req.new_name).display().to_string())
		})
		.collect();

	// Step 8: remove the old-name dirs. A removal failure rolls back the txn.
	let mut old_skill = crate::models::Skill::new(req.old_name);
	if let Some(dir) = agent_dirs.first() {
		old_skill.source_path = Some(
			dir.join(req.old_name)
				.join("SKILL.md")
				.display()
				.to_string(),
		);
	}
	let removal_plan = crate::skills::removal::plan_removal(
		&old_skill,
		None,
		&agent_dirs,
		project_root,
		true,
	);
	let removal_roots =
		crate::skills::removal::allowed_skill_roots(&agent_dirs, project_root);
	let removal_report = match crate::skills::removal::execute_removal(
		&removal_plan,
		&removal_roots,
	) {
		Ok(r) => r,
		Err(e) => {
			rollback_all();
			return Err(RenameError::RemovalFailed(format!(
				"Failed to remove old skill '{}': {e}",
				req.old_name
			)));
		}
	};
	if !removal_report.failed.is_empty() {
		// Per-path detail goes to the log; the returned message stays path-free
		// so the API contract (no raw filesystem paths in errors) holds when a
		// surface forwards it verbatim (Codex A review, Minor 6).
		let failed_msgs: Vec<String> = removal_report
			.failed
			.iter()
			.map(|(p, e)| format!("{}: {e}", p.display()))
			.collect();
		log::warn!(
			"rename: partial removal failure for old skill '{}': {}",
			req.old_name,
			failed_msgs.join("; ")
		);
		rollback_all();
		return Err(RenameError::RemovalFailed(format!(
			"Partial removal failure removing old skill '{}'",
			req.old_name
		)));
	}

	// Step 9: remove the old-name lock entry. A failure here means the txn did
	// not fully commit -> roll everything back.
	if let Err(e) = remove_lock_entry(req.old_name, &req.scope) {
		rollback_all();
		return Err(RenameError::LockRemovalFailed(format!(
			"Failed to remove old lock entry '{}': {e}",
			req.old_name
		)));
	}

	Ok(RenameSuccess {
		installed_hash: install_report.installed_hash,
		paths: installed_paths,
	})
}

/// Whether `new_name` already has a lock entry OR an on-disk skill dir in the
/// target scope's agent dirs / universal master.
fn new_name_exists_in_scope(
	new_name: &str,
	scope: ResourceScope,
	project_root: Option<&Path>,
	agent_dirs: &[PathBuf],
) -> bool {
	let in_lock = match scope {
		ResourceScope::GlobalOnly => {
			skill::lock::global::get_skill_from_lock(new_name).is_some()
		}
		ResourceScope::ProjectOnly => project_root.is_some_and(|root| {
			skill::lock::local::read_local_lock(Some(root))
				.skills
				.contains_key(new_name)
		}),
		ResourceScope::Both => false,
	};
	if in_lock {
		return true;
	}
	let safe = skill::sanitize::sanitize_name(new_name);
	let mut targets: Vec<PathBuf> =
		agent_dirs.iter().map(|d| d.join(&safe)).collect();
	let canonical_root = if matches!(scope, ResourceScope::ProjectOnly) {
		project_root
	} else {
		None
	};
	if let Some(master) = universal_canonical_dir(canonical_root) {
		targets.push(master.join(&safe));
	}
	targets.iter().any(|p| std::fs::symlink_metadata(p).is_ok())
}

fn remove_lock_entry(name: &str, scope: &RenameScope) -> Result<(), String> {
	match scope {
		RenameScope::Global => skill::lock::global::modify_skill_lock(|lock| {
			lock.skills.remove(name);
		})
		.map_err(|e| format!("global lock write failed: {e}")),
		RenameScope::Project { root } => {
			skill::lock::local::modify_local_lock(Some(root), |lock| {
				lock.skills.remove(name);
			})
			.map_err(|e| format!("project lock write failed: {e}"))
		}
	}
}

fn restore_lock_entry(
	name: &str,
	scope: &RenameScope,
	global_entry: Option<&skill::SkillLockEntry>,
	local_entry: Option<&skill::LocalSkillLockEntry>,
) -> Result<(), String> {
	match scope {
		RenameScope::Global => {
			let Some(entry) = global_entry else {
				return Ok(());
			};
			let entry = entry.clone();
			let name = name.to_string();
			skill::lock::global::modify_skill_lock(move |lock| {
				lock.skills.insert(name, entry);
			})
			.map_err(|e| format!("global lock restore failed: {e}"))
		}
		RenameScope::Project { root } => {
			let Some(entry) = local_entry else {
				return Ok(());
			};
			let entry = entry.clone();
			let name = name.to_string();
			skill::lock::local::modify_local_lock(Some(root), move |lock| {
				lock.skills.insert(name, entry);
			})
			.map_err(|e| format!("project lock restore failed: {e}"))
		}
	}
}

/// A filesystem snapshot of one skill name across the in-scope agent dirs + the
/// universal master, copied into a temp backup so a failed transaction can be
/// rolled back to its pre-mutation state.
struct SkillSnapshot {
	/// `_tmp` owns the backup tree; dropping it deletes the backup.
	_tmp: tempfile::TempDir,
	/// `(live_path, backup_path)` pairs for every captured location.
	entries: Vec<(PathBuf, PathBuf)>,
}

/// Capture the old-name skill across the in-scope agent dirs + the universal
/// master into a temp backup. Symlinks are preserved as symlinks; real dirs are
/// deep-copied. MUST run BEFORE any mutation: a backup failure aborts (returns
/// `Err`) so it can never become permanent old-skill loss when a later step
/// fails. Genuinely-absent paths are skipped.
fn snapshot_old_skill(
	name: &str,
	scope: ResourceScope,
	project_root: Option<&Path>,
	agent_dirs: &[PathBuf],
) -> Result<SkillSnapshot, String> {
	let safe = skill::sanitize::sanitize_name(name);
	let tmp = tempfile::tempdir()
		.map_err(|e| format!("Failed to create snapshot backup dir: {e}"))?;
	let mut entries: Vec<(PathBuf, PathBuf)> = Vec::new();
	let mut captured = std::collections::HashSet::new();

	let mut targets: Vec<PathBuf> =
		agent_dirs.iter().map(|d| d.join(&safe)).collect();
	let canonical_root = if matches!(scope, ResourceScope::ProjectOnly) {
		project_root
	} else {
		None
	};
	if let Some(master) = universal_canonical_dir(canonical_root) {
		targets.push(master.join(&safe));
	}

	for (idx, live) in targets.into_iter().enumerate() {
		if !captured.insert(live.clone()) {
			continue;
		}
		// A genuinely-absent path (NotFound) has nothing to back up. ANY OTHER
		// stat error (permission, I/O) on a possibly-existing target must ABORT
		// before mutation — treating it as "absent" could skip the backup and
		// then lose the old skill when a later step rolls back (Codex A review).
		let meta = match std::fs::symlink_metadata(&live) {
			Ok(m) => m,
			Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
			Err(e) => {
				return Err(format!(
					"Failed to stat old skill target before rename: {e}"
				));
			}
		};
		let backup = tmp.path().join(format!("snap-{idx}"));
		// A reparse point (Unix symlink OR Windows symlink/junction) is captured
		// by recording its target and re-creating it as a link — NEVER
		// deep-copied. `Linker::is_link` covers junctions, which bare
		// `is_symlink()` may miss.
		let result = if Linker::is_link(&live) {
			std::fs::read_link(&live)
				.and_then(|target| Linker::symlink(&target, &backup))
		} else if meta.is_dir() {
			Linker::copy_preserving_links(&live, &backup)
		} else {
			std::fs::copy(&live, &backup).map(|_| ())
		};
		result.map_err(|e| {
			format!("Failed to snapshot old skill before rename: {e}")
		})?;
		entries.push((live, backup));
	}

	Ok(SkillSnapshot { _tmp: tmp, entries })
}

/// Restore every captured location from a snapshot (best-effort rollback).
fn restore_snapshot(snapshot: &SkillSnapshot) {
	for (live, backup) in &snapshot.entries {
		// Clear whatever (partial) state is at `live` before restoring. A
		// reparse point is unlinked with `Linker::unlink` (junction-safe), NEVER
		// `remove_dir_all` — recursing into a junction would delete the Master.
		if Linker::is_link(live) {
			let _ = Linker::unlink(live);
		} else if let Ok(meta) = std::fs::symlink_metadata(live) {
			if meta.is_file() {
				let _ = std::fs::remove_file(live);
			} else if meta.is_dir() {
				let _ = std::fs::remove_dir_all(live);
			}
		}
		let Ok(meta) = std::fs::symlink_metadata(backup) else {
			continue;
		};
		let _ = if Linker::is_link(backup) {
			std::fs::read_link(backup)
				.and_then(|target| Linker::symlink(&target, live))
		} else if meta.is_dir() {
			Linker::copy_preserving_links(backup, live)
		} else {
			std::fs::copy(backup, live).map(|_| ())
		};
	}
}

/// Best-effort rollback of the just-installed new-name dirs (and the universal
/// master if it was freshly created), re-asserting containment before each
/// `remove_dir_all` (TOCTOU guard).
fn rollback_rename_install(
	new_name: &str,
	scope: ResourceScope,
	project_root: Option<&Path>,
	agent_dirs: &[PathBuf],
) {
	let safe = skill::sanitize::sanitize_name(new_name);
	let roots =
		crate::skills::removal::allowed_skill_roots(agent_dirs, project_root);
	for dir in agent_dirs {
		let target = dir.join(&safe);
		// A reparse point is unlinked directly with `Linker::unlink`
		// (junction-safe) — NEVER `remove_dir_all`, which would recurse into a
		// junction's Master. A real dir is removed only if contained.
		if Linker::is_link(&target) {
			let _ = Linker::unlink(&target);
		} else if let Ok(meta) = std::fs::symlink_metadata(&target) {
			if meta.is_dir()
				&& crate::skills::removal::assert_contained(&target, &roots)
					.is_some()
			{
				let _ = std::fs::remove_dir_all(&target);
			} else if meta.is_file() {
				let _ = std::fs::remove_file(&target);
			}
		}
	}
	let canonical_root = if matches!(scope, ResourceScope::ProjectOnly) {
		project_root
	} else {
		None
	};
	if let Some(canonical_dir) = universal_canonical_dir(canonical_root) {
		let canonical = canonical_dir.join(&safe);
		if canonical.exists()
			&& crate::skills::removal::assert_contained(&canonical, &roots)
				.is_some()
		{
			let _ = std::fs::remove_dir_all(&canonical);
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn only_target_exists_variants_carry_the_machine_code() {
		assert_eq!(
			RenameError::TargetExists.code(),
			Some(RENAME_TARGET_EXISTS_CODE)
		);
		assert_eq!(
			RenameError::SameSanitizedName.code(),
			Some(RENAME_TARGET_EXISTS_CODE)
		);
		assert_eq!(RenameError::NoInstalledCopy.code(), None);
		assert_eq!(
			RenameError::NameMismatch {
				declared: "a".into(),
				expected: "b".into()
			}
			.code(),
			None
		);
	}

	#[test]
	fn ensure_distinct_names_rejects_sanitized_collision() {
		// "old skill" and "old-skill" sanitize to the same on-disk dir.
		assert!(matches!(
			ensure_distinct_names("old skill", "old-skill"),
			Err(RenameError::SameSanitizedName)
		));
		assert!(ensure_distinct_names("old-skill", "new-skill").is_ok());
	}

	#[test]
	fn messages_are_non_empty_and_name_mismatch_reports_both() {
		let m = RenameError::NameMismatch {
			declared: "got".into(),
			expected: "want".into(),
		}
		.message();
		assert!(m.contains("got") && m.contains("want"));
		assert!(!RenameError::NoInstalledCopy.message().is_empty());
	}
}
