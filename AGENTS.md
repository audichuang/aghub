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

Aghub manages AI coding agent configurations across **23 agents** (Claude, OpenCode, Cursor, Windsurf, Copilot, RooCode, Cline, Gemini, Codex, Zed, Warp, and more), handling MCP servers, skills, and sub-agents through a unified interface. It also manages inference providers, Claude Code plugins, and SSH-based remote deployment. Stateless design — it reads the actual config files, tracks capability sources, and requires explicit opt-in for changes.

Delivered through three surfaces:

- **CLI** (`aghub-cli`) — clap-based command surface
- **HTTP API** (`aghub-api`) — Rocket v0.5 server, ~105 routes under `/api/v1/`
- **Desktop** (`crates/desktop`) — Tauri v2 app embedding `aghub-api` on localhost

Full agent list: the `AgentType` enum in `crates/agents/src/models.rs` (NOT `crates/core`).

## Maps & Decisions (read these first)

- **Design specs**: `docs/specs/` — date-stamped design docs (largely shipped by now; e.g. the Sources page + `.agents`-symlink universal install). Treat them as design rationale, not current-state truth — the code wins.
- **Domain language**: [`CONTEXT.md`](CONTEXT.md) is the glossary (Source hash, Master, Referrer, Relink, …).
- **Load-bearing decisions**: [`docs/adr/`](docs/adr/) (e.g. transactional skill rename).
- **Fork upstream sync log**: [`UPSTREAM.md`](UPSTREAM.md) — what we port / deliberately skip from `AkaraChen/aghub` (the **fork** upstream, distinct from the npx `skills` ecosystem upstream the skills above mirror).
- **Deep, reusable domain knowledge** lives in project skills under `.claude/skills/` (skill-subsystem invariants, the frozen npx round-trip contract, the upstream vercel `skills` flow, forcing fs failures in tests, the release runbook). In Claude Code these **auto-register each session** — invoke them by name; this file does not re-list them. Other agents can read the `SKILL.md` files directly.
- `.impeccable.md` — project code style guide (read before writing Rust); `cliff.toml` — git-cliff changelog config used by the release workflow.

## Structure

Module map (what each crate is + why it exists). Not an exhaustive file listing
— `Cargo.toml` is the authoritative member list; use CodeGraph for files/symbols.

```
.
├── crates/
│   ├── agents/       # aghub-agents: SINGLE SOURCE OF TRUTH for agent behavior —
│   │                 #   AgentDescriptor constants, AgentType enum + normalized
│   │                 #   models (AgentConfig, Skill, McpServer), format/ serializers
│   ├── core/         # aghub-core: orchestration — re-exports agents; ConfigManager,
│   │                 #   registry, adapter dispatch, skills discovery, cross-agent transfer
│   ├── cli/          # aghub-cli (bin aghub-cli): clap commands
│   ├── api/          # aghub-api: Rocket HTTP server (~105 routes)
│   ├── desktop/      # Tauri v2 + React 19 + HeroUI v3 + Tailwind v4 (bun);
│   │                 #   src-tauri package is `aghub` — `-p aghub` builds THIS, not the CLI
│   ├── skill/        # skill: .skill/zip packaging + npx-compatible lock files, content hashing
│   ├── skill-update/ # skill-update: shared update-check orchestrator (group →
│   │                 #   ls-refs preflight → treeless fetch → hash compare); used
│   │                 #   by BOTH the API check route and CLI `check --online`.
│   │                 #   ALSO hosts the Sources domain service (`sources` mod:
│   │                 #   list/diff/classify + injected Fetcher/TokenResolver),
│   │                 #   consumed by the API sources routes AND CLI `source`
│   ├── skills-sh/    # skills-sh: skills.sh registry HTTP client (search only)
│   ├── inference/    # aghub-inference: inference providers (SQLite meta + keyring)
│   ├── remote/       # aghub-remote: SSH remote VM mgmt (desktop Tauri layer, NOT the API)
│   ├── cc-plugins/   # aghub-cc-plugins: Claude Code plugin lifecycle
│   ├── git/          # aghub-git: git clone/fetch with credential injection
│   ├── json/         # aghub-json: JSON/JSONC editing
│   └── markdown/     # aghub-markdown: YAML frontmatter parsing helpers
├── .agents/skills/   # universal skill Master
├── justfile          # task runner
└── AGENTS.md         # this file
```

Dependency direction: `agents` → `core` → `cli`/`api`/`desktop`; the tool crates are used laterally. `skills-ref` is an **external git dependency** (`AkaraChen/skills-ref`), not a local crate.

## Where to Look

Coarse pointers to the right crate/module. For the exact symbol, its callers,
and verbatim source, run `codegraph_explore` instead of reading by hand.

