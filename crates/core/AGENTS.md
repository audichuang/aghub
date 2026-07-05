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
- `manager/` — `ConfigManager` CRUD, split per resource: `mod.rs` / `skill.rs` / `mcp.rs` / `sub_agent.rs`
- `dto/` — CLI/API shared wire views (`removal.rs` `RemovalView`, `skill.rs` `SkillView`) — single source for both surfaces
- `paths.rs` — XDG path helpers; `registry/` — `ALL_AGENTS` + `get()`
- `skills/` — the skill subsystem: `discovery.rs` (SKILL.md + frontmatter), `linker/` (universal-master link decisions, `classify.rs`), `lock.rs`, `install_fetched.rs`, `removal.rs`, `prune.rs`, `update.rs` (`stage_and_swap_dir` + `RecoveryHint` rollback hints), `resync.rs`
- `transfer.rs` — batch install/copy/delete + `reconcile_{skill,mcp,sub_agent}` (`ensure_disjoint` rejects an agent in both add and remove)
- `testing.rs` — `TestConfig` (feature = "testing")

## WHERE TO LOOK

| Task                     | Location                  | Notes                                    |
| ------------------------ | ------------------------- | ---------------------------------------- |
| Agent descriptors/models | `crates/agents/`          | NOT here — core re-exports them          |
| Adapter dispatch         | `src/adapter.rs`          | Maps AgentType → fn calls on descriptor  |
| CRUD for MCPs/skills     | `src/manager/`            | `ConfigManager::new(agent, scope)`       |
| All-agent bulk load      | `src/all_agents.rs`       | `load_all_agents() → AgentResources`     |
| Registry lookup          | `src/registry/mod.rs`     | `registry::get(agent_type)` → descriptor |
| Skills from filesystem   | `src/skills/discovery.rs` | Parses SKILL.md YAML frontmatter         |
| Cross-agent batch ops    | `src/transfer.rs`         | `OperationBatchResult`, reconcile fns    |
| CLI/API wire DTOs        | `src/dto/`                | `RemovalView` / `SkillView`              |
| XDG paths                | `src/paths.rs`            | `~` prefix convention                    |
| Agent CLI detection      | `src/availability.rs`     | Checks for installed agent binaries      |
| Test isolation           | `src/testing.rs`          | `TestConfig` + per-agent path overrides  |

## KEY ABSTRACTIONS

**`ConfigManager`**: Central CRUD — `load()`, `save()`, `load_both()`. Scope: `GlobalOnly | ProjectOnly | Both`.

**`AgentAdapter`** (trait in `adapters/mod.rs`): wraps a descriptor; `create_adapter(agent_type)` returns one.

**`transfer.rs`**: `InstallTarget { agent, scope, project_root }`, `OperationBatchResult { results: Vec<OperationResult> }` — used for installing/copying skills to multiple agents at once. The reconcile family routes skill removal through `remove_skill_planned` (never blind `remove_dir_all` — a NativeReader's read path contains the shared master).

**MCP removal contract**: `remove_mcp_planned` rewrites the shared config entry and deletes NO disk path — `RemovalPlan.paths` stays empty on purpose (a non-empty path once made the desktop preview claim it would delete `~/.claude.json`).

**Skills discovery**: Walks directories looking for `SKILL.md`; parses YAML frontmatter between `---` markers; records `source_path` with `~` prefix.

## CONVENTIONS

- Agent IDs: lowercase strings, kebab-case where multiword (e.g. `jetbrains-ai`); Rust module names are snake_case
- Paths: `~` prefix for home-relative (converted at I/O boundary)
- Skills deduplication: by name, project takes precedence over global
- MCPs: not deduplicated

## TESTING

```bash
cargo test -p aghub-core                           # All core tests (testing feature on by default)
cargo test -p aghub-core --test integration_tests  # Integration only
cargo test -p aghub-core --features agent-validation  # Tests requiring real CLIs installed
```

`TestConfig` creates isolated temp dirs. Per-agent path overrides via `set_skills_path_override(agent_id, path)` (thread-local).

Tests that mutate `HOME`/`XDG_*` **or** read `dirs::home_dir()` must hold the env lock for their suite (`transfer.rs` tests have a module-local `env_lock()`; api uses `test_env_lock`) — the race only surfaces under `cargo test --workspace`, not `-p <crate>`. Unix-gated tests: keep any helper/import they share under the same `#[cfg(unix)]`, or Windows clippy goes red.

## ANTI-PATTERNS

- NEVER add agent descriptors here — they belong in `crates/agents/src/agents/`
- NEVER bypass `ConfigManager` for config mutations
- NEVER skip `source_path` on Skill — required for provenance tracking
- NEVER use non-XDG paths — always use `dirs` crate + `paths.rs` helpers
- NEVER add to `registry/mod.rs` without first adding to `crates/agents`
