# CLI agent-operability audit — deferred findings

The `cli-agent-ergonomics` branch fixed the CLI's agent-operability defects and,
across five rounds of independent adversarial review (a Claude workflow and
Codex alternating, each catching what the other missed), **four separate silent
data-loss paths** in `reconcile`. Every one had the same signature: exit 0,
"N succeeded, 0 failed", resource gone from every agent.

Those are fixed and shipped. The review also surfaced defects that are
**pre-existing and out of that scope** — they are not made worse by this branch,
and each is a design decision rather than a patch. They are recorded here so the
release can name what it is knowingly not fixing.

## What the branch DID fix (for context, not tracked here)

- `reconcile` shared-backing: a removal may not take the resource from a copy
  target or from the SOURCE — one guard, all three resource kinds, checked both
  before any write and again at delete time
- the skill copy must PROVE the source content landed before a paired removal
- `add_skill_universal` / `add_skill_from_path_universal` now answer
  "already installed?" the same way, from the resolved path
- `RemovalKind::Partial` no longer reports `success: true`
- `add` reports the skill as it is ON DISK, not the source it just parsed
- a refused copy rolls back the referrer it created
- the reconcile PREVIEW raises the same refusals the commit does

## Verification standard used

Every fix has a regression test proved by reverting the fix and watching the
assertion go red (root `AGENTS.md`, Testing). `just preflight` green.
