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
	/// Nothing was wrong with the Master or the write slots; what this run did
	/// was detach stale Referrers from dirs the agent only READS. A distinct
	/// outcome because `Conformant` says "nothing written", and reporting a
	/// write as "already correct" is the same lie the removal side added
	/// `Partial` to stop telling.
	Tidied,
	/// Nothing written. `reason` says why, `fix` is the literal next command or
	/// path — a refused row must read as an instruction, not a diagnosis.
	Refused { reason: String, fix: String },
	/// The repair was attempted and the OS said no (EACCES, ENOSPC, a Windows
	/// sharing violation). Distinct from `Refused` on purpose: a refusal is a
	/// DECISION and the next run repeats it, whereas this is a transient state
	/// the next run absorbs — steps 4 and 5 fail after the master exists, so
	/// re-running reports the same skill as `Relinked` or `Reconciled`, not as
	/// this. Folding the two together would break dry-run parity, which
	/// promises a preview and its commit reach the same verdict.
	Failed { reason: String, fix: String },
}

/// The result of one skill's repair.
#[derive(Debug, Clone, serde::Serialize)]
pub struct RepairReport {
	pub name: String,
	/// The shape repair FOUND, alongside what it did about it. An agent
	/// choosing its next action should not need a second command to learn why
	/// the outcome was what it was.
	pub shape: Option<crate::skills::shape::SkillShape>,
	pub outcome: RepairOutcome,
	pub master: PathBuf,
	/// Referrers created or repointed.
	pub referrers: Vec<PathBuf>,
	/// Stale Referrers detached from read-only compat dirs. Separate from
	/// `referrers`: those are grants this run MADE, these are duplicates it
	/// took away, and folding them together would read as granting the skill
	/// twice.
	pub unlinked: Vec<PathBuf>,
	/// Where a fork was moved, when one was.
	pub quarantined: Option<PathBuf>,
	/// Agents that STILL share one directory after this repair — they have no
	/// private skills dir, so granting or revoking for one of them does it for
	/// all. The preview has to say so: a user who cannot see that codex remains
	/// fused does not know what the migration bought them.
	pub fused: Vec<String>,
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

/// Plan and apply a repair for ONE skill, under the interprocess mutation lock.
///
/// **This is the seam both surfaces call.** `execute_repair` below is the pure
/// applier and takes no lock; calling it directly from a surface skips the
/// guard, and hand-mirroring the lock across the CLI and the API route is the
/// "NEVER hand-mirror a mutating flow across surfaces" the root `AGENTS.md`
/// forbids.
///
/// The guard is taken BEFORE `plan_repair`, not just before the writes: the plan
/// IS the state read that decides the mutation, and a plan chosen outside the
/// lock is a view another process may already have invalidated. That matters
/// here more than almost anywhere — this module exists because npx rewrites
/// these very directories, and the hash-compare → rename window is exactly what
/// a concurrent `aghub skills add` of the same name would tear.
///
/// A dry run takes NO guard, same as every other verb: it decides and writes
/// nothing, so there is nothing to serialize.
///
/// `grant_to` is computed in here rather than passed in, so the "answered
/// against the layout as it stands, before anything moves" rule cannot be got
/// wrong by a caller — and so it is answered under the same lock as the plan.
///
/// `Ok(None)` when the scope names no single store (`Both`), matching
/// [`plan_repair`].
pub fn repair_skill(
	scope: crate::models::ResourceScope,
	project_root: Option<&Path>,
	name: &str,
	in_lock: bool,
	dry_run: bool,
) -> Result<Option<RepairReport>> {
	let _guard = if dry_run {
		None
	} else {
		Some(
			crate::skills::lock::mutation_guard(
				"skill repair",
				scope,
				project_root,
			)
			.map_err(|e| io_err("acquire the mutation lock", e))?,
		)
	};
	let grant_to = crate::skills::shape::readers_of(scope, project_root, name);
	let Some(plan) = crate::skills::shape::plan_repair(
		scope,
		project_root,
		name,
		in_lock,
		&grant_to,
	) else {
		return Ok(None);
	};
	execute_repair(&plan, dry_run).map(Some)
}

/// Repair EVERY skill the lock names at this scope (or just `name`).
///
/// **The one home for the batch.** The CLI and the API route each grew their own
/// copy of this loop — same worklist, same fail-closed lock read, same
/// "stay quiet about the conformant ones" rule — and they had already drifted:
/// the route took no outer bulk guard, so a fifty-skill desktop migration was
/// fifty independently racing mutations, which is exactly what the CLI's own
/// comment says must not happen. That is the "NEVER hand-mirror a mutating flow
/// across surfaces" rule in the root `AGENTS.md`; surfaces are adapters now.
///
/// **A failing skill does not abort the batch.** Both loops used `?`, so an
/// EACCES on skill 25 of 50 threw away the report for the 24 that had already
/// migrated — the disk was fine (every step is crash-safe and re-running is
/// idempotent) but the user was told nothing at all. Each skill's error becomes
/// a [`RepairOutcome::Failed`] row instead, so the answer is always a complete
/// receipt of what happened to all of them.
pub fn repair_all(
	scope: crate::models::ResourceScope,
	project_root: Option<&Path>,
	name: Option<&str>,
	dry_run: bool,
) -> Result<Vec<RepairReport>> {
	// ONE guard around the whole bulk run, not one per skill, and taken BEFORE
	// the lock read below — `crates/core/AGENTS.md`'s "guard before the
	// deciding read". The lock decides which directory may be ADOPTED as a
	// Master, so a snapshot taken outside the guard can authorize a swap the
	// current lock no longer permits: this run reads `demo` as locked, another
	// aghub deletes that entry and releases, npx drops a fresh real
	// `.agents/skills/demo` in place, and this run resumes with a stale
	// `in_lock = true` and adopts content nobody authorized. Dry runs take no
	// guard (they write nothing), so a PREVIEW still reads outside it —
	// deliberate, and the reason a preview is authority for nothing.
	//
	// It is also ONE guard for the whole batch: `repair_skill` takes its own
	// (reentrant per thread, so the inner acquire is free), but without this
	// outer one a fifty-skill migration is fifty independently racing
	// mutations another aghub could interleave halfway through.
	let _bulk_guard = if dry_run {
		None
	} else {
		Some(
			crate::skills::lock::mutation_guard(
				"skill repair (bulk)",
				scope,
				project_root,
			)
			.map_err(|e| io_err("acquire the mutation lock", e))?,
		)
	};

	// Fail CLOSED. The lock IS the worklist and it decides which directories may
	// be adopted as a Master, so an unreadable lock must not come back as
	// "nothing to repair" — that answer looks like success and the user would
	// believe they had migrated.
	let mut in_lock: std::collections::BTreeSet<String> =
		std::collections::BTreeSet::new();
	if matches!(scope, crate::models::ResourceScope::GlobalOnly) {
		in_lock.extend(
			skill::lock::read_global_lock_checked()
				.map_err(|e| io_err("read the global skill lock", e))?
				.skills
				.into_keys(),
		);
	}
	if let Some(root) = project_root {
		in_lock.extend(
			skill::lock::local::read_local_lock_checked(Some(root))
				.map_err(|e| io_err("read the project skill lock", e))?
				.skills
				.into_keys(),
		);
	}

	// A named skill is repaired whether or not the lock knows it — the lock only
	// decides ADOPTION, and refusing to look at an unlocked skill would leave the
	// user with no way to diagnose it.
	let worklist: Vec<String> = match name {
		Some(one) => vec![one.to_string()],
		None => in_lock.iter().cloned().collect(),
	};

	let mut reports = Vec::new();
	for skill_name in &worklist {
		match repair_skill(
			scope,
			project_root,
			skill_name,
			in_lock.contains(skill_name),
			dry_run,
		) {
			// `Ok(None)` = the scope names no single store; nothing to say.
			Ok(None) => continue,
			Ok(Some(report)) => {
				// In a bulk run, silence about the already-correct skills is the
				// point; a named one still reports so the user learns it was
				// fine.
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
/// Carries no master, no referrers and no quarantine path: none of them
/// happened, or happened only partly, and naming a path here would tell the user
/// a write landed. `fix` names re-running because repair is idempotent — the
/// next run picks the skill up in whatever state it now holds.
fn failed_report(name: &str, dry_run: bool, e: &ConfigError) -> RepairReport {
	RepairReport {
		name: name.to_string(),
		shape: None,
		outcome: RepairOutcome::Failed {
			reason: e.to_string(),
			fix: format!(
				"fix the cause above (most often a permission on the skill \
				 directory), then re-run `aghub skills repair {name}` — repair \
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
		// The shape of the directory this repair is ABOUT: the shared slot
		// where there is one, since that is the slot every non-conformant
		// shape shows up in.
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
		fused: plan
			.actions
			.iter()
			.filter(|a| a.shared)
			.flat_map(|a| a.agents.iter().map(|id| id.to_string()))
			.collect(),
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
		// The `!exists` guard is what scopes the cleanup below. It claims
		// exactly one thing and no more: the master did not exist at plan time
		// and does not exist now, so anything sitting at that path after a
		// failed copy is THIS call's own partial write. It is NOT a claim that
		// the path is ours in general — removing a master that pre-existed
		// would delete the user's only intact copy, which is why the cleanup
		// lives inside this branch and nowhere else.
		if !dry_run && !plan.master.exists() {
			if let Some(parent) = plan.master.parent() {
				std::fs::create_dir_all(parent)
					.map_err(|e| io_err("create master store", e))?;
			}
			if let Err(e) = Linker::copy_preserving_links(src, &plan.master) {
				// Without this the failed run leaves an EMPTY master behind,
				// and the next run compares the intact slot against it, calls
				// it a diverged fork and refuses forever — the user has to
				// `rm -rf` the store entry by hand before anything works.
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
		// Re-checked at write time, not trusted from plan time — but only
		// THREE of the compat-Referrer sweep's four guards (the ones actually
		// inside `compat_unlink_permitted`: link-only, resolves to this
		// master or the adopt source, nobody-else's-write-slot) live in this
		// recheck: the disk it was decided against can move in between, npx
		// rewriting the same directories or a user retargeting a link by
		// hand onto private content or a protected write directory, and this
		// call answers those three fresh. Only a
		// row this recheck STILL permits gets unlinked and reported;
		// recording the path BEFORE the check (as this used to) is how an
		// entry that was left untouched still reported itself removed.
		//
		// The FOURTH guard — "the write slot covers it afterwards" — is NOT
		// re-asked here. It lives in `plan_repair`'s "THE WRITE SLOT COVERS
		// IT AFTERWARDS" comment, decided once against the PLAN (not the
		// disk) before this loop ever runs, and `RepairPlan` carries no
		// `scope`/`project_root` for this call to re-derive a write slot's
		// path and re-classify it fresh. The gap that leaves: a `Leave` +
		// `Conformant` write slot the plan is trusting as coverage could, in
		// the narrow window between that plan-time read and this loop
		// running (real writes in steps 3-5 take measurable time; nothing
		// aghub's own mutation lock excludes runs here, since the lock only
		// serializes aghub against aghub), be broken by something outside
		// aghub — and this recheck would still detach the compat referrer
		// that was the agent's only surviving link. Left unclosed this round:
		// closing it means threading `scope`/`project_root` onto `RepairPlan`
		// (or into this fn) so each `Unlink` row's covering write slot(s) can
		// be re-classified here, which is more plumbing than the risk (an
		// external actor racing inside one locked `execute_repair` call)
		// currently buys back.
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
		// `unlink_reporting`, not `unlink`: the latter folds `NotFound` into
		// success so step 4 can be idempotent, and a RECEIPT must not inherit
		// that. An entry another process removed between the recheck above and
		// this call came back `Ok(())` and was then reported as unlinked by
		// THIS run — a removal attributed to the wrong actor. Nothing is
		// recorded unless this call is what removed it.
		//
		// Residual, deliberately left: the check and the removal are two
		// syscalls, so a replacement placed at the path inside that window is
		// what gets removed. Narrowing it further needs `unlinkat` against a
		// directory fd (or an inode compare that is itself racy), and the
		// window is bounded by these two adjacent statements. The receipt half
		// — the part that told the user something untrue — is closed here.
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
		RefuseReason::UnreadableCompatDir { path } => format!(
			"{} could not be read, so it is undecided whether it is safe to \
			 detach",
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
		RefuseReason::UnreadableCompatDir { path } => format!(
			"fix the permission on {} (or one of its parent directories), or \
			 move it aside if it is not yours to change or its mount is \
			 unreachable — repair proceeds once the path is readable or gone; \
			 then re-run `aghub skills repair {name}`",
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
		let compat = root.join(".agent").join("skills");
		fs::create_dir_all(&compat).unwrap();
		Linker::symlink(&master, &compat.join(name)).unwrap();

		let readers = crate::skills::shape::readers_of(
			ResourceScope::ProjectOnly,
			Some(&root),
			name,
		);
		assert!(
			readers.contains(&"antigravity"),
			"the compat dir is what makes antigravity a reader, got {readers:?}"
		);

		let write_slot = root.join(".agents").join("skills").join(name);
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
		let write_slot = root.join(".agents").join("skills").join(name);
		fs::create_dir_all(write_slot.parent().unwrap()).unwrap();
		Linker::symlink(&master, &write_slot).unwrap();
		let compat = root.join(".agent").join("skills").join(name);
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
		let compat = root.join(".agent").join("skills").join(name);
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
		// The shared slot is eight agents' only way in — it becomes a link,
		// never a casualty of the sweep.
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

	/// Codex 5.6 blocker 2 (DO-NOT-SHIP review): the write-time step recorded
	/// `Tidied` and pushed the path into `report.unlinked` BEFORE checking
	/// whether the entry could still be unlinked at all — so a disk that
	/// changed between planning and writing (npx's `cleanAndCreateDirectory`,
	/// or a user replacing the stale link by hand) was reported as removed
	/// while the directory sat there completely untouched.
	#[test]
	fn a_compat_entry_that_changed_since_planning_is_never_reported_as_unlinked(
	) {
		let (_tmp, root) = fixture();
		let name = "demo";
		let master = root.join(".aghub").join(name);
		write_skill(&master, name, "shared");
		let write_slot = root.join(".agents").join("skills").join(name);
		fs::create_dir_all(write_slot.parent().unwrap()).unwrap();
		Linker::symlink(&master, &write_slot).unwrap();
		let compat = root.join(".agent").join("skills").join(name);
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
	///  * a link pointing somewhere else is not ours to move (D5)
	///  * `.agents/skills` is codex's second READ dir and eight other agents'
	///    only WRITE dir — sweeping it revokes the skill for all eight. That one
	///    really was planned before guard 4 existed.
	#[test]
	fn the_compat_sweep_never_takes_what_it_must_not() {
		// (a) a real directory in the compat dir
		let (_tmp, root) = fixture();
		let name = "demo";
		let master = root.join(".aghub").join(name);
		write_skill(&master, name, "shared");
		let write_slot = root.join(".agents").join("skills").join(name);
		fs::create_dir_all(write_slot.parent().unwrap()).unwrap();
		Linker::symlink(&master, &write_slot).unwrap();
		let compat = root.join(".agent").join("skills").join(name);
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
		let write_slot = root.join(".agents").join("skills").join(name);
		fs::create_dir_all(write_slot.parent().unwrap()).unwrap();
		Linker::symlink(&master, &write_slot).unwrap();
		let foreign = root.join(".agent").join("skills").join(name);
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
			"the shared slot is eight agents' only slot, never a detach: {:?}",
			p.actions
		);

		// (d) the compat link is the agent's ONLY Referrer and this run is not
		//     granting it one (`grant_to` is empty here). Detaching would leave
		//     the agent unable to read a skill it reads today — the exact
		//     pre-2.18 shape `repair` exists to rescue, not to finish off.
		let (_tmp4, root) = fixture();
		let master = root.join(".aghub").join(name);
		write_skill(&master, name, "shared");
		let only = root.join(".agent").join("skills").join(name);
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
	/// The round that added `UnreadableCompatDir` put `if !covered { continue }`
	/// BEFORE the fallible probe, so the refusal only fired when some other
	/// agent's slot happened to be covered — with nothing covered the run
	/// reported `ok` and left the stale referrer in place. An external reviewer
	/// reproduced that by running it.
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
		let compat_dir = root.join(".agent").join("skills");
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
			repair_all(ResourceScope::ProjectOnly, Some(&root), None, false)
				.unwrap();

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
			repair_all(ResourceScope::ProjectOnly, Some(&root), None, false)
				.unwrap();
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
			repair_all(ResourceScope::ProjectOnly, Some(&root), None, false)
				.unwrap();
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
			repair_all(ResourceScope::ProjectOnly, Some(&root), None, false)
				.unwrap();
		assert_eq!(first.len(), 2);

		let second =
			repair_all(ResourceScope::ProjectOnly, Some(&root), None, false)
				.unwrap();
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
}
