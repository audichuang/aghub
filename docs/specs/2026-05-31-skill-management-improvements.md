# Skill Management Improvements: Clean Removal + Git-Native Update Checking

- **Date:** 2026-05-31
- **Status:** Approved design, revised after adversarial review (rev 2)
- **Scope:** `crates/skill`, `crates/core`, `crates/git`, `crates/cli`, `crates/api`, `crates/desktop`

## 1. Context & motivation

aghub manages portable skills across many agents using the same on-disk
conventions and lock files as the `npx skills` (vercel-labs/skills) ecosystem:
`SKILL.md` folders, the universal `.agents/skills` directory, a global lock
(`~/.agents/.skill-lock.json`, version 3) and a project lock
(`skills-lock.json`, version 1). Source review surfaced three concrete gaps:

1. **No content hashing.** Every install writes the placeholder
   `EMPTY_SKILLS_LOCK_DIGEST` (`crates/skill/src/install.rs:8,144-164`,
   `crates/api/src/routes/skills.rs:482-486`) instead of a real hash.

2. **No skill update detection.** Nothing fetches an upstream hash and compares
   it (`crates/skill/src/lock/types.rs:24-28` defers it to "the telemetry
   server"; the CLI `update skills` only edits local metadata,
   `crates/cli/src/commands/update.rs:24-51`).

3. **Deletion leaves residue.** Deletion never prunes the lock (the prune
   functions exist but have zero production callers —
   `crates/skill/src/lock/global.rs:30-39`, `local.rs:125-137`), and because
   skills are **copied** into each agent dir, single-agent deletion leaves
   orphan copies elsewhere.

### npx interoperability constraints (verified against source)

We must not break `npx skills`, which reads/writes the same lock files. Verified
in `skill-lock.ts`, `local-lock.ts`, `update.ts`, `blob.ts`, `sync.ts`,
`add.ts`:

- npx parses locks with `JSON.parse` + a TS `as` cast — **no schema validation,
  no per-entry key iteration**. Locks wipe **only** on JSON parse error,
  `version` not a number, missing `skills`, or `version < CURRENT_VERSION`. An
  unknown `contentHash` field is inert and preserved on untouched entries; an
  empty `skillFolderHash` is a supported skip state npx itself writes
  (`add.ts:804`).
- **`computed_hash` (project lock) is behavior-bearing**, not cosmetic: npx
  `experimental_sync` recomputes the folder hash and compares it
  (`sync.ts:202-203`) to decide skip-vs-reinstall. **Byte-for-byte hash parity
  with npx's `computeSkillFolderHash` is therefore a correctness requirement.**
- **Never write a content hash into the global `skill_folder_hash`.** npx treats
  it as a GitHub tree SHA (`blob.ts:199-218`); a non-empty mismatch causes a
  false `UpdateAvailable` + reinstall in `npx skills update -g`. Leave it
  empty (`""`).
- **`skill_path` must use npx's exact form**: POSIX separators,
  `<repo-relative-dir>/SKILL.md`, case-preserving (`add.ts:1568-1575`), or npx's
  `checkAndPromptForDeletions` (`update.ts:254`) misfires. Populating project
  `skill_path` also makes `npx skills update -p` treat aghub skills as
  updatable (it will re-clone and run `add --skill`).
- npx's `addSkillToLock` rebuilds the single entry it rewrites as
  `{...entry, installedAt, updatedAt}` — dropping `contentHash` on **that one
  entry** (others preserved). aghub must treat a missing `content_hash` as
  "recompute", never as an error. `experimental_install` round-trips through
  npx `add` and overwrites `computed_hash`/`skill_path` with npx's own values —
  another reason parity is mandatory.

These hold for the currently vendored npx version (global `CURRENT_VERSION=3`,
project `=1`). Re-verify if upstream npx adds schema validation or bumps a
version.

## 2. Goals / non-goals

**Goals**

- Real, npx-compatible content hashing for installed skills.
- Git-native, per-skill update detection that works for **private repos** and
  any git host, reusing aghub's credentials — surfaced via the refresh flow.
- Deletion that leaves no residue: disk-reconciled lock pruning + correct
  handling of the `.agents` symlink layout and the copy layout.
- Full backward compatibility with `npx skills` lock files.

**Non-goals**