| Task               | Location                          | Notes                            |
| ------------------ | --------------------------------- | -------------------------------- |
| Add agent support  | `crates/agents/src/agents/`       | Create `<name>.rs` descriptor    |
| Agent models/types | `crates/agents/src/models.rs`     | `AgentConfig`, `AgentType`       |
| Agent registry     | `crates/core/src/registry/mod.rs` | `ALL_AGENTS` array (cross-crate) |
| Config management  | `crates/core/src/manager/mod.rs`  | `ConfigManager` struct           |
| Adapter trait      | `crates/core/src/adapters/mod.rs` | `AgentAdapter` trait             |
| Batch install/copy | `crates/core/src/transfer.rs`     | `OperationBatchResult`           |
| CLI commands       | `crates/cli/src/commands/`        | Clap-based subcommands           |
| API routes         | `crates/api/src/routes/`          | Rocket route handlers            |
| Desktop UI         | `crates/desktop/src/`             | React + HeroUI v3 (search docs)  |

## Key Design Patterns

- **Adapter pattern**: the `AgentAdapter` trait. All agents dispatch through `create_adapter(agent_type)` → `registry::get(agent_type)` → `&'static AgentDescriptor`, which implements `AgentAdapter` via `adapter.rs`. There are **no hand-wired adapter structs** — behavior is entirely driven by function pointers on each descriptor.
- **Normalized model**: `AgentConfig` in `models.rs` — `Vec<Skill>` (frontmatter: name, description, author, version, tools) + `Vec<McpServer>` with `McpTransport` (`Stdio` | `Sse` | `StreamableHttp`).
- **ConfigManager** (`manager/` — mod/skill/mcp/sub_agent): central abstraction coordinating adapter operations; CRUD for resources. MCP delete (`remove_mcp_planned`) rewrites the shared config and deletes NO disk path — its `RemovalPlan.paths` is deliberately empty.

## Agent-Specific Behavior

Defined entirely in `crates/agents/src/agents/<name>.rs` descriptor constants (NOT in `crates/core`).

- **Claude**: skills are NOT stored in JSON; discovered from `~/.claude/skills/` SKILL.md files. Disabled MCPs are omitted on serialize (URL-based ones serialize as `"type": "sse"/"http"`).
- **OpenCode**: native format with `mcp` object key (not `mcp_servers` array). SSE and StreamableHttp unified as `"type": "remote"` — SSE identity is lost on roundtrip. Reads skills from its own dir (`.opencode/skills` project / `~/.config/opencode/skills` global) **plus** the universal `.agents/skills` Master — never another agent's private dir.
- **Codex/Mistral**: TOML config format.
- **Copilot**: own global dir `~/.copilot/skills`; at project scope reads the `.agents/skills` Master.
- **Universal-master reads** (`.agents/skills`): an agent reads the Master only where its descriptor maps that scope's skills dir to `.agents/skills` — **per-agent and per-scope, not a blanket rule**. At **project** scope, `<root>/.agents/skills` is read by Codex, OpenCode, Cursor, Cline, Copilot, Gemini, Antigravity, Amp, Kimi, Warp. At **global** scope, `~/.agents/skills` is read by a smaller subset — **Codex, OpenCode, Cursor, Cline, Warp**; the rest (Claude, Gemini, Copilot, Kiro, Windsurf, Trae, RooCode, Mistral, Pi, KiloCode, …) read only their own per-agent global dir. Invariant: each agent reads ONLY its own dir + the Master where mapped, and never another agent's private dir (e.g. Cursor/OpenCode do **not** read `.claude/skills` or `.codex/skills`). Only **Amp** and **Kimi** set `capabilities.skills.universal: true`, which additionally appends the XDG `$XDG_CONFIG_HOME/agents/skills` (default `~/.config/agents/skills`) — that is the XDG path, **not** `~/.agents/skills`.
- **`registry::get()` fallback**: returns Claude's descriptor silently if the agent ID is not found.

## Commands

Use `just`:

```bash
# Build & run
just dev                       # Debug build
just build                     # Release build
just start -- --help           # Run CLI with cargo
just start -- -a claude get skills   # List skills
just install                   # Release build → ~/.cargo/bin/

# Test
just preflight                 # pre-release gate: fmt --check + clippy + typecheck + test + doc tests
just test                      # All workspace tests
just integration-test          # Integration tests only
just test-with-validation      # Requires real CLIs (claude, opencode, …)
cargo test --package aghub-core <name> -- --exact   # single test

# Lint / format
just lint                      # clippy with warnings as errors
just fmt                       # rustfmt (Rust) + prettier (JS/TS)

# Desktop
cd crates/desktop && bun run dev     # Vite dev
cd crates/desktop && bun run start   # Tauri dev
cd crates/desktop && bun run test    # frontend unit tests (node:test, src/**/*.test.ts)
```

