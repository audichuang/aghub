# Extract the skill-rename transaction into one deep core module

**Status**: proposed → implementing on `refactor/codebase-deepening` (after C)
**Scope**: candidate A from the 2026-07-15 architecture review. Depends on C
(the transaction's snapshot/restore now call `Linker::symlink` /
`Linker::copy_preserving_links`).

## Problem

`source accept-rename` (CLI) and the `accept-rename` route (API) each run the
**same** transactional rename:

1. read the OLD-name lock entry for source coordinates
2. target agents = those that actually have the old name installed
3. fetch upstream
4. locate the skill file in the fetched tree (containment)
5. verify the fetched SKILL.md name matches `new_name`
6. **snapshot** old-name dirs + clone old lock entry (before any mutation)
7. **install** the new name (rolls back on failure)
8. **remove** the old name (rolls back on failure)
9. **remove** the old lock entry (rolls back on failure)

plus three data-loss guards — **P0-1** (install writes master/link before the
lock, so an install `Err` may leave a half-installed new name → full rollback),
**P0-2** (refuse a degenerate same-sanitized rename, and refuse when `new_name`
already exists, because the cleanup deletes _every_ `new_name` path), **P0-3**
(a snapshot failure aborts before install) — and a best-effort `rollback_all`
that restores dirs + lock to the pre-transaction state.

The two implementations are logically identical and **mirrored by hand** — the
code documents this five times (`Mirrors the API's …`). They differ only in
surface concerns:

|               | CLI `accept_rename`                      | API `accept_rename_inner`                  |
| ------------- | ---------------------------------------- | ------------------------------------------ |
| error channel | `Result<()>` + `bail!` + `println!`      | `Json<AcceptRenameResponse>` + error codes |
| output        | human / `--json`                         | structured DTO                             |
| args          | `-g`/`-p`/`--all`, `--yes`, `--ref`      | `scope: String`, `confirm`                 |
| fetch/auth    | lazy-auth (`fetch_source_with_resolver`) | inject `Fetcher` + resolve-first           |

The transaction — the part that can **delete a user's Master on a bad
rollback** — has tests on the API side (`accept_rename_inner_*`, 5 of them) and
**zero tests on the CLI side**. A rollback fix must be applied twice by hand.

## Dependency constraint (shapes the seam)

`skill-update` depends on `aghub-core`; therefore **core cannot depend on
skill-update**. The git fetch machinery (`Fetcher`, `TokenResolver`,
`SourceRef`, `FetchError`, `fetch_source_with_resolver`) lives in skill-update,
_above_ core. But everything the transaction actually mutates —
`install_fetched::install_fetched_skill_and_lock`, `removal::plan_removal` /
`execute_removal`, `linker`, `skills::update::sanitize_skill_path`, the `skill`
lock read/write — is already **core-level**.

So the seam is: **core owns the transaction; the adapter owns the fetch.** The
adapter fetches (its own auth strategy) and hands core an already-fetched
`repo_root` + `oid`. Core never fetches, so it needs no injected `Fetcher` — and
its test surface is simply "a tempdir containing a SKILL.md", exactly how the
API tests already fake the fetch (`LocalRepoFetcher`).

## Solution

New module `crates/core/src/skills/rename.rs`. The `RenameLockSource` struct
(currently duplicated in both surfaces) moves here.

