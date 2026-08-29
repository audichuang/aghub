# 03 — API skill import overwrites a git lock entry as `local` and records a hash it never installed

**Status:** open · pre-existing · API-only

`crates/api/src/routes/skills.rs:1145` stamps the lock with
`sourceType: "local"` and the hash of the content it _parsed_, over an existing
entry whose source is a git repo — the route's own comment forbids exactly this.
Downstream, `check`/`source sync` then believe a git-backed skill is a local one
at a hash that was never installed.

**Related:** the root cause it escalated (an install reporting the parsed source
as if it had landed) IS fixed on the `cli-agent-ergonomics` branch; this route
still needs its own guard, because a public seam is only as safe as its own
preconditions (root `AGENTS.md`).

**Found by:** round-4 workflow (HIGH, source-grounded).
