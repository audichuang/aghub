# AGHUB KNOWLEDGE BASE

**Project**: aghub — AI coding agent configuration management tool\
**Stack**: Rust workspace (root `Cargo.toml`) + Tauri v2 desktop + React 19/TypeScript\
**Package manager**: cargo (Rust), **bun** (desktop frontend — never npm/yarn/pnpm)

> Every `CLAUDE.md` here is a one-line `@AGENTS.md` import (a real file, not a
> symlink) — edit the sibling `AGENTS.md`.
>
> This is the **navigation layer**: what each crate is for, the rules that span
> crates, and the approval boundaries. A rule that belongs to ONE crate lives in
> that crate's `AGENTS.md`. The WHY behind a flow — what it used to do, what was
> deliberately not done, which test pins it — lives in the Hindsight knowledge
> pages (their list is injected every session; read the page before changing the
> flow) and in `docs/`. For structure, ask CodeGraph (`.codegraph/` is indexed).
>
> Priority when they disagree: the user's current instruction > this file > a
> per-crate `AGENTS.md` > a skill. If something here blocks you, say which line.

## Overview

Aghub manages AI coding agent configurations across many agents (see
`AgentType::ALL` in `crates/agents/src/agents/mod.rs` — NOT `crates/core`), handling
MCP servers, skills, and sub-agents through a unified interface. It also manages
inference providers, Claude Code plugins, and SSH-based remote deployment.
Stateless design — it reads the actual config files, tracks capability sources,
and requires explicit opt-in for changes.

## Maps & Decisions

- **Design specs**: `docs/specs/` — rationale, not current-state truth (code wins)
- **`docs/plans/` + `docs/superpowers/`**: historical checkbox plans from a
  retired workflow. A same-named file is that spec's _plan_, not a rival copy —
  except `docs/superpowers/specs/`, which holds the only design docs for the
  remote-SSH **bring-up/VM-management** flow and the api origin guard (other
  `remote-*` designs are in `docs/specs/`)
- **Domain language**: [`CONTEXT.md`](CONTEXT.md) (Source hash, Master, Referrer, Relink, …)
- **Load-bearing decisions**: [`docs/adr/`](docs/adr/)
- **Incident history**: [`docs/history/`](docs/history/) — the story behind a
  rule in the code (what it used to do, the finding, the pinning test). A code
  comment states the rule and links there; never write the narrative inline
- **Fork upstream sync log**: [`UPSTREAM.md`](UPSTREAM.md) — port / skip from `AkaraChen/aghub`
- **Deep domain playbooks**: project skills under `.agents/skills/`, mirrored as
  symlinks in `.claude/skills/`. They trigger on their own `description` — never
  keep a catalog of them here. Exception: when a session's skill list overflows
  its budget, a skill that has never been used loses its description and cannot
  trigger, so the per-crate `AGENTS.md` names such a skill where its task happens

## Structure

Module map (crate → why it exists). Authoritative member list: `Cargo.toml`.

