# AGHUB KNOWLEDGE BASE

**Project**: aghub — AI coding agent configuration management tool\
**Stack**: Rust workspace (root `Cargo.toml`) + Tauri v2 desktop + React 19/TypeScript\
**Package manager**: cargo (Rust), **bun** (desktop frontend — never npm/yarn/pnpm)

> Every `CLAUDE.md` here is a one-line `@AGENTS.md` import (a real file, not a
> symlink) — edit the sibling `AGENTS.md`.
>
> This is the **navigation layer**: what each crate is for and the invariants you
> must not break. Deliberately coarse — for structure, ask CodeGraph
> (`.codegraph/` is indexed).

## Overview

Aghub manages AI coding agent configurations across many agents (see
`AgentType::ALL` in `crates/agents/src/models.rs` — NOT `crates/core`), handling
MCP servers, skills, and sub-agents through a unified interface. It also manages
inference providers, Claude Code plugins, and SSH-based remote deployment.
Stateless design — it reads the actual config files, tracks capability sources,
and requires explicit opt-in for changes.

## Maps & Decisions (read these first)

- **Design specs**: `docs/specs/` — the current design corpus; rationale, not
  current-state truth (code wins)
- **`docs/plans/` + `docs/superpowers/`**: historical checkbox plans from the
  retired superpowers workflow (dead since 2026-07-14). A same-named file is
  that spec's _plan_, not a rival copy — except `docs/superpowers/specs/`, which
  holds the ONLY design docs for remote-SSH and the api origin guard
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
  cli/           # `aghub-cli` — the clap command surface
  api/           # `aghub-api` — Rocket v0.5 under /api/v1/ (mounted set is
                 #   lib.rs + routes/; never hardcode a route count)
  desktop/       # Tauri v2 + React, embeds aghub-api on localhost;
                 #   src-tauri package name is `aghub` (−p aghub ≠ CLI)
  skill/         # .skill zip + npx-compatible locks + hashing
  skill-update/  # shared update-check + Sources domain + source-mutation seam (API + CLI)
  skills-sh/     # skills.sh registry client (search only)
  inference/     # providers: SQLite meta + keyring
  remote/        # SSH remote VM (desktop only, not API)
  cc-plugins/    # Claude Code plugin lifecycle
  git/           # clone/fetch + credential injection
  json/          # JSON/JSONC editing
  markdown/      # YAML frontmatter helpers
