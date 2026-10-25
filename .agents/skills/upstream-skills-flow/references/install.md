# Install flow: `add` → `install` (upstream ↔ aghub)

## Upstream `runAdd` (`add.ts`) — the orchestrator

```
parseSource(source)                 # source-parser.ts: shorthand/URL/SSH/local/well-known
  → tryBlobInstall(ownerRepo, …)    # blob.ts; GitHub only, BLOB_ALLOWED_OWNERS/REPOS, not --full-depth
  → cloneRepo(...) + discoverSkills # fallback on any blob failure; walk for SKILL.md
  → filterSkills + prompt           # skill selection (interactive, or -y)
  → detectInstalledAgents           # which agents to target
  → installSkillForAgent(skill, agent, {global, cwd, mode})   # per (skill,agent)
  → addSkillToLock / addSkillToLocalLock
```

aghub has no blob fast path; its git fetch is `aghub-git` (`fetch_ref_to_temp`,
`materialize_tree`) and must end at the same on-disk content.

## Upstream `installSkillForAgent` (`installer.ts`) — the per-pair writer

1. **Reject unsupported global** — `global && agent.globalSkillsDir === undefined` fails.
2. **Sanitize + path-safety** — `sanitizeName`, then `isPathSafe` on BOTH `canonicalDir`
   and `agentDir`.
3. **`installMode === 'copy'`** — clean + `copyDirectory(skill.path, agentDir)`; no Master.
   aghub has **no** equivalent (symlink-only).
4. **symlink mode** (upstream default) — clean + `copyDirectory` into `canonicalDir`
   (`~/.agents/skills/<name>`), then:
    - **global + universal agent** → return; no symlink (it reads `.agents/skills`).
    - **project + non-universal + agent root dir absent** (and not `claude-code`) →
      `skipped: true`; avoids creating empty `.windsurf/`, `.kiro/`, …
    - **otherwise** → `createSymlink(canonicalDir, agentDir)`.
5. **`createSymlink` fails → copy into `agentDir`**, `symlinkFailed: true` (Windows
   without symlink privilege). aghub does NOT do this — see below.

### `createSymlink` defenses

- `realpath`s target and link (plus a parent-symlink resolver), so when e.g.
  `~/.claude/skills` is itself a symlink to `~/.agents/skills` it sees target ≡ link and
  does **not** delete the Master.
- Treats `ELOOP` (circular link) / `ENOENT` as removable.
- Computes the link relative to the real parent dir.

### `copyDirectory` exclusions

Upstream skips `metadata.json` and dirs `.git` / `__pycache__` / `__pypackages__`,
**dereferences** symlinked files (`cp … dereference: true`) and skips broken ones.

aghub's `copy_dir_recursive` (`crates/core/src/skills/linker/mod.rs`, skip list
`skill::excluded_from_master` in `crates/skill/src/hash.rs`) uses the same exclusions but **skips** symlinks instead of dereferencing
— dereferencing would import outside bytes and make the Master disagree with the
lstat-based Source hash. Pinned by `copy_dir_recursive_excludes_vcs_cache_and_metadata`
and `master_copy_skips_links_and_preserves_source_hash`.

## aghub install

| Upstream concept                                   | aghub                                                                                                               |
| -------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------- |
| `runAdd` from a local path                         | `install_local_skill` (`crates/core/src/skills/install_local.rs`); `transfer` / `reconcile` skill copies use it too |
| `runAdd` from git (CLI `source sync`, API install) | `install_fetched_skill_and_lock` (`crates/core/src/skills/install_fetched.rs`)                                      |
| `installSkillForAgent` symlink mode                | `install_universal` (`crates/core/src/skills/linker/mod.rs`)                                                        |
| materialize Master                                 | `install_universal`: atomic `create_dir` claim, then `copy_dir_recursive`; `AlreadyExists` → reuse, no copy         |
| link each agent                                    | `link_agents_to_canonical`                                                                                          |
| relative vs absolute link                          | `LinkTarget::{Relative,Absolute}` + `relative_path`; project → relative, global → absolute                          |
| symlink failure                                    | per-agent entry in `UniversalInstallReport.failed` — no copy fallback                                               |

Master location is `master_store_dir(root)/<sanitize_name(name)>` (`.aghub`), not
`.agents/skills`; which agents need a Referrer is `classify_agent` / `agent_link_need`
(`linker/classify.rs`) — every supported agent gets one.

### aghub additions beyond upstream

- **`UniversalInstallReport`** — `linked`, `already_linked` (idempotent), `conflicts`,
  `failed`, and `created_master` (true only when THIS call claimed the Master, so a
  rollback never deletes another process's Master).
- **Conflict-not-clobber** — a slot occupied by a real dir/file or a foreign link is
  reported in `conflicts` and left untouched (upstream's `cleanAndCreateDirectory`
  removes it). Preserve this; it protects hand-placed skills.
- **Master reuse** — an existing Master is left intact; only the link is (re)made.
- **Mutation lock** — the install holds the interprocess mutation lock from the first
  state read through the lock write (`crates/core/AGENTS.md` "Mutation attribution").
