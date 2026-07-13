# AGHUB KNOWLEDGE BASE

**Project**: aghub — AI coding agent configuration management tool\
**Stack**: Rust workspace (root `Cargo.toml`) + Tauri v2 desktop + React 19/TypeScript\
**Package manager**: cargo (Rust), **bun** (desktop frontend — never npm/yarn/pnpm)

> This is the single source of truth for project context. The root `CLAUDE.md`
> and every per-crate `CLAUDE.md` are one-line `@AGENTS.md` imports of the
> sibling `AGENTS.md` (not symlinks).
>
> **What this file is for**: orienting you to the modules — what each crate is,
> why it exists, how they depend on each other, and the invariants you must not
> break. It is the **navigation layer**, deliberately coarse.
>
> **For file/symbol-level work, use CodeGraph** (`.codegraph/` is indexed): one
> `codegraph_explore` call returns the verbatim source of the relevant symbols
> plus their callers/callees — more accurate and current than any hand-written
> list. This file does NOT enumerate files, line numbers, or symbols; those
> drift. It carries the _why_ and _what-not-to-break_ that CodeGraph cannot
> derive. `Cargo.toml` `[workspace].members` is the authoritative crate list.

## Overview

Aghub manages AI coding agent configurations across many agents (see
`AgentType::ALL` in `crates/agents/src/models.rs` — NOT `crates/core`), handling
MCP servers, skills, and sub-agents through a unified interface. It also manages
inference providers, Claude Code plugins, and SSH-based remote deployment.
Stateless design — it reads the actual config files, tracks capability sources,
and requires explicit opt-in for changes.

Delivered through three surfaces:

- **CLI** (`aghub-cli`) — clap-based command surface
- **HTTP API** (`aghub-api`) — Rocket v0.5 under `/api/v1/` (mounted set:
  `crates/api` `lib.rs` + `routes/` — do not hardcode a route count)
- **Desktop** (`crates/desktop`) — Tauri v2 embedding `aghub-api` on localhost

## Maps & Decisions (read these first)

- **Design specs**: `docs/specs/` — design rationale, not current-state truth; code wins
- **Domain language**: [`CONTEXT.md`](CONTEXT.md) (Source hash, Master, Referrer, Relink, …)
- **Load-bearing decisions**: [`docs/adr/`](docs/adr/)
- **Fork upstream sync log**: [`UPSTREAM.md`](UPSTREAM.md) — port / skip from `AkaraChen/aghub`
- **Deep domain playbooks**: project skills under `.claude/skills/` (auto-register in
  Claude Code; do not re-list the catalog here)
- `.impeccable.md` — Rust style; `cliff.toml` — git-cliff for releases

## Structure

Module map (crate → why it exists). Authoritative member list: `Cargo.toml`.

```
crates/
  agents/        # SSOT for agent behavior: descriptors, AgentType, models, format/
  core/          # orchestration: ConfigManager, registry, skills, transfer
  cli/           # aghub-cli binary
  api/           # Rocket HTTP server
  desktop/       # Tauri + React; src-tauri package name is `aghub` (−p aghub ≠ CLI)
  skill/         # .skill zip + npx-compatible locks + hashing
  skill-update/  # shared update-check + Sources domain (API check + CLI check/source)
  skills-sh/     # skills.sh registry client (search only)
  inference/     # providers: SQLite meta + keyring
  remote/        # SSH remote VM (desktop only, not API)
  cc-plugins/    # Claude Code plugin lifecycle
  git/           # clone/fetch + credential injection
  json/          # JSON/JSONC editing
  markdown/      # YAML frontmatter helpers
.agents/skills/  # universal skill Master (when used as a project)
justfile         # task runner
```

Cargo graph (depends-on): `agents` ← `core` ← `{cli, api}`; `desktop` → `api`
(+ `remote`), not core directly. Tool crates used laterally.
`skills-ref` is an external git dep (`AkaraChen/skills-ref`).

## Where to Look

