# API CRATE KNOWLEDGE BASE

**Crate**: `aghub-api` — REST API server for aghub\
**Framework**: Rocket v0.5 + rocket_cors\
**Domain**: HTTP API exposing agent config operations

## STRUCTURE

Role map (`lib.rs` mounts the real route set — do not hardcode route counts):

- `lib.rs` / `main.rs` / `cli.rs` — Rocket build + standalone bin
- `state.rs` / `error.rs` / `extractors.rs` — `AppState`, `ApiError`, `AgentParam` / `ScopeParams` / `TrustedLocalOrigin`
- `credentials/` — token resolve + remote forwarding; host-scoped source→credential bindings (never in lock files)
- `skills/` — rename-guard + resync helpers for skill routes (scan/lock
  orchestration moved to the `skill-update` crate)
- `source_sessions.rs` — TTL'd `PinnedSourceSession` cache pairing a
  `SkillRepository` + `RepoSnapshot` so git scan→install flows reuse one fetch
- `editor_detection.rs` — code-editor discovery for the integrations surface
- `dto/` — ts-rs DTOs per domain (`bun run generate:dto` via `bin/export-dto.rs`;
  a new DTO must be REGISTERED there or it silently doesn't generate. An
  `Option` field with `skip_serializing_if` also needs `#[ts(optional)]`, or
  the generated TS declares it required — unsound contract)
- `routes/` — handlers under `/api/v1/`, one file per surface (the ROUTES table below)

## ROUTES

All under `/api/v1/`. **Source of truth: `lib.rs` + `routes/*.rs`.** Module → intent (paths drift; read the file):

| Module          | Surface intent                                                                                           |
| --------------- | -------------------------------------------------------------------------------------------------------- |
| `agents`        | list agents + availability                                                                               |
| `mcps`          | per-agent MCP CRUD, all-agents list, transfer/reconcile, multi-agent batch create (`core::batch` policy) |
| `skills`        | skill CRUD/import/transfer/reconcile/by-path/prune/install/content/tree/lock/git                         |
| `skills_update` | check-updates, apply-update, accept-rename                                                               |
| `sources`       | source list + diff                                                                                       |
| `coverage`      | static per-agent capability matrix (`classify_all`) — reads no master, names no skill                    |
| `sub_agents`    | sub-agent CRUD + transfer/reconcile                                                                      |
| `credentials`   | credential store + source bindings                                                                       |
| `inference`     | provider inventory, keyring keys, per-agent bindings/routing/presets                                     |
| `plugins`       | Claude Code plugin + marketplace lifecycle                                                               |
| `integrations`  | code-editor open/preferences                                                                             |
| `market`        | skills.sh search                                                                                         |

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
  **Policy**: every `/api/v1` route (except OPTIONS preflight) takes
  `_origin: TrustedLocalOrigin` as its first parameter. Layer 1 blocks foreign
  Origin; Layer 2 blocks foreign Host / DNS-rebinding. Coverage is enforced by
  `all_routes_reject_foreign_host` in `lib.rs` tests — any new non-OPTIONS route
  that omits the guard fails that enumeration.

> When adding any non-OPTIONS `/api/v1` route, add `_origin: TrustedLocalOrigin`
> first in the handler parameter list. `allow_credentials: true` is retained
> (no cookie/HTTP-auth is used; the custom forwarding header is unaffected by
> that flag).

## RUNNING

