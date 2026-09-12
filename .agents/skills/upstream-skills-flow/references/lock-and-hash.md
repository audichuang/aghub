# Lock files + folder hash (upstream ↔ aghub)

Two lock files, one shared Source-hash idea. These are the round-trip surface — see
[`npx-skills-contract`](../../npx-skills-contract/SKILL.md) for what is frozen.

## Global lock — `~/.agents/.skill-lock.json` (v3)

Upstream: `skill-lock.ts`. `CURRENT_VERSION = 3` (bumped from 2 for GitHub-tree-SHA
folder hash). Path is `$XDG_STATE_HOME/skills/.skill-lock.json` if set, else
`~/.agents/.skill-lock.json`.

**Wipe-on-old-version:** `readSkillLock` returns an _empty_ lock if it reads a file
whose `version < CURRENT_VERSION`. This is why **bumping the version is forbidden** —
an older-versioned write makes the other tool silently discard all cross-tool state.

Upstream `SkillLockEntry` fields: `source`, `sourceType`, `sourceUrl`, `ref?`,
`skillPath?`, `skillFolderHash` (GitHub tree SHA), `installedAt`, `updatedAt`,
`pluginName?`.

aghub adds (all `#[serde(skip_serializing_if="Option::is_none", default)]` so npx
ignores them): `contentHash?` (aghub's real Source hash), `refCommit?` (branch tip OID).
Struct: `crates/skill/src/lock/types.rs`; writer `add_skill_to_lock` (`global.rs`).

## Project lock — `<root>/skills-lock.json` (v1)

Upstream: `local-lock.ts`. `LocalSkillLockEntry`: `source`, `ref?`, `sourceType`,
`skillPath?`, `computedHash` (SHA-256 of folder). `writeLocalLock` sorts entries by key
and writes **no timestamps** — minimizes git merge conflicts; meant to be committed.
aghub: `crates/skill/src/lock/local.rs` (+ `refCommit?`).

## The mutual-exclusion invariant (aghub-side)

A v3 entry holds the Source hash in **exactly one** of two fields:

- npx-written → `skillFolderHash` populated, no `contentHash`.
- aghub-written → `contentHash` populated, `skillFolderHash` **empty**.

Never both. When aghub reads an npx entry it heals it via `apply_content_hash` (sets
`contentHash`, clears `skillFolderHash`, bumps `updatedAt`; idempotent). Project lock
uses `apply_computed_hash` (no timestamp). **Never hand-edit these fields** — those two
methods are the only home of the invariant. Tests: `apply_content_hash_*` in `types.rs`,
`apply_computed_hash_*` in `local.rs`.

## `skillPath`

Repo-relative POSIX path to SKILL.md (`SKILL.md` at root, else `<dir>/SKILL.md`); omit
if absent. Locks are keyed by the **raw frontmatter `name`**, not the sanitized dir name.

## Folder content hash (must be byte-identical across tools)

Upstream `computeSkillFolderHash` (`local-lock.ts`):

```
collectFiles(dir):            # recurse; skip dirs named .git / node_modules; files only
files.sort((a,b) => a.relativePath.localeCompare(b.relativePath))   # ICU localeCompare
hash = sha256()
for f in files:
  hash.update(f.relativePath)   # relativePath has '\' → '/'
  hash.update(f.content)        # raw bytes, NO delimiter between path and content
return hash.hex()
```

aghub `compute_skill_folder_hash` (`crates/skill/src/hash.rs`) reproduces this exactly,
with the one subtle catch: the sort. JS `localeCompare` is ICU/UCA; aghub uses
`feruca::Collator::new(Tailoring::Cldr(Locale::Root), false, true)`. The **`false` =
`shifting` off (punctuation NON-IGNORABLE)** is load-bearing — feruca's default
(`shifting=true`) reorders punctuation/numeric/case-collision paths versus
`localeCompare`. aghub also adds bounds (MAX_FILES, MAX_TOTAL_BYTES, MAX_DEPTH) that
upstream lacks; those only reject pathological inputs, they don't change the hash.

Parity is pinned in `crates/skill/tests/hash_parity_golden.rs` against real npx output
(including exotic `1/10/2` and `z/ZEBRA` cases). If you touch the walk, the sort, or the
exclusions, re-pin those goldens against `npx skills` — don't trust a green unit test alone.
