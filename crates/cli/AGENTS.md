# CLI CRATE

`aghub-cli` — the clap binary. **`-p aghub` is the desktop src-tauri package, not
this one**; this crate is `-p aghub-cli`.

`src/main.rs` holds `Cli`/`Commands` + dispatch; `src/commands/` is one file per
subcommand. User-facing semantics (`-a`, scope flags, destructive defaults,
creds) live in root AGENTS.md "CLI Command Surface" and are not repeated here.

## Two dispatch funnels — keep both halves in step

**Scope.** Every generic CRUD path resolves scope through
`resolve_scope_and_root`, gated by `scope_policy`. A new **mutating** subcommand
MUST be added to that fn's `SingleWrite` arm: the `_ => AllowBoth` wildcard means
one that isn't silently bypasses the project-root write guard, and `-p` outside a
project then writes the global Master.

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

## Tests

`tests/cli_tests.rs` (`assert_cmd`). `source sync` e2e need no network:
`AGHUB_TEST_SOURCE_FETCH_ROOT` (a `#[cfg(debug_assertions)]` hook in
`commands/source.rs`) serves a local dir as the fetched repo. `check skills` is
read-only — it never mutates a lock — and its JSON shape is pinned by
`check_skills_outputs_json_array`.

## Anti-patterns

- **Don't** `println!` diagnostics — use `eprintln_verbose!`
- **Don't** hardcode agent id strings — use `AgentType`
