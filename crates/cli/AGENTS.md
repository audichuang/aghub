# CLI CRATE

**Crate**: `aghub-cli` — Binary `aghub-cli`. **`-p aghub` is the desktop src-tauri package, NOT this.**

## STRUCTURE

`src/main.rs` (clap `Cli`/`Commands` + dispatch; `describe` is inline) +
`src/commands/` — one file per subcommand (`ls` it; do not enumerate here).

## WHERE TO LOOK

| Task                 | Location                           | Notes                                       |
| -------------------- | ---------------------------------- | ------------------------------------------- |
| Add flag/subcommand  | `main.rs` + `commands/<name>.rs`   | Register in `mod.rs` + `Commands` enum      |
| Resource aliases     | `main.rs` `ResourceType`           | `#[value(alias = "...")]`                   |
| transfer/reconcile   | `commands/transfer.rs`             | Thin over `core::transfer`                  |
| coverage / inference | `commands/{coverage,inference}.rs` | Thin over core / `inference::cascade`       |
| skill-usage          | `commands/skill_usage.rs`          | Claude-global only                          |
| CLI e2e tests        | `tests/cli_tests.rs`               | `assert_cmd`; unix-gated helpers stay cfg'd |

Multi-agent runs: `-a` semantics live in root AGENTS.md "CLI Command Surface";
`AgentSelection` (from `aghub_core::models`; defined in `crates/agents`) is the
single parser — never re-parse `-a` per command, and never hand-roll the batch
envelope (`core/src/batch.rs`).

`source sync` e2e need no network: `AGHUB_TEST_SOURCE_FETCH_ROOT` (debug-only
env hook in `commands/source.rs`) serves a local dir as the fetched repo.

## SKILL UPDATE CHECK (`check skills`)

`commands/check.rs` — **read-only** (never mutates locks):

- **Default offline**: remote sources → `uncheckable` (`network`); local → `local`
- **`--online`** (alias `--check-remote`): shared `skill-update` orchestrator +
  the same env token resolver as `source` (creds contract: root AGENTS.md)

Contract pinned by `check_skills_outputs_json_array` in `cli_tests.rs`.

## ANTI-PATTERNS

- **Don't** `println!` diagnostics — use `eprintln_verbose!`
- **Don't** hardcode agent id strings — use `AgentType`
