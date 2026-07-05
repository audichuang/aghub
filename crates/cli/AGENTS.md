# CLI CRATE

**Crate**: `aghub-cli` — Binary providing the `aghub-cli` CLI tool (`-p aghub` is the desktop src-tauri crate, NOT this one).

## OVERVIEW

Command-line interface for managing AI agent configurations. Uses clap derive macros for argument parsing and dispatches to subcommand handlers.

## STRUCTURE

`src/main.rs` (clap `Cli`/`Commands` + dispatch; `describe` is an inline
module here, not a file) + `src/commands/` — one file per subcommand
(`ls` it for the current list; includes v2.4.0's `transfer.rs` for
transfer/reconcile, `coverage.rs`, `inference.rs`, plus `source.rs`,
`check.rs`, `apply_update.rs`, `prune.rs`, `plugin.rs`). Don't maintain an
enumerated tree here — it drifts.

## WHERE TO LOOK

| Task                                  | Location                                        | Notes                                                                            |
| ------------------------------------- | ----------------------------------------------- | -------------------------------------------------------------------------------- |
| Add CLI flag                          | `src/main.rs`                                   | Modify `Cli` or `Commands` struct                                                |
| Add subcommand                        | `src/commands/<name>.rs` + `mod.rs`             | Follow existing command pattern                                                  |
| Resource type alias                   | `src/main.rs:ResourceType`                      | Add `#[value(alias = "...")]`                                                    |
| Table output format                   | `src/commands/get.rs`                           | Uses `tabled` crate                                                              |
| transfer/reconcile/coverage/inference | `src/commands/{transfer,coverage,inference}.rs` | thin CLI over deep core seams (`core::transfer`, `inference::cascade`)           |
| CLI tests                             | `tests/cli_tests.rs`                            | `assert_cmd` e2e; unix-gated tests: keep helpers/imports under the same `#[cfg]` |

## COMMANDS

```bash
# Build this crate only (package is `aghub-cli`; `-p aghub` is the desktop src-tauri crate!)
cargo build -p aghub-cli

# Run with args
cargo run -p aghub-cli -- get skills
cargo run -p aghub-cli -- add mcp my-server --command "npx -y @modelcontextprotocol/server-filesystem /path"

# Test this crate only
cargo test -p aghub-cli                    # integration tests: cargo test -p aghub-cli --test cli_tests

# Install locally (from workspace root)
just install
```

## SKILL UPDATE CHECK (`check skills`)

`src/commands/check.rs` reports each locked skill's update status as a
`status`-tagged JSON array. Two modes, both **read-only** (never mutate either
lock):

- **Default (offline)** — no network; remote sources are reported
  `uncheckable` (`network`), local-only sources `local`.
- **`--online`** (alias `--check-remote`) — runs the shared `skill-update`
  orchestrator with the same env `TokenResolver` as the `source` commands
  (`GIT_PASSWORD` on any host; `GITHUB_TOKEN` bound to github.com): a
  cheap ls-refs preflight skips the fetch when the upstream tip is unchanged
  **and** the installed copy is unmodified, else a treeless fetch + hash compare.

The orchestrator itself lives in `crates/skill-update` (shared with the desktop
API); the CLI only injects its env token resolver and the default git adapters.
The default offline JSON contract is pinned by `check_skills_outputs_json_array`
in `tests/cli_tests.rs`; the online path has a network-free empty-lock test plus
an `#[ignore = "network"]` end-to-end test.

## ANTI-PATTERNS

- **Don't** use `println!` for diagnostic output — use `eprintln_verbose!` macro
- **Don't** hardcode agent type strings — use `AgentType` enum and `to_string()`
- **Don't** bypass `ConfigManager` — all config operations go through the manager
