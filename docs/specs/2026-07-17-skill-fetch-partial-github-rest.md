# Partial skill fetch — GitHub REST fast-path + shallow-gix fallback

**Status**: ready — Codex design + spec reviews folded (2026-07-17); decomposed via `/to-tickets`
**Date**: 2026-07-17
**Area**: `aghub-git`, `skill-update`, `aghub-api` skills routes, `aghub-core`
skills install/materialize; round-trip contract with npx `skills`
(`.claude/skills/npx-skills-contract`)

> Domain vocabulary (see [CONTEXT.md](../../CONTEXT.md)): **Source**, **Source
> hash**, **Master**, **Referrer**, **skillPath**. This spec adds two internal
> terms: **RepoSnapshot** (an immutable `(commit_oid, tree_oid, commit_time)`
> pin) and **SkillCatalog** (the discovered skill list for one snapshot).

## Problem Statement

Installing, checking, or browsing a skill from a git repo is slow and wasteful,
and the cost is proportional to the **repository**, not to the skill.

Today every repo-fetching path — CLI `check` / `apply-update` / `source`, and
the desktop "Import from GitHub" scan + install — downloads the **entire repo,
including all commit history and all blobs**, materializes the whole tip tree
into a temp dir, and only **then** slices out the one skill folder it wanted:

- github path: a gix **bare fetch with `Shallow::NoChange`** — full history, all
  blobs (`aghub-git` `fetch.rs`), then `materialize_tree(root_tree)`
  (`skill-update` `git.rs`), then the skill sub-dir is selected in
  `install_fetched`.
- desktop path: a full worktree clone cached in `GitCloneSession`, reused by
  install.

So a few-KB skill (`skills/music/`) living in a large or long-history repo pulls
the whole repo down the wire. The narrowing to "just this skill" happens
**after** the bytes are already on disk — it saves disk, not network. (The
github path is not even shallow; ironically the non-github system-git fallback
uses `--depth 1` and is lighter.)

## Solution

Move the narrowing **before** the download: fetch only the latest version of only
the requested skill's files.

- **From the user's view**: on the GitHub REST path, installing / checking /
  browsing a normal skill (one that lives in a sub-directory) downloads only that
  skill's folder at the tip commit — never the rest of the repo, never history. A
  tiny skill is a tiny fetch, regardless of how big or old the repo is. (This "only
  the skill" guarantee holds on the REST success path; any **fallback** fetches the
  tip tree shallowly — history is still dropped, but the whole tip is pulled.)
