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
//! The two platforms differ in ways this design deliberately does not depend on.
//! `flock` is advisory and whole-file; `LockFileEx` is MANDATORY over a byte
//! range (std locks `0..u32::MAX:u32::MAX`, i.e. all of it), so on Windows a
//! foreign handle's reads and writes inside the range fail outright. That never
//! matters here because the lock file's CONTENTS are never used: it is opened
//! append-only, nothing is ever written to it, and it stays 0 bytes. What both
//! platforms do share is what the reentrancy below relies on — a lock belongs to
//! the open handle, not the process, so a second `open` of the same path from
//! this very process blocks against itself.
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
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// Bound on how long an acquire waits for FOREIGN processes before giving up —
/// one deadline covering every scope, not one per scope (`Both` must not get 2x).
/// Never an unbounded wait on another process: for a user a hang is worse than
/// the race this closes. In-process queueing is separate and unbounded, see
/// [`lock_process`].
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

/// How often a waiter retries. The lock is held for whole-file rewrites and
/// symlink work, so a coarse poll costs nothing.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// Name of the lock file, in aghub's own state / `.agents` dir.
///
/// Never inside a skill **Master** dir (`<home|project>/.agents/skills`, or an
/// agent's own skills dir) — `npx skills` enumerates those and the folder hash
/// walks them. Note the global lock file's own directory IS called `skills` when
/// `XDG_STATE_HOME` is set (`$XDG_STATE_HOME/skills/.skill-lock.json`); that is
/// the state dir, not a Master, and nothing enumerates it for skills.
///
/// Not "can never be hashed", which would be too strong:
/// [`crate::compute_skill_folder_hash`] recurses whatever source directory it is
/// handed, so a LOCAL install whose source tree sits at or above one of these
/// locations would include this file. Master hashing is safe because it hashes
/// `<Master>/<skill>`, below both.
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
			//
			// CANONICALIZED first, or two spellings of one project get two lock
			// files and serialize against nothing: the API takes `project_root`
			// straight from a request (`/proj` vs `/proj/`), and on macOS the
			// walk-up root can arrive as `/var/...` where another caller resolved
			// `/private/var/...`.
			//
			// Falls back to the raw path when canonicalize fails, which needs
			// every component to exist. So two spellings of a root that does NOT
			// exist can still fork — accepted: a mutating flow with a missing
			// project root has nothing to serialize against and rejects the root
			// on its own.
			Self::Project(root) => std::fs::canonicalize(root)
				.unwrap_or_else(|_| root.clone())
				.join(".agents")
				.join(LOCK_FILE_NAME),
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

	/// [`Self::order_key`] as an owned value, for the per-thread high-water mark
	/// that rejects a nested acquire in the wrong order.
	fn order_rank(&self) -> (u8, PathBuf) {
		let (tag, path) = self.order_key();
		(tag, path.to_path_buf())
	}
}

/// Serializes mutations between THREADS of this process, replacing the two
/// per-module write mutexes the lock writers used to take: one lock, held for the
/// whole flow, so an in-process race cannot slip between a check and its write
/// either. It also makes in-process contention a WAIT rather than a timeout —
/// two threads racing for one path through the file lock alone would have one of
/// them fail after the bound.
///
/// ponytail: ONE process-wide mutex, not one per lock path. The ceiling: it is
/// held across the file-lock wait, so while thread A waits on an EXTERNAL aghub
/// process, an unrelated mutation on thread B queues too (up to A's bound). That
/// needs a real external holder to bite, and the spec's own non-goal is "making
/// concurrent aghub mutations fast". Upgrade path if it ever bites: a registry of
/// per-lock-path mutexes, acquired in [`MutationScope::order_key`] order.
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
	/// Highest [`MutationScope::order_rank`] this thread has taken, so a nested
	/// acquire in the wrong order is refused instead of deadlocking against
	/// another process that nests the other way round.
	static HIGHEST: RefCell<Option<(u8, PathBuf)>> =
		const { RefCell::new(None) };
}

/// Holds the skill mutation lock until dropped.
///
/// A pure depth token — it deliberately records NO paths of its own. The last
/// guard alive on a thread releases everything that thread took, so guards may
/// be dropped in any order: an early `drop()` of an outer guard cannot free a
/// lock a nested guard still relies on.
///
/// Deliberately **not `Send`**: every piece of bookkeeping behind it
/// ([`HELD`], [`DEPTH`], [`PROCESS_GUARD`]) is thread-local, so dropping a guard
/// on a thread other than the one that took it would release nothing, leave the
/// open handle in the acquiring thread's [`HELD`] forever, and drop the wrong
/// thread's depth to zero — wedging [`PROCESS_LOCK`] for the rest of the
/// process. The `PhantomData` makes that a compile error:
///
/// ```compile_fail
/// use skill::lock::{mutation_guard, MutationScope};
/// let guard = mutation_guard("x", &[MutationScope::Global]).unwrap();
/// // Moving the guard to another thread must not compile.
/// std::thread::spawn(move || drop(guard)).join().unwrap();
/// ```
#[derive(Debug)]
#[must_use = "the mutation lock is released as soon as the guard is dropped"]
pub struct MutationGuard {
	_not_send: PhantomData<*const ()>,
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
	// ONE deadline for the whole acquisition — the process mutex AND every scope.
	// Per-step timeouts would make the documented bound a lie: a `Both` acquire
	// would get 2x, and an unbounded `Mutex::lock` would let a hung mutation on
	// another thread block this one forever.
	let deadline = Instant::now() + timeout;