```
crates/
  agents/        # SSOT for agent behavior: descriptors, AgentType, models, format/
  core/          # orchestration: ConfigManager, registry, skills, transfer
  cli/           # `aghub-cli` — the clap command surface + its semantics
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

Also at the repo root: `.agents/skills/` (this repo's own hand-edited skills, a
real-directory layout lazy migration leaves alone — it moves only the one skill
it mutates) and `justfile` (task runner).

**`skills/aghub-cli/` is the skill users install, and it is part of the CLI's
contract.** It is not dev tooling — that is `.agents/skills/`; keep the two apart.
A change to CLI semantics (flags, `-a` parsing, `source sync`, `repair`, `doctor`,
lock behaviour) edits `skills/aghub-cli/` in the SAME commit, or the skill
teaches the old behaviour. It reaches a machine only after the commit is on
`fork/main` and `aghub-cli source sync audichuang/aghub --update` runs; editing
the installed copy under `~/.aghub` is overwritten by that sync.

Cargo graph (depends-on): `agents` ← `core` ← `{cli, api}`; `desktop` → `api`
(+ `remote`), not core directly. Tool crates used laterally.
`skills-ref` is an external git dep (`AkaraChen/skills-ref`).

## Where to Look

Only where the obvious guess is wrong; everything else, ask CodeGraph. The one
that catches everybody: the `AgentAdapter` **trait** is in
`crates/core/src/adapters/mod.rs`, but the impl is in `core/src/adapter.rs`.

**ONE app data root: `aghub_core::paths::app_data_dir()`** — `$AGHUB_DATA_DIR`
else `dirs::data_dir()/aghub`. `aghub-cli`'s `commands::app_data_dir()` and
`aghub_api::default_app_data_dir()` are one-line delegations to it (the desktop
reaches it through the api re-export, having no `aghub-core` dep), so pinning
that env var isolates every surface at once. **Never re-spell the formula** — a
hand-rolled `$XDG_DATA_HOME` guess, or Tauri's identifier-scoped
`app_data_dir()` (`<data>/com.akrc.aghub`), agrees on Linux and diverges on
macOS/Windows, so the split never shows up locally. Parity is pinned by
`aghub_api::tests::default_app_data_dir_matches_core_seam` and the CLI's
`commands::tests::app_data_dir_matches_core_seam`. Why: knowledge page `推論供應商與 app data root`.

## Key Design Patterns

- **Adapter pattern**: `create_adapter(agent_type)` → `registry::get` →
  `&'static AgentDescriptor` implements `AgentAdapter`. **No hand-wired adapter
  structs** — behavior is function pointers on each descriptor.

## Agent-Specific Behavior

Each agent's **descriptor** lives in `crates/agents/src/agents/<name>.rs` (not
core) and owns that agent's config paths — there is no path table to maintain
anywhere else. The MCP **parse/serialize** logic it points at lives in
`crates/agents/src/format/`. Per-agent dialect gotchas: `crates/agents/AGENTS.md`.

The two rules that span crates stay here:

- **Master store vs Referrer (`.aghub` vs `.agents/skills`)**: the ONE physical
  copy lives in `.aghub/<sanitized-name>` (`~/.aghub` global, `<root>/.aghub`
  project), a directory **no agent reads** — storing a skill must not grant it.
  Every grant is a symlink Referrer in an agent's own skills dir.
  `.agents/skills` is an ordinary Referrer slot that is **shared**: granting
  there reaches every agent/scope that reads it. Prefer supported private write
  slots; shared read compatibility does not imply a shared write slot. Slot
  membership is per-agent AND per-scope — read the descriptor, never a list
  (`crates/agents/tests/descriptor_regression.rs`
  `test_global_skill_paths` / `test_project_skill_paths`). `classify` computes
  the sharing once and carries it as `shared_with`; never re-derive it per
  consumer. Full layout + shape/repair chain: knowledge page
  `技能連結形狀與 repair 鏈`.
- **`registry::get()` has NO fallback.** It is `agent_type.descriptor()`, a
  total `match` generated by `agent_roster!`, so an unknown id cannot
  reach any descriptor. The remaining failure is a mistyped roster ROW — only
  the variant is compiler-checked, the id literal and the module path are free
  text — and `crates/core/tests/registry_bijection.rs` catches all three
  spellings of that.

## Commands

`just --list` is the catalog. What it doesn't tell you:

- `just preflight` = the three `scripts/check-*` guards (history links, test tmpdir, string tabs) + fmt + clippy + `bun install --frozen-lockfile` (root AND
  desktop) + desktop typecheck + **desktop frontend unit tests** + workspace
  tests + doc tests. It is the release gate; its `just --list` blurb is truncated
- **preflight does NOT run oxfmt or oxlint** — the pre-push hook does, and it
  runs `bun run format:check` from the REPO ROOT (`oxfmt --check .`), so it
  covers `scripts/` and `docs/`. `crates/desktop`'s own `format:check` never
  sees root files
- `just featured-check` (bundled skills-sh catalog still installable) needs the
  network and a `gh` login, so it is deliberately outside preflight. Run it
  after editing `crates/desktop/src/data/featured-skills.json` — the catalog
  points at other people's repos and rots on their schedule
