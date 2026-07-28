//! Cross-process behaviour of the skill mutation lock.
//!
//! **Threads cannot test this.** They share the process mutex inside the guard,
//! so a thread-based race test passes with no file lock at all — the single most
//! likely way this ships broken. Every exclusion test here therefore spawns a
//! REAL second process: this same test binary, re-invoked with
//! `--exact holder_child`, which acquires the guard and holds it.
//!
//! No test mutates this process's environment, and none uses
//! [`MutationScope::Global`] (whose lock file lives under the real `$HOME` when
//! `XDG_STATE_HOME` is unset): every guard is keyed on a per-test `tempdir`
//! passed to the child through `Command::env`.

use skill::lock::{mutation_guard, mutation_guard_with_timeout, MutationScope};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// Project root the child locks (set by the parent, per test).
const ROOT_ENV: &str = "AGHUB_LOCK_TEST_ROOT";
/// How long the child holds the lock once acquired, in milliseconds.
const HOLD_ENV: &str = "AGHUB_LOCK_TEST_HOLD_MS";
/// File the child creates once it holds the lock. A file rather than a stdout
/// line so the parent never has to keep a pipe alive for the child's lifetime.
const READY_FILE: &str = "lock-acquired";

/// The other process. Normally a no-op `#[test]`; it only does anything when the
/// parent re-invokes this binary with the two env vars set.
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
}

/// Spawn the holder child and return it only once it reports the lock is held.
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

/// The primary test: a second PROCESS must wait for the holder, then succeed.
/// Without a real file lock the acquire returns immediately and `waited` is
/// ~0ms, so this goes red.
#[test]
fn second_process_waits_for_the_holder_then_succeeds() {
	let tmp = tempfile::tempdir().unwrap();
	let mut child = spawn_holder(tmp.path(), Duration::from_millis(1200));

	let scope = MutationScope::Project(tmp.path().to_path_buf());
	let started = Instant::now();
	let guard = mutation_guard("install", &[scope]);
	let waited = started.elapsed();

	child.wait().unwrap();
	assert!(
		guard.is_ok(),
		"acquire after the holder released: {guard:?}"
	);
	assert!(
		waited >= Duration::from_millis(700),
		"expected to block until the holder released, waited only {waited:?}"
	);
}

/// A killed holder must NOT wedge the lock. This is the test that rules out a
/// `create_new` lockfile regression: the OS releases `flock`/`LockFileEx` when
/// the process dies, whereas a lockfile would need hand-rolled staleness
/// detection and would leave the user's skills stuck until they deleted a file.
#[test]
fn a_killed_holder_releases_the_lock() {
	let tmp = tempfile::tempdir().unwrap();
	// Far longer than the acquire bound below: only the kill can free this.
	let mut child = spawn_holder(tmp.path(), Duration::from_secs(60));
	child.kill().expect("kill holder");
	child.wait().unwrap();

	let scope = MutationScope::Project(tmp.path().to_path_buf());
	let guard = mutation_guard_with_timeout(
		"prune",
		&[scope],
		Duration::from_millis(1500),
	);
	assert!(
		guard.is_ok(),
		"a killed holder must not need manual cleanup: {guard:?}"
	);
}

/// Timeout path: a holder that never releases must produce a bounded error that
/// names the operation and suggests the other process — never an unbounded hang
/// and never a silent unlocked fallthrough.
#[test]
fn a_never_releasing_holder_times_out_naming_the_operation() {
	let tmp = tempfile::tempdir().unwrap();
	let mut child = spawn_holder(tmp.path(), Duration::from_secs(30));

	let scope = MutationScope::Project(tmp.path().to_path_buf());
	let started = Instant::now();
	let error = mutation_guard_with_timeout(
		"source sync",
		&[scope],
		Duration::from_millis(200),
	)
	.expect_err("a held lock must not be handed out");
	let waited = started.elapsed();
	let _ = child.kill();
	child.wait().unwrap();

	assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock);
	assert!(
		waited < Duration::from_secs(5),
		"the wait must be bounded, took {waited:?}"
	);
	let message = error.to_string();
	assert!(
		message.contains("source sync") && message.contains("another aghub"),
		"error must name the operation and the other process: {message}"
	);
	assert!(
		!message.contains(".aghub-mutation.lock"),
		"a forwarded API error must not carry the internal lock path: {message}"
	);
}

/// Reentrancy: a flow holding the guard calls `modify_*_lock`, which takes it
/// again on the same thread. A second `open` + `flock` of one path in one
/// process BLOCKS, so without the depth counter this deadlocks. Asserted with a
/// timeout so a regression fails instead of hanging CI.
#[test]
fn a_nested_acquire_on_the_same_thread_does_not_deadlock() {
	let one = tempfile::tempdir().unwrap();
	let two = tempfile::tempdir().unwrap();
	let (first, second) = (one.path().to_path_buf(), two.path().to_path_buf());
	let (tx, rx) = std::sync::mpsc::channel();

	std::thread::spawn(move || {
		// Two scopes at once, the widest case a `Both`-scope flow can take.
		let scopes = [
			MutationScope::Project(first.clone()),
			MutationScope::Project(second),
		];
		let outer = mutation_guard("rename", &scopes);
		let nested = outer.as_ref().ok().map(|_| {
			// What `modify_local_lock` / `modify_skill_lock` do underneath.
			mutation_guard_with_timeout(
				"modify project lock",
				&[MutationScope::Project(first)],
				Duration::from_millis(500),
			)
			.is_ok()
		});
		let _ = tx.send((outer.is_ok(), nested));
	});

	let (outer_ok, nested_ok) = rx
		.recv_timeout(Duration::from_secs(5))
		.expect("nested acquire deadlocked");
	assert!(outer_ok, "outer acquire failed");
	assert_eq!(nested_ok, Some(true), "nested acquire must be free");
}
