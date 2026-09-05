//! Apply a [`RepairPlan`]. The execution half of `shape.rs`.
//!
//! **The plan is the ONLY input.** Nothing here re-runs `classify_shape`: a
//! second classification between planning and writing is a second opinion, and
//! the two can disagree across the window (npx running concurrently is the
//! whole reason this module exists). If the plan says `AdoptAsMaster`, this
//! adopts — the caller re-plans if it wants a fresher view.
//!
//! **Ordering is crash-safety, not tidiness.** Every step is ordered so that a
//! process dying at ANY point leaves one of exactly two readable states: the old
//! real directory still serving the skill (nothing lost), or the Master present
//! with the Referrer still a real directory — which is npx's own shape, and the
//! repair policy already absorbs it. So there is no rollback and no receipt.
//!
//! The one destructive step is the shared-slot swap, and it goes LAST:
//!
//! 1. create a dot-prefixed temp link inside the slot dir — this proves link
//!    creation works on this filesystem BEFORE anything destructive, and the dot
//!    prefix keeps npx's `scanDir` and agent scanners off it;
//! 2. rename the old directory into `.aghub/.quarantine/<name>/<stamp>/`;
//! 3. rename the temp link over the real name.
//!
//! Doing 2 before 1 leaves a window where the skill is readable from NOWHERE,
//! and anything ending the process there (SIGKILL, ENOSPC, a Windows host where
//! both `symlink_dir` and the `mklink /J` fallback fail) leaves the *legal*
//! `Absent` shape — making a dead repair indistinguishable from a deliberate
//! withhold.
//!
//! Quarantine is nested `<name>/<stamp>/`, never flat `<name>-<stamp>`:
//! sanitized names contain hyphens, so the flat form cannot be split back.

use std::path::{Path, PathBuf};

use crate::errors::{ConfigError, Result};
use crate::skills::linker::Linker;
use crate::skills::shape::{ReferrerAction, RefuseReason, RepairPlan};

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
	/// Nothing written. `reason` says why, `fix` is the literal next command or
	/// path — a refused row must read as an instruction, not a diagnosis.
	Refused { reason: String, fix: String },
}

/// The result of one skill's repair.
#[derive(Debug, Clone, serde::Serialize)]
pub struct RepairReport {
	pub name: String,
	pub outcome: RepairOutcome,
	pub master: PathBuf,
	/// Referrers created or repointed.
	pub referrers: Vec<PathBuf>,
	/// Where a fork was moved, when one was.
	pub quarantined: Option<PathBuf>,
	/// True when the writes were withheld. A dry run walks the SAME branches —
	/// including the hash comparison that decides `Reconciled` vs `Refused` —
	/// so a preview that reports `reconciled` is a commit that will reconcile.
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

/// Apply `plan`. With `dry_run`, decides everything and writes nothing.
pub fn execute_repair(
	plan: &RepairPlan,
	dry_run: bool,
) -> Result<RepairReport> {
	let mut report = RepairReport {
		name: plan.name.clone(),
		outcome: RepairOutcome::Conformant,
		master: plan.master.clone(),
		referrers: Vec::new(),
		quarantined: None,
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
						"compare them, then keep the one you want: `diff -r \
						 {} {}`. Move the copy you do not want aside and \
						 re-run `aghub skills repair {}`",
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
						 {}.bak`) and re-run `aghub skills repair {}`",
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
		if !dry_run && !plan.master.exists() {
			if let Some(parent) = plan.master.parent() {
				std::fs::create_dir_all(parent)
					.map_err(|e| io_err("create master store", e))?;
			}
			Linker::copy_preserving_links(src, &plan.master)
				.map_err(|e| io_err("copy master out of the shared slot", e))?;
		}
	}

	// 4. Private Referrers next: additive, so a crash here loses nothing.
	for action in &plan.actions {
		match action.action {
			ReferrerAction::Create | ReferrerAction::Relink => {}
			_ => continue,
		}
		if action.action == ReferrerAction::Relink
			&& report.outcome == RepairOutcome::Conformant
		{
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

	Ok(report)
}

/// `.aghub/.quarantine/<name>/<stamp>/`.
///
/// Sits inside the store because `top_level_skill_dirs` is one level deep and
/// requires a root `SKILL.md`, so a dot-prefixed nested tree is invisible to the
/// store scan. That invisibility is a property of THAT function, not of the
/// layout — any new enumerator of `.aghub` has to keep the same depth.
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
		// Leave nothing half-done: the temp link is transient, so drop it and
		// report. The slot still holds the fork, which is a shape repair can
		// absorb on the next run.
		let _ = Linker::unlink(&temp);
		// EXDEV (quarantine on another filesystem) and Windows sharing
		// violations both land here. Copy-then-remove is NOT a safe fallback:
		// it doubles the window and can half-copy, so the honest answer is to
		// report and leave the fork where it is.
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
			 `aghub skills repair {name}`"
		),
		RefuseReason::ReferrerIsNotADir => format!(
			"move {} aside yourself, then re-run `aghub skills repair {name}`",
			at.display()
		),
		RefuseReason::MasterMissing => format!(
			"nothing to repair from — install it again with `aghub skills add \
			 <source> -a <agent>`, or delete the dead referrer at {}",
			at.display()
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

	/// The migration this whole change exists for: a real directory in the
	/// shared slot becomes the Master, and the slot becomes a link to it.
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
}
