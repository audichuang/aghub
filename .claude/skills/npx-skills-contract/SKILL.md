---
name: npx-skills-contract
description: The interop contract aghub must preserve to stay round-trip compatible with the upstream npx `skills` package (v1.5.x) — lock-file schemas, the .agents Master + symlink layout, the folder-content hash algorithm, and where source normalization lives. Use when changing anything that reads or writes a skill lock file, the universal/.agents install layout, the folder hash, or skillPath, so an aghub-written artifact stays readable by `npx skills` and vice-versa.
---

# npx `skills` interop contract

aghub must round-trip with the upstream `skills` CLI (source of truth:
`/home/audichuang/research/vercel_npx_skill` — its `package.json` is
`name: "skills"`, currently v1.5.19. NOT `/home/audichuang/research/skills`, which
is a skills-content repo with no CLI source). Keep these identical; additions are
only safe when they are ignored by the other tool. Layout/removal already match —
see [aghub-skills](../aghub-skills/SKILL.md) for the aghub-side map.

## Frozen — never change

- **Lock versions**: global `.skill-lock.json` = v3, project `skills-lock.json` =
  v1. NEVER bump — npx resets a lock to empty on a version it considers older,
  silently wiping cross-tool state.
- **Global lock PATH**: `$XDG_STATE_HOME/skills/.skill-lock.json` when that var is
  set, else **`~/.agents/.skill-lock.json`** — inside `.agents`, not `~/.skill-lock.json`
  (upstream `skill-lock.ts:67-73`; aghub `crates/skill/src/lock/io.rs:20-31`).
  Consequence: **never delete `~/.agents/`**, however empty it looks. Upstream
  `readSkillLock` returns an empty lock rather than erroring on a failed read, so
  wiping it presents as "no skills tracked" instead of a fault.
- **npx overwrites the whole lock entry.** `addSkillToLock` (`skill-lock.ts:205-221`)
  writes `lock.skills[name] = { ...entry, installedAt, updatedAt }`, so any npx
  write erases aghub's `contentHash` and `refCommit`. No aghub key can be added to
  the npx lock, and bumping the version to add one cleanly makes npx's
  `readSkillLock` (`:93-98`) return `createEmptyLockFile()` — every user entry at once.
- **Master location & name**: `<home if global | project-root if project>/.agents/skills/<sanitized-name>`.
  `sanitize_name` is a direct port of upstream `sanitizeName` (lowercase, runs of
  `[^a-z0-9._]+` → `-`, trim `.-`, 255-cap, `unnamed-skill` fallback).
- **Layout**: one physical copy at the Master + per-agent **symlink** Referrers
  pointing at it, with copy-on-symlink-failure fallback. Removal is
  reference-counted: delete the Master only when no other installed agent
  references it.
- **Folder hash**: SHA-256 → lowercase hex over a stream of, per file,
  `relativePath` (UTF-8, `\`→`/`) immediately followed by raw file bytes, NO
  delimiters; recurse skipping dirs literally named `.git` / `node_modules`;
  `lstat`-skip symlinks. Files sorted by **JS `localeCompare` (ICU)**.
- **skillPath**: repo-relative POSIX to SKILL.md (`SKILL.md` at root, else
  `<dir>/SKILL.md`); omit when absent. Locks are keyed by the **raw** frontmatter
  `name`, not the sanitized dir name.

## Which npx verbs WRITE (verified v1.5.19, live fixtures on Node v26.8.1)

Do not assert "npx interop is unaffected" from `sync` evidence alone — `sync` is the
only discovery-driven verb, and it is not representative.

| verb                                                    | effect                                                                                               |
| ------------------------------------------------------- | ---------------------------------------------------------------------------------------------------- |
| `list`, folder hash                                     | Follows a symlinked skill dir (`installer.ts:86` `isDirEntryOrSymlinkToDir`); digest identical       |
| `experimental_sync` (`cli.ts:368`)                      | Worklist from `node_modules` only, never the lock — safe except on a frontmatter-name collision      |
| `remove`                                                | `scanDir` (`remove.ts:45`) filters on `isDirectory()`, false for symlinks → **silent no-op, exit 0** |
| `add`/`install`/`i`/`a` (`cli.ts:334`)                  | `cleanAndCreateDirectory` (`installer.ts:359`) unlinks any symlink and writes a **real directory**   |
| `update`/`upgrade`/**`check`** (`cli.ts:378`)           | `check` is an ALIAS for `update`, not a read. Re-runs `add` unconditionally per lock entry           |
| `experimental_install` (`cli.ts:329` → `install.ts:18`) | The lock-driven wholesale rebuild — writes `.agents/skills/<n>` for every project-lock entry         |

`fs.rm` dispatches on `lstat`, so npx unlinks a symlink without descending — it can
never delete through a link into aghub's store. The loss risk is the reverse: after
any npx write the directory holds bytes existing nowhere else, so aghub code that
assumes that path is a symlink and `remove_dir_all`s it destroys the only copy.

## Additive rule (how aghub extends safely)

Any aghub-only lock key MUST be `#[serde(skip_serializing_if = "Option::is_none", default)]`
so npx-written entries deserialize cleanly and npx ignores aghub's extra keys.
Live extensions that round-trip (keep): global `contentHash` (real Source hash,
with `skillFolderHash` left empty), project `computedHash`, the transactional
rename, apply-update, the upstream-rename guard, atomic+locked lock writes, prune,
and the TOCTOU/containment-hardened removal.

## Aligned with upstream (were divergent — keep them aligned)

- **Master materialization excludes** — `copy_dir_recursive`
  (`crates/core/src/skills/install_layout.rs`) now mirrors upstream
  `copyDirectory`: skip `metadata.json` + `.git`/`__pycache__`/`__pypackages__`,
  dereference symlinks, skip broken ones. Don't reintroduce a plain copy — the
  Master must hash identically to npx.
- **Hash sort collator** — `compute_skill_folder_hash` (`crates/skill/src/hash.rs`)
  sorts with `feruca::Collator::new(Tailoring::Cldr(Locale::Root), false, true)`.
  The `shifting = false` (non-ignorable punctuation) is load-bearing: feruca's
  default (`shifting = true`) reorders punctuation / numeric / case-collision
  paths vs `localeCompare`. Verified against real npx output in
  `crates/skill/tests/hash_parity_golden.rs` (incl. the exotic `1/10/2`, `z/ZEBRA`
  cases) — keep those goldens green.

## Cosmetic (round-trips; fix only for byte-diff minimization)

- Global symlink target is absolute in aghub vs always-relative upstream.
- Global lock: aghub adds a trailing newline + emits `contentHash` mid-struct;
  upstream has no trailing newline. Both parse fine.

Source-URL normalization (`getOwnerRepo`, SSH preservation, `SOURCE_ALIASES`,
`parseSource`) lives OUTSIDE `crates/skill` — in the `crates/api` install routes +
`aghub-git::resolve_remote_source`. Verifying hash parity changes? Use the
[testing-fs-failures](../testing-fs-failures/SKILL.md) techniques and re-pin
`crates/skill/tests/hash_parity_golden.rs` against real npx output.
