# 02 — Saving a renamed sub-agent leaves the file it was loaded from

**Status:** open · pre-existing

`save_sub_agent_to_dir` (`crates/agents/src/sub_agents.rs:218-224`) writes to
`sanitize_filename(name).md` without removing the file the sub-agent was loaded
from. Rename it and the directory holds both — the old one still loads, so the
agent sees two.

**Fix direction:** carry the loaded `source_path` into save and unlink it when
the computed filename differs, the way the skill rename transaction does.

**Found by:** round-4 workflow (HIGH, confirmed).
