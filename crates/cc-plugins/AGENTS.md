# CC-PLUGINS CRATE KNOWLEDGE BASE

**Crate**: `aghub-cc-plugins` — Claude Code plugin lifecycle (install / marketplace / enable-disable).

## OVERVIEW

Manages Claude Code plugins and their marketplaces. Most lifecycle operations **shell out to the `claude` CLI** via `ClaudeCli` (`cli/mod.rs`); marketplace discovery + git/tarball materialization are done in-crate.

## STRUCTURE

```
src/
├── lib.rs                  # Public exports
├── cli/                    # ClaudeCli: spawn the `claude` binary (the process seam)
├── claude/                 # manager.rs (multi-scope plugin info), manifest.rs, settings.rs (pluginConfig in settings.json), capabilities.rs
├── discovery/              # marketplace.rs + registry.rs (catalog lookup)
└── installer/              # lifecycle.rs (install/update/uninstall via CLI), git.rs (clone), marketplace*.rs (materialize), registry.rs
```

## WHERE TO LOOK

| Task                          | Location                           |
| ----------------------------- | ---------------------------------- |
| install/update/uninstall      | `src/installer/lifecycle.rs`       |
| marketplace add/remove/update | `src/installer/marketplace_ops.rs` |
| list/enable/disable + scopes  | `src/claude/manager.rs`            |
| settings.json `pluginConfig`  | `src/claude/settings.rs`           |
| spawn the claude binary       | `src/cli/mod.rs` (`ClaudeCli`)     |

## COMMANDS

```bash
cargo test -p aghub-cc-plugins
```

## GOTCHAS / ANTI-PATTERNS

- **The testability seam is `ClaudeCli` (the process boundary), not the installers** — lifecycle ops are thin wrappers over `claude plugin …`. `ClaudeCli::new()` runs `which`, so tests must avoid real spawns.
- `lifecycle.rs` documents that the CLI has **no "check for updates"** semantics — update is install-latest; don't assume a diff step.
- `manager.rs` (`load_via_cli`) and `lifecycle.rs` (`fetch_installed`) parse the same `plugin list` output into **different** types (discovery vs management) — intentional, not duplication.
