# Plugin marketplace grouping + source-style market browsing

**Status:** design approved (not yet implemented)
**Date:** 2026-06-23

## Motivation

The Claude Code plugins TAB (`/cc-plugins`) gives no sense of **where a plugin
came from**. Two concrete gaps:

1. The left **installed list** (`PluginList`) is a flat, alphabetical list. You
   cannot tell which marketplace each installed plugin was downloaded from
   without clicking into the detail pane's "installed from" card.
2. The **market dialog** (`PluginMarketDialog`) → "Plugins" tab is one flat
   table mixing every marketplace's plugins together, and it _hides_ anything
   already installed. There is no "browse a source, see what it offers, see what
   I have / don't have, and act" experience — which the **Sources** view
   (`InstallFromSourcePanel`) already does well for skills.

This work brings marketplace grouping to both surfaces, and ports the Sources
"grouped browse + state + select-and-act" UX onto plugins — staying **plugins
only** (no skills mixed in) and **frontend only** (no Rust/DTO changes).

The marketplace identity is available, but Part B needed one small backend
addition (see below):

- Installed list: `CCPluginResponse.id` is `name@source` (`crates/cc-plugins`
  `PluginId { name, source }`); the `@source` segment is the marketplace name.
  `source_info.{label,is_github}` gives the resolved repo label + icon hint.
- Market list: `CCPluginMarketResponse` originally had **no** `marketplace`
  field — only `github_url`. That url is **not** a usable grouping key: for the
  official marketplace it is a _per-plugin_ `…/tree/main/<name>` url
  (`PluginInfo::github_url`, LocalRelative + `claude-plugins-official`), so
  grouping by it shatters the official marketplace into one singleton group per
  plugin. Fix: a `marketplace: String` field was added to the DTO (sourced from
  the existing `PluginInfo.marketplace`) so Part B groups by the same
  marketplace-name key as Part A. The header _label_ still derives from
  `github_url` (trimmed at `/tree/`), falling back to the marketplace name.

---

## Part A — Installed list grouped by marketplace (left pane)

Change `PluginList` from a flat `ListBox` into a marketplace-grouped accordion.
Everything else in the pane (search header, the +/multi-select/refresh buttons,
`MultiSelectFloatingBar`, empty states) is **unchanged**.

### Grouping

- **Group key**: substring of `plugin.id` after the **last** `@` (mirrors the
  backend `PluginId` `rsplit_once('@')` contract). Plugins whose id has no `@`
  fall into an "ungrouped/unknown" bucket rather than being dropped.
- **Group display**: take the group's first plugin's `source_info` for the
  header `label` (e.g. `anthropics/claude-code`) and `is_github` (icon choice).
  Same-marketplace plugins share these, so any member is representative.