	let outermost = DEPTH.with(|d| {
		d.set(d.get() + 1);
		d.get() == 1
	});
	// From here every early return must go through `guard`'s Drop to unwind the
	// depth, so the guard is built BEFORE anything that can fail.
	let guard = MutationGuard {
		_not_send: PhantomData,
	};
	if outermost {
		PROCESS_GUARD.with(|p| *p.borrow_mut() = Some(lock_process()));
	}

	let mut ordered: Vec<&MutationScope> = scopes.iter().collect();
	ordered.sort_by_key(|s| s.order_key());
	ordered.dedup();
	for scope in ordered {
		let path = scope.lock_path();
		if HELD.with(|h| h.borrow().contains_key(&path)) {
			continue;
		}
		reject_out_of_order(scope, op)?;
		// `guard` is dropped on the `?`. That unwinds the depth, and if this was
		// the outermost acquire the depth hits zero and Drop releases everything
		// taken so far — a partial acquire never leaks.
		let file = lock_file(&path, op, deadline)?;
		HELD.with(|h| h.borrow_mut().insert(path, file));
	}
	Ok(guard)
}

/// Take [`PROCESS_LOCK`], queueing behind any other mutation on this process.
///
/// Deliberately UNBOUNDED, unlike the file wait. The two are not the same kind of
/// wait: the file lock waits on a FOREIGN process that may be hung or gone, so a
/// bound is the only protection there. This one waits on OUR own code, which will
/// finish or fail on its own — and bounding it turns ordinary queued work into
/// spurious failures. Measured, not assumed: a bounded version made a 200ms
/// acquire in this crate's own suite fail as soon as the other lock tests ran
/// alongside it, which is the exact shape of a desktop bulk operation over N
/// skills (the last one queues behind the other N-1). A genuinely hung flow is a
/// bug to fix in that flow; failing an unrelated thread would not fix it.
fn lock_process() -> MutexGuard<'static, ()> {
	// Poisoned: a previous holder panicked mid-mutation. The data it was writing is
	// not this lock's business, and refusing every later mutation would be worse,
	// so adopt it exactly as the write mutex this replaced did.
	PROCESS_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// Refuse a nested acquire that would take a lock ordering BEFORE one this
/// thread already holds.
///
/// Sorting within a single call cannot prevent a cycle across nested calls: one
/// process taking Project-then-Global while another takes Global-then-Project
/// deadlocks both. No flow in this repo does that, and this keeps it that way —
/// an error at the second acquire, rather than two processes waiting out the
/// timeout mid-transaction.
fn reject_out_of_order(scope: &MutationScope, op: &str) -> io::Result<()> {
	let rank = scope.order_rank();
	let inverted = HIGHEST.with(|h| {
		let mut highest = h.borrow_mut();
		match highest.as_ref() {
			Some(held) if rank < *held => true,
			_ => {
				*highest = Some(rank);
				false
			}
		}
	});
	if inverted {
		return Err(io::Error::other(format!(
			"'{op}' tried to take a lower-ordered skill mutation lock while \
			 already holding a higher one; acquire every scope in ONE \
			 `mutation_guard` call instead, so the ordering is applied for you"
		)));
	}
	Ok(())
}

impl Drop for MutationGuard {
	fn drop(&mut self) {
		// `try_with`: a panic in Drop aborts, and TLS may already be torn down.
		let depth = DEPTH
			.try_with(|d| {
				let next = d.get().saturating_sub(1);
				d.set(next);
				next
			})
			.unwrap_or(0);
		if depth > 0 {
			// A guard is a pure depth token: it owns no paths of its own, so the
			// LAST guard on this thread releases everything the thread took, in
			// any drop order. That is why an out-of-order release cannot free a
			// lock an inner guard still relies on.
			return;
		}
		let _ = HELD.try_with(|h| {
			for (_, file) in h.borrow_mut().drain() {
				// Closing the handle would release the lock anyway; unlock first
				// so the release is explicit and not an artifact of drop order.
				let _ = file.unlock();
			}
		});
		let _ = HIGHEST.try_with(|h| *h.borrow_mut() = None);
		let _ = PROCESS_GUARD.try_with(|p| *p.borrow_mut() = None);
	}
}

