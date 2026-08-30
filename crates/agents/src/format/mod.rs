//! MCP config (de)serializers, one per agent config dialect.
//!
//! Most agents reuse `json_map` (a `mcpServers` map). A bespoke module is only
//! needed when an agent stores MCP inside a large shared document whose other
//! keys must survive a rewrite, or with a dialect the JSON path can't express.
//!
//! `json_map` rebuilds the managed server map and omits newly disabled servers
//! only when a dialect has no native toggle field; existing disabled entries
//! are retained during unrelated rewrites.
//! `yaml_hermes` (Hermes) and `toml_grok` (Grok) are the strict *preserve-and-
//! merge* pair: they keep every other document key AND every unowned per-server
//! field, reject malformed input instead of coercing, remove transport keys
//! before re-inserting, keep disabled servers (`enabled: false`), and reject
//! entries mixing stdio (command/args/env) with remote (url/headers[/type]).
//!
//! No two dialects share a `Value` type (serde_json vs serde_yaml vs toml differ
//! on key types, null semantics, comment retention and numeric fit), so each
//! keeps its own engine. What the seven HAND-WRITTEN dialects share is the
//! ANSWERS to a handful of questions, and those live in [`mcp_policy`] as DATA
//! each of them declares: a `RemoteVocabulary` (which remote words it has — an
//! empty SSE spelling is what `refuse_unwritable` turns into a refusal, so no
//! dialect restates the CONDITION, though each still has to call it), an
//! `OwnedKeys` (which keys a transport owns), plus `reject_mixed_transport`,
//! `remote_transport`, `missing_transport_error` and `transport_fields` over the
//! neutral `FieldValue`. The 16 `json_map` agents answer the same questions
//! through [`json_map::Dialect`] and its `Discriminator` instead — a separate,
//! deliberately untouched copy, because one parse/serialize pair already serves
//! all 16 and merging the two would rewrite 16 agents' error text for tidiness.
//! Each dialect keeps its own SYNTAX and phase order
//! (validate `enabled` → reject mixed on key presence → dispatch on presence →
//! extract the chosen branch → build), so error precedence is byte-identical to
//! before the extraction. This is NOT a `ConfigDoc` trait abstracting the whole
//! document — the shared surface is a handful of pure functions over primitives.
//!
//! A shared function nobody is FORCED to call does not propagate: the mixed-key
//! rule existed from the first review and was still found missing in three
//! dialects at the sixth. `crates/core/tests/mcp_dialect_decisions.rs` is the
//! forcing half — registry-driven, one row per MCP-capable AGENT (`json_map`
//! agents included, so adding one needs a row even though it adds no dialect) —
//! and `tests/format_tests.rs` carries the cross-dialect contract test.

pub mod json_map;
pub mod json_openclaw;
pub mod json_opencode;
pub mod mcp_policy;
pub mod toml_format;
pub mod toml_grok;
pub mod toml_mistral;
pub mod yaml_hermes;
