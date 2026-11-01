---
name: adding-an-agent
description: The descriptor, registration and contract spots for adding or removing a coding agent in aghub, and the test sentinels that break when an existing agent gains a capability. Use when adding or removing an agent, or opening skills, MCP or sub-agent support on an existing one; not for per-agent dialect details (crates/agents/AGENTS.md).
---

# Adding or removing an agent

One agent = one **descriptor** file plus six registration and contract spots. A
step-2 row naming a module with no `pub mod`, and a missing step-6 row, fail the
BUILD. A missing step-7 asset fails no build, lint or typecheck — only
`just preflight`'s desktop unit tests catch it.

1. `crates/agents/src/agents/<name>.rs` — descriptor — declares the MCP binding
   (two paths + parse/serialize; all `None` when the agent has no MCP) and
   writes no MCP load/save fns. Scope support is NOT declared: it is derived from
   which bindings are `Some` (MCP paths, skill write slots, sub-agent dirs), pinned by
   `descriptor_regression::scope_support_matches_bindings` (naming gotchas: `crates/agents/AGENTS.md` AGENT-SPECIFIC GOTCHAS)
2. `crates/agents/src/agents/mod.rs` — `pub mod`, **and ONE `agent_roster!`
   row**: `Variant => "id", module, ["alias", …];`. That macro emits the
   `AgentType` enum, `ALL`, `as_str`, `FromStr`, `AgentType::descriptor` and
   `ALL_DESCRIPTORS` — there is no second list, and no `AgentType` edit in
   `models.rs` (it re-exports). **Row position is the order of everything**: the
   desktop agent list, `-a all` expansion, batch row order and first-error all read it.
3. `crates/core/tests/mcp_dialect_golden.rs` — a `row!` naming what the agent
   writes and how it reads a config aghub did not write. REQUIRED for any agent
   claiming MCP support
4. `crates/core/tests/mcp_dialect_decisions.rs` — a second `row!`: a mixed entry,
   an unknown transport tag, a field the model does not own, a value that does
   not fit, an SSE server it cannot spell. Required even for a `json_map` agent
   that introduces no new dialect
5. `crates/core/tests/mcp_dialect_roundtrip.rs` — `NATIVE_TOGGLE`, if the agent
   has a native enabled/disabled flag. Exhaustive BOTH ways: a listed agent that
   drops a disabled server fails, an unlisted one that keeps it fails too
6. `crates/agents/tests/descriptor_regression.rs` — a row in **every** table.
   Lengths are derived from `AgentType::ALL.len()`, so a missing row is a COMPILE
   error and a row naming the WRONG agent is a runtime panic.
   `test_global_data_dirs` is the one table an agent may sit out, and only by
   joining `OS_CONFIG_DIR_AGENTS` **and getting its own arm after the loop** —
   membership alone is a `panic!`, because an agent added to the exception list
   and never asserted was how that table stopped checking anything
7. `crates/desktop/src/assets/agent/<id>.svg` — `src/lib/agent-icons.tsx` globs
   `../assets/agent/*.svg` and keys it by `${id}.svg`; a missing file silently
   falls back to a first-letter avatar. `crates/desktop/src/lib/agent-icons.test.ts`
   closes that (it parses `agent_roster!` and asserts the asset exists) and
   `just preflight` runs it. An id with a dash may ship either spelling — the
   lookup has an `id.replaceAll("-", "_")` fallback

`crates/core/tests/registry_bijection.rs` covers the three row mistakes the
compiler cannot: a row naming the wrong MODULE, two rows sharing an ID, and a
row whose id literal drifts from the `id:` field inside `agents/<module>.rs`.

**Opening a capability on an EXISTING agent has a hidden blast radius**: tests
across `cli`, `core` and `api` pick some agent that does not support skills as
their "unsupported target" sentinel. Give that agent the capability and those
tests stop testing anything. Grep the agent's id across `crates/*/tests/` and
`crates/api/src/routes/` BEFORE changing its capabilities, and move the sentinel
rather than deleting the assertion.

## Crate-level detail

The descriptor is
`pub const DESCRIPTOR: AgentDescriptor = …`, and the roster it joins is
`agents::ALL_DESCRIPTORS` in `crates/agents` (`core`'s `ALL_AGENTS` is that same
const).
Skill dirs are declared per scope as `GlobalSkillPaths` / `ProjectSkillPaths { write, also_reads }` — see the Descriptors bullet in `crates/agents/AGENTS.md`.