- No semantic versioning. No shelling out to `npx` or the `git` binary (aghub
  stays Node-free and uses gix). No GitHub Trees API (breaks private repos). No
  MCP/plugin changes.

## 3. Design overview

- **P. Content hashing** — a real SHA-256 folder hash matching npx's
  `computeSkillFolderHash`, written at install time over the **source** folder.
- **F1. Git-native update check** — treeless ref fetch via gix, recompute each
  skill folder's hash, compare to the stored hash; credential-aware; cached.
- **F2. Clean removal** — layout-aware deletion + disk-reconciled lock prune,
  with `--all-agents` and a default dry-run.

## 4. Detailed design

### P. Content hashing (`crates/skill/src/hash.rs`, new; add `sha2` dep)

`compute_skill_folder_hash(dir) -> io::Result<String>` reimplements npx's
`computeSkillFolderHash` (`local-lock.ts:108-147`) **byte-for-byte**:

1. **Collect** files recursively from `dir`. Skip directories named exactly
   `.git` or `node_modules` (case-sensitive) — **only these two**, matching
   `local-lock.ts:138` (do NOT also exclude `dist`/`build`/`__pycache__`).
   Detect symlinks via `symlink_metadata` (lstat); **skip symlinks and never
   descend into symlinked directories** (`follow_links(false)`, as in
   `scan.rs:191`). `relative_path` = path relative to `dir` with `\` → `/`.
2. **Sort** files by `relative_path`. Collation: a plain Unicode **code-point**
   sort is byte-identical to JS `localeCompare` for ASCII filenames (the
   overwhelmingly common case). For non-ASCII filenames it MAY diverge; we
   accept that and rely on the recompute-on-mismatch safeguard below rather than
   pulling in a full ICU collator. (If exact parity on non-ASCII is later
   required, pin `icu_collator`/`feruca`.)
3. **Hash** with one SHA-256: per file in sorted order,
   `update(relative_path UTF-8 bytes)` then `update(raw file bytes)` — **no
   delimiter, no length prefix**.
4. **Output** lowercase hex.
5. **Bounds** (DoS guard, since F1 hashes untrusted fetched content): abort with
   an error if the walk exceeds a max file count, max total bytes, or max
   recursion depth (reuse the existing `max_depth: 10`, `install.rs:73-77`).

**Hash the SOURCE folder, never the post-copy installed dir.** aghub's
`copy_dir_recursive` (`transfer.rs:193-207`) copies everything (incl.
`metadata.json`/`.git`) and npx's `copyDirectory` excludes some files, so the
installed dir differs from the hashed bytes. Install-time hashing and F1's
recompute must both hash the repo/source subfolder so steady-state comparison is
stable and matches npx.

### Lock schema changes

- **Project lock** (`crates/skill/src/lock/local.rs`): write the real source
  hash into the **existing** `computed_hash` field (replaces aghub's current
  placeholder). Add `skill_path` to `LocalSkillLockEntry`, written in npx's
  exact POSIX `<repo-relative-dir>/SKILL.md` form. Keep `version = 1`. Match
  npx serialization: keys sorted, two-space indent, trailing newline.
- **Global lock** (`crates/skill/src/lock/types.rs`): add an optional per-entry
  `content_hash: Option<String>` holding the source SHA-256. **Leave
  `skill_folder_hash` empty (`""`).** Keep `version = 3`.
- **serde**: new fields use
  `#[serde(rename = "...", skip_serializing_if = "Option::is_none", default)]`
  to preserve npx round-trip and old-lock readability. Update every direct
  struct literal (`install.rs:138-149`, lock tests).
- **Do not bump versions. Do not add top-level project-lock keys** (npx rebuilds
  the top level as `{version, skills}` and drops extras). All aghub data lives
  inside entries.
- **Placeholder migration / auto-heal**: when a stored hash equals
  `EMPTY_SKILLS_LOCK_DIGEST`, treat it as **unknown** — recompute the local
  folder hash and overwrite it before any upstream comparison. Otherwise F1
  would report `UpdateAvailable` for every legacy entry.
- Cosmetic/expected: `npx skills update -g` will list aghub global skills as
  "Private or deleted repo (cannot be checked automatically)" (`update.ts:181`)
  because `skill_folder_hash` is empty. This is benign; F1 is the authoritative
  path for these.

### Credential model (resolved)

