# Transactional universal-skill rename

When `update_skill` renames a Universal install's Master, the `fs::rename` of the
Master directory and the Relink of its Referrers are treated as one transaction:
if the Relink fails, we roll back — rename the Master back to its old name and
restore the old-name symlinks — so a partial failure can never leave dangling
Referrers. We chose rollback over forward-recovery because the pre-rename state is
the only one we can reconstruct from data captured before the rename (the Referrer
list and old name).

## Considered Options

- **No transaction (status quo).** A Relink failure after `fs::rename` left the
  Master renamed but Referrers dangling and the agent config un-updated. Rejected —
  this is the bug being fixed.
- **Wider boundary (include the SKILL.md write and `save_current`).** Rejected: once
  the rename + Relink succeed the filesystem is already consistent, so a later
  content-write or config-save failure is low-harm and surfaces as a plain error;
  rolling back a successful Relink across N agents would add risk for little gain.

## Consequences

- The rollback scope is deliberately **rename + relink only**. A reader who sees the
  SKILL.md write _not_ participating in the transaction should not "fix" that — it is
  intentional.
- Rollback is best-effort. If rollback itself fails, the operation returns a
  **compound error** naming both the original Relink failure and the rollback
  failure (plus the affected Master path), signalling that manual recovery is needed
  rather than silently leaving a worse state.
