# Remove, prune, sync (upstream ↔ aghub)

## Remove — reference-counted Master deletion

Neither tool may delete a Master another agent still references.

**Upstream `removeCommand` (`remove.ts`):** per selected skill, per target agent,
remove the agent's dir/symlink (and its native dir, to catch stale links) but **defer**
the Master. Then if any non-target agent still has an install path for it,
`isStillUsed` keeps the Master; otherwise `rm(canonicalPath)`. Global removal also
calls `removeSkillFromLock`; project removal leaves `skills-lock.json` alone.

**aghub:** `ConfigManager::remove_skill_planned` / `remove_skill_planned_for_agents`
(`crates/core/src/manager/skill.rs`) + `crates/core/src/skills/removal.rs`
(`removal/{verdict,batch}.rs`).

- Builds a `RemovalPlan` (symlink sweep + containment + canonical-keep), default
  dry-run; deletes only when not dry-run AND (non-destructive OR `confirm`), re-checking
  each path's type and containment at delete time.
- A Referrer goes; the `.aghub` Master is reclaimed only when nothing still reads it.
  "Did that removal take anything away?" has one home, `removal::read_effect_after`.
- On execution the per-scope lock is **pruned** (`RemovalOutcome.prune` →
  `PruneStatus::Pruned`; a preview reports `WouldPrune`). A prune failure is non-fatal.

Semantics and why: knowledge page `技能移除與 Master 回收`; CLI surface rules in
`crates/cli/AGENTS.md`.

## Prune — drop orphaned lock entries (aghub-only)

Upstream has no standalone prune. aghub: `crates/core/src/skills/prune.rs`
(`prune_lock_scanning`, `preview_prune`) + `aghub-cli prune-lock`
(`crates/cli/src/commands/prune.rs`).

```
prune_lock_scanning(scope):
  union every agent's skills dirs → scan disk → disk_names
  global  → skill::retain_locked_skills(disk_names)
  project → skill::retain_local_locked_skills(disk_names, root)
```

**Scan-abort safety:** any directory scan error aborts and the lock is left
**unmodified**. `prune-lock` previews by default; `--yes` writes (`--dry-run` forces a
preview even with `--yes`).

## Sync — node_modules skills (upstream `runSync`, `sync.ts`)

`skills experimental_sync` links skills shipped inside installed npm packages into the
project. Always **project scope**, always **symlink** mode. Discovery under
`node_modules/<pkg>` (and `@<scope>/<pkg>`): root `SKILL.md`, else `*/SKILL.md`,
`skills/*/SKILL.md`, `.agents/skills/*/SKILL.md`.

Per skill: `computeSkillFolderHash` vs the project lock's `computedHash` — unchanged →
up to date; changed/new → `installSkillForAgent(…, {global:false, mode:'symlink'})` then
`addSkillToLocalLock(name, {source: pkg, sourceType: 'node_modules', computedHash})`.

aghub has no node_modules sync; it only round-trips such entries in the v1 project lock
(same `computedHash`, `sourceType` kept as an opaque string).
