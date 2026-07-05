# AGENTS CRATE KNOWLEDGE BASE

**Crate**: `aghub-agents` — Agent descriptors, models, and format serializers\
**Role in monorepo**: The single source of truth for all agent-specific behavior. `aghub-core` re-exports this crate's public API.

## STRUCTURE

```
crates/agents/src/
├── lib.rs              # Public exports (AgentDescriptor, models, errors, format)
├── descriptor.rs       # AgentDescriptor struct + fn pointer type aliases
├── macros.rs           # define_mcp_paths!/define_skill_paths! — descriptors are built with these
├── models.rs           # AgentConfig, AgentType, McpServer, McpTransport, Skill
├── sub_agents.rs       # SubAgent model
├── errors.rs           # ConfigError, Result
├── agents/             # One descriptor per supported agent (AgentType::ALL = 23);
│   │                   #   `codex/` is a subdirectory; `factory.rs` is the
│   │                   #   Factory-AI agent's descriptor (NOT a dispatch factory —
│   │                   #   there is no dispatch match in this crate at all)
│   ├── mod.rs          # pub mod declarations
│   ├── claude.rs       # Claude descriptor
│   └── ...             # `ls` for the full list
└── format/
    ├── mod.rs           # Format trait
    ├── json_opencode.rs # OpenCode native format
    ├── json_map.rs      # MCP as JSON object map
    ├── json_list.rs     # MCP as JSON array
    └── toml_format.rs   # TOML (Codex, Mistral)
```

## WHERE TO LOOK

| Task                   | Location                                                          |
| ---------------------- | ----------------------------------------------------------------- |
| Add new agent          | `src/agents/<name>.rs` + `mod.rs`                                 |
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

Must touch ALL of these in this crate:

1. `src/agents/<name>.rs` — descriptor constant (`pub const DESCRIPTOR: AgentDescriptor = AgentDescriptor { ... }`)
2. `src/agents/mod.rs` — `pub mod <name>;`
3. `src/models.rs` — `AgentType` enum variant + `ALL` array + `as_str()` + `from_str()`

Then in `crates/core`: `src/registry/mod.rs` — add `&agents::<name>::DESCRIPTOR` to `ALL_AGENTS`. (There is no dispatch match to edit — dispatch is find-by-id over `ALL_AGENTS`.)

## ANTI-PATTERNS

- NEVER add an agent without also wiring `models.rs` and core's `ALL_AGENTS`
- NEVER hand-wire adapter structs — behavior is entirely in `AgentDescriptor` fn pointers
- NEVER use `AgentType` string literals — always use `as_str()` / `from_str()`
- NEVER give `AgentDescriptor` fields that aren't const-constructible — `pub const DESCRIPTOR` needs `&'static str` + fn pointers
