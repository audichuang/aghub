# CORE CRATE KNOWLEDGE BASE

**Crate**: `aghub-core` — Orchestration layer. Re-exports `aghub-agents` and adds adapter dispatch, registry, manager, skills discovery, and batch transfer operations.

> Agent descriptors, models, and format modules live in `crates/agents`. This crate wires them together.

## STRUCTURE

Role map (directory → purpose; for the file list `ls` the dir or ask
codegraph — enumerated trees drift):

- `lib.rs` — re-exports aghub-agents + `convert_skill()`
- `adapter.rs` / `adapters/` — adapter dispatch (`AgentAdapter` trait, `create_adapter()`)
- `all_agents.rs` — `load_all_agents()` → `AgentResources` bulk load
- `availability.rs` — which agent CLIs are installed
- `batch.rs` — multi-target mutation policy: preflight-before-any-write +
  attempt-all + the `AgentBatchView` wire shape. CLI `-a a,b`, API
  `/mcps/batch`, transfer/reconcile, and shared Source-to-Master installs are
  thin maps over it — extend it here, never per surface
- `manager/` — `ConfigManager` CRUD, split per resource: `mod.rs` / `skill.rs` / `mcp.rs` / `sub_agent.rs`
- `dto/` — CLI/API shared wire views (`removal.rs` `RemovalView`, `skill.rs` `SkillView`) — single source for both surfaces
- `paths.rs` — project-root detection (agent-marker walk-up); `registry/` — `ALL_AGENTS` + `get()`
- `skills/` — the skill subsystem (`ls` for the full list). Load-bearing:
  `linker/classify.rs` (universal-master link decisions), `rename.rs` (the
  transactional skill rename — rollback lives here, not in surfaces),
  `update.rs` (`stage_and_swap_dir` + `RecoveryHint` rollback hints)
- `transfer.rs` — batch install/copy/delete + `reconcile_{skill,mcp,sub_agent}` (`ensure_disjoint` rejects an agent in both add and remove)
- `testing.rs` — `TestConfig` (feature = "testing")

## WHERE TO LOOK

| Task                     | Location                  | Notes                                               |
| ------------------------ | ------------------------- | --------------------------------------------------- |
| Agent descriptors/models | `crates/agents/`          | NOT here — core re-exports them                     |
| Adapter dispatch         | `src/adapter.rs`          | Maps AgentType → fn calls on descriptor             |
| CRUD for MCPs/skills     | `src/manager/`            | `ConfigManager::new(adapter, global, project_root)` |
| All-agent bulk load      | `src/all_agents.rs`       | `load_all_agents() → AgentResources`                |
| Registry lookup          | `src/registry/mod.rs`     | `registry::get(agent_type)` → descriptor            |
| Skills from filesystem   | `src/skills/discovery.rs` | Parses SKILL.md YAML frontmatter                    |
| Cross-agent batch ops    | `src/transfer.rs`         | `OperationBatchResult`, reconcile fns               |
| CLI/API wire DTOs        | `src/dto/`                | `RemovalView` / `SkillView`                         |
| Project root detection   | `src/paths.rs`            | agent-marker walk-up                                |
| Agent CLI detection      | `src/availability.rs`     | Checks for installed agent binaries                 |
| Test isolation           | `src/testing.rs`          | `TestConfig` + per-agent path overrides             |

## KEY ABSTRACTIONS

**`ConfigManager`**: Central CRUD — `load()`, `save()`, `load_both_annotated()` (provenance-carrying; plain `load_both()` is private). Construct with `ConfigManager::new(adapter, global: bool, project_root: Option<&Path>)` (or `with_scope(...)`). Scope: `ResourceScope::{GlobalOnly, ProjectOnly, Both}`.

**`AgentAdapter`** (trait in `adapters/mod.rs`): wraps a descriptor; `create_adapter(agent_type)` returns one.

**`transfer.rs`**: `InstallTarget { agent, scope, project_root }`, `OperationBatchResult { results: Vec<OperationResult> }` — used for installing/copying skills to multiple agents at once. The reconcile family routes skill removal through `remove_skill_planned` (never blind `remove_dir_all` — a NativeReader's read path contains the shared master). A skill discovered as a plain directory has NO master (`canonical_path` is set only for a link), so its removal plan targets the sole on-disk copy — "there is always a master to fall back on" holds for the link layout only.

**Mutation attribution**: a flow that may roll its own writes back takes what to undo from the mutating call's OWN receipt — the linker's `created_master` / `created_referrer_dirs`, the lock write's replaced entry — never from a pre-write observation, and never from `installed` (a NativeReader row is `installed` with no link, and its first read path IS the master). Those receipts are trustworthy across aghub processes: every mutating flow (install / prune / rename / resync / manager add-remove-update, plus the API's by-path delete and import routes) holds the **interprocess mutation lock** (`skills::lock::mutation_guard`, implemented in `skill::lock::guard`) for its whole check → write → rollback span, so a `modify_*_lock` insert underneath is a genuine compare-and-set. Reentrant per thread, keyed on ONE identity per scope (its resolved lock-file path — used for the held-set, the ordering and the inversion check alike; ordering by anything else is a deadlock), a nested acquire in the wrong order is REFUSED rather than deadlocked, and the OS releases it if the holder dies. Acquisition NEVER degrades to unlocked — that once masked a total Windows failure. The 10s bound covers waits on FOREIGN processes only (one deadline for all scopes); queueing behind another thread of this process is unbounded on purpose, because bounding it turns ordinary queued work into spurious failures.