/// Open `path` and take its OS lock, waiting until `deadline`.
///
/// EVERY failure is an error — there is deliberately no "proceed unlocked"
/// fallback. An earlier revision degraded to a `log::warn` when the file could
/// not be created or the filesystem rejected locking, on the theory that
/// refusing to work was worse than the pre-lock status quo. That was wrong twice
/// over: a warning is invisible in the desktop/API, and it masked a total
/// failure — opening append-only made `try_lock` fail on ALL of Windows, so the
/// degrade path would have shipped Windows with no interprocess lock at all and
/// no visible symptom. A stale root-owned lock file, or a mount that allows I/O
/// but rejects locking, deserve the same actionable error rather than silent
/// loss of the exclusion every receipt in this subsystem now assumes.
fn lock_file(path: &Path, op: &str, deadline: Instant) -> io::Result<File> {
	if let Some(parent) = path.parent() {
		std::fs::create_dir_all(parent).map_err(|error| {
			lock_unavailable(
				op,
				path,
				format!("cannot create its directory: {error}"),
			)
		})?;
	}
	// `read` + `write`, NOT append-only: std documents that Windows cannot lock a
	// file opened only for append (`LockFileEx` needs read or write access). The
	// contents are never used either way — the file stays 0 bytes.
	let file = File::options()
		.read(true)
		.write(true)
		.create(true)
		// Never truncate: another process may hold this very file locked, and its
		// contents are not ours to reset (they are unused, and it stays 0 bytes).
		.truncate(false)
		.open(path)
		.map_err(|error| {
			lock_unavailable(op, path, format!("cannot open it: {error}"))
		})?;

	loop {
		match file.try_lock() {
			Ok(()) => return Ok(file),
			Err(std::fs::TryLockError::WouldBlock) => {}
			Err(std::fs::TryLockError::Error(error)) => {
				return Err(lock_unavailable(
					op,
					path,
					format!("this filesystem rejected the lock: {error}"),
				));
			}
		}
		if Instant::now() >= deadline {
			// Path stays OUT of the message: a surface may forward it verbatim
			// and API errors must not carry internal lock paths. It goes to the
			// log below instead.
			log::warn!(
				"skill mutation lock held, '{op}' gave up on '{}'",
				path.display()
			);
			return Err(io::Error::new(
				io::ErrorKind::WouldBlock,
				format!(
					"another aghub mutation is still in progress, so '{op}' timed \
					 out waiting for the skill mutation lock; retry once it \
					 finishes"
				),
			));
		}
		std::thread::sleep(POLL_INTERVAL);
	}
}

