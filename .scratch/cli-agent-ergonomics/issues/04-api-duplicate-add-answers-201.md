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

## Update — the CLI/API gap WIDENED this round

`0e1625d3` fixed the CLI side to report the skill as it exists on disk. The API's
`create_skill` route still builds its response from the REQUEST body before
calling the manager (`crates/api/src/routes/skills.rs:1040-1048`), so the two
surfaces now disagree about the same operation rather than being wrong together.
Fixing this route is the same one-line change the CLI took: build the response
from what `add_skill*` returned.

**Found by:** round-5 workflow (LOW, source-grounded).