- **github.com** uses the GitHub REST API (recursive git trees to list, git
  blobs to download only the selected skill's files) — the same mechanism npx
  `skills` uses in `blob.ts`, adapted to aghub's credential model.
- **Every other host, and every resolve-time failure**, transparently falls
  back to a git fetch that is now **shallow (`--depth 1`)** — history is dropped
  there too. (Fallback is decided at **resolve**; a `RestFallback` that surfaces
  _after_ a successful resolve — chiefly a `truncated` tree at `read_tree` — is a
  clean error, not a re-route. See Known limitations.)
- The installed skill is **byte-identical** to what a clone would have produced,
  so the Source hash and the npx lock round-trip are unchanged.

## User Stories

1. As a CLI user syncing skills from a source repo (`aghub source sync` — the CLI
   `source` surface that fetches a repo via `fetch_source_with_resolver` +
   `install_fetched`; the CLI `source` subcommands are list/diff/sync/accept-rename,
   and `aghub add` is local-`--from` only, NOT a remote installer), I want only the
   needed skill folders downloaded, so that syncing is lean even from a large repo.
2. As a desktop user (and any caller of `POST /skills/install`), I want a
   repo-sourced install to download only the selected skill's folder, so that the
   same lean-fetch benefit applies to the API/desktop install surface, not just
   the CLI.
3. As a CLI user, I want `aghub check --online` to detect updates without
   cloning each source repo in full, so that update checks are fast.
4. As a CLI user, I want `apply-update` and `source accept-rename` to re-fetch
   only the affected skill(s), so that applying an update is not a full re-clone.
5. As a CLI user running `source sync`, I want it to reason over the whole catalog
   (to detect renames/removals against the source) and materialize the source's
   **skill folders + root `CHANGELOG.md`** — not the whole repo, no history, no
   non-skill files — so that sync is far leaner than a full re-clone even though it
   must reason over the catalog.
6. As a desktop user, I want "Import from GitHub" to list a repo's skills without
   cloning the whole repo, so that browsing a repo is near-instant.
7. As a desktop user, I want installing a selected skill to fetch only that
   skill, so that install does not re-download everything the scan saw.
8. As a user on a private GitHub repo with a configured token, I want the fast
   path to work first-try (token sent up front), so that I never eat an
   avoidable 401 or rate-limit round-trip.
9. As a user of a non-GitHub host (GitLab, TFS, Azure DevOps, self-hosted GitHub
   Enterprise), I want fetch to still work, so that the optimization never breaks
   an install — it falls back to a shallow git fetch **that preserves the existing
   system-git + OS credential-helper path** (GCM / Windows Credential Manager) for
   hosts that authenticate that way.
10. As a user behind a flaky network or GitHub rate limit at **resolve** time, I
    want the tool to fall back to a git transport **when one is reachable** and
    succeed, so that a REST failure never produces a **wrong** install — it either
    falls back (resolve-time triggers) or surfaces a **clean error** (a
    post-resolve failure such as a `truncated` tree; a total network outage also
    fails cleanly). Correctness never depends on REST _succeeding_.
11. As a user installing a skill that carries supporting files (`scripts/`,
    `references/`), I want those fetched too, so that the skill works after
    install.
12. As a user who also uses npx `skills`, I want an aghub-installed skill's Source
    hash and lock entry to match what npx would write, so that cross-tool lock
    state is not wiped.
13. As a user installing a skill whose `SKILL.md` sits at the repo root, I want
    the whole skill (its root folder) installed, so that its supporting files are
    not silently dropped.
14. As a user, I do **not** want a pathologically large repo (one that merely
    happens to have a root `SKILL.md`) pulled in full — I want a size preflight to
    **refuse** before download with a clear error, rather than silently fall back
    to a shallow clone that pulls the whole thing.
15. As a security-conscious user, I want a fetched skill containing a symlink that
    points outside its own folder to be rejected during staging, so that a
    malicious skill can never copy host files into `.agents/skills`.
16. As a security-conscious user, I want a skill path supplied to install to be
    validated (no `..`, no absolute, no prefix escape), so that install cannot
    write outside the staging root.
17. As a desktop user, I want the skill I saw in the scan to be exactly the skill
    that gets installed even if the branch advanced meanwhile, so that the lock
    records the commit I actually installed.
18. As a maintainer, I want one shared fetch primitive used by both the CLI/update
    path and the desktop path, so that the two surfaces cannot drift.
19. As a maintainer, I want the skill list shown by desktop scan to match what the
    gix fallback would discover for the same repo — using the **existing
    case-sensitive** dedup-by-frontmatter-name semantics — so that "browse" and
    "install" never disagree and no discovery behavior silently changes.
20. As a maintainer, I want the lock to always record the **commit** OID (never a
    tree OID), so that the update preflight, lock heal, and rename transaction
    keep working.
21. As a maintainer, I want adding a future host backend (e.g. GitLab REST) to be
    a new backend behind the same seam, so that extension does not touch callers.
22. As a maintainer, I want the change to add **no new production dependency**, so
    that build weight and supply-chain surface do not grow.
23. As a CI/offline user, I want `check --offline` and network-free unit tests to
    keep passing, so that the fast path is fully testable without the network.

## Implementation Decisions

### Architecture — snapshot-first, two layers

- **`RepoFetchBackend` (in `aghub-git`) — git objects only, no skill knowledge.**
  Capabilities:
    - `resolve(source_ref, auth) -> RepoSnapshot` — resolve the requested ref to an
      **immutable** `RepoSnapshot { commit_oid, tree_oid, commit_time }`.
    - `read_tree(snapshot) -> RepoTree` — the entry listing (path, type, mode, size,
      blob oid) for that snapshot. Listing is **metadata only** — it does NOT read
      `SKILL.md` contents; producing a frontmatter-bearing catalog is the upper
      layer's job (it reads the discovered `SKILL.md` blobs, budget-accounted).
    - `read_blobs(snapshot, blob_oids) -> …` — fetch specific blobs (used both to
      read `SKILL.md` for the catalog and to materialize selected skills).
    - `materialize(snapshot, validated_paths, dest)` — write only the requested
      sub-trees to `dest`, preserving mode/symlink, through the shared
      **Source-staging** materializer.
      Implementations: **`GithubRest`** and **`GixShallow`**. `GixShallow` MUST retain
      the existing gix→system-git + OS-credential-helper fallback (today's
      `GitFetcherWithFallback`) so TFS / Azure DevOps / self-hosted GitLab private
      repos that authenticate via GCM / Windows Credential Manager keep working. A
      future `GitLabRest` is a new backend — **no generic plugin API is written now**
      (YAGNI; Story 21 is a design consequence, not new surface).
- **`SkillRepository` (concrete orchestrator in `skill-update`) — the skill-aware
  layer.** `skill-update` already depends on both `aghub-git` and `skill`, so it is
  the correct home; the discovery **policy** is a pure function that may live in
  `skill`. `SkillRepository` is NOT split across two crates.
    - `resolve(source_ref, auth) -> RepoSnapshot` — owned here; every workflow gets
      its snapshot from this one call.
    - `list(snapshot) -> SkillCatalog` — read tree, run the shared discovery policy,
      read discovered `SKILL.md` blobs for frontmatter. **`SkillCatalog` carries the
      `RepoSnapshot`** so a later `fetch` reuses the exact same commit.
    - `fetch(snapshot, selection) -> FetchedRepo` — materialize the selection. A
      caller that already knows its skill paths (install / apply-update / rename)
      calls `resolve` then `fetch` **directly, without a full `list`**.
      `resolve`/`list`/`fetch` **always share one immutable `RepoSnapshot`**; the branch
      may advance between them and it must not change what is fetched.
- **The existing `skill-update::Fetcher` cannot stay as-is**: its
  `fetch(source_ref, token) -> FetchedRepo` has no selection. It is either given a
  selection-carrying signature or replaced by `SkillRepository`; `check_updates`
  already holds each `(source, ref)` group's skill paths and must pass them
  through so the fetch is path-scoped. The fallback-routing (REST → gix →
  system-git) has a **single owner** — the `SkillRepository`/backend-composite
  layer — never re-decided per surface.
- Frontmatter parsing and discovery policy live in `skill`/`skill-update`, **not**
  in the generic `aghub-git` crate (respects the crate map in `AGENTS.md`).

### Fetch selection is a required, typed choice (not an ignorable hint)

```rust
enum FetchSelection<'a> {
    Skills(&'a [SkillPath]),   // install / update / rename: exact skill folders
    CatalogSnapshot,           // source sync/diff: every skill folder + root CHANGELOG.md
}
```

`SkillPath` is a **validated newtype** naming the repo-relative **skill folder**
(the directory that is materialized), POSIX, no `..`, no absolute, no leading `/`,
no prefix escape. It maps to the lock's npx `skillPath` (which points at
`<folder>/SKILL.md`) by appending `SKILL.md` — the newtype is the folder; the lock
field is the file inside it. A root-level skill's `SkillPath` is the empty
repo-root folder. Every surface — including desktop install, which today
raw-`join`s a client string — must pass `SkillPath`, never a raw string.

