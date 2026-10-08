//! Apply a [`RepairPlan`]. The execution half of `shape.rs`.
//!
//! **The plan is the ONLY input.** Nothing here re-runs `classify_shape`: a
//! second classification is a second opinion that can disagree across the
//! window (concurrent npx). The caller re-plans if it wants a fresher view.
//!
//! **Ordering is crash-safety.** A process dying at ANY point leaves one of two
//! readable states: the old real directory still serving the skill, or the
//! Master present with the Referrer still a real directory (npx's own shape,
//! which repair absorbs). So there is no rollback and no receipt.
//!
//! The one destructive step is the shared-slot swap, and it goes LAST:
//!
//! 1. create a dot-prefixed temp link inside the slot dir — this proves link
//!    creation works on this filesystem BEFORE anything destructive, and the dot
//!    prefix keeps npx's `scanDir` and agent scanners off it;
//! 2. rename the old directory into `.aghub/.quarantine/<name>/<stamp>/`;
//! 3. rename the temp link over the real name.
//!
//! Doing 2 before 1 leaves a window where the skill is readable from NOWHERE;
//! dying there (SIGKILL, ENOSPC, no symlink/junction support) leaves the legal
//! `Absent` shape, indistinguishable from a deliberate withhold.
//!
//! Quarantine is nested `<name>/<stamp>/`, never flat `<name>-<stamp>`:
//! sanitized names contain hyphens, so the flat form cannot be split back.

use std::path::{Path, PathBuf};

use crate::errors::{ConfigError, Result};
use crate::scope::WriteScope;
use crate::skills::linker::Linker;
use crate::skills::shape::{
	compat_unlink_permitted, ReferrerAction, RefuseReason, RepairPlan,
};

/// What repair DID, not what the skill IS. One per shape, per the spec table.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RepairOutcome {
	/// Already correct; nothing written.
	Conformant,
	/// Master moved into the store and implicit reads became explicit
	/// Referrers.
	Migrated,
	/// A chain or a Referrer pointing elsewhere, repointed at the Master.
	Relinked,
	/// npx-clobbered and hash-equal: the fork was quarantined and the Referrer
	/// restored.
	Reconciled,
	/// Only stale Referrers were detached from dirs the agent only READS.
	/// Distinct from `Conformant`, which promises nothing was written.
	Tidied,
	/// Nothing written. `reason` says why, `fix` is the literal next command or
	/// path — a refused row must read as an instruction, not a diagnosis.
	Refused { reason: String, fix: String },
	/// The repair was attempted and failed with ANY per-skill error: an OS
	/// refusal (EACCES, ENOSPC, a Windows sharing violation), a mutation lock
	/// that could not be taken, a failed copy. A re-run MAY clear it but is not
	/// guaranteed to, so read `reason`. Distinct from `Refused`: a refusal is a
	/// DECISION the next run repeats.
	/// Folding them would break dry-run parity (preview and commit agree).
	Failed { reason: String, fix: String },
}

/// The result of one skill's repair.
#[derive(Debug, Clone, serde::Serialize)]
pub struct RepairReport {
	pub name: String,
	/// The shape repair FOUND, so the outcome explains itself.
	pub shape: Option<crate::skills::shape::SkillShape>,
	pub outcome: RepairOutcome,
	pub master: PathBuf,
	/// Referrers created or repointed.
	pub referrers: Vec<PathBuf>,
	/// Stale Referrers detached from read-only compat dirs. Separate from
	/// `referrers` (grants this run MADE), or it reads as granting twice.
	pub unlinked: Vec<PathBuf>,
	/// Where a fork was moved, when one was.
	pub quarantined: Option<PathBuf>,
	/// Agents that STILL share one directory after this repair (no private
	/// skills dir, so a grant for one is a grant for all). The preview must
	/// say so.
	pub fused: Vec<String>,
	/// True when the writes were withheld. A dry run walks the SAME branches
	/// (hash comparison included), so its verdict is the commit's verdict.
	pub dry_run: bool,
}

/// Compare a fork against the Master.
///
/// Tri-state on purpose. `Undecidable` must never fold into `Equal`: two hash
/// `Err`s are not evidence of sameness, and treating them as such would
/// `remove_dir_all` a user's only copy.
enum Comparison {
	Equal,
	Diverged,
	Undecidable(String),
}

fn compare(fork: &Path, master: &Path) -> Comparison {
	match (
		skill::hash::compute_skill_folder_hash(fork),
		skill::hash::compute_skill_folder_hash(master),
	) {
		(Ok(a), Ok(b)) if a == b => Comparison::Equal,
		(Ok(_), Ok(_)) => Comparison::Diverged,
		(Err(e), _) | (_, Err(e)) => Comparison::Undecidable(e.to_string()),
	}
}

/// A collision-safe quarantine stamp.
///
/// Nanoseconds since the epoch: a same-instant collision would silently merge
/// two different forks into one directory, so the caller treats an existing
/// stamp dir as a hard error rather than reusing it.
fn stamp() -> String {
	std::time::SystemTime::now()
		.duration_since(std::time::UNIX_EPOCH)
		.map(|d| format!("{}-{:09}", d.as_secs(), d.subsec_nanos()))
		.unwrap_or_else(|_| "0-000000000".to_string())
}

fn io_err(context: &str, e: std::io::Error) -> ConfigError {
	ConfigError::Io(std::io::Error::new(e.kind(), format!("{context}: {e}")))
}

/// Plan and apply a repair for ONE skill, under the interprocess mutation lock.
///
/// **The seam both surfaces call.** `execute_repair` is the lock-free applier;
/// a surface calling it directly skips the guard.
///
/// The guard is taken BEFORE `plan_repair`: the plan IS the deciding read
/// (`crates/core/AGENTS.md` "Mutation attribution"). A dry run takes none.
///
/// `grant_to` is computed here, not passed in, so it is answered against the
/// layout as it stands and under the same lock as the plan.
pub fn repair_skill(
	scope: &WriteScope,
	name: &str,
	in_lock: bool,
	dry_run: bool,
) -> Result<RepairReport> {
	let _guard = if dry_run {
		None
	} else {
		Some(
			crate::skills::lock::mutation_guard(
				"skill repair",
				scope.resource_scope(),
				scope.project_root(),
			)
			.map_err(|e| io_err("acquire the mutation lock", e))?,
		)
	};
	let grant_to = crate::skills::shape::readers_of(
		scope.resource_scope(),
		scope.project_root(),
		name,
	);
	// Invariant: a WriteScope always names a single store.
	let plan = crate::skills::shape::plan_repair(
		scope.resource_scope(),
		scope.project_root(),
		name,
		in_lock,
		&grant_to,
	)
	.expect("a WriteScope always names a single store");
	// Bulk worklists hold only lock names, so this fires only for a NAMED,
	// unlocked name that exists nowhere (a typo). A locked name with nothing
	// on disk stays Conformant on purpose: the desktop migration banner must
	// not start refusing.
	if !in_lock && grant_to.is_empty() && plan.finds_nothing() {
		return Ok(not_found_report(&plan, dry_run));
	}
	execute_repair(&plan, dry_run)
}

/// The refusal for a name that is in no lock and that no agent reads.
fn not_found_report(plan: &RepairPlan, dry_run: bool) -> RepairReport {
	let name = &plan.name;
	RepairReport {
		name: name.clone(),
		shape: plan.actions.first().map(|a| a.shape.clone()),
		outcome: RepairOutcome::Refused {
			reason: format!(
				"no skill named '{name}' at this scope: no Master at {}, no lock entry, and no agent reads it from any of its skill dirs",
				plan.master.display()
			),
			fix: "check the spelling: repair takes the SKILL.md frontmatter `name:`. List what exists with `aghub-cli doctor` (or `get skills -a all`); to install it, use `aghub-cli source sync <source> --skill <name> --install-missing`".to_string(),
		},
		master: plan.master.clone(),
		referrers: Vec::new(),
		unlinked: Vec::new(),
		quarantined: None,
		fused: Vec::new(),
		dry_run,
	}
}

