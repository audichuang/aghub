---
name: aghub-skills
description: Domain knowledge and invariants for aghub's skill-management subsystem — npx-compatible lock files, universal vs copy install layout, transactional filesystem mutations, and cross-crate wiring. Use when modifying skill install, update, remove, prune, rename, lock files, or skill discovery anywhere in the aghub repo (crates/skill, crates/core skills/manager, the skills routes in crates/api, or the apply-update CLI).
---

# Working on aghub's skill subsystem

Hard-won, non-obvious rules for the skill code. Terms (Source hash, Master,
Referrer, Relink, …) are defined in [CONTEXT.md](../../../CONTEXT.md) — use them.

## Lock files: two formats, one Source hash

- **Global** lock (`~/.agents/.skill-lock.json`, v3, npx-compatible) and
  **project** lock (`<proj>/skills-lock.json`, v1, intentionally timestamp-free).
- The v3 entry holds a **mutual-exclusion invariant**: a Source hash lives in
  `contentHash` and `skillFolderHash` is kept **empty** — never both populated.
  The project lock (v1) stores the same Source hash under `computedHash` and has
  no folder-hash field.
- **Never mutate these fields by hand.** Use `SkillLockEntry::apply_content_hash`
  / `LocalSkillLockEntry::apply_computed_hash` (idempotent; the global one also
  bumps `updatedAt`). They are the one home for the invariant.
- **npx compatibility is load-bearing**: never bump the lock versions; keep
  `skillFolderHash` empty; hash parity is fixture-pinned
  (`crates/skill/tests/hash_parity_golden.rs`). See `crates/skill/CLAUDE.md` and
  the [npx-skills-contract](../npx-skills-contract/SKILL.md) skill for the full
  round-trip contract + known divergences.

## Install layout: universal vs copy

- **Universal**: one Master at `.agents/skills/<name>` + per-agent symlink
  Referrers. **Copy**: an isolated per-agent copy, no Master.
- A skill is universal iff `canonical_path.is_some()`.
- **Renaming or removing a Master must account for every Referrer.** Discover
  referrers _before_ the rename (canonicalize resolves through the live Master),
  then re-point them.
- Rename is **transactional**: `rename_skill_master` rolls back on relink failure
  so a partial rename never leaves dangling Referrers. Boundary = rename + relink
  only — see [docs/adr/0001](../../../docs/adr/0001-transactional-universal-skill-rename.md).
  Don't "fix" the SKILL.md write into the transaction; that's deliberate.

## Where things live (don't guess)

- Agent descriptors/models/format → `crates/agents` (NOT `crates/core`).
- All config mutation goes through **`ConfigManager`** (`crates/core/src/manager/`);
  never bypass it. Disk-discovered skills (Claude) barely use `save_current`.
- Adding/removing an agent touches **7 places**: the descriptor, two
  registration spots in `crates/agents`, and four test contracts (three
  `crates/core/tests/mcp_dialect_*`, plus every table in
  `crates/agents/tests/descriptor_regression.rs`). `crates/core/src/registry`
  needs NO edit — `ALL_AGENTS` IS `aghub_agents::agents::ALL_DESCRIPTORS`.
  See the root `AGENTS.md` "Adding / Removing an Agent" checklist.
- Removal/prune logic clusters in `crates/core/src/skills/`; the transactional
  Master **rename** (`rename_skill_master`) lives in
  `crates/core/src/manager/skill.rs`; the lock store is `crates/skill/src/lock/`.

## Testing

- Isolate per-agent paths with `set_skills_path_override` (thread-local) /
  `TestConfig` (`crates/core/src/testing.rs`).
- Skills/project precedence: dedupe by name, **project beats global**.
- Forcing fs failures (rollback, prune-abort, permission paths) → use the
  **testing-fs-failures** skill; gate permission tests to `#[cfg(unix)]` with a
  root probe.
