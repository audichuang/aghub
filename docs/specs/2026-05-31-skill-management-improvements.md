# Skill Management Improvements: Clean Removal + Git-Native Update Checking

- **Date:** 2026-05-31
- **Status:** Approved design (pre-implementation)
- **Scope:** `crates/skill`, `crates/core`, `crates/git`, `crates/cli`, `crates/api`, `crates/desktop`

## 1. Context & motivation

aghub manages portable skills across many agents using the same on-disk
conventions and lock files as the `npx skills` (vercel-labs/skills) ecosystem:
`SKILL.md` folders, the universal `.agents/skills` directory, a global lock
(`~/.agents/.skill-lock.json`, version 3) and a project lock
(`skills-lock.json`, version 1). Source review surfaced three concrete gaps:

1. **No content hashing.** Every install writes the placeholder
   `EMPTY_SKILLS_LOCK_DIGEST` (`crates/skill/src/install.rs:8,144-164`,
   `crates/api/src/routes/skills.rs:482-486`) instead of a real hash. There is
   no real version fingerprint to compare against.

2. **No skill update detection.** Nothing fetches an upstream hash and compares
   it. The `skill_folder_hash` doc-comment defers this to "the telemetry
   server" (`crates/skill/src/lock/types.rs:24-28`). The CLI `update skills`
   only edits local metadata (`crates/cli/src/commands/update.rs:24-51`). The
   desktop refresh re-scans disk but never checks upstream.

3. **Deletion leaves residue.** Deletion never prunes the lock — the prune
   functions exist but have zero production callers
   (`crates/skill/src/lock/global.rs:30-39`,
   `crates/skill/src/lock/local.rs:125-137`, called only from tests). Because
   skills are **copied** into each agent dir (not symlinked), single-agent
   deletion leaves orphan copies in other agents.

### npx interoperability constraint (verified against source)

We must not break `npx skills`, which reads/writes the same lock files. Source
review of `skill-lock.ts`, `local-lock.ts`, `update.ts`, `blob.ts` confirms:

- npx parses locks with `JSON.parse` + a TypeScript `as` cast. There is **no
  runtime schema validation** (no zod/ajv/joi) and **no per-entry key
  iteration**. Locks are wiped **only** on: JSON parse error, `version` not a
  number, missing `skills`, or `version < CURRENT_VERSION`.
- An **unknown extra field** on an entry (e.g. `contentHash`) is therefore
  inert: it never triggers a wipe and is preserved verbatim on entries npx does
  not touch (`writeSkillLock`/`writeLocalLock` serialize the whole object).
- An **empty `skillFolderHash`** is a supported state: npx skips such entries
  (`update.ts:315` → `skipped`, reason "Private or deleted repo") and never
  errors. npx itself writes `skillFolderHash: ''` for well-known skills
  (`add.ts:804`).

These guarantees hold for the currently vendored npx version (global lock
`CURRENT_VERSION=3`, project lock `CURRENT_VERSION=1`). They must be re-checked
if upstream npx adds schema validation or bumps a version.

## 2. Goals / non-goals

**Goals**

- Real, npx-compatible content hashing for installed skills.
- Git-native, per-skill update detection that works for **private repos** and
  any git host, reusing aghub's existing credentials — surfaced through the
  existing refresh button.
- Deletion that leaves no residue: prune the lock (disk-reconciled) and handle
  the `.agents` symlink layout and copy layout correctly.
- Full backward compatibility with `npx skills` lock files.

**Non-goals**

- No semantic versioning (the ecosystem has none; version = git ref + content
  hash).
- No shelling out to the `npx` CLI (aghub stays Node-free).
- No GitHub Trees API dependency (it breaks on private repos — the exact pain
  we are solving).
- No change to MCP/plugin handling.

## 3. Design overview

Three workstreams:

- **P. Content hashing** — a real SHA-256 folder hash matching npx's
  `computeSkillFolderHash`, written at install time.
- **F1. Git-native update check** — shallow-fetch each source ref via gix,
  recompute each skill folder's hash, compare to the stored hash; runs on
  refresh.
- **F2. Clean removal** — layout-aware deletion + disk-reconciled lock pruning,
  with an `--all-agents` sweep for the copy layout.

## 4. Detailed design

