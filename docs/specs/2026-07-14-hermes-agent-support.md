# Add Hermes agent support (Nous Research Hermes Agent)

**Date**: 2026-07-14
**Status**: Design — pending Codex review, then implementation plan.

## Problem / Context

Upstream `AkaraChen/aghub` has no `hermes` agent (23 agents on `origin/main`,
none is Hermes). We want this fork to manage
[Hermes Agent](https://github.com/NousResearch/hermes-agent) — Nous Research's
self-improving Python agent — as a first-class agent, on par with the others.

Local Hermes source: `~/research/hermes-agent`. Runtime home: `~/.hermes/`.

### Hermes config surface (verified against `~/.hermes` + `cli-config.yaml.example`)

- **Home**: `~/.hermes/` (POSIX) / `%LOCALAPPDATA%\hermes` (native Windows).
  Profiles (`~/.hermes/profiles/<name>`) exist but v1 targets the **default home
  only**. There is **no project-level config** — everything is global.
- **Skills**: `~/.hermes/skills/` — standard `SKILL.md` + YAML frontmatter
  (`name`, `description`, `version`, `metadata.hermes.*`), i.e. the
  agentskills.io open standard, identical in shape to what aghub already manages
  for Claude. Global only. Hermes also has a user-config `skills.external_dirs`
  list, but that is **not** aghub's universal master — do **not** append it.
- **MCP**: the `mcp_servers:` key inside `~/.hermes/config.yaml` (**YAML**). One
  big managed file (`_config_version`) with many unrelated top-level keys
  (`model`, `agent`, `terminal`, `skills`, …). Per-server schema:
    - stdio: `command`, `args: [..]`, `env: {..}`
    - remote/http: `url`, `headers: {..}` — a **single** remote transport; Hermes
      does **not** distinguish SSE from streamable-HTTP.
    - optional per-server: `enabled` (bool, default true), `timeout`,
      `connect_timeout`, `keepalive_interval`, `sampling: {..}`.
- **Sub-agents**: only runtime delegation (`delegation:` config). **No
  file-based sub-agent definitions** → aghub sub-agent support is **off**.
- **CLI**: `hermes` binary; `hermes --version` for validation.

## Non-goals

- Profile support (`~/.hermes/profiles/*`) — default home only.
- Sub-agents (Hermes has no file-based definition surface).
- Preserving YAML **comments** on `config.yaml` rewrite (see tradeoff below).
- Deep enumeration of Hermes' own nested skills (`skills/github/<x>/SKILL.md`);
  aghub manages the skills it installs at `~/.hermes/skills/<name>`. Whether the
  existing discovery walks 2+ levels is core behavior, out of scope here.

## Design

New agent following the standard descriptor pattern (AGENTS.md "Adding an
Agent"). The **only** genuinely new machinery is a YAML MCP format module —
today the `format/` module has JSON (map/list/opencode) and TOML, but no YAML.

### 1. New format module — `crates/agents/src/format/yaml_hermes.rs`

`serde_yaml` is already an `aghub-agents` dependency. Two public fns mirroring
`json_map`'s signature:

```rust
pub fn parse(content: &str) -> Result<AgentConfig>;
pub fn serialize(config: &AgentConfig, original: Option<&str>) -> Result<String>;
```

**parse** — strict (never silently drop; see Codex finding #4)

- `serde_yaml::from_str::<serde_yaml::Value>(content)`; empty/whitespace → empty
  `AgentConfig`.
- Read the top-level `mcp_servers` value: missing → no MCPs; present but **not a
  mapping** → `ConfigError::InvalidConfig`. Use an explicit
  `Value::String("mcp_servers".into())` key for lookups.
- Per entry `(name, server_map)`:
    - name key must be a string, `server_map` must be a mapping → else
      `InvalidConfig` (do **not** skip — a skipped entry vanishes on the next
      serialize = data loss).
    - `enabled` = `server_map["enabled"]` as bool, default `true`.
    - if `command` present → `McpTransport::Stdio { command, args, env, timeout: None }`
      (`args` default `[]`, `env` default `None`).
    - else if `url` present → `McpTransport::StreamableHttp { url, headers, timeout: None }`
      (unify all remote here — `McpTransport::Sse` is deprecated and loses identity
      on roundtrip anyway; do **not** try to infer SSE for Hermes).
    - else (neither `command` nor `url`) → `InvalidConfig` naming the server (an
      entry we cannot represent must not be silently lost).
    - push `McpServer { name, enabled, transport, timeout: None, config_source: None }`.

**serialize** (data-safety is the whole point of a bespoke module)

- Load `original` into `serde_yaml::Value`; `None`/empty → `Value::Mapping(new)`.
  Non-mapping root → error (`ConfigError::InvalidConfig`).
- Grab the **existing** `mcp_servers` mapping (clone) as the source for
  per-server field preservation.
- Build a **fresh** `mcp_servers` mapping in `config.mcps` order. Use explicit
  `Value::String` keys for all `Mapping::get`/`insert`/`remove`.
    - For each `mcp`: start from the existing entry for `mcp.name` if present
      (clone — preserves `timeout`/`connect_timeout`/`keepalive_interval`/
      `sampling`/any unknown keys), else a new empty mapping.
    - **First remove ALL transport-owned keys** from the (cloned) entry —
      `command`, `args`, `env`, `url`, `headers` — _then_ insert only those the
      current transport needs (Codex finding #5: a cloned entry keeps stale keys
      otherwise, e.g. a leftover `url` on a now-stdio server):
        - `Stdio { command, args, env }` → insert `command`, `args`; insert `env`
          only when `Some`.
        - `StreamableHttp { url, headers }` **and** `Sse { url, headers }` → insert
          `url`; insert `headers` only when `Some`. (Serialize `Sse` identically to
          `StreamableHttp` — the `match` must be exhaustive over all three
          `McpTransport` arms or it won't compile, and a server transferred in from
          another agent can carry `Sse`. Mirrors `json_opencode.rs`. Codex #2.)
    - Set `enabled` = `mcp.enabled` (bool). (Because `enable_disable: true` — see
      §3 — disabled servers are **kept** in the file with `enabled: false`, never
      dropped.)
- Replace `root["mcp_servers"]` with the fresh mapping; **leave every other
  top-level key untouched**.
- `serde_yaml::to_string(&root)`.

**Tradeoff (accepted)**: serde_yaml regenerates formatting and **drops
comments**. This only happens when the user mutates Hermes MCP through aghub;
`config.yaml` is a Hermes-managed file (`_config_version`) whose heavy comments
live in the `.example`, not the live file. This mirrors how aghub already treats
JSON configs (regenerated via `to_string_pretty`). Not worth a comment-preserving
YAML editor (no good Rust equivalent of ruamel; heavy new dep).

### 1b. Fix shared save read-error handling — `crates/agents/src/descriptor.rs` (Codex #1)

`save_mcps_to_file` (≈ line 291) currently does
`let original_content = fs::read_to_string(path).ok();` — **any** read error
(permission, transient IO), not just NotFound, becomes "no original", so
`serialize` rebuilds the file from `mcp_servers` alone and **nukes every other
top-level key**. For a dedicated `mcp.json` that loses sibling MCP-file keys; for
Hermes' shared `config.yaml` (model, agent, skills, …) it is catastrophic data
loss. The sibling `load_mcps_from_file` already handles this correctly.

Fix (shared — strictly safer for **all** agents; NotFound behavior unchanged):

```rust
let original_content = match fs::read_to_string(path) {
    Ok(c) => Some(c),
    Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
    Err(e) => return Err(e.into()),
};
```

Existing agent tests use non-existent paths (NotFound → None) so they are
unaffected; add a regression test that a readable-but-present file's non-mcp keys
survive (covered by the yaml_hermes preserve test + a core-level check).

### 2. `mcp_strategy` wrappers — `crates/agents/src/descriptor.rs`

In the existing `pub mod mcp_strategy { … }`, add:

```rust
pub fn parse_yaml_hermes_mcp_servers(content: &str) -> Result<AgentConfig> {
    yaml_hermes::parse(content)
}
pub fn serialize_yaml_hermes_mcp_servers(
    config: &AgentConfig,
    original: Option<&str>,
) -> Result<String> {
    yaml_hermes::serialize(config, original)
}
```

Add `pub mod yaml_hermes;` to `crates/agents/src/format/mod.rs`.

### 3. Descriptor — `crates/agents/src/agents/hermes.rs`

**Hand-write the path fns** (like `augmentcode.rs`) — do **not** use
`define_mcp_paths!` / `define_skill_paths!`. Both macros unconditionally generate
project paths; Hermes is global-only, and declaring a phantom
`<project>/.hermes/...` path is exactly the "invented path" class of bug
UPSTREAM.md records fixing (`b95e1f61`). Project paths = `None`.

All paths derive from one **platform-aware** `hermes_home()` (Codex #3): Hermes'
home is `~/.hermes` on POSIX/WSL2 but `%LOCALAPPDATA%\hermes` on native Windows.
`dirs` is already an `aghub-agents` dep. Both cfg arms are defined on every
platform (single fn, cfg blocks inside) so there are no unused-fn / Windows-clippy
issues (per the fork's Windows-clippy gap note).

```rust
use crate::descriptor::*;
use std::path::{Path, PathBuf};

fn hermes_home() -> Option<PathBuf> {
    #[cfg(windows)]
    { dirs::data_local_dir().map(|d| d.join("hermes")) }
    #[cfg(not(windows))]
    { home_dir().map(|h| h.join(".hermes")) }
}
fn mcp_global_path() -> Option<PathBuf> {
    hermes_home().map(|h| h.join("config.yaml"))
}
fn global_data_dir() -> Option<PathBuf> {
    hermes_home()
}
fn load_mcps(project_root: Option<&Path>, scope: crate::ResourceScope)
    -> crate::Result<Vec<crate::McpServer>> {
    load_scoped_mcps(project_root, scope, Some(mcp_global_path), None,
        mcp_strategy::parse_yaml_hermes_mcp_servers)
}
fn save_mcps(project_root: Option<&Path>, scope: crate::ResourceScope,
    mcps: &[crate::McpServer]) -> crate::Result<()> {
    save_scoped_mcps(project_root, scope, mcps, Some(mcp_global_path), None,
        mcp_strategy::serialize_yaml_hermes_mcp_servers)
}
fn global_skills_read() -> Vec<PathBuf> {
    match hermes_home() { Some(h) => vec![h.join("skills")], None => vec![] }
}
fn global_skills_write() -> Option<PathBuf> {
    hermes_home().map(|h| h.join("skills"))
}
```

Descriptor const:

- `id: "hermes"`, `display_name: "Hermes"`
- `mcp_parse_config / mcp_serialize_config`: the two new `mcp_strategy` fns
- `load_mcps / save_mcps`: above
- `mcp_global_path: Some(mcp_global_path)`, `mcp_project_path: None`
- `global_data_dir`
- **capabilities**:
    - `skills`: `scopes { global: true, project: false }`, `universal: false`
    - `mcp`: `scopes { global: true, project: false }`, `stdio: true`,
      `remote: true`, `enable_disable: true`
    - `sub_agents`: `scopes { global: false, project: false }`
- `global_skill_paths: Some(GlobalSkillPaths { read: global_skills_read, write: global_skills_write })`
- `project_skill_paths: None`
- `load_sub_agents: load_sub_agents_noop`, `save_sub_agents: save_sub_agents_noop`
- `cli_name: "hermes"`, `validate_args: &["--version"]`
- `project_markers: &[]` (global-only; no project detection)
- `skills_cli_name`: **verify during impl** — check whether the `hermes` CLI has
  a skills subcommand (`hermes skills …`). If yes → `Some("hermes")`; if not →
  `None`. (Affects skill sync/usage via Hermes' own CLI only.)

### 4. Wiring (the 4 mandatory steps)

1. `crates/agents/src/agents/mod.rs` — `pub mod hermes;`
2. `crates/agents/src/models.rs` — `AgentType::Hermes` variant + add to `ALL` +
   `as_str()` → `"hermes"` + `from_str()` `"hermes" => Ok(AgentType::Hermes)`.
3. `crates/core/src/registry/mod.rs` — add `&agents::hermes::DESCRIPTOR` to
   `ALL_AGENTS`.

### 5. Tests

- **`yaml_hermes` unit tests** (in-module, like `json_map`):
    - parse stdio (command/args/env)
    - parse remote (url/headers) → `StreamableHttp`
    - parse `enabled: false` → `McpServer.enabled == false`; missing `enabled` →
      `true`
    - parse **strict** (Codex #4): `mcp_servers` not a mapping → `InvalidConfig`;
      an entry with neither `command` nor `url` → `InvalidConfig` (never silently
      skipped)
    - serialize **preserves non-mcp top-level keys** (`model`, `agent`, arbitrary)
    - serialize **preserves per-server extra fields** (`timeout`, `sampling`) on an
      updated server
    - serialize **removes stale transport keys** (Codex #5): take an existing
      stdio server with `command`/`args`, save it as `StreamableHttp` → result has
      `url`, and **no** `command`/`args` left behind (and vice-versa)
    - serialize a transferred **`Sse`** server (Codex #2) → emitted as `url`
      (identical to `StreamableHttp`); confirms the `match` compiles over all arms
    - serialize a **disabled** server → present with `enabled: false` (not dropped)
    - roundtrip parse→serialize→parse is stable
- **Shared read fix** (Codex #1, `save_mcps_to_file`): a present, readable
  config with non-mcp keys keeps them after an MCP save (regression that a
  readable original is not discarded). Isolate `$HOME`/tempdir.
- **Wiring**: `AgentType::from_str("hermes")` → `Hermes`; `as_str()` roundtrip;
  `AgentType::ALL` contains `Hermes`; `registry::get("hermes")` returns the
  Hermes descriptor (not the Claude fallback).
- If `crates/core/tests/test_agent_paths.rs` asserts per-agent paths, add Hermes'
  global config + skills paths.
- **Isolation**: any test touching skills under global scope must isolate `$HOME`
  / use tempdir per AGENTS.md Testing — never write real `~/.hermes`.

### 6. Docs

- `crates/agents/AGENTS.md`: bump "`AgentType::ALL` = 23" → 24.
- Root `AGENTS.md` "Agent-Specific Behavior": add a short Hermes bullet (YAML
  `mcp_servers` in `config.yaml`; global-only; skills = `~/.hermes/skills`
  SKILL.md; remote unified as `url`; native `enabled`).
- Check the **doc-sync test** (touched recently by `1ffe458c`) — if it asserts an
  agent count or an agent list, update the fixture/expectation.
- `UPSTREAM.md`: note Hermes as a fork-only agent (not from upstream).
- **Optional** (polish, not required): `crates/desktop/src/assets/agent/hermes.svg`.
  The desktop is data-driven — `agent-icons.tsx` globs `assets/agent/*.svg` with a
  first-letter fallback, so a missing icon just renders "H". No TS change needed.

## Verification (before code review)

- `just fmt` + `cargo clippy -p aghub-agents -p aghub-core -D warnings`
- `cargo test -p aghub-agents` (yaml_hermes + models) and
  `cargo test -p aghub-core` (registry + agent paths)
- `just start -- mcp list -a hermes -g` against a scratch `$HOME` shows an added
  server; confirm `~/.hermes/config.yaml` keeps its other keys.

## Codex design review — resolved

Reviewed by Codex (GPT-5.6, read-only) 2026-07-14. Verdict: sound to implement
after folding in 5 findings, **all now incorporated above**:

1. (blocker) shared `save_mcps_to_file` `.ok()` swallows read errors → §1b.
2. (blocker) serialize must handle the `Sse` arm (exhaustive + transfer) → §1.
3. (blocker) Windows home is `%LOCALAPPDATA%\hermes`, not `~/.hermes` →
   `hermes_home()` helper in §3.
4. (should-fix) strict parse — no silent skip of malformed entries → §1.
5. (should-fix) remove stale transport keys before insert on merge → §1.

Codex **confirmed**: the manager enable/disable path passes the full MCP slice
(disabled included) to `save_mcps` (`manager/mcp.rs:125` → `save_scoped_mcps`
`descriptor.rs:282`), so `enable_disable: true` is correct and matches OpenCode
(`json_opencode.rs:112`); no additional framework wiring step is missing.
