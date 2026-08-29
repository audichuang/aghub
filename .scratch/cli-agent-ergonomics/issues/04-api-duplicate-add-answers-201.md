# 04 — `POST /agents/<agent>/skills` answers 201 Created on a duplicate add

**Status:** open · pre-existing · API-only

`crates/api/src/routes/skills.rs:1042` returns 201 with
`already_installed: false`, echoing the request back, when the skill is already
present. The CLI's own comment on the equivalent path says this was the defect
fixed on its side; the API surface never got it.

**Fix direction:** route it through the same `SkillAdd` contract the CLI reads —
report the skill as it exists on disk, with `already_installed` telling the
truth.

**Found by:** round-4 workflow (HIGH, source-grounded).
