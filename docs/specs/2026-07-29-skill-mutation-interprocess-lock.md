# One interprocess mutation lock for the skill subsystem

Status: planned, not started. Written after v2.8.2 closed everything in this
area that a single process can close on its own.

## Problem

Every "did I create this?" signal in the skill subsystem is **process-local**, so
a flow that rolls its own writes back can act on a receipt that is a lie.

`modify_skill_lock` / `modify_local_lock` serialize through a `Mutex` in _this_
process (`crates/skill/src/lock/io.rs`, `lock/local.rs`). Two aghub processes
therefore both read an absent entry, both `insert`, and both are told they
created it. The same hole exists one layer down: `install_universal` claims the
Master with an atomic `create_dir`, which is genuine provenance for the Master
itself, but nothing serializes the **check → write → rollback** span around it.

Concretely, today:

- **rename** — `accept_rename` checks the new name is absent, installs, and on
  failure rolls back what its receipts attribute. A concurrent installer of the
  same name between the check and the failure is indistinguishable from our own
  work in the report-less `Err` path, and its lock entry can be deleted in the
  `created_lock` path.
- **prune** — scans disk, then rewrites the lock. A skill installed by another
  process in that window is pruned from the lock by a stale disk set (`prune-lock`
  module doc, window 3).
- **install / source sync** — the ownership, Master-hash and adoption guards in
  `install_fetched_skill_and_lock` all read state that another process may change
  before the write lands.

v2.8.2 narrowed this from "a concurrent writer is **always** destroyed" (the old
rollback removed the Master, every agent dir and the lock entry unconditionally)
to "a concurrent writer loses a **narrow** race". Closing it needs mutual
exclusion across processes; there is no local change that gets further.

Provenance for the current state: `crates/core/src/skills/rename.rs`
`rollback_new_only`'s residual-limit comment, `crates/cli/src/commands/prune.rs`
module doc, and the `Mutation attribution` paragraph in `crates/core/AGENTS.md`.

## Non-goals

- **Serializing against `npx skills`.** It takes no lock of ours, so this only
  serializes aghub against aghub. Say so in the docs rather than implying more.
- **Making concurrent aghub mutations fast.** Correct and serialized beats
  parallel here; the lock file is a whole-file read-modify-write anyway.
- **Locking read paths.** `doctor`, `check`, `coverage`, `source list/diff` stay
  lock-free. A torn read is already tolerated and blocking them would be a
  usability regression for no safety gain.

## Design decisions

### 1. Mechanism: OS-released advisory lock, not a lockfile

`flock` (unix) / `LockFileEx` (Windows) are released by the kernel when the
holder dies. A `create_new` lockfile scheme has to invent staleness detection,
and a crashed aghub then wedges the user's skills until they delete a file by
hand — the single worst failure mode available here.

Candidates:

| Option                          | New dep      | Stale on crash   | Notes                                                                    |
| ------------------------------- | ------------ | ---------------- | ------------------------------------------------------------------------ |
| `fd-lock`                       | yes (small)  | no (OS-released) | Purpose-built, flock + LockFileEx. Recommended.                          |
| `gix-lock` (already transitive) | promote only | yes (lockfile)   | Battle-tested, but lockfile-based → owns a staleness policy we'd inherit |
| hand-rolled `create_new`        | no           | yes              | We would be writing the staleness bug ourselves                          |

Recommend **`fd-lock`**. `gix-lock` is the zero-new-supply-chain fallback if the
dep is refused (it is already compiled in this tree via `aghub-git`) — accept its
staleness policy explicitly if so. Adding a workspace dep needs the owner's
approval per root AGENTS.md.

### 2. Granularity: one lock per lock file

Per-scope, keyed off the lock file path that already serializes everything:
`$XDG_STATE_HOME/skills/.skill-lock.json` for global, `<project>/skills-lock.json`
for project. Lock a sibling `.skill-lock.json.aghub-lock` rather than the lock
file itself, so the lock is independent of the atomic temp+rename that replaces
the target.

Per-skill locking is the tempting refinement and should be rejected for now:
every mutation rewrites the whole lock file, so per-skill locks would still need
the file lock underneath, and two locks is where deadlock ordering bugs live.

