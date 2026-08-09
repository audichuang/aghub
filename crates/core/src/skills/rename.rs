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
use crate::skills::install_fetched::FetchedSkillInstallReport;
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
	/// The OLD-name entry's identity as it stood when [`rename_source_from_lock`]
	/// read it, BEFORE the fetch. An adapter is expected to override `ref_name`
	/// (a `--ref`) and to rewrite `skill_path` to the resolved new location — this
	/// field is the one it must leave alone, because it is what
	/// [`accept_rename`] compares the live entry against under the lock.
	pub captured: crate::skills::lock::EntryIdentity,
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
	/// The old name (carried) is locked but no installed copy was found.
	NoInstalledCopy(String),
	/// `old_name` and `new_name` sanitize to the same on-disk directory.
	SameSanitizedName,
	/// The new name (carried) already exists (lock entry or on-disk dir).
	TargetExists(String),
	/// The locked `skillPath` was not found in the fetched tree.
	SkillPathNotFound,
	/// The fetched `SKILL.md` declares a name other than `new_name`.
	NameMismatch { declared: String, expected: String },
	/// The fetched `SKILL.md` failed to parse.
	ParseFailed(String),
	/// The interprocess mutation lock could not be taken (nothing was mutated).
	Locked(String),
	/// The lock entry changed source/skillPath while this rename was fetching, so
	/// it is no longer the entry that was fetched (nothing was mutated).
	StaleFetch(String),
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
			RenameError::SameSanitizedName | RenameError::TargetExists(_) => {
				Some(RENAME_TARGET_EXISTS_CODE)
			}
			// Both are retryable and neither mutated anything — a surface that
			// cannot tell them from a permanent failure makes the caller give up
			// on a transient condition. Rename is the most destructive flow here,
			// so "retry" vs "do not retry" matters most.
			RenameError::Locked(_) => {
				Some(crate::skills::lock::MUTATION_LOCK_BUSY_CODE)
			}
			RenameError::StaleFetch(_) => {
				Some(crate::skills::lock::SOURCE_CHANGED_DURING_FETCH_CODE)
			}
			_ => None,
		}
	}

	/// The canonical, safe, user-facing message. Every variant produced by the
	/// transaction itself is name-based, not path-based. The two exceptions wrap
	/// an upstream error whose Display can embed a fetched temp path:
	/// `ParseFailed` (from `skill::parser::parse`) and, for a whole-install
	/// failure, `InstallFailed`. Neither is reachable through the API today --
	/// the adapter pre-parses the fetched `SKILL.md` before `accept_rename`
	/// re-parses it -- so redact them at the source if that ever changes.
	pub fn message(&self) -> String {
		match self {
			RenameError::NotLocked(m) => m.clone(),
			RenameError::NoInstalledCopy(old_name) => format!(
				"'{old_name}' is locked but no installed copy was found"
			),
			RenameError::SameSanitizedName => {
				"old_name and new_name resolve to the same on-disk skill \
				 directory; choose a distinct rename target"
					.to_string()
			}
			RenameError::TargetExists(new_name) => format!(
				"A skill named '{new_name}' already exists in this scope (lock \
				 entry or on-disk directory); pick a rename target that does \
				 not already exist"
			),
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
			RenameError::Locked(e) => e.clone(),
			RenameError::StaleFetch(e) => e.clone(),
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
			// From THIS read, not a second one: the coordinates below and the
			// identity must describe the same observation, or another process's
			// repoint between two reads would be fetched under one set and
			// compare-verified against the other.
			let captured =
				crate::skills::lock::EntryIdentity::of_global_entry(entry);
			Ok(RenameLockSource {
				source: entry.source.clone(),
				source_type: entry.source_type.clone(),
				source_url: entry.source_url.clone(),
				ref_name: entry.ref_name.clone(),
				skill_path,
				captured,
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
			// Same read as the coordinates below — see the Global arm.
			let captured =
				crate::skills::lock::EntryIdentity::of_project_entry(entry);
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
				captured,
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

	// Hold the interprocess mutation lock for the WHOLE transaction: the
	// target-absence check, the install, the old-name removal AND the rollback.
	// This is what makes the rollback's attribution sound — without it a
	// `new_name` another process created between the check and a failure here is
	// indistinguishable from our own work.
	let _mutation_guard = crate::skills::lock::mutation_guard(
		"accept rename",
		resource_scope,
		project_root,
	)
	.map_err(|e| RenameError::Locked(e.to_string()))?;

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
		return Err(RenameError::NoInstalledCopy(req.old_name.to_string()));
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
		return Err(RenameError::TargetExists(req.new_name.to_string()));
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

	// Reassert the old-name lock precondition INSIDE the transaction. The CLI/API
	// adapters obtain the source via `rename_source_from_lock` (which requires the
	// lock), but this is a public core entry point and `RenameLockSource` is
	// constructible — refuse to rename a skill that is installed but not lock-
	// managed rather than trusting fabricated coordinates. Checked before any
	// mutation (the snapshot above is non-mutating and is dropped on return).
	let (old_is_locked, scope_label) = match &req.scope {
		RenameScope::Global => (old_global_entry.is_some(), "global"),
		RenameScope::Project { .. } => (old_local_entry.is_some(), "project"),
	};
	if !old_is_locked {
		return Err(RenameError::NotLocked(format!(
			"'{}' is not in the {scope_label} lock; refusing to rename a skill \
			 that is not lock-managed",
			req.old_name
		)));
	}

	// Compare-after-fetch. The adapter read these coordinates, then FETCHED —
	// seconds during which another aghub process may have repointed this very
	// entry at a different source. The mutation lock cannot cover a network fetch,
	// so proving the entry is still the one we fetched is the other half of the
	// guarantee. Without it this transaction removes the old name AND its lock
	// entry (steps 8-9) on behalf of coordinates that no longer exist — deleting
	// the other process's skill and replacing it with content from a source the
	// lock no longer names. Checked here: after the guard, before any mutation.
	// All three coordinates bind, including the OLD skillPath: the snapshot holds
	// what the entry said pre-fetch, while the adapter's own resolved new path
	// lives in `source.skill_path` and is unaffected — so a rename that legitimately
	// follows a MOVED skill still works, and one racing another process's repoint
	// of the old name does not.
	fetched
		.source
		.captured
		.ensure_unchanged(req.old_name, resource_scope, project_root)
		.map_err(RenameError::StaleFetch)?;

	// Undo ONLY the new-name artifacts. This is the correct rollback for every
	// failure BEFORE Step 8 touches the old name: at that point the old dirs
	// and the old lock entry are still complete, so restoring them would mean
	// deleting live, intact directories and re-copying them from the backup --
	// pure risk with nothing to gain. `restore_snapshot` clears `live` before
	// re-copying and swallows both errors, so one unrelated I/O failure there
	// (ENOSPC, a parent that turned read-only) would destroy a skill the
	// transaction had not touched.
	//
	// Removal is scoped to what THIS call created, read off the install report:
	// only the agent dirs whose row reports `installed`, and the Master only when
	// `wrote_master`. A Master or Referrer we merely found and verified belongs
	// to whoever wrote it. `created: None` means the install returned Err without
	// a report, so nothing can be attributed and every new-name slot is cleared.
	//
	// The lock this transaction holds makes the RECEIPTS trustworthy: a
	// `modify_*_lock` insert under it is a genuine compare-and-set, so
	// `created_lock` can no longer be reported to two aghub processes at once,
	// and the `Some(report)` arms below are exact.
	//
	// The `None` arm is still a BLANKET cleanup, and it is not provably correct.
	// It is sound against another aghub process — the lock plus the pre-install
	// absence check leave nothing at `new_name` that could be someone else's —
	// but `npx skills` takes no lock of ours (see `skill::lock::guard`). So an
	// `npx skills` install of this exact `new_name`, landing between our absence
	// check and a report-less install failure, has its work deleted here. The
	// spec's intended payoff was an ATTRIBUTED fallback; delivering it needs
	// `install_fetched_skill_and_lock` to carry a partial receipt out of its Err
	// path (today an Err after `materialize_universal_master` but before the lock
	// write reports nothing, and that Master genuinely is ours to remove), which
	// is a signature change across every caller. Deferred deliberately, recorded
	// in docs/specs/2026-07-29-skill-mutation-interprocess-lock.md.
	let rollback_new_only = |created: Option<&FetchedSkillInstallReport>| {
		let (dirs, remove_master) = match created {
			Some(report) => {
				(report.created_referrer_dirs.clone(), report.wrote_master)
			}
			None => (agent_dirs.clone(), true),
		};
		rollback_materialized_install(
			req.new_name,
			resource_scope,
			project_root,
			&dirs,
			remove_master,
		);
		match created {
			// The write's own receipt decides: an entry we created is ours to
			// drop; one we only REPLACED must be put back, because deleting it
			// would destroy a writer we merely overwrote.
			Some(report) if report.created_lock => {
				let _ = remove_lock_entry(req.new_name, &req.scope);
			}
			Some(report) if report.wrote_lock => {
				let _ = restore_lock_entry(
					req.new_name,
					&req.scope,
					report.replaced_global_entry.as_ref(),
					report.replaced_project_entry.as_ref(),
				);
			}
			// Wrote no entry at all -- nothing to undo.
			Some(_) => {}
			// No report: nothing can be attributed, so fall back to removal.
			None => {
				let _ = remove_lock_entry(req.new_name, &req.scope);
			}
		}
	};

	// Roll the WHOLE transaction back to its pre-mutation state, old name
	// included. Used ONLY from Step 8 onward, once the old name itself has been
	// mutated. Defined BEFORE install so every post-old-mutation failure path
	// (P0-1) runs the SAME rollback.
	let rollback_all = |created: Option<&FetchedSkillInstallReport>| {
		rollback_new_only(created);
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
				rollback_new_only(None);
				return Err(RenameError::InstallFailed(e.to_string()));
			}
		};
	// A genuine runtime failure (`error: Some(..)`) on ANY target agent must
	// abort the transaction, even when other agents installed successfully --
	// otherwise Step 8 below removes the old name for every agent and the
	// failed one loses the skill entirely. `installed: false, error: None` is a
	// legitimate idempotent soft skip (already-correct link, see
	// `materialize_universal_master`), not a failure, so it must NOT trip this
	// branch on its own.
	if let Some((agent, detail)) = install_report
		.agent_results
		.iter()
		.find_map(|r| r.error.as_ref().map(|e| (r.agent, e.clone())))
	{
		// The per-agent detail comes from `LinkError`'s Display, which embeds
		// absolute link/target paths -- log it, but keep the returned message
		// path-free (naming only the agent) so the API contract (no raw
		// filesystem paths in errors) holds when a surface forwards it
		// verbatim. Mirrors the removal-failure branch below.
		log::warn!(
			"rename: install failed for agent '{}' installing '{}': {detail}",
			agent.as_str(),
			req.new_name
		);
		rollback_new_only(Some(&install_report));
		return Err(RenameError::InstallFailed(format!(
			"agent '{}' failed to install",
			agent.as_str()
		)));
	}
	if install_report.agent_results.is_empty() {
		// Degenerate case: no target rows at all, so nothing can have received
		// the skill. A NON-empty report whose rows are all `installed: false,
		// error: None` is deliberately NOT this case: that is the idempotent
		// soft skip, meaning every target's link for the new name was already
		// correct, so the new name IS installed and the rename may proceed.
		// (Unreachable in practice -- the pre-install target-existence check
		// above rejects a new_name that already exists in this scope, so only a
		// concurrent installer racing that check can produce an all-soft-skip
		// report, and failing here would delete that installer's work.)
		rollback_new_only(Some(&install_report));
		return Err(RenameError::InstallFailed(
			"no agent received the skill".to_string(),
		));
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
			rollback_all(Some(&install_report));
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
		rollback_all(Some(&install_report));
		return Err(RenameError::RemovalFailed(format!(
			"Partial removal failure removing old skill '{}'",
			req.old_name
		)));
	}

	// Step 9: remove the old-name lock entry. A failure here means the txn did
	// not fully commit -> roll everything back.
	if let Err(e) = remove_lock_entry(req.old_name, &req.scope) {
		rollback_all(Some(&install_report));
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

/// Best-effort rollback of the new-name artifacts this call created,
/// re-asserting containment before each `remove_dir_all` (TOCTOU guard).
///
/// `agent_dirs` is the caller's attribution of which dirs to clear, and
/// `remove_master` whether the canonical Master was newly written by the same
/// call — a Master that merely existed and verified belongs to whoever wrote it.
/// Undo a `materialize_universal_master` from its OWN receipt: unlink the
/// referrers this call created, then remove the Master only if this call wrote
/// it. Shared with `install_fetched`, which must clean up after itself when a
/// post-materialization step fails — the rename flow is not the only caller
/// that can fail with a fresh Master already on disk.
pub(crate) fn rollback_materialized_install(
	new_name: &str,
	scope: ResourceScope,
	project_root: Option<&Path>,
	agent_dirs: &[PathBuf],
	remove_master: bool,
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
	if !remove_master {
		return;
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

	/// Exactly which variants carry a machine code, and WHICH code — checked
	/// exhaustively, so attaching one to the wrong variant fails here.
	///
	/// The two RETRYABLE variants (`Locked`, `StaleFetch`) must be distinguishable
	/// from a permanent failure or a surface tells the user to give up on a
	/// transient condition; every other variant is permanent and carries none.
	#[test]
	fn only_target_exists_variants_carry_the_machine_code() {
		let s = || "x".to_string();
		let all = [
			RenameError::NotLocked(s()),
			RenameError::NoInstalledCopy(s()),
			RenameError::SameSanitizedName,
			RenameError::TargetExists(s()),
			RenameError::SkillPathNotFound,
			RenameError::NameMismatch {
				declared: s(),
				expected: s(),
			},
			RenameError::ParseFailed(s()),
			RenameError::Locked(s()),
			RenameError::StaleFetch(s()),
			RenameError::Snapshot(s()),
			RenameError::InstallFailed(s()),
			RenameError::RemovalFailed(s()),
			RenameError::LockRemovalFailed(s()),
		];
		for e in &all {
			let expected = match e {
				RenameError::SameSanitizedName
				| RenameError::TargetExists(_) => Some(RENAME_TARGET_EXISTS_CODE),
				RenameError::Locked(_) => {
					Some(crate::skills::lock::MUTATION_LOCK_BUSY_CODE)
				}
				RenameError::StaleFetch(_) => {
					Some(crate::skills::lock::SOURCE_CHANGED_DURING_FETCH_CODE)
				}
				_ => None,
			};
			assert_eq!(e.code(), expected, "unexpected code() for {e:?}");
		}
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

	/// Messages carry the identifying names the CLI/API surfaced before the
	/// extraction (pins the "no message change" contract), and stay path-free.
	#[test]
	fn messages_carry_their_identifying_names() {
		assert!(RenameError::NoInstalledCopy("myskill".into())
			.message()
			.contains("myskill"));
		assert!(RenameError::TargetExists("newname".into())
			.message()
			.contains("newname"));
		let mismatch = RenameError::NameMismatch {
			declared: "got".into(),
			expected: "want".into(),
		}
		.message();
		assert!(mismatch.contains("got") && mismatch.contains("want"));
	}

	/// Ownership attribution: the rollback may only remove what its own install
	/// created. A Master the call merely found and verified (`wrote_master` false)
	/// and a Referrer that already pointed at it (a soft-skipped row, so not in
	/// the attributed dirs) both belong to whoever wrote them — a rollback that
	/// takes them out destroys another writer's install.
	#[cfg(unix)]
	#[test]
	fn rollback_keeps_a_master_and_referrer_it_did_not_create() {
		let tmp = tempfile::tempdir().unwrap();
		let root = tmp.path();
		let master = universal_canonical_dir(Some(root)).unwrap();
		let mine = root.join(".claude/skills");
		let theirs = root.join(".other/skills");
		std::fs::create_dir_all(&master).unwrap();
		std::fs::create_dir_all(&mine).unwrap();
		std::fs::create_dir_all(&theirs).unwrap();

		// A Master this call did NOT write, plus one Referrer per agent dir.
		let foreign_master = master.join("new-skill");
		std::fs::create_dir_all(&foreign_master).unwrap();
		std::fs::write(foreign_master.join("SKILL.md"), "NOT MINE").unwrap();
		let my_ref = mine.join("new-skill");
		let their_ref = theirs.join("new-skill");
		std::os::unix::fs::symlink(&foreign_master, &my_ref).unwrap();
		std::os::unix::fs::symlink(&foreign_master, &their_ref).unwrap();

		// Attribution: only `mine` was freshly linked; the Master pre-existed.
		rollback_materialized_install(
			"new-skill",
			ResourceScope::ProjectOnly,
			Some(root),
			std::slice::from_ref(&mine),
			false,
		);

		assert!(
			!Linker::is_link(&my_ref) && !my_ref.exists(),
			"the referrer this call created must be removed"
		);
		assert!(
			Linker::is_link(&their_ref),
			"a referrer this call did not create must survive"
		);
		assert_eq!(
			std::fs::read_to_string(foreign_master.join("SKILL.md")).unwrap(),
			"NOT MINE",
			"a Master this call did not write must survive untouched"
		);
	}

	/// The data-safety heart of the transaction: after the old skill has ALREADY
	/// been removed (steps 7+8 done) and a later step fails, the rollback must
	/// clean every new-name path AND restore the old skill — including a
	/// universal-install REFERRER re-created as a link (not materialized), with
	/// its original content. Drives the private rollback helpers directly, in
	/// project scope under a tempdir (no HOME, deterministic, no root-skip) — the
	/// path the through-the-transaction tests cannot reach without a failure hook.
	#[cfg(unix)]
	#[test]
	fn rollback_restores_old_skill_and_referrer_after_removal() {
		let tmp = tempfile::tempdir().unwrap();
		let root = tmp.path();
		let claude = root.join(".claude/skills");
		let master = universal_canonical_dir(Some(root)).unwrap();
		std::fs::create_dir_all(&claude).unwrap();
		std::fs::create_dir_all(&master).unwrap();
		// Universal layout for old-skill: Master (real dir) + Claude referrer
		// (symlink → Master).
		let old_master = master.join("old-skill");
		std::fs::create_dir_all(&old_master).unwrap();
		std::fs::write(old_master.join("SKILL.md"), "ORIGINAL").unwrap();
		let old_ref = claude.join("old-skill");
		std::os::unix::fs::symlink(&old_master, &old_ref).unwrap();

		let agent_dirs = vec![claude.clone()];
		// Step 6: snapshot BEFORE any mutation.
		let snapshot = snapshot_old_skill(
			"old-skill",
			ResourceScope::ProjectOnly,
			Some(root),
			&agent_dirs,
		)
		.unwrap();

		// Simulate steps 7+8 completing: new-skill installed, old removed.
		let new_ref = claude.join("new-skill");
		let new_master = master.join("new-skill");
		std::fs::create_dir_all(&new_master).unwrap();
		std::os::unix::fs::symlink(&new_master, &new_ref).unwrap();
		Linker::unlink(&old_ref).unwrap();
		std::fs::remove_dir_all(&old_master).unwrap();
		assert!(
			!old_ref.exists() && !old_master.exists(),
			"precondition: old skill must be fully removed"
		);

		// A later step (9) fails → the same rollback the transaction runs. This
		// call created both the referrer and the Master, so both are attributed.
		rollback_materialized_install(
			"new-skill",
			ResourceScope::ProjectOnly,
			Some(root),
			&agent_dirs,
			true,
		);
		restore_snapshot(&snapshot);

		// New-name paths are gone.
		assert!(
			!Linker::is_link(&new_ref) && !new_ref.exists(),
			"rollback must remove the new-name referrer"
		);
		assert!(!new_master.exists(), "rollback must remove the new master");
		// Old skill restored: master content intact, referrer re-created AS A
		// LINK (not deep-copied into a real dir).
		assert_eq!(
			std::fs::read_to_string(old_master.join("SKILL.md")).unwrap(),
			"ORIGINAL",
			"old master content must be restored"
		);
		assert!(
			Linker::is_link(&old_ref),
			"old referrer must be restored as a link, not materialized"
		);
	}
}