- **Group order**: `claude-plugins-official` always first, then by `label`
  alphabetically (mirrors `marketplaces-panel`'s official-pinned convention).
- **Within a group**: keep the existing "enabled-first → name alpha" sort.
- Pure helper `groupPluginsByMarketplace` lives in `lib/` (next to
  `getMcpMergeKey`), is unit-testable, and is the single grouping seam.

### Header row (per group)

`source icon + repo label (truncate) + count`. The icon reuses the **source-card
visual language**: `simple-icons` `siGithub` when `is_github`, else
`GlobeAltIcon` — this is the existing "plugin provenance" idiom, more fitting
here than `marketplaces-panel`'s lobehub icon. Count is a muted small badge.

### Default expand / search

- Default: **all groups expanded**; v1 does **not** persist manual collapse
  state (reopening the app returns to all-expanded). This is the simplest v1;
  installed plugins are typically few.
- On search: filter first, then group; **hide groups with zero matches**, and
  force-expand the groups that do contain matches (controlled `expandedKeys`,
  reverting to all-expanded when the query clears).

### Component shape (the core trade-off)

HeroUI v3 `Accordion` (`selectionMode="multiple"`, `defaultExpandedKeys` = all
group keys, no-frame `variant="default"` to suit the 320px pane). Each
marketplace is one `Accordion.Item`:

```
ListSearchHeader (unchanged)
└ Accordion (multiple, defaultExpandedKeys = all groups)
  └ Accordion.Item (one per marketplace)
     ├ Accordion.Heading → Trigger: source icon + repo label + count; Indicator
     └ Accordion.Panel → ListBox (that group only)
        └ ListBox.Item (puzzle icon + name + enabled dot — moved verbatim)
MultiSelectFloatingBar (unchanged)
```

### Selection across groups (must not regress multi-select)

Each group has its own `ListBox`, but the selected set is global. To stop one
group's `onSelectionChange` from clobbering another group's selection:

- Pass each `ListBox` only `selectedKeys ∩ that group's ids`.
- On a group's change: **remove all of that group's old keys from the global
  set, then merge in the keys it just returned**.
- This is added as a "grouped merge" mode to the existing `useMultiSelect` hook;
  `index.tsx`'s `handleSelectionChange` / `effectiveSelectedKeys` are unchanged.
- Known limit: up/down arrow navigation does not flow continuously across group
  boundaries (inherent to multiple `ListBox`es). Acceptable for a
  click-dominated installed list.

---

## Part B — Market dialog "Plugins" tab: grouped browse + state + manage

Rework the "Plugins" tab (`PluginMarketTable` + the dialog's filtering) into a
marketplace-grouped, source-style view that shows **all** plugins (installed and
not), tags state, lets you batch-install the missing, and lets you manage the
installed ones inline.

### Grouping & display

- **Group key**: `CCPluginMarketResponse.marketplace` (the marketplace-name
  field added to the DTO for this — see Motivation). **Not** `github_url`, which
  is per-plugin for the official marketplace. Same header treatment as Part A
  (icon + label + count): the label is derived from `github_url` (trimmed at
  `/tree/`), falling back to the marketplace name.
- **Show everything**: do **not** filter out installed plugins (current behavior
  filters them). Installed entries render with an "installed" state chip; not-
  installed entries are selectable for install. This is the Sources-style
  "source overview" the user asked for.
- **State is binary**: only `installed` / `notInstalled`. There is **no
  "outdated" state** — `cc-plugins` documents that the claude CLI has no
  check-for-updates semantics (update = install-latest). So the 7-state Sources
  diff collapses to 2 states here. (See follow-ups for a possible future
  "update available" via commit-hash compare.)
- **Default expand**: opposite of Part A — groups **default collapsed** (a
  marketplace can offer dozens of plugins). Expand the groups that contain
  not-installed plugins (or the first group if none do); on search, expand the
  groups with matches.
- **Search + category filter**: keep the existing search field and category
  `Select`; results are simply grouped by marketplace.

### Installing the not-installed (batch)

- Source-style: not-installed plugins are selectable (checkbox); a footer action
  installs the **selected** not-installed plugins, or **all** not-installed
  plugins currently shown (respecting the active search / category filters).
- Reuse the existing `installPluginMutationOptions` — no new install path.

### Managing the installed (inline, per-plugin)

Installed rows expose **update / enable-disable / uninstall**, reusing the
existing per-plugin hook `usePluginDetailActions({ pluginId, scope })`:

- update → `POST /plugins/{id}/update`
- enable/disable → `POST /plugins/{id}/config` (`{ scope, enabled }`)
- uninstall → `DELETE /plugins/{id}`

### Data join (market entry lacks enabled/version/scope)

`CCPluginMarketResponse` has only `installed_scopes`; it has no `enabled`,
`version`, or scope detail. To render the enabled toggle and feed the actions
correctly, **join the market entry to the installed entry by `id`**: look the
market `id` up in `pluginListQueryOptions` (`CCPluginResponse`, which has
`enabled` / `version` / `scopes`). The chosen scope for an action comes from the
installed entry's `scopes` (falling back to the dialog's `installScope`).

### State synchronization (the user's explicit concern)

This is the load-bearing invariant. All plugin mutations — install, uninstall,
update, enable/disable, whether triggered from the left pane, the detail pane,
or this market view — already funnel through one invalidation:

```ts
await queryClient.invalidateQueries({ queryKey: queryKeys.plugins.all() }); // ["plugins"]
```

Because `plugins.list()`, `plugins.market()`, and `plugins.detail(id)` all sit
under the `["plugins"]` prefix, any mutation re-fetches **all three views**, so
they stay consistent automatically.

**Invariant: the market view MUST NOT implement its own install/manage logic.**
It reuses `installPluginMutationOptions` and `usePluginDetailActions` so the sync
path stays single-sourced. This is the precondition that makes a second
management entry point safe.

### Accepted trade-off

Managing plugins now has two entry points (left pane/detail + market view). The
cost is UI complexity; the inconsistency risk is removed by the shared-mutation +
unified-invalidate rule above.

---

## File map (what will change)

**Backend (`crates/api`) — one additive DTO field for Part B grouping**

- `src/dto/plugin.rs` — add `marketplace: String` to `CCPluginMarketResponse`.
- `src/routes/plugins.rs` — populate it from `PluginInfo.marketplace` in
  `list_plugin_market`.
- `crates/desktop/src/generated/dto/` — regenerate (`export-dto` + prettier);
  only `CCPluginMarketResponse.ts` changes.

**Desktop (`crates/desktop`) — frontend**

- `src/lib/group-plugins-by-marketplace.ts` _(new)_ — pure grouping helper that
  takes a key-extractor + header-resolver, so Part A passes the id-derived
  `@source` key and Part B passes the explicit `marketplace` field (same sort /
  official-pinned logic for both). Unit tests: multi-group, official-pinned,
  within-group sort, id-without-`@` fallback.
- `src/hooks/use-multi-select.ts` — add the "grouped merge" selection mode.
- `src/components/plugin-list.tsx` — Accordion + per-group `ListBox` (Part A).
- `src/components/plugin-market/market-table.tsx` — grouped rows + state chip +
  batch-select install + inline manage (Part B); may split into smaller
  components (group section, row) if it grows too large.
- `src/components/plugin-market-dialog.tsx` — stop filtering out installed;
  join market↔installed by id; pass scope into row actions; group-expansion
  state.
- (maybe) `src/lib/` — a small market↔installed merge util if the join logic
  warrants its own seam.
- `src/lib/locales/{en,zh-Hant,zh-Hans}.ts` — i18n keys (group headers, state
  chips, batch-install actions, manage tooltips).

**Not touched**: the detail pane (`PluginDetail`), the "Marketplaces" tab, the
Sources view. (The only Rust/DTO change is the additive `marketplace` field
above.)

---

## Implementation status

- [ ] `groupPluginsByMarketplace` pure helper + unit tests
- [ ] `useMultiSelect` grouped-merge mode
- [ ] Part A: `PluginList` accordion + per-group ListBox + cross-group selection
- [ ] Part A: search-aware expansion + empty-group hiding
- [ ] Part B: market dialog stops hiding installed + market↔installed id join
- [ ] Part B: grouped market rows + binary state chip + default-collapsed expand
- [ ] Part B: batch-select install (reuse `installPluginMutationOptions`)
- [ ] Part B: inline manage (reuse `usePluginDetailActions`) with correct scope
- [ ] i18n keys (en / zh-Hant / zh-Hans)
- [ ] Verify: `bun run build` + prettier/eslint/tsc (the pre-push gate)

## Known follow-ups / not yet done

- **Expand-state persistence** (store migration) — v1 is non-persistent for both
  panes.
- **Group-level "select all"** in either pane — v1 is per-item selection only.
- **"Update available" detection for plugins** — not possible today (claude CLI
  has no check-for-updates); a future option is comparing installed
  `commit_hash` against the marketplace's latest, surfaced as a third state.
- Part B grouping uses the `marketplace` field directly; if a resolved repo
  label isn't derivable for the market entries, headers fall back to the raw
  marketplace name (acceptable; revisit if labels look poor).
