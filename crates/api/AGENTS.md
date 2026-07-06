# API CRATE KNOWLEDGE BASE

**Crate**: `aghub-api` — REST API server for aghub\
**Framework**: Rocket v0.5 + rocket_cors\
**Domain**: HTTP API exposing agent config operations

## STRUCTURE

```
crates/api/src/
├── main.rs              # Binary entry point
├── lib.rs               # Exports + route mounting — SOURCE OF TRUTH for the mounted set
├── cli.rs               # CLI arg parsing for the standalone server bin
├── state.rs             # AppState, agent registry
├── error.rs             # ApiError + ErrorBody (HTTP status + machine-readable code)
├── extractors.rs        # Rocket request guards: AgentParam, ScopeParams
├── editor_detection.rs  # Detect installed code editors (integrations routes)
├── bin/export-dto.rs    # ts-rs DTO export bin (`bun run generate:dto` calls this)
├── credentials/         # Token resolution + remote forwarding (resolve/origin/public/forwarding);
│                        #   host-scoped source→credential bindings, never written to lock files
├── skills/              # Skill-route helpers: path containment, scan, lock
├── dto/                 # ts-rs-exported DTOs, one file per domain (skill, mcp, sub_agent,
│   └── …                #   inference, credential, sources, plugin, agents, market, …) + data/
└── routes/              # HTTP handlers, mounted under /api/v1/
    ├── mod.rs catchers.rs
    ├── agents.rs mcps.rs skills.rs skills_update.rs sources.rs coverage.rs
    └── sub_agents.rs credentials.rs inference.rs plugins.rs integrations.rs market.rs
```

## ROUTES

All mounted under `/api/v1/`. **`lib.rs` (the mounted set) + `routes/*.rs` are the source of truth — count them there, don't trust a stale number here.** Do NOT treat this map as exhaustive per-path; it covers every module:

| Module (`routes/…`) | #   | Surface                                                                                                                                                                                                                                                                                                                                                                            |
| ------------------- | --- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `agents.rs`         | 2   | `GET /agents`, `GET /agents/availability`                                                                                                                                                                                                                                                                                                                                          |
| `mcps.rs`           | 10  | per-agent MCP CRUD + enable/disable (`/agents/<agent>/mcps[/<name>]`), `GET /agents/all/mcps`, `POST /mcps/transfer`, `/mcps/reconcile`                                                                                                                                                                                                                                            |
| `skills.rs`         | 24  | per-agent skill CRUD + enable/disable + `import`, `GET /agents/all/skills`; `transfer`/`reconcile`; `DELETE /skills/by-path`; `prune-lock`; `install`/`open`/`edit`; **`GET /skills/content`, `/skills/tree`** (take `scope`+`project_root`, constrained to allow-listed roots); `lock/global`, `lock/project`; **`git/scan`, `git/install`, `git/sync`**, `git/credential-status` |
| `skills_update.rs`  | 3   | `GET /skills/check-updates`, `POST /skills/apply-update`, `POST /skills/accept-rename`                                                                                                                                                                                                                                                                                             |
| `sources.rs`        | 2   | `GET /skills/sources`, `/skills/sources/diff` (npx-style source browse/diff)                                                                                                                                                                                                                                                                                                       |
| `coverage.rs`       | 1   | `GET /skills/coverage` (read-only per-agent coverage of the `.agents/skills` master)                                                                                                                                                                                                                                                                                               |
| `sub_agents.rs`     | 8   | per-agent sub-agent CRUD (`/agents/<agent>/sub-agents[/<name>]`), `GET /agents/all/sub-agents`, `transfer`/`reconcile`                                                                                                                                                                                                                                                             |
| `credentials.rs`    | 5   | `GET`/`POST /credentials`, `DELETE /credentials/<id>`, `GET`/`PUT /credentials/source-bindings`                                                                                                                                                                                                                                                                                    |
| `inference.rs`      | 26  | provider CRUD + keyring password; per-agent (claude/codex/opencode) provider bindings, model routing, profile, catalog `sync`, `state`; `presets`                                                                                                                                                                                                                                  |
| `plugins.rs`        | 21  | Claude Code plugin lifecycle (install/uninstall/update/enable/disable/detail/config/prune/validate/open), marketplaces CRUD + update, `plugins-market`, `cli/status`                                                                                                                                                                                                               |
| `integrations.rs`   | 3   | `GET /integrations/code-editors`, `POST /integrations/open-with-editor`, `GET /integrations/preferences`                                                                                                                                                                                                                                                                           |
| `market.rs`         | 1   | `GET /skills-market/search` (skills.sh registry)                                                                                                                                                                                                                                                                                                                                   |

Path params: `<agent>` (agent id), `<name>` (resource name). Scope is a query guard — `?<scope..>` (`ScopeParams`: `scope` + optional `project_root`); the skill content/tree routes pass `scope`+`project_root` explicitly to compute allow-listed roots. There is currently **no token auth** on the API (see CORS below).

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
  Mounted ONLY on credential/keyring-touching + oracle routes: `git/scan`,
  `git/install`, `git/sync`, `git/credential-status`, all 5 `/credentials*`, and
  `inference/.../password`. Read-only list/get routes rely on Layer 1.

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

## DEPENDENCIES

- `aghub-core` — core library (re-exports `aghub-agents`)
- `aghub-inference` — inference provider routes
- `aghub-cc-plugins` — Claude Code plugin lifecycle (plugins routes)
- `aghub-git` — git clone/scan with credential injection (skills git routes)
- `skills-sh` — skills.sh registry client (market)
- `rocket` + `rocket_cors` — web framework + CORS
- `keyring` — credential/keychain storage
- `url` — URL validation (e.g. github.com credential guard)
- `tokio` — async runtime

## PATTERNS

- Uses `AppState` for shared agent registry
- Routes extract agent type from path params
- Delegates to `ConfigManager` for actual operations
- Error handling via Rocket catchers

## ANTI-PATTERNS

- NEVER widen CORS or add new mutating routes without considering the no-auth posture above
- (path/ConfigManager rules: see root AGENTS.md Anti-Patterns — errors here use machine codes + safe messages)
