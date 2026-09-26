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
//! keeps its own engine. What ALL 23 MCP-capable agents share is the ANSWERS to
//! a handful of questions, and those live in [`mcp_policy`] as DATA each dialect
//! declares: a `TransportVocabulary` (which word it has for each transport — an
//! empty SSE spelling is what `refuse_unwritable` turns into a refusal, so no
//! dialect restates the CONDITION, though each still has to call it), an
//! `OwnedKeys` (which keys a transport owns), plus `reject_mixed_transport`,
//! `remote_transport`, `missing_transport_error` and `transport_fields` over the
//! neutral `FieldValue`. The seven hand-written dialects declare a vocabulary
//! each; the 16 `json_map` agents declare one INSIDE their
//! [`json_map::Dialect`], which is the same type — it used to be a second
//! declaration (`Discriminator`, field-for-field the same, plus its own
//! `writes_sse` and its own mixed-entry rule). What stays split is only the
//! WORDING: `MixedWording::CommandAndUrl` keeps the sentence 16 agents' users
//! already see, because unifying the text is a user-visible change, not a merge.
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

fn has_unrepresented_shape(
	raw: &serde_json::Value,
	canonical: &serde_json::Value,
) -> bool {
	use serde_json::Value;
	match (raw, canonical) {
		(Value::Object(raw), Value::Object(canonical)) => {
			raw.iter().any(|(key, value)| {
				canonical
					.get(key)
					.is_none_or(|known| has_unrepresented_shape(value, known))
			})
		}
		(Value::Array(raw), Value::Array(canonical)) => {
			raw.len() > canonical.len()
				|| raw
					.iter()
					.zip(canonical)
					.any(|(value, known)| has_unrepresented_shape(value, known))
		}
		(Value::Object(_) | Value::Array(_), _) => true,
		_ => false,
	}
}

/// Detect native server fields that the normalized MCP model cannot copy.
pub fn unmanaged_mcp_source_fields(
	server: &crate::models::McpServer,
	original: &str,
	serialize: fn(
		&crate::models::AgentConfig,
		Option<&str>,
	) -> crate::Result<String>,
) -> crate::Result<bool> {
	use aghub_json::parse_jsonc_opt;
	use serde_json::Value;

	#[derive(Clone, Copy)]
	enum Format {
		Json,
		Toml,
		Yaml,
	}

	fn parse(format: Format, text: &str) -> crate::Result<Value> {
		let invalid =
			|error: String| crate::errors::ConfigError::InvalidConfig(error);
		match format {
			Format::Json => parse_jsonc_opt(text)
				.map_err(|error| invalid(error.to_string()))?
				.ok_or_else(|| invalid("empty MCP document".to_string())),
			Format::Toml => {
				let value: toml::Value = toml::from_str(text)
					.map_err(|error| invalid(error.to_string()))?;
				serde_json::to_value(value)
					.map_err(|error| invalid(error.to_string()))
			}
			Format::Yaml => serde_yaml::from_str(text)
				.map_err(|error| invalid(error.to_string())),
		}
	}

	#[derive(Clone)]
	enum Step {
		Key(String),
		Index(usize),
		Named(String),
	}

	fn server_path(value: &Value, name: &str) -> Option<Vec<Step>> {
		fn visit(value: &Value, name: &str, path: &mut Vec<Step>) -> bool {
			match value {
				Value::Object(map) => {
					for (key, child) in map {
						path.push(Step::Key(key.clone()));
						if visit(child, name, path) {
							return true;
						}
						path.pop();
					}
					if map.get(name).is_some_and(Value::is_object) {
						path.push(Step::Key(name.to_string()));
						return true;
					}
					false
				}
				Value::Array(items) => {
					if items.iter().any(|item| {
						item.get("name").and_then(Value::as_str) == Some(name)
					}) {
						path.push(Step::Named(name.to_string()));
						return true;
					}
					for (index, child) in items.iter().enumerate() {
						path.push(Step::Index(index));
						if visit(child, name, path) {
							return true;
						}
						path.pop();
					}
					false
				}
				_ => false,
			}
		}
		let mut path = Vec::new();
		visit(value, name, &mut path).then_some(path)
	}

	fn entry<'a>(
		value: &'a Value,
		path: &[Step],
	) -> crate::Result<&'a serde_json::Map<String, Value>> {
		path.iter()
			.try_fold(value, |node, step| match step {
				Step::Key(key) => node.get(key),
				Step::Index(index) => node.get(*index),
				Step::Named(name) => node.as_array()?.iter().find(|item| {
					item.get("name").and_then(Value::as_str) == Some(name)
				}),
			})
			.and_then(Value::as_object)
			.ok_or_else(|| {
				crate::errors::ConfigError::InvalidConfig(
					"cannot verify native MCP server fields".to_string(),
				)
			})
	}

	let config = crate::models::AgentConfig {
		mcps: vec![server.clone()],
		skills: vec![],
		sub_agents: vec![],
	};
	let canonical = serialize(&config, None)?;
	let format = [Format::Json, Format::Toml, Format::Yaml]
		.into_iter()
		.find(|format| parse(*format, &canonical).is_ok())
		.ok_or_else(|| {
			crate::errors::ConfigError::InvalidConfig(
				"cannot inspect MCP source format".to_string(),
			)
		})?;
	let canonical = parse(format, &canonical)?;
	let path = server_path(&canonical, &server.name).ok_or_else(|| {
		crate::errors::ConfigError::InvalidConfig(
			"cannot locate native MCP server".to_string(),
		)
	})?;
	let raw = parse(format, original)?;
	let preserved = parse(format, &serialize(&config, Some(original))?)?;
	let canonical_entry = entry(&canonical, &path)?;
	let raw_entry = entry(&raw, &path)?;
	let preserved_entry = entry(&preserved, &path)?;
	Ok(preserved_entry != canonical_entry
		|| raw_entry.iter().any(|(key, value)| {
			canonical_entry
				.get(key)
				.is_none_or(|known| has_unrepresented_shape(value, known))
		}))
}

