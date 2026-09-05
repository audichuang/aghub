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

Also at the repo root: `.agents/skills/` (this repo's own hand-edited skills —
a legacy real-directory layout that migration deliberately leaves alone, D7) and `justfile` (task runner).

Cargo graph (depends-on): `agents` ← `core` ← `{cli, api}`; `desktop` → `api`
(+ `remote`), not core directly. Tool crates used laterally.
`skills-ref` is an external git dep (`AkaraChen/skills-ref`).

## Where to Look

Only where the obvious guess is wrong; everything else, ask CodeGraph. The one
that catches everybody: the `AgentAdapter` **trait** is in
`crates/core/src/adapters/mod.rs`, but the impl is in `core/src/adapter.rs`.

**Three different "app data roots" exist.** `aghub-cli`'s `app_data_dir()` is
`$AGHUB_DATA_DIR` else `dirs::data_dir()/aghub`; `aghub_api::default_app_data_dir()`
is the SAME formula **minus the env override** — it never reads `$AGHUB_DATA_DIR`,
so pinning that var isolates the CLI (and the desktop's own hand-rolled copy in
`skill_check.rs`) while a standalone `aghub-api` keeps writing the real root. The
parity test says so itself: it removes the var before asserting equality. The
desktop starts its embedded api with **Tauri's**
`app_data_dir()`, which is identifier-scoped (`<data>/com.akrc.aghub`). Any file
one surface WRITES and another READS must use the CLI formula — Tauri's, or a
hand-rolled `$XDG_DATA_HOME` guess, agrees on Linux and diverges on
macOS/Windows, so the mismatch never shows up locally.

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

- **Master store vs Referrer (`.aghub` vs `.agents/skills`)**: the ONE physical
  copy lives in `.aghub/<sanitized-name>` (`~/.aghub` global,
  `<root>/.aghub` project), a directory **no agent reads** — storing a skill
  must not grant it. Every grant is a symlink Referrer in an agent's own skills
  dir. `.agents/skills` is now an ordinary Referrer slot, except that it is
  **shared**: ten agent/scope combinations read it and eight of those have no
  private dir, so granting to one grants to all of them. `classify` computes
  that sharing once and carries it as `shared_with`; never re-derive it per
  consumer. Read the descriptor, never a list — per-agent AND per-scope.
  `capabilities.skills.universal: true` ALSO appends XDG
  `$XDG_CONFIG_HOME/agents/skills` (default `~/.config/agents/skills`) — a
  SECOND shared slot (amp + kimi at global), and **not** `~/.agents/skills`.
- **`registry::get()` fallback**: unknown id → Claude's descriptor silently.

## Commands

`just --list` is the catalog. What it doesn't tell you:

- `just preflight` = fmt + clippy + **desktop typecheck** + workspace tests +
  doc tests. It is the release gate; its `just --list` blurb is truncated
- **preflight does NOT run prettier or eslint** — the pre-push hook does, and it
  runs `bun run format:check` from the REPO ROOT (`prettier --check .`), so it
  covers `scripts/` and `docs/`. A green preflight is not a pushable tree;
  `crates/desktop`'s own `format:check` never sees root files
- `just featured-check` (bundled skills-sh catalog still installable) needs the
  network and a `gh` login, so it is deliberately outside preflight. Run it
  after editing `crates/desktop/src/data/featured-skills.json` — the catalog
  points at other people's repos and rots on their schedule
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
  (`apply-update` refuses outright instead of printing a preview; it also
  rejects `--all` — core never supported it, and the refusal now arrives from
  the scope table, i.e. BEFORE the `--yes` one)
