# Skill update-check: `ls-remote` preflight

- **Date**: 2026-06-05
- **Status**: Design approved, ready for implementation plan
- **Author**: audichuang (with Claude)
- **Related**: [`2026-05-31-skill-management-improvements.md`](./2026-05-31-skill-management-improvements.md)

## Context & Problem

aghub already has a network-capable skill update check, but it is **expensive**:

- The orchestrator `check_updates` (`crates/api/src/skills/update_check.rs`) groups locked
  skills by `(source, ref)` and, for each non-pinned/non-local group, performs a **full
  bare git fetch** of the entire repository history via `aghub_git::fetch_ref_to_temp`
  (`crates/git/src/fetch.rs`). gix 0.83 does not expose a `blob:none`/`tree:0` partial filter
  on the blocking path, so this downloads **all objects and full history** — just to recompute
  one folder's hash and discover that nothing changed.
- This is what makes the desktop "檢查更新" button (`GET /skills/check-updates`, used by
  `crates/desktop/src/components/skill-detail.tsx` and `pages/settings/skills.tsx`) feel slow.
- The CLI `aghub-cli check` (`crates/cli/src/commands/check.rs`) is intentionally **offline**:
  it always returns `Uncheckable` for remote sources and never touches the network.

`fetch_ref_to_temp` already returns the resolved commit `ObjectId` as a free byproduct, and
`crates/git/src/remote.rs::discover_remote_refs` can already perform a git **ref advertisement
(ls-refs)** that downloads **no objects**. The repository is therefore one cheap round trip away
from a "did anything change at all?" preflight that can skip the full fetch in the common case.

## Goals

1. Make "check updates" fast by skipping the full fetch when the upstream ref tip has not moved
   **and** the installed copy is provably unmodified.
2. Cover **both** surfaces: desktop (`GET /skills/check-updates`) and CLI (`aghub-cli check`).
3. Stay **git-native**: support private repos and non-GitHub hosts. Reuse the existing fetch and
   credential machinery. Never change the correctness of the existing hash-based comparison.

## Non-goals

- **No GitHub Trees REST API.** This remains a non-goal (per `2026-05-31` spec): it is
  GitHub-only, rate-limited, and needs a token for private repos. The preflight uses
  git ref advertisement instead.
- **No shallow/partial fetch rework** in this spec. Switching `fetch_ref_to_temp` to
  `depth=1`/`blob:none` is a complementary optimization for the *changed* case; tracked as a
  follow-up, not blocking this work.
- **No publishing, no MCP server.** Lifting the orchestrator out of `crates/api` makes a future
  MCP `check`/`update` tool cheap, but wiring MCP is out of scope here.

## Approach

Insert a **preflight** step into the existing orchestrator. Before fetching a `(source, ref)`
group, do a cheap ls-refs to learn the current upstream tip commit OID. If — and only if — every
member of the group is **provably up to date**, emit `UpToDate` and skip the fetch. Otherwise
fall through to the existing full-fetch + per-skill-hash-compare path unchanged.

The preflight is a **pure optimization**: on any ambiguity, missing data, or error it falls
through to the canonical hash comparison. It can never produce a false `UpToDate`.

### Why a commit OID is a safe signal

A git commit OID is a cryptographic hash over its tree + parents + metadata. Therefore:

- **No false negatives.** If the stored tip OID equals the current tip OID, the tree content is
  identical by construction — there cannot be an undetected upstream change.
- **False positives are harmless.** A force-push/rebase that changes the tip OID without changing
  content merely causes the preflight to fall through to the full fetch, which then correctly
  reports `UpToDate`. Zero correctness impact; only a missed optimization.

The one thing an OID match does **not** prove is that the *locally installed* copy is unmodified.
That gap is closed by the trustworthiness gate (below).

## Architecture

The async/network orchestrator is **extracted into a new dedicated crate** rather than moved into
`crates/core`. `crates/core` is a (largely synchronous) config-management library with no
`tokio`/`gix` dependency; feature-gating public async functions there would force every call site
to be feature-gated and would pull `tokio`+`gix` into the dep graph of config-only consumers.