/// The error for a location that cannot hold a lock at all. Names the operation,
/// says what to do, and keeps the internal path in the log rather than in a
/// message a surface may forward verbatim.
fn lock_unavailable(op: &str, path: &Path, detail: String) -> io::Error {
	log::warn!(
		"skill mutation lock unavailable for '{op}' at '{}': {detail}",
		path.display()
	);
	io::Error::other(format!(
		"the skill mutation lock could not be acquired, so '{op}' was refused \
		 rather than run without protecting concurrent aghub processes; check \
		 that aghub's state directory is writable and supports file locking"
	))
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
		let tmp = tempfile::tempdir().unwrap();
		let root = tmp.path();
		assert_eq!(
			MutationScope::Project(root.to_path_buf()).lock_path(),
			std::fs::canonicalize(root)
				.unwrap()
				.join(".agents")
				.join(LOCK_FILE_NAME)
		);

		let global = MutationScope::Global.lock_path();
		assert_eq!(global.file_name().unwrap(), LOCK_FILE_NAME);
		assert_eq!(
			global.parent(),
			super::super::io::get_skill_lock_path().parent(),
			"the global lock file must follow the global lock's directory"
		);
	}

	/// Two spellings of ONE project must resolve to the SAME lock file, or they
	/// serialize against nothing. The API takes `project_root` from a request, so
	/// a trailing slash or a symlinked root is reachable input.
	#[test]
	fn one_project_spelled_two_ways_shares_its_lock_file() {
		let tmp = tempfile::tempdir().unwrap();
		let root = tmp.path();
		let plain = MutationScope::Project(root.to_path_buf()).lock_path();

		let trailing = MutationScope::Project(PathBuf::from(format!(
			"{}/",
			root.display()
		)))
		.lock_path();
		assert_eq!(plain, trailing, "a trailing slash must not fork the lock");

		std::fs::create_dir_all(root.join("sub")).unwrap();
		let indirect =
			MutationScope::Project(root.join("sub").join("..")).lock_path();
		assert_eq!(plain, indirect, "a `..` hop must not fork the lock");

		#[cfg(unix)]
		{
			let link = tmp.path().parent().unwrap().join(format!(
				"{}-link",
				root.file_name().unwrap().to_string_lossy()
			));
			std::os::unix::fs::symlink(root, &link).unwrap();
			let via_link = MutationScope::Project(link.clone()).lock_path();
			let _ = std::fs::remove_file(&link);
			assert_eq!(
				plain, via_link,
				"a symlinked root must not fork the lock"
			);
		}
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

	/// The same scope listed twice must be acquired once. Success under a SHORT
	/// file-lock bound is the whole assertion and it cannot false-pass: a second
	/// acquire of the same path would block on the first and error at the bound.
	/// No elapsed-time assertion — these tests run in parallel threads of one
	/// process and queue on `PROCESS_LOCK`, so wall-clock here measures the other
	/// tests, not this one.
	#[test]
	fn duplicate_scopes_acquire_once() {
		let tmp = tempfile::tempdir().unwrap();
		let scope = MutationScope::Project(tmp.path().to_path_buf());
		let guard = mutation_guard_with_timeout(
			"dup",
			&[scope.clone(), scope],
			Duration::from_millis(200),
		);
		assert!(guard.is_ok(), "a duplicate scope must not self-block");
	}

	/// A location that cannot hold a lock file must REFUSE the mutation, never
	/// proceed unlocked: an earlier revision degraded here with only a
	/// `log::warn`, which is invisible in the desktop/API and would have shipped
	/// Windows (where an append-only handle cannot be locked at all) with no
	/// interprocess lock and no symptom. A regular FILE where the `.agents`
	/// directory would go makes `create_dir_all` fail.
	#[test]
	fn an_unlockable_location_refuses_instead_of_running_unlocked() {
		let tmp = tempfile::tempdir().unwrap();
		let root = tmp.path().join("proj");
		std::fs::create_dir_all(&root).unwrap();
		std::fs::write(root.join(".agents"), b"not a directory").unwrap();

		let error = mutation_guard_with_timeout(
			"degrade",
			&[MutationScope::Project(root)],
			Duration::from_millis(200),
		)
		.expect_err("an unlockable location must not hand out a guard");
		let message = error.to_string();
		assert!(
			message.contains("degrade") && message.contains("refused"),
			"the error must name the operation and say it was refused: {message}"
		);
		assert!(
			!message.contains(".aghub-mutation.lock"),
			"a forwarded API error must not carry the internal lock path: {message}"
		);
	}

	/// The depth must unwind even when the acquire FAILS, or one refused
	/// mutation wedges every later one on this thread behind a depth that never
	/// returns to zero (and a `PROCESS_GUARD` that is never released).
	#[test]
	fn a_failed_acquire_leaves_no_depth_behind() {
		let tmp = tempfile::tempdir().unwrap();
		let bad = tmp.path().join("bad");
		std::fs::create_dir_all(&bad).unwrap();
		std::fs::write(bad.join(".agents"), b"not a directory").unwrap();

		for _ in 0..3 {
			assert!(mutation_guard_with_timeout(
				"fail",
				&[MutationScope::Project(bad.clone())],
				Duration::from_millis(50),
			)
			.is_err());
		}
		assert_eq!(
			DEPTH.with(Cell::get),
			0,
			"depth leaked after a failed acquire"
		);

		// The real proof: a good scope still acquires afterwards. A leaked depth
		// would have left this thread believing it was nested, so the outermost
		// release never runs and `PROCESS_LOCK` is held for the rest of the
		// process.
		let good = tmp.path().join("good");
		std::fs::create_dir_all(&good).unwrap();
		let guard = mutation_guard_with_timeout(
			"after",
			&[MutationScope::Project(good)],
			Duration::from_millis(200),
		);
		assert!(guard.is_ok(), "a later acquire must still work: {guard:?}");
	}

	/// A nested acquire that orders BEFORE one this thread already holds is
	/// refused: two processes nesting the opposite way round would otherwise
	/// deadlock each other mid-transaction.
	#[test]
	fn a_nested_acquire_in_the_wrong_order_is_refused() {
		let tmp = tempfile::tempdir().unwrap();
		let root = tmp.path().to_path_buf();
		// Project sorts AFTER Global, so holding Project and then asking for
		// Global is the inversion.
		let _outer = mutation_guard_with_timeout(
			"outer",
			&[MutationScope::Project(root)],
			Duration::from_millis(200),
		)
		.unwrap();
		let error = mutation_guard_with_timeout(
			"inner",
			&[MutationScope::Global],
			Duration::from_millis(200),
		)
		.expect_err("a lock-order inversion must be refused, not attempted");
		assert!(
			error.to_string().contains("lower-ordered"),
			"unexpected message: {error}"
		);
	}
}