### 3. Placement: the flow acquires, not the writer

The lock must span **check → write**, so it cannot live inside `modify_*_lock`.
Add a guard in the crate that owns the lock path (`crates/skill`, next to
`lock/io.rs`), and have core flows take it at the top of the transaction:

```
skill::lock::mutation_guard(scope, project_root) -> io::Result<MutationGuard>
```

Reentrant per process: a process-local `Mutex` + a depth counter around the file
lock, so nested `modify_*_lock` calls inside a flow that already holds it do not
self-deadlock and existing call sites need no reordering. Single-shot writers
that are not inside a flow acquire it themselves, which the reentrancy makes
safe.

### 4. Blocking policy

Bounded blocking acquire (start at 10s), then an error naming the operation and
suggesting the other process — never an unbounded hang, and never a silent
non-blocking skip that would reintroduce the race it exists to remove.

### 5. Call sites that must hold it

One guard per flow, at the outermost mutating entry point:

- `core::skills::rename::accept_rename` — the whole transaction, including the
  target-absence check and the rollback
- `core::skills::install_fetched::install_fetched_skill_and_lock` — guards,
  materialize, and the lock write
- `core::skills::prune` — the scan and the rewrite (both scopes; note the CLI
  preflight in `commands/prune.rs` is a separate concern and can then be
  simplified)
- `core::skills::{resync, update}` — stage-and-swap plus lock write
- `core::manager::skill` add/remove paths
- `skill-update::mutation` — its flows call the above; verify no second
  acquisition ordering issue once reentrancy is in

## What this unlocks (do it in the same series, not before)

Once the lock exists, several v2.8.2 mitigations become simplifiable or
strictly stronger — this is the payoff, and it is also how the reviewer will
check the lock is really doing its job:

- `rename`'s report-less `Err` fallback can become attributed instead of
  clearing every new-name slot.
- The lock-write receipt becomes a true CAS, so `created_lock` /
  `replaced_*_entry` can be trusted without the process-local caveat.
- `prune`'s scan→rewrite window (module doc, window 3) closes.
- The residual-limit comments in `rename.rs` and `prune.rs`, and the
  `Mutation attribution` paragraph in `crates/core/AGENTS.md`, all get rewritten.

## Tests

The failure mode is cross-process, so **threads cannot test it** — they share the
process-local mutex and would pass without any file lock at all. That is the
single most likely way this ships broken.

- **Two-process race, must be the primary test.** Spawn the built binary twice
  against one isolated `HOME`/`XDG_STATE_HOME`; have the first hold the guard
  inside a `#[cfg(debug_assertions)]` sleep hook (same pattern as
  `AGHUB_TEST_SOURCE_FETCH_ROOT` in `crates/cli/src/commands/source.rs`), and
  assert the second blocks, then succeeds, and that both entries survive.
- **Crash releases the lock.** Kill the holder with `SIGKILL` and assert the next
  acquire succeeds without manual cleanup. This is the test that rules out a
  lockfile regression.
- **Reentrancy.** A flow holding the guard performing a nested `modify_*_lock`
  must not deadlock — assert with a timeout so a regression fails instead of
  hanging CI.
- **Timeout path.** Holder never releases → the waiter errors within the bound
  with a message naming the operation.
- Follow root AGENTS.md: revert each fix and watch the assertion go red before
  believing any of these.

## Risks

- **Hangs are worse than the race for a user.** Every acquire bounded; no
  unbounded wait anywhere.
- **A test that passes without the lock.** Covered above; treat any thread-only
  test here as a false green.
- **Windows.** `LockFileEx` semantics differ from `flock` (mandatory vs advisory,
  byte ranges). The Windows leg only runs in CI on push to main and cannot be
  reproduced locally (`crates/core/AGENTS.md` records why), so budget for a
  CI-only iteration.
- **Deadlock via lock ordering** if per-skill locks or a second lock get added
  later. Keep it at one lock while this lands.

## Rollback

Revert the series. The v2.8.2 attribution work stands on its own and does not
depend on the lock; nothing in it has to be undone.