```
crates/skill-update   NEW. Owns the orchestrator + traits + default git adapters.
                      deps (unconditional): aghub-core (pure compare/hash helpers),
                      aghub-git, skill, tokio, tempfile.
crates/git            Adds resolve_ref_oid + a credentials- and tag-aware ref advertisement.
crates/skill          lock: adds optional `refCommit` to both lock entry types.
crates/core           UNCHANGED deps. Keeps the pure helpers in core::skills::update
                      (compare_known_hashes, precheck_source, sanitize_skill_path,
                      SkillUpdateStatus, UncheckableReason, is_placeholder_digest).
crates/api            Depends on skill-update. Keeps its keyring TokenResolver.
                      Endpoint GET /skills/check-updates unchanged.
crates/cli            Depends on skill-update. Adds an env-based TokenResolver and an
                      opt-in online mode for `check`.
```

The pure status/compare helpers stay in `crates/core::skills::update` (already there); only the
async orchestration (`check_updates`, `ResultCache`, `EntryInput`, `CheckOutput`, `Fetcher`,
`TokenResolver`, and the concrete `GitFetcher`) moves from `crates/api/src/skills/update_check.rs`
into `crates/skill-update`. `crates/api` and `crates/cli` each supply their own `TokenResolver`.

## Components

### 1. `aghub_git::resolve_ref_oid` (`crates/git/src/remote.rs`)

```
fn resolve_ref_oid(
    opts: RemoteOptions<'_>,   // carries url + optional Credentials
    ref_: Option<&str>,        // branch or tag; None = remote default branch
) -> Result<String>            // 40-hex commit OID
```

- Reuses `discover_remote_refs`, which must be **broadened** to advertise `refs/tags/*` in
  addition to `refs/heads/*` (today the refspec is `+refs/heads/*:refs/remotes/origin/*`).
- `branches_from_remote_refs` is refactored to accept a ref-prefix/filter so the existing
  branch-listing callers (`list_remote_branches`) are unaffected.
- Matching: resolve `ref_` against both `refs/heads/<ref>` and `refs/tags/<ref>`. For
  **annotated tags** (`Ref::Peeled`), return the peeled `object` (the commit), not the tag object.
  When `ref_` is `None`, return the tip OID of the HEAD symbolic target.
- **Credentials**: `resolve_ref_oid` takes credentials and injects them into the URL via the
  existing `resolve_remote_url` / `inject_credentials` path (mirroring `list_remote_branches`),
  so private repos work. Errors are redacted by `GitError` as today.
- Downloads **no git objects** (ref advertisement only).

### 2. lock `refCommit` field (`crates/skill/src/lock/types.rs`)

Add to **both** entry types — `SkillLockEntry` (global v3) and `LocalSkillLockEntry` (project v1):

```rust
#[serde(rename = "refCommit", skip_serializing_if = "Option::is_none", default)]
pub ref_commit: Option<String>,
```

- **aghub-only**, additive, optional. **No schema version bump.** Round-trips with npx exactly
  like the existing `contentHash` (npx ignores unknown fields; verified pattern via
  `entry_deserializes_without_content_hash_to_none`).
- Semantics, documented inline: *"aghub-only optimization. Repo-level commit OID (SHA-1 hex) of
  the branch/tag tip at install/update time. Stored per-entry for simplicity; identical across all
  members of the same `source`+`ref` group. Never read or written by npx."*
