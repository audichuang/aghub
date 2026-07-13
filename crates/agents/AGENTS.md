# AGENTS CRATE KNOWLEDGE BASE

**Crate**: `aghub-agents` — Agent descriptors, models, and format serializers\
**Role in monorepo**: The single source of truth for all agent-specific behavior. `aghub-core` re-exports this crate's public API.

## STRUCTURE

Role map (not a full file tree — `ls` / codegraph for that):

- `descriptor.rs` — `AgentDescriptor` + capabilities + path fn types
- `macros.rs` — `define_mcp_paths!` / `define_skill_paths!` (prefer these over hand-written path fns)
- `models.rs` — `AgentConfig`, `AgentType`, `McpServer`, `McpTransport`, `Skill`
- `agents/` — one descriptor per agent (`AgentType::ALL` = 23); `codex/` is a subdirectory; `factory.rs` is the Factory-AI agent (NOT a dispatch factory)
- `format/` — serializers: OpenCode native, JSON map/list MCP, TOML (Codex/Mistral)

## WHERE TO LOOK

| Task                   | Location                                                          |
| ---------------------- | ----------------------------------------------------------------- |
| Add new agent          | `src/agents/<name>.rs` + `mod.rs` + `models.rs` + core registry   |
| Agent capability flags | `src/descriptor.rs` — `Capabilities`                              |
| Normalized data types  | `src/models.rs`                                                   |
| Config serialization   | `src/format/`                                                     |
| MCP value validation   | `src/models.rs` — `McpTransport::from_inputs` / `validate_values` |

## KEY TYPES

**`AgentDescriptor`** (static per agent): holds id, display_name, fn pointers for load_mcps/save_mcps/mcp_parse_config/mcp_serialize_config, path fns, capabilities.

**`Capabilities`**: `{ skills: SkillCapabilities, mcp: McpCapabilities, sub_agents: SubAgentCapabilities }` — scopes (global/project), transport support (stdio/remote), enable/disable toggle.

**`AgentConfig`**: normalized `{ mcps: Vec<McpServer>, skills: Vec<Skill> }`.

**`McpTransport`**: `Stdio { command, args, env }` | `Sse { url, headers }` | `StreamableHttp { url, headers }`. **`from_inputs` + `validate_values` are the single validation seam shared by CLI and API** (reject empty command/url, stdio-with-headers, …) — never validate MCP values anywhere else.

## AGENT-SPECIFIC GOTCHAS

Per-agent behavior (Claude/OpenCode/Codex/Copilot, universal-master read
matrix, registry fallback) is documented once in the **root AGENTS.md
"Agent-Specific Behavior"** — don't duplicate it here. Crate-level extras:

- **SSE transport**: Deprecated in `models.rs` — use `StreamableHttp` instead (SSE identity lost on OpenCode roundtrip anyway)
- **Descriptors are macro-built**: path mappings come from `define_mcp_paths!`/`define_skill_paths!` in `macros.rs` — read those before hand-writing a path fn

## ADDING AN AGENT

In this crate:

1. `src/agents/<name>.rs` — `pub const DESCRIPTOR: AgentDescriptor = …`
2. `src/agents/mod.rs` — `pub mod <name>;`
3. `src/models.rs` — `AgentType` variant + `ALL` + `as_str()` + `from_str()`

Then in `crates/core/src/registry/mod.rs`: add `&agents::<name>::DESCRIPTOR` to `ALL_AGENTS` (dispatch is find-by-id over that array — no match to edit).

## ANTI-PATTERNS

- NEVER add an agent without also wiring `models.rs` and core's `ALL_AGENTS`
- NEVER hand-wire adapter structs — behavior is entirely in `AgentDescriptor` fn pointers
- NEVER use `AgentType` string literals — always use `as_str()` / `from_str()`
- NEVER give `AgentDescriptor` fields that aren't const-constructible — `pub const DESCRIPTOR` needs `&'static str` + fn pointers
