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
//! These two share a contract but NOT a `Value` type (serde_yaml vs toml differ
//! on key types, null semantics, and SSE/HTTP handling). The drift trigger the
//! old note set — "the next cross-dialect invariant change causes drift" —
//! fired (the mixed-key rule landed in Grok, then had to be hand-ported to
//! Hermes), so the drift-prone INVARIANTS now live in [`transport_policy`]:
//! `reject_mixed_transport` (mixed stdio/remote keys), `remote_transport` (the
//! `url`→Sse/StreamableHttp `type` split), `missing_transport_error`, and the
//! serialize decision (`transport_keys` + `transport_fields`, over the neutral
//! `FieldValue`). Each dialect keeps its own SYNTAX and phase order (validate
//! `enabled` → reject mixed on key presence → dispatch on presence → extract the
//! chosen branch → build), so error precedence is byte-identical to before.
//! This is NOT a `ConfigDoc` trait abstracting the whole document — the shared
//! surface is a handful of pure functions over primitives + `FieldValue`.
//! `tests/format_tests.rs` carries the cross-dialect contract test that fails if
//! either dialect drifts.

pub mod json_list;
pub mod json_map;
pub mod json_openclaw;
pub mod json_opencode;
pub mod toml_format;
pub mod toml_grok;
pub mod toml_mistral;
pub mod transport_policy;
pub mod yaml_hermes;
