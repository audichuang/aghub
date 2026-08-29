//! The mutation lock as the FLOWS see it.
//!
//! `crates/skill/tests/mutation_lock.rs` proves the lock mechanism works. This
//! file proves a core flow actually TAKES it — delete the
//! `let _mutation_guard = …` from `prune_lock_from_dirs` and both tests below go
//! red, which nothing in the skill crate's tests would catch.
//!
//! Note WHICH assertion catches it: not a timing one. Without the outer guard the
//! prune still blocks, at the writer floor inside the lock rewrite; what changes
//! is that it scans disk BEFORE blocking. See each test.
//!
//! Prune is the flow under test because it is the one the spec called out by
//! name (its scan→rewrite window) and the only guarded flow that needs no git
//! fetch, no agent config and no `$HOME`: an injectable scanner plus a tempdir
//! project root is the whole fixture.
//!
//! Like the skill-crate tests this spawns a REAL second process (threads share
//! the in-process mutex and would pass with no file lock at all) and mutates no
//! environment variable of its own — the child gets its project root through
//! `Command::env`.

use aghub_core::skills::prune::{prune_lock_from_dirs, PruneScope};
use skill::lock::{mutation_guard, MutationScope};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const ROOT_ENV: &str = "AGHUB_LOCK_FLOW_TEST_ROOT";
const HOLD_ENV: &str = "AGHUB_LOCK_FLOW_TEST_HOLD_MS";
const READY_FILE: &str = "lock-acquired";

/// The other process: takes the project mutation lock, writes a lock entry and
/// its on-disk skill dir while holding it, then releases. Normally a no-op
/// `#[test]`; only the parent's `Command::env` switches it on.
#[test]
fn holder_child() {
	let (Ok(root), Ok(hold_ms)) =
		(std::env::var(ROOT_ENV), std::env::var(HOLD_ENV))
	else {
		return;
	};
	let root = PathBuf::from(root);
	let _guard =
		mutation_guard("child hold", &[MutationScope::Project(root.clone())])
			.expect("child acquire");
	std::fs::write(root.join(READY_FILE), b"").expect("signal readiness");
	std::thread::sleep(Duration::from_millis(hold_ms.parse().unwrap()));

	// Installed by "another process" INSIDE the held lock: disk first, then the
	// lock entry, the same order a real install writes them.
	let dir = skills_dir(&root).join("child-skill");
	std::fs::create_dir_all(&dir).unwrap();
	std::fs::write(dir.join("SKILL.md"), skill_md("child-skill")).unwrap();
	skill::lock::local::add_skill_to_local_lock(
		"child-skill",
		entry(),
		Some(&root),
	)
	.expect("child lock write");
}

fn skills_dir(root: &Path) -> PathBuf {
	root.join(".claude").join("skills")
}

fn skill_md(name: &str) -> String {
	format!("---\nname: {name}\ndescription: d\n---\n\nbody\n")
}

fn entry() -> skill::LocalSkillLockEntry {
	skill::LocalSkillLockEntry {
		source: "owner/repo".to_string(),
		source_url: None,
		ref_name: Some("main".to_string()),
		source_type: "github".to_string(),
		computed_hash: "h".to_string(),
		skill_path: Some("s/SKILL.md".to_string()),
		ref_commit: None,
	}
}

/// Spawn the holder and return once it reports the lock is held.
fn spawn_holder(root: &Path, hold: Duration) -> Child {
	let mut child = Command::new(std::env::current_exe().unwrap())
		.args(["--exact", "holder_child"])
		.env(ROOT_ENV, root)
		.env(HOLD_ENV, hold.as_millis().to_string())
		.stdout(Stdio::null())
		.stderr(Stdio::null())
		.spawn()
		.expect("spawn holder child");

	let ready = root.join(READY_FILE);
	let deadline = Instant::now() + Duration::from_secs(20);
	while !ready.exists() {
		if child.try_wait().expect("poll holder child").is_some() {
			panic!("holder child exited without acquiring the lock");
		}
		if Instant::now() >= deadline {
			let _ = child.kill();
			panic!("holder child never acquired the lock");
		}
		std::thread::sleep(Duration::from_millis(10));
	}
	child
}