Take the guard **before the state read that decides the mutation**, not just before the write, and re-read under it: a target, plan or referrer sweep chosen outside the lock (including from a `ConfigManager` whose `load()` predates the guard) is a view another process may already have invalidated. Every guarded `ConfigManager` mutation therefore goes through `guard_and_reload` (`manager/skill.rs`), which takes the lock and re-reads config as ONE step — calling `mutation_guard` directly there is how the stale-view half gets forgotten. A dry-run takes neither. ONE exception, documented at its call site: `update_skill` takes the guard WITHOUT re-reading, because the re-read regressed universal-rename relinking on macOS only and the condition does not reproduce on Linux; its stale-view window is still open.

Never hold it across a network fetch — instead read coordinates, fetch unlocked, then prove under the lock that the entry is still the one you fetched (`skills::lock::EntryIdentity`). That read→fetch→mutate window is SECONDS wide, the widest in the subsystem, and the lock alone cannot close it. Build the identity from the SAME read that produced the fetch coordinates (`of_global_entry` / `of_project_entry`; `capture` only when the entry is not already in hand) — two reads can straddle a repoint, fetching one set of coordinates and then verifying against the other. All three coordinates bind, `ref_name` included.

Acquiring the guard BLOCKS the thread (unbounded in-process queue, then up to 10s on `flock`). An async caller must not do that on an executor thread: `aghub-api` runs every mutating skill route through `api::blocking::in_mutation_pool`, or enough contended requests park every Rocket worker and the server stops answering even unlocked reads.

It serializes aghub against aghub only — `npx skills` takes no lock of ours, so `rename`'s report-less blanket rollback can still remove an npx install of the same name. Read paths are deliberately unlocked. The remaining holes are listed in `docs/specs/2026-07-29-skill-mutation-interprocess-lock.md`; don't re-derive them.

**MCP removal contract**: root AGENTS.md states the invariant (`RemovalPlan.paths` deliberately empty). The why: a non-empty path once made the desktop preview claim it would delete `~/.claude.json`.

**Skills discovery**: Walks directories looking for `SKILL.md`; parses YAML frontmatter between `---` markers; records `source_path` with `~` prefix.

## CONVENTIONS

- Agent IDs: lowercase strings, kebab-case where multiword (e.g. `jetbrains-ai`); Rust module names are snake_case
- Paths: `~` prefix for home-relative (converted at I/O boundary)
- Skills deduplication: by name, project takes precedence over global (sub-agents: same rule)
- MCPs: not deduplicated

## TESTING

```bash
cargo test -p aghub-core                           # All core tests (testing feature on by default)
cargo test -p aghub-core --test integration_tests  # Integration only
cargo test -p aghub-core --features agent-validation  # Tests requiring real CLIs installed
```

`TestConfig` creates isolated temp dirs. Per-agent path overrides via `set_skills_path_override(agent_id, path)` (thread-local).

**Real-home pollution**: global install resolves the universal master via `dirs::home_dir()/.agents/skills` (override does **not** redirect that path). Clearing `skills_path_override` under global scope / no `project_root` **must** isolate `$HOME` (Unix) into a temp dir, hold that binary's env lock, and Drop-clean written skill dirs — never leave junk under the developer's real `~/.agents/skills` or agent skill dirs. Prefer `project_root = tempdir` when project scope is enough. See `test_opencode_global_creation_persists` in `tests/test_agent_paths.rs`.

Tests that read **or** mutate `HOME`/`XDG_*` — including anything that calls `dirs::home_dir()` — must hold their binary's env lock (`transfer.rs` and `test_agent_paths.rs` each have a module-local `env_lock()`; api uses `test_env_lock`). libtest runs one binary's tests in parallel threads of a single process, and on Unix mutating env while another thread reads it is UB; so an env swap is unsound against **any** other env-touching test **within a single test binary** — even `cargo test -p <crate> --test <name>`, not only under `--workspace`. That includes pure readers whose assertions only check path shape: a fake `$HOME` under a `Library`-containing `TMPDIR` can still flip a `!contains("Library")` assertion. Unix-gated tests: keep any helper/import they share under the same `#[cfg(unix)]`, or Windows clippy goes red. That gap is CI-only — `cargo clippy --target x86_64-pc-windows-msvc` cannot stand in for it even with the target installed, because `zstd-sys` / `aws-lc-sys` / `libsqlite3-sys` build scripts need an MSVC C toolchain — so a `cfg` slip surfaces only on push to main.

A "was this file re-created?" assertion must pin the inode with an OPEN handle: ext4 hands a freed inode straight back, so comparing `(dev, ino)` alone false-passes after an unlink + re-copy.

## ANTI-PATTERNS

- NEVER add agent descriptors here — they belong in `crates/agents/src/agents/`
- NEVER bypass `ConfigManager` for config mutations
- NEVER skip `source_path` on Skill — required for provenance tracking
- NEVER hand-build home paths — always use the `dirs` crate; `~` display formatting goes through `lib.rs` `format_path_with_tilde`
- NEVER add to `registry/mod.rs` without first adding to `crates/agents`
- NEVER clear `skills_path_override` for a global write without isolating `$HOME` (or using a project `tempdir`) and tearing down written skill dirs
