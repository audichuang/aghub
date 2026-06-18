---
name: upstream-skills-flow
description: >-
    End-to-end map of how the upstream vercel-labs `skills` CLI (the `npx skills` /
    `add-skill` package) actually performs each skill lifecycle operation — add,
    install, update/check, remove, sync — plus the exact aghub function that mirrors
    each step. Consult this WHENEVER you are working on aghub's skill install/update/
    remove/sync/lock/hash code and need to know "what does upstream do here?", trace
    or compare against the upstream flow, decide whether an aghub change still matches
    vercel behavior, or onboard onto how the two implementations line up. Use it even
    if the task only mentions one phase (e.g. "how does install pick universal vs
    copy", "where does the folder hash get computed", "why does update re-run add") —
    the upstream source is the ground truth and this skill points you straight at it.
    For the *frozen* round-trip contract (what must never change) use npx-skills-contract;
    for aghub-side invariants use aghub-skills; this skill is the upstream *behavior* reference.
---

# Upstream `skills` flow — reference map

The upstream tool is **`vercel-labs/skills`** (npm package `skills`, bins `skills`
and `add-skill`, e.g. `npx skills add vercel-labs/agent-skills`). Its TypeScript
source is the **ground truth** for what aghub must round-trip with. It lives at:

```
/home/audichuang/research/skills/src
```

aghub is a Rust re-implementation of the same on-disk behavior, not a fork. This
skill is the side-by-side **behavior** map so you can answer "what does upstream
actually do in step X, and which aghub function mirrors it?" without re-reading
both trees each time.

> **Three sibling skills, three jobs — don't confuse them:**
>
> - **this skill** — upstream _behavior_, end to end (what the TS does, step by step).
> - [`npx-skills-contract`](../npx-skills-contract/SKILL.md) — the _frozen_ points
>   that must stay byte/semantically identical for round-trip (lock versions, Master
>   name, hash algorithm). Read it before changing anything those points cover.
> - [`aghub-skills`](../aghub-skills/SKILL.md) — aghub-side invariants (transactional
>   rename, lock mutual-exclusion, where things live in the Rust tree).

## The golden rule

Everything below exists to keep **one shared on-disk world** readable by both tools:
the same two lock files, the same `.agents/skills` Master + per-agent symlink layout,
and the same folder-content hash. If a change you make would make an aghub-written
artifact unreadable by `npx skills` (or vice-versa), stop and check
`npx-skills-contract`. Verify symbol names against the live source before quoting them
— upstream line numbers drift, so this skill references **files + function names**, not
line numbers (except where called out).

## Shared on-disk layout (the thing every phase manipulates)

```
Master (one physical copy):
  global   ~/.agents/skills/<sanitized-name>/
  project  <project-root>/.agents/skills/<sanitized-name>/

Referrer (per-agent symlink → Master):
  ~/.claude/skills/<name>  ->  ~/.agents/skills/<name>   (global: ABSOLUTE link)
  .opencode/skills/<name>  ->  ../../.agents/skills/<name> (project: RELATIVE link)

Universal agent: its skills dir already *is*/reads .agents/skills, so it gets
  NO symlink — it sees the Master directly (no double-listing).
```

- **`sanitize_name`** — upstream `sanitizeName` lives in `installer.ts` (NOT
  `sanitize.ts`). aghub ports it; it's a frozen-contract point.
- **Universal vs copy** is the central fork. Upstream defaults toward symlink/shared;
  **aghub defaults to copy/isolation** and only goes universal on `--universal`. A
  skill is universal in aghub iff `canonical_path.is_some()`.

## Lifecycle map: upstream symbol → aghub symbol

| Phase                     | Upstream (`/home/audichuang/research/skills/src`)                                                                     | aghub                                                                                                                                                                                 |
| ------------------------- | --------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Parse source              | `source-parser.ts` → `parseSource`, `getOwnerRepo`, `getLockSource`, `SOURCE_ALIASES`                                 | `aghub-git::resolve_remote_source` (`crates/git/src/source.rs`) + `crates/api` install routes                                                                                         |
| Add (orchestrate)         | `add.ts` → `runAdd`                                                                                                   | `ConfigManager::add_skill_from_path[_universal]` (`crates/core/src/manager/skill.rs`); API `POST /skills/...`                                                                         |
| Install one (skill,agent) | `installer.ts` → `installSkillForAgent`, `getCanonicalSkillsDir`, `getAgentBaseDir`, `createSymlink`, `copyDirectory` | copy: `ConfigManager::add_skill`; universal: `add_skill_universal` → `install_universal`/`link_agents_to_canonical`/`copy_dir_recursive` (`crates/core/src/skills/install_layout.rs`) |
| Global lock (v3)          | `skill-lock.ts` → `readSkillLock`/`writeSkillLock`/`addSkillToLock` (`CURRENT_VERSION = 3`)                           | `crates/skill/src/lock/{types.rs,global.rs}`; `apply_content_hash`                                                                                                                    |
| Project lock (v1)         | `local-lock.ts` → `readLocalLock`/`writeLocalLock`/`addSkillToLocalLock`                                              | `crates/skill/src/lock/local.rs`; `apply_computed_hash`                                                                                                                               |
| Folder hash               | `local-lock.ts` → `computeSkillFolderHash`                                                                            | `crates/skill/src/hash.rs` → `compute_skill_folder_hash` (feruca, `shifting=false`)                                                                                                   |
| Update / check            | `update.ts` → `updateGlobalSkills`, `updateProjectSkills`, `checkAndPromptForDeletions`                               | `crates/cli/src/commands/{check.rs,apply_update.rs}`; `crates/core/src/skills/update.rs` → `detect_rename`, `stage_and_swap_dir`                                                      |
| Remove                    | `remove.ts` → `removeCommand` (reference-counted)                                                                     | `ConfigManager::remove_skill` / `remove_skill_planned`; `crates/core/src/skills/removal.rs`                                                                                           |
| Prune lock                | (no upstream equivalent)                                                                                              | `crates/core/src/skills/prune.rs` → `prune_lock_scanning`; `crates/cli/src/commands/prune.rs`                                                                                         |
| Sync (node_modules)       | `sync.ts` → `runSync`                                                                                                 | (project-scope discovery; see `references/remove-sync.md`)                                                                                                                            |

## When to open which reference

Each reference is a step-by-step walkthrough of one phase with the gotchas that bite.
Read the one matching your task — don't read all four.

- **`references/install.md`** — `add` → `install`: source parse, blob-vs-clone,
  universal/copy fork, symlink creation + copy-on-failure fallback, the `createSymlink`
  realpath/ELOOP defenses, and aghub's extra conflict-not-clobber behavior.
- **`references/lock-and-hash.md`** — both lock schemas (v3 global, v1 project), the
  `skillFolderHash`⊕`contentHash` mutual-exclusion, `skillPath`, and the folder-hash
  algorithm + the feruca `shifting=false` collation that makes it match JS `localeCompare`.
- **`references/update.md`** — `check` (aghub offline-by-default vs upstream always-online),
  hash-diff detection, why upstream update re-runs `add`, aghub's atomic stage-and-swap,
  and the rename guard that upstream lacks.
- **`references/remove-sync.md`** — reference-counted Master removal, aghub's planned
  removal + scan-safe prune, and node_modules sync.

## Load-bearing gotchas (true regardless of phase)

1. **Never bump a lock version.** Upstream `readSkillLock` wipes the lock to empty if
   it sees a version below `CURRENT_VERSION` — bumping silently destroys cross-tool state.
2. **Folder hash must be byte-identical.** Same file walk (skip `.git`/`node_modules`,
   `lstat`-skip symlinks), same `relativePath`+bytes stream with no delimiters, same
   sort. aghub's `shifting=false` feruca collation is what reproduces JS `localeCompare`;
   it's golden-pinned in `crates/skill/tests/hash_parity_golden.rs`.
3. **`skillFolderHash` and `contentHash` are mutually exclusive.** npx writes the GitHub
   tree SHA into `skillFolderHash`; aghub writes its real Source hash into `contentHash`
   and leaves `skillFolderHash` empty. Use `apply_content_hash`/`apply_computed_hash` —
   never hand-edit.
4. **aghub adds safety upstream lacks** — transactional rename, atomic stage-and-swap on
   apply-update, the rename guard, conflict-not-clobber on install, scan-abort prune. These
   are additive and round-trip-safe; don't "simplify" them away to match upstream.