/// Repair EVERY skill the lock names at this scope (or just `name`).
///
/// **The one home for the batch** — CLI and API are thin adapters over it.
///
/// **A failing skill does not abort the batch**: its error becomes a
/// [`RepairOutcome::Failed`] row, so the answer is always a complete receipt.
/// See docs/history/core-repair-rename.md#repair-batch-loop-lived-in-each-surface
pub fn repair_all(
	scope: &WriteScope,
	name: Option<&str>,
	dry_run: bool,
) -> Result<Vec<RepairReport>> {
	// ONE guard for the whole batch, taken BEFORE the lock read below: the lock
	// decides which directory may be ADOPTED as a Master, so a stale `in_lock`
	// could adopt content nobody authorized. `repair_skill`'s inner acquire is
	// reentrant. Dry runs take none, so a preview is authority for nothing.
	let _bulk_guard = if dry_run {
		None
	} else {
		Some(
			crate::skills::lock::mutation_guard(
				"skill repair (bulk)",
				scope.resource_scope(),
				scope.project_root(),
			)
			.map_err(|e| io_err("acquire the mutation lock", e))?,
		)
	};

	// Fail CLOSED: an unreadable lock must not read as "nothing to repair".
	let mut in_lock: std::collections::BTreeSet<String> =
		std::collections::BTreeSet::new();
	match scope {
		WriteScope::Global => {
			in_lock.extend(
				skill::lock::read_global_lock_checked()
					.map_err(|e| io_err("read the global skill lock", e))?
					.skills
					.into_keys(),
			);
		}
		WriteScope::Project { root } => {
			in_lock.extend(
				skill::lock::local::read_local_lock_checked(Some(root))
					.map_err(|e| io_err("read the project skill lock", e))?
					.skills
					.into_keys(),
			);
		}
	}

	// A named skill is repaired even if unlocked: the lock only decides ADOPTION.
	let worklist: Vec<String> = match name {
		Some(one) => vec![one.to_string()],
		None => in_lock.iter().cloned().collect(),
	};

	let mut reports = Vec::new();
	for skill_name in &worklist {
		match repair_skill(
			scope,
			skill_name,
			in_lock.contains(skill_name),
			dry_run,
		) {
			Ok(report) => {
				// Bulk runs stay quiet about conformant skills; a named one reports.
				if report.outcome == RepairOutcome::Conformant && name.is_none()
				{
					continue;
				}
				reports.push(report);
			}
			Err(e) => reports.push(failed_report(skill_name, dry_run, &e)),
		}
	}
	Ok(reports)
}

/// The row a skill gets when its own repair errored.
///
/// Names no path: that would claim a write landed. `fix` says re-run because
/// repair is idempotent.
fn failed_report(name: &str, dry_run: bool, e: &ConfigError) -> RepairReport {
	RepairReport {
		name: name.to_string(),
		shape: None,
		outcome: RepairOutcome::Failed {
			reason: e.to_string(),
			fix: format!(
				"fix the cause above (most often a permission on the skill \
				 directory), then re-run `aghub-cli repair {name}` — repair \
				 is idempotent, so a partly-applied skill is picked up where it \
				 stands"
			),
		},
		master: PathBuf::new(),
		referrers: Vec::new(),
		unlinked: Vec::new(),
		quarantined: None,
		fused: Vec::new(),
		dry_run,
	}
}

/// Apply `plan`. With `dry_run`, decides everything and writes nothing.
pub fn execute_repair(
	plan: &RepairPlan,
	dry_run: bool,
) -> Result<RepairReport> {
	let mut report = RepairReport {
		name: plan.name.clone(),
		// The shared slot where there is one: every non-conformant shape shows
		// up there.
		shape: plan
			.actions
			.iter()
			.find(|a| a.shared)
			.or_else(|| plan.actions.first())
			.map(|a| a.shape.clone()),
		outcome: RepairOutcome::Conformant,
		master: plan.master.clone(),
		referrers: Vec::new(),
		unlinked: Vec::new(),
		quarantined: None,
		// Only agents the user manages: a disabled one is not worth a line
		// in a preview about agents aghub serves.
		fused: {
			let disabled = crate::agent_settings::disabled_agents();
			plan.actions
				.iter()
				.filter(|a| a.shared)
				.flat_map(|a| a.agents.iter())
				.filter(|id| !disabled.contains(**id))
				.map(|id| id.to_string())
				.collect()
		},
		dry_run,
	};

	// 1. Refusals block the WHOLE plan. A partially applied repair is how a
	//    skill ends up readable from nowhere, so this runs before any write.
	if let Some((at, reason)) = plan.refusals().into_iter().next() {
		report.outcome = RepairOutcome::Refused {
			reason: describe(reason, &at.path),
			fix: fix_for(reason, &at.path, &plan.name),
		};
		return Ok(report);
	}

	// 2. Compare every fork BEFORE writing anything. A diverged fork must not
	//    be discovered halfway through, after the Master was already adopted.
	let forks: Vec<&Path> = plan
		.actions
		.iter()
		.filter(|a| a.action == ReferrerAction::CompareThenQuarantine)
		.map(|a| a.path.as_path())
		.collect();
	for fork in &forks {
		match compare(fork, &plan.master) {
			Comparison::Equal => {}
			Comparison::Diverged => {
				report.outcome = RepairOutcome::Refused {
					reason: format!(
						"{} holds content that differs from the master at {}",
						fork.display(),
						plan.master.display()
					),
					fix: format!(
						"both copies are preserved; aghub will not choose which one \
							to discard. Compare them first: `diff -r {} {}`. If you \
							choose to consolidate them, move the copy you do not want \
							aside and re-run `aghub-cli repair {}`",
						fork.display(),
						plan.master.display(),
						plan.name
					),
				};
				return Ok(report);
			}
			Comparison::Undecidable(why) => {
				// An unreadable file is permanent and deterministic — without a
				// named escape the user is wedged here forever.
				report.outcome = RepairOutcome::Refused {
					reason: format!(
						"cannot hash {} to compare it with the master: {why}",
						fork.display()
					),
					fix: format!(
						"make it readable, or move it aside yourself (`mv {} \
						 {}.bak`) and re-run `aghub-cli repair {}`",
						fork.display(),
						fork.display(),
						plan.name
					),
				};
				return Ok(report);
			}
		}
	}

	// 3. Master first. Copied, never renamed: a crash after this leaves the old
	//    real directory still serving the skill.
	let adopt = plan.adopts().map(|p| p.to_path_buf());
	if let Some(src) = &adopt {
		report.outcome = RepairOutcome::Migrated;
		// `!exists` scopes the cleanup below: only then is whatever sits at the
		// path after a failed copy THIS call's partial write. Never clean up a
		// pre-existing master — it may be the user's only intact copy.
		if !dry_run && !plan.master.exists() {
			crate::skills::linker::ensure_master_store_parent(&plan.master)
				.map_err(|e| io_err("create master store", e))?;
			if let Err(e) = Linker::copy_preserving_links(src, &plan.master) {
				// An empty master left behind would make every later run see
				// the intact slot as a diverged fork and refuse forever
				// (`a_failed_copy_leaves_no_empty_master_to_wedge_the_next_run`).
				let _ = std::fs::remove_dir_all(&plan.master);
				return Err(io_err("copy master out of the shared slot", e));
			}
		}
	}

	// 4. Private Referrers next: additive, so a crash here loses nothing.
	for action in &plan.actions {
		match action.action {
			ReferrerAction::Create | ReferrerAction::Relink => {}
			_ => continue,
		}
		if report.outcome == RepairOutcome::Conformant {
			report.outcome = RepairOutcome::Relinked;
		}
		report.referrers.push(action.path.clone());
		if dry_run {
			continue;
		}
		// Idempotent against a link left by a crashed run: unlink first, and
		// `unlink` uses `remove_dir`, never `remove_dir_all`, so it can only
		// detach a reparse point and never recurse into the Master.
		if Linker::is_link(&action.path) {
			Linker::unlink(&action.path)
				.map_err(|e| io_err("unlink stale referrer", e))?;
		}
		if let Some(parent) = action.path.parent() {
			std::fs::create_dir_all(parent)
				.map_err(|e| io_err("create referrer dir", e))?;
		}
		Linker::symlink(&plan.master, &action.path)
			.map_err(|e| io_err("create referrer", e))?;
	}

	// 5. The shared slot LAST — the only destructive step. See the module docs
	//    for why the temp link is created before the rename and not after.
	for action in &plan.actions {
		let is_swap = action.action == ReferrerAction::CompareThenQuarantine
			|| (adopt.as_deref() == Some(action.path.as_path()));
		if !is_swap {
			continue;
		}
		if action.action == ReferrerAction::CompareThenQuarantine
			&& report.outcome == RepairOutcome::Conformant
		{
			report.outcome = RepairOutcome::Reconciled;
		}
		let dest = quarantine_dir(&plan.master, &plan.name);
		report.quarantined = Some(dest.clone());
		report.referrers.push(action.path.clone());
		if dry_run {
			continue;
		}
		swap_slot(&action.path, &plan.master, &dest)?;
	}

	// 6. Compat-dir detaches LAST. Until step 4/5 really put a Referrer in the
	//    agent's own slot, the stale link here is the only thing still handing
	//    it the skill — a crash before this point leaves the agent reading it,
	//    which is the safe direction.
	for action in &plan.actions {
		if action.action != ReferrerAction::Unlink {
			continue;
		}
		// A dry run reports the PLAN, not a fresh disk read — it never calls
		// `Linker::unlink`, so re-probing here would just be a second opinion
		// with nothing behind it.
		if dry_run {
			if report.outcome == RepairOutcome::Conformant {
				report.outcome = RepairOutcome::Tidied;
			}
			report.unlinked.push(action.path.clone());
			continue;
		}
		// Re-checked at write time: three of the four compat-unlink guards
		// (link-only, resolves to this master or the adopt source, nobody
		// else's write slot) are answered fresh by `compat_unlink_permitted`,
		// and only a row it STILL permits is unlinked and reported.
		//
		// The FOURTH guard ("the write slot covers it afterwards") is decided
		// once in `plan_repair` and NOT re-asked: `RepairPlan` carries no
		// scope/root to re-classify the write slot. Known open gap — a non-aghub
		// actor breaking that slot mid-run leaves this unlinking the agent's
		// last link. See docs/history/core-repair-rename.md#compat-unlink-recheck-and-the-fourth-guard
		if !compat_unlink_permitted(
			&action.path,
			&plan.master,
			adopt.as_deref(),
			&plan.actions,
		)
		.map_err(|e| io_err("recheck compat referrer before unlink", e))?
		{
			continue;
		}
		// `unlink_reporting`, not `unlink` (which folds `NotFound` into
		// success): record only what THIS call removed. Residual, deliberate:
		// check and removal are two syscalls; closing that needs `unlinkat`
		// on a dir fd.
		let removed = Linker::unlink_reporting(&action.path)
			.map_err(|e| io_err("unlink stale compat referrer", e))?;
		if !removed {
			continue;
		}
		if report.outcome == RepairOutcome::Conformant {
			report.outcome = RepairOutcome::Tidied;
		}
		report.unlinked.push(action.path.clone());
	}

	Ok(report)
}