#[cfg(test)]
mod native_source_tests {
	use super::*;
	use crate::models::{McpServer, McpTransport};

	#[test]
	fn nested_native_fields_and_object_to_scalar_changes_are_unrepresented() {
		let canonical =
			serde_json::json!({"transport":{"type":"stdio"}, "enabled":true});
		assert!(has_unrepresented_shape(
			&serde_json::json!({"transport":{"type":"stdio","native":{"mode":"strict"}}}),
			&canonical,
		));
		assert!(has_unrepresented_shape(
			&serde_json::json!({"transport":{"type":{"native":"stdio"}}}),
			&canonical,
		));
		assert!(!has_unrepresented_shape(
			&serde_json::json!({"transport":{"type":"stdio"}}),
			&canonical,
		));
	}

	#[test]
	fn native_extra_guard_covers_json_toml_and_yaml_without_refusing_clean_servers(
	) {
		let server = McpServer::new("srv", McpTransport::stdio("run", vec![]));
		for (descriptor, clean, extra) in [
			(
				&crate::agents::cursor::DESCRIPTOR,
				r#"{"mcpServers":{"srv":{"type":"stdio","command":"run"}}}"#,
				r#"{"mcpServers":{"srv":{"type":"stdio","command":"run","oauth":{"clientId":"x"}}}}"#,
			),
			(
				&crate::agents::codex::DESCRIPTOR,
				"[mcp_servers.srv]\ncommand = \"run\"\n",
				"[mcp_servers.srv]\ncommand = \"run\"\nsampling = true\n",
			),
			(
				&crate::agents::hermes::DESCRIPTOR,
				"mcp_servers:\n  srv:\n    command: run\n",
				"mcp_servers:\n  srv:\n    command: run\n    sampling: true\n",
			),
			(
				&crate::agents::mistral::DESCRIPTOR,
				"[[mcp_servers]]\nname = \"srv\"\ntransport = \"stdio\"\ncommand = \"run\"\n",
				"[[mcp_servers]]\nname = \"other\"\ntransport = \"stdio\"\ncommand = \"other\"\n[[mcp_servers]]\nname = \"srv\"\ntransport = \"stdio\"\ncommand = \"run\"\nrequired = true\n",
			),
		] {
			let serialize = descriptor.mcp_serialize_config.unwrap();
			assert!(!unmanaged_mcp_source_fields(&server, clean, serialize).unwrap(), "{} clean", descriptor.id);
			assert!(unmanaged_mcp_source_fields(&server, extra, serialize).unwrap(), "{} native extra", descriptor.id);
		}
	}

	#[test]
	fn native_extra_guard_accepts_every_dialects_own_clean_output() {
		let server = McpServer::new("srv", McpTransport::stdio("run", vec![]));
		let config = crate::models::AgentConfig {
			mcps: vec![server.clone()],
			skills: vec![],
			sub_agents: vec![],
		};
		let mut checked = 0;
		for descriptor in crate::agents::ALL_DESCRIPTORS {
			if !descriptor.capabilities.mcp.stdio {
				continue;
			}
			let serialize = descriptor.mcp_serialize_config.unwrap();
			let canonical = serialize(&config, None).unwrap();
			assert!(
				!unmanaged_mcp_source_fields(&server, &canonical, serialize)
					.unwrap_or_else(|error| panic!(
						"{}: {error}",
						descriptor.id
					)),
				"{}",
				descriptor.id
			);
			checked += 1;
		}
		assert!(
			checked >= 20,
			"test must cover the MCP roster, got {checked}"
		);
	}
}
