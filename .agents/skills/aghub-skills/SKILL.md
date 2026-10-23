---
name: aghub-skills
description: Invariants for aghub's own skill-subsystem code — lock hash fields, universal vs copy layout, the two rename transactions, and where skill code lives across crates. Use when changing skill install, update, remove, prune, rename or discovery in crates/skill, crates/core or the api skills routes; not for npx lock/hash interop (npx-skills-contract).
---

# Working on aghub's skill subsystem

Terms (Source hash, Master, Referrer, Relink, …) are defined in
[CONTEXT.md](../../../CONTEXT.md) — use them.

## Lock files: two formats, one Source hash

- **Global** lock (`.skill-lock.json`, v3; path rules in `crates/skill/AGENTS.md`)
  and **project** lock (`<proj>/skills-lock.json`, v1, timestamp-free).
- v3 mutual exclusion: the Source hash lives in `contentHash`;
  `skillFolderHash` stays **empty** — never both populated. v1 stores it as
  `computedHash` (no folder-hash field).
- **Never set these fields by hand.** Use `SkillLockEntry::apply_content_hash`
  / `LocalSkillLockEntry::apply_computed_hash` (idempotent; the global one also
  bumps `updatedAt`) — the one home for the invariant.
- **A flow that starts writing a lock entry** (or a new shape of one): fill
  [references/lock-entry-readers.md](references/lock-entry-readers.md)
  against the fetched-install entry for the same repo, and give the reviewer
  the same table.
- Never bump lock versions; hash parity is pinned by
  `crates/skill/tests/it/hash_parity_golden.rs`. Full round-trip contract:
  [npx-skills-contract](../npx-skills-contract/SKILL.md).

## Install layout: universal vs copy

- **Universal**: one Master at `.aghub/<sanitized-name>` + per-agent symlink
  Referrers. **Copy** (isolated): a real per-agent directory, no Master.
- `canonical_path.is_some()` selects the universal rules (`plan_removal`
  dispatches on it alone); it is set only for a link — except the
  `--all-agents` store fallback, which synthesizes it (see `crates/core/AGENTS.md`).
- Renaming or removing a Master must account for every Referrer. Record
  referrers **before** the rename (`canonicalize` resolves through the live
  Master), then re-point them.

## Two rename transactions

- `update_skill` → `rename_skill_master` (`crates/core/src/manager/skill.rs`):
  rolls back on Relink failure. Boundary = rename + relink only
  ([ADR-0001](../../../docs/adr/0001-transactional-universal-skill-rename.md));
  don't pull the later SKILL.md write into it — deliberate.
- Lock-managed rename (reinstall from the fetched source under the new name)
  → `accept_rename` (`crates/core/src/skills/rename.rs`): install new + remove
  old + both lock entries as ONE transaction under the mutation lock, with a
  snapshot rollback. The git fetch stays out (adapter hands in `FetchedRename`).

## Where things live

- Removal/prune/repair/shape: `crates/core/src/skills/` (file map in
  `crates/core/AGENTS.md`); lock store: `crates/skill/src/lock/`.
- Skill mutations persist to disk and the lock, **not** via `save_current()` —
  `save()` serializes MCPs only and rewriting them drops fields aghub does not
  model ([history](../../../docs/history/core-manager.md#skill-mutations-rewrote-mcp-config)).
- Adding an agent: `crates/agents/AGENTS.md` "ADDING / REMOVING AN AGENT".

## Testing

- Path overrides, `TestConfig`, `$HOME` isolation and the one-env-mutex rule:
  `crates/core/AGENTS.md` Testing. Lock tests: `TestLockGuard::new()`
  (`crates/skill/AGENTS.md`).
- Forcing fs failures (rollback, prune-abort, permission paths) → the
  **testing-fs-failures** skill.