/// `.aghub/.quarantine/<name>/<stamp>/`.
///
/// Invisible to the store scan only because `top_level_skill_dirs` is one
/// level deep and needs a root `SKILL.md` — any new `.aghub` enumerator must
/// skip it too (`is_store_bookkeeping`).
fn quarantine_dir(master: &Path, name: &str) -> PathBuf {
	master
		.parent()
		.unwrap_or(master)
		.join(".quarantine")
		.join(name)
		.join(stamp())
}

/// Temp-link, rename away, rename over. See the module docs.
fn swap_slot(slot: &Path, master: &Path, dest: &Path) -> Result<()> {
	let parent = slot.parent().ok_or_else(|| {
		ConfigError::InvalidConfig(format!(
			"referrer {} has no parent directory",
			slot.display()
		))
	})?;
	let name = slot.file_name().and_then(|n| n.to_str()).ok_or_else(|| {
		ConfigError::InvalidConfig(format!(
			"referrer {} has no file name",
			slot.display()
		))
	})?;
	let temp = parent.join(format!(".{name}.aghub-migrating"));

	// Step 1: prove link creation works here BEFORE anything destructive.
	Linker::unlink(&temp).map_err(|e| io_err("clear stale temp link", e))?;
	Linker::symlink(master, &temp)
		.map_err(|e| io_err("create temp referrer link", e))?;

	// Step 2: move the fork aside. A same-instant stamp collision is a hard
	// error — reusing the directory would merge two different forks into one.
	if dest.exists() {
		return Err(ConfigError::InvalidConfig(format!(
			"quarantine {} already exists; refusing to merge two forks",
			dest.display()
		)));
	}
	if let Some(p) = dest.parent() {
		std::fs::create_dir_all(p)
			.map_err(|e| io_err("create quarantine dir", e))?;
	}
	if let Err(e) = std::fs::rename(slot, dest) {
		// Drop the temp link; the slot still holds the fork, which the next
		// run absorbs. No copy-then-remove fallback for EXDEV/sharing
		// violations: it doubles the window and can half-copy.
		let _ = Linker::unlink(&temp);
		return Err(io_err("move the fork into quarantine", e));
	}

	// Step 3: the temp link takes the real name. POSIX forbids renaming a
	// symlink over a NON-EMPTY directory, which is why step 2 had to run first.
	std::fs::rename(&temp, slot).map_err(|e| {
		let _ = Linker::unlink(&temp);
		io_err("move the referrer link into place", e)
	})?;
	Ok(())
}

fn describe(reason: &RefuseReason, at: &Path) -> String {
	match reason {
		RefuseReason::AliasedMaster => format!(
			"{} IS the master, reached through a symlinked parent",
			at.display()
		),
		RefuseReason::MasterIsLink => {
			"the store holds a link where it must hold a real directory"
				.to_string()
		}
		RefuseReason::MasterIsNotADir => {
			"something that is not a directory occupies the master path"
				.to_string()
		}
		RefuseReason::ReferrerIsNotADir => {
			format!("{} is neither a link nor a directory", at.display())
		}
		RefuseReason::MasterMissing => {
			"there is no master and nothing that may be adopted as one"
				.to_string()
		}
		RefuseReason::UnreadableCompatDir { path } => format!(
			"{} could not be read, so it is undecided whether it is safe to \
			 detach",
			path.display()
		),
		RefuseReason::GitTrackedSource { paths } => format!(
			"git tracks {}, so it is authored in place rather than installed \
			 — migrating would move it into the store and delete it from \
			 version control",
			paths
				.iter()
				.map(|p| p.display().to_string())
				.collect::<Vec<_>>()
				.join(", ")
		),
		RefuseReason::GitTrackingUndecided { path } => format!(
			"{} is inside a git repository, but `git` could not say whether it \
			 tracks the directory",
			path.display()
		),
	}
}

fn fix_for(reason: &RefuseReason, at: &Path, name: &str) -> String {
	match reason {
		RefuseReason::AliasedMaster => format!(
			"a parent of {} is a symlink; resolve that symlink (or move the \
			 store) so the master and the referrer are distinct paths",
			at.display()
		),
		RefuseReason::MasterIsLink | RefuseReason::MasterIsNotADir => format!(
			"replace the store entry with a real directory, then re-run \
			 `aghub-cli repair {name}`"
		),
		RefuseReason::ReferrerIsNotADir => format!(
			"move {} aside yourself, then re-run `aghub-cli repair {name}`",
			at.display()
		),
		RefuseReason::MasterMissing => format!(
			"nothing to repair from — install it again with `aghub-cli \
			 source sync <owner/repo> -a <agent>` (or `aghub-cli -a <agent> \
			 add skill --from <dir>`), or delete the dead referrer at {}",
			at.display()
		),
		RefuseReason::UnreadableCompatDir { path } => format!(
			"fix the permission on {} (or one of its parent directories), or \
			 move it aside if it is not yours to change or its mount is \
			 unreachable — repair proceeds once the path is readable or gone; \
			 then re-run `aghub-cli repair {name}`",
			path.display()
		),
		RefuseReason::GitTrackedSource { paths } => format!(
			"keep authoring it there, or migrate it deliberately: `git rm -r \
			 --cached {}` (and commit that), then re-run `aghub-cli repair \
			 {name}`",
			paths
				.iter()
				.map(|p| p.display().to_string())
				.collect::<Vec<_>>()
				.join(" ")
		),
		RefuseReason::GitTrackingUndecided { path } => format!(
			"make `git ls-files --error-unmatch {}` answer — install `git`, or \
			 repair the repository above it — then re-run `aghub-cli repair \
			 {name}`",
			path.display()
		),
	}
}

