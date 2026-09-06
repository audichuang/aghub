# AGENTS CRATE KNOWLEDGE BASE

**Crate**: `aghub-agents` — Agent descriptors, models, and format serializers\
**Role in monorepo**: The single source of truth for all agent-specific behavior. `aghub-core` re-exports this crate's public API.

## STRUCTURE

Role map (not a full file tree — `ls` / codegraph for that):

- `descriptor.rs` — `AgentDescriptor` + capabilities + path fn types
- `macros.rs` — `define_mcp_paths!` / `define_skill_paths!` (prefer these over hand-written path fns)
- `models.rs` — `AgentConfig`, `AgentType`, `McpServer`, `McpTransport`, `Skill`
- `agents/` — one descriptor per agent (authoritative list: `AgentType::ALL` in `models.rs`); `codex/` is a subdirectory; `factory.rs` is the Factory-AI agent (NOT a dispatch factory)
- `sub_agents.rs` — markdown sub-agent I/O + `SubAgentLayout`: `Flat { suffix }` (`.md` for Claude/Grok/OpenCode, `.agent.md` for Copilot) vs `Nested { file_name }` (Antigravity's `<name>/agent.md`). The layout decides the read filter, the NAME and the written filename at once — get one wrong and aghub round-trips with itself while the vendor sees nothing. Frontmatter keys aghub does not model ride the model as `SubAgent::extra_frontmatter` (deserialized through a flattened `extra`), with the destination file read back only when the model carries none — a save rewrites EVERY sub-agent in the directory, not just the edited one, so without this creating one strips its siblings' `tools`/`model`/`color`. Codex is not here: its sub-agents are TOML (`agents/codex/sub_agent.rs`)
- `format/` — serializers: OpenCode native, JSON map MCP, TOML (Codex/Mistral/Grok), YAML (Hermes). Every dialect keeps its own engine (no two share a `Value` type). **All 24 MCP-capable agents** declare the answers they must not differ on in `mcp_policy.rs` — `TransportVocabulary` (its word for each transport; `sse: ""` is what `refuse_unwritable` turns into a refusal — but the dialect still has to CALL it, declaring alone writes an empty tag; `mcp_dialect_roundtrip` is what catches a missing call, NOT `mcp_dialect_decisions`), `OwnedKeys`, `reject_mixed_transport`, `remote_transport`, `transport_fields`, `reads_http` (the one "is this tag streamable HTTP?" condition — `json_map` shares ONE wide alias list across its 17 agents, so narrowing it per dialect is a behaviour change). The seven hand-written dialects declare a `TransportVocabulary` each; the **17 `json_map` agents** declare one inside `json_map::Dialect`, which is the SAME type (it was a second copy, `Discriminator`, until it was merged). Only the mixed-entry WORDING is still split, by `MixedWording` — those 17 agents' users already see `cannot contain both command and url`. Read `mcp_policy.rs` before touching any parser, and add a row to `crates/core/tests/mcp_dialect_decisions.rs` when you add an **MCP-capable agent** (a `json_map` agent introduces no dialect and still owes a row)

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
- **Copilot**: skills — global `~/.copilot/skills` + `~/.agents/skills`;
  project `.agents/skills` (the WRITE dir, first) + `.github/skills`.
  `.claude/skills` is documented by the vendor but deliberately NOT read
  (decision #11). Sub-agents at both scopes: `~/.copilot/agents/<name>.agent.md`
  and `.github/agents/<name>.agent.md` — the `.agent.md` suffix is load-bearing
  (`SubAgentLayout::Flat`); a bare `<name>.md` round-trips green with aghub and
  is invisible to Copilot
- **Omp** (Oh My Pi, a `can1357/oh-my-pi` fork of pi): `json_map` on the default
  `type` tag, and the ONLY `json_map` agent with `ToggleKey::Enabled` — a native
  `enabled` bool, so dropping it remounts a server the user switched off. omp's
  loader prefers a `transport` key but its validator reads `type` alone and runs
  on every entry at connect time, so `type` is the one spelling that both mounts
  and round-trips. An untagged remote is streamable HTTP, never SSE — a
  hand-written `transport: "sse"` is the one thing aghub cannot see. MCP at
  `~/.omp/agent/mcp.json` / `.omp/mcp.json`, deliberately NOT the root
  `.mcp.json` Claude and Copilot share
- **Antigravity**: global skills WRITE `~/.gemini/config/skills`; READ that plus
  the legacy `.gemini/antigravity/skills` and `.gemini/antigravity-cli/skills`.
  Project READ `.agents/skills` (the write dir) + `.agent/skills`. Sub-agents
  are a DIRECTORY per agent (`SubAgentLayout::Nested`):
  `.agents/agents/<name>/agent.md` and `~/.gemini/config/agents/<name>/agent.md`
- **Hermes** (Nous Research): global-only — no project scope, no sub-agents.
  Skills from `~/.hermes/skills/` (SKILL.md). MCP under `mcp_servers` in
  `~/.hermes/config.yaml` — the **only YAML MCP agent**; one remote transport
  (`url`, no sse/http split), native `enabled` flag (`enable_disable: true`);
  other top-level keys preserved on rewrite (comments are **not**). Windows home
  is `%LOCALAPPDATA%\hermes`
- **SSE transport**: Deprecated in `models.rs` — use `StreamableHttp` instead
- **Descriptors are macro-built — until they can't be**: path mappings come from `define_mcp_paths!`/`define_skill_paths!` in `macros.rs` — read those before hand-writing a path fn. `define_skill_paths!` expresses exactly ONE dir per scope, so every agent that also reads the shared `.agents/skills` slot, a vendor alias or a legacy dir hand-writes the fns instead. **When you hand-write them the WRITE dir goes FIRST**: `load_skills_from_dirs` is first-dir-wins and the winner becomes `source_path` — the path `remove_skill` deletes and `check` hashes

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
