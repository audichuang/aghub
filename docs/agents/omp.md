# Oh My Pi (`omp`) descriptor notes

Binary forensics behind the MCP dialect in `crates/agents/src/agents/omp.rs`.
Oh My Pi is `can1357/oh-my-pi`, a fork of `earendil-works/pi`. Its native MCP
config is an `mcpServers` map tagged `type` — `stdio` | `sse` | `http`.

## Two tag keys read, one validated

omp reads TWO tag keys and validates only ONE, so `type` is the sole spelling
that works everywhere. Its loader prefers `transport`:

```js
function Yge(e) {
  if ("transport" in e && (e.transport === "stdio"|"sse"|"http")) return e.transport;
  if ("type" in e && (e.type === "stdio"|"sse"|"http")) return e.type;
  if ("url" in e && ...) return "http";
  return "stdio";
}
```

but its validator — run by `mcp add`, `mcp update` AND by `connectServers` on
every entry at startup — looks at `type` alone:

```js
function GY(e, t) { const o = t.type ?? "stdio"; ... }
```

So an entry tagged only `transport: "http"` defaults to `stdio`, fails
`stdio server requires "command" field`, and is SKIPPED at connect time rather
than reported. Writing `type` satisfies `GY`, and `Yge` falls through to it.

A hand-written `transport: "sse"` is the one thing aghub cannot see: it reads
as streamable HTTP via the URL fallback. Narrow, and the alternative is a
config omp silently refuses to mount.

## Untagged entries and the toggle

- An untagged entry resolves by which field is present, so an untagged remote
  is streamable HTTP, never SSE.
- The per-server toggle is a native `enabled` bool. omp's own importers
  propagate `enabled: false` from every foreign config they read, so dropping
  it would remount a server the user switched off.
- Fields omp owns that aghub does not (`cwd`, `oauth`, `auth`,
  `requestIdFormat`, `envPolicy`) survive because `json_map` rewrites only the
  transport keys.