#[cfg(all(test, unix))]
mod tests {
	use super::*;
	use crate::models::ResourceScope;
	use crate::skills::shape::plan_repair;
	use std::fs;

	/// A project-scoped fixture: the store, the shared slot and every agent dir
	/// all resolve under one tempdir, so no test touches a real home.
	fn fixture() -> (tempfile::TempDir, PathBuf) {
		let tmp = tempfile::tempdir().unwrap();
		let root = tmp.path().canonicalize().unwrap();
		// A marker so project-root detection is satisfied.
		fs::create_dir_all(root.join(".claude")).unwrap();
		(tmp, root)
	}

	fn write_skill(dir: &Path, name: &str, body: &str) {
		fs::create_dir_all(dir).unwrap();
		fs::write(
			dir.join("SKILL.md"),
			format!("---\nname: {name}\ndescription: {body}\n---\n"),
		)
		.unwrap();
	}

	fn plan(root: &Path, name: &str, in_lock: bool) -> RepairPlan {
		plan_repair(ResourceScope::ProjectOnly, Some(root), name, in_lock, &[])
			.expect("project scope always names a store")
	}

	/// A NAMED repair of a name that is in no lock and on no disk is a typo, not
	/// a healthy layout: it must be refused, while a LOCKED name with nothing
	/// on disk stays `Conformant` (bulk worklists are lock names).
	#[test]
	fn a_named_repair_of_a_skill_that_exists_nowhere_is_refused() {
		let (_tmp, root) = fixture();
		for dry_run in [true, false] {
			let report = repair_skill(
				&WriteScope::project(&root),
				"no-such-skill",
				false,
				dry_run,
			)
			.unwrap();
			match &report.outcome {
				RepairOutcome::Refused { reason, fix } => {
					assert!(reason.contains("no-such-skill"), "{reason}");
					assert!(fix.contains("doctor"), "{fix}");
				}
				other => {
					panic!("dry_run={dry_run}: expected Refused: {other:?}")
				}
			}
		}
		assert!(!root.join(".aghub/no-such-skill").exists());
		assert!(!root.join(".claude/skills/no-such-skill").exists());

		let locked = repair_skill(
			&WriteScope::project(&root),
			"locked-gone",
			true,
			true,
		)
		.unwrap();
		assert!(matches!(locked.outcome, RepairOutcome::Conformant));
	}

	/// A copy that sits only in a read-only compat dir still EXISTS (agents
	/// read it), so the refusal must not claim the name is misspelled.
	#[test]
	fn a_skill_held_only_by_a_compat_dir_is_not_reported_as_not_found() {
		let (_tmp, root) = fixture();
		write_skill(&root.join(".clinerules/skills/foo"), "foo", "compat");
		let report =
			repair_skill(&WriteScope::project(&root), "foo", false, true)
				.unwrap();
		if let RepairOutcome::Refused { reason, .. } = &report.outcome {
			assert!(!reason.contains("no skill named"), "{reason}");
		}
	}

	#[test]
	fn creating_private_referrers_is_reported_as_a_repair() {
		let (_tmp, root) = fixture();
		let master = root.join(".aghub/demo");
		write_skill(&master, "demo", "shared");
		let shared = root.join(".agents/skills/demo");
		fs::create_dir_all(shared.parent().unwrap()).unwrap();
		Linker::symlink(&master, &shared).unwrap();
		let preview =
			repair_skill(&WriteScope::project(&root), "demo", true, true)
				.unwrap();
		assert_eq!(preview.outcome, RepairOutcome::Relinked);
		assert!(preview.referrers.contains(&root.join(".codex/skills/demo")));
		assert!(!root.join(".codex/skills/demo").exists());
		let committed =
			repair_skill(&WriteScope::project(&root), "demo", true, false)
				.unwrap();
		assert_eq!(committed.outcome, RepairOutcome::Relinked);
		assert_eq!(
			fs::canonicalize(root.join(".codex/skills/demo")).unwrap(),
			master
		);
		// GUARD 1's only remaining red light. This fixture is the state space
		// guard 4 cannot police: the shared entry resolves to the Master, so
		// `readers_of` grants to EVERY reader and all of them end up covered —
		// `compat_unlink_authorized` therefore says yes, nobody is stranded.
		// The one thing left stopping step 6 from deleting the referrer step 4
		// just created is "the path is nobody's write slot" (amp's, here).
		// Without this line, deleting guard 1 keeps the whole suite green while
		// a dozen project-scope agents silently lose the skill.
		assert!(
			Linker::is_link(&shared),
			"the shared slot is amp's write slot — the sweep must not detach \
			 the Referrer this same run relinked"
		);
	}

	/// The core migration: a real directory in the shared slot becomes the
	/// Master, and the slot becomes a link to it.
	#[test]
	fn migrating_adopts_the_shared_dir_and_leaves_a_link_behind() {
		let (_tmp, root) = fixture();
		let name = "demo";
		let slot = root.join(".agents").join("skills").join(name);
		write_skill(&slot, name, "legacy");

		let p = plan(&root, name, true);
		let report = execute_repair(&p, false).unwrap();

		assert_eq!(report.outcome, RepairOutcome::Migrated);
		let master = root.join(".aghub").join(name);
		assert!(
			master.join("SKILL.md").is_file(),
			"the master must hold the real bytes"
		);
		assert!(
			!Linker::is_link(&master),
			"the store must hold a REAL directory, never a link"
		);
		assert!(
			Linker::is_link(&slot),
			"the shared slot becomes an ordinary referrer"
		);
		assert_eq!(
			fs::canonicalize(&slot).unwrap(),
			fs::canonicalize(&master).unwrap(),
			"and it must resolve to the master"
		);
		// The old bytes are kept, not deleted: hash equality is npx-parity and
		// skips symlinks, .git and empty dirs, so "equal" is not "identical".
		let q = report.quarantined.unwrap();
		assert!(q.join("SKILL.md").is_file(), "the original is quarantined");
		assert!(
			q.starts_with(root.join(".aghub").join(".quarantine")),
			"quarantine lives inside the store, one level below the scan: {q:?}"
		);
	}

	/// THE POINT OF THE WHOLE CHANGE: migrating expands implicit reads into
	/// explicit per-agent Referrers.
	///
	/// Before, every agent read one shared directory, so a skill could not be
	/// granted or revoked per agent. Migration must hand each agent that reads
	/// the skill TODAY its own link — otherwise the Master just moves and every
	/// agent keeps reading it through the single shared slot, which buys the
	/// user nothing.
	///
	/// This is the case `master_exists` used to gate wrongly: during a migration
	/// the Master does not exist YET, so a one-pass plan created no Referrer at
	/// all. Collapse `will_have_master` back to `master_exists` in `plan_repair`
	/// and this goes red.
	#[test]
	fn migrating_gives_each_reader_its_own_referrer() {
		let (_tmp, root) = fixture();
		let name = "demo";
		let slot = root.join(".agents").join("skills").join(name);
		write_skill(&slot, name, "legacy");

		// Answered against the CURRENT layout, exactly as the CLI does.
		let readers = crate::skills::shape::readers_of(
			ResourceScope::ProjectOnly,
			Some(&root),
			name,
		);
		assert!(
			readers.contains(&"cursor"),
			"fixture premise: cursor must read the shared slot today, got 			 {readers:?}"
		);
		let p = plan_repair(
			ResourceScope::ProjectOnly,
			Some(&root),
			name,
			true,
			&readers,
		)
		.unwrap();
		execute_repair(&p, false).unwrap();

		let master = root.join(".aghub").join(name);
		let private = root.join(".cursor").join("skills").join(name);
		assert!(
			Linker::is_link(&private),
			"cursor read the skill through the shared slot, so migration owes 			 it an explicit referrer it can individually revoke"
		);
		assert_eq!(
			fs::canonicalize(&private).unwrap(),
			fs::canonicalize(&master).unwrap()
		);
		// And nobody NEW was granted: an agent that could not read it before
		// must not be handed it by a repair.
		assert!(
			!root.join(".windsurf").join("skills").join(name).exists(),
			"repair must not grant a skill to an agent nobody asked for"
		);
	}

