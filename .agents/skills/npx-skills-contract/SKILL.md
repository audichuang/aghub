---
name: npx-skills-contract
description: The round-trip contract between aghub and the upstream npx `skills` CLI (v1.5.x) — lock schemas and paths, the shared `.agents/skills` slot, the folder hash, skillPath, and which npx verbs overwrite aghub's state. Use when changing code that reads or writes a skill lock, that slot, the folder hash or skillPath; not for tracing upstream code (upstream-skills-flow).
---

# npx `skills` interop contract

Upstream source of truth: `/home/audichuang/research/vercel_npx_skill`
(`package.json` `name: "skills"`, v1.5.19). NOT `/home/audichuang/research/skills`
— that is a skills-content repo with no CLI. Keep the items below identical;
an addition is safe only when the other tool ignores it. aghub-side map:
[aghub-skills](../aghub-skills/SKILL.md).

## Frozen — never change

- **Lock versions**: global `.skill-lock.json` = v3, project `skills-lock.json` =
  v1 (`CURRENT_VERSION` in `crates/skill/src/lock/{types,local}.rs`). NEVER
  bump — npx `readSkillLock` (`skill-lock.ts:80-102`) returns
  `createEmptyLockFile()` for a version it considers older, wiping every entry.
- **Global lock path**: `$XDG_STATE_HOME/skills/.skill-lock.json` when set, else
  **`~/.agents/.skill-lock.json`** (upstream `getSkillLockPath`; aghub
  `get_skill_lock_path` in `crates/skill/src/lock/io.rs`). **Never delete
  `~/.agents/`**, however empty it looks: npx reads a missing lock as empty, so
  the loss shows up as "no skills tracked", not as an error.
- **npx overwrites the whole lock entry.** `addSkillToLock` writes
  `lock.skills[name] = { ...entry, installedAt, updatedAt }`, so any npx write
  erases aghub's `contentHash` / `refCommit`. No aghub key survives an npx
  write, and a version bump to add one wipes the lock (above).
- **`.agents/skills/<sanitized-name>`** is npx's canonical dir. For aghub it is
  a shared Referrer slot: aghub's Master lives in `.aghub/<sanitized-name>` and
  `.agents/skills/<n>` is a symlink to it (project link relative, global
  absolute). `sanitize_name` (`crates/skill/src/sanitize.rs`) is a direct port of
  upstream `sanitizeName` (lowercase, runs of `[^a-z0-9._]+` → `-`, trim `.-`,
  255-cap, `unnamed-skill` fallback) — both tools must pick the same dir name.
