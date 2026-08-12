# MCP compatibility audit (2026-08-13)

This audit compares the descriptors and serializers in `crates/agents` with
the vendors' current configuration documentation or released source. The
important rule is that a path, JSON key, transport discriminator, and toggle
field are one dialect; sharing a lossy generic serializer across dialects is
not safe.

## Results

| Agent                  | Native MCP contract                                                                                                                               | Aghub result                                                                                                                                                                                                                                                                  |
| ---------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Cursor                 | `~/.cursor/mcp.json`, `.cursor/mcp.json`, `mcpServers`; stdio/SSE/Streamable HTTP                                                                 | Paths and transports retained; unknown fields are preserved; no persisted file toggle is advertised. [Docs](https://cursor.com/docs/mcp)                                                                                                                                      |
| Windsurf/Cascade       | `~/.codeium/windsurf/mcp_config.json`, `mcpServers`; no documented project file; `disabledTools` is tool-level                                    | Project MCP scope removed; global URL/server fields are retained. [Docs](https://docs.devin.ai/desktop/cascade/mcp)                                                                                                                                                           |
| GitHub Copilot CLI     | `$COPILOT_HOME/mcp-config.json`, `.mcp.json` (or `.github/mcp.json`), `mcpServers`; stdio/SSE/HTTP                                                | VS Code path/key mix-up fixed to the CLI dialect; no per-server toggle is claimed. [Docs](https://docs.github.com/en/copilot/how-tos/copilot-cli/customize-copilot/add-mcp-servers)                                                                                           |
| Claude Code            | Global/project `mcpServers` JSON; stdio/SSE/HTTP                                                                                                  | Existing paths/capabilities retained; unmanaged fields survive rewrites. [Docs](https://docs.anthropic.com/en/docs/claude-code/mcp)                                                                                                                                           |
| Roo Code               | `.roo/mcp.json`, `mcpServers`; stdio/SSE/`streamable-http`; `disabled`                                                                            | Project-only scope and kebab-case HTTP discriminator fixed. [Docs](https://github.com/RooCodeInc/Roo-Code/blob/v3.54.0/apps/docs/docs/features/mcp/using-mcp-in-roo.mdx)                                                                                                      |
| Cline                  | `~/.cline/data/settings/cline_mcp_settings.json`, `mcpServers`; `streamableHttp`/legacy HTTP; `disabled`                                          | Invented project file removed; camel-case discriminator and toggle are retained. [Source](https://github.com/cline/cline/blob/v4.1.8/sdk/packages/shared/src/storage/paths.ts)                                                                                                |
| Gemini CLI             | `~/.gemini/settings.json` and project settings; `mcpServers`                                                                                      | Existing path/key and stdio/remote support retained with lossless JSON merging. [Docs](https://github.com/google-gemini/gemini-cli)                                                                                                                                           |
| Codex CLI              | `$CODEX_HOME/config.toml`, project `.codex/config.toml`; TOML MCP tables; stdio/HTTP                                                              | TOML adapter now handles remote entries, headers, timeout, enabled state, and `CODEX_HOME`. [Docs](https://developers.openai.com/codex/mcp/)                                                                                                                                  |
| Antigravity            | `~/.gemini/config/mcp_config.json`, `.agents/mcp_config.json`; `serverUrl`; `disabled`/`disabledTools`                                            | Both paths, URL key, remote capability, and toggle fixed; explicit WebSocket remains rejected because the normalized model has no WebSocket transport. [Docs](https://antigravity.google/docs/mcp)                                                                            |
| OpenClaw               | `~/.openclaw/openclaw.json`, nested `mcp.servers`; `transport`, `enabled`                                                                         | mcporter registry mix-up replaced by a dedicated nested dialect; config/state path overrides are honored. [Source](https://github.com/openclaw/openclaw/blob/0790d9f593ad30c940ed93b5872a8cf8d6f3cf8c8/docs/cli/mcp.md)                                                       |
| OpenCode               | Global config layers plus root/project `opencode.json`/`opencode.jsonc`; nested `mcp`; local/remote; `enabled`; `OPENCODE_CONFIG(_DIR)` overrides | `.opencode/settings.json` corrected; JSONC, unmanaged fields, and documented global overrides are retained. [Config docs](https://opencode.ai/docs/zh-tw/config/#位置) · [Loader](https://github.com/anomalyco/opencode/blob/v1.18.16/packages/opencode/src/config/config.ts) |
| Augment/Auggie         | `~/.augment/settings.json`, `mcpServers`; stdio/SSE/HTTP                                                                                          | Existing global-only descriptor matches the documented contract. [Docs](https://docs.augmentcode.com/cli/integrations)                                                                                                                                                        |
| Kilo Code              | Canonical `kilo.json[c]` layers under XDG/global and project; nested `mcp`; local/remote; `enabled`                                               | Legacy `.kilocode/mcp.json` assumption replaced by canonical path precedence and OpenCode-compatible format. [Source](https://github.com/Kilo-Org/kilocode/blob/v7.4.21/packages/opencode/src/cli/cmd/mcp.ts)                                                                 |
| Amp                    | `~/.config/amp/settings.json[c]`, nearest `.amp/settings.json[c]`; nested `amp.mcpServers`; `disabled`                                            | Project filename corrected; native disabled/transport fields are preserved. [Manual](https://ampcode.com/manual#mcp)                                                                                                                                                          |
| Zed                    | Platform config `settings.json`, project `.zed/settings.json`; `context_servers`; `command`/`args`/`env` or `url`/`headers`                       | Context-server key and platform-aware config are modeled. The documented entry has no transport tag and no per-server toggle, so neither is invented. [Docs](https://github.com/zed-industries/zed/blob/v1.14.2/docs/src/ai/mcp.md)                                           |
| Kiro                   | `~/.kiro/settings/mcp.json`, `.kiro/settings/mcp.json`; `mcpServers`; `disabled`                                                                  | Native disabled field and command/URL shape are retained. [Docs](https://kiro.dev/docs/cli/mcp/configuration/)                                                                                                                                                                |
| Warp                   | `~/.warp/.mcp.json`, `{repo}/.warp/.mcp.json`; `mcpServers`                                                                                       | Missing dot in the filename fixed; project/global wrapper fields survive. [Skill](https://github.com/warpdotdev/warp/blob/2249469e5d24e472cee6ce97d3d324293f67db71/resources/bundled/skills/add-mcp-server/SKILL.md)                                                          |
| Trae                   | `.trae/mcp.json`; `mcpServers`; URL-based remote transports                                                                                       | Stable global path is intentionally unsupported; project path and remote capability retained. [Docs](https://docs.trae.ai/ide/add-mcp-servers)                                                                                                                                |
| Factory/Droid          | `~/.factory/mcp.json`, `.factory/mcp.json`; `mcpServers`; `disabled`                                                                              | Native field is parsed/preserved; toggle is deliberately not advertised because Factory writes project-defined toggles as user-level overrides. [Docs](https://github.com/Factory-AI/factory/blob/1fd9026d72f81668d88f37237cb5a2e89a17e6e2/docs/cli/configuration/mcp.mdx)    |
| Kimi CLI               | `$KIMI_SHARE_DIR/mcp.json`, `mcpServers`; `transport: "http"`                                                                                     | Global-only scope and environment override fixed; canonical HTTP spelling is emitted. [Source](https://github.com/MoonshotAI/kimi-cli/blob/4a550effdfcb29a25a5d325bf935296cc50cd417/src/kimi_cli/cli/mcp.py)                                                                  |
| Mistral Vibe           | `$VIBE_HOME/config.toml`, `.vibe/config.toml`; `[[mcp_servers]]`; `transport`, `disabled`                                                         | Native array-of-tables TOML adapter replaces the incompatible generic table serializer.                                                                                                                                                                                       |
| Pi                     | No native MCP config; extension-owned                                                                                                             | Correctly remains unsupported; no third-party extension path is guessed. [README](https://github.com/earendil-works/pi/blob/v0.84.1/packages/coding-agent/README.md)                                                                                                          |
| JetBrains AI Assistant | IDE-managed persistence; no stable writable file path                                                                                             | Safely remains unsupported rather than inventing a path. [Docs](https://www.jetbrains.com/help/ai-assistant/mcp.html)                                                                                                                                                         |
| Hermes                 | Global YAML MCP registry; `transport: sse/http/streamable-http`                                                                                   | SSE transport and `$HERMES_HOME` are modeled; project scope remains unsupported.                                                                                                                                                                                              |
| Grok                   | Native TOML MCP tables with global/project scopes and enable state; `$GROK_HOME` override                                                         | Existing dedicated TOML strategy retained, with the override applied to MCP/data/skills paths.                                                                                                                                                                                |

## Cross-cutting fixes

- Each map dialect is declared ONCE as a `json_map::Dialect` (via the
  `json_map_dialect!` macro in the agent's own file) and drives BOTH the parser
  and the serializer. Configuring the halves separately is what let an agent
  read a transport or toggle it had no way to write back, so an unrelated
  `aghub mcp add` silently rewrote the user's SSE server as streamable HTTP.
- A dialect with no native word for a transport now REFUSES to write it instead
  of downgrading it, and never parses into one either. Same for the on/off
  field: a dialect without a persisted toggle does not report a disabled state
  the user could not change, and a dialect WITH one reads its own field first
  (reading the other lets a stale `enabled` flip a natively-disabled server back
  on). `crates/core/tests/mcp_dialect_roundtrip.rs` pins this for every agent in
  the registry, including new ones, against an exhaustive list of which agents
  may refuse — "any error counts as a deliberate refusal" would let a real
  regression pass as a skip.
- The refusal is predictable BEFORE any write: `supports_mcp_transport` asks the
  real serializer instead of reading the `remote` capability bit (which collapses
  SSE and streamable HTTP into one flag), so a multi-agent batch rejects the
  whole set up front rather than writing the agents that can take it and then
  failing on the one that cannot.
- A server aghub cannot read is a server the next save DELETES, because the
  writers keep only the names they were handed. Every dialect therefore rejects
  an entry it cannot model rather than skipping it — the Codex TOML reader used
  to skip silently.
- `json_map` parses JSONC and keeps root/server fields it does not own
  (OAuth/tool/cwd metadata survives). Scalars written where the MCP schema wants
  a string (a port as a bare number) are coerced rather than costing the user
  every other server in the file; structurally ambiguous entries are still
  rejected, because dropping them would delete them on the next save.
- Test isolation: overriding `$HOME` is not enough now that descriptors honour
  `OPENCODE_CONFIG_DIR`, `CODEX_HOME`, and friends — those outrank it. The CLI
  test harness clears them, and `descriptor_regression` clears them before
  asserting default paths.
- Native OpenClaw and Mistral formats have dedicated modules because their
  nesting/array-of-table shapes cannot be represented by the generic map.
- OpenCode writes the project-level canonical `opencode.json` by default and
  no longer writes `.opencode/settings.json`; the real OpenCode loader does not
  read that filename.
- Tests cover descriptor paths/capabilities, native serialization keys,
  transport/toggle round trips, preservation of unmanaged fields, CLI batch
  preflight, and an isolated CLI add regression.

## Deliberate ceilings

- Aghub's normalized transport model has no WebSocket variant, so an explicit
  WebSocket config is rejected rather than mislabelled as HTTP.
- Codex, OpenCode, KiloCode and Mistral Vibe have exactly one remote entry shape
  by construction (`url` alone, or `type: "remote"`), so aghub refuses to write
  an SSE server to them. Refusing is recoverable (switch to streamable HTTP);
  writing it and reading back something else is not.
- The `type` tag is still written for the JSON-map agents whose vendor docs only
  show it on stdio entries (Cursor, Trae, Windsurf, Antigravity, Kiro, Zed).
  Dropping it was tried and reverted: it makes SSE indistinguishable from
  streamable HTTP on the next read, and every config the previous release wrote
  already carries the tag — a reader that honours it while the writer refuses to
  reproduce it leaves those files unmanageable.
- The OpenCode descriptor selects one existing writable candidate; OpenCode's
  runtime merges several global/project JSON and JSONC layers. Supporting full
  precedence would need a merge-aware ConfigManager seam, not a second guessed
  filename.
- OpenClaw's dedicated adapter uses the repository JSONC parser; exotic JSON5
  constructs outside that parser are rejected instead of being rewritten.
- Copilot, Claude, Gemini, and similar agents expose no stable persisted
  per-server toggle in the documented file contract; Aghub does not invent one.
