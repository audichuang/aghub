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
//! on key types, null semantics, and SSE/HTTP handling), so the logic is
//! duplicated deliberately — a generic `ConfigDoc` trait would be a leaky ~200-
//! line seam hiding little. Keep their invariants in sync by hand for now.
//! **Extract a shared preserve/merge policy (or a shared contract-test harness
//! first) when a 3rd strict dialect appears, or the next cross-dialect invariant
//! change causes drift.**

pub mod json_list;
pub mod json_map;
pub mod json_opencode;
pub mod toml_format;
pub mod toml_grok;
pub mod yaml_hermes;
