# 01 — Saving any sub-agent drops every unmodeled frontmatter key from all of them

**Status:** open · pre-existing · deferred from the v2.15.0 release

**What happens:** `SubAgentFrontmatter` (`crates/agents/src/sub_agents.rs:20-24`)
models only `name` + `description`. `format_sub_agent` (:74) re-serializes from
that model, and the save-all loop (:337) rewrites EVERY sub-agent in the target
directory — so a `transfer`/`reconcile` naming one sub-agent silently strips
`tools`, `model`, `color` and anything else from all of its siblings.

**Why it is not a patch:** this is the same class the 2026-08 MCP dialect audit
ruled on — _a value the model cannot hold must be REFUSED, not approximated_.
The fix is either round-tripping unknown keys (a parse/serialize change across
the sub-agent format) or refusing to save a file whose frontmatter carries keys
aghub does not model. Both are design decisions.

**Why it matters for THIS release:** v2.15.0 claims the sub-agent
`transfer`/`reconcile` flows are now safe against data loss. They are — against
the _whole-file_ loss the guards close. This is _field-level_ loss on the same
flows, and the release notes should say so rather than let the claim over-read.

**Found by:** round-4 workflow (HIGH, confirmed and reproduced).