```rust
pub struct RenameLockSource {           // was duplicated CLI + API
    pub source: String,
    pub source_type: String,
    pub source_url: String,
    pub ref_name: Option<String>,
    pub skill_path: String,
}

pub struct RenameRequest<'a> {
    pub old_name: &'a str,
    pub new_name: &'a str,
    pub scope: ResourceScope,           // GlobalOnly | ProjectOnly
    pub project_root: Option<&'a Path>,
}

/// Everything the adapter learned by fetching, in types core can name.
pub struct FetchedRename<'a> {
    pub repo_root: &'a Path,
    pub oid: &'a str,
    /// The effective source coords to write into the new lock entry
    /// (adapter has already applied any `--ref` override into `ref_name`).
    pub source: &'a RenameLockSource,
}

pub struct RenameSuccess {
    pub installed_hash: String,
    pub paths: Vec<String>,
}

/// Precondition + transaction failures, surface-agnostic. Each adapter maps
/// these to its own channel (CLI message / API message+code).
pub enum RenameError {
    SameSanitizedName,
    NotLocked,
    NoInstalledCopy,
    TargetExists,                       // API → RENAME_TARGET_EXISTS_CODE
    SkillPathNotFound,
    NameMismatch { declared: String, expected: String },
    ParseFailed(String),
    InstallFailed(String),
    RemovalFailed(String),
    LockRemovalFailed(String),
}

/// Step 1, kept public so the adapter can read the fetch coordinates before it
/// fetches. Reads the OLD-name lock entry.
pub fn rename_source_from_lock(
    old_name: &str,
    scope: ResourceScope,
    project_root: Option<&Path>,
) -> Result<RenameLockSource, RenameError>;

/// Steps 2,4,5,6,7,8,9 + P0-1/2/3 guards + snapshot/rollback. NO fetch, NO I/O
/// injection — the dangerous transaction, in one place, tested through here.
pub fn accept_rename(
    req: RenameRequest,
    fetched: FetchedRename,
) -> Result<RenameSuccess, RenameError>;
```

The snapshot/restore/rollback helpers (`SkillSnapshot`, `snapshot_old_skill`,
`restore_snapshot`, `rollback_rename_install`, `new_name_exists_in_scope`,
`remove_lock_entry`, `restore_lock_entry`) move into this module as its private
implementation. They already call the C primitives (`Linker::symlink`,
`Linker::copy_preserving_links`).

### Adapter shape after extraction

CLI `accept_rename` (source.rs) becomes:

```
parse args → scope + dry-run(--yes) handling (CLI-only)
let source = rename::rename_source_from_lock(old, scope, root)?;   // or map err → bail
let repo = fetch_source_with_resolver(SourceRef{source.source_url, effective_ref}, CliFetcher, EnvTokenResolver)?;  // CLI auth
source.ref_name = effective_ref;   // apply --ref
match rename::accept_rename(req, FetchedRename{repo.root, repo.oid, &source}) {
    Ok(ok)  => println! human / json,
    Err(e)  => bail!(message_for(e)),
}
```

API `accept_rename_inner` becomes the analogous adapter: request → scope +
`confirm` check (API-only), fetch via injected `Fetcher`+`TokenResolver`, call
core, map `RenameSuccess`/`RenameError` → `AcceptRenameResponse` (+ codes).

Both surfaces **delete** their copies of the transaction and all seven helpers.

## Interface (the test surface)

`accept_rename(req, fetched) -> Result<RenameSuccess, RenameError>`. Depth: nine
ordered steps, three data-loss guards, and a best-effort rollback behind one
call. Tested with a tempdir `repo_root` + isolated `$HOME`/lock state — no git,
no network.

## Dependency category

**Local-substitutable.** The one external thing (git fetch) is lifted OUT of the
module by design; what remains is filesystem + lock state, tested with tempdirs.

## Tests (move to `rename.rs`; port the API's 5, replace the CLI's 0)

Written against the core interface:

1. `rejects_confirm/degenerate_sanitized_collision` → `SameSanitizedName`.
2. `rejects_when_new_name_already_installed` → `TargetExists`.
3. `installs_new_and_removes_old` (happy path) → `RenameSuccess`, old gone, new
   present, lock transitioned.
4. `rollback_on_removal_failure` — force step 8 to fail; assert old dirs + lock
   fully restored, new-name paths cleaned (the P0-1 rollback).
5. `rollback_on_install_failure` — force step 7 to fail; assert pre-txn state.
6. `name_mismatch_aborts_before_mutation` → `NameMismatch`, nothing touched.

The API keeps a thin route-level test that the adapter maps
`RenameError::TargetExists` → the response code, and its network E2E stays.

## Non-goals

- No change to the fetch/auth strategies (they stay per-surface — they are not
  the data-loss risk, and they legitimately differ).
- No change to ADR-0001's rollback scope (rename + relink only). This gives that
  decision one home rather than two.
- No behavior change: same guards, same ordering, same messages/codes.

## Wins

