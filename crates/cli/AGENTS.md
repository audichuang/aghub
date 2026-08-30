# CLI CRATE

`aghub-cli` — the clap binary. **`-p aghub` is the desktop src-tauri package, not
this one**; this crate is `-p aghub-cli`.

`src/main.rs` holds `Cli`/`Commands` + dispatch; `src/commands/` is one file per
subcommand. User-facing semantics (`-a`, scope flags, destructive defaults,
creds) live in root AGENTS.md "CLI Command Surface" and are not repeated here.

## Two dispatch funnels — keep both halves in step

**Scope.** ONE table (`scope_policy`), ONE resolver (`resolve_scope`), ONE
resolved value (`Scope`). `scope_policy` is **exhaustive** over `Commands` and
over `SourceAction`, so a new subcommand does not COMPILE until it is
classified — it used to end in `_ => AllowBoth` and rely on a comment. `None`
means the command ignores scope entirely (`inference`, `plugin`) and must not
reach the resolver.

Command modules receive a `Scope`, never `cli.global/project/all`; its fields
are private **to `mod scope`**, so there is nothing left for them to re-derive.
The module matters: Rust privacy reaches every DESCENDANT of the defining
module, so a `Scope` declared in the crate root would still let
`commands::source` forge `Scope { ProjectOnly, None }` and skip the table.
(`aghub_core::paths::find_project_root` stays re-importable by anyone — the
seal is on CONSTRUCTING a resolved scope, not on finding a root.) That is what
stops `source`/`coverage`/`transfer` regrowing private resolvers with their own
wording of the project-root bail (there were five). The bail covers reads too
(`-p get skills` must not answer `[]` from a non-project dir) and every
rejection runs BEFORE the cwd is touched — `-g` and the plain global default
resolve no project root at all, because a deleted cwd must not kill a
global-only command.

**One policy opts out** (`rootless_project_passthrough`, only
`TRANSFER_SCOPE`): `transfer`/`reconcile` never resolved a root in the CLI, so
a rootless `-p` stays `ProjectOnly` with no root and core's source lookup fails
with a typed `ResourceNotFound`. Bailing early instead rewrites `--json`'s
`error.code` to `CLI_ERROR` with nothing else visibly different — pinned by
`rootless_project_transfer_keeps_resource_not_found_code`. That is also why
`transfer::install_scope` maps the scope itself rather than calling
`write_target()`.

**`Scope::write_target()`** is the ONE answer to "which store does this write?"
(`Some(root)` = project, `None` = global) and it ERRORS on anything else.
`source`'s `write_scope`, `accept-rename`'s `RenameScope` and `transfer`'s
`install_scope` each used to close that match with `_ => …::Global`, so a scope
the table let through became a silent write to the GLOBAL lock. Never reopen
one of those matches with a catch-all.

The classification itself is compile-forced but not compile-CHECKED: only review
catches a command classified under the wrong policy. What the test suite adds is
that a new subcommand cannot escape the case table —
`every_subcommand_has_a_policy_case` asks clap for the subcommand list rather
than a second hand-written one.

**Early dispatch.** `source`, `apply-update`, `inference`, `transfer`,
`reconcile`, `coverage`, `doctor` and `skill-usage` run BEFORE any adapter or
`ConfigManager` exists, so a missing or malformed agent config cannot block a
command that never needed one. The `unreachable!()` arms in `run_for_agent`'s
match are that contract — adding an early dispatch without its arm (or the
reverse) is how it rots.

**Multi-agent.** `AgentSelection` (re-exported by `aghub_core::models`, defined
in `crates/agents/src/models.rs`) is the ONE `-a` parser and `core/src/batch.rs`
owns the envelope. Never re-parse `-a` per command; never hand-roll the envelope.

## Commands stay thin

`transfer` / `reconcile` / `coverage` / `inference` are adapters over core (and
`inference::cascade`). Anything a second surface also needs belongs in core —
this crate is a surface, not a home for policy.

## Failure output

`main` is a thin wrapper: `run(cli)` returns `Result`, and `report_failure`
renders it. Under `--json` the error goes to **stdout** as
`{"error":{code,message,retryable}}` with `code` from
`aghub_core::error_codes` (shared with the API) — so raise a `ConfigError` where
one fits instead of an ad-hoc `bail!`, or the code degrades to `CLI_ERROR`.

A command that has ALREADY printed its full answer and returns `Err` only to set
the exit code (the batch envelope, `transfer`/`reconcile`, a partial
`prune-lock`) must call `note_answer_on_stdout()` first, or stdout ends up
holding two JSON documents and every parse of it fails.

## Tests

`tests/cli_tests.rs` (`assert_cmd`). `source sync` e2e need no network:
`AGHUB_TEST_SOURCE_FETCH_ROOT` (a `#[cfg(debug_assertions)]` hook in
`commands/source.rs`) serves a local dir as the fetched repo. `check skills` is
read-only — it never mutates a lock — and its JSON shape is pinned by
`check_skills_outputs_json_array`.

## Anti-patterns

- **Don't** `println!` diagnostics — use `eprintln_verbose!`
- **Don't** hardcode agent id strings — use `AgentType`
