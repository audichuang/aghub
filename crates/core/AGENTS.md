# CORE CRATE KNOWLEDGE BASE

**Crate**: `aghub-core` — Orchestration layer. Re-exports `aghub-agents` and adds adapter dispatch, registry, manager, skills discovery, and batch transfer operations.

> Agent descriptors, models, and format modules live in `crates/agents`. This crate wires them together.

## STRUCTURE

Role map (directory → purpose; for the file list `ls` the dir or ask
codegraph — enumerated trees drift):

- `lib.rs` — re-exports aghub-agents + `convert_skill()`
- `adapters/mod.rs` — the `AgentAdapter` trait + `create_adapter()`;
  `adapter.rs` — the `impl` for `&'static AgentDescriptor` plus the thread-local
  MCP test override (`set_mcp_path_override`). The skills override and the
  write/read dir seam live in `skills/agent_dirs.rs` (`AgentSkillDirs::of`),
  which the adapter's `get_skills_paths` / `target_skills_dir` delegate to.
- `all_agents.rs` — `load_all_agents()` → `AgentResources` bulk load
- `availability.rs` — which agent CLIs are installed
- `batch.rs` — multi-target mutation policy: preflight-before-any-write +
  attempt-all + the `AgentBatchView` wire shape. CLI `-a a,b`, API
  `/mcps/batch`, transfer/reconcile, and shared Source-to-Master installs are
  thin maps over it — extend it here, never per surface
- `manager/` — `ConfigManager` CRUD, split per resource: `mod.rs` / `skill.rs` / `mcp.rs` / `sub_agent.rs`
- `dto/` — CLI/API shared wire views (`RemovalView`, `SkillView`, `McpView`, `SubAgentView`) — single source for both surfaces
- `paths.rs` — project-root detection (agent-marker walk-up) AND
  `app_data_dir()`, the one app data root every surface delegates to; `registry/` — `get()` + `ALL_AGENTS`, which is just `aghub_agents::agents::ALL_DESCRIPTORS`; add agents THERE
