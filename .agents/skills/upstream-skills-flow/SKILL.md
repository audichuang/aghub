---
name: upstream-skills-flow
description: >-
    Maps how the upstream vercel-labs `npx skills` CLI performs each skill lifecycle step (install, lock, folder hash, update, remove, sync) to the aghub function that mirrors it. Use when asking what upstream does in a step, or tracing aghub's skill install/update/remove/lock/hash code against it — not for the frozen contract (npx-skills-contract) or aghub-side invariants (aghub-skills).
---

# Upstream `skills` flow — reference map

Upstream is **`vercel-labs/skills`** (npm `skills`, bins `skills` / `add-skill`).
Its TypeScript source, the ground truth, is at:

```
/home/audichuang/research/vercel_npx_skill/src
```

(NOT `/home/audichuang/research/skills` — that is a skills-content repo with no CLI.)
aghub is a Rust re-implementation, not a fork. This map cites files + function
names, not line numbers; grep the live source before quoting one.

Siblings: [`npx-skills-contract`](../npx-skills-contract/SKILL.md) owns what is
_frozen_ (lock versions, hash algorithm, lock path); [`aghub-skills`](../aghub-skills/SKILL.md)
owns aghub-side invariants. This skill is upstream _behavior_ and the side-by-side map.
If a change could make an aghub-written artifact unreadable by `npx skills` (or the
reverse), stop and read `npx-skills-contract` first.

## The layouts differ — know which one you are matching

```
upstream (npx):
  Master    ~/.agents/skills/<name>/   |  <root>/.agents/skills/<name>/
  Referrer  per-agent symlink -> Master (global absolute, project relative)
  universal agent (reads .agents/skills): global install makes NO symlink
  project install: agents whose root dir (.windsurf/, .kiro/ …) is absent are
    skipped (claude-code exempt); symlink failure falls back to a COPY

aghub (symlink-only):
  Master    ~/.aghub/<sanitized-name>/  |  <root>/.aghub/<sanitized-name>/
            (a dir NO agent reads — storing is not granting)
  Referrer  EVERY agent gets a symlink, .agents/skills included (it is just a
            shared Referrer slot); project links relative, global absolute
  no copy install path, no copy fallback: a failed link is a per-agent
    `failed` entry in UniversalInstallReport
```

`--universal` is a deprecated no-op; `add_skill` delegates to
`add_skill_universal`, and every local-path install (`add --from`, API import,
`transfer` / `reconcile` copies) is `install_local_skill`. Layout rules: AGENTS.md "Master store vs
Referrer" and knowledge page `技能連結形狀與 repair 鏈`.

## Lifecycle map

| Phase                      | Upstream (`src/`)                                                                                | aghub                                                                                                                                                              |
| -------------------------- | ------------------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| Parse source               | `source-parser.ts` `parseSource`, `getOwnerRepo`; `add.ts` `getLockSource`                       | `aghub_git::resolve_remote_source` (`crates/git/src/source.rs`)                                                                                                    |
| Add (orchestrate)          | `add.ts` `runAdd` (`blob.ts` `tryBlobInstall` fast path, else `git.ts` `cloneRepo`)              | local path: `install_local_skill` (`crates/core/src/skills/install_local.rs`); git: `install_fetched_skill_and_lock` (`crates/core/src/skills/install_fetched.rs`) |
| Install one (skill, agent) | `installer.ts` `installSkillForAgent`, `getCanonicalSkillsDir`, `createSymlink`, `copyDirectory` | `install_universal` → `copy_dir_recursive` + `link_agents_to_canonical` (`crates/core/src/skills/linker/mod.rs`); Master path `master_store_dir`                   |
| Sanitize                   | `installer.ts` `sanitizeName` (not `sanitize.ts`)                                                | `crates/skill/src/sanitize.rs` `sanitize_name`                                                                                                                     |
| Global lock (v3)           | `skill-lock.ts` `readSkillLock` / `writeSkillLock` / `addSkillToLock`                            | `crates/skill/src/lock/{types.rs,global.rs}`; `apply_content_hash`                                                                                                 |
| Project lock (v1)          | `local-lock.ts` `readLocalLock` / `writeLocalLock` / `addSkillToLocalLock`                       | `crates/skill/src/lock/local.rs`; `apply_computed_hash`                                                                                                            |
| Folder hash                | `local-lock.ts` `computeSkillFolderHash`                                                         | `crates/skill/src/hash.rs` `compute_skill_folder_hash`                                                                                                             |
| Check                      | `update.ts` `updateGlobalSkills` / `updateProjectSkills` (detect half)                           | `aghub-cli check skills [--online]` → `skill_update::run_update_check` (`crates/skill-update/src/lib.rs`)                                                          |
| Update (apply)             | same functions → re-run `add` via `spawnSync`                                                    | `aghub-cli apply-update skills <name> --yes` / `source sync` → `resync_installed_skill` (`crates/core/src/skills/resync.rs`)                                       |
| Remove                     | `remove.ts` `removeCommand` (reference-counted)                                                  | `ConfigManager::remove_skill_planned[_for_agents]`; `crates/core/src/skills/removal.rs`                                                                            |
| Prune lock                 | (none)                                                                                           | `aghub-cli prune-lock` → `crates/core/src/skills/prune.rs` `prune_lock_scanning`                                                                                   |
| Sync (node_modules)        | `sync.ts` `runSync`                                                                              | no aghub equivalent; it only round-trips the project lock                                                                                                          |

## Which reference to open (one, not all)

- [`references/install.md`](references/install.md) — `runAdd` → `installSkillForAgent`,
  the `createSymlink` defenses, `copyDirectory` exclusions, and where aghub diverges.
- [`references/lock-and-hash.md`](references/lock-and-hash.md) — both lock schemas, the
  `skillFolderHash`⊕`contentHash` exclusion, `skillPath`, the folder-hash algorithm and
  feruca collation.
- [`references/update.md`](references/update.md) — upstream detect-then-re-`add` vs
  aghub's offline `check` + transactional resync with the rename guard.
- [`references/remove-sync.md`](references/remove-sync.md) — reference-counted removal,
  aghub prune, upstream node_modules sync.

## Gotchas true in every phase

1. **Never bump a lock version** — upstream `readSkillLock` returns an empty lock for
   `version < CURRENT_VERSION` (3), wiping cross-tool state. Details: `npx-skills-contract`.
2. **The folder hash must stay byte-identical** — golden-pinned in
   `crates/skill/tests/hash_parity_golden.rs`; see `references/lock-and-hash.md`.
3. **`skillFolderHash` and `contentHash` are mutually exclusive** — only
   `apply_content_hash` / `apply_computed_hash` may set them.
4. **aghub's extra safety is deliberate** — interprocess mutation lock, conflict-not-clobber
   linking, rename guard, staged swap with rollback, scan-abort prune. Do not "simplify"
   them away to match upstream.
