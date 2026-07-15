# Deepen the strict preserve-merge MCP dialects behind a neutral transport policy

**Status**: proposed → implementing on `refactor/codebase-deepening`
**Scope**: candidate B from the 2026-07-15 architecture review.

## Problem

`toml_grok.rs` (Grok) and `yaml_hermes.rs` (Hermes) are the two strict
"preserve-and-merge" MCP dialects. They share a **contract** — reject malformed
input, reject entries mixing stdio (`command`/`args`/`env`) with remote
(`url`/`headers`[/`type`]) keys, default `enabled` to true, keep disabled
servers, remove transport keys before re-inserting, preserve every other
document + per-server key — but each carries its own full copy of the
**transport semantics** because the `Value` types differ (`toml::Value` vs
`serde_yaml::Value`).

`format/mod.rs` documents this duplication as deliberate, with a trigger:
_"Extract a shared preserve/merge policy (or a shared contract-test harness
first) when a 3rd strict dialect appears, or the next cross-dialect invariant
change causes drift."_ **That trigger has fired**: the mixed-key rejection
landed in Grok first (`e6a827b0`), then had to be hand-ported to Hermes to align
(`9a83a9fe`) — the exact drift the note predicted.

## Solution — a neutral policy, not a `ConfigDoc` trait

The team's stated worry was a "generic `ConfigDoc` trait" — a leaky ~200-line
seam abstracting the whole document. This design avoids that. The seam is a pair
of **neutral DTOs** carrying only the transport fields; the deep module is the
**transport policy** (the semantics that drift). The `Value`-typed work (field
extraction, `Value` construction, preserve-merge) stays per-dialect — exactly
where the team wanted it.

New `crates/agents/src/format/transport_policy.rs`:

```rust
/// Neutral extraction of one server's transport-relevant fields. The dialect
/// adapter fills this from its own Value type; the policy never sees TOML/YAML.
pub struct RawServer {
    pub name: String,
    pub enabled: Option<bool>,
    pub has_stdio_key: bool,   // any of command/args/env present
    pub has_remote_key: bool,  // any of url/headers/type present
    pub command: Option<String>,
    pub args: Vec<String>,
    pub env: Option<HashMap<String, String>>,
    pub url: Option<String>,
    pub headers: Option<HashMap<String, String>>,
    pub type_key: Option<String>,   // Grok only; Hermes passes None
}

/// The SHARED invariants: reject mixed keys, dispatch command→Stdio /
/// url→remote (type→Sse/StreamableHttp when !single_remote), enabled default.
/// `dialect` names the agent in error text; `single_remote` = Hermes.
pub fn raw_to_mcp(raw: RawServer, dialect: &str, single_remote: bool)
    -> Result<McpServer>;

/// A native-agnostic field value the adapter converts to its Value type.
pub enum FieldValue { Str(String), List(Vec<String>), Map(HashMap<String,String>) }

/// The SHARED serialize decision: which transport keys to write, and their
/// values, for a given transport. (Grok Sse → +`type="sse"`; StreamableHttp and
/// all single_remote → no `type`.) `enabled` stays the adapter's one-liner.
pub fn transport_fields(t: &McpTransport, single_remote: bool)
    -> Vec<(&'static str, FieldValue)>;

/// The transport-owned keys to strip before re-inserting (6 for Grok / type,
/// 5 for Hermes). Keyed by `single_remote`.
pub fn transport_keys(single_remote: bool) -> &'static [&'static str];
```

`toml_grok.rs` and `yaml_hermes.rs` become **thin syntax adapters**:

- **parse**: walk the container (`mcp_servers`), extract each server into a
  `RawServer` (the only dialect-specific, `Value`-typed work — including the
  per-field "must be a string/array/table" type errors), then call
  `raw_to_mcp(raw, "Grok"/"Hermes", single_remote)`.
- **serialize**: parse the existing document, and for each `McpServer` clone the
  existing entry (preserving unowned keys), remove `transport_keys(..)`, then set
  each `transport_fields(..)` pair (converting `FieldValue` → the native
  `Value`) plus the `enabled` bool.

## Interface (the test surface)

`raw_to_mcp` + `transport_fields` are pure (neutral in, neutral/`McpServer`
out) — the semantics tested once, directly. Depth: the mixed-key rule, the
command/url/type dispatch, the enabled default, and the serialize key-set live
behind two pure functions; the dialects only translate syntax.

## Dependency category

**In-process** (pure). No adapter/port — the neutral DTO boundary is internal.

## Tests

- `transport_policy.rs` unit tests: mixed-key rejection, command→Stdio,
  url→StreamableHttp, `type="sse"`→Sse (only when `!single_remote`), unknown
  `type` error, `single_remote` collapses Sse+Http to one remote, enabled
  default, and `transport_fields` round-trip per variant.
- A **shared contract test** parameterized over both dialects' `parse`/
  `serialize`: rejects mixed keys, keeps a disabled server, preserves an
  unrelated top-level key and an unowned per-server field, round-trips. This is
  the harness `format/mod.rs` asked for — it fails if either dialect drifts.
- The existing per-dialect tests stay as-is: they are the behaviour-preserving
  safety net for the rewrite.

## Non-goals

- No `ConfigDoc`/whole-document trait. No change to the lenient dialects
  (`json_map`, `toml_format`) — their omit-disabled / lenient-continue semantics
  are deliberately different and out of scope.
- No behaviour change: byte-identical parse/serialize output AND identical
  error behaviour, including precedence on malformed input (the existing
  per-dialect tests + the contract test pin this).

## Codex-review follow-up (2026-07-16)

The first cut extracted every field into a neutral `RawServer` before the shared
dispatch. Codex caught that this changed the ERROR PRECEDENCE on malformed input
(a server both mixing families AND carrying a wrong-typed field reported the
type error instead of the mixed-key error; a lone malformed `args` reported a
type error instead of "neither command nor url"). Fixed by keeping each
dialect's original phase order — validate `enabled` → `reject_mixed_transport`
on key PRESENCE → dispatch on presence → extract ONLY the chosen branch — and
sharing just the three invariant helpers (`reject_mixed_transport`,
`remote_transport`, `missing_transport_error`) plus the serialize pair
(`transport_keys` / `transport_fields`). The contract test gained a remote
server per dialect, exact `type` assertions (Grok writes `type="sse"`; Hermes
never writes `type`), and a mixed-key-precedence assertion.

## ADR / decision note

Updates the deliberate-duplication note in `format/mod.rs`: the drift trigger
fired, so the shared policy now exists; the note becomes "semantics shared via
`transport_policy`; only syntax is per-dialect."

## Wins

- **locality**: the mixed-key / dispatch / enabled invariants live once — a
  future change lands in one place, not two hand-synced copies.
- **leverage**: two dialects (and any 3rd strict dialect) reuse one policy.
- **interface is the test surface**: the semantics get direct pure-function
  tests + a cross-dialect contract test that catches drift.
- reframes the team's "leaky seam" concern: the boundary is a thin neutral DTO,
  not a document-abstracting trait.

## Rollback

Revert the phase commit. No lock/on-disk/DTO changes; the config file formats
are unchanged (pinned by the retained per-dialect tests).
