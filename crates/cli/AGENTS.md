# CLI CRATE

**Crate**: `aghub` — Binary providing the `aghub-cli` CLI tool.

## OVERVIEW

Command-line interface for managing AI agent configurations. Uses clap derive macros for argument parsing and dispatches to subcommand handlers.

## STRUCTURE

```
src/
├── main.rs           # Entry point, CLI parsing, command dispatch
├── commands/
│   ├── mod.rs        # Module exports
│   ├── get.rs        # List resources (skills, mcps)
│   ├── add.rs        # Add resources with options
│   ├── update.rs     # Update existing resources
│   ├── delete.rs     # Delete resources
│   ├── enable.rs     # Enable disabled resources
│   ├── disable.rs    # Disable resources
│   └── describe.rs   # JSON output for a single resource
└── ui/               # (reserved for UI utilities)
```

## WHERE TO LOOK

| Task                | Location                            | Notes                             |
| ------------------- | ----------------------------------- | --------------------------------- |
| Add CLI flag        | `src/main.rs`                       | Modify `Cli` or `Commands` struct |
| Add subcommand      | `src/commands/<name>.rs` + `mod.rs` | Follow existing command pattern   |
| Resource type alias | `src/main.rs:ResourceType`          | Add `#[value(alias = "...")]`     |
| Table output format | `src/commands/get.rs`               | Uses `tabled` crate               |
| CLI tests           | `tests/cli_tests.rs`                | Uses `assert_cmd`                 |

## COMMANDS

```bash
# Build this crate only
cargo build -p aghub

# Run with args
cargo run -p aghub -- get skills
cargo run -p aghub -- add mcp my-server --command "npx -y @modelcontextprotocol/server-filesystem /path"

# Test this crate only
cargo test -p aghub

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
  orchestrator with an env (`GIT_USERNAME`/`GIT_PASSWORD`) `TokenResolver`: a
  cheap ls-refs preflight skips the fetch when the upstream tip is unchanged
  **and** the installed copy is unmodified, else a treeless fetch + hash compare.

The orchestrator itself lives in `crates/skill-update` (shared with the desktop
API); the CLI only injects its env token resolver and the default git adapters.
The default offline JSON contract is pinned by `check_skills_outputs_json_array`
in `tests/cli_tests.rs`; the online path has a network-free empty-lock test plus
an `#[ignore = "network"]` end-to-end test.

## ANTI-PATTERNS

- **Don't** use `println!` for diagnostic output — use `eprintln_verbose!` macro
- **Don't** add `--interactive` as a global flag — use the `interactive` subcommand pattern instead
- **Don't** hardcode agent type strings — use `AgentType` enum and `to_string()`
- **Don't** bypass `ConfigManager` — all config operations go through the manager
