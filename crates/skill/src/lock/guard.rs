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
//! matters here because the lock file's CONTENTS are never used: nothing is ever
//! written to it and it stays 0 bytes (it IS opened read+write, because Windows
//! cannot lock an append-only handle at all — see `lock_file`). What both
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
	/// The file whose OS lock guards this scope — and the scope's ONE identity.
	///
	/// Everything downstream keys off this value: the `HELD` map, the acquisition
	/// order, and the high-water mark that rejects an inverted nested acquire.
	/// They must not use anything else. Ordering by the raw project root while
	/// locking a canonicalized path is a deadlock: an alias for project P that
	/// sorts after Q lets one thread order P→Q and another Q→P while both pass
	/// the ordering check, and they then wait each other out.
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
			// Resolved through `resolve_existing`, or two spellings of one project
			// get two lock files and serialize against nothing: the API takes
			// `project_root` straight from a request (`/proj` vs `/proj/`), and on
			// macOS the walk-up root can arrive as `/var/...` where another caller
			// resolved `/private/var/...`.
			Self::Project(root) => {
				resolve_existing(root).join(".agents").join(LOCK_FILE_NAME)
			}
		}
	}

	/// Total, stable acquisition order. Global first (so a `Both` acquire reads
	/// naturally), then projects by their resolved lock path — the SAME identity
	/// the lock itself uses.
	fn order_rank(&self) -> (u8, PathBuf) {
		match self {
			Self::Global => (0, self.lock_path()),
			Self::Project(_) => (1, self.lock_path()),
		}
	}
}