The refresh-time fetch of a **private** repo needs credentials, and the desktop
path sets no `GIT_USERNAME`/`GIT_PASSWORD`; the keychain PAT lives only in an
in-memory `GitCloneSession` with no source→credential link. Chosen design:

- **Where:** the fetch + credential resolution live in **`crates/api`** (which
  has `keyring`/aghub-git). `crates/core` exposes only pure hashing/comparison
  (`compute_skill_folder_hash`, status diffing) and receives an already-resolved
  optional token as a parameter. (Do NOT place the fetch in `core`; it has
  neither dep.)
- **Resolution order per source:** (1) an optional **aghub-local, non-committed
  source→credential binding** stored in aghub settings/state (NOT in the lock —
  the project lock is VCS-committed and must carry no credential reference);
  (2) else try the stored keychain credentials, matched by host where possible,
  caching the one that works per source for the refresh session; (3) else mark
  the skill `Uncheckable { reason: auth }` and surface a credential picker /
  "add credential" affordance in the UI so the user can bind one and retry that
  source.
- HTTPS only (existing constraint). SSH/`git@` sources → `Uncheckable { ssh }`.
- **Credential leakage guard:** `inject_credentials` embeds the PAT in the URL
  (`credentials.rs:87-104`) and gix errors are formatted verbatim
  (`clone.rs:121-125`, surfaced at `skills.rs:1186-1192`). A **redaction helper
  must strip URL userinfo from every gix error string** before it becomes a
  `GitError`, an `Uncheckable` reason, a log line, or UI text. Also strip
  userinfo from `source_url`/`clone_url` before persisting (`source.rs:178-185`
  stores it verbatim today). Add tests asserting the token never appears in
  failed-fetch output or the persisted lock.

### F1. Git-native update check

`crates/core::compute_skill_folder_hash` + comparison; orchestration +
fetch in `crates/api`.

1. Read the lock for the scope; group entries by `(source, ref)` so each source
   is fetched once.
2. **Fetch** the ref with a new `crates/git` **treeless/bare** helper
   (`fetch_ref_to_temp`) — no full worktree checkout (`clone.rs:115-128` does a
   full checkout today; this is new git-crate API, not "reuse plumbing").
    - **ref handling:** `ref = None` → resolve the default branch via gix HEAD
      symref (NOT the `git` binary — `detect_current_branch` at `skills.rs:1299`
      shells out, violating the no-binary goal). Tags and pinned commit SHAs are
      fetched directly; SHA-/tag-pinned skills are reported `UpToDate` (a pin is
      intentional). Note `ref` is not universally `None` (`git_install_skills`
      records the branch, `skills.rs:1335-1339`).
    - **resilience:** a `(source, ref)` **result cache with a TTL**, a
      **concurrency bound**, a **per-fetch timeout**, and an explicit
      **offline/skip** path (network failure → `Uncheckable { network }`, refresh
      continues).
3. Locate each skill's folder via `skill_path`. **Sanitize first:** reject any
   `skill_path` that is absolute or contains `..`; canonicalize the joined path
   and verify it stays under the temp checkout root before reading. Recompute
   the source folder hash and compare to the stored hash (`content_hash` global
   / `computed_hash` project). Apply the placeholder auto-heal. Emit `UpToDate`,
   `UpdateAvailable`, or `Uncheckable { reason }`.
4. Missing `content_hash` (npx stripped it) → recompute, never error.

**Error/edge states:** `Uncheckable` reasons: `auth`, `network`, `local`
(local/path source), `ssh`/`unsupported_scheme`, `no_path` (missing
`skill_path`, hint to reinstall). All non-fatal; refresh never crashes.

### F2. Clean removal (`crates/core/src/manager/skill.rs`, `cli`, `api`)

Layout is detected from the recorded `canonical_path` (set during discovery when
a skill dir is a symlink, `discovery.rs:44-57`). **Note `canonical_path` is a
tilde-abbreviated `SKILL.md` FILE path** (`discovery.rs:51-53`) — expand the
tilde and take the PARENT directory (reuse `resolve_skill_root`,
`transfer.rs:159-174`) before any directory removal.