```

Also at the repo root: `.agents/skills/` (universal skill Master, when aghub is
used as a project) and `justfile` (task runner).

Cargo graph (depends-on): `agents` ← `core` ← `{cli, api}`; `desktop` → `api`
(+ `remote`), not core directly. Tool crates used laterally.
`skills-ref` is an external git dep (`AkaraChen/skills-ref`).

## Where to Look

Only where the obvious guess is wrong; everything else, ask CodeGraph. The one
that catches everybody: the `AgentAdapter` **trait** is in
`crates/core/src/adapters/mod.rs`, but the impl is in `core/src/adapter.rs`.

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
core) and owns that agent's config paths — there is no path table to maintain
anywhere else. The MCP **parse/serialize** logic it points at lives in
`crates/agents/src/format/`. Change either as the case needs.

Per-agent dialect gotchas (which key holds MCPs, which transports survive a
round-trip, what a rewrite preserves) live with the descriptors:
**`crates/agents/AGENTS.md`**. The two rules that span crates stay here:

- **Universal Master (`.agents/skills`)**: an agent reads the Master **only**
  where its own descriptor maps that scope's skill paths there — per-agent AND
  per-scope, so read the descriptor rather than trusting any list. Invariant:
  an agent reads its own dir + a mapped Master, never another agent's private
  dir. `capabilities.skills.universal: true` ALSO appends XDG
  `$XDG_CONFIG_HOME/agents/skills` (default `~/.config/agents/skills`) — which is
  **not** `~/.agents/skills`.
- **`registry::get()` fallback**: unknown id → Claude's descriptor silently.

## Commands

`just --list` is the catalog. What it doesn't tell you:

- `just preflight` = fmt + clippy + **desktop typecheck** + workspace tests +
  doc tests. It is the release gate; its `just --list` blurb is truncated
- Prefer file-scoped over the full suite: `cargo test -p aghub-core <name> -- --exact`
- Desktop frontend commands run from `crates/desktop` via `bun run …`

## CLI Command Surface

Authoritative: clap (`just start -- --help`, `crates/cli/src/commands/`).
Aliases: `skills`/`skill`, `mcps`/`mcp`. Scope: `-a` (one id, a comma-separated
list, or `all` — one `AgentSelection` parser; multi-target runs emit a batch
envelope, policy in `core/src/batch.rs`), `-g`/`-p`, `--all`.

Non-obvious invariants:

- **Destructive defaults**: `delete`, `apply-update`, `prune-lock`, `source sync`,
  `source accept-rename`, reconcile-with-removals → **dry-run unless `--yes`**
  (`apply-update` refuses outright instead of printing a preview)
- **Scope flags are mutually exclusive**, enforced MANUALLY in `main()` before
  every dispatch (a clap `ArgGroup` does not propagate to `global = true` args —
  so this is exit **1**, not clap's exit 2). A generic mutation must resolve
  exactly ONE write scope: `--all` is rejected, and **`-p` with no project root
  bails BEFORE anything happens — for reads too**, so `get`/`check`/`describe`
  cannot answer `[]` from a non-project directory
- **`doctor`'s `health` covers lock ↔ Master only** — per-agent referrer state
  needs `--verify-links`
- **`check` is offline by default** — remote sources report `uncheckable/network`
  with `checked: false`; pass `--online` for a real update check. Its scope
  follows the GLOBAL default, unlike `doctor`/`source list`/`source diff`, which
  span both
- **`source diff` ALWAYS fetches** (no offline mode); `--online` is accepted as a
  no-op alias so the `check` habit does not become a clap error
- Skill install is **always symlink-only**; `--universal` is a hidden no-op
- Source creds: `GIT_PASSWORD` (any host) / `GITHUB_TOKEN` (github.com https-only)
- **`skill-usage`**: Claude-global only; rejects project/`--all`
- **`coverage`**: rejects `--all`; scope `-g` or `-p` only. It is a static agent
  CAPABILITY matrix — no skill names, no counts; use `doctor --verify-links` for
  per-skill link state
- **Narrowed resource args**: `check`/`apply-update` take skills ONLY and
  `enable`/`disable` take mcps ONLY — enforced by their own clap value_enums, so
  the rejection is a parse error naming the valid values. (`enable`/`disable
