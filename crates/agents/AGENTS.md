# AGENTS CRATE KNOWLEDGE BASE

**Crate**: `aghub-agents` — Agent descriptors, models, and format serializers\
**Role in monorepo**: The single source of truth for all agent-specific behavior. `aghub-core` re-exports this crate's public API.

## STRUCTURE

Role map (not a full file tree — `ls` / codegraph for that):

- `descriptor.rs` — `AgentDescriptor` + capabilities + path fn types
- `macros.rs` — `define_mcp_paths!` / `define_skill_paths!` (prefer these over hand-written path fns)
- `models.rs` — `AgentConfig`, `McpServer`, `McpTransport`, `Skill`, `AgentSelection`, and `AgentType`'s re-export plus `parse_list` (the enum itself comes from `agents/mod.rs`)
- `agents/` — one descriptor per agent, plus the `agent_roster!` macro in `mod.rs` that declares the roster ONCE and emits `AgentType`, `AgentType::ALL`, `as_str`, `FromStr`, `AgentType::descriptor` and `ALL_DESCRIPTORS` from it (so `models.rs` re-exports `AgentType` rather than defining it); `codex/` is a subdirectory; `factory.rs` is the Factory-AI agent (NOT a dispatch factory)
- `sub_agents.rs` — markdown sub-agent I/O + `SubAgentLayout`: `Flat { suffix }` (`.md` for Claude/Grok/OpenCode, `.agent.md` for Copilot) vs `Nested { file_name }` (Antigravity's `<name>/agent.md`). The layout decides the read filter, the NAME and the written filename at once — get one wrong and aghub round-trips with itself while the vendor sees nothing. Frontmatter keys aghub does not model ride the model as `SubAgent::extra_frontmatter` (deserialized through a flattened `extra`), with the destination file read back only when the model carries none — a save rewrites EVERY sub-agent in the directory, not just the edited one, so without this creating one strips its siblings' `tools`/`model`/`color`. Codex is not here: its sub-agents are TOML (`agents/codex/sub_agent.rs`)
- `format/` — serializers: OpenCode native, JSON map MCP, TOML (Codex/Mistral/Grok), YAML (Hermes). Every dialect keeps its own engine (no two share a `Value` type). **Every MCP-capable agent** declares the answers they must not differ on in `mcp_policy.rs` — `TransportVocabulary` (its word for each transport; `sse: ""` is what `refuse_unwritable` turns into a refusal — but the dialect still has to CALL it, declaring alone writes an empty tag; `mcp_dialect_roundtrip` is what catches a missing call, NOT `mcp_dialect_decisions`), `OwnedKeys`, `reject_mixed_transport`, `remote_transport`, `transport_fields`, `reads_http` (the one "is this tag streamable HTTP?" condition — `json_map` shares ONE wide alias list across every agent that inherits it, so narrowing it per dialect is a behaviour change). The seven hand-written dialects declare a `TransportVocabulary` each; the `json_map` agents declare one inside `json_map::Dialect`, which is the SAME type (it was a second copy, `Discriminator`, until it was merged). Only the mixed-entry WORDING is still split, by `MixedWording` — a `json_map` agent's users already see `cannot contain both command and url`. (Counts rot on every roster edit: ask `agent_roster!` in `agents/mod.rs` and the capability tables in `tests/descriptor_regression.rs` for who is in which set.) Read `mcp_policy.rs` before touching any parser, and add a row to `crates/core/tests/mcp_dialect_decisions.rs` when you add an **MCP-capable agent** (a `json_map` agent introduces no dialect and still owes a row)

## KEY TYPES

**`AgentDescriptor`** (static per agent): holds id, display_name, fn pointers for load_mcps/save_mcps/mcp_parse_config/mcp_serialize_config, path fns, capabilities.

**`Capabilities`**: `{ skills: SkillCapabilities, mcp: McpCapabilities, sub_agents: SubAgentCapabilities }` — scopes (global/project), transport support (stdio/remote), enable/disable toggle.

**`AgentConfig`**: normalized `{ mcps: Vec<McpServer>, skills: Vec<Skill>, sub_agents: Vec<SubAgent> }`.

**`McpTransport`**: `Stdio { command, args, env }` | `Sse { url, headers }` | `StreamableHttp { url, headers }`. **`from_inputs` + `validate_values` are the single validation seam shared by CLI and API** (reject empty command/url, stdio-with-headers, …) — never validate MCP values anywhere else.

## Project skill grants

Prefer a vendor-supported private write directory even when the vendor defaults
to `.agents/skills`. Keep shared directories in the read set for discovery and
legacy migration; do not treat that as authorization to write grants there.
Changing the read order also changes which copy wins discovery. Evidence and
migration constraints: `docs/specs/2026-09-11-private-project-skill-slots.md` at
the repository root.

## AGENT-SPECIFIC GOTCHAS

The cross-crate rules (universal-master read matrix, `registry::get()`)
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
  project `.github/skills` (the WRITE dir, first) + `.agents/skills`.
  `.claude/skills` is documented by the vendor but deliberately NOT read
  (decision #11). Sub-agents at both scopes: `~/.copilot/agents/<name>.agent.md`
  and `.github/agents/<name>.agent.md` — the `.agent.md` suffix is load-bearing
  (`SubAgentLayout::Flat`); a bare `<name>.md` round-trips green with aghub and
  is invisible to Copilot
- **Omp** (Oh My Pi, a `can1357/oh-my-pi` fork of pi): `json_map` on the default
  `type` tag, and one of two `json_map` agents with `ToggleKey::Enabled` (ZCode
  is the other, spelled `enable`) — a native
  `enabled` bool, so dropping it remounts a server the user switched off. omp's
  loader prefers a `transport` key but its validator reads `type` alone and runs
  on every entry at connect time, so `type` is the one spelling that both mounts
  and round-trips. An untagged remote is streamable HTTP, never SSE — a
  hand-written `transport: "sse"` is the one thing aghub cannot see. MCP at
  `~/.omp/agent/mcp.json` / `.omp/mcp.json`, deliberately NOT the root
  `.mcp.json` Claude and Copilot share
- **Antigravity**: global skills WRITE `~/.gemini/config/skills`; READ that plus
  the legacy `.gemini/antigravity/skills` and `.gemini/antigravity-cli/skills`.
  Project READ `.agent/skills` (the write dir) + `.agents/skills`. A skill an
  older release left in a compat dir is migrated with `aghub repair`, never
  `aghub add` (which refuses `resource_exists` — the skill already loads);
  `repair` plans WRITE dirs but `readers_of` asks the READ paths, which is the
  only reason the stranded agent reaches `grant_to`. Sub-agents
  are a DIRECTORY per agent (`SubAgentLayout::Nested`):
  `.agents/agents/<name>/agent.md` and `~/.gemini/config/agents/<name>/agent.md`
- **Hermes** (Nous Research): global-only — no project scope, no sub-agents.
  Skills from `~/.hermes/skills/` (SKILL.md). MCP under `mcp_servers` in
  `~/.hermes/config.yaml` — the **only YAML MCP agent**; one remote transport
  (`url`, no sse/http split), native `enabled` flag (`enable_disable: true`);
  other top-level keys preserved on rewrite (comments are **not**). Windows home
  is `%LOCALAPPDATA%\hermes`
- **ZCode**: `json_map` under a NESTED `mcp.servers` key, and the toggle is
  spelled **`enable`** (missing = enabled) — one letter off the canonical
  `enabled`, which is why `ToggleKey` carries its spelling as data. The two
  config files are both `config.json` at DIFFERENT depths: user
  `~/.zcode/cli/config.json`, workspace `<root>/.zcode/config.json`. Skills: the WRITE
  slot is its own `.zcode/skills` at both scopes, and `.agents/skills` is a
  READ path at both — ZCode's own discovery order is user `.zcode` → user
  `.agents` → workspace `.zcode` → workspace `.agents`. Do not read
  `universal: false` as "does not read the shared slot": that flag only decides
  whether XDG `$XDG_CONFIG_HOME/agents/skills` is appended, and ZCode names
  `~/.agents/skills` instead. Dropping the shared dir from the READ list is what
  makes `skills::shape::compat_unlink_authorized` miscount ZCode as a
  non-reader. ZCode also falls back to
  `.agents/mcp.json` per scope, but only while its `.zcode` config defines NO
  server — aghub implements the `.zcode` side only, exactly like ZCode's own
  settings panel, so the first `mcp add` makes a user's `.agents`-only servers
  stop loading. The transport tag and the untagged-remote reading are the
  family defaults (`type`, streamable HTTP): the vendor documents neither
- **SSE transport**: Deprecated in `models.rs` — use `StreamableHttp` instead
- **Descriptors are macro-built — until they can't be**: path mappings come from `define_mcp_paths!`/`define_skill_paths!` in `macros.rs` — read those before hand-writing a path fn. `define_skill_paths!` expresses exactly ONE dir per scope, so every agent that also reads the shared `.agents/skills` slot, a vendor alias or a legacy dir hand-writes the fns instead. **When you hand-write them the WRITE dir goes FIRST**: `load_skills_from_dirs` is first-dir-wins and the winner becomes `source_path` — the path `remove_skill` deletes and `check` hashes

## ADDING AN AGENT

Wiring steps: root AGENTS.md "Adding / Removing an Agent". Crate-level detail:
the descriptor is `pub const DESCRIPTOR: AgentDescriptor = …`, and the roster
it must join is `agents::ALL_DESCRIPTORS` in this crate (`core`'s `ALL_AGENTS`
is that same const, emitted with the `AgentType` enum, `ALL`, `as_str`,
`FromStr` and `AgentType::descriptor` by the `agent_roster!` macro right above
it). Add ONE row — `Variant => "id", module, ["alias", …];` — and nothing in
`models.rs`, which only re-exports `AgentType`. Dispatch is that generated
`match`, so a variant with no descriptor no longer compiles and `registry::get`
has no Claude fallback left. Only the VARIANT is compiler-checked though: the
id literal and the module path are free text, so a copy-pasted row builds fine
and `registry_bijection.rs` is what catches it.

## ANTI-PATTERNS

- NEVER use `AgentType` string literals — always use `as_str()` / `from_str()`
- NEVER give `AgentDescriptor` fields that aren't const-constructible — `pub const DESCRIPTOR` needs `&'static str` + fn pointers