## CLI Command Surface

```
aghub-cli [-a <agent>] [-g|--global] [-p|--project] [--all] [-v|--verbose] <command>

  get    <skills|mcps>                # list resources
  add    <skills|mcps>                # --name, --from PATH, --command, --url, --transport,
                                      #   --header KEY:VALUE, --env KEY=VAL, --timeout SECONDS,
                                      #   --description, --author, --version, --tools.
                                      #   Skill installs are ALWAYS symlink-only (.agents master +
                                      #   per-agent link); --universal is a deprecated hidden no-op
  update <skills|mcps> <name>         # same flags as add
  delete <skills|mcps> <name>         # --all-agents, --dry-run (default), --yes to actually remove
  enable/disable <skills|mcps> <name> # soft toggle; only meaningful for OpenCode
  describe <skills|mcps> <name>       # JSON output for a single resource (inline in main.rs)
  check <skills|mcps>                 # read-only update status; offline by default (remote sources
                                      #   report uncheckable); --online (alias --check-remote) runs the
                                      #   shared orchestrator (env creds same as source cmds); --json
  apply-update <skills> <name>        # apply a locked skill update; dry-run default, --yes; --json
  prune-lock                          # drop lock entries with no on-disk skill (dry-run by default; --yes)
  source list                         # list installed sources (current project + global); --json
  source diff <source>                # read-only per-skill state vs installed
                                      #   (notInstalled/installedCurrent/installedOutdated/renamed/removed/
                                      #    deprecated/uncheckable); --ref R, --json
  source sync  <source>               # --install-missing (notInstalled only, excludes deprecated/renamed/removed)
                                      #   and/or --update (installedOutdated only); dry-run default, --yes to apply;
                                      #   scope -g|-p, install agent via -a <agent>, --ref R, --json.
                                      #   credentials: GIT_PASSWORD (any host) / GITHUB_TOKEN (github.com
                                      #   only, https-only) — token-only, no username
  source accept-rename <old> <new>    # accept an upstream skill rename as one transaction (install new,
                                      #   remove old, rewrite lock; rolls back on failure); --ref, --yes
                                      #   (dry-run default), --json
  inference <list|get|add|update|delete|key>   # inference provider inventory CRUD
                                      #   (shared SQLite store + OS-keyring keys; not agent-scoped).
                                      #   add: --latin-name --display-name --format --api-base-url
                                      #   --model (repeatable), key from --api-key/stdin/$AGHUB_INFERENCE_API_KEY;
                                      #   delete needs --yes; keys are write-only (never printed); --json
  transfer <skill|mcp|sub-agent>      # copy a resource from --from-agent into --to <agent>
                                      #   (repeatable); scope -g|-p; non-zero exit on any failure; --json
  reconcile <skill|mcp|sub-agent>     # --from-agent --name, then --add / --remove <agent>
                                      #   (repeatable) to match a desired agent set; scope -g|-p; --json.
                                      #   With removals: dry-run default, --yes to apply; the same agent
                                      #   in both --add and --remove is rejected
  coverage                            # read-only per-agent skill coverage of the
                                      #   .agents/skills master; scope -g|-p (rejects --all); --json
  skill-usage                         # Claude skill usage counts from ~/.claude.json's skillUsage,
                                      #   least-used first (read-only, Claude-only); --json
  plugin <list|install|uninstall|update|enable|disable|prune|validate>   # Claude Code plugins
  plugin marketplace <add|remove|update|list>
```

Resource type aliases: `skills`/`skill`, `mcps`/`mcp`.

## Skills Discovery

Skills load from directories containing a `SKILL.md`; the adapter parses YAML frontmatter (between `---` markers) for name, description, author, version. `Skill.source_path: Option<String>` records where the skill was loaded from. `skills-lock.json` tracks skill dependencies with content hashes.

**Skill-install link decision** goes through `classify_agent` / `agent_link_need` (`crates/core/src/skills/linker/classify.rs`): a NativeReader (reads `.agents/skills` directly) gets the Master only — **no** per-agent link. Both install paths must use it — the CLI add path (`manager::skill::add_skill_universal` / `add_skill_from_path_universal`) and the fetched/desktop path (`install_universal`); keep them consistent (they diverged once when the CLI used a narrower `agent_write_dir == canonical_dir` check).

## Adding / Removing an Agent

Touch ALL of these (descriptors live in `crates/agents`, the registry in `crates/core`):