| Task               | Location                          | Notes                        |
| ------------------ | --------------------------------- | ---------------------------- |
| Add agent support  | `crates/agents/src/agents/`       | + models + core registry     |
| Agent models/types | `crates/agents/src/models.rs`     | `AgentConfig`, `AgentType`   |
| Agent registry     | `crates/core/src/registry/mod.rs` | `ALL_AGENTS`                 |
| Config management  | `crates/core/src/manager/`        | `ConfigManager`              |
| Adapter trait      | `crates/core/src/adapters/mod.rs` | impl via `adapter.rs`        |
| Batch install/copy | `crates/core/src/transfer.rs`     | `OperationBatchResult`       |
| CLI commands       | `crates/cli/src/commands/`        | clap; `just start -- --help` |
| API routes         | `crates/api/src/routes/`          |                              |
| Desktop UI         | `crates/desktop/src/`             | HeroUI v3                    |

## Key Design Patterns

- **Adapter pattern**: `create_adapter(agent_type)` → `registry::get` →
  `&'static AgentDescriptor` implements `AgentAdapter`. **No hand-wired adapter
  structs** — behavior is function pointers on each descriptor.
- **Normalized model**: `AgentConfig` — `Vec<Skill>` + `Vec<McpServer>` +
  `Vec<SubAgent>` with `McpTransport` (`Stdio` | `Sse` | `StreamableHttp`).
- **ConfigManager**: CRUD for resources. MCP delete (`remove_mcp_planned`)
  rewrites shared config and deletes **no** disk path — `RemovalPlan.paths` is
  deliberately empty.

## Agent-Specific Behavior

Each agent's **descriptor** lives in `crates/agents/src/agents/<name>.rs` (not
core); the MCP **parse/serialize** logic it points at lives in
`crates/agents/src/format/`. Change either as the case needs.

- **Claude**: skills from `~/.claude/skills/` SKILL.md (not JSON). Disabled MCPs
  omitted on serialize; URL MCPs as `"type": "sse"/"http"`.
- **OpenCode**: `mcp` object key; SSE + StreamableHttp unify as `"type": "remote"`
  (SSE identity lost). Reads own dir **plus** `.agents/skills` Master — never
  another agent's private dir.
- **Codex/Mistral**: TOML.
- **Copilot**: global `~/.copilot/skills`; project scope reads Master.
- **Universal Master (`.agents/skills`)**: an agent reads the Master **only**
  where its descriptor maps that scope's skill paths there — **per-agent and
  per-scope**. Do **not** maintain a hand list here; read each descriptor's
  skill read paths. Invariant: each agent reads only its own dir + Master where
  mapped, never another agent's private dir. `capabilities.skills.universal:
