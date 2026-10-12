# Update / check flow (upstream ↔ aghub)

The biggest divergence between the two tools — be deliberate about which model you match.

## Upstream `updateGlobalSkills` / `updateProjectSkills` (`update.ts`)

Always online. Global, per source:

1. Entries without `skillFolderHash` or `skillPath` are **skipped** (`getSkipReason`) —
   so every aghub-written entry (empty `skillFolderHash`) is invisible to `npx skills update`.
2. **GitHub source** → `fetchRepoTree(source, ref, getGitHubToken)` (one API call) +
   `findSkillMdPaths`; **other git** → `cloneRepo` + `discoverSkills`.
3. **Deletion detection** → `checkAndPromptForDeletions`: a locked skill whose
   `skillPath` is no longer discovered is offered for removal.
4. **Diff** → `getSkillFolderHashFromTree(tree, skillPath) !== entry.skillFolderHash`
   (tree SHA vs tree SHA) marks it for update.
5. **Apply** → `update.ts` writes no files: `buildUpdateInstallSource` + `spawnSync(…,
'add', url, '-g', '-y')`, so the lock updates through `runAdd`. Failures skip and continue.

Project (`skills-lock.json`): clone + deletion check, then re-`add` **every remaining
skill** (`buildLocalUpdateSource`, `add … --skill <name> -y`) — no hash diff.

There is **no rename guard**: if the frontmatter `name` changed upstream, the re-`add`
installs under the new name and can leave the old Master/Referrers dangling.

## aghub: `check` then `apply-update` (two explicit commands)

aghub splits detection from application and is offline-by-default.

### `aghub-cli check skills [--online]` (`crates/cli/src/commands/check.rs`)

Scope defaults to global + current project. Both modes run the one orchestrator,
`skill_update::run_update_check` (`crates/skill-update/src/lib.rs`, also behind API
`GET /skills/check-updates`), and are read-only on both locks.

- **Offline (default)** — remote sources report `uncheckable`/`network` with
  `checked: false`; no network. The main difference from npx.
- **`--online`** (alias `--check-remote`):
    1. precheck source (local / no `skillPath` / unsupported → `Uncheckable`),
    2. tip preflight that downloads no objects: tip unchanged **and** a local hash
       known → `UpToDate`, no fetch,
    3. else treeless fetch → compare hash,
    4. parsed frontmatter `name` ≠ locked name (`detect_rename`) → `Renamed`.

### `aghub-cli apply-update skills <name> --yes` (`crates/cli/src/commands/apply_update.rs`)

Without `--yes` a single-name apply refuses; `--outdated` updates every
`update-available` row (preview without `--yes`) and skips renamed ones. The caller
fetches and sanitizes (`sanitize_skill_path`); the transaction is the shared
`resync_installed_skill` (`crates/core/src/skills/resync.rs`), also used by
`source sync` and the API apply-update / git-sync routes:

```
mutation lock (held to the end)
  → resync_targets_in            # installed targets; none → NotInstalled
  → expected.ensure_unchanged    # compare-after-fetch: lock entry still the one fetched
  → detect_rename                # RENAME GUARD → ResyncError::Renamed
  → compute_skill_folder_hash
  → refuse_conflicting_copy      # differing real copy beside a Master → refuse
  → assert_targets_strictly_contained
  → DirSwapTransaction prepare → commit_all   # stage every target, then swap
  → update_lock_hash             # failure rolls every swapped target back
```

Upstream just re-copies via `add`; staging every target, keeping backups until the
lock commits, and rolling back is aghub's addition.

### The rename guard (aghub-only)

If the source's frontmatter `name` no longer matches the locked name, the resync
**refuses** rather than renaming in place (which would break every Referrer):

- CLI: `skill_renamed_message` — "Delete the old skill and install '{new}' instead";
  `--outdated` points at `aghub-cli source accept-rename <old> <new>` (the transactional
  rename).
- API: error code `SKILL_RENAMED_IN_SOURCE` (`SKILL_RENAMED_CODE`).

This closes the upstream gap in step 5 above.