- **locality**: P0 guards + rollback live once; a fix lands once.
- **leverage**: one interface, CLI + API + (via API) desktop.
- **interface is the test surface**: the transaction is tested once with
  tempdirs; both surfaces inherit it. The CLI's untested copy is deleted.
- source.rs sheds the ~320-line `accept_rename` + ~230 lines of helpers.

## Implementation notes (ordering)

The split moved the git fetch into the adapters (before the `accept_rename`
call), which shifts two pre-checks relative to the fetch:

- **Degenerate-name guard (P0-2a)** stays **before** any lock read or fetch:
  it is a core fn `ensure_distinct_names` that each adapter calls first (and
  `accept_rename` re-checks defensively). Behaviour + message + code preserved.
- **"No installed copy" (step 2)** now runs **inside** `accept_rename`, i.e.
  after the adapter's fetch, where both surfaces previously checked before the
  fetch. A locked-but-uninstalled skill therefore fetches once before failing —
  a rare error path; the data-safety ordering (all guards still precede any
  mutation) is unchanged.

The `--ref` override stays a CLI concern: the CLI adapter writes the effective
ref into `RenameLockSource.ref_name` before handing it to the transaction, so
the new lock entry records it.

## Tests

- `crates/core/tests/rename_tests.rs` (new, own binary + env lock): drives
  `accept_rename` directly with a tempdir repo — happy path, name-mismatch
  (nothing mutated), and rollback-on-removal-failure (old skill survives).
- `rename.rs` unit tests: `RenameError` code/message contract +
  `ensure_distinct_names`.
- The API keeps its route-level integration tests (they now exercise the core
  transaction through the thin adapter, confirming the wiring + error→code
  mapping). The CLI e2e `source accept-rename` tests are unchanged and green.

## Codex-review follow-up (2026-07-16)

Codex's adversarial pass confirmed the extraction preserves the transaction
sequence, closure captures, guards, `--ref`/`ref_commit`/`LinkTarget`/code
mapping, and the fetch-stays-in-adapter boundary. Two findings were fixed in
this phase (small, safety-positive, within the extracted code):

- **Snapshot skip-on-error (was silent data-loss risk).** `snapshot_old_skill`
  now aborts on any non-`NotFound` stat error instead of treating it as
  "absent" — proceeding without a backup could lose the old skill on rollback.
- **API path leak (Minor).** A partial-removal failure logged the per-path
  detail but now returns a path-free message, so a surface forwarding it
  verbatim keeps the API "no raw paths in errors" contract.

The following are **pre-existing** issues, inherited unchanged from BOTH the CLI
and API originals (not introduced by this extraction). They are deferred to a
dedicated **"harden rename transaction"** follow-up — the extraction's payoff is
that they are now fixable in ONE place. They are NOT fixed here because rushing a
rollback-recovery / cross-process-locking rewrite in safety-critical code risks
introducing the very data-loss it aims to prevent:

- **Cross-process TOCTOU (P0-2b).** `new_name_exists_in_scope` is a
  time-of-check gate, not an ownership guarantee: a concurrent process creating
  `new_name` between the check and install can have this transaction adopt and
  then (on rollback) delete data it did not create. A real fix needs a
  cross-process transaction lock + atomic no-clobber creation.
- **Rollback is best-effort but discards its own failures (ADR-0001 gap).**
  `rollback_all` suppresses errors and the snapshot backup is dropped on the way
  out, so a failed restore leaves no recovery path. ADR-0001 asks for a compound
  error naming the rollback failure + the affected Master, and the backup should
  be retained when restoration fails. Both originals already violated this.
- **Core interface is misusable.** `RenameLockSource` is freely constructible
  and `accept_rename` does not re-verify the old lock at transaction time, so a
  direct core caller with fabricated coordinates could rename an unmanaged
  skill. Production is safe (both adapters obtain the source via
  `rename_source_from_lock`, which requires the lock), but the follow-up should
  make the value non-forgeable (a prepared value returned by
  `rename_source_from_lock`) and/or re-assert the lock precondition, then seed
  the lock in the core tests (adds `skill` as a core dev-dependency) to assert
  the full old/new lock state across the rollback branches.

## Rollback

Revert the phase commit. No lock-format / on-disk / DTO changes — messages and
the one error code are preserved through `RenameError` mapping.
