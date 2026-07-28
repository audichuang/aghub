//! The interprocess mutation lock for the skill subsystem.
//!
//! Every "did I create this?" receipt in this subsystem (the linker's
//! `created_master`, a lock write's replaced entry) is only trustworthy if the
//! whole check → write → rollback span is mutually exclusive across PROCESSES,
//! not just across threads. `modify_*_lock`'s process mutex cannot do that: two
//! aghub processes both read an absent entry, both insert, and both are told
//! they created it. This module is that mutual exclusion; the flows in
//! `aghub-core` take it at the top of their transaction and the lock writers
//! take it as a floor (reentrancy makes both safe together).
//!
//! Mechanism: `std::fs::File::lock` — `flock` on unix, `LockFileEx` on Windows
//! — so the OS releases the lock when the holder dies. A `create_new` lockfile
//! scheme would have to invent staleness detection, and a crashed aghub would
//! then wedge the user's skills until they deleted a file by hand.
//!
//! Scope: this serializes aghub against aghub only. `npx skills` takes no lock
//! of ours, so a concurrent `npx skills` run is still unserialized. Read paths
//! (`doctor`, `check`, `coverage`, `source list/diff`) are deliberately NOT
//! locked — a torn read is already tolerated and blocking them would be a
//! usability regression for no safety gain.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// Bound on how long [`mutation_guard`] waits before giving up. Bounded, never
/// an unbounded wait: for a user a hang is worse than the race this closes.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

/// How often a waiter retries. The lock is held for whole-file rewrites and
/// symlink work, so a coarse poll costs nothing.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// Name of the lock file, in aghub's own state / `.agents` dir. Never inside a
/// `skills/` dir: `npx skills` enumerates those, and the folder hash walks them.
const LOCK_FILE_NAME: &str = ".aghub-mutation.lock";

/// Which scope's mutation lock to take.
///
/// One lock per lock FILE, which is the granularity that already serializes
/// everything (every mutation rewrites the whole lock file). Per-skill locks are
/// the tempting refinement and are deliberately not here: they would still need
/// this lock underneath, and two locks is where deadlock ordering bugs live.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MutationScope {
	/// The global lock (`.skill-lock.json`) + the home `.agents/skills` Master.
	Global,
	/// One project's lock (`skills-lock.json`) + its `.agents/skills` Master.
	Project(PathBuf),
}

impl MutationScope {
	/// The file whose OS lock guards this scope.
	pub fn lock_path(&self) -> PathBuf {
		match self {
			// Follows the global lock file wherever `XDG_STATE_HOME` puts it, so
			// isolating that in a test isolates this too.
			Self::Global => {
				super::io::get_skill_lock_path().with_file_name(LOCK_FILE_NAME)
			}
			// Deliberately NOT beside `skills-lock.json`: that is the user's repo
			// root, while `.agents/` is already aghub's (it holds the project
			// Master). Users who commit `.agents/` should gitignore this file.
			Self::Project(root) => root.join(".agents").join(LOCK_FILE_NAME),
		}
	}

	/// Fixed acquisition order, so a caller taking BOTH scopes can never
	/// deadlock against one taking them the other way round.
	fn order_key(&self) -> (u8, &Path) {
		match self {
			Self::Global => (0, Path::new("")),
			Self::Project(root) => (1, root.as_path()),
		}
	}
}

/// Serializes mutations between THREADS of this process. Replaces the two
/// per-module write mutexes it used to take: one lock, held for the whole flow,
/// so an in-process race cannot slip between a check and its write either. It is
/// also the floor if the OS lock turns out to be unavailable (see
/// [`lock_file`]).
static PROCESS_LOCK: Mutex<()> = Mutex::new(());

thread_local! {
	/// Lock files THIS thread holds. A nested acquire of a path already in here
	/// is a no-op, which is what makes the lock reentrant: a flow may hold the
	/// guard and still call `modify_*_lock`, which takes it again, without
	/// self-deadlocking (a second `open` + `flock` of the same path in one
	/// process blocks, so without this the nested call would hang).
	static HELD: RefCell<HashMap<PathBuf, File>> =
		RefCell::new(HashMap::new());
	/// The process mutex, held while this thread is anywhere inside a guard.
	static PROCESS_GUARD: RefCell<Option<MutexGuard<'static, ()>>> =
		const { RefCell::new(None) };
	/// Nesting depth, so only the outermost guard takes/releases the mutex.
	static DEPTH: Cell<usize> = const { Cell::new(0) };
}

/// Holds the skill mutation lock until dropped.
#[derive(Debug)]
#[must_use = "the mutation lock is released as soon as the guard is dropped"]
pub struct MutationGuard {
	/// Only the paths THIS guard acquired. A nested guard acquires nothing and
	/// therefore releases nothing.
	owned: Vec<PathBuf>,
}

/// Acquire the mutation lock for every scope in `scopes` (order-independent —
/// they are sorted internally), waiting up to [`DEFAULT_TIMEOUT`].
///
/// `op` names the operation for the timeout error. Reentrant: acquiring a scope
/// this thread already holds is free, so a flow-level guard and the lock
/// writers underneath it compose without reordering any call site.
pub fn mutation_guard(
	op: &str,
	scopes: &[MutationScope],
) -> io::Result<MutationGuard> {
	mutation_guard_with_timeout(op, scopes, DEFAULT_TIMEOUT)
}