	/// The migration path out of a read-only compatibility dir.
	///
	/// Antigravity reads `.agent/skills` (singular) but writes `.agents/skills`,
	/// so a skill an older release parked in the compat dir audits as `withheld`
	/// in `doctor --verify-links` and cannot be removed for antigravity alone.
	/// Both are documented costs — this pins the way OUT of them, because the
	/// descriptor comment tells the user to relink and a comment is not a test.
	///
	/// `repair` never sees the compat dir: `candidate_referrers` is built from
	/// WRITE dirs only. What carries the fact across is `readers_of`, which asks
	/// the READ paths — so the compat dir is why antigravity lands in `grant_to`
	/// and its absent write slot is planned `Create` instead of `Leave`.
	/// Drop `.agent/skills` from antigravity's read paths and this goes red at
	/// the `readers` premise.
	#[test]
	fn repair_relinks_a_skill_stranded_in_a_read_only_compat_dir() {
		let (_tmp, root) = fixture();
		let name = "demo";
		// The shape a user upgrading actually has: the Master in the store and
		// a Referrer in the dir aghub now only READS.
		let master = root.join(".aghub").join(name);
		write_skill(&master, name, "stranded");
		let compat = root.join(".clinerules").join("skills");
		fs::create_dir_all(&compat).unwrap();
		Linker::symlink(&master, &compat.join(name)).unwrap();

		let readers = crate::skills::shape::readers_of(
			ResourceScope::ProjectOnly,
			Some(&root),
			name,
		);
		assert!(
			readers.contains(&"cline"),
			"the compat dir is what makes antigravity a reader, got {readers:?}"
		);

		let write_slot = root.join(".cline").join("skills").join(name);
		assert!(
			!write_slot.exists(),
			"fixture premise: the write slot must start empty"
		);

		let p = plan_repair(
			ResourceScope::ProjectOnly,
			Some(&root),
			name,
			true,
			&readers,
		)
		.unwrap();
		execute_repair(&p, false).unwrap();

		assert!(
			Linker::is_link(&write_slot),
			"repair owes the stranded reader a Referrer in the dir it WRITES"
		);
		assert_eq!(
			fs::canonicalize(&write_slot).unwrap(),
			fs::canonicalize(&master).unwrap()
		);
		// The compat Referrer is now a DUPLICATE of the write slot, and repair
		// detaches it. This assertion used to be its opposite ("left exactly as
		// found"), which was right while nothing else served the skill and
		// wrong the moment the write slot did: the leftover keeps the agent
		// reading the skill from two places, so `remove for this agent alone`
		// can never take anything away and refuses forever. That is the
		// antigravity bug this pair of tests exists for.
		assert!(
			compat.join(name).symlink_metadata().is_err(),
			"the stale compat Referrer must be detached once the write slot \
			 serves the same Master"
		);
		assert!(
			master.join("SKILL.md").is_file(),
			"detaching a Referrer must never touch the Master"
		);
	}

	/// A disabled agent's compat dir is not aghub's to tidy either: the same
	/// layout as the test below, with cline off, plans no detach at all.
	#[test]
	fn a_disabled_agents_stale_compat_referrer_is_left_in_place() {
		let (_tmp, root) = fixture();
		let name = "demo";
		let master = root.join(".aghub").join(name);
		write_skill(&master, name, "shared");
		let write_slot = root.join(".cline").join("skills").join(name);
		fs::create_dir_all(write_slot.parent().unwrap()).unwrap();
		Linker::symlink(&master, &write_slot).unwrap();
		let compat = root.join(".clinerules").join("skills").join(name);
		fs::create_dir_all(compat.parent().unwrap()).unwrap();
		Linker::symlink(&master, &compat).unwrap();

		let _off = crate::agent_settings::test_override::disable(&["cline"]);
		let p = plan(&root, name, true);
		assert!(
			!p.actions.iter().any(|a| a.path == compat
				&& a.action == crate::skills::shape::ReferrerAction::Unlink),
			"repair must not unlink inside a disabled agent's dir: {:?}",
			p.actions
		);
		assert!(p.is_noop());
	}

	/// The "still shared by" line names only managed agents.
	#[test]
	fn fused_omits_a_disabled_agent() {
		let (_tmp, root) = fixture();
		let name = "demo";
		write_skill(&root.join(".aghub").join(name), name, "shared");
		let fused = |root: &Path| {
			execute_repair(&plan(root, name, true), true).unwrap().fused
		};
		assert!(fused(&root).contains(&"amp".to_string()), "fixture premise");
		let _off = crate::agent_settings::test_override::disable(&["amp"]);
		assert!(!fused(&root).contains(&"amp".to_string()));
	}

	/// The compat-dir sweep fires when the write slot ALREADY serves the skill.
	///
	/// The sibling test above covers guard 3's `Create` branch (an empty write
	/// slot this run fills). This is the `Leave` branch: nothing else about the
	/// skill is wrong, so the whole repair IS the detach — which is why it needs
	/// an outcome of its own instead of reporting `conformant` after a write.
	#[test]
	fn a_stale_compat_referrer_is_detached_once_the_write_slot_is_conformant() {
		let (_tmp, root) = fixture();
		let name = "demo";
		let master = root.join(".aghub").join(name);
		write_skill(&master, name, "shared");
		// antigravity PROJECT pair: `.agents/skills` writes, `.agent/skills` is
		// read-only compat.
		let write_slot = root.join(".cline").join("skills").join(name);
		fs::create_dir_all(write_slot.parent().unwrap()).unwrap();
		Linker::symlink(&master, &write_slot).unwrap();
		let compat = root.join(".clinerules").join("skills").join(name);
		fs::create_dir_all(compat.parent().unwrap()).unwrap();
		Linker::symlink(&master, &compat).unwrap();

		let p = plan(&root, name, true);
		let row = p
			.actions
			.iter()
			.find(|a| a.path == compat)
			.expect("the compat Referrer must be planned");
		assert_eq!(
			row.action,
			crate::skills::shape::ReferrerAction::Unlink,
			"a duplicate the write slot already covers is a detach"
		);

		let report = execute_repair(&p, false).unwrap();
		assert_eq!(report.outcome, RepairOutcome::Tidied);
		assert_eq!(report.unlinked, vec![compat.clone()]);
		assert!(
			compat.symlink_metadata().is_err(),
			"the duplicate must be gone"
		);
		assert!(
			Linker::is_link(&write_slot),
			"the agent must still reach the skill through its own slot"
		);
		assert!(
			master.join("SKILL.md").is_file(),
			"a detach must never touch the Master"
		);
	}

	/// A MIGRATION detaches the stale compat Referrer in the SAME run.
	///
	/// The exact sequence a real upgrader hits, and the one that made this
	/// whole change necessary: content in the shared slot, no store, and a
	/// Referrer an older release left in a dir the agent now only reads. It
	/// took TWO `repair` runs before guard 2 learned about the adopt source —
	/// the first reported `migrated` while the compat link stayed, so the
	/// agent kept reading the skill twice over and its toggle went on refusing
	/// after a run that said it was fixed.
	#[test]
	fn a_migration_detaches_the_compat_referrer_in_the_same_run() {
		let (_tmp, root) = fixture();
		let name = "demo";
		// Pre-2.18: the bytes live in the shared slot, `.aghub` does not exist.
		let slot = root.join(".agents").join("skills").join(name);
		write_skill(&slot, name, "pre-2.18");
		let compat = root.join(".clinerules").join("skills").join(name);
		fs::create_dir_all(compat.parent().unwrap()).unwrap();
		Linker::symlink(&slot, &compat).unwrap();

		let readers = crate::skills::shape::readers_of(
			ResourceScope::ProjectOnly,
			Some(&root),
			name,
		);
		let p = plan_repair(
			ResourceScope::ProjectOnly,
			Some(&root),
			name,
			true,
			&readers,
		)
		.unwrap();
		let report = execute_repair(&p, false).unwrap();

		assert_eq!(report.outcome, RepairOutcome::Migrated);
		assert_eq!(
			report.unlinked,
			vec![compat.clone()],
			"one run must migrate AND detach"
		);
		assert!(
			compat.symlink_metadata().is_err(),
			"the stale compat Referrer must be gone after ONE run"
		);
		// The shared slot is the only way in for every agent that writes
		// nowhere else — it becomes a link, never a casualty of the sweep.
		assert!(
			Linker::is_link(&slot),
			"the shared slot must survive as an ordinary Referrer"
		);
		let master = root.join(".aghub").join(name);
		assert_eq!(
			fs::canonicalize(&slot).unwrap(),
			fs::canonicalize(&master).unwrap()
		);
		assert!(
			root.join(".agents").join("skills").join(name).exists(),
			"and it must still resolve"
		);
	}

