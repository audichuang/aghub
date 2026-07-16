# Desktop agent-coverage matrix + MCP bulk-manage parity

**Date**: 2026-07-16 · **Scope**: `crates/desktop` (frontend only) · **Backend**: none (reuses existing APIs)

Originated from an upstream sync review (`UPSTREAM.md`, review @ `c1801ede`). The
upstream `agent-coverage-matrix.tsx` (a bulk-panel summary that shows per-agent
installed-count over a selection + one-click bulk install/uninstall) is **already
present in this fork** as `BulkManageGroupAgentsDialog` (skills only). So we do not
port it. We build the two genuine gaps instead.

## Understanding

- **What**: (Z) extend "bulk-manage agents" to MCP; (Y) a standalone coverage
  overview grid page — skills + MCPs as rows × usable agents as columns, each cell
  = installed/not, click to toggle one resource on one agent.
- **Why**: (Z) fills an asymmetry (skills have a bulk-manage dialog, MCP only has
  bulk-delete). (Y) is the desktop face of the CLI `coverage` command — at-a-glance
  visibility + direct editing without opening a per-resource dialog.
- **Who**: users managing which agents carry which skills/MCPs (the tool's core job).
- **Non-goals**: NOT adopting upstream's dnd-kit / #282 architecture; NOT
  re-implementing the existing skill bulk dialog; no drag-drop / custom groups.

## Decision log

1. Upstream coverage-matrix ≈ existing `BulkManageGroupAgentsDialog` → build the
   real gaps, don't port a duplicate.
2. Data source: reuse existing `skillListQueryOptions` / `mcpListQueryOptions`
   (each resource already carries `{name, agent, source}`) → group by name into
   `{name, items:[{agent, source}]}`; `installedAgents = items.map(agent)`. **No new API.**
3. Reuse pure primitives in `lib/group-agent-plan.ts`: `computeGroupAgentStats`,
   `computeSkillAgentDiff`, `buildReconcilePlans`, `wouldOrphanSkill`.
4. **Data-loss safety**: removing a skill's _last_ agent via reconcile can orphan it
   (`reconcile_skill` removes even if the copy to the new agent failed — see
   `aghub-reconcile-orphan-guard`). The grid's per-cell removal MUST route through
   the same `wouldOrphanSkill` guard as `manage-skill-agents-dialog`. MCP has no such
   risk (delete rewrites shared config, deletes no disk path).
5. (Z) approach: **generalize** `BulkManageGroupAgentsDialog` to a `kind: "skill" |
"mcp"` param (pick reconcile mutation + capability check + orphan-guard by kind),
   per AGENTS.md "don't hand-mirror across surfaces". Skills path stays behaviorally
   identical.
6. (Y) placement: standalone sidebar page `/coverage` (spans skills+MCP, doesn't
   belong under the Skills page).
7. (Y) columns: one **unified** column set = all usable agents supporting skill OR
   mcp mutation at the scope. In the skills section, cells for agents that don't
   support skill mutation render a muted non-interactive "–" (and vice-versa for mcp),
   so the two sections share the same column axis.

## Design

### (Z) Generalized bulk-manage dialog

- `BulkManageGroupAgentsDialog` gains `kind: "skill" | "mcp"` (default keeps skill
  behavior). Internals branch: reconcile mutation (`reconcileSkills` vs
  `reconcileMcps`), capability filter (`supportsSkillMutation` vs `supportsMcpScope`),
  orphan guard (skill only). `group-agent-plan` helpers are already resource-agnostic.
- Wire an MCP entry point on the mcp-servers page's multi-select bar (a "manage
  agents" action beside delete), mirroring the skills page.

### (Y) Coverage grid page — `pages/settings/coverage.tsx`, route `/coverage`

- Sidebar item `coverage` (add to `SIDEBAR_ITEM_IDS` + `DEFAULT_SIDEBAR_ITEMS` +
  `SIDEBAR_ITEM_DEFINITIONS`; icon e.g. `TableCellsIcon`).
- Scope control (global / project) — extract the local `ScopeControl` from
  `settings/skills.tsx` into a shared component and reuse.
- Fetch skills + mcps for the scope; group by name. Rows = two sections
  (Skills, MCP Servers), each row a resource; columns = unified usable-agent set.
- Cell state via `computeSkillAgentDiff`; click → optimistic reconcile add/remove of
  that single agent for that single resource; last-agent skill removal blocked by
  `wouldOrphanSkill` (toast + no-op, same as the dialog).
- Column header shows per-agent totals (`computeGroupAgentStats`).
- Virtualize with `react-virtuoso` (already a dep) only if row count warrants;
  otherwise plain — decide at build time, note the ceiling.

## Test strategy (AGENTS.md: a test must be able to FAIL)

- Pure logic gets colocated `*.test.ts` (node:test): the grid's cell-applicability /
  cell-state derivation, and the kind→(mutation, guard) selection. Assert observable
  outcomes — e.g. removing a skill's only agent yields a blocked plan, not a reconcile
  call; an mcp cell for a skill-only agent is non-interactive.
- Existing `group-agent-plan` tests continue to cover the shared primitives.
