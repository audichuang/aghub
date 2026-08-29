# AGENTS CRATE KNOWLEDGE BASE

**Crate**: `aghub-agents` — Agent descriptors, models, and format serializers\
**Role in monorepo**: The single source of truth for all agent-specific behavior. `aghub-core` re-exports this crate's public API.

## STRUCTURE

Role map (not a full file tree — `ls` / codegraph for that):

- `descriptor.rs` — `AgentDescriptor` + capabilities + path fn types
- `macros.rs` — `define_mcp_paths!` / `define_skill_paths!` (prefer these over hand-written path fns)
- `models.rs` — `AgentConfig`, `AgentType`, `McpServer`, `McpTransport`, `Skill`
- `agents/` — one descriptor per agent (authoritative list: `AgentType::ALL` in `models.rs`); `codex/` is a subdirectory; `factory.rs` is the Factory-AI agent (NOT a dispatch factory)
- `format/` — serializers: OpenCode native, JSON map/list MCP, TOML (Codex/Mistral/Grok), YAML (Hermes). The strict dialects (Grok TOML / Hermes YAML) share their transport invariants — mixed-key rejection, `url` → Sse/Http split — in `transport_policy.rs`; read it before touching either parser

## KEY TYPES

**`AgentDescriptor`** (static per agent): holds id, display_name, fn pointers for load_mcps/save_mcps/mcp_parse_config/mcp_serialize_config, path fns, capabilities.

**`Capabilities`**: `{ skills: SkillCapabilities, mcp: McpCapabilities, sub_agents: SubAgentCapabilities }` — scopes (global/project), transport support (stdio/remote), enable/disable toggle.

**`AgentConfig`**: normalized `{ mcps: Vec<McpServer>, skills: Vec<Skill>, sub_agents: Vec<SubAgent> }`.

**`McpTransport`**: `Stdio { command, args, env }` | `Sse { url, headers }` | `StreamableHttp { url, headers }`. **`from_inputs` + `validate_values` are the single validation seam shared by CLI and API** (reject empty command/url, stdio-with-headers, …) — never validate MCP values anywhere else.

## AGENT-SPECIFIC GOTCHAS

The cross-crate rules (universal-master read matrix, `registry::get()` fallback)
are in the **root AGENTS.md** — not repeated here. The per-agent dialect traps:

- **Claude**: skills from `~/.claude/skills/` SKILL.md (not JSON). Disabled MCPs
  omitted on serialize; URL MCPs as `"type": "sse"/"http"`
- **OpenCode**: `mcp` object key; SSE + StreamableHttp unify as
  `"type": "remote"` — **SSE identity is lost** on round-trip
- **Codex/Mistral/Grok**: TOML. Grok: MCP under `mcp_servers` in
  `~/.grok/config.toml` (project: `.grok/config.toml`); streamable HTTP carries
  **no** `type` key — only SSE has `type = "sse"`; native `enabled` flag; other
  top-level keys preserved on rewrite
- **Copilot**: global `~/.copilot/skills`; project scope reads the Master
- **Hermes** (Nous Research): global-only — no project scope, no sub-agents.
  Skills from `~/.hermes/skills/` (SKILL.md). MCP under `mcp_servers` in
  `~/.hermes/config.yaml` — the **only YAML MCP agent**; one remote transport
  (`url`, no sse/http split), native `enabled` flag (`enable_disable: true`);
  other top-level keys preserved on rewrite (comments are **not**). Windows home
  is `%LOCALAPPDATA%\hermes`
- **SSE transport**: Deprecated in `models.rs` — use `StreamableHttp` instead
- **Descriptors are macro-built**: path mappings come from `define_mcp_paths!`/`define_skill_paths!` in `macros.rs` — read those before hand-writing a path fn

## ADDING AN AGENT

Wiring steps: root AGENTS.md "Adding / Removing an Agent". Crate-level detail:
the descriptor is `pub const DESCRIPTOR: AgentDescriptor = …`, and the roster
it must join is `agents::ALL_DESCRIPTORS` in this crate (`core`'s `ALL_AGENTS`
is that same const). Dispatch is find-by-id over the array — no match arm to
edit, which is exactly why the miss is silent; `registry_bijection.rs` is what
makes it loud.

## ANTI-PATTERNS

- NEVER use `AgentType` string literals — always use `as_str()` / `from_str()`
- NEVER give `AgentDescriptor` fields that aren't const-constructible — `pub const DESCRIPTOR` needs `&'static str` + fn pointers
