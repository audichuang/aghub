# 07 — `doctor` UX debts

**Status:** open · low

Three small ones, all confirmed:

1. `crates/cli/src/commands/doctor.rs:645-651` — doctor's own remediation note
   tells the user to run `delete`, and that `delete` refuses (shared master).
2. :667-678 — `--verify-links` audits the DEFAULT `-a` agent, so a healthy
   NativeReader-only install reports `orphanMaster` and goes red.
3. `crates/core/src/manager/skill.rs:730-735` — the refusal for
   `delete skills -a <NativeReader>` reads as garbled and does not mention
   `--all-agents`, which is the thing that works.

**Found by:** round-4 workflow (LOW, confirmed).