- `just test` and preflight run **cargo-nextest** (one process per test), not
  `cargo test`; doc tests stay `cargo test --workspace --doc`. Plain
  `cargo test` must still pass, so the env-mutex rule still binds. Why:
  [`docs/history/tooling.md`](docs/history/tooling.md#cargo-test-serialized-on-the-env-mutex)
- Prefer file-scoped over the full suite:
  `cargo test -p aghub-core <full::module::path::name> -- --exact`
- Desktop frontend commands run from `crates/desktop` via `bun run …`

## Definition of done

Done is a green gate, not a first implementation that compiles. Pick the gate by
blast radius, run it yourself, and do not come back for review between
implementing and verifying.

- **Scoped change**: the change's own test exists and
  `cargo test -p <crate> <full::module::path::name> -- --exact` is green. A bare
  short name under `--exact` runs ZERO tests and exits 0 — check the test count.
- **Before push or tag**: `just preflight` AND `bun run format:check` from the
  REPO ROOT. Neither alone is a pushable tree — preflight runs no
  oxfmt/oxlint, and the pre-push hook runs no tests. A push whose diff is
  Markdown only (`*.md`: AGENTS.md, `docs/`, skills) needs just
  `bun run format:check` + `node scripts/check-history-links.mjs`; any other
  file in the diff takes the full gate.
- **Before tagging a release**: tag `v*` only after green CI.
- **Bump a dependency in its OWN commit**, never inside a feature or fix commit.
  A bump reviewed as a bump gets the question that catches a breaking change
  ("what changed in the components we call?"); one buried under another title
  does not, and the revert is no longer cheap. It has cost this repo a release
  once already.
- **After editing `crates/desktop/src/data/featured-skills.json`**:
  `just featured-check` (see Commands).

Return early only when an ask-first item below blocks you, or when the gate
fails for a reason outside the requested change. A failure you caused is part of
the task, not a reason to stop.

## Surfaces

Authoritative for the CLI: clap (`just start -- --help`, `crates/cli/src/commands/`).
Authoritative for the API: `crates/api/src/lib.rs` + `routes/`.

The user-facing semantics a surface must not get wrong — destructive defaults,
scope resolution, `-a` parsing, what each `outcome` means, which commands refuse
what — live with the surface that owns them: **`crates/cli/AGENTS.md`** and
**`crates/api/AGENTS.md`**. The behaviour they implement, and why it is that way,
lives in the knowledge pages (`技能移除與 Master 回收`,
`多目標突變的 scope 閘門與批次歸因`, `Skill update pipeline`,
`check --write-result 的受管狀態守衛`, `CLI 與 API 的共用錯誤契約`).

Two cross-surface invariants that neither file owns alone:

- **An unreadable lock fails the commands that report it** (`check`, `doctor`,
  `source list`/`diff`). The lock read paths fail OPEN by design; those commands
  present lock contents AS their answer, so they probe first.
- **A verdict has ONE home.** "Did that removal take anything away?" is
  `removal::read_effect_after`, asked of discovery — `delete`, the API delete
  route and `reconcile skill` must not answer it differently.

## Skill mutations

**Mutation lock**: every mutating skill flow holds ONE interprocess lock across
its whole check→write→rollback span; read paths are deliberately unlocked, and it
serializes aghub against aghub only. Invariants and the call-site rule:
`crates/core/AGENTS.md` "Mutation attribution".

## Testing

**The suite is designed to write only into temp dirs, an isolated `$HOME` and
`$AGHUB_DATA_DIR` — a leak into the real home is a bug in that test, not a reason
to ask before running the suite.** The Rust tests make no outbound network calls
(the git-backed ones serve `git://` from a loopback `git daemon`). What reaches
outside a plain `cargo test` is `just featured-check` and the `verify` chain;
`--features agent-validation` needs real agent CLIs on `PATH`, not the network.
Run `cargo test`, `cargo test --workspace` or `just preflight` freely, fix the
failures your change caused, and rerun without asking at each step.

When you write a new test, the isolation is yours to get right. **Never pollute
the real home**: a global-scope write still lands in `~/.aghub` plus each agent's
own skills dir, and overriding `$HOME` alone is not enough. Isolation mechanics,
the one-env-mutex-per-binary rule and the inode-assertion trap:
`crates/core/AGENTS.md` Testing. The app data root also holds the
**disabled-agent selection** every server-side fan-out reads (repair, update,
rename, delete-from-all, `source sync -a all`): a test outside core's lib that
expects one of those to reach a given agent must pin `$AGHUB_DATA_DIR` (the
CLI's `isolated_cli` does), or the developer's own selection changes its answer.

**A test must be able to FAIL on a real regression** — a green test that can't is
worse than none. Assert observable OUTCOMES (values, on-disk / lock state), not a
variant or `is_err()`; for a safety-critical flow exercise the FAILURE path
(rollback AFTER the destructive step). PROVE it: revert the fix, watch the
assertion go red, restore. **A malformed fixture is the sneakiest false green**:
the commands that report lock contents probe the lock before answering, so a
fixture missing a required field makes the command bail while READING and the
assertion passes with the code under test never reached — copy a fixture shape
from an existing test. Worked example:
`docs/specs/2026-07-15-skill-rename-transaction-deepening.md`.

## Agent permissions / approval boundaries

Reasons are given so you can generalize to the case not listed here.

- **Free — do it, do not ask**: editing code; `just fmt` / `just lint`;
  `cargo build`; `bun run typecheck` / `lint:check` / `format:check`; the Rust
  test suite at any scope, `just preflight` included; reading and writing under
  a temp dir, `$AGHUB_DATA_DIR`, or a tempdir project root; committing and
  pushing to `fork`. Run it, fix the failures your change caused, and rerun.
  Stop and ask only if a test would need the real `~/.agents`, the OS keyring,
  or the network.
- **Ask first — legitimate, but external or irreversible**: pushing to someone
  else's upstream (`origin` = AkaraChen), force-push, amending published
  history; release tags, `just bump`, the Homebrew tap unless the user asked
  for the release; touching the developer's REAL `~/.aghub`, `~/.agents`, agent
  skill dirs or the system keyring; adding any new workspace dependency without
  a clear need. These leave the machine or cannot be undone; the dependency
  budget is the maintainer's call.
- **Never — no task reaches these**: changing the shipped `tauri.conf.json`
  updater `pubkey`, or pointing its `endpoints` elsewhere (it bricks
  auto-update for every installed user); committing secrets.

## Anti-Patterns

> Formatting and lint are not listed here — `rustfmt.toml` and CI
> (`cargo fmt --check`, `clippy -D warnings`) enforce them deterministically.
>
> These are correctness invariants, not approval boundaries: they constrain WHICH
> design you pick, never WHETHER you proceed. None is a reason to stop and ask —
> and none is negotiable either. The same holds for every `NEVER` in a per-crate
> `AGENTS.md`.

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
  walk is the trap — `file_name()` is `None` for a path ending in `..`
- When promoting a **private** flow to a **public** seam, re-assert the
  preconditions the old callers used to guarantee (e.g. `accept_rename`
  re-checks the lock itself) — a public entry point is only as safe as its own
  guards

## Release & Packaging

Runbook (versioning, `just bump`, signing secrets, Homebrew tap, workflow
failures): project skill **`releasing-aghub`** + `.github/workflows/release.yml`.
The two release rules that are also approval boundaries are in the table above.

## Agent workflows

- **Issues**: GitHub Issues on the fork — ALWAYS `gh … -R audichuang/aghub`
  (a bare `gh` can resolve to upstream `origin`) — `docs/agents/issue-tracker.md`;
  triage labels in `docs/agents/triage-labels.md`. Pre-switch specs stay under
  `.scratch/<feature>/`
- **Domain docs**: single-context, one root `CONTEXT.md` + `docs/adr/` —
  `docs/agents/domain.md`