- **Scope flags are mutually exclusive**, enforced MANUALLY in `main()` before
  every dispatch (a clap `ArgGroup` does not propagate to `global = true` args —
  so this is exit **1**, not clap's exit 2). A generic mutation must resolve
  exactly ONE write scope: `--all` is rejected, and **`-p` with no project root
  bails BEFORE anything happens — for reads too**, so `get`/`check`/`describe`
  cannot answer `[]` from a non-project directory. ONE exception:
  `transfer`/`reconcile` let a rootless `-p` reach core, so their `--json`
  failure keeps `code: RESOURCE_NOT_FOUND` instead of the bail's `CLI_ERROR`
- **`doctor`'s `health` covers lock ↔ Master only** — per-agent referrer state
  needs `--verify-links`. `linkAudit.state` is `verified` ONLY when every agent
  row is healthy (`issues` otherwise); `orphanMaster` is a leftover master with
  no lock entry and no slot — do NOT offer `source sync --install-missing` for
  it, there is no source, and `delete --yes` produces exactly that state when it
  keeps a master another agent still reads. Exit code is unchanged by default;
  `--fail-on-issues` opts into a non-zero exit
- **`check` is offline by default** — `checked: false`, and the reason is the
  ORCHESTRATOR's, not the surface's: a source nothing could fetch keeps its
  permanent reason (`local` / `ssh` / `unsupportedScheme`) and everything else
  reports `network`, meaning "we did not look". `network` is reserved for rows an
  `--online` run really would answer, so `--online` is only ever suggested for
  those. Offline hashes NO skill folder on either surface. Pass `--online` for a
  real update check. Its scope defaults to BOTH, like
  `doctor`/`source list`/`source diff` (it followed the global default and
  answered "this project is up to date" without reading the project lock)
- **`check` never writes, and `--write-result` is why that needs a guard.** The
  sidecar path is arbitrary, so the write is refused by **file NAME**
  (`.skill-lock.json`, `skills-lock.json`, `.aghub-mutation.lock`), by an
  `.aghub` path SEGMENT, and by `.agents/skills` as adjacent SEGMENTS, before any
  resolved-path comparison. Do not "simplify" it back to comparing resolved
  paths: three review rounds each found a new spelling that slipped through,
  because the scope never resolved the target (`-g` resolves no project root at
  all, so the project lock one `../` away was invisible). Normalize with
  `skill::lock::resolve_existing`
- **`source diff` ALWAYS fetches** (no offline mode); `--online` is accepted as a
  no-op alias so the `check` habit does not become a clap error. It judges each
  read scope against the origin THAT scope's lock records, so a host-blind
  `owner/repo` spanning two forges is reported per scope instead of refused —
  every JSON scope view carries `origin`, and the human table gets a stderr note
  when the origins disagree. Ambiguity within ONE scope is still a refusal.
  `GET /skills/sources/diff?scope=all` deliberately answers DIFFERENTLY: it
  judges the union of both locks and refuses (`SOURCE_AMBIGUOUS`), because its
  response is one flat merged list with a single `source` field and has nowhere
  to attribute a forge per scope — a consequence of the merged-vs-per-scope
  shapes, not a second ambiguity rule. Both sides are pinned by tests
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
  `reconcile` needs at least one `--add`/`--remove`; `-a/--agent` is ignored.
  An already-present target is an **idempotent success** (`already_present:
true`) for both verbs — a skill because the shared Master is what "already
  there" means, an MCP/sub-agent only when the existing value is EQUIVALENT (a
  same-named entry holding a different command is still a hard conflict).
  **Removing a skill refuses an end state that cannot exist** — if the agent
  reads the skill from exactly the same set of places afterwards, the removal
  took nothing away (a private copy shadowing a Master DOES take something
  away, and stays legal — the Master it falls back to is disclosed in
  `skipped`). That verdict has ONE home
  (`removal::read_effect_after`, asked of discovery, never of a
  `dir.join(name)` guess), so `delete`, the API delete route and
  `reconcile skill` cannot answer it differently — they used to, and `delete`
  was the one reporting `removed` for a skill still on disk. `reconcile skill`
  additionally refuses BEFORE the first write, so the whole batch errors and
  the disk is untouched (no half-applied copy-then-failed-delete), counting the
  Master its OWN copies would create — "add windsurf, remove cursor" cannot
  hand cursor the skill back through a Master the same command just made. Its
  **preview runs that same check**: a `--remove` without `--yes` that exits 0
  is a commit that will run. Both spellings report
  `UNSUPPORTED_OPERATION` / HTTP 422 — batching is transport and must not
  relabel the refusal as bad parameters. **`reconcile mcp`'s guard is narrower
  than it looks**: `ensure_removals_spare` DOES compare resolved backing paths
  (preflight, then re-checked at delete time), but its protect set is only the
  COPY TARGETS + the source. An agent that shares the backing file and is named
  NOWHERE in the command is in neither list, so `reconcile mcp --remove claude`
  still takes the server from copilot too when both resolve project MCPs to
  `<root>/.mcp.json`, reported as success — verified, unfixed. `reconcile skill`
  builds its protect set from the SAME `protected_targets`, so do not assume the
  skill side is closed against an unnamed dir-sharer without testing it; what IS
  closed there is the shared-Master case, by the keep rules
- **`inference`**: provider inventory + keyring keys. Bindings/routing are
  desktop/API-only — there is no `inference bind` on the CLI. `--api-key -`
  reads the key from stdin; nothing else does
- **`--json` failures are JSON too**: `{"error":{code,message,retryable}}` on
  stdout, exit 1. `code` is `aghub_core::error_codes` — the SAME vocabulary the
  HTTP API sends. clap usage errors stay exit 2 with prose
