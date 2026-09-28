# ZCode descriptor notes

Research and chosen defaults behind `crates/agents/src/agents/zcode.rs`.

## MCP dialect

- Servers live under `mcp.servers` in a native `config.json`. `json_map`'s
  `server_key` is a DOTTED path, so the nesting needs no parser.
- The per-server toggle is spelled `enable`, NOT `enabled`: writing `false`
  switches the server off and a server WITHOUT the field counts as ENABLED.
  That one letter is why `ToggleKey` carries its spelling as data — a toggle
  aghub wrote under a name ZCode does not read is a server the user switched off
  that comes back on.

## Two answers the vendor docs do not give

Chosen conservatively; recorded so the next reader knows they were chosen, not
attested.

- **The transport tag.** The docs name stdio, HTTP and SSE but never say which
  field distinguishes them, and the one worked example (stdio) carries no tag at
  all. ZCode inherits `MCP_SERVERS` — `type: stdio | sse | http`, the spelling
  most of aghub's `json_map` agents already use — rather than inventing a key.
- **An untagged remote is streamable HTTP.** `InferSseFromUrl` would make a URL
  with an `/sse/` path segment parse as SSE and the next save write
  `type: "sse"` over it. That heuristic is a guess about a vendor whose docs say
  nothing, so the one transport aghub can read back unchanged wins.

### How to close the transport-tag question

No test in this repo can. `MCP_SERVERS` READS `http`, `streamable-http` and
`streamableHttp` alike, but WRITES `http`, and a save rebuilds EVERY server in
the file — so if ZCode validates a different spelling, one `aghub mcps add`
retags a remote server the user already had working, silently. The family
default has been wrong twice already (`cline` writes `streamableHttp`,
`roocode` writes `streamable-http`, both overriding it).

The probe: put
`{"mcp":{"servers":{"probe":{"type":"http","url":"https://example.test/mcp"}}}}`
in `~/.zcode/cli/config.json`, open a ZCode session and check Settings → MCP
for that server; repeat with `"streamable-http"`. Whichever one connects is the
answer, and the fix is one line in `zcode.rs`:
`vocab: TransportVocabulary { http: "<answer>", ..MCP_SERVERS.vocab }`.

## `.agents/mcp.json` compatibility: deliberately not implemented

ZCode also reads `~/.agents/mcp.json` and `<root>/.agents/mcp.json` under an
`mcpServers` key, but only as a FALLBACK: within a scope, if the `.zcode`
config defines any server at all, the `.agents` file for that scope is skipped
ENTIRELY — no merging. ZCode's own settings panel always writes back to the
`.zcode` native config and never touches `.agents`, and aghub does the same.

The footgun the vendor calls out: a user who keeps their servers only in
`.agents/mcp.json` stops loading ALL of them the moment anything writes one
server into the `.zcode` config — including the first `aghub mcp add`. Reading
both and merging them would write a file ZCode reads differently than aghub
does, so the split stays visible instead.

## Config paths

The depths differ and that is the vendor's, not a typo: the USER config sits
under `.zcode/cli/` (`~/.zcode/cli/config.json`), the WORKSPACE one directly
under `.zcode/` (`<root>/.zcode/config.json`). Both are named `config.json`.

## Skill roots

ZCode reads FOUR skill roots, and the private one comes first at each scope.
Its own `zcode-configuration-guide` gives the discovery order: user
`~/.zcode/skills` → user `~/.agents/skills` → workspace `<root>/.zcode/skills`
→ workspace `<root>/.agents/skills` → plugin roots, and "within a level,
`.zcode` is scanned before `.agents`".

So the WRITE slot is the private `.zcode/skills` — that is what makes a grant
visible to ZCode alone — while `.agents/skills` must still be listed as a READ
path. That second half is not cosmetic: root `AGENTS.md` keeps the whole
shared-slot section because an agent missing from a shared dir's reader set is
an agent `skills::shape::compat_unlink_authorized` does not count, and a
`repair` run for a DIFFERENT agent may then detach a compat Referrer that ZCode
is still reading. Covered reader, not read-only co-reader: ZCode has its own
write slot at both scopes, so the quorum passes on its own coverage.

NOT `universal: true`: that flag appends `$XDG_CONFIG_HOME/agents/skills`,
which ZCode never names. Its shared root is `~/.agents/skills`, spelled out in
the read lists.

Symlinked skill directories are supported by the vendor — importing skills from
other agents that way is a documented feature — which is what aghub's
symlink-only install needs.
