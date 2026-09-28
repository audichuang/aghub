# aghub-agents history

What a flow in `crates/agents` used to do, and the rule it left behind. The
code comments keep the current rule; this page keeps the story.

## Json map discriminator merged into TransportVocabulary

`json_map` used to carry its own `Option<Discriminator>` — field-for-field the
same as `TransportVocabulary`, with its own `writes_sse` and its own
mixed-entry rule; `None` meant "no transport tag". Two copies invited drift, and
the drift is on the record: the mixed-key rule landed in Grok and had to be
hand-ported to Hermes. Related one-definition consolidations from the same
period:

- `json_map`, Mistral and OpenCode each spelled the "is this tag streamable
  HTTP?" condition out for themselves — three places to forget when a dialect
  gains a word. It is now `TransportVocabulary::reads_http`.
- The universal HTTP read-alias set (`http` / `streamable-http` /
  `streamableHttp`) was hardcoded in `json_map`'s parser; it is now the declared
  `http_read_aliases`.
- Mistral inlined its stdio tag literal at its two call sites, and the two only
  agreed by luck; a dispatching dialect now spells it once in `vocab.stdio`.
- The per-server toggle spelling was hard-coded in the serializer, so
  `enabled` / `disabled` were the only words any `json_map` agent could have.
  ZCode's is `enable`, which is why `ToggleKey` carries its spelling as data.

**Rule**: every MCP-capable agent declares ONE `TransportVocabulary` (the
`json_map` ones inside `json_map::Dialect`); no dialect restates a condition
`mcp_policy` owns. Commits: `05d77281`, `355a0a07`.

## Single remote bool replaced by fact parameters

The shared policy used to take a single `single_remote: bool`. One bit had to
answer four independent questions — is the tag spelled `type` or `transport`,
is SSE spellable at all, which remote keys appear in the mixed-entry message,
and is `streamable-http` readable — and it only fit because the two dialects it
was written for happened to answer all four the same way. The third dialect
broke the collinearity: `json_openclaw` writes `transport: "sse"` yet passed
`single_remote: true`, purely to borrow a message. `toml_mistral` and
`json_opencode` passed the same lie for the same reason.

**Rule**: `mcp_policy` parameters name FACTS, never dialects — a fact cannot be
borrowed like that. Commit: `355a0a07`.

## Mixed entry rule missing in three dialects

`reject_mixed_transport` existed from the first adversarial review, yet the
sixth review found the same half-parsed mixed entry in three dialects at once —
all of which could have called it since the first review. A shared function
nobody is FORCED to call does not propagate.

**Rule**: `crates/core/tests/mcp_dialect_decisions.rs` is the forcing half —
registry-driven, one row per MCP-capable agent (`json_map` agents included).
Commit: `1c0c42b9`.

## Four hand-written roster lists

The roster used to be four separate hand-written lists (the `AgentType` enum,
`ALL`, the string mappings, the descriptor table), which is how "add an agent"
became "update three of them and hope". A variant with no descriptor entry was
served **Claude's** descriptor by `registry::get` — silently, so its MCP
servers landed in `~/.claude.json` and its skills in Claude's directory.

**Rule**: one `agent_roster!` row per agent; `AgentType::descriptor` is a total
`match`, so `registry::get` has no fallback left to reach. The row mistakes the
compiler cannot see are pinned by `crates/core/tests/registry_bijection.rs`.
Commit: `5179cf38`.

## Absent vs unreadable sub agent dir

`load_sub_agents_from_dir` used to return the same empty list for a directory
that was absent and one that existed but could not be read, which turned an
I/O anomaly into a confident `RESOURCE_NOT_FOUND`. The skill loader made the
same mistake, and there it became a silent deletion of a shared master a
genuine holder was still reading.

**Rule**: an existing-but-unreadable directory is `Err`; only a deliberately
refused dir (a symlinked one) stays empty. Commit: `fe0db092`.

## Hand copied env override lists leaked into real configs

Tests leaked into the developer's real agent config three times, in three
different test harnesses — an ambient `OPENCODE_CONFIG_DIR` wrote MCP servers
into a live `opencode.json`; a real `~/.config/orca/...` turned up in an api
test's allow-listed roots — each time because the harness kept its own
hand-copied list of override variables and missed one.

**Rule**: ONE list, in `crates/agents/src/env_overrides.rs`, next to the
descriptors that read it; isolating `$HOME` alone is NOT isolation.
Commit: `3171ae10`.