### P. Content hashing (`crates/skill/src/hash.rs`, new)

`pub fn compute_skill_folder_hash(dir: &Path) -> io::Result<String>` reimplements
npx's `computeSkillFolderHash` (`local-lock.ts:108-147`) **byte-for-byte**:

1. **Collect** files recursively from `dir`. Skip directories named exactly
   `.git` or `node_modules` (case-sensitive). Read regular files as raw bytes.
   Skip symlinks and non-regular entries. Hidden/empty files are included.
   `relative_path` = path relative to `dir` with all `\` replaced by `/`.
2. **Sort** files by `relative_path` using **ICU/Unicode collation** equivalent
   to JavaScript `String.prototype.localeCompare` — **not** a bytewise or
   codepoint sort. A naive sort can diverge on mixed-case/accented/punctuated
   filenames.
3. **Hash** with a single SHA-256: for each file in sorted order,
   `update(relative_path as UTF-8 bytes)` then `update(raw file bytes)`, with
   **no delimiter and no length prefix** anywhere.
4. **Output** lowercase hex.

This hash is reused by P (install), F1 (update check), and the cross-check test.

### Lock schema changes

- **Project lock** (`crates/skill/src/lock/local.rs`): write the real hash into
  the **existing** `computed_hash` field (replaces the placeholder). Add the
  **`skill_path`** field to `LocalSkillLockEntry` (needed by F1 to locate the
  folder in a fetched repo; npx's project lock already supports an optional
  `skillPath`). Keep `version = 1`.
- **Global lock** (`crates/skill/src/lock/types.rs`): add a new optional
  per-entry field **`content_hash: Option<String>`** holding the SHA-256.
  **Leave `skill_folder_hash` as an empty string** (so npx cleanly skips the
  entry instead of misfiring an update check). Keep `version = 3`.
- **Do not bump either version.** Do not add new **top-level** keys to the
  project lock — npx's `writeLocalLock` rebuilds the top level as exactly
  `{ version, skills }` and would drop them. All aghub-specific data lives
  inside entry objects.
- Match npx's project-lock serialization: skill keys sorted ascending, two-space
  indent, trailing newline (`local-lock.ts:89-101`) to keep VCS diffs clean.

### Install changes (`crates/skill/src/install.rs`, `crates/api/src/routes/skills.rs`)

- Compute the real hash with `compute_skill_folder_hash` and write it:
  `computed_hash` (project) and `content_hash` (global). Stop writing the
  placeholder into `skill_folder_hash`; write `""` there for global entries.
- Record `skill_path` in both locks.

### F1. Git-native update check (`crates/core/src/update.rs`, new)

`check_skill_updates(scope) -> Vec<SkillUpdateStatus>`:

1. Read the lock for `scope`; group entries by `(source, ref)` so each source is
   fetched once.
2. For each group, shallow-fetch the ref into a temp dir via a new
   `crates/git` helper (`fetch_ref_to_temp`, reusing `clone_to_temp` plumbing
   and the existing credential path — `GIT_USERNAME`/`GIT_PASSWORD` or the
   keychain PAT). HTTPS only, same as today.
3. For each skill in the group, locate its folder via `skill_path`, compute the
   folder hash, and compare to the stored hash (`content_hash` global /
   `computed_hash` project). Emit `UpToDate`, `UpdateAvailable`, or
   `Uncheckable { reason }`.
4. Because npx's `addSkillToLock` strips `content_hash` from the single entry it
   rewrites, **treat a missing `content_hash` as "recompute"**, never as an
   error.

**Refresh integration** (`crates/desktop` + `crates/api`): the existing refresh
flow runs, in order: (a) disk re-scan (existing), (b) **lock reconcile**
(Section: Lock reconcile), (c) `check_skill_updates`. The UI shows an
"update available" badge per skill. A new API route exposes the check; a CLI
`check`/`update` subcommand exposes it on the terminal.

**Error handling:** auth/network failure → `Uncheckable { auth|network }`,
refresh continues (no crash). Local/non-git source → `Uncheckable { local }`.
Missing `skill_path` → `Uncheckable { no_path }` with a "reinstall to populate"
hint.

### F2. Clean removal (`crates/core/src/manager/skill.rs`, `crates/cli`, `crates/api`)

Removal decides behavior by **layout**, detected from the already-recorded
`canonical_path` (set during discovery when a skill dir is a symlink,
`crates/core/src/skills/discovery.rs:44-57`):

- **Symlink / `.agents` layout** (`canonical_path` is set): full removal of the
  logical skill — delete the canonical `.agents/skills/<name>` real directory
  **and** every symlink across agent dirs that resolves to it (scan agent skill
  dirs, canonicalize, match). This is inherently cross-agent because the
  symlinks are views of one object.
- **Copy layout** (no `canonical_path`): by default remove only the targeted
  agent's copy. With **`--all-agents`**, scan every agent and remove each copy
  by name. `--all-agents` is a new flag on the CLI `delete` and a new parameter
  on the delete API route (orthogonal to `scope`).

### Lock reconcile (the unified lock rule)

The lock is keyed by skill **name** per scope and has **no per-agent field**
(`crates/skill/src/lock/types.rs:57`). Therefore lock pruning is **disk-driven**,
not tied to layout or flags:

> After any removal, re-scan the scope's disk. If **no agent** still has the
> skill, prune the lock entry (wire up the existing `remove_skill_from_lock` /
> `remove_skill_from_local_lock`). If any agent still has it, keep the entry.

A standalone reconcile (`aghub skills reconcile`, also run on refresh) prunes
every lock entry whose skill name has **zero on-disk presence** in the scope.
This self-heals residue from manual `rm` as well — including the "phantom
`Checking from source`" entries.

## 5. Components / files

| Crate | Change |
|---|---|
| `crates/skill` | New `hash.rs`; `install.rs` writes real hash + `content_hash`, stops the placeholder; `lock/types.rs` adds `content_hash`; `lock/local.rs` adds `skill_path`; wire `remove_skill_from_lock` / `remove_skill_from_local_lock` |
| `crates/core` | New `update.rs` (group/fetch/hash/compare); `manager/skill.rs` (layout-aware removal + lock reconcile); reconcile helper |
| `crates/git` | New `fetch_ref_to_temp` (shallow, reuse clone + credentials); subfolder access |
| `crates/cli` | `--all-agents` on `delete`; `check`/`update` and `reconcile` subcommands |
| `crates/api` | Update-check route; `--all-agents` param on delete; reconcile in refresh |
| `crates/desktop` | Refresh triggers update-check; per-skill "update available" badge |

## 6. Data flow — refresh button

1. User clicks refresh.
2. Disk re-scan → current skills per agent (existing, stateless).
3. Lock reconcile → prune entries with zero disk presence (heals manual-`rm`
   residue).
4. `check_skill_updates` → group by source, shallow-fetch, recompute folder
   hash, compare → per-skill status.
5. UI renders badges (up-to-date / update available / uncheckable + reason).

## 7. Testing

- **Unit:** `compute_skill_folder_hash` parity with npx — a golden cross-check
  test that hashes a fixture folder and asserts equality with npx's
  `computeSkillFolderHash` output, **including a fixture with uppercase /
  lowercase / accented filenames** to catch localeCompare-vs-bytewise sort
  divergence. Lock reconcile prunes orphans only. Removal layout detection
  (symlink vs copy).
- **Integration** (fixture repo + fake agent dirs, following existing
  `crates/cli/tests` and `descriptor_regression` conventions): install →
  mutate upstream → check detects `UpdateAvailable`; symlink-layout delete →
  canonical + all symlinks + lock gone; copy-layout delete single vs
  `--all-agents`; manual-`rm` residue → reconcile clears the lock entry; private
  repo update check succeeds with credentials and reports `Uncheckable` without
  them (no crash).

## 8. Rollout / fork plan

- Work on a fork of `AkaraChen/aghub`; implement on a feature branch in an
  isolated worktree.
- All changes are additive and backward-compatible (optional `content_hash`,
  optional `skill_path`, new flag, new route), so existing locks and `npx
  skills` keep working.
- Candidates for upstream PRs: the lock-prune-on-delete fix (clear bug fix) and
  the git-native update check (feature).

## 9. Open questions

None blocking. Revisit if upstream npx introduces strict lock-schema validation
or bumps a lock version, which would invalidate the additive-field guarantee in
Section 1.
