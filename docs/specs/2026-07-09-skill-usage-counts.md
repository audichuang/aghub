# Skill Usage Counts (Claude)

**Status**: design (brainstorming validated 2026-07-09)
**Scope**: read-only query surface in CLI + desktop; Claude only.

## Understanding Summary

- **What**: a read-only view that shows, per Claude skill, how many times it has
  been invoked (`usageCount`) and when it was last used (`lastUsedAt`), so the
  user can find never-/rarely-used skills to prune.
- **Why**: surface dead skills. The zero-use ones are the prune candidates.
- **Who**: the user, via **both** the CLI and the desktop app.
- **Data source**: `~/.claude.json` → `skillUsage` map
  (`{ <name>: { usageCount, lastUsedAt } }`). Claude Code already writes this on
  every real dispatch — we do NOT build our own tracker.
- **Non-goals**: no self-built tracking; no other agents; no mutation of
  `skillUsage`; no change to the shared `Skill` model.

## Assumptions

- `skillUsage` keys join to aghub-managed Claude skills by the skill's
  **frontmatter `name`** (== the invocation name Claude keys usage by), which is
  exactly the name aghub's discovery records (`convert_skill` → `skill.name`) and
  shows in the UI. This is NOT necessarily the directory name: e.g. the
  `ticktick-skill/` dir declares `name: ticktick`, and Claude's key is
  `ticktick` — a dir-name join would miss it, a frontmatter-name join hits it.
  Plugin skills use `plugin:name` keys and are out of scope (aghub does not
  manage them).
- An installed skill absent from `skillUsage` = **0 uses / never used** (the
  key is only written on first dispatch). This is the prune signal we want.
- `lastUsedAt` is epoch millis; may be missing → render as "never".
- Reading `~/.claude.json` follows the stateless design: read the real file,
  return empty map if the file or key is missing (no error).

## Decision Log

1. **Reuse Claude's `skillUsage`, not a new tracker.**
   Alternatives: (a) parse Claude/Codex session transcripts; (b) aghub-side
   counter. Chosen reuse because the exact, pre-computed counter already exists
   at zero cost. Sessions/transcripts are heuristic and O(all history).
2. **Claude only; Codex deferred.**
   Codex has no usage counter — the only source is heuristic `SKILL.md`-read
   grepping across `~/.codex/sessions/*.jsonl` (loaded ≠ used, slows with
   history, format drift). Not worth shipping inaccurate data. Revisit only on
   real demand.
3. **Do not extend the shared `Skill` model.**
   23 agents share it; a Claude-only field leaks. Usage is a side map joined at
   the surface (like `sources`), CLI-gated behind `--usage`.
4. **Mirror the marketplace/inference/source layering.**
   core domain fn → API response DTO (`#[derive(TS)]` → generated `.ts`) → CLI
   (domain fn + `tabled` table + `--json`) → desktop (request + react-query +
   list column). Same道理, no new architecture.
5. **CLI surface = a dedicated `skill-usage` subcommand** (dispatched before
   agent-config setup, like `coverage`/`source`), not a flag on `get skills`.
   `get skills` is entangled with the per-agent manager + scope; usage is
   Claude-global, so a standalone read subcommand matches the codebase
   convention ("manage it like marketplace"). It rejects `-p`/`--all`.
6. **Desktop surface = a usage line in the skill detail panel**, shown only in
   global-scope views (matching by name against a project-scoped skill would
   surface an unrelated global skill's count).

## Design

### Layer 1 — core domain (`crates/core/src/skills/usage.rs`)

```rust
pub struct SkillUsage {
    pub name: String,
    pub usage_count: u64,
    pub last_used_at: Option<i64>, // epoch millis
    pub installed: bool,           // present in the installed skill set
}

/// Read `skillUsage` from Claude's global config. Empty map if file/key absent.
fn read_claude_skill_usage() -> HashMap<String, RawUsage>;

/// Left-join installed Claude skills against skillUsage; absent → 0/None.
/// Sorted usage_count ASC (least-used first).
pub fn list_skill_usage(installed: &[Skill]) -> Vec<SkillUsage>;
```

- Claude global config path comes from the Claude descriptor (not hard-coded).
- Left-join over **installed** skills so 0-use skills appear (they are the point).
- Ponytail check: one `test_*` asserting the left-join defaults absent skills to
  `{count:0, last:None, installed:true}` and sorts ascending.

### Layer 2 — API (`crates/api/src/routes/skills.rs`)

- `GET /api/v1/skills/usage` (agent fixed to claude; scope via existing query
  params like the skills list route).
- Response DTO with `#[derive(Serialize, TS)]`:

```rust
struct SkillUsageResponse { name, usage_count, last_used_at, installed }
struct SkillUsageListResponse { skills: Vec<SkillUsageResponse> }
```

- ts-rs emits `crates/desktop/src/generated/dto/SkillUsage*.ts` (same as
  `CCMarketplaceListResponse`).

### Layer 3 — CLI (`crates/cli/src/commands/skill_usage.rs`)

- A dedicated `skill-usage` subcommand, dispatched before agent-config setup:
    - `--json` → serialize the core `SkillUsage` rows.
    - else → `tabled::Builder` table `SKILL | USES | LAST USED`, `Style::sharp()`,
      already sorted ascending by core (0-use rows on top). `LAST USED` renders
      the epoch-ms `last_used_at` as a local date via `chrono`, or "never".
- Rejects `-p`/`--project` and `--all` (Claude-global only).

### Layer 4 — desktop (`crates/desktop/src`)

- `requests/skills.ts`: `skillUsageQueryOptions` hitting the new route.
- The skill **detail panel** (`components/skill-detail.tsx`) shows a usage
  chip (`Used N times` / `Never used`) plus last-used date, joined by name.
  Rendered ONLY in global-scope views (query `enabled: !projectPath`) so a
  project skill sharing a name with a global one cannot show its count.

## Risks

- **Key mismatch**: a skill whose frontmatter `name` differs from its dir name
  could miss the join. Mitigation: join on the name aghub already uses to list
  the skill (same string the UI shows), so it stays consistent with the list.
- **`skillUsage` schema drift** if Claude Code changes the shape. Low risk;
  guarded by `serde(default)` and empty-map fallback.
- **`lastUsedAt` staleness**: counts are cumulative since install, never reset —
  documented, not a bug.
- **Plugin-managed skills** are counted by bare name. Claude Code keys a
  plugin skill's usage under `plugin:name`, so a plugin skill that happens to
  live in `~/.claude/skills` would under-report as 0. In practice
  `~/.claude/skills` holds only user/aghub-managed skills (plugin skills live
  in the plugin cache), so this does not currently manifest. Unlike the API
  `/agents/all/skills` list route — which filters plugin-managed skills via
  `ClaudePluginManager` — `skill-usage` does not (that manager is API-only;
  wiring it through `aghub-core` for an edge case is deferred). Revisit if a
  plugin is found installing skills directly into `~/.claude/skills`.
