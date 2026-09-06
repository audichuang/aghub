# 01 — Saving any sub-agent drops every unmodeled frontmatter key from all of them

**Status:** fixed (2026-09-06, `wip: agent descriptor corrections + omp`)

**Fixed by round-tripping the unknown keys** — the first of the two designs
below. Two mechanisms, in this order: `SubAgent::extra_frontmatter` carries the
keys aghub does not model from load to save (`SubAgentFrontmatter` deserializes
them through a `#[serde(flatten)]` `serde_yaml::Mapping`), so the save-all loop
re-emits each sibling's own keys — and a rename keeps the renamed agent's keys
too, because they ride the in-memory model rather than the destination file.
When the model carries none — an API create DTO, or an in-place edit of an entry
aghub never loaded — `save_sub_agent_to_dir_with` falls back to reading the
destination file (`unowned_frontmatter`). The model's own keys win over the
file's.

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

**Found by:** round-4 workflow (HIGH, confirmed and reproduced). Codex round 5
re-raised it as mis-triaged, on the grounds that this branch introduced the CLI
`transfer`/`reconcile` sub-agent surface that makes the lossy save-all path
reachable. That premise is wrong — `git show main:crates/cli/src/commands/transfer.rs`
carries `TransferAction::SubAgent` and `ReconcileAction::SubAgent` already — so
the triage stands. Its point about severity does not depend on the premise and
is accepted: this is silent FIELD-level data loss on a flow this release
otherwise claims to have made safe, which is why it is called out in the release
note rather than left in this file alone.