	/// A compat entry that changed between planning and writing (npx's
	/// `cleanAndCreateDirectory`, or a user replacing the stale link by hand)
	/// is never recorded as `Tidied` / `report.unlinked`: the write-time step
	/// rechecks before reporting.
	/// See docs/history/core-repair-rename.md#compat-unlink-recheck-and-the-fourth-guard
	#[test]
	fn a_compat_entry_that_changed_since_planning_is_never_reported_as_unlinked(
	) {
		let (_tmp, root) = fixture();
		let name = "demo";
		let master = root.join(".aghub").join(name);
		write_skill(&master, name, "shared");
		let write_slot = root.join(".cline").join("skills").join(name);
		fs::create_dir_all(write_slot.parent().unwrap()).unwrap();
		Linker::symlink(&master, &write_slot).unwrap();
		let compat = root.join(".clinerules").join("skills").join(name);
		fs::create_dir_all(compat.parent().unwrap()).unwrap();
		Linker::symlink(&master, &compat).unwrap();

		let p = plan(&root, name, true);
		assert_eq!(
			p.actions
				.iter()
				.find(|a| a.path == compat)
				.map(|a| a.action.clone()),
			Some(crate::skills::shape::ReferrerAction::Unlink),
			"fixture premise: the compat referrer must be planned for detach"
		);

		// Between planning and writing, the compat slot stopped being a
		// link — exactly what npx's `cleanAndCreateDirectory` does, or a user
		// manually replacing it. `execute_repair` must re-observe this, not
		// trust the plan it was handed.
		fs::remove_file(&compat).unwrap();
		write_skill(&compat, name, "content that appeared after planning");

		let report = execute_repair(&p, false).unwrap();

		assert!(
			!report.unlinked.contains(&compat),
			"a row that was never actually removed must not be reported as \
			 unlinked: {:?}",
			report.unlinked
		);
		assert!(
			compat.join("SKILL.md").is_file(),
			"a real directory must never be deleted by the compat-detach step"
		);
	}

	/// The three things the sweep must NEVER take. Each is its own way to lose
	/// a skill, and each guard was written against a real failure:
	///  * a real DIRECTORY may hold the only copy of bytes aghub never installed
	///  * a link pointing somewhere else is not ours to move (D5 of
	///    `.scratch/aghub-skill-store/spec.md`)
	///  * `.agents/skills` is codex's second READ dir and the only WRITE slot
	///    for every other agent whose descriptor writes there — sweeping it
	///    revokes the skill for all of them.
	#[test]
	fn the_compat_sweep_never_takes_what_it_must_not() {
		// (a) a real directory in the compat dir
		let (_tmp, root) = fixture();
		let name = "demo";
		let master = root.join(".aghub").join(name);
		write_skill(&master, name, "shared");
		let write_slot = root.join(".cline").join("skills").join(name);
		fs::create_dir_all(write_slot.parent().unwrap()).unwrap();
		Linker::symlink(&master, &write_slot).unwrap();
		let compat = root.join(".clinerules").join("skills").join(name);
		write_skill(&compat, name, "bytes aghub never installed");

		let p = plan(&root, name, true);
		assert!(
			!p.actions.iter().any(|a| a.path == compat
				&& a.action == crate::skills::shape::ReferrerAction::Unlink),
			"a real directory is never a detach: {:?}",
			p.actions
		);

		// (b) a link pointing somewhere that is not this Master
		let (_tmp2, root) = fixture();
		let master = root.join(".aghub").join(name);
		write_skill(&master, name, "shared");
		let elsewhere = root.join("elsewhere");
		write_skill(&elsewhere, name, "someone else's");
		let write_slot = root.join(".cline").join("skills").join(name);
		fs::create_dir_all(write_slot.parent().unwrap()).unwrap();
		Linker::symlink(&master, &write_slot).unwrap();
		let foreign = root.join(".clinerules").join("skills").join(name);
		fs::create_dir_all(foreign.parent().unwrap()).unwrap();
		Linker::symlink(&elsewhere, &foreign).unwrap();

		let p = plan(&root, name, true);
		assert!(
			!p.actions.iter().any(|a| a.path == foreign
				&& a.action == crate::skills::shape::ReferrerAction::Unlink),
			"a link to somebody else's content is never a detach: {:?}",
			p.actions
		);

		// (c) the SHARED slot, reachable as a read-only dir for an agent that
		//     has its own private one
		let (_tmp3, root) = fixture();
		let master = root.join(".aghub").join(name);
		write_skill(&master, name, "shared");
		let shared = root.join(".agents").join("skills").join(name);
		fs::create_dir_all(shared.parent().unwrap()).unwrap();
		Linker::symlink(&master, &shared).unwrap();
		let codex = root.join(".codex").join("skills").join(name);
		fs::create_dir_all(codex.parent().unwrap()).unwrap();
		Linker::symlink(&master, &codex).unwrap();

		let p = plan(&root, name, true);
		assert!(
			!p.actions.iter().any(|a| a.path == shared
				&& a.action == crate::skills::shape::ReferrerAction::Unlink),
			"the shared slot is its readers' only slot, never a detach: {:?}",
			p.actions
		);

		// (d) the compat link is the agent's ONLY Referrer and this run is not
		//     granting it one (`grant_to` is empty here). Detaching would leave
		//     the agent unable to read a skill it reads today — the exact
		//     pre-2.18 shape `repair` exists to rescue, not to finish off.
		let (_tmp4, root) = fixture();
		let master = root.join(".aghub").join(name);
		write_skill(&master, name, "shared");
		let only = root.join(".clinerules").join("skills").join(name);
		fs::create_dir_all(only.parent().unwrap()).unwrap();
		Linker::symlink(&master, &only).unwrap();
		assert!(
			!root.join(".agents").join("skills").join(name).exists(),
			"fixture premise: the write slot must start empty"
		);

		let p = plan(&root, name, true);
		assert!(
			!p.actions.iter().any(|a| a.path == only
				&& a.action == crate::skills::shape::ReferrerAction::Unlink),
			"an uncovered write slot must not let the only Referrer be taken: \
			 {:?}",
			p.actions
		);
	}

	/// An unreadable compat dir REFUSES even when no write slot is covered.
	///
	/// Pins the probe running before the coverage check: with nothing covered
	/// the run must not report `ok` with the stale referrer in place.
	/// See docs/history/core-skills-shape.md#compat-sweep-skipped-the-unreadable-probe
	#[cfg(unix)]
	#[test]
	fn an_unreadable_compat_dir_refuses_even_with_no_covered_slot() {
		use std::os::unix::fs::PermissionsExt;
		let (_tmp, root) = fixture();
		let name = "demo";
		let master = root.join(".aghub").join(name);
		write_skill(&master, name, "master");
		// Nothing is granted anywhere: no write slot, and `grant_to` empty, so
		// every candidate row is `Leave` and NOTHING is covered.
		let compat_dir = root.join(".clinerules").join("skills");
		fs::create_dir_all(&compat_dir).unwrap();
		Linker::symlink(&master, &compat_dir.join(name)).unwrap();
		fs::set_permissions(&compat_dir, fs::Permissions::from_mode(0o000))
			.unwrap();
		let enforced = fs::read_dir(&compat_dir).is_err();

		let p = plan(&root, name, true);
		fs::set_permissions(&compat_dir, fs::Permissions::from_mode(0o755))
			.unwrap();
		if !enforced {
			eprintln!("skip: perms not enforced (root)");
			return;
		}

		assert!(
			p.refusals().iter().any(|(_, reason)| matches!(
				reason,
				RefuseReason::UnreadableCompatDir { .. }
			)),
			"a dir that might hold a referrer and cannot be read must refuse, \
			 not report the skill conformant: {:?}",
			p.actions
		);
	}

