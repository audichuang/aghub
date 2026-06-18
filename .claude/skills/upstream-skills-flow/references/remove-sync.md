# Remove, prune, sync (upstream ↔ aghub)

## Remove — reference-counted Master deletion

Both tools must never delete a Master that another installed agent still references.

**Upstream `removeCommand` (`remove.ts`):** for each selected skill, for each target
agent, remove the agent's dir/symlink (and the agent's native dir, to catch stale
symlinks) — but **defer** the canonical Master. Then check the _remaining_ (non-target)
agents: if any still has an install path for that skill, the Master `isStillUsed` and is
kept; only when no agent references it does `rm(canonicalPath)` run. Global removal also
drops the lock entry via `removeSkillFromLock`.

**aghub:** `ConfigManager::remove_skill` / `remove_skill_planned`
(`crates/core/src/manager/skill.rs`) + `crates/core/src/skills/removal.rs`.

- Copy layout → delete `<agent_dir>/<safe_name>/` (or `.md`).
- Universal layout → remove the Referrer symlink; keep the Master unless unreferenced.
- `remove_skill_planned` builds a removal _plan_ (symlink scan + containment check +
  canonical-retention) and executes atomically, rolling back on failure.
- Removal does **not** touch the lock — pruning is a separate, explicit step (below).

## Prune — drop orphaned lock entries (aghub-only)

Upstream has no standalone prune. aghub: `crates/core/src/skills/prune.rs`
(`prune_lock_scanning`) + `crates/cli/src/commands/prune.rs`.

```
prune_lock_scanning(scope, project_root):
  union all agents' skills dirs → scan disk → disk_names (BTreeSet<String>)
  prune_lock(scope, disk_names, …):
    global  → skill::retain_locked_skills(disk_names)
    project → skill::retain_local_locked_skills(disk_names, root)
```

Lock entries with no on-disk skill are removed. **Scan-abort safety:** if any directory
scan errors, prune aborts and the lock is left **unmodified** — never partially wiped.
Default is dry-run; `--yes` actually writes (atomic temp+rename).

## Sync — node_modules skills (upstream `runSync`, `sync.ts`)

`skills sync` (a.k.a. `experimental_sync`) discovers skills shipped inside installed npm
packages and links them into the project. Always **project-scope** and always
**symlink** mode.

Discovery locations under `node_modules/<pkg>` (and `@<scope>/<pkg>`):
`SKILL.md` at root, `skills/*/SKILL.md`, `.agents/skills/*/SKILL.md`.

Per discovered skill: `computeSkillFolderHash` vs the project lock's `computedHash` —
unchanged → up-to-date; changed/new → `installSkillForAgent(..., {global:false,
mode:'symlink'})` then `addSkillToLocalLock(name, {source: pkg, sourceType:
'node_modules', computedHash})`.

aghub interoperates by reading/writing the same project lock (v1) with the same
`computedHash`; the `node_modules` `sourceType` round-trips as an opaque string.