- **Not** a replacement for `skillFolderHash` (a per-folder GitHub *tree* SHA, semantically
  distinct) nor `contentHash`/`computedHash` (aghub's SHA-256 of folder contents).

### 3. `skill-update` orchestrator + traits (`crates/skill-update/src/`)

- Moved from `crates/api/src/skills/update_check.rs`: `check_updates`, `ResultCache`,
  `EntryInput` (gains a `ref_commit: Option<String>` field), `CheckOutput`, `Fetcher`,
  `TokenResolver`, and the concrete `GitFetcher`.
- New `RefResolver` trait, symmetric to `Fetcher`:
  ```
  trait RefResolver: Send + Sync {
      fn resolve(&self, source_ref: &SourceRef, token: Option<&str>)
          -> Result<String, FetchError>;   // 40-hex tip OID
  }
  ```
  with a default `GitRefResolver` wrapping `aghub_git::resolve_ref_oid`.
- `FetchedRepo` is **extended** to also carry the resolved commit `oid: String` (the `GitFetcher`
  already obtains it from `fetch_ref_to_temp`). This is the single source of truth for OID healing
  after a real fetch.

### 4. TokenResolver implementations (unchanged location)

- `crates/api`: keyring/keychain resolver (existing, untouched).
- `crates/cli`: env resolver using `aghub_git::read_credentials()` (consistent with
  `apply_update.rs`).

### 5. CLI `check` online opt-in (`crates/cli/src/commands/check.rs`)

- **Default stays offline** (preserves the documented read-only, network-free, deterministic
  contract and the CI test `check_skills_outputs_json_array`).
- New explicit flag `--online` (alias `--check-remote`) runs the `skill-update` orchestrator with
  the env `TokenResolver`. Online mode is **read-only on the project lock** (see Healing).
- The online path builds `EntryInput` itself — including `local_hash` for each installed skill,
  computed the same way the route's `local_hashes_for_scope` does — before calling `check_updates`,
  so the C1 trustworthiness gate has its `local_hash` baseline.
- Update the `check.rs` doc comment and `crates/cli/AGENTS.md` to describe the opt-in online mode.

## The preflight decision rule (the load-bearing logic)

Inserted in `check_updates`, **after** the existing `offline` short-circuit, pinned-SHA
short-circuit, `precheck_source` (local/ssh), and `ResultCache` lookup, and **before** the fetch.
`local_hash` is already computed per entry by the route (`local_hashes_for_scope`) and is present
on `EntryInput`.

For a `(source, ref)` group, **skip the fetch and report `UpToDate`** if and only if **every**
member satisfies all three:

- **(a)** `ref_commit == <remote tip OID from RefResolver>`, and
- **(b)** `stored_hash` is present and non-placeholder (`!lock_hash_unknown(stored_hash)`), and
- **(c)** `local_hash == stored_hash` (the installed copy has not drifted).

If any member fails (a), (b), or (c) — or the group has mixed/disagreeing `ref_commit`, or the
`RefResolver` errors — the **whole group falls through** to the existing full-fetch +
`classify_member_from_probe` path, so npx legacy-lock content-hash healing and local-drift
detection still run.

> **Why all three.** OID equality (a) proves upstream is unchanged. (b)+(c) prove the local copy
> matches the recorded hash, i.e. the user has not edited the installed skill. Without (b)/(c) a
> drifted local copy would be wrongly reported `UpToDate`, and a legacy/npx entry (no
> `contentHash`) would silently skip the auto-heal that the fetch path performs.

**Implementation note (output & cache).** A preflight hit must flow through the existing
per-member path rather than a blanket `Terminal(UpToDate)` (which hardcodes `heal_hash: None` and
skips the per-member logic). Synthesize a `CachedGroup::Hashes` mapping each member's `skill_path`
to `HashProbe::Fresh(stored_hash)` and run it through `classify_member_from_probe`; because the
gate guarantees (b) `stored_hash` is known, this yields `UpToDate` with `heal_hash: None` for every
member and caches correctly per `skill_path`.

**Async placement.** `RefResolver.resolve` is synchronous blocking I/O, so it runs inside the
**same `spawn_blocking` + `per_fetch` timeout** envelope as `do_fetch`, inheriting the concurrency
semaphore, per-fetch timeout, and overall deadline. No new unbounded blocking call is added to the
synchronous pre-spawn loop.

## OID lifecycle & healing scope

`refCommit` is written at three points, but healing is **scoped by lock kind** to avoid dirtying
version control:

| Event | Global lock (`~/.agents`, untracked) | Project lock (`skills-lock.json`, **git-tracked**) |
|---|---|---|
| install | write `refCommit` | write `refCommit` |
| apply-update (`apply_update.rs:135` already has the oid) | write `refCommit` (+ `content_hash`) | write `refCommit` (+ `computed_hash`) |
| **check** (online) | self-heal: write `refCommit` + `content_hash` | **do NOT write `refCommit` by default** |

- The project lock's `refCommit` is populated only by install/apply-update (explicit, mutating
  operations), so a read-style "check" never silently dirties a VCS-tracked file. The preflight
  still works on the project lock using whatever `refCommit` install/apply-update wrote.
- Optional future opt-in (`?autoHeal=true` / CLI `--heal-lock`) may enable project-lock healing,
  but is off by default.
- All heal writes go through the existing atomic `modify_skill_lock` / `modify_local_lock` RMW,
  which does **not** rewrite an unchanged lock (byte-identity preserved for untouched entries).

## Data flow

- **A — unchanged, trustworthy (fast path):** build `EntryInput` (with `ref_commit`, `stored_hash`,
  `local_hash`) → group → preflight: one ls-refs per repo → all members pass (a)+(b)+(c) →
  `UpToDate`, cached, **0 fetches**.
- **B — upstream moved (or monorepo neighbor changed):** preflight (a) fails → full fetch →
  per-skill hash compare → some `UpToDate`/`UpdateAvailable` → heal new OID into the **global**
  lock; project lock OID left to install/apply-update.