/// [`mutation_guard`] with an explicit wait bound (tests, and any caller that
/// knows it should fail faster than the default).
pub fn mutation_guard_with_timeout(
	op: &str,
	scopes: &[MutationScope],
	timeout: Duration,
) -> io::Result<MutationGuard> {
	if DEPTH.with(|d| {
		d.set(d.get() + 1);
		d.get()
	}) == 1
	{
		let held = PROCESS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
		PROCESS_GUARD.with(|p| *p.borrow_mut() = Some(held));
	}
	let mut guard = MutationGuard { owned: Vec::new() };

	let mut ordered: Vec<&MutationScope> = scopes.iter().collect();
	ordered.sort_by_key(|s| s.order_key());
	ordered.dedup();
	for scope in ordered {
		let path = scope.lock_path();
		if HELD.with(|h| h.borrow().contains_key(&path)) {
			continue;
		}
		// `guard` is dropped on the `?`, releasing whatever it already took and
		// unwinding the depth — a partial acquire never leaks.
		if let Some(file) = lock_file(&path, op, timeout)? {
			HELD.with(|h| h.borrow_mut().insert(path.clone(), file));
			guard.owned.push(path);
		}
	}
	Ok(guard)
}

impl Drop for MutationGuard {
	fn drop(&mut self) {
		for path in self.owned.drain(..) {
			let file = HELD.with(|h| h.borrow_mut().remove(&path));
			// Closing the handle would release the lock anyway; unlock first so
			// the release is explicit and not an artifact of drop order.
			if let Some(file) = file {
				let _ = file.unlock();
			}
		}
		// `try_with`: a panic in Drop aborts, and TLS may already be torn down.
		let depth = DEPTH
			.try_with(|d| {
				let next = d.get().saturating_sub(1);
				d.set(next);
				next
			})
			.unwrap_or(0);
		if depth == 0 {
			let _ = PROCESS_GUARD.try_with(|p| *p.borrow_mut() = None);
		}
	}
}

/// Open `path` and take its OS lock, waiting up to `timeout`.
///
/// `Ok(None)` means this location cannot be locked at all — the directory or
/// file could not be created, or the filesystem does not support locking (some
/// network mounts). That DEGRADES to the pre-lock behaviour (still serialized
/// in-process by [`PROCESS_LOCK`]) rather than refusing to work in an
/// environment where aghub works today; it is never used for contention, which
/// would silently reintroduce the race this exists to remove.
fn lock_file(
	path: &Path,
	op: &str,
	timeout: Duration,
) -> io::Result<Option<File>> {
	if let Some(parent) = path.parent() {
		if let Err(error) = std::fs::create_dir_all(parent) {
			log::warn!(
				"skill mutation lock unavailable for '{op}': cannot create '{}': \
				 {error}",
				parent.display()
			);
			return Ok(None);
		}
	}
	let file = match File::options().create(true).append(true).open(path) {
		Ok(file) => file,
		Err(error) => {
			log::warn!(
				"skill mutation lock unavailable for '{op}': cannot open '{}': \
				 {error}",
				path.display()
			);
			return Ok(None);
		}
	};

	let deadline = Instant::now() + timeout;
	loop {
		match file.try_lock() {
			Ok(()) => return Ok(Some(file)),
			Err(std::fs::TryLockError::WouldBlock) => {}
			Err(std::fs::TryLockError::Error(error)) => {
				log::warn!(
					"skill mutation lock unsupported here for '{op}' ('{}'): \
					 {error}",
					path.display()
				);
				return Ok(None);
			}
		}
		if Instant::now() >= deadline {
			// Path stays OUT of the message: a surface may forward it verbatim
			// and API errors must not carry internal lock paths. It goes to the
			// log below instead.
			log::warn!(
				"skill mutation lock held by another process, '{op}' gave up on \
				 '{}'",
				path.display()
			);
			return Err(io::Error::new(
				io::ErrorKind::WouldBlock,
				format!(
					"another aghub process is mutating skills, so '{op}' timed \
					 out after {:.0}s waiting for the skill mutation lock; retry \
					 once it finishes",
					timeout.as_secs_f32()
				),
			));
		}
		std::thread::sleep(POLL_INTERVAL);
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	/// npx compatibility + repo hygiene: a project's lock file goes in
	/// `.agents/`, never in the repo root and never inside `.agents/skills`
	/// (`npx skills` enumerates that dir and the folder hash walks it); the
	/// global one sits next to the global lock file.
	#[test]
	fn lock_files_live_where_documented() {
		assert_eq!(
			MutationScope::Project(PathBuf::from("/tmp/proj")).lock_path(),
			PathBuf::from("/tmp/proj/.agents").join(LOCK_FILE_NAME)
		);

		let global = MutationScope::Global.lock_path();
		assert_eq!(global.file_name().unwrap(), LOCK_FILE_NAME);
		assert_eq!(
			global.parent(),
			super::super::io::get_skill_lock_path().parent(),
			"the global lock file must follow the global lock's directory"
		);
	}

	/// Fixed order, so taking both scopes cannot deadlock against a caller that
	/// listed them the other way round.
	#[test]
	fn both_scopes_sort_global_first() {
		let mut scopes = vec![
			MutationScope::Project(PathBuf::from("/b")),
			MutationScope::Global,
			MutationScope::Project(PathBuf::from("/a")),
		];
		scopes.sort_by(|a, b| a.order_key().cmp(&b.order_key()));
		assert_eq!(
			scopes,
			vec![
				MutationScope::Global,
				MutationScope::Project(PathBuf::from("/a")),
				MutationScope::Project(PathBuf::from("/b")),
			]
		);
	}

	/// The same scope listed twice must be acquired once, or the second acquire
	/// would block on the first for the whole timeout.
	#[test]
	fn duplicate_scopes_acquire_once() {
		let tmp = tempfile::tempdir().unwrap();
		let scope = MutationScope::Project(tmp.path().to_path_buf());
		let guard = mutation_guard_with_timeout(
			"dup",
			&[scope.clone(), scope],
			Duration::from_millis(200),
		)
		.unwrap();
		assert_eq!(guard.owned.len(), 1);
	}
}
