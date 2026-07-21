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

**`transfer.rs`**: `InstallTarget { agent, scope, project_root }`, `OperationBatchResult { results: Vec<OperationResult> }` — used for installing/copying skills to multiple agents at once. The reconcile family routes skill removal through `remove_skill_planned` (never blind `remove_dir_all` — a NativeReader's read path contains the shared master).

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

Tests that read **or** mutate `HOME`/`XDG_*` — including anything that calls `dirs::home_dir()` — must hold their binary's env lock (`transfer.rs` and `test_agent_paths.rs` each have a module-local `env_lock()`; api uses `test_env_lock`). libtest runs one binary's tests in parallel threads of a single process, and on Unix mutating env while another thread reads it is UB; so an env swap is unsound against **any** other env-touching test **within a single test binary** — even `cargo test -p <crate> --test <name>`, not only under `--workspace`. That includes pure readers whose assertions only check path shape: a fake `$HOME` under a `Library`-containing `TMPDIR` can still flip a `!contains("Library")` assertion. Unix-gated tests: keep any helper/import they share under the same `#[cfg(unix)]`, or Windows clippy goes red.

## ANTI-PATTERNS

- NEVER add agent descriptors here — they belong in `crates/agents/src/agents/`
- NEVER bypass `ConfigManager` for config mutations
- NEVER skip `source_path` on Skill — required for provenance tracking
- NEVER hand-build home paths — always use the `dirs` crate; `~` display formatting goes through `lib.rs` `format_path_with_tilde`
- NEVER add to `registry/mod.rs` without first adding to `crates/agents`
- NEVER clear `skills_path_override` for a global write without isolating `$HOME` (or using a project `tempdir`) and tearing down written skill dirs