/// Canonicalize as much of `path` as exists and resolve the rest lexically.
///
/// Plain `canonicalize` needs EVERY component to exist, and a project root that
/// does not exist yet is reachable — the API accepts an absolute root it has
/// never seen, and acquiring the lock then CREATES `<root>/.agents`. With plain
/// canonicalize that root would key one way before the create and another way
/// after, so a nested acquire would miss `HELD` and contend with its own handle
/// until the timeout. Resolving the longest existing PREFIX keeps one identity
/// across the create.
///
/// The order matters, and getting it wrong forks the identity — which defeats the
/// exclusion entirely, because two spellings of one project then take two
/// different locks:
///
/// - **Absolutize before anything else.** A relative root is resolved against the
///   process cwd, so `..` in it only means something once the cwd is prepended.
///   Normalizing `../target` lexically first yields `target` — a DIFFERENT
///   directory (a sibling of the cwd vs a child of it), while
///   `get_local_lock_path` still resolves the original spelling. One caller would
///   lock `<cwd>/target` while mutating `<cwd>/../target`.
/// - **Try the longest prefix first, so the filesystem resolves `..`.** `..` after
///   a symlink is the parent of the link's TARGET; no lexical pass can know that.
///   `<p>/link/..` and the target's real parent are the same directory and must
///   share one lock file.
///
/// Only the tail that cannot be resolved gets lexical treatment, and that is
/// exact: a path component that does not exist cannot be a symlink. Not a
/// security boundary — every mutating flow has its own containment guards — just
/// one identity per directory.
pub fn resolve_existing(path: &Path) -> PathBuf {
	let absolute =
		std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
	let components: Vec<std::path::Component<'_>> =
		absolute.components().collect();

	// Longest existing prefix wins. Walking down from the full path (rather than
	// up from the root) also means an already-existing root costs ONE
	// `canonicalize`. `components()` is used instead of `parent()`/`file_name()`
	// because `file_name()` is None for a path ending in `..`, which would
	// abandon the walk.
	for split in (0..=components.len()).rev() {
		let prefix: PathBuf = components[..split].iter().collect();
		let Ok(resolved) = std::fs::canonicalize(&prefix) else {
			continue;
		};
		let mut out = resolved;
		for component in &components[split..] {
			match component {
				std::path::Component::ParentDir => {
					out.pop();
				}
				std::path::Component::CurDir => {}
				other => out.push(other.as_os_str()),
			}
		}
		return out;
	}

	// Nothing resolvable at all (an unreadable cwd left `path` relative, or the
	// root itself cannot be stat'd). Fall back to a purely lexical form so one
	// spelling still maps to one key.
	let mut lexical = PathBuf::new();
	for component in &components {
		match component {
			std::path::Component::ParentDir => {
				lexical.pop();
			}
			std::path::Component::CurDir => {}
			other => lexical.push(other.as_os_str()),
		}
	}
	lexical
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
/// per-lock-path mutexes, acquired in `MutationScope::order_rank` order.
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
/// Deliberately **not `Send`**: every piece of bookkeeping behind it (`HELD`,
/// `DEPTH`, `PROCESS_GUARD` — all module-private) is thread-local, so dropping a
/// guard on a thread other than the one that took it would release nothing, leave
/// the open handle in the acquiring thread's held-set forever, and drop the wrong
/// thread's depth to zero — wedging the process mutex for the rest of the
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
/// they are sorted internally), waiting up to 10s for other PROCESSES (see
/// [`mutation_guard_with_timeout`] for the exact contract).
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
///
/// The exact contract, because "bounded" alone is not true: `timeout` bounds the
/// waits on FOREIGN processes — one deadline shared by every scope, so a
/// two-scope acquire cannot quietly take twice as long. Queueing behind another
/// thread of THIS process happens first and is deliberately unbounded; the
/// deadline starts after it, so a thread that queued for a while still gets its
/// full bound. `lock_process` (module-private) documents why the two waits are
/// treated differently. A local filesystem operation that hangs while holding the
/// lock can therefore still stall this process — availability, not corruption.
///
/// Both waits BLOCK the calling thread. An async caller must not call this on an
/// executor thread: an aghub process holding the lock would then park a worker for
/// up to the bound. `aghub-api` runs every mutation through
/// `crate::blocking::in_mutation_pool` for exactly that reason.
pub fn mutation_guard_with_timeout(
	op: &str,
	scopes: &[MutationScope],
	timeout: Duration,
) -> io::Result<MutationGuard> {
	let outermost = DEPTH.with(|d| {
		d.set(d.get() + 1);
		d.get() == 1
	});
	// From here every early return must go through `guard`'s Drop to unwind the
	// depth, so the guard is built BEFORE anything that can fail.
	let guard = MutationGuard {
		_not_send: PhantomData,
	};
	// Queue behind other threads FIRST, unbounded — see `lock_process`. The
	// deadline starts after, and covers only the waits on foreign processes, so a
	// thread that queued for a while still gets its full bound. ONE deadline for
	// all scopes, not one each, or a `Both` acquire would silently get 2x.
	if outermost {
		PROCESS_GUARD.with(|p| *p.borrow_mut() = Some(lock_process()));
	}
	let deadline = Instant::now() + timeout;

	// Resolved ONCE per scope, then sorted, deduplicated, locked and range-checked
	// against that same value. Recomputing `lock_path()` after ordering would let
	// a directory being created (or an alias resolving) between the two calls
	// separate the ordering identity from the path actually locked.
	let mut ordered: Vec<(u8, PathBuf)> =
		scopes.iter().map(|s| s.order_rank()).collect();
	ordered.sort();
	ordered.dedup();
	for rank in ordered {
		let path = rank.1.clone();
		if HELD.with(|h| h.borrow().contains_key(&path)) {
			continue;
		}
		reject_out_of_order(&rank, op)?;
		// `guard` is dropped on the `?`. That unwinds the depth, and if this was
		// the outermost acquire the depth hits zero and Drop releases everything
		// taken so far — a partial acquire never leaks.
		let file = lock_file(&path, op, deadline)?;
		HELD.with(|h| h.borrow_mut().insert(path, file));
		// Advanced only AFTER the lock is really held. Recording the rank of a
		// lock we then failed to take would spuriously reject a later legal
		// nesting for the rest of this guard's life.
		HIGHEST.with(|h| *h.borrow_mut() = Some(rank));
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
fn reject_out_of_order(rank: &(u8, PathBuf), op: &str) -> io::Result<()> {
	let inverted =
		HIGHEST.with(|h| h.borrow().as_ref().is_some_and(|held| rank < held));
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
	///
	/// Holds [`TestLockGuard`] because the GLOBAL half reads `XDG_STATE_HOME`
	/// (twice), which makes it one of the lock tests the crate's rule covers — not
	/// the tempdir-only exception the other tests here qualify for. Without it
	/// another test's state-home swap lands between the two reads and they simply
	/// describe different moments: caught on Windows CI, where the pair came back
	/// as `~/.agents` vs a temp `…/skills`.
	#[test]
	fn lock_files_live_where_documented() {
		let _state_home = super::super::test_utils::TestLockGuard::new();

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

		// The same hop through a path that does NOT exist, so canonicalize cannot
		// resolve it and the lexical normalization has to.
		let missing =
			MutationScope::Project(root.join("never-created").join(".."))
				.lock_path();
		assert_eq!(
			plain, missing,
			"a `..` hop below a non-existent path must not fork the lock"
		);

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

	/// A RELATIVE root must be resolved against the cwd, not normalized lexically
	/// first. Normalizing first turns `../x` into `x` — a sibling of the cwd
	/// becomes a child of it, so the guard locks a different directory than
	/// `get_local_lock_path` writes to and the exclusion protects nothing.
	#[test]
	fn a_relative_root_resolves_through_the_cwd() {
		let cwd = std::fs::canonicalize(".").unwrap();
		assert_eq!(
			resolve_existing(Path::new("..")),
			cwd.parent().unwrap(),
			"`..` must mean the cwd's parent"
		);
		assert_eq!(
			resolve_existing(Path::new(".")),
			cwd,
			"`.` must mean the cwd itself"
		);
	}

	/// `..` after a SYMLINK belongs to the link's target, and only the filesystem
	/// knows that. Two spellings of one directory must key the same, or two
	/// callers mutating it take two different locks.
	#[cfg(unix)]
	#[test]
	fn a_parent_hop_after_a_symlink_follows_the_link() {
		let tmp = tempfile::tempdir().unwrap();
		let real = tmp.path().join("real");
		std::fs::create_dir_all(real.join("inner")).unwrap();
		let link = tmp.path().join("link");
		std::os::unix::fs::symlink(real.join("inner"), &link).unwrap();

		assert_eq!(
			resolve_existing(&link.join("..")),
			std::fs::canonicalize(&real).unwrap(),
			"`link/..` is the parent of the link's TARGET, not of the link"
		);
	}

	/// Fixed order, so taking both scopes cannot deadlock against a caller that
	/// listed them the other way round — and the order must be derived from the
	/// same identity the lock uses, its lock path, not from the raw root.
	#[test]
	fn scopes_order_global_first_then_by_lock_path() {
		// Ranking `Global` resolves the global lock path, i.e. reads
		// `XDG_STATE_HOME`. The assertions do not depend on its value (a `Global`
		// rank sorts first on the discriminant alone), but reading env while
		// another thread swaps it is UB on unix, so take the guard anyway.
		let _state_home = super::super::test_utils::TestLockGuard::new();

		let tmp = tempfile::tempdir().unwrap();
		let a = tmp.path().join("a");
		let b = tmp.path().join("b");
		std::fs::create_dir_all(&a).unwrap();
		std::fs::create_dir_all(&b).unwrap();

		let mut scopes = vec![
			MutationScope::Project(b.clone()),
			MutationScope::Global,
			MutationScope::Project(a.clone()),
		];
		scopes.sort_by_key(|x| x.order_rank());
		assert_eq!(
			scopes,
			vec![
				MutationScope::Global,
				MutationScope::Project(a.clone()),
				MutationScope::Project(b),
			]
		);

		// The ordering key must survive an alias: a symlinked spelling of `a` has
		// to rank identically, or two threads can order the same pair both ways.
		#[cfg(unix)]
		{
			let alias = tmp.path().join("z-alias-outranking-b");
			std::os::unix::fs::symlink(&a, &alias).unwrap();
			assert_eq!(
				MutationScope::Project(a).order_rank(),
				MutationScope::Project(alias).order_rank(),
				"an alias must not get its own rank"
			);
		}
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
	///
	/// Two project roots named so they sort deterministically — NOT
	/// [`MutationScope::Global`], whose lock file is under the real `$HOME` when
	/// `XDG_STATE_HOME` is unset.
	#[test]
	fn a_nested_acquire_in_the_wrong_order_is_refused() {
		let tmp = tempfile::tempdir().unwrap();
		let lower = tmp.path().join("a-project");
		let higher = tmp.path().join("b-project");
		std::fs::create_dir_all(&lower).unwrap();
		std::fs::create_dir_all(&higher).unwrap();
		assert!(
			MutationScope::Project(lower.clone()).order_rank()
				< MutationScope::Project(higher.clone()).order_rank(),
			"fixture assumption: 'a-project' must order before 'b-project'"
		);

		let _outer = mutation_guard_with_timeout(
			"outer",
			&[MutationScope::Project(higher)],
			Duration::from_millis(200),
		)
		.unwrap();
		let error = mutation_guard_with_timeout(
			"inner",
			&[MutationScope::Project(lower)],
			Duration::from_millis(200),
		)
		.expect_err("a lock-order inversion must be refused, not attempted");
		assert!(
			error.to_string().contains("lower-ordered"),
			"unexpected message: {error}"
		);
	}

	/// One identity, before and after the directory exists. A project root the
	/// caller has never created keys the same way once acquiring the lock creates
	/// `<root>/.agents` — otherwise a nested acquire misses `HELD` and contends
	/// with its own handle until the timeout.
	#[test]
	fn a_root_that_does_not_exist_yet_keys_the_same_after_creation() {
		let tmp = tempfile::tempdir().unwrap();
		let root = tmp.path().join("not-yet");
		let scope = MutationScope::Project(root.clone());
		let before = scope.lock_path();

		let guard = mutation_guard_with_timeout(
			"create",
			std::slice::from_ref(&scope),
			Duration::from_millis(200),
		)
		.expect("a missing root is created, not refused");
		let after = scope.lock_path();
		assert_eq!(
			before, after,
			"identity moved when the root came into being"
		);

		// The nested acquire must hit `HELD`, not the filesystem.
		let nested = mutation_guard_with_timeout(
			"nested",
			&[MutationScope::Project(root)],
			Duration::from_millis(200),
		);
		assert!(
			nested.is_ok(),
			"a nested acquire contended with its own handle: {nested:?}"
		);
		drop(guard);
	}
}