	/// A name collision must not turn `repair` into an unanswerable question.
	///
	/// The end-to-end shape behind `SkillShape::ForeignDir`: an agent groups
	/// its OWN skills under a category directory whose name happens to match a
	/// skill aghub manages. That used to classify as a forked copy, hash
	/// differently (of course it does — it is a different thing) and refuse
	/// with `fix: compare them, then keep the one you want`. Following that
	/// advice moves the agent's whole skill collection aside.
	#[test]
	fn a_name_colliding_category_dir_is_left_alone_not_refused() {
		let (_tmp, root) = fixture();
		let name = "research";
		let master = root.join(".aghub").join(name);
		write_skill(&master, name, "the skill aghub manages");
		// Every other agent is linked correctly; only this one collides.
		let claude = root.join(".claude").join("skills").join(name);
		fs::create_dir_all(claude.parent().unwrap()).unwrap();
		Linker::symlink(&master, &claude).unwrap();
		// The collision: a category directory with sub-skills and no root
		// SKILL.md, exactly the `~/.hermes/skills/research/` layout. Staged in
		// a PROJECT-scope private dir, which needs no real home; hermes's own
		// dir is global-only and runs the same two functions on the same state.
		let category = root.join(".grok").join("skills").join(name);
		fs::create_dir_all(category.join("arxiv")).unwrap();
		fs::write(category.join("DESCRIPTION.md"), "grouped skills\n").unwrap();
		fs::write(
			category.join("arxiv").join("SKILL.md"),
			"---\nname: arxiv\ndescription: d\n---\n",
		)
		.unwrap();

		let p = plan(&root, name, true);
		let row = p
			.actions
			.iter()
			.find(|a| a.path == category)
			.expect("the colliding slot must still be reported");
		assert_eq!(row.shape, crate::skills::shape::SkillShape::ForeignDir);
		assert_eq!(
			row.action,
			crate::skills::shape::ReferrerAction::LeaveForeign
		);
		assert!(
			p.refusals().is_empty(),
			"a name collision is not a decision the user can make: {:?}",
			p.refusals()
		);
		assert!(
			p.is_noop(),
			"nothing to do here, so the migration banner must stop asking"
		);

		let report = execute_repair(&p, false).unwrap();
		assert_eq!(report.outcome, RepairOutcome::Conformant);
		// The whole point: somebody else's fourteen skills stay where they are.
		assert!(
			category.join("DESCRIPTION.md").is_file(),
			"the category directory must be untouched"
		);
		assert!(
			category.join("arxiv").join("SKILL.md").is_file(),
			"and so must every skill inside it"
		);
		assert!(report.quarantined.is_none(), "nothing may be quarantined");
	}

	/// A diverged fork must leave the disk EXACTLY as it found it. This is the
	/// test that fails if the hash comparison is ever moved after the adopt.
	#[test]
	fn a_diverged_fork_is_refused_without_writing_anything() {
		let (_tmp, root) = fixture();
		let name = "demo";
		let master = root.join(".aghub").join(name);
		write_skill(&master, name, "master content");
		let slot = root.join(".agents").join("skills").join(name);
		write_skill(&slot, name, "npx wrote something else");

		let before = fs::read_to_string(slot.join("SKILL.md")).unwrap();
		let p = plan(&root, name, true);
		let report = execute_repair(&p, false).unwrap();

		match &report.outcome {
			RepairOutcome::Refused { reason, fix } => {
				assert!(reason.contains("differs"), "{reason}");
				assert!(
					fix.contains("diff -r"),
					"a refusal must read as an instruction: {fix}"
				);
				assert!(fix.contains("both copies are preserved"), "{fix}");
			}
			other => panic!("expected a refusal, got {other:?}"),
		}
		assert_eq!(
			fs::read_to_string(slot.join("SKILL.md")).unwrap(),
			before,
			"nothing may be written when the comparison refuses"
		);
		assert!(report.quarantined.is_none());
		assert!(
			!root.join(".aghub").join(".quarantine").exists(),
			"no quarantine dir may be created by a refused run"
		);
	}

	/// Hash-equal fork: quarantine it and restore the link.
	#[test]
	fn an_identical_fork_is_reconciled() {
		let (_tmp, root) = fixture();
		let name = "demo";
		let master = root.join(".aghub").join(name);
		write_skill(&master, name, "same");
		let slot = root.join(".agents").join("skills").join(name);
		write_skill(&slot, name, "same");

		let report = execute_repair(&plan(&root, name, true), false).unwrap();

		assert_eq!(report.outcome, RepairOutcome::Reconciled);
		assert!(Linker::is_link(&slot));
		assert_eq!(
			fs::canonicalize(&slot).unwrap(),
			fs::canonicalize(&master).unwrap()
		);
		assert!(report.quarantined.unwrap().join("SKILL.md").is_file());
	}

	/// A dry run decides everything and writes nothing — including running the
	/// hash comparison, so a preview reporting `reconciled` is a commit that
	/// will reconcile.
	#[test]
	fn a_dry_run_reaches_the_same_verdict_and_writes_nothing() {
		let (_tmp, root) = fixture();
		let name = "demo";
		let master = root.join(".aghub").join(name);
		write_skill(&master, name, "same");
		let slot = root.join(".agents").join("skills").join(name);
		write_skill(&slot, name, "same");

		let preview = execute_repair(&plan(&root, name, true), true).unwrap();
		assert_eq!(preview.outcome, RepairOutcome::Reconciled);
		assert!(preview.dry_run);
		assert!(!Linker::is_link(&slot), "a dry run must not touch the slot");
		assert!(!root.join(".aghub").join(".quarantine").exists());

		// And the commit agrees.
		let commit = execute_repair(&plan(&root, name, true), false).unwrap();
		assert_eq!(commit.outcome, preview.outcome);
	}