Default port is `0` — the OS assigns an ephemeral port, and the bound port is
printed to stdout after bind (the desktop / SSH-tunnel callers **parse that
line** — don't reword it). Pass `--port N` to pin one.

## PATTERNS

- `AppState` holds shared state; routes take agent/scope from extractors
- MCP / skill / sub-agent CRUD goes through `ConfigManager` (never bypass);
  other domains own their store — credentials → credential store, inference →
  `InferenceProviderStore`, plugins → `ClaudePluginManager`
- Errors: machine-readable codes + safe messages — no internal temp/lock/keyring
  paths (user-config paths, e.g. the missing-config-file message, are intentional)
- Route error PRECEDENCE is public contract (e.g. MCP create answers
  capability → validate → writable): refactoring a route into shared helpers
  must not reorder which error wins on compound-invalid requests

## SURFACE SEMANTICS THAT DIFFER FROM THE CLI

- **`GET /skills/sources/diff?scope=all` refuses an origin-ambiguous source**
  (`SOURCE_AMBIGUOUS`) where the CLI reports it per scope. The response is ONE
  flat merged list with a single `source` field, so it has nowhere to attribute
  a forge per scope — a consequence of the merged-vs-per-scope shapes, not a
  second ambiguity rule. Both sides are pinned by tests.
- `delete`'s `outcome` gains an api-only `failed` for early errors; the rest of
  the vocabulary is the CLI's (`crates/cli/AGENTS.md`).
- The by-name skill and MCP delete routes take `?agents=a,b`: every agent ONE
  user action deletes from. A shared config file / Referrer is removed only
  when all of its readers are in it; desktop sends the group's agents. Absent
  means the path agent alone; an unknown id is a 400.
- `DELETE /skills/by-path` accepts only a skill directory (own parsable
  `SKILL.md`) or that `SKILL.md`; a category folder, a skill's subdirectory or
  stray file, and an unresolvable path (ELOOP) are refused with path-free
  messages — core's `by_path_skill_dir` / `by_path_skill_name`. Why:
  `docs/history/api.md#delete-by-path-target-must-be-a-skill-root`.
- Batch routes answer HTTP 200 for a handled batch whose rows failed — the row's
  `error` is the answer, and failed rows are also logged (never the source URL
  or a forwarded token).
- `POST /skills/updates/check` deliberately reads the lock fail-OPEN (a corrupted
  or unreadable lock is treated as empty/missing; the CLI `check` is fail-closed,
  because CLI check presents lock contents as its answer and probes first).
- Codes that legitimately remain API-only (request-shape/policy checks owned by the HTTP surface, not outcomes of a Resync):
    - `SKILL_SOURCE_MISMATCH`: the API session URL does not match what the skill is locked to; request validation owned by the HTTP sync session guard.
    - `MISSING_PARAM`: `project_root` is required when `scope` is project; request-shape validation owned by the HTTP boundary.
    - `INVALID_SCOPE`: scope must be global or project; request-shape validation owned by the HTTP boundary.
    - `confirm=true` requirement (`CONFIRMATION_REQUIRED`): safety gate requiring explicit confirmation to overwrite installed skills or accept renames; policy check owned by the HTTP surface.
    - batch-names cap (`INVALID_PARAM`): limit of 256 names per batch request; payload policy check owned by the HTTP boundary.

## ANTI-PATTERNS

- NEVER widen CORS or add new mutating routes without considering the no-auth posture above
- **NEVER take the skill mutation lock (or call a `core` flow that does) directly in
  a handler body** — wrap that transaction in `blocking::in_mutation_pool`.
  Acquiring blocks the thread for up to 10s, and Rocket's worker count is the CPU
  count, so enough contended mutations park every worker and the server stops
  answering **everything**, unlocked read routes included (measured: 25 concurrent
  deletes against a held lock took `GET /agents` from 0.00s to a 30s timeout).
  There is no compile-time guard; the awaits (git fetch, plugin detection) stay
  OUTSIDE the closure, which is also where they belong relative to the lock.
- NEVER orchestrate two or more core mutation steps within a handler to form a
  single action — orchestration belongs in core behind one entry point.
- NEVER write an error-code string literal in API route code — error codes come
  directly from core errors via `aghub_core::error_codes` (or `ApiError::from`).
- (path/ConfigManager rules: see root AGENTS.md Anti-Patterns — errors here use machine codes + safe messages)
