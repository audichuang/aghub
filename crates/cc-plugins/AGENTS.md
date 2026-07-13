# CC-PLUGINS CRATE KNOWLEDGE BASE

**Crate**: `aghub-cc-plugins` — Claude Code plugin lifecycle (install /
marketplace / enable-disable). Most ops shell out via `ClaudeCli`; marketplace
discovery + git/tarball materialization are in-crate.

## WHERE TO LOOK

| Task                          | Location                       |
| ----------------------------- | ------------------------------ |
| install/update/uninstall      | `installer/lifecycle.rs`       |
| marketplace add/remove/update | `installer/marketplace_ops.rs` |
| list/enable/disable + scopes  | `claude/manager.rs`            |
| settings.json `pluginConfig`  | `claude/settings.rs`           |
| spawn `claude` binary         | `cli/mod.rs` (`ClaudeCli`)     |

## GOTCHAS / ANTI-PATTERNS

- **Test seam is `ClaudeCli` (process boundary)**, not installers — lifecycle is
  thin over `claude plugin …`. `ClaudeCli::new()` runs `which`; tests must avoid
  real spawns
- CLI has **no "check for updates"** semantics — update = install-latest; no diff step
- `manager.rs` (`load_via_cli`) and `lifecycle.rs` (`fetch_installed`) parse the
  same `plugin list` into **different** types (discovery vs management) — intentional
- **Lenient per-entry parse** applies to the CLI **list** output —
  `plugin_list_available` (catalog) and `marketplace_list` (via
  `lenient_collect`) — one bad entry must not blank those. Strict
  whole-document parse (one bad entry fails everything) is used both for the
  installed `plugin list` (`ClaudeCli::plugin_list` → `Vec<CliInstalledPlugin>`)
  AND for on-disk `MarketplaceConfig` (`serde_json::from_str` in
  discovery/installer)
- Marketplace `"source": "directory"` (alias `"local"`) → `MarketplaceEntry::Directory`;
  do not assume git-only sources

```bash
cargo test -p aghub-cc-plugins
```