- **C — legacy/npx entry (no `ref_commit` or no `contentHash`):** preflight (a)/(b) fails → full
  fetch → hash compare + existing content-hash heal → OID healed (global only).
- **D — pinned 40-hex SHA:** existing short-circuit → `UpToDate`, no preflight, no fetch.
- **E — CLI `aghub-cli check` (default):** offline, `Uncheckable` for remote, no network/mutation
  (unchanged). With `--online`: behaves like A/B/C using the env resolver, read-only on project
  lock.
- **F — desktop `GET /skills/check-updates`:** same endpoint, faster; heals only the global lock.

## Error handling & edge cases

| Case | Behavior |
|---|---|
| `RefResolver` auth/network/unborn-HEAD/ref-not-found | Soft failure: fall through to full fetch (which yields the correct `Uncheckable{Auth/Network}` or a real comparison). Never a hard error or false `UpToDate`. |
| ref is a tag (`v1.0`) | `resolve_ref_oid` matches `refs/tags/*`; annotated tags return the peeled commit `object`. |
| ref = default branch (`None`) | Tip OID of HEAD's symbolic target from the advertisement. |
| monorepo | Repo-level OID: any upstream commit triggers one fetch for that repo (one ls-refs proves it moved). A debug log records the tip-OID mismatch fallthrough so the limitation is observable. |
| project vs global lock | Both carry `refCommit`; healing scoped per the table above. |
| npx round-trip | `refCommit` serialized only when `Some`; unknown to npx; dropped-then-reheal is harmless. No version bump. |
| concurrency / cache / timeouts | Unchanged: per-`SourceRef` grouping (one ls-refs per repo), request-scoped `ResultCache` + TTL, bounded concurrency, per-fetch timeout, overall deadline. |

## Testing strategy

- **`crates/git`**: `resolve_ref_oid` unit tests — branch match, tag match, annotated-tag peel,
  default-branch (`None`), private-repo credential injection (URL-injection assertion), ref-not-found
  → error; plus a `#[ignore = "network"]` test against `octocat/Hello-World` mirroring existing
  fetch tests. `branches_from_remote_refs` prefix-filter unit test (heads-only still correct).
- **`crates/skill-update`**: a `StubRefResolver` (symmetric to the existing `StubFetcher`). Tests:
  - all members trustworthy + OID match → **skip fetch** (assert `fetcher.calls == 0`), `UpToDate`;
  - member with `local_hash != stored_hash` (drift) → **falls through to fetch**;
  - member with absent/placeholder `stored_hash` (legacy) → falls through; heal still runs;
  - OID mismatch → falls through; new OID healed from `FetchedRepo.oid`;
  - `RefResolver` error → falls through (no false `UpToDate`);
  - members with disagreeing `ref_commit` → falls through;
  - `RefResolver` runs under the per-fetch timeout/semaphore (reuse the existing async harness).
- **`crates/skill`**: `refCommit` serializes camelCase / omitted when `None` / deserializes absent
  → `None`; npx round-trip — read an npx lock without `refCommit`, heal one entry, re-serialize,
  assert the field lands and untouched entries are byte-preserved (extend the existing `retain_*`
  byte-identity tests).
- **`crates/api`**: `apply_update` stores `ref_commit`; `check-updates` heals only the global lock
  (project lock `refCommit` untouched by check).
- **`crates/cli`**: `check` with no flag keeps the existing offline `Uncheckable` JSON contract
  (existing test unchanged); `--online` covered by a separate `#[ignore = "network"]` test.

## Known limitations

- **Repo-level OID granularity.** In a frequently-updated monorepo the preflight rarely hits;
  it falls through to one fetch per repo (still strictly better than always fetching). Logged at
  debug level.
- **No cross-process file lock.** Simultaneous CLI + desktop heals of the same lock can lose a
  heal write; it is idempotent and self-corrects on the next check. Acceptable for the single-user
  goal; cross-process locking is a separate follow-up.

## Out of scope / follow-ups

1. Shallow `depth=1` / partial fetch in `crates/git` to also shrink the *changed*-case download.
2. Persistent (cross-request) check cache so repeated checks within a session are instant.
3. Optional `--heal-lock` / `?autoHeal=true` to populate the project lock's `refCommit` on check.
4. Exposing the orchestrator as MCP `check`/`update` tools (the `skill-update` crate makes this
   cheap).
5. Inter-process advisory file locking for lock writes.
