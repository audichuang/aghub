# 09 — The reconcile preview does not run the content-landing refusal

**Status:** open · low · known gap, deliberately deferred

`crates/cli/src/commands/transfer.rs` runs three read-only preflights before
reporting a plan: the source exists, the add/remove sets are disjoint, and
`ensure_*_reconcile_spares`. The fourth refusal a `--yes` run can raise — the
content-landing proof in `crates/core/src/transfer.rs` — has no preview seam, so
a plan that will be refused on commit still previews green.

**Why it was not hoisted:** the proof keys on `added.wrote_master`, a RETURN
VALUE of the mutation. A preview would have to simulate copy ordering (the first
`--add` materializes the master; later ones legitimately do not write it) to
avoid refusing a legitimate multi-target plan. That is a real design problem, not
an oversight to patch.

**Why it is low:** nothing is lost. The commit fails closed and atomically (the
staged gate keeps the delete off), exits 1, rolls back the referrer it created,
and its message names the fix ("Reconcile the master first, or drop the
--remove"). The cost is one failed command for a caller that previewed first.

The preview was never an exhaustive oracle — a purely static capability refusal
(`reconcile sub-agent --add cursor`, "Cannot copy sub-agent for cursor agent")
is also commit-only, and `7a09af41` established that the commit-time re-check is
the real guard because the preflight is only a snapshot.

**Found by:** round-5 workflow (LOW, confirmed, and argued down from the
claimant's own MEDIUM by the verifier).