- **`delete`'s JSON carries `outcome`**: `preview` | `removed` | `absent` |
  `partial` | `kept` (shared Master another agent still reads — `success: true`
  but THE ENTITY IS STILL THERE; the API adds an api-only `failed` for early
  errors). A preview also carries `would_prune_lock_entries`: the lock keys the
  commit would drop, separate from the committed `pruned_lock_entries` because a
  preview must not claim entries were dropped. Read that, not `dry_run`/`executed`: those two cannot separate a
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
(`crates/core/src/skills/linker/classify.rs`). Every supported agent takes a
Referrer — there is no "reads the Master directly" case any more, and the
`LinkNeed::NativeReader` variant was DELETED rather than left unreachable
(a variant still constructed but never produced draws no dead-code warning and
silently kills every `matches!` arm testing for it). Both install paths must use
it — CLI `add_skill_universal` / `add_skill_from_path_universal` and fetched
`install_universal`.

**Shape classification**: `skills::shape` — `classify_shape` (one
`(referrer, master)` pair), `candidate_referrers` (each agent's Referrer PATH,
derived from its write dir, never from what is on disk) and `plan_repair`.
Three traps with their own tests: `symlink_metadata` alone cannot decide
conformance; two canonicalize `Err`s must never compare equal; and identity
(same inode through a symlinked parent) comes before any content comparison.

## Adding / Removing an Agent

One agent = one file. Miss a step and the `registry::get()` fallback above takes
over silently — no compile error, no runtime error, just Claude's behavior.

1. `crates/agents/src/agents/<name>.rs` — descriptor (naming gotchas:
   `crates/agents/AGENTS.md`)
2. `crates/agents/src/agents/mod.rs` — `pub mod` **and an `ALL_DESCRIPTORS`
   entry**. That const is the ONE roster; `core`'s `registry::ALL_AGENTS`
   points at it (find-by-id; no match arm)
3. `crates/agents/src/models.rs` — `AgentType` + `ALL` + `as_str`/`from_str`
4. `crates/core/tests/mcp_dialect_golden.rs` — a `row!` naming what the agent
   writes and how it reads a config aghub did not write. There is no way to
   skip this: the row is REQUIRED for any agent claiming MCP support
5. `crates/core/tests/mcp_dialect_decisions.rs` — a second `row!`, same
   registry-driven requirement, naming what it does with a mixed entry, an
   unknown transport tag, a field the model does not own, a value that does not
   fit, and an SSE server it cannot spell. Also required for a `json_map` agent
   that introduces no new dialect at all

Steps 2 and 3 are asserted bijective in `crates/core/tests/registry_bijection.rs`,
so a missed roster entry fails loudly instead of silently serving Claude.

**Opening a capability on an EXISTING agent has a hidden blast radius**: tests
across `cli`, `core` and `api` pick some agent that does not support skills as
their "unsupported target" sentinel, and assert that a mutation is refused. Give
that agent the capability and those tests stop testing anything — they go red if
you are lucky, and silently pass a real write if you are not. Grep the agent's
id across `crates/*/tests/` and `crates/api/src/routes/` BEFORE changing its
capabilities, and move the sentinel rather than deleting the assertion.

## Testing

**Do not pollute real home**: clearing `set_skills_path_override` under
**global** scope still writes the master to `dirs::home_dir()/.aghub`, and
Referrers into the agents' own dirs. Overriding `$HOME` alone is NOT enough —
`dirs::config_dir()` prefers `$XDG_CONFIG_HOME` and several descriptors honour
their own variable ahead of both, so a developer's real `~/.config` leaks in
(observed: a live `~/.config/orca/...` in a test's allow-listed roots).
Isolate `$HOME` (Unix) or use a project `tempdir` + teardown — mechanics and the
env-lock rule in `crates/core/AGENTS.md` Testing.

**A test must be able to FAIL on a real regression** — a green test that can't
is worse than none (it reads as "covered"). Assert observable OUTCOMES (values,
on-disk / lock state), not a variant or `is_err()`; for a safety-critical flow
exercise the FAILURE path (rollback AFTER the destructive step, not just the
happy path). PROVE it: revert the fix, watch the assertion go red, restore —
reasoning that it _would_ fail is how false greens survive. **A malformed
fixture is the sneakiest false green here**: the lock read paths fail CLOSED for
the commands that report lock contents (`check`, `doctor`, `source list`/`diff`),
so a lock fixture missing a required field makes the command bail while READING
and the assertion passes with the code under test never reached. Copy a fixture
shape from an existing test rather than hand-writing a minimal one. Worked example:
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
- NEVER hand-roll path normalization to compare two paths. Use
  `skill::lock::resolve_existing` (the one the mutation lock uses): it resolves
  the longest existing prefix so the FILESYSTEM answers `..` after a symlink,
  then treats only the unresolvable tail lexically. A `parent()`/`file_name()`
  walk is the trap — `file_name()` is `None` for a path ending in `..`, so the
  walk abandons and returns the path unnormalized
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