- `skills/` — the skill subsystem (`ls` for the full list). Load-bearing:
  `shape.rs` (`classify_shape` / `candidate_referrers` / `observe_shape` / `plan_repair` — observation vs policy: `observe_shape` is the collapsed candidates + shapes + the shared dir's renamed folders, with scope/root; `plan_repair` = observation + policy (managed, git probe, actions, compat sweep and its four guards); `verify_shape` reads observation only and spawns no git; doctor's link axis uses `classify_shape` + `AgentDirs`, never `plan_repair`),
  `removal.rs` (`read_effect_after` / `plan_removal`), `repair.rs`, `health.rs` (`report`: one scope's lock↔Master rows + link audit; compat dirs = `AgentDirs::compat_dirs`, shared with repair). Then
  `enumerate.rs` (the ONE skill-directory walker and marker rule — root `SKILL.md` or
  `skill.md`, `SkillMarker` three-state; discovery, prune, shape and removal all call it and each
  keeps its own `Unknown` bias), `discovery.rs` (frontmatter names on top of it),
  `source_coordinate.rs` (the ONE home of a lock entry's clone coordinate,
  whether its host is provable — `known_clone_source` —, Source membership
  `source_matches` and a scope's ref cohort `recorded_refs`; skill-update,
  rename, `EntryIdentity`, transfer and `install_local` all call it),
  `linker/classify.rs` (which
  Referrer each agent needs), `lock.rs` (`EntryIdentity` + the `mutation_guard`
  forward), `rename.rs` (the transactional skill rename — rollback lives here,
  not in surfaces), `update.rs` (`stage_and_swap_dir` + `RecoveryHint`)
- `transfer.rs` — batch install/copy/delete + `reconcile_{skill,mcp,sub_agent}` (`ensure_disjoint` rejects an agent in both add and remove)
- `testing.rs` — `TestConfig` (feature = "testing")

## KEY ABSTRACTIONS

**`ConfigManager`**: Central CRUD — `load()`, `save()`, `load_both_annotated()` (provenance-carrying; plain `load_both()` is private). Construct with `ConfigManager::new(adapter, global: bool, project_root: Option<&Path>)` (or `with_scope(...)`). Scope: `ResourceScope::{GlobalOnly, ProjectOnly, Both}`.

**`AgentAdapter`** (trait in `adapters/mod.rs`): wraps a descriptor; `create_adapter(agent_type)` returns one.

**`transfer.rs`**: `InstallTarget { agent, scope, project_root }`, `OperationBatchResult { results: Vec<OperationResult> }` — used for installing/copying skills to multiple agents at once. The reconcile family routes skill removal through `remove_skill_planned` (never blind `remove_dir_all` — a real directory in the shared `.agents/skills` slot is read by many agents that leave no link behind — the roster is per-descriptor, `crates/agents/tests/descriptor_regression.rs::test_{global,project}_skill_paths`). A skill discovered as a plain directory has NO master (`canonical_path` is set only for a link), so its removal plan targets the sole on-disk copy — "there is always a master to fall back on" holds for the link layout only. ONE deliberate exception, and it is what selects the layout: an unlinked Master enumerated straight out of the `.aghub` store by `skill_for_planned_removal`'s `--all-agents` fallback gets `canonical_path` SYNTHESIZED from its `source_path`, because `plan_removal` dispatches on `canonical_path.is_some()` alone and that Master must be judged by the Referrer keep rules (`plan_symlink_removal`), not the copy ones.

**"Did that removal take anything away?"** is `removal::read_effect_after(read_dirs, name, deleting)` — re-run DISCOVERY over the read dirs, drop the doomed entries, and report `{survivors, changed}`. `changed` is the set of RESOLVED locations (leaf canonicalized too, unlike `entry_identity`) getting smaller, and it is what separates the two shapes survivors alone cannot: unlinking an npx-era Referrer beside the Master it points at resolves to the same location and takes nothing away (refuse), while deleting a private copy that shadows a Master really does shrink the set (allow, and disclose the Master in `skipped`). `--all-agents` reads the OTHER field — its promise is "gone everywhere", so any survivor blocks. Never re-derive any of it from a plan (`paths` / `shared_master_kept`) — why: `docs/history/core-removal.md#kept-outcome-for-a-shared-master`. A `dir.join(sanitize_name(name))` existence probe is not a substitute either — it cannot see a grouped/renamed layout, and an empty same-named folder reads as a live skill and vetoes a legitimate removal. `Verdict` is the single home for removal verdicts (`Removed`, `Kept`, `Refused`, `Partial`, `LockOnly`, `Absent`), computed in `remove_skill_planned` (or `RemovalOutcome::noop` when absent from disk) from `read_effect_after`, plan facts, and lock state; `shared_master_kept` is derived from `Verdict` for wire compatibility. Readers (`preview`, `RemovalView`, transfer preflight) read the `Verdict`; `transfer`'s skill reconcile calls `remove_skill_batch` twice — a dry run in `plan_reconcile_skill` for the preflight, then ONE commit call after the copies, for every delete row the post-copy spare re-check cleared — and maps both responses' rows through ONE function, `ReconcileSkillPlan::delete_row_result` (the preflight adds only its shared-master refusal and the never-holder / unloadable-config downgrade); the batch folds shared-first sorting and prior-row credit into the entry.

**Mutation attribution**: every mutating flow holds the interprocess mutation lock (`skills::lock::mutation_guard`) across check → write → rollback. Take it before the state read that decides the mutation, undo only from the mutating call's own receipt, never hold it across a fetch (prove the entry with `EntryIdentity`), and in `aghub-api` wrap it in `blocking::in_mutation_pool`. One documented exception: `update_skill` takes the guard without re-reading. Read the `aghub-skills` skill (`references/mutation-attribution.md`) before changing a mutating flow.

**MCP removal contract**: MCP delete (`remove_mcp_planned`) rewrites shared config and deletes **no** disk path — `RemovalPlan.paths` is deliberately empty. The why: a non-empty path once made the desktop preview claim it would delete `~/.claude.json`.

**Disabled agents and real directories**: a flow that picks its own agents writes to the managed set, naming an agent is consent, and "does anyone still hold this?" uses the full roster. `single_agent_keep_reason` is the one verdict on deleting a real directory. Read the `aghub-skills` skill (`references/rosters-and-keep-rules.md`) before changing any of these.

**Plugin ownership** (`skills::plugin_roots`): each surface installs ONE provider at startup (`install_provider`: the API in `build_rocket`, the CLI first thing in `main`). Every skill mutation entry reads the roots itself, BEFORE it takes the mutation lock, because reading them may spawn `claude`. An empty list (no provider, or the plugin list unreadable) refuses nothing (fail-open). Tests pin the roots with `plugin_roots::test_override::set`. A lock-holding caller such as reconcile reads them before its guard and calls `remove_skill_batch_with`.

**Skills discovery**: every walk over a skills dir goes through `skills/enumerate.rs`
(`entries` by identity, `skills` by frontmatter name, `skill_marker` for the one marker rule:
root `SKILL.md` or `skill.md`, links followed). Never add a second `read_dir` walker or marker
probe. `skill::scan::scan_skills` is NOT one of these: it scans a fetched source repo, lives
below core and already uses the same marker rule. Records `source_path` with `~` prefix.

**Link decision**: `classify_agent` / `agent_link_need`
(`crates/core/src/skills/linker/classify.rs`). Every supported agent takes a
Referrer — there is no "reads the Master directly" case; `LinkNeed` has no such
variant, and none is to be added. Every install path must use it — CLI
`add_skill_universal`, local `install_local_skill` (`add --from`, API import,
transfer/reconcile copies) and fetched `install_universal`.

**Shape classification**: `skills::shape` — `classify_shape` (one
`(referrer, master)` pair), `candidate_referrers` (each agent's Referrer PATH,
derived from its write dir, never from what is on disk) and `plan_repair`.
`repair`, `doctor --verify-links`, the pre-mutation guard and migration all read
that ONE classification; none may derive its own. The shape order, the three
traps with their own tests, the four compat-unlink guards and doctor's two axes:
knowledge page `技能連結形狀與 repair 鏈` — read it before touching any of them.

**Project root detection**: Walk up for agent markers (`.claude/`, `.opencode/`, `.cursor/`, `.mcp.json`,
`skills-lock.json`, …) — `paths.rs`. **`.git` alone is not enough.**

## CONVENTIONS

- Agent IDs: lowercase strings, kebab-case where multiword (e.g. `jetbrains-ai`); Rust module names are snake_case
- Paths: `~` prefix for home-relative (converted at I/O boundary)
- Skills deduplication: by name, project takes precedence over global (sub-agents: same rule)
- MCPs: not deduplicated

## TESTING

To make a filesystem op fail on purpose (error, rollback and cleanup paths), read the `testing-fs-failures` project skill before inventing a fixture.

`--features agent-validation` additionally runs the tests that need the real
agent CLIs installed; the `testing` feature (`TestConfig`, isolated temp dirs)
is on by default. Per-agent path overrides via
`skills::agent_dirs::set_skills_path_override(agent_id, path)` (thread-local).

**Real-home pollution**: global install resolves the Master store via `dirs::home_dir()/.aghub` (override does **not** redirect that path), and writes Referrers into the agents' own dirs. Overriding `$HOME` alone is not enough either: `dirs::config_dir()` prefers `$XDG_CONFIG_HOME` and several descriptors honour their own variable ahead of both (observed: a live `~/.config/orca/...` among a test's allow-listed roots). Clearing `skills_path_override` under global scope / no `project_root` **must** isolate `$HOME` (Unix) into a temp dir, hold that binary's env lock, and Drop-clean written skill dirs — never leave junk under the developer's real `~/.aghub`, `~/.agents/skills` or agent skill dirs. Prefer `project_root = tempdir` when project scope is enough. See `test_opencode_global_creation_persists` in `tests/test_agent_paths.rs`.

