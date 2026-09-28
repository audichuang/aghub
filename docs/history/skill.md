# skill crate history

Why some rules in `crates/skill` look the way they do. The code comment keeps
the current rule; this file keeps what happened.

## Mutation lock no unlocked fallback

An earlier revision of `lock_file` (`src/lock/guard.rs`) degraded to a
`log::warn` and proceeded UNLOCKED when the lock file could not be created or
the filesystem rejected locking, on the theory that refusing to work was worse
than the pre-lock status quo. It was wrong twice over: a warning is invisible
in the desktop/API, and it masked a total failure — the lock file was opened
append-only, which makes `try_lock` fail on ALL of Windows, so the degrade path
would have shipped Windows with no interprocess lock and no visible symptom.

Rule: every failure to open or lock is an actionable error (a stale root-owned
lock file, a mount that allows I/O but rejects locking included), and the file
is opened read+write.

Pinned by: `lock::guard::tests::an_unlockable_location_refuses_instead_of_running_unlocked`.
Commit: 6ecb5862.

## Bounded process mutex failed queued work

`lock_process` once bounded its wait on the in-process mutex like the file
wait. Measured, not assumed: a 200ms acquire in this crate's own suite failed
as soon as the other lock tests ran alongside it — the exact shape of a desktop
bulk operation over N skills, where the last one queues behind the other N-1.

Rule: the process-mutex wait is unbounded (it waits on our own code, which
finishes or fails on its own); only the wait on FOREIGN processes is bounded.
A genuinely hung flow is a bug to fix in that flow. The process mutex also
replaced the two per-module write mutexes the lock writers used to take
(5850bac6), and adopts a poisoned lock exactly as they did.

Commit: 6ecb5862.

## Fail closed lock reads for reporting commands

The lock read paths fail OPEN to an empty lock so a corrupt lock does not break
every query. `check skills`, `source list` and `doctor` present the lock's
CONTENTS as their answer, and each reported "nothing installed" for "I could
not read the file" — on exit 0 with an empty stderr — and `doctor` went on to
recommend deleting the skills it had just failed to see (b610db98 era).

The first fix was a predicate-only probe (`*_lock_readable`, 7b2ee2d4). It left
the caller to read the file a second time through the fail-open reader, and
between those reads a non-aghub writer (an editor, `npx skills`) could truncate
it, so the command answered `[]` on exit 0 anyway.

Rule: `read_global_lock_checked` / `read_local_lock_checked` return the parsed
lock from ONE read, and go through the versioned reader so the old-format wipe
still applies (otherwise v2 entries the fail-open reader treats as empty would
be resurrected).

Pinned by: `lock::io::tests::checked_read_applies_the_old_format_wipe`.
