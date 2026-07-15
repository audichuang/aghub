//! MCP config (de)serializers, one per agent config dialect.
//!
//! Most agents reuse `json_map` (a `mcpServers` map). A bespoke module is only
//! needed when an agent stores MCP inside a large shared document whose other
//! keys must survive a rewrite, or with a dialect the JSON path can't express.
//!
//! `json_map` rebuilds the server map wholesale and omits disabled servers.
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
//! Hermes), so the shared transport SEMANTICS now live in
//! [`transport_policy`]: mixed-key rejection, command/url dispatch, the `type`
//! sse/http split, the `enabled` default, and the serialize key/value choice.
//! Each dialect owns ONLY its syntax (extract fields from its `Value`, write
//! `Value`s back preserving unowned keys). This is NOT a `ConfigDoc` trait — the
//! boundary is the neutral `RawServer`/`FieldValue` DTOs. `format_tests.rs`
//! carries the cross-dialect contract test that fails if either dialect drifts.

pub mod json_list;
pub mod json_map;
pub mod json_opencode;
pub mod toml_format;
pub mod toml_grok;
pub mod transport_policy;
pub mod yaml_hermes;
