//! The mutation lock as the FLOWS see it.
//!
//! `crates/skill/tests/mutation_lock.rs` proves the lock mechanism works. This
//! file proves a core flow actually TAKES it — delete the
//! `let _mutation_guard = …` from `prune_lock_from_dirs` and the wait assertion
//! below goes red, which nothing in the skill crate's tests would catch.
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
