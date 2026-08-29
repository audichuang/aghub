# 08 — `RemovalOutcome::commit()` is public but does not assert the mutation lock

**Status:** open · low · no current reproduction

Both callers acquire the interprocess mutation guard before calling it, so there
is no live defect. But root `AGENTS.md` is explicit: when a private flow is
promoted to a public seam it must re-assert the preconditions its old callers
guaranteed — a public entry point is only as safe as its own guards. This seam
was extracted on the `cli-agent-ergonomics` branch and did not.

**Fix direction:** take the guard inside `commit`, or take proof of it as a
parameter.

**Related and FIXED:** Codex round 5 found the same class with a live window —
the reconcile skill Copy arm ran its content proof and its referrer rollback
AFTER `add_skill_from_path` had released the manager's internal guard, so a
rollback could unlink a referrer another aghub process had just recreated. That
arm now holds one `mutation_guard` across check → write → rollback (it is
reentrant, so the inner one costs nothing). `RemovalOutcome::commit()` still has
no guard of its own.

**Found by:** Codex round 4 (non-blocking, source-grounded).
