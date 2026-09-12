# Install flow: `add` → `install` (upstream ↔ aghub)

## Upstream `runAdd` (`add.ts`) — the orchestrator

```
parseSource(source)                 # source-parser.ts: shorthand/URL/SSH/local/well-known
  → tryBlobInstall(ownerRepo, …)    # GitHub fast path (no full clone); falls back to:
  → cloneRepo(...) / discoverSkills # walk for SKILL.md
  → filterSkills / selectSkills     # interactive (or -y) skill selection
  → detectInstalledAgents           # which agents to target
  → installSkillForAgent(skill, agent, {global, cwd, mode})   # per (skill,agent)
  → addSkillToLock / addSkillToLocalLock                      # record in lock
```

`tryBlobInstall` is a GitHub-only optimization (allowed owners like `vercel`,
`vercel-labs`) that pulls a skill without a full clone; any failure cleanly degrades
to `cloneRepo`. aghub's remote fetch is `aghub-git` (`fetch_ref_to_temp`,
`materialize_tree`); it does not need the blob fast-path but must end at the same
on-disk result.

## Upstream `installSkillForAgent` (`installer.ts`) — the per-pair writer

This is the heart of install. Read it before touching aghub's install layout.

1. **Reject unsupported global** — if `global && agent.globalSkillsDir === undefined`,
   fail (agent can't install globally).
2. **Sanitize + path-safety** — `skillName = sanitizeName(rawSkillName)`, then
   `isPathSafe()` is checked on BOTH `canonicalDir` and `agentDir` (directory-traversal
   guard). aghub mirrors this with `sanitize_skill_path` / containment checks.
3. **`mode === 'copy'`** — clean+`copyDirectory(skill.path, agentDir)` straight into the
   agent's own dir. No `.agents` Master. This is aghub's **default** (`add_skill`).
4. **`mode === 'symlink'`** (upstream default) — clean+`copyDirectory(skill.path,
canonicalDir)` first (materialize the Master), then decide whether to link: - **global + universal agent** → return now, `path = canonicalDir`. No symlink (the
   agent reads `.agents/skills` directly; a link would double-list). - **project + non-universal + agent root dir absent** → `skipped: true`, no symlink.
   Avoids creating empty `.windsurf/`, `.kiro/`, … for agents not used in this project.
   The skill is already reachable via `.agents/skills/`. - **otherwise** → `createSymlink(canonicalDir, agentDir)`.
5. **`createSymlink` fails → fallback copy** — clean+copy into `agentDir`, return with
   `symlinkFailed: true`. This is the Windows-without-symlink-privilege path.

### `createSymlink` defenses (the subtle part)

- Resolves both target and link via `realpath` (and a parent-symlink resolver) so that
  if e.g. `~/.claude/skills` is itself a symlink to `~/.agents/skills`, it recognizes
  target ≡ link and does **not** delete the Master.
- Handles `ELOOP` (circular symlink) by removing the broken link.
- Recomputes the link **relative** to the real parent dir so links survive even when a
  parent is a symlink. Project links are relative; global links absolute.

### `copyDirectory` exclusions (frozen — Master must hash identically)

Upstream `copyDirectory` skips `metadata.json` and dirs `.git` / `__pycache__` /
`__pypackages__`, dereferences symlinks, and tolerates broken ones. aghub's
`copy_dir_recursive` (`crates/core/src/skills/install_layout.rs`) mirrors this exactly
— tests `copy_dir_recursive_excludes_vcs_cache_and_metadata` and
`copy_dir_recursive_skips_broken_symlink_and_dereferences` pin it. Do NOT reintroduce a
plain copy; the Master must hash the same as npx's.

## aghub install (`crates/core/src/manager/skill.rs` + `skills/install_layout.rs`)

| Upstream concept                    | aghub                                                                      |
| ----------------------------------- | -------------------------------------------------------------------------- |
| `runAdd` from a local path          | `ConfigManager::add_skill_from_path` / `add_skill_from_path_universal`     |
| `installSkillForAgent` copy mode    | `ConfigManager::add_skill` (default; isolated per-agent copy)              |
| `installSkillForAgent` symlink mode | `ConfigManager::add_skill_universal` → `install_universal`                 |
| materialize Master                  | `install_universal` calls `copy_dir_recursive` only if Master absent       |
| link each agent                     | `link_agents_to_canonical` → `link_one`                                    |
| relative vs absolute link           | `relative_path` + `use_relative_links` (project=relative, global=absolute) |
| symlink-fail fallback               | `link_one` returns `CopiedFallback`                                        |

### aghub additions beyond upstream

- **`UniversalInstallReport`** classifies every target: `linked`, `already_linked`
  (idempotent), `copied_fallback`, `conflicts`.
- **Conflict-not-clobber**: if a target is occupied by a _foreign_ symlink or a real
  file/dir, `link_one` reports it as a conflict and leaves it untouched — upstream's
  `cleanAndCreateDirectory` is more willing to `rm`. Preserve this; it prevents
  destroying a user's hand-placed skill.
- **Master reuse**: `add_skill_universal` does not overwrite an existing Master — it
  reuses it and tells the user to `aghub update` to refresh content.