`CatalogSnapshot` exists because `source sync`/`diff` must reason over the **whole**
catalog to classify renames/removals against the source (it reads the root
`CHANGELOG.md`). It materializes **the source's catalogued skill folders + root
`CHANGELOG.md`** — far leaner than a whole-repo clone (no history, no non-skill
files), though NOT restricted to only the changed skills (a per-skill-diff sync is
a possible future optimization). `accept-rename` and `apply-update`, by contrast,
know their target and fetch `Skills(affected)` directly (no catalog).

**Per-workflow selection** (prevents surface drift):

| Workflow                                             | Catalog needed?                                                                             | Content fetched                                |
| ---------------------------------------------------- | ------------------------------------------------------------------------------------------- | ---------------------------------------------- |
| desktop install (paths known)                        | no                                                                                          | `Skills(selected)`                             |
| API `/skills/install` (client sends skill **names**) | yes — list to map names→paths (reads only the repo's `SKILL.md` blobs), then fetch selected | `Skills(selected)`                             |
| desktop scan / browse                                | yes (list only)                                                                             | none until install                             |
| `check --online`                                     | no                                                                                          | `Skills(locked)` per `(source,ref)` group      |
| `apply-update`, `source accept-rename`               | no                                                                                          | `Skills(affected)`                             |
| `source sync` / `diff`                               | `CatalogSnapshot` (classify renames/removals)                                               | catalogued skill folders + root `CHANGELOG.md` |

### GitHub REST fast-path

- **List**: recursive git trees API for the resolved commit. `truncated:true`
  (GitHub caps at ~100k entries / 7MB) surfaces AFTER resolve, so it is a **clean
  error, not a re-route** (see Known limitations); it cannot occur for a real
  single-skill repo. (A future per-level non-recursive subtree walk for a known
  path could avoid the cap, but is not implemented.)
- **Download**: git blobs API for only the selected skill's blobs, requesting the
  **raw media type** (avoid the ~33% base64 inflation); the bytes obtained are the
  stored git blob bytes.
- **Auth (locked)**: send the resolved token up front when present (keyring +
  forwarded header; no subprocess, so npx's `gh auth token` / Defender rationale
  does not apply → 5000 req/hr, private repos succeed first-try); anonymous only
  when no token (60/hr). The existing unauthenticated-first source wrapper
  (`fetch_source_with_resolver`) is updated to the same token-first policy so CLI
  `source diff/sync` is consistent.
- **Host gate**: exact `github.com` only maps to `api.github.com`. `*.github.com`
  is **not** treated as GitHub Enterprise; GHES uses custom domains → fallback.
  Origin/token pinning uses an **explicit** `github.com → api.github.com` trusted
  mapping, not a loose suffix match.
- **Request discipline**: dedup blobs by SHA; **default concurrency 6** (named
  constant, not a range); compute the request/byte budget from tree metadata
  **before** downloading and check the remaining rate-limit; write to a private
  staging `TempDir` and expose the `FetchedRepo` **only after all blobs succeed**
  (all-or-nothing); the backend accepts an **absolute deadline / cancellation**
  (the orchestrator's outer timeout wraps `spawn_blocking` and does **not** abort
  in-flight blocking HTTP, so the deadline must be threaded into the backend).

### Fallback — shallow gix `--depth 1`

Fallback is decided at **resolve** time. Resolve-time triggers (correctness never
depends on REST): non-GitHub host (incl. GHES); rate-limit (403 +
`x-ratelimit-remaining: 0`); 401/403/404; any network / timeout /
unexpected-shape error at resolve. The existing gix bare fetch gains
`with_shallow(DepthAtRemote(1))`; the desktop scan clone becomes shallow too.
`RepoSnapshot.commit_oid` / `commit_time` remain readable from the shallow tip.
Fallback is only for **backend / transient / unsupported-capability** reasons —
a **security validation failure must not be masked by a silent fallback**.
A `RestFallback` that surfaces **after** a successful resolve (chiefly tree
`truncated` at `read_tree`) is a **clean error, not a re-route** — see Known
limitations (gix 0.84 cannot re-fetch a pinned commit by OID; the trigger cannot
occur for a real single-skill repo).

### Two distinct materializers — do NOT merge them

There are **two** materialization steps with **deliberately different** semantics;
this spec touches only the first and must not change the second:

1. **Source staging (NEW, in scope)** — both backends write the fetched tree into a
   private staging `TempDir` through one shared, mode-aware, safe materializer:
    - regular file → raw bytes at the skill-root-relative path;
    - mode `100755` → set the exec bit on Unix (does not affect the hash, affects
      install semantics);
    - mode `120000` (symlink) → recreate as a symlink **only if the target stays
      inside the skill root**; reject out-of-root / absolute / cyclic targets;
    - mode `160000` (submodule/gitlink) → never written as a file; shallow fetch
      does not init submodules (absent/empty, not hashed);
    - the **minimal** safety validation needed to write REST bytes safely: path
      containment + the symlink-target check above. Broad cross-platform filename
      normalization (case-collision folding, reserved names, Unicode NFC) is **out
      of scope** (see Out of Scope); a case-collision that cannot be represented is
      reported as an error, not silently merged.
2. **Master materialization (EXISTING, unchanged)** — `install_fetched` copies the
   staged skill into the `.agents/skills` Master. It **deliberately dereferences
   symlinks and applies the npx excludes** (`.git`, `__pycache__`,
   `__pypackages__`, `metadata.json`) so the Master hashes identically to npx (see
   `linker-primitive-consolidation` spec). This spec does **not** change it.

Rejecting out-of-root symlinks at **staging** (step 1) is what closes the traversal
risk for the fetch paths: by the time step 2 dereferences, no out-of-root symlink
survives. The pre-existing local-`add --from` path (which reaches step 2 without
step 1's containment) is a **separate hardening, out of scope here** (recorded in
Further Notes).

Note: the npx excludes belong to step 2 (Master materialization), **not** to the
Source hash — the REST staging path must NOT exclude before hashing.

### RepoSnapshot & lock identity

`RepoSnapshot { commit_oid, tree_oid, commit_time }` keeps the three OIDs
distinct. The lock (`refCommit` / update preflight / rename) **always** records
the **commit** OID, never the tree OID (the GitHub trees API root `sha` is a tree
OID). Desktop session pins `commit_oid`; scan and install both operate on that
commit. A forwarded-only token (no keyring) is preserved in the session/handle to
its TTL.

### Root-level skills (SKILL.md at repo root)

Materialize the **whole root folder** (clone parity, matches npx's own clone path
/ issue #1603 and aghub's current behavior), **guarded by a tree-metadata size
preflight** computed from `read_tree` before any blob download:

- **Threshold**: reuse the existing Source-hash bounds (`MAX_SKILL_FILES` = 10,000
  files, `MAX_SKILL_BYTES` = 256 MiB in `crates/skill/src/hash.rs`) so "too big to
  be one skill" is one definition, not a second knob.
- **Over threshold → REFUSE** with a distinct machine error (e.g.
  `ROOT_SKILL_TOO_LARGE`) and a safe message. It does **not** silently fall back to
  a shallow clone (which would pull the very repo the user is trying to avoid). A
  fallback is only taken for transport reasons, never to bypass this cap.
- A normal single-skill repo (small) passes the preflight, is fetched whole, and
  hashes identically to a clone.

npx's blob fast-path "root = SKILL.md only" is a **documented upstream exception**,
not adopted.

### Discovery policy

One **shared pure function** over a tree-entry stream, fed by both the REST tree
and the gix filesystem walk, using **aghub's existing discovery semantics**
(`full_depth`, `max_depth 10`, gitignore-consistent for tracked files,
**case-sensitive** dedup by raw frontmatter name — `HashSet<String>` in
`scan.rs`, matching today's behavior; **no** case-folding change is introduced).
npx `PRIORITY_PREFIXES` semantics are **not** adopted — round-trip requires
install-hash parity, not listing parity. (`.gitignore` semantics over the REST
tree: only tracked entries appear in the tree anyway; `.git/info/exclude` and
global excludes are not reconstructable from the tree API and are intentionally
not applied — see needs-verification.)

### Desktop session slimming

`GitCloneSession` becomes a `SkillSource` handle that pins the `RepoSnapshot`
(+ token + branch + optional cached listing) and does **not** hold a whole-repo
`TempDir` on the github path. Only the gix fallback keeps a shallow clone temp
dir for reuse. `TempDir` is no longer leaked through the session type.

### Frozen (must not change)

Source-hash algorithm (`feruca` collator, SHA-256 over relativePath+raw-bytes,
symlink-skipped, mode-excluded), global lock v3, project lock v1, the Master +
symlink Referrer layout, reference-counted removal.

## Testing Decisions

**What makes a good test here**: assert observable outcomes — the exact bytes /
file set materialized, the computed Source hash, the discovered catalog, the lock
entry written, and which backend a given condition routes to — never internal
call sequences. A test must be able to fail on a real regression (a green test
that can't is worse than none — see `AGENTS.md` Testing).

**Seams (fewest, highest):**

1. **`RepoFetchBackend` trait (aghub-git)** — the injected backend seam;
   `SkillRepository` tests use a fake backend, mirroring the existing `Fetcher`
   `StubFetcher` pattern in `skill-update`. Note: `SkillRepository` / the existing
   update orchestration is the **highest** seam (closest to user outcome);
   `RepoFetchBackend` is the necessary external-dependency seam, not the primary
   acceptance seam. The existing `Fetcher` is **not** a drop-in thin adapter — it
   gains a selection-carrying signature (or is replaced), because it has no
   selection today.
2. **HTTP transport seam inside `GithubRest`** — a small injectable
   request→response function fed **canned GitHub API JSON fixtures** (tree, blob,
   429/403 rate-limit, 401, `truncated`). No new mock-server dependency. This seam
   also **records the request set**, so tests can assert what was and was not
   requested.
3. **Materializer + discovery policy as pure functions** — tested directly; these
   are where the parity and safety guarantees live.

**Key tests:**

- **Hash-parity golden** (highest value): the same fixture repo materialized via
  (a) gix clone and (b) REST-style tree-entries+blobs must be **byte-identical**
  and produce an identical `compute_skill_folder_hash`. Prior art:
  `crates/skill/tests/hash_parity_golden.rs`.
- **Backend catalog equivalence**: REST `list` and gix `list` for one repo yield
  the identical skill set (discovery unification).
- **Fallback routing**: each **resolve-time** trigger (rate-limit, 401, network,
  non-GitHub host) falls back to gix and still installs; a post-resolve
  `RestFallback` (chiefly tree `truncated`) is a **clean error, not a re-route**;
  a security-validation failure does **not** fall back.
- **Security**: a `120000` symlink pointing outside the skill root is rejected; a
  `SkillPath` with `..` / absolute / prefix-escape is rejected before any write.
- **Snapshot isolation**: with the branch advanced between `list` and `fetch`, the
  pinned `commit_oid` is fetched and recorded (not the moved branch tip).
- **OID identity**: the lock records the commit OID; a tree OID never reaches
  `refCommit`.
- **Root-level**: a small root skill fetches the whole root and hashes like a
  clone; a size-preflight-exceeding tree yields `ROOT_SKILL_TOO_LARGE` and
  **no blobs are requested**.
- **Performance acceptance (the core requirement — must be executable, not
  "near-instant" prose):**
    - **No over-fetch**: given a canned tree with unrelated large blobs, the HTTP
      seam's recorded request set contains **only** the catalog `SKILL.md` blobs
      (for `list`) or the selected skill's blob OIDs (for `fetch`) — asserting the
      unrelated blobs were **never requested**.
    - **No history**: the REST path issues **no** commits/history requests; the gix
      fallback is proven **depth-1** by a local-remote fixture where a parent commit
      / its objects are unreachable after fetch (not merely "install succeeded").
- **Network E2E** behind `#[ignore = "network"]` for the real GitHub path. Prior
  art: `crates/git` `fetch.rs` ignored tests, `aghub-api` `skills_update.rs`
  ignored network tests.

## Out of Scope

- REST fast-paths for GitLab / Bitbucket / GitHub Enterprise (they use the gix
  fallback; the backend seam allows adding them later).
- Any `skills.sh` bundle-download dependency.
- Changing the Source-hash algorithm, lock schema (v3/v1), or Master+symlink
  layout.
- Upgrading gix for native partial-clone / sparse-checkout, or moving GitHub onto
  system-git partial clone (evaluated and declined in the Decision Log).
- **Known gix 0.84 limitation:** the blocking high-level fetch exposes shallow
  history but no partial-clone blob filter or tree-metadata-only fetch. A
  non-GitHub root skill's size preflight therefore runs after its depth-1 tip
  blobs transfer, though an over-bound install is still refused before
  materialization. Refusing before transfer requires the out-of-scope gix
  partial-clone/filter upgrade above.
- **Mid-operation REST fallback is NOT re-routed to gix (documented limitation).**
  The REST→gix fallback is decided at `resolve`. A REST failure that surfaces
  _after_ a successful resolve — most notably a `truncated` recursive tree
  (GitHub caps at ~100k entries / 7MB) — surfaces as a clean error rather than
  re-routing to gix for the same commit, because gix 0.84's high-level
  `PrepareFetch::with_ref_name(<40-hex OID>)` panics (it cannot fetch a pinned
  commit by OID), and re-fetching by branch would break snapshot pinning. The
  trigger cannot occur for a real single-skill repo (a >100k-entry / >7MB tree),
  so the safe error is preferred over fragile code for an unreachable case; a
  proper fix needs the same gix partial-clone / by-OID-fetch upgrade above. The
  common path (resolve + tree + blobs all succeed) and all resolve-time triggers
  (non-GitHub host, rate-limit, 401/403/404, network, deadline) fall back
  correctly and are tested.
- Adopting npx `PRIORITY_PREFIXES` discovery semantics.
- A generic public multi-host plugin API (only the two concrete backends now).
- Broad refactors of the existing materializer beyond the mode/symlink-containment
  work required for parity and the in-scope security fix.
- **Cross-platform filename normalization** (case-collision folding, Windows
  reserved names, Unicode NFC/case-fold) — its own project; only the minimal
  path-containment + symlink-target checks needed to stage REST bytes safely are in
  scope, and an unrepresentable case-collision is an error, not silent merge.
- **Hardening the existing Master materialization / local-`add --from` path**
  against out-of-root symlink dereference — a real pre-existing issue (surfaced in
  the Codex design review), but separate from this perf change; staging containment
  already closes the fetch paths. Tracked as a follow-up.
- **A new remote-clone CLI install command** (e.g. `aghub skills add <repo>`) — the
  fetch surfaces are the existing CLI `source`, API `/skills/install`, and the
  desktop scan/install; adding a brand-new CLI verb is a separate feature with its
  own scope.

## Further Notes

### Decision Log

| #   | Decision                                                                                                                             | Alternatives considered                                                     | Why                                                                                                                                                             |
| --- | ------------------------------------------------------------------------------------------------------------------------------------ | --------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 1   | Token-first auth for the GitHub API                                                                                                  | Mirror npx anonymous-first + lazy token                                     | aghub's token has no subprocess (keyring/header), so npx's `gh`/Defender rationale does not apply; token-first = higher limit, private first-try                |
| 2   | One shared primitive in `aghub-git`, both surfaces call it                                                                           | Patch CLI and desktop separately                                            | `AGENTS.md` anti-drift rule; CLI↔API hand-mirroring has drifted before (rename txn, link primitives)                                                            |
| 3   | Snapshot-first API (`resolve`→`list`/`materialize` on one immutable snapshot)                                                        | Branch-based session; skill_paths hint                                      | Codex: prevents scan↔install TOCTOU (branch moves); makes identity explicit                                                                                     |
| 4   | GitHub REST (trees + blobs, raw media type), zero new deps                                                                           | `octocrab` SDK; system-git partial clone                                    | reqwest/base64/serde_json already present (skills-sh uses reqwest); npx also hand-rolls; SDK is over-weight for 2 endpoints                                     |
| 5   | Shallow-gix `--depth 1` fallback (resolve-time); correctness never depends on REST _succeeding_ (post-resolve failure → clean error) | REST-only; keep full-history fetch                                          | universal floor; also fixes today's non-shallow github fetch                                                                                                    |
| 6   | Keep REST + gix; **decline** system-git partial clone                                                                                | Codex-ranked #1: `git clone --filter=blob:none --sparse`                    | user decision: pure-Rust, no git-binary requirement, no github shell-out; REST is "a sound #2 with no fundamental flaw" (Codex)                                 |
| 7   | Two-layer seam (`RepoFetchBackend` in git crate, `SkillRepository` above)                                                            | Single trait carrying frontmatter/discovery                                 | keeps skill domain out of the generic git crate                                                                                                                 |
| 8   | commit OID ≠ tree OID ≠ lock refCommit                                                                                               | Reuse one `oid` field                                                       | GitHub trees root `sha` is a tree OID; misuse breaks preflight/heal/rename                                                                                      |
| 9   | Symlink parity **gated by** root-containment; reject out-of-root                                                                     | Faithfully mirror npx symlink deref                                         | closes a pre-existing traversal vector that REST would newly activate                                                                                           |
| 10  | Keep aghub discovery semantics, unify both backends onto it                                                                          | Adopt npx `PRIORITY_PREFIXES`                                               | round-trip needs install-hash parity, not listing parity; avoids behavior/test churn                                                                            |
| 11  | Root skill = whole folder + size preflight (`ROOT_SKILL_TOO_LARGE`, reuse hash bounds)                                               | npx blob "SKILL.md only"; unconditional whole-repo; silent shallow fallback | matches clone parity + npx's own clone path; preflight guards the pathological big repo; refuse (not silent-pull) honors the user's "never pull the whole repo" |
| 12  | Two **separate** materializers: new Source-staging (containment) vs existing Master (dereference + excludes, unchanged)              | One shared materializer for both                                            | Codex spec review: Master must keep npx-parity dereference/excludes; merging would change the Master hash + over-scope                                          |
| 13  | `GixShallow` retains gix→system-git + OS-credential-helper fallback                                                                  | Pure-gix only                                                               | Codex spec review: TFS/Azure/self-hosted private repos authenticate via GCM/Windows Credential Manager today; dropping it breaks them                           |
| 14  | Keep existing **case-sensitive** discovery dedup                                                                                     | Introduce case-insensitive                                                  | Codex spec review: current `scan.rs` is case-sensitive; changing it is an unrequested behavior change                                                           |
| 15  | Correct surface inventory (CLI `source`, API `/skills/install`, desktop); **no** new CLI remote-add                                  | Assume `aghub skills add <repo>` exists                                     | Codex spec review + verification: `aghub add` skill path is local-`--from` only; a new verb is separate scope                                                   |

### Motivation evidence (current-state, to be removed by this change)

`aghub-git` `fetch.rs` bare fetch is `Shallow::NoChange` (full history);
`skill-update` `git.rs` materializes the root tree; `aghub-api` `skills.rs`
desktop scan uses a full `clone_to_temp`; the skill sub-dir is selected only in
`aghub-core` `install_fetched.rs` — i.e. after the whole repo is on disk.

### Round-trip contract

Governed by `.claude/skills/npx-skills-contract` (verified against
vercel-labs/skills v1.5.19). This change touches only _how bytes arrive_; the
materialized folder, its hash, and the lock schemas are unchanged.

### Codex reviews & resolution

Two independent Codex (gpt-5.6-sol, xhigh, read-only) passes ran before this gate.

**Design review** (pre-spec) surfaced, and this spec adopted: snapshot-first with a
distinct commit/tree OID model, symlink root-containment (Critical — the existing
dereference path had no containment), the `SkillPath` newtype, one shared discovery
policy, rate-limit budget + cancelable deadline, and raw media type. It ranked
system-git partial-clone #1 over REST; that fork was put to the user, who chose to
keep REST + gix (Decision 6).

**Spec review** returned "not `ready`" with 7 P1 blockers; all were folded in:

| Codex P1                                                                                                                                                       | Resolution in this spec                                                                                                                                          |
| -------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Wrong install surface (`aghub skills add <repo>` does not exist)                                                                                               | Verified: `add` skill path is local-`--from` only. Stories 1–2 + selection matrix now name CLI `source`, `/skills/install`, desktop; new CLI verb → Out of Scope |
| `SkillRepository` interface underspecified (snapshot ownership, catalog carries snapshot, direct fetch, `SkillPath` = folder vs SKILL.md, `Fetcher` signature) | Architecture section now specifies all five; `Fetcher` explicitly gains a selection signature                                                                    |
| Single fallback owner unspecified                                                                                                                              | Fallback routing owned by the `SkillRepository`/backend-composite layer                                                                                          |
| Non-GitHub loses system-git + OS credential helper                                                                                                             | `GixShallow` retains `GitFetcherWithFallback` (Decision 13; Story 9)                                                                                             |
| Merging Source staging into Master materialization = scope creep + parity break                                                                                | Two distinct materializers (Decision 12); Master unchanged                                                                                                       |
| Root-large policy untestable (no threshold/error)                                                                                                              | Reuse hash bounds, `ROOT_SKILL_TOO_LARGE`, **refuse** (Story 14)                                                                                                 |
| Discovery claimed case-insensitive; code is case-sensitive                                                                                                     | Corrected to case-sensitive (Decision 14; Story 19)                                                                                                              |
| Core perf requirement had no acceptance test                                                                                                                   | Added request-set "no over-fetch / no history" + depth-1 fixture tests                                                                                           |

P2 folded: conditional-fallback wording (Story 10), filename normalization → Out of
Scope, named concurrency default (6).

**Still open (needs-verification, to confirm during implementation, not blockers):**
GitHub raw media type behavior on large blobs / redirects / rate-limit headers;
`DepthAtRemote(1)` correctness across branch/tag/SHA/annotated-tag; whether REST
tree discovery must emulate `.git/info/exclude` / global ignores (not obtainable
from the tree API — provisionally **not** applied); and pinning the exact npx
version evidence for the root-skill clone-vs-blob behavior into
`npx-skills-contract` (verified against v1.5.19).
