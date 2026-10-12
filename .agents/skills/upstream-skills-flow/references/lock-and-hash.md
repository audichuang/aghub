# Lock files + folder hash (upstream ↔ aghub)

What is frozen (versions, global lock path, npx overwriting whole entries) is owned by
[`npx-skills-contract`](../../npx-skills-contract/SKILL.md); this page is the behavior map.

## Global lock — `~/.agents/.skill-lock.json` (v3)

Upstream `skill-lock.ts`, `CURRENT_VERSION = 3`; `$XDG_STATE_HOME/skills/.skill-lock.json`
when that var is set. `readSkillLock` returns an **empty** lock for `version < 3`.

Upstream `SkillLockEntry`: `source`, `sourceType`, `sourceUrl`, `ref?`, `skillPath?`,
`skillFolderHash` (GitHub tree SHA), `installedAt`, `updatedAt`, `pluginName?`.

aghub adds `contentHash?` (its real Source hash) and `refCommit?` (branch tip OID), both
`skip_serializing_if = Option::is_none` so npx ignores them. Struct
`crates/skill/src/lock/types.rs`; writer `add_skill_to_lock` (`global.rs`).

## Project lock — `<root>/skills-lock.json` (v1)

Upstream `local-lock.ts`. `LocalSkillLockEntry`: `source`, `ref?`, `sourceType`,
`skillPath?`, `computedHash` (folder SHA-256). `writeLocalLock` sorts keys and writes
**no timestamps** (it is meant to be committed). aghub: `crates/skill/src/lock/local.rs`
(+ `refCommit?`).

## Mutual exclusion (aghub-side)

A v3 entry holds the Source hash in **exactly one** field: npx-written →
`skillFolderHash`; aghub-written → `contentHash` with `skillFolderHash` empty.
Reading an npx entry heals it via `apply_content_hash` (sets `contentHash`, clears
`skillFolderHash`, bumps `updatedAt`; idempotent); the project lock uses
`apply_computed_hash` (no timestamp). Never hand-edit these fields. Tests:
`apply_content_hash_*` (`types.rs`), `apply_computed_hash_*` (`local.rs`).

## `skillPath`

Repo-relative POSIX path to SKILL.md (`SKILL.md` at root, else `<dir>/SKILL.md`);
omitted when absent. Locks are keyed by the **raw frontmatter `name`**, not the
sanitized dir name.

## Folder content hash

Upstream `computeSkillFolderHash` (`local-lock.ts`):

```
collectFiles(dir):            # recurse; skip dirs .git / node_modules; regular files only (symlinks skipped)
files.sort((a,b) => a.relativePath.localeCompare(b.relativePath))
hash = sha256()
for f in files:
  hash.update(f.relativePath)   # '\' → '/'
  hash.update(f.content)        # raw bytes, NO delimiter
return hash.hex()
```

aghub `compute_skill_folder_hash` (`crates/skill/src/hash.rs`) reproduces it; the catch
is the sort. aghub uses `feruca::Collator::new(Tailoring::Cldr(Locale::Root), false, true)`
— the **`false` (`shifting` off, punctuation non-ignorable)** is load-bearing; feruca's
default reorders punctuation/numeric/case paths versus `localeCompare`. aghub's bounds
(`MAX_FILES`, `MAX_TOTAL_BYTES`, `MAX_DEPTH`) only reject pathological input.

Parity is golden-pinned in `crates/skill/tests/hash_parity_golden.rs` against real npx
output (incl. `hash_parity_exotic_filenames_match_npx`: `1/2/10`, `z/ZEBRA`). Touching
the walk, sort or exclusions means re-pinning those goldens against `npx skills` — a
green unit test alone proves nothing.
