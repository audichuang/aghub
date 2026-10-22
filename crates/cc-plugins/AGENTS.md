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
  thin over `claude plugin …`. `ClaudeCli::new()` resolves the binary via `which` and then a list of
  candidate install dirs (`~/.local/bin`, `~/.bun/bin`, homebrew, …), so
  emptying `PATH` does not neutralize it; tests must avoid real spawns
- `ClaudePluginManager::owned_roots` is cached process-wide, keyed on `(mtime, len)` of `~/.claude/plugins/installed_plugins.json` (deliberately not `CLAUDE_CONFIG_DIR`: tests isolate `$HOME` only) plus a 10s TTL; `synced/` and `skills/<dir>/.claude-plugin` sources never touch that file, so they refresh by TTL only. `new()`/`load_via_cli` stay uncached (the plugins page needs fresh `enabled`)
- CLI has **no "check for updates"** semantics — update = install-latest; no diff step
- `manager.rs` (`load_via_cli`) and `lifecycle.rs` (`fetch_installed`) parse the
  same `plugin list` into **different** types (discovery vs management) — intentional
- **Lenient per-entry parse** applies to every third-party list we read —
  `plugin_list` (installed), `plugin_list_available` (catalog),
  `marketplace_list` (all via `lenient_collect`) and on-disk
  `MarketplaceConfig.plugins` — one bad entry is skipped + warned, never
  blanks the list. Claude Code adds entry shapes faster than these typed views
  (e.g. `<name>@skills-dir` entries carry no `installedAt`/`lastUpdated`). A
  field a third party may omit gets `#[serde(default)]`, not a required type
- Marketplace `"source": "directory"` (alias `"local"`) parses to
  `CliMarketplaceSource::Local` (`cli/types.rs`); the separate
  `MarketplaceEntry::Directory` is `registry_catalog.rs`'s plugins-subdir
  fallback, unrelated to the JSON `source` field. Do not assume git-only sources