	/// Seed a project lock naming `names`, which is `repair_all`'s worklist.
	///
	/// The shape is copied from a shipped fixture rather than hand-minimized:
	/// the lock read paths fail CLOSED here, so a lock missing a required field
	/// makes the command bail while READING and every assertion below passes
	/// with the code under test never reached.
	fn seed_project_lock(root: &Path, names: &[&str]) {
		let entries: Vec<String> = names
			.iter()
			.map(|n| {
				// The PROJECT lock schema (version 1, `computedHash`), copied
				// from the shipped `cli_tests.rs` fixtures. The global lock's
				// shape is different and reading it here fails closed, which
				// makes every assertion below pass without running the code.
				format!(
					r#""{n}":{{"source":"o/r","sourceType":"github","computedHash":"deadbeef"}}"#
				)
			})
			.collect();
		fs::write(
			root.join("skills-lock.json"),
			format!(r#"{{"version":1,"skills":{{{}}}}}"#, entries.join(",")),
		)
		.unwrap();
	}

	fn perms_enforced(path: &Path) -> bool {
		use std::os::unix::fs::PermissionsExt;
		let probe = path.join(".perm-probe");
		fs::create_dir_all(&probe).unwrap();
		let orig = fs::metadata(&probe).unwrap().permissions();
		fs::set_permissions(&probe, fs::Permissions::from_mode(0o000)).unwrap();
		let denied = fs::read_dir(&probe).is_err();
		fs::set_permissions(&probe, orig).unwrap();
		fs::remove_dir_all(&probe).unwrap();
		denied
	}

	/// THE regression this batch seam exists for.
	///
	/// Both surfaces used to `?` out of the loop, so an EACCES on one skill
	/// threw away the report for every skill already migrated — observed live
	/// through the API route: HTTP 500, 29 of 50 migrated, and a response that
	/// mentioned none of them. The batch must answer with a COMPLETE receipt.
	///
	/// Revert the `Err(e) => reports.push(failed_report(...))` arm back to `?`
	/// and this goes red on the row count.
	#[test]
	fn one_failing_skill_does_not_throw_away_the_rest_of_the_batch() {
		use std::os::unix::fs::PermissionsExt;
		let (_tmp, root) = fixture();
		if !perms_enforced(&root) {
			eprintln!("skip: perms not enforced (root)");
			return;
		}
		for n in ["alpha", "beta", "gamma"] {
			write_skill(&root.join(".agents").join("skills").join(n), n, "x");
		}
		seed_project_lock(&root, &["alpha", "beta", "gamma"]);
		// `beta`'s own directory cannot be read, so its copy fails with EACCES.
		let beta = root.join(".agents").join("skills").join("beta");
		fs::set_permissions(&beta, fs::Permissions::from_mode(0o000)).unwrap();

		let reports =
			repair_all(&WriteScope::project(&root), None, false).unwrap();

		fs::set_permissions(&beta, fs::Permissions::from_mode(0o755)).unwrap();

		assert_eq!(
			reports.len(),
			3,
			"every skill must be accounted for, not just the ones before the \
			 failure: {reports:?}"
		);
		let by_name = |n: &str| {
			reports
				.iter()
				.find(|r| r.name == n)
				.unwrap()
				.outcome
				.clone()
		};
		assert!(
			matches!(by_name("beta"), RepairOutcome::Failed { .. }),
			"the unreadable skill is reported as failed, got {:?}",
			by_name("beta")
		);
		assert_eq!(by_name("alpha"), RepairOutcome::Migrated);
		assert_eq!(by_name("gamma"), RepairOutcome::Migrated);
		// And the two that worked really did land on disk — a row saying
		// "migrated" that wrote nothing would be the worse bug.
		for n in ["alpha", "gamma"] {
			assert!(
				root.join(".aghub").join(n).join("SKILL.md").is_file(),
				"{n} must have a real master"
			);
			assert!(
				Linker::is_link(&root.join(".agents").join("skills").join(n)),
				"{n}'s shared slot must have become a referrer"
			);
		}
	}

	/// A failed copy must not leave the empty master it created behind.
	///
	/// Observed live: the next run compared the intact slot against the empty
	/// store entry, called it a diverged fork and refused FOREVER — the user
	/// had to `rm -rf` the store entry by hand before anything worked again.
	/// So the second run must get the skill migrated, not refuse it.
	///
	/// Delete the `remove_dir_all` in step 3 and this goes red on the re-run.
	#[test]
	fn a_failed_copy_leaves_no_empty_master_to_wedge_the_next_run() {
		use std::os::unix::fs::PermissionsExt;
		let (_tmp, root) = fixture();
		if !perms_enforced(&root) {
			eprintln!("skip: perms not enforced (root)");
			return;
		}
		let name = "demo";
		let slot = root.join(".agents").join("skills").join(name);
		write_skill(&slot, name, "legacy");
		seed_project_lock(&root, &[name]);
		fs::set_permissions(&slot, fs::Permissions::from_mode(0o000)).unwrap();

		let first =
			repair_all(&WriteScope::project(&root), None, false).unwrap();
		assert!(matches!(first[0].outcome, RepairOutcome::Failed { .. }));
		assert!(
			!root.join(".aghub").join(name).exists(),
			"a failed copy must clean up the master it created, or the next \
			 run sees a diverged fork that can never be resolved"
		);

		// Now make it readable and re-run: repair is idempotent, so this must
		// simply migrate.
		fs::set_permissions(&slot, fs::Permissions::from_mode(0o755)).unwrap();
		let second =
			repair_all(&WriteScope::project(&root), None, false).unwrap();
		assert_eq!(
			second[0].outcome,
			RepairOutcome::Migrated,
			"the re-run must migrate, not refuse: {:?}",
			second[0].outcome
		);
		assert!(root.join(".aghub").join(name).join("SKILL.md").is_file());
	}

	/// A bulk run stays quiet about the skills that were already correct, and
	/// re-running the whole batch is a no-op rather than a second migration.
	#[test]
	fn a_second_bulk_run_reports_nothing_left_to_do() {
		let (_tmp, root) = fixture();
		for n in ["alpha", "beta"] {
			write_skill(&root.join(".agents").join("skills").join(n), n, "x");
		}
		seed_project_lock(&root, &["alpha", "beta"]);

		let first =
			repair_all(&WriteScope::project(&root), None, false).unwrap();
		assert_eq!(first.len(), 2);

		let second =
			repair_all(&WriteScope::project(&root), None, false).unwrap();
		assert!(
			second.is_empty(),
			"a conformant bulk re-run must say nothing, got {second:?}"
		);
		// And the quarantine did not grow a second copy.
		let q = root.join(".aghub").join(".quarantine").join("alpha");
		assert_eq!(
			fs::read_dir(&q).unwrap().count(),
			1,
			"a no-op re-run must not quarantine anything again"
		);
	}

	/// Re-running after a crashed repair must not wedge the skill: a private
	/// Referrer that already exists is repointed, never turned into a chain.
	#[test]
	fn creating_a_referrer_is_idempotent_against_a_stale_link() {
		let (_tmp, root) = fixture();
		let name = "demo";
		let master = root.join(".aghub").join(name);
		write_skill(&master, name, "m");
		let slot = root.join(".agents").join("skills").join(name);
		write_skill(&slot, name, "m");
		// What a crashed run leaves: a private referrer pointing at the SLOT
		// rather than the master. Re-running must not chain through it.
		let private = root.join(".claude").join("skills");
		fs::create_dir_all(&private).unwrap();
		std::os::unix::fs::symlink(&slot, private.join(name)).unwrap();

		execute_repair(&plan(&root, name, true), false).unwrap();

		let link = private.join(name);
		assert!(Linker::is_link(&link));
		assert_eq!(
			fs::read_link(&link).unwrap(),
			master,
			"a stale referrer must be repointed AT THE MASTER, not left as a \
			 chain through the slot"
		);
	}

	/// BUG #18: repair with scope GlobalOnly and a project_root given must not
	/// merge the project lock into `in_lock`. A skill listed only in the
	/// project lock must not be adopted as a global Master.
	#[test]
	fn global_repair_with_project_root_does_not_adopt_project_skill() {
		let _env = crate::skills::prune::test_lock::env_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());

		let fake_home_tmp = tempfile::tempdir().unwrap();
		let fake_home = fake_home_tmp.path().canonicalize().unwrap();
		let project_tmp = tempfile::tempdir().unwrap();
		let project_root = project_tmp.path().canonicalize().unwrap();

		let keys = [
			"HOME",
			"XDG_CONFIG_HOME",
			"XDG_STATE_HOME",
			"AGHUB_DATA_DIR",
		];
		let prev: Vec<(&'static str, Option<std::ffi::OsString>)> =
			keys.iter().map(|k| (*k, std::env::var_os(k))).collect();

		std::env::set_var("HOME", &fake_home);
		std::env::set_var("XDG_CONFIG_HOME", fake_home.join(".config"));
		std::env::set_var("XDG_STATE_HOME", fake_home.join(".local/state"));
		std::env::set_var(
			"AGHUB_DATA_DIR",
			fake_home.join(".local/share/aghub"),
		);

		struct EnvGuard(Vec<(&'static str, Option<std::ffi::OsString>)>);
		impl Drop for EnvGuard {
			fn drop(&mut self) {
				for (k, v) in &self.0 {
					match v {
						Some(val) => std::env::set_var(k, val),
						None => std::env::remove_var(k),
					}
				}
			}
		}
		let _guard = EnvGuard(prev);

		fs::create_dir_all(project_root.join(".claude")).unwrap();

		let name = "proj-only-skill";
		seed_project_lock(&project_root, &[name]);

		let global_slot = fake_home.join(".agents").join("skills").join(name);
		write_skill(&global_slot, name, "legacy-global-slot");

		let global_master = fake_home.join(".aghub").join(name);
		assert!(!global_master.exists());

		let reports = repair_all(&WriteScope::Global, None, false).unwrap();

		assert!(
			!global_master.exists(),
			"project skill must not be adopted as a global Master in ~/.aghub"
		);
		assert!(
			!Linker::is_link(&global_slot),
			"global shared slot must not be converted to a referrer link"
		);
		assert!(
			global_slot.is_dir(),
			"global shared slot must remain an untouched directory"
		);
		assert!(
			reports.is_empty(),
			"bulk global repair must not report any repair for project-only \
			 lock entry, got {reports:?}"
		);

		let named_reports =
			repair_all(&WriteScope::Global, Some(name), false).unwrap();

		assert!(
			!global_master.exists(),
			"named global repair must still not adopt project skill as a \
			 global Master"
		);
		assert!(
			!Linker::is_link(&global_slot),
			"global shared slot must remain an untouched directory after \
			 named repair"
		);
		assert!(global_slot.is_dir());
		assert!(
			!named_reports
				.iter()
				.any(|r| r.outcome == RepairOutcome::Migrated),
			"named repair must not migrate project-only skill into global \
			 master: {named_reports:?}"
		);
	}
}