true` (Amp, Kimi today) also appends XDG `$XDG_CONFIG_HOME/agents/skills`
  (default `~/.config/agents/skills`) — that is **not** `~/.agents/skills`.
- **`registry::get()` fallback**: unknown id → Claude's descriptor silently.

## Commands

Prefer `just --list`. Common:

```bash
just dev / just build / just start -- --help
just preflight          # fmt + clippy + typecheck + test + doc tests
just test               # workspace tests
just lint / just fmt
# Desktop: cd crates/desktop && bun run dev|start|test
# Single test: cargo test -p aghub-core <name> -- --exact
```

## CLI Command Surface

Authoritative: clap (`just start -- --help`, `crates/cli/src/commands/`).
Aliases: `skills`/`skill`, `mcps`/`mcp`. Scope: `-a`, `-g`/`-p`, `--all`.

Non-obvious invariants:

- **Destructive defaults**: `delete`, `apply-update`, `prune-lock`, `source sync`,
  `source accept-rename`, reconcile-with-removals → **dry-run unless `--yes`**
- Skill install is **always symlink-only**; `--universal` is a hidden no-op
- Source creds: `GIT_PASSWORD` (any host) / `GITHUB_TOKEN` (github.com https-only)
- **`skill-usage`**: Claude-global only; rejects project/`--all`
- **`coverage`**: rejects `--all`; scope `-g` or `-p` only

## Skills Discovery

Skills = dirs with `SKILL.md` + YAML frontmatter. `Skill.source_path` records
load origin. Locks track content hashes.

**Link decision**: `classify_agent` / `agent_link_need`
(`crates/core/src/skills/linker/classify.rs`). NativeReader → Master only, **no**
per-agent link. Both install paths must use it — CLI
`add_skill_universal` / `add_skill_from_path_universal` and fetched
`install_universal` (they diverged once).

## Adding / Removing an Agent

1. `crates/agents/src/agents/<name>.rs` — descriptor (`codex/` is a subdir;
   `factory.rs` = Factory-AI agent, not a dispatch factory)
2. `crates/agents/src/agents/mod.rs` — `pub mod`
3. `crates/agents/src/models.rs` — `AgentType` + `ALL` + `as_str`/`from_str`
4. `crates/core/src/registry/mod.rs` — `ALL_AGENTS` entry (find-by-id; no match)

## Testing

`TestConfig` + `set_skills_path_override` (thread-local). Suites: core
`integration_tests`, `mcp_tests`, `test_agent_paths`, `sources_install_tests`,
cli `cli_tests`.

**Do not pollute real home**: clearing override under **global** scope still
writes master to `dirs::home_dir()/.agents/skills`. Isolate `$HOME` (Unix) or
use project `tempdir` + teardown — see `crates/core/AGENTS.md` Testing.

## Agent permissions / approval boundaries

| AI may do autonomously                        | Ask first                                         |
| --------------------------------------------- | ------------------------------------------------- |
| Edit code, fmt/clippy, package-scoped tests   | `git push`, force-push, amend published history   |
| `just fmt` / `just lint` / single-crate tests | Release tags, `just bump`, Homebrew tap           |
| Read-only under temp dirs                     | Real `~/.agents`, keyring, `tauri` updater pubkey |
|                                               | New workspace deps without a clear need           |

Never commit secrets. Prefer file-scoped tests over full `just test` unless
verifying a release gate.

## Conventions

**Rust**: hard tabs (width 4); 80-col; `rustfmt`; `clippy -D warnings`.\
**TS**: `bun` only; React 19 + HeroUI v3; Tailwind v4; strict TS.\
**Org**: one agent = one file under `agents/`; no hand-wired adapters.

## Anti-Patterns

- NEVER spaces for Rust indent; NEVER ignore clippy; NEVER skip the 4 agent-wiring steps
- NEVER bypass `ConfigManager`
- NEVER return arbitrary internal temp/lock/keyring paths in API **errors**;
  skill DTOs may expose intentional `source_path` / `canonical_path` for UI

## Release & Packaging

Full runbook: project skill **`releasing-aghub`** + `.github/workflows/release.yml`.
Hard gotchas only:

- Tag `v*` after green CI; `just preflight` first — **pre-push does NOT run tests**
- Version from git tag (CI sed); don't hand-bump for release
- **Never change** shipped `tauri.conf.json` updater `pubkey` / wrong `endpoints`
- Unset `APPLE_*` secrets break macOS (`security import` empty cert) — leave
  commented until real certs exist
- Homebrew tap needs `HOMEBREW_TAP_TOKEN` (default `GITHUB_TOKEN` cannot write it)
- Unix-gated test helpers need the same `#[cfg]` as callers or Windows CI clippy fails

## Configuration Paths

Per-descriptor in `crates/agents/src/agents/<name>.rs` — do not maintain a full
table. Examples: Claude `~/.claude.json` / `.mcp.json` / `~/.claude/skills/`;
OpenCode `~/.config/opencode/opencode.json` / `.opencode/settings.json` /
own skills + Master.

Project root: walk up for agent markers (`.claude/`, `.opencode/`, `.cursor/`,
`.mcp.json`, …). **`.git` alone is not enough.**