Tests that read **or** mutate `HOME`/`XDG_*` — including anything that calls `dirs::home_dir()` — must hold their binary's env lock. **One env mutex per test binary**: extend the existing one, never add a second — two mutexes serialize nobody against each other, which is how `manager::skill`'s prune tests started resolving the wrong lock file. The lib binary's is `skills::prune::test_lock::env_lock()` (`GlobalLockGuard` already holds it, and it is not reentrant — so take it once); `test_agent_paths.rs` has its own module-local `env_lock()` and api uses `test_env_lock`, both separate binaries. libtest runs one binary's tests in parallel threads of a single process, and on Unix mutating env while another thread reads it is UB; so an env swap is unsound against **any** other env-touching test **within a single test binary** — even `cargo test -p <crate> --test <name>`, not only under `--workspace`. That includes pure readers whose assertions only check path shape: a fake `$HOME` under a `Library`-containing `TMPDIR` can still flip a `!contains("Library")` assertion. A test that isolates global scope by redirecting `HOME` is unix-only in effect: on Windows `dirs::home_dir()` asks the OS for the profile folder and ignores `HOME`/`USERPROFILE`, so it reads the runner's real profile — gate it `#[cfg(unix)]`. Unix-gated tests: keep any helper/import they share under the same `#[cfg(unix)]`, or Windows clippy goes red. That gap is CI-only — `cargo clippy --target x86_64-pc-windows-msvc` cannot stand in for it even with the target installed, because `zstd-sys` / `aws-lc-sys` / `libsqlite3-sys` build scripts need an MSVC C toolchain — so a `cfg` slip surfaces only on push to main.

A "was this file re-created?" assertion must pin the inode with an OPEN handle: ext4 hands a freed inode straight back, so comparing `(dev, ino)` alone false-passes after an unlink + re-copy.

## ANTI-PATTERNS

- NEVER add agent descriptors here — they belong in `crates/agents/src/agents/`
- NEVER bypass `ConfigManager` for config mutations
- NEVER skip `source_path` on Skill — required for provenance tracking
- NEVER hand-build home paths — always use the `dirs` crate; `~` display formatting goes through `lib.rs` `format_path_with_tilde`
- NEVER hand-wire an adapter struct — `create_adapter` → `registry::get` → `&'static AgentDescriptor`; behavior is function pointers on each descriptor