- **Symlink / `.agents` layout** (`canonical_path` set): full logical removal —
  delete the canonical `.agents/skills/<name>` directory **and** every symlink
  across agent dirs that resolves to it. **Containment guard:** after
  `canonicalize`, assert the resolved path is a descendant of an allow-listed
  skills root (`~/.config/agents/skills`, `~/.agents/skills`,
  `<project>/.agents/skills`, the agent's own skills dir) before any
  `remove_dir_all`; otherwise skip + warn. Re-check the symlink type at delete
  time (TOCTOU). Before deleting the canonical, verify no other agent/project
  view still symlinks to it (treat `canonicalize` failure as skip+warn, not "no
  match"). Match by recorded canonical identity, not sanitized name alone.
- **Copy layout** (no `canonical_path`): default removes only the targeted
  agent's copy. `--all-agents` scans every agent and removes each same-named
  copy.
- **Safety:** removal defaults to **`--dry-run`** returning the exact path list;
  destructive execution of `--all-agents` or a symlink-layout full removal
  requires an explicit confirm flag. The API delete response includes a
  removed-path summary.
- **Both delete routes:** apply this to `DELETE /agents/<agent>/skills/<name>`
  **and** `DELETE /skills/by-path` (`skills.rs:217-282`, which today does a raw
  `remove_dir_all` with no symlink awareness and no lock prune), or unify them.

### Lock prune (renamed from "reconcile" to avoid collision)

A non-destructive add/remove `reconcile` already exists
(`transfer::reconcile_skill`, `POST /skills/reconcile`). This disk-driven
**prune** is a different, destructive operation: CLI `skills prune-lock`, route
`/skills/prune-lock`.

The lock is keyed by skill **name** per scope with **no per-agent field**
(`types.rs:57`), so pruning is disk-driven:

> After a removal (or on demand), re-scan the scope's disk. If **no agent** in
> that scope still has the skill, prune the lock entry (wire up the existing
> `remove_skill_from_lock` / `remove_skill_from_local_lock`); otherwise keep it.

Guards (this edits a VCS-committed file, so data loss is the risk):

- **Only prune on a provably successful scan.** Use the error-returning
  `scan_skills`, NOT the discovery collector that swallows read errors
  (`discovery.rs:34-36 let Ok(entries) = ... else { return }`). Abort all
  pruning if any scan errors.
- **Per-scope disk sets:** global prune scans the **union of all agents' global
  skill dirs**; project prune scans **only the project's skill dirs**. A project
  refresh must never prune global entries, and vice versa.
- **Gate project prune** on a present + readable `project_root`
  (`descriptor.rs:214-227`); skip project prune entirely in global/no-root
  refreshes.
- The disk scan is **lock-independent**, so an on-disk skill with no lock entry
  keeps a same-named entry alive (no spurious prune).
- **No automatic destructive prune on every refresh without a guard.** Refresh
  runs prune only when the scan succeeded and (for project scope) an explicit
  action or a dry-run preview is in effect.
- **Atomicity:** lock writes are whole-file today with no locking
  (`io.rs:23-60`). Use atomic write + rename and a lock-file mutex covering
  prune + check so a concurrent npx writer cannot cause last-writer-wins data
  loss; define half-pruned-on-interruption behavior.

## 5. Components / files

| Crate            | Change                                                                                                                                                                                                                                                                                                                                                                                            |
| ---------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `crates/skill`   | New `hash.rs` (+ `sha2` dep, bounds, lstat); `install.rs` writes real **source** hash + `content_hash`, stops the placeholder, `write_project_install_lock` **signature changes** to take the source dir; `lock/types.rs` + `content_hash` (serde-optional); `lock/local.rs` + `skill_path`; wire `remove_skill_from_lock`/`remove_skill_from_local_lock`; update tests asserting the placeholder |
| `crates/core`    | New update module: **pure** hash/compare + `SkillUpdateStatus`; `manager/skill.rs` layout-aware removal (canonical=file→parent, containment, TOCTOU); prune helper using `scan_skills`                                                                                                                                                                                                            |
| `crates/git`     | New treeless `fetch_ref_to_temp`; default-branch via gix HEAD symref; tag/SHA fetch; **redaction helper** (strip URL userinfo from errors); userinfo stripping in `source.rs`                                                                                                                                                                                                                     |
| `crates/cli`     | `--all-agents` + `--dry-run`/confirm on `delete`; `check`/`update`; `prune-lock`                                                                                                                                                                                                                                                                                                                  |
| `crates/api`     | **Credential resolution** (keychain + source binding) + fetch orchestration; update-check route; delete dry-run/confirm + removed-path summary; atomic lock writes; redaction applied to `skills.rs:1189`                                                                                                                                                                                         |
| `crates/desktop` | Refresh triggers update-check (async loading/error/auth states); per-skill "update available" badge; **badge reads `content_hash`** (not the now-empty `skill_folder_hash`, `skill-detail.tsx:129,405-411`); credential picker on `Uncheckable{auth}`                                                                                                                                             |

**`SkillUpdateStatus` DTO** (`UpToDate | UpdateAvailable | Uncheckable{reason}`)
is defined in core and serialized for the API/desktop; the badge maps onto the
existing source-grouped, sorted skill list (`skill-list.tsx:210-225`).

## 6. Data flow — refresh

1. User refreshes.
2. Disk re-scan via `scan_skills` (error-returning) per scope.
3. **Lock prune** — only if the scan succeeded; per-scope disk set; project
   prune gated on a readable `project_root`; atomic write under the lock mutex.
4. **Update check** — group by `(source, ref)`; resolve credentials (api); use
   the result cache (TTL) else treeless-fetch with timeout/concurrency bound;
   sanitize `skill_path`; recompute source hash (auto-heal placeholder); diff.
5. UI renders badges; `Uncheckable{auth}` offers a credential picker.

## 7. Testing

- **Unit:** hash parity with npx — a **CI-blocking** golden cross-check over a
  fixture (including **uppercase/lowercase/accented filenames** to exercise the
  code-point-vs-`localeCompare` divergence and the recompute safeguard); hasher
  bounds + symlink-skip; `skill_path` traversal rejection (absolute/`..`);
  credential redaction (token never in error/lock output); prune scope isolation
  (a project refresh never prunes a global entry); prune aborts on scan error;
  placeholder auto-heal; layout detection (symlink vs copy).
- **Integration** (fixture repo + fake agent dirs, following
  `crates/cli/tests` / `descriptor_regression`): install → mutate upstream →
  `UpdateAvailable`; symlink-layout delete → canonical + all symlinks + lock
  gone; **out-of-tree symlink → asserts NO out-of-tree deletion**; copy delete
  single vs `--all-agents`; `--dry-run` lists paths and deletes nothing;
  manual-`rm` residue → prune clears the lock; private repo check succeeds with
  a credential and reports `Uncheckable{auth}` without one (no crash); credentialed
  source URL → persisted `sourceUrl` carries no userinfo.

## 8. Rollout / fork plan

- Work on a fork of `AkaraChen/aghub`; implement on a feature branch in an
  isolated worktree.
- **npx-facing changes are additive/backward-compatible** (optional
  `content_hash`/`skill_path`, empty `skill_folder_hash`, unchanged versions).
- **Internally, several non-additive updates are required and intentional:**
  the emptied `skill_folder_hash` changes the desktop badge source (→ read
  `content_hash`), `write_project_install_lock` changes signature, and the
  existing `write_project_install_lock_uses_placeholder_hash` test must be
  updated.
- Upstream PR candidates: the lock-prune-on-delete fix (bug) and the git-native
  update check (feature).

## 9. Open questions / resolved decisions

**Resolved in this revision:** credential model (api-side, source binding +
keychain + auth prompt; never in the committed lock); destructive prune is
scan-success-gated, per-scope, atomic, renamed `prune-lock`; symlink sweep is
containment-checked; hash is over the source folder, `.git`/`node_modules` only,
with bounds and code-point sort + recompute safeguard; placeholder auto-heal;
ref handling via gix symref/tag/SHA; treeless fetch with cache/TTL/timeout;
credential redaction.

**Remaining (non-blocking) open items:**

- **ICU exact non-ASCII collation:** deferred — code-point sort + recompute
  safeguard is accepted; revisit with `icu_collator`/`feruca` only if non-ASCII
  skill filenames prove common.
- **Windows:** normalize `\\?\` verbatim/UNC prefixes before canonical-path
  comparison; the symlink sweep is largely inert on Windows (copy layout);
  document as a known limitation for now.
- Re-verify the npx-interop guarantees against the exact npx version users run
  if upstream adds schema validation or bumps a lock version.