1. `crates/agents/src/agents/<name>.rs` — create/delete the descriptor constant (`codex` is a subdirectory, not a single `.rs` file; note `factory.rs` is the Factory-AI agent's descriptor, NOT a dispatch factory)
2. `crates/agents/src/agents/mod.rs` — add/remove `pub mod <name>;`
3. `crates/agents/src/models.rs` — add/remove the `AgentType` variant + `ALL` array entry + `as_str()` arm + `from_str()` arm
4. `crates/core/src/registry/mod.rs` — add/remove `&agents::<name>::DESCRIPTOR` from `ALL_AGENTS` (the cross-crate step that's easy to miss; there is no dispatch match anywhere — dispatch is a find-by-id over this array)

## Testing

Integration tests in `crates/core/tests/integration_tests.rs` use a `TestConfig` helper that builds isolated temp dirs with `.claude/`/`.opencode/` structures. For test isolation, `TestConfig` uses `crate::adapter::set_skills_path_override(agent_id, path)` (per-agent thread-local). Other suites: `crates/core/tests/mcp_tests.rs` (MCP transports, dedup), `crates/core/tests/test_agent_paths.rs` (XDG skills paths per agent), `crates/cli/tests/cli_tests.rs` (end-to-end CLI via `assert_cmd`).

## Conventions

**Rust**: hard tabs (width 4, NOT spaces); 80-char max line width; `rustfmt`; `cargo clippy -- -D warnings` (warnings = errors).
**TypeScript/frontend**: `bun` only; React 19 + HeroUI v3; Tailwind CSS v4; strict TS.
**Code organization**: one agent = one file in `crates/agents/src/agents/<name>.rs`; descriptors define config paths, file format, capabilities; no hand-wired adapters.

## Anti-Patterns

- NEVER use spaces for Rust indentation (hard tabs enforced); NEVER exceed 80 cols; NEVER ignore clippy warnings (build treats as errors).
- NEVER add an agent without wiring all 4 steps above.
- NEVER expose raw filesystem paths in API responses; NEVER bypass `ConfigManager` (always use the adapter pattern).

## Release & Packaging

- **Tag-driven**: pushing a `v*` tag runs `.github/workflows/release.yml` → a 3-platform `test` gate (ubuntu/macOS/Windows `just test`) → desktop bundles (macOS/Windows/Linux via `tauri-action`) + CLI, generates `latest.json`, updates the Homebrew tap. No manual build/upload. See the `releasing-aghub` skill.
- **Test-gated + serialized**: no artifact is built or published unless the tagged commit passes tests on **all 3 platforms**; a per-tag concurrency group (`cancel-in-progress: false`) prevents overlapping/half-published runs. Tag only CI-green commits — run `just preflight` locally first (the pre-push hook does **NOT** run tests). The build only compiles, so a platform-specific bug that passes on Linux but fails elsewhere is caught by the gate, not shipped.
- **Version comes from the git tag** — CI `sed`s it into `Cargo.toml`, `crates/desktop/package.json`, `tauri.conf.json`. Don't hand-bump for a release; `just bump <ver>` only syncs those three manifests locally.
- **Tauri updater**: the committed `tauri.conf.json` `pubkey` must pair with the `TAURI_SIGNING_PRIVATE_KEY` secret, and `endpoints` must point at _this_ repo's releases. The pubkey must never change once a build ships, or installed apps can't auto-update.
- **Gotcha**: unset `APPLE_*` secrets resolve to empty strings and break the macOS build (`security import` on an empty cert). Keep them commented out in `release.yml` until real Apple certs exist — unsigned dmg builds fine otherwise.
- The Homebrew tap is a **separate repo** written via the `HOMEBREW_TAP_TOKEN` PAT (the default `GITHUB_TOKEN` can't reach it).
- `git push` is gated by a **pre-push hook**: `cargo fmt --all --check` + prettier `--check` + clippy `-D warnings` + eslint + tsc — but NOT tests, and NOT Windows-target clippy (unix-gated test helpers/imports must carry the same `#[cfg]` as their callers or Windows CI clippy goes red on merge).

## Configuration Paths Reference

| Agent    | Global Config                      | Project Config            | Skills Path                                           |
| -------- | ---------------------------------- | ------------------------- | ----------------------------------------------------- |
| Claude   | `~/.claude.json`                   | `.mcp.json`               | `~/.claude/skills/`                                   |
| OpenCode | `~/.config/opencode/opencode.json` | `.opencode/settings.json` | `~/.config/opencode/skills` + `.agents/skills` Master |

Project root is detected by walking up looking for agent markers (`.claude/`, `.opencode/`, `.cursor/`, `.mcp.json`, …). `.git` alone is NOT sufficient — the directory must also contain at least one agent marker.