- **Folder hash** (`computeSkillFolderHash` in `local-lock.ts`; aghub
  `crates/skill/src/hash.rs`): SHA-256 lowercase hex over, per file,
  `relativePath` (UTF-8, `\`→`/`) immediately followed by raw bytes, NO
  delimiters; recurse skipping dirs named `.git` / `node_modules`; symlinks are
  skipped (dirent is neither dir nor file). Files sorted by **JS `localeCompare`
  (ICU)**.
- **skillPath**: repo-relative POSIX to SKILL.md (`SKILL.md` at root, else
  `<dir>/SKILL.md`); omit when absent. Locks are keyed by the **raw** frontmatter
  `name`, not the sanitized dir name.

## Which npx verbs WRITE (verified v1.5.19, Node v26.8.1)

Do not claim "npx interop is unaffected" from `sync` evidence alone — it is the
only discovery-driven verb and is not representative.

| verb                                                    | effect                                                                                               |
| ------------------------------------------------------- | ---------------------------------------------------------------------------------------------------- |
| `list`, folder hash                                     | Follows a symlinked skill dir (`installer.ts:85` `isDirEntryOrSymlinkToDir`); digest identical       |
| `experimental_sync` (`cli.ts:368`)                      | Worklist from `node_modules` only, never the lock — safe except on a frontmatter-name collision      |
| `remove`                                                | `scanDir` (`remove.ts:45`) filters on `isDirectory()`, false for symlinks → **silent no-op, exit 0** |
| `add`/`install`/`i`/`a` (`cli.ts:334`)                  | `cleanAndCreateDirectory` (`installer.ts:359`) unlinks the symlink and writes a **real directory**   |
| `update`/`upgrade`/**`check`** (`cli.ts:378`)           | `check` is an ALIAS for `update`, not a read. Re-runs `add` unconditionally per lock entry           |
| `experimental_install` (`cli.ts:329` → `install.ts:18`) | Lock-driven wholesale rebuild — writes `.agents/skills/<n>` for every project-lock entry             |

`fs.rm` dispatches on `lstat`, so npx unlinks aghub's Referrer without
descending — it never deletes through the link into `.aghub`. The loss risk is
the reverse: after any npx write `.agents/skills/<n>` is a real directory whose
bytes may exist nowhere else, so aghub code that assumes that path is a link and
`remove_dir_all`s it destroys the only copy. How aghub treats that real dir
(repair into the store, removal guards): knowledge pages `技能連結形狀與 repair 鏈`
and `技能移除與 Master 回收`, plus `docs/history/core-removal.md`.

## Additive rule

Any aghub-only lock key MUST be `#[serde(skip_serializing_if = "Option::is_none", default)]`
so npx-written entries deserialize and npx ignores the extra key. Live
extensions: global `contentHash` (Source hash, with `skillFolderHash` left
empty — see `CONTEXT.md`) and `refCommit` in both locks (project
`computedHash` is npx's own field, not an extension). Pinned by
`crates/skill/tests/npx_interop.rs` and the serde tests in `lock/{types,local}.rs`.

The reverse direction: a key aghub does NOT model (another installer's
`pinnedRef`, provider data, a newer schema's field) lands in the `extra`
`#[serde(flatten)]` map on every lock struct and is written back unchanged.
`add_skill_to_lock` / `add_skill_to_local_lock` merge the replaced entry's
`extra` under the incoming one, as they do for `pluginName`. A global →
project copy builds a fresh entry and starts with an empty `extra` on purpose:
the project lock gets committed, and a field aghub cannot read could hold a
token.

## Aligned with upstream — keep aligned

- **Master materialization excludes** — `copy_dir_recursive`
  (`crates/core/src/skills/linker/mod.rs`, `EXCLUDE_FILES` / `EXCLUDE_DIRS`)
  mirrors upstream `copyDirectory`'s skip list: `metadata.json` +
  `.git`/`__pycache__`/`__pypackages__`. A plain copy makes the Master hash
  differently from npx. Deliberate divergence: aghub SKIPS symlinks (matching
  the lstat-based hash) where upstream dereferences them.
- **No copy fallback** — upstream copies into the agent dir when a symlink
  fails (`symlinkFailed: true`); aghub deliberately does not
  (`LinkError::LinkUnsupported`). Don't "align" by adding one.
- **Hash sort collator** — `hash.rs` sorts with
  `feruca::Collator::new(Tailoring::Cldr(Locale::Root), false, true)`.
  `shifting = false` (non-ignorable punctuation) is load-bearing: feruca's
  default reorders punctuation / numeric / case-collision paths vs
  `localeCompare`. Pinned against real npx output in
  `crates/skill/tests/hash_parity_golden.rs` (incl. `10.md`, `ZEBRA.md`); a
  change to the hash re-pins those goldens from real npx, never from aghub.

## Cosmetic (round-trips; fix only to minimize byte diffs)

- Global symlink target is absolute in aghub vs always-relative upstream.
- Global lock: aghub adds a trailing newline and emits `contentHash` mid-struct;
  upstream writes no trailing newline. Both parse.

Source-URL normalization (upstream `getOwnerRepo`, SSH preservation,
`SOURCE_ALIASES`, `parseSource`) lives OUTSIDE `crates/skill`, in
`crates/git/src/source.rs` (`resolve_remote_source`,
`normalize_repo_source_from_url`), consumed by the api routes, the CLI `source`
command and `skill-update`.