/// A prune must WAIT for another process's mutation instead of reconciling the
/// lock against a disk set taken while that mutation was mid-flight.
///
/// **The STATE assertions are the ones with teeth**, not the timing one. Remove
/// the guard from `prune_lock_from_dirs` and the prune still blocks — at the
/// writer floor inside `retain_local_locked_skills` — so `waited` stays above
/// the threshold. What changes is WHERE it blocks: unguarded, it scans disk
/// first, so the scan predates the child's install and the rewrite then prunes
/// `child-skill` from the lock. Verified: reverting the guard fails on
/// `pruned.is_empty()` with `["child-skill"]`, and the timing assertion passes.
/// The timing assertion is kept only to prove the prune blocks at all.
///
/// Timing dependence, honestly: the child holds for 1200ms and the parent needs
/// microseconds to reach its scan, so the losing interleaving is what happens
/// unless the parent is descheduled for over a second. A parent starved that
/// badly would make this pass spuriously rather than fail.
#[test]
fn a_prune_waits_for_another_process_and_keeps_its_entry() {
	let tmp = tempfile::tempdir().unwrap();
	let root = tmp.path();
	// A skill that is already installed and locked before either process runs.
	let parent_dir = skills_dir(root).join("parent-skill");
	std::fs::create_dir_all(&parent_dir).unwrap();
	std::fs::write(parent_dir.join("SKILL.md"), skill_md("parent-skill"))
		.unwrap();
	skill::lock::local::add_skill_to_local_lock(
		"parent-skill",
		entry(),
		Some(root),
	)
	.unwrap();

	let mut child = spawn_holder(root, Duration::from_millis(1200));

	let dirs = vec![skills_dir(root)];
	let started = Instant::now();
	let pruned = prune_lock_from_dirs(
		PruneScope::Project,
		&dirs,
		Some(root),
		top_level_dirs,
	)
	.expect("prune");
	let waited = started.elapsed();
	child.wait().unwrap();

	// The teeth: an unguarded prune scans before the child installs, so its
	// rewrite drops the child's fresh entry.
	assert!(
		pruned.is_empty(),
		"the prune scanned before the other process finished, so it pruned a \
		 skill that IS on disk: {pruned:?}"
	);
	// Weaker, kept only to prove the prune blocks somewhere at all.
	assert!(
		waited >= Duration::from_millis(700),
		"prune did not wait for the holder at all, waited {waited:?}"
	);
	let locked = skill::lock::local::read_local_lock(Some(root));
	assert!(
		locked.skills.contains_key("parent-skill"),
		"the pre-existing entry must survive: {:?}",
		locked.skills.keys().collect::<Vec<_>>()
	);
	assert!(
		locked.skills.contains_key("child-skill"),
		"the other process's entry must survive the prune: {:?}",
		locked.skills.keys().collect::<Vec<_>>()
	);
}

/// Placement, with NO timing in it: the guard must be taken BEFORE the disk scan.
///
/// An unlockable location makes acquisition fail, so if the guard really is the
/// first thing the flow does, the scanner is never called. Move the guard below
/// the scan (or delete it) and the scanner runs — which is exactly the stale-view
/// bug, proven here without depending on the scheduler at all.
#[test]
fn a_prune_takes_the_guard_before_it_scans_disk() {
	let tmp = tempfile::tempdir().unwrap();
	let root = tmp.path().join("proj");
	std::fs::create_dir_all(root.join(".claude/skills")).unwrap();
	// A regular file where `.agents/` must be: the lock cannot be created here.
	std::fs::write(root.join(".agents"), b"not a directory").unwrap();

	let scanned = std::cell::Cell::new(false);
	let result = prune_lock_from_dirs(
		PruneScope::Project,
		&[skills_dir(&root)],
		Some(&root),
		|dir| {
			scanned.set(true);
			top_level_dirs(dir)
		},
	);

	assert!(
		result.is_err(),
		"an unacquirable lock must refuse the prune, got {result:?}"
	);
	assert!(
		!scanned.get(),
		"the disk scan ran before the mutation lock was held"
	);
}

