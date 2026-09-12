# Update / check flow (upstream ↔ aghub)

The biggest behavior divergence between the two tools lives here — so be deliberate
about which model you're matching.

## Upstream `updateGlobalSkills` / `updateProjectSkills` (`update.ts`)

Always online. Per source:

1. **GitHub source** → `fetchRepoTree(source, ref, getGitHubToken)` pulls the whole repo
   tree in one API call; `findSkillMdPaths(tree)` lists discovered SKILL.md paths.
2. **Non-GitHub source** → `cloneRepo(sourceUrl, ref)` then `discoverSkills(tempDir)`.
3. **Deletion detection** → `checkAndPromptForDeletions(...)`: any locked skill whose
   `skillPath` is no longer in `discoveredPaths` is offered for removal.
4. **Update detection** (the diff):
    ```
    latestHash = getSkillFolderHashFromTree(tree, entry.skillPath)
    if (latestHash && latestHash !== entry.skillFolderHash) → mark for update
    ```
    i.e. compare the GitHub tree SHA in the lock against the live tree SHA.
5. **Apply** → upstream does NOT write files in `update.ts`. It builds an install URL
   (`buildUpdateInstallSource`) and **re-runs `add`** (`spawnSync(..., 'add', url, '-g',
'-y')`), so the lock is updated through the normal `runAdd` path. Failures skip that
   skill and continue.

So upstream "update" = detect-by-hash, then re-install via `add`. There is **no rename
guard** — if the skill's frontmatter `name` changed upstream, the re-`add` installs it
under the new name and can leave the old Master/Referrers dangling.

## aghub: `check` then `apply-update` (two explicit commands)

aghub splits detection from application and is offline-by-default.

### `aghub check skills [--online]` (`crates/cli/src/commands/check.rs`)

- **Offline (default)** — reads the lock and reports each entry as `Uncheckable` with a
  reason (it does not hit the network). This is the main behavior difference from npx.
- **`--online`** — runs the shared orchestrator (`skill_update::check_updates`):
    1. precheck source (local / SSH / unsupported → `Uncheckable`),
    2. cheap ls-refs preflight: if the branch tip OID is unchanged **and** a local hash is
       known → `UpToDate` (skips even a fetch),
    3. else treeless fetch → compare hash,
    4. if the parsed frontmatter `name` ≠ the locked name → `Renamed`.

### `aghub apply-update <name> --yes` (`crates/cli/src/commands/apply_update.rs`)

```
read lock (source_url, ref, skillPath)
  → fetch_source (gix clone → materialize tree)
  → sanitize_skill_path                 # traversal guard
  → ensure_source_not_renamed           # RENAME GUARD (see below)
  → stage_and_swap_dir                   # atomic: temp dir → backup old → rename → rollback on fail
  → update_lock_hash                     # record new hash + refCommit
```

`stage_and_swap_dir` (`crates/core/src/skills/update.rs`) copies into a staging dir
(skipping symlinks, tolerating broken ones), backs up the old dir, renames staging into
place, and rolls back from the backup on any failure. This atomicity is an aghub
addition — upstream just re-copies.

### The rename guard (aghub-only safety)

`detect_rename(parsed_name, expected)` (`crates/core/src/skills/update.rs`) returns the
new name if the source's frontmatter `name` no longer matches the locked name. Apply-update
**refuses** rather than silently renaming (which would break every Referrer symlink):

- CLI: "Delete the old skill and install '{newName}' instead".
- API: error code `SKILL_RENAMED_IN_SOURCE`.

This is the deliberate fix for the upstream gap in step 5 above.