skills` was dead for all 25 agents; core still refuses it for the API path.)
- **`transfer`** / **`reconcile`**: cross-agent copy / reconcile of
  skills·mcps·sub-agents (reconcile-with-removals is dry-run — see above).
  `reconcile` needs at least one `--add`/`--remove`; `-a/--agent` is ignored
- **`inference`**: provider inventory + keyring keys. Bindings/routing are
  desktop/API-only — there is no `inference bind` on the CLI. `--api-key -`
  reads the key from stdin; nothing else does
- **`--json` failures are JSON too**: `{"error":{code,message,retryable}}` on
  stdout, exit 1. `code` is `aghub_core::error_codes` — the SAME vocabulary the
  HTTP API sends. clap usage errors stay exit 2 with prose
- **`delete`'s JSON carries `outcome`**: `preview` | `removed` | `absent` |
  `partial`. Read that, not `dry_run`/`executed`: those two cannot separate a
  refused preview from an already-gone resource, and `executed: true` is set for
  the whole execute branch even when every single delete failed (`partial`).
  `absent` outranks the caller's intent — an unconfirmed delete of something
  that does not exist is not a preview of any change
- **An unreadable lock fails the commands that report it** (`check`, `doctor`,
  `source list`/`diff`). The lock read paths fail OPEN by design; those three
  present lock contents AS their answer, so they probe first

## Skills Discovery

**Mutation lock**: every mutating skill flow holds ONE interprocess lock across
its whole check→write→rollback span; read paths are deliberately unlocked, and it
serializes aghub against aghub only. Invariants and the call-site rule:
`crates/core/AGENTS.md` "Mutation attribution".

**Link decision**: `classify_agent` / `agent_link_need`
(`crates/core/src/skills/linker/classify.rs`). NativeReader → Master only, **no**
per-agent link. Both install paths must use it — CLI
`add_skill_universal` / `add_skill_from_path_universal` and fetched
`install_universal`.

## Adding / Removing an Agent

One agent = one file. Miss a step and the `registry::get()` fallback above takes
over silently — no compile error, no runtime error, just Claude's behavior.

1. `crates/agents/src/agents/<name>.rs` — descriptor (naming gotchas:
   `crates/agents/AGENTS.md`)
2. `crates/agents/src/agents/mod.rs` — `pub mod`
3. `crates/agents/src/models.rs` — `AgentType` + `ALL` + `as_str`/`from_str`
4. `crates/core/src/registry/mod.rs` — `ALL_AGENTS` entry (find-by-id; no match)

## Testing

**Do not pollute real home**: clearing `set_skills_path_override` under
**global** scope still writes the master to `dirs::home_dir()/.agents/skills`.
Isolate `$HOME` (Unix) or use a project `tempdir` + teardown — mechanics and the
env-lock rule in `crates/core/AGENTS.md` Testing.

**A test must be able to FAIL on a real regression** — a green test that can't
is worse than none (it reads as "covered"). Assert observable OUTCOMES (values,
on-disk / lock state), not a variant or `is_err()`; for a safety-critical flow
exercise the FAILURE path (rollback AFTER the destructive step, not just the
happy path). PROVE it: revert the fix, watch the assertion go red, restore —
reasoning that it _would_ fail is how false greens survive. Worked example:
`docs/specs/2026-07-15-skill-rename-transaction-deepening.md`.

## Agent permissions / approval boundaries

| AI may do autonomously                            | Ask first                                         |
| ------------------------------------------------- | ------------------------------------------------- |
| Edit code; `just fmt` / `just lint`; scoped tests | `git push`, force-push, amend published history   |
| Read-only under temp dirs                         | Release tags, `just bump`, Homebrew tap           |
|                                                   | Real `~/.agents`, keyring, `tauri` updater pubkey |
|                                                   | New workspace deps without a clear need           |

Never commit secrets.

## Anti-Patterns

> Formatting and lint are not listed here — `rustfmt.toml` and CI
> (`cargo fmt --check`, `clippy -D warnings`) enforce them deterministically.

- NEVER bypass `ConfigManager`
- NEVER return arbitrary internal temp/lock/keyring paths in API **errors**;
  skill DTOs may expose intentional `source_path` / `canonical_path` for UI
- NEVER hand-mirror a mutating/transactional flow across surfaces (CLI ↔ API, or
  per-dialect parsers) and "keep it in sync by hand" — it WILL drift (worked
  example: the multi-agent batch policy, extracted to `core/src/batch.rs`).
  Extract the invariant to `core` / a shared policy behind ONE tested interface;
  surfaces stay thin adapters.
- When promoting a **private** flow to a **public** seam, re-assert the
  preconditions the old callers used to guarantee (e.g. `accept_rename`
  re-checks the lock itself) — a public entry point is only as safe as its own
  guards

## Release & Packaging

Full runbook (versioning, `just bump`, signing secrets, Homebrew tap, workflow
failures): project skill **`releasing-aghub`** + `.github/workflows/release.yml`.
Two things the skill won't stop you from getting wrong:

- Tag `v*` only after green CI, and run `just preflight` first — **the pre-push
  hook does NOT run tests**
- **Never change** shipped `tauri.conf.json` updater `pubkey`, or point
  `endpoints` elsewhere — it bricks auto-update for installed users

## Project Root Detection

Walk up for agent markers (`.claude/`, `.opencode/`, `.cursor/`, `.mcp.json`,
`skills-lock.json`, …) — `core/src/paths.rs`. **`.git` alone is not enough.**

## Agent workflows

- **Issues**: local markdown at `.scratch/<feature>/issues/<NN>-<slug>.md` (spec
  at `.scratch/<feature>/spec.md`), triage in a `Status:` line —
  `docs/agents/issue-tracker.md`
- **Domain docs**: single-context, one root `CONTEXT.md` + `docs/adr/` —
  `docs/agents/domain.md`