/// The scanner `prune_lock_from_dirs` takes: top-level dirs of one skills dir.
fn top_level_dirs(dir: &Path) -> Result<Vec<PathBuf>, skill::ScanError> {
	let mut out = Vec::new();
	for entry in std::fs::read_dir(dir)
		.map_err(|_| skill::ScanError::PermissionDenied(dir.to_path_buf()))?
	{
		let entry = entry.map_err(|_| {
			skill::ScanError::PermissionDenied(dir.to_path_buf())
		})?;
		if entry.path().is_dir() {
			out.push(entry.path());
		}
	}
	Ok(out)
}

/// A skill reconcile decides what to delete from TWO unlocked reads — the
/// holder scan that computes `exhaustive`, and a dry-run removal plan per row —
/// and only then writes. Both must happen under the guard, so the guard has to
/// be the first thing the flow does.
///
/// Same no-timing trick as `a_prune_takes_the_guard_before_it_scans_disk`: an
/// unlockable location makes acquisition fail, so a correctly placed guard
/// refuses the whole reconcile. Move it below the planning (or drop it) and the
/// flow reads state, plans every row and returns a BATCH instead — the shape
/// that tells a caller "we looked at your agents and here is what happened".
#[test]
fn a_skill_reconcile_takes_the_guard_before_it_plans() {
	let tmp = tempfile::tempdir().unwrap();
	let root = tmp.path().join("proj");
	// A private copy claude holds; removing it needs no `.agents` of its own.
	let owned = skills_dir(&root).join("mover");
	std::fs::create_dir_all(&owned).unwrap();
	std::fs::write(owned.join("SKILL.md"), skill_md("mover")).unwrap();
	// A regular file where `.agents/` must be: the lock cannot be created here.
	std::fs::write(root.join(".agents"), b"not a directory").unwrap();

	let result = aghub_core::transfer::reconcile_skill(
		aghub_core::transfer::ResourceLocator {
			agent: aghub_core::models::AgentType::Claude,
			scope: aghub_core::transfer::InstallScope::Project,
			project_root: Some(root.clone()),
			name: "mover".to_string(),
		},
		vec![],
		vec![aghub_core::models::AgentType::Claude],
		true, // confirm
	);

	assert!(
		result.is_err(),
		"an unacquirable lock must refuse the whole reconcile before it plans \
		 anything, got a batch: {:?}",
		result.map(|batch| batch.results)
	);
	assert!(
		owned.join("SKILL.md").exists(),
		"nothing may be removed when the lock was never held"
	);
}

/// The ORDERING half, which the test above cannot pin on its own.
///
/// There the skill really exists, so planning SUCCEEDS and the guard fails
/// whichever side of the planning it sits on — both placements produce the same
/// `Err`, and moving the guard below `plan_reconcile_skill` leaves that test
/// green. Name a skill that is NOT there and the two placements finally
/// disagree: a guard taken first reports the LOCK, a guard taken after the
/// planning reports "no such skill" — proof the flow read state it had no right
/// to read yet. That read is the whole point of the guard's position: the
/// holder scan it feeds decides whether the shared Master gets collected.
#[test]
fn a_skill_reconcile_reports_the_lock_before_it_looks_the_skill_up() {
	let tmp = tempfile::tempdir().unwrap();
	let root = tmp.path().join("proj");
	std::fs::create_dir_all(skills_dir(&root)).unwrap();
	// A regular file where `.agents/` must be: the lock cannot be created here.
	std::fs::write(root.join(".agents"), b"not a directory").unwrap();

	let message = aghub_core::transfer::reconcile_skill(
		aghub_core::transfer::ResourceLocator {
			agent: aghub_core::models::AgentType::Claude,
			scope: aghub_core::transfer::InstallScope::Project,
			project_root: Some(root.clone()),
			name: "never-installed".to_string(),
		},
		vec![],
		vec![aghub_core::models::AgentType::Claude],
		true, // confirm
	)
	.expect_err("an unacquirable lock must refuse the reconcile")
	.to_string();

	assert!(
		message.contains("mutation lock"),
		"the LOCK must be what refused this, not a lookup the flow should \
		 never have reached without it; got: {message}"
	);
}
