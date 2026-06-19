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
├── credentials/         # Credential token resolution
│   └── resolve.rs       #   host-scoped source→credential bindings (never written to lock files)
├── skills/              # Skill-route helpers: path containment, scan, lock
├── dto/                 # ts-rs-exported DTOs, one file per domain (skill, mcp, sub_agent,
│   └── …                #   inference, credential, sources, plugin, agents, market, …) + data/
└── routes/              # HTTP handlers, mounted under /api/v1/
    ├── mod.rs catchers.rs
    ├── agents.rs mcps.rs skills.rs skills_update.rs sources.rs
    └── sub_agents.rs credentials.rs inference.rs plugins.rs integrations.rs market.rs
```

## ROUTES

All mounted under `/api/v1/`. **`lib.rs` (the mounted set) + `routes/*.rs` are the source of truth — ~103 handlers across 11 modules.** Do NOT treat this map as exhaustive per-path; it covers every module:

| Module (`routes/…`) | #   | Surface                                                                                                                                                                                                                                                                                                                                                   |
| ------------------- | --- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `agents.rs`         | 2   | `GET /agents`, `GET /agents/availability`                                                                                                                                                                                                                                                                                                                 |
| `mcps.rs`           | 10  | per-agent MCP CRUD + enable/disable (`/agents/<agent>/mcps[/<name>]`), `GET /agents/all/mcps`, `POST /mcps/transfer`, `/mcps/reconcile`                                                                                                                                                                                                                   |
| `skills.rs`         | 23  | per-agent skill CRUD + enable/disable + `import`, `GET /agents/all/skills`; `transfer`/`reconcile`; `DELETE /skills/by-path`; `prune-lock`; `install`/`open`/`edit`; **`GET /skills/content`, `/skills/tree`** (take `scope`+`project_root`, constrained to allow-listed roots); `lock/global`, `lock/project`; **`git/scan`, `git/install`, `git/sync`** |
| `skills_update.rs`  | 2   | `GET /skills/check-updates`, `POST /skills/apply-update`                                                                                                                                                                                                                                                                                                  |
| `sources.rs`        | 2   | `GET /skills/sources`, `/skills/sources/diff` (npx-style source browse/diff)                                                                                                                                                                                                                                                                              |
| `sub_agents.rs`     | 8   | per-agent sub-agent CRUD (`/agents/<agent>/sub-agents[/<name>]`), `GET /agents/all/sub-agents`, `transfer`/`reconcile`                                                                                                                                                                                                                                    |
| `credentials.rs`    | 5   | `GET`/`POST /credentials`, `DELETE /credentials/<id>`, `GET`/`PUT /credentials/source-bindings`                                                                                                                                                                                                                                                           |
| `inference.rs`      | 26  | provider CRUD + keyring password; per-agent (claude/codex/opencode) provider bindings, model routing, profile, catalog `sync`, `state`; `presets`                                                                                                                                                                                                         |
| `plugins.rs`        | 21  | Claude Code plugin lifecycle (install/uninstall/update/enable/disable/detail/config/prune/validate/open), marketplaces CRUD + update, `plugins-market`, `cli/status`                                                                                                                                                                                      |
| `integrations.rs`   | 3   | `GET /integrations/code-editors`, `POST /integrations/open-with-editor`, `GET /integrations/preferences`                                                                                                                                                                                                                                                  |
| `market.rs`         | 1   | `GET /skills-market/search` (skills.sh registry)                                                                                                                                                                                                                                                                                                          |

Path params: `<agent>` (agent id), `<name>` (resource name). Scope is a query guard — `?<scope..>` (`ScopeParams`: `scope` + optional `project_root`); the skill content/tree routes pass `scope`+`project_root` explicitly to compute allow-listed roots. There is currently **no token auth** on the API (see CORS below).

## CORS CONFIGURATION

- Allowed origins: All (`AllOrSome::All`)
- Allowed methods: GET, POST, PUT, DELETE
- Allowed headers: Authorization, Accept, Content-Type
- Credentials: allowed

> The desktop embeds this server on `127.0.0.1` (random port). There is **no
> token auth / origin guard** today — CORS is wide open and `credentials: true`.
> Upstream added `ApiAuth` + `TrustedLocalOrigin`; porting them is deferred
> because token injection collides with this fork's multi-connection / SSH-remote
> server model. Treat the API as same-host-trusted for now.

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

- NEVER expose raw filesystem paths in API responses (errors use machine codes + safe messages)
- NEVER bypass `ConfigManager` — always use adapter pattern
- NEVER widen CORS or add new mutating routes without considering the no-auth posture above
