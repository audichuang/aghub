# 08 — `RemovalOutcome::commit()` is public but does not assert the mutation lock

**Status:** open · low · no current reproduction

Both callers acquire the interprocess mutation guard before calling it, so there
is no live defect. But root `AGENTS.md` is explicit: when a private flow is
promoted to a public seam it must re-assert the preconditions its old callers
guaranteed — a public entry point is only as safe as its own guards. This seam
was extracted on the `cli-agent-ergonomics` branch and did not.

**Fix direction:** take the guard inside `commit`, or take proof of it as a
parameter.

**Found by:** Codex round 4 (non-blocking, source-grounded).
