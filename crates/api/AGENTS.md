# API CRATE KNOWLEDGE BASE

**Crate**: `aghub-api` — REST API server for aghub\
**Framework**: Rocket v0.5 + rocket_cors\
**Domain**: HTTP API exposing agent config operations

## STRUCTURE

Role map (`lib.rs` mounts the real route set — do not hardcode route counts):

- `lib.rs` / `main.rs` / `cli.rs` — Rocket build + standalone bin
- `state.rs` / `error.rs` / `extractors.rs` — `AppState`, `ApiError`, `AgentParam` / `ScopeParams` / `TrustedLocalOrigin`
- `credentials/` — token resolve + remote forwarding; host-scoped source→credential bindings (never in lock files)
- `skills/` — path containment, scan, lock helpers for skill routes
- `dto/` — ts-rs DTOs per domain (`bun run generate:dto` via `bin/export-dto.rs`)
- `routes/` — handlers under `/api/v1/` (one file per surface: agents, mcps, skills, skills_update, sources, coverage, sub_agents, credentials, inference, plugins, integrations, market)

## ROUTES

All under `/api/v1/`. **Source of truth: `lib.rs` + `routes/*.rs`.** Module → intent (paths drift; read the file):

| Module          | Surface intent                                                                   |
| --------------- | -------------------------------------------------------------------------------- |
| `agents`        | list agents + availability                                                       |
| `mcps`          | per-agent MCP CRUD, all-agents list, transfer/reconcile                          |
| `skills`        | skill CRUD/import/transfer/reconcile/by-path/prune/install/content/tree/lock/git |
| `skills_update` | check-updates, apply-update, accept-rename                                       |
| `sources`       | source list + diff                                                               |
| `coverage`      | per-agent coverage of `.agents/skills` master                                    |
| `sub_agents`    | sub-agent CRUD + transfer/reconcile                                              |
| `credentials`   | credential store + source bindings                                               |
| `inference`     | provider inventory, keyring keys, per-agent bindings/routing/presets             |
| `plugins`       | Claude Code plugin + marketplace lifecycle                                       |
| `integrations`  | code-editor open/preferences                                                     |
| `market`        | skills.sh search                                                                 |

Path params: `<agent>`, `<name>`. Scope via `ScopeParams` (`scope` + optional `project_root`). **No token auth** — see CORS below.

## CORS & BROWSER-DRIVE-BY DEFENCE

The desktop embeds this server on `127.0.0.1` (random port). There is still **no
token auth** (a shared token collides with the SSH-remote / multi-connection
model — that's why upstream's `ApiAuth` is deliberately NOT ported). Instead two
transport-agnostic layers block the real threat — a malicious web page driving
the localhost API — without any client-side token:

- **Layer 1 — CORS allow-list** (`lib.rs` `build_rocket`, the single construction
  point for both the standalone bin and the desktop-embedded server): origins are
  restricted to the webview's own (`tauri://localhost`, `http(s)://tauri.localhost`,
  `http://localhost:1420` dev), NOT `AllOrSome::All`. A cross-origin JSON POST
  fails preflight. `X-Aghub-Git-Tokens` stays allow-listed (remote forwarding).
- **Layer 2 — `TrustedLocalOrigin` request guard** (`extractors.rs`): rejects a
  present-but-foreign `Origin` (browser cross-origin) AND a present-but-foreign
  `Host` (DNS-rebinding, where no Origin is sent). Both checks are LENIENT when
  the header is absent, so CLI/curl/the SSH-tunnel proxy/local test client pass.
  Currently guarded (SSOT is the `_origin: TrustedLocalOrigin` args in
  `routes/*.rs`, not this list): `git/scan`, `git/install`, `git/sync`,
  `git/credential-status`, all 5 `/credentials*`, `inference/.../password`,
  `skills/check-updates`, `skills/apply-update`, `skills/accept-rename`,
  `skills/sources/diff`. **Gap**: the `inference` provider
  create/update/delete/sync + `set_api_key` routes also write the keyring but
  carry NO Layer-2 guard — they lean on Layer 1 (CORS) alone. Plain read-only
  list/get routes likewise rely on Layer 1.

> When adding a route that touches keyring/OS credentials or leaks
> credential-existence, add `_origin: TrustedLocalOrigin` to its handler.
> `allow_credentials: true` is retained (no cookie/HTTP-auth is used; the custom
> forwarding header is unaffected by that flag).

## RUNNING

```bash
# Run API server
cargo run -p aghub-api

# Or with custom port
cargo run -p aghub-api -- --port 8080
```

Default: localhost:8000

## PATTERNS

- `AppState` holds shared state; routes take agent/scope from extractors
- MCP / skill / sub-agent CRUD goes through `ConfigManager` (never bypass);
  other domains own their store — credentials → credential store, inference →
  `InferenceProviderStore`, plugins → `ClaudePluginManager`
- Errors: machine-readable codes + safe messages (no raw filesystem paths in responses)

## ANTI-PATTERNS

- NEVER widen CORS or add new mutating routes without considering the no-auth posture above
- (path/ConfigManager rules: see root AGENTS.md Anti-Patterns — errors here use machine codes + safe messages)
