//! OpenClaw (`~/.openclaw/openclaw.json`) MCP configuration.
//!
//! OpenClaw stores MCP servers under `mcp.servers` in its shared JSON/JSON5
//! document. This module owns only the normalized transport fields and the
//! inline `enabled` flag; unrelated root, `mcp`, and per-server fields survive
//! rewrites.

use crate::errors::{ConfigError, Result};
use crate::format::mcp_policy::{reject_mixed_transport, RemoteVocabulary};
use crate::models::{AgentConfig, McpServer, McpTransport};
use aghub_json::{parse_jsonc_opt, patch_jsonc_object};
use serde_json::{Map, Value};
use std::collections::HashMap;

/// OpenClaw tags transports with `transport` and spells BOTH remote shapes, so
/// nothing here is ever refused.
///
/// `http_read_aliases` is EMPTY and that is the honest value: the field means
/// "read as streamable HTTP UNDER `tag_key`", and `transport: "http"` is
/// refused here (`has unsupported transport `http``). `http` is understood only
/// under the pre-rename `type` key, which is this module's own syntax layer —
/// see `LEGACY_TYPE_HTTP`. Declaring it up here would be the `single_remote:
/// true` mistake again: a field whose value is wrong for the thing it names,
/// held together only by the one call site that happens to make it read true.
const VOCAB: RemoteVocabulary = RemoteVocabulary {
	tag_key: "transport",
	sse: "sse",
	http: "streamable-http",
	http_read_aliases: &[],
};

/// Spellings the legacy `type` key carried, normalised to `VOCAB.http` on read
/// and never written back.
const LEGACY_TYPE_HTTP: &[&str] = &["http"];

const TRANSPORT_KEYS: &[&str] = &[
	"transport",
	"type",
	"command",
	"args",
	"env",
	"url",
	"headers",
	"timeout",
];

fn invalid(message: impl Into<String>) -> ConfigError {
	ConfigError::InvalidConfig(message.into())
}

fn parse_root(content: &str) -> Result<Value> {
	parse_jsonc_opt(content)
		.map_err(|error| invalid(format!("invalid OpenClaw JSON: {error}")))
		.map(|value| value.unwrap_or_else(|| Value::Object(Map::new())))
}

fn string_map(
	value: &Value,
	server: &str,
	field: &str,
) -> Result<HashMap<String, String>> {
	let object = value.as_object().ok_or_else(|| {
		invalid(format!(
			"OpenClaw MCP server `{server}` field `{field}` must be an object"
		))
	})?;
	object
		.iter()
		.map(|(key, value)| {
			value
				.as_str()
				.map(|value| (key.clone(), value.to_string()))
				.ok_or_else(|| {
					invalid(format!(
						"OpenClaw MCP server `{server}` field `{field}`.`{key}` must be a string"
					))
				})
		})
		.collect()
}

fn optional_string_map(
	server: &Map<String, Value>,
	name: &str,
	field: &str,
) -> Result<Option<HashMap<String, String>>> {
	server
		.get(field)
		.map(|value| string_map(value, name, field))
		.transpose()
}

fn required_string(
	server: &Map<String, Value>,
	name: &str,
	field: &str,
) -> Result<String> {
	server
		.get(field)
		.and_then(Value::as_str)
		.map(str::to_string)
		.ok_or_else(|| {
			invalid(format!(
				"OpenClaw MCP server `{name}` field `{field}` must be a string"
			))
		})
}

fn optional_args(
	server: &Map<String, Value>,
	name: &str,
) -> Result<Vec<String>> {
	let Some(value) = server.get("args") else {
		return Ok(Vec::new());
	};
	let args = value.as_array().ok_or_else(|| {
		invalid(format!(
			"OpenClaw MCP server `{name}` field `args` must be an array"
		))
	})?;
	args.iter()
		.map(|arg| {
			arg.as_str().map(str::to_string).ok_or_else(|| {
				invalid(format!(
					"OpenClaw MCP server `{name}` field `args` must contain only strings"
				))
			})
		})
		.collect()
}

fn optional_timeout(
	server: &Map<String, Value>,
	name: &str,
) -> Result<Option<u64>> {
	server
		.get("timeout")
		.map(|value| {
			value.as_u64().ok_or_else(|| {
				invalid(format!(
					"OpenClaw MCP server `{name}` field `timeout` must be a non-negative integer"
				))
			})
		})
		.transpose()
}

pub fn parse(content: &str) -> Result<AgentConfig> {
	let root = parse_root(content)?;
	let root = root
		.as_object()
		.ok_or_else(|| invalid("OpenClaw config root must be an object"))?;
	let Some(mcp) = root.get("mcp") else {
		return Ok(AgentConfig::new());
	};
	let mcp = mcp.as_object().ok_or_else(|| {
		invalid("OpenClaw config field `mcp` must be an object")
	})?;
	let Some(servers) = mcp.get("servers") else {
		return Ok(AgentConfig::new());
	};
	let servers = servers.as_object().ok_or_else(|| {
		invalid("OpenClaw config field `mcp.servers` must be an object")
	})?;

	let mut config = AgentConfig::new();
	for (name, value) in servers {
		let server = value.as_object().ok_or_else(|| {
			invalid(format!("OpenClaw MCP server `{name}` must be an object"))
		})?;
		let enabled = match server.get("enabled") {
			None => true,
			Some(value) => value.as_bool().ok_or_else(|| {
				invalid(format!(
					"OpenClaw MCP server `{name}` field `enabled` must be a boolean"
				))
			})?,
		};
		let transport_name = match server.get(VOCAB.tag_key) {
			Some(value) => Some(value.as_str().ok_or_else(|| {
				invalid(format!(
					"OpenClaw MCP server `{name}` field `{key}` must be a string",
					key = VOCAB.tag_key
				))
			})?),
			None => server.get("type").and_then(Value::as_str).map(|value| {
				if LEGACY_TYPE_HTTP.contains(&value) {
					VOCAB.http
				} else {
					value
				}
			}),
		};
		reject_mixed_transport(
			&["command"],
			&["url"],
			|key| server.contains_key(key),
			name,
			"OpenClaw",
		)?;
		let timeout = optional_timeout(server, name)?;
		let transport = match transport_name {
			// `stdio` is not remote vocabulary — it stays a literal.
			Some("stdio") => McpTransport::Stdio {
				command: required_string(server, name, "command")?,
				args: optional_args(server, name)?,
				env: optional_string_map(server, name, "env")?,
				timeout,
			},
			Some(tag) if tag == VOCAB.sse => McpTransport::Sse {
				url: required_string(server, name, "url")?,
				headers: optional_string_map(server, name, "headers")?,
				timeout,
			},
			Some(tag) if tag == VOCAB.http => McpTransport::StreamableHttp {
				url: required_string(server, name, "url")?,
				headers: optional_string_map(server, name, "headers")?,
				timeout,
			},
			None if server.contains_key("command") => McpTransport::Stdio {
				command: required_string(server, name, "command")?,
				args: optional_args(server, name)?,
				env: optional_string_map(server, name, "env")?,
				timeout,
			},
			None if server.contains_key("url") => {
				McpTransport::StreamableHttp {
					url: required_string(server, name, "url")?,
					headers: optional_string_map(server, name, "headers")?,
					timeout,
				}
			}
			Some(other) => {
				return Err(invalid(format!(
					"OpenClaw MCP server `{name}` has unsupported transport `{other}`"
				)))
			}
			None => {
				return Err(invalid(format!(
					"OpenClaw MCP server `{name}` requires `command` or `url`"
				)))
			}
		};
		config.mcps.push(McpServer {
			name: name.clone(),
			enabled,
			transport,
			timeout: None,
			config_source: None,
		});
	}
	Ok(config)
}

pub fn serialize(
	config: &AgentConfig,
	original: Option<&str>,
) -> Result<String> {
	let mut root = match original {
		Some(content) => parse_root(content)?,
		None => Value::Object(Map::new()),
	};
	let root = root
		.as_object_mut()
		.ok_or_else(|| invalid("OpenClaw config root must be an object"))?;
	let mcp = root
		.entry("mcp")
		.or_insert_with(|| Value::Object(Map::new()))
		.as_object_mut()
		.ok_or_else(|| {
			invalid("OpenClaw config field `mcp` must be an object")
		})?;
	let existing = match mcp.get("servers") {
		None => Map::new(),
		Some(value) => value.as_object().cloned().ok_or_else(|| {
			invalid("OpenClaw config field `mcp.servers` must be an object")
		})?,
	};

	let mut servers = Map::new();
	for server in &config.mcps {
		let mut entry = match existing.get(&server.name) {
			None => Map::new(),
			Some(value) => value.as_object().cloned().ok_or_else(|| {
				invalid(format!(
					"OpenClaw MCP server `{}` must be an object",
					server.name
				))
			})?,
		};
		// A no-op while `VOCAB.sse` is non-empty, and that is the point:
		// declaring a spelling is not enough on its own, the dialect has to
		// ASK. Drop this call and emptying `VOCAB.sse` writes `transport: ""`
		// instead of refusing — `mcp_dialect_roundtrip` catches a missing call,
		// `mcp_dialect_decisions` does not.
		VOCAB.refuse_unwritable(
			&server.transport,
			&format!("OpenClaw MCP server `{}`", server.name),
		)?;
		for key in TRANSPORT_KEYS {
			entry.remove(*key);
		}
		match &server.transport {
			McpTransport::Stdio {
				command,
				args,
				env,
				timeout,
			} => {
				// `stdio` is not remote vocabulary — it stays a literal.
				entry.insert(
					VOCAB.tag_key.into(),
					Value::String("stdio".into()),
				);
				entry.insert("command".into(), Value::String(command.clone()));
				if !args.is_empty() {
					entry.insert(
						"args".into(),
						Value::Array(
							args.iter().cloned().map(Value::String).collect(),
						),
					);
				}
				if let Some(env) = env {
					entry.insert("env".into(), string_map_value(env));
				}
				if let Some(timeout) = timeout {
					entry.insert("timeout".into(), Value::from(*timeout));
				}
			}
			McpTransport::Sse {
				url,
				headers,
				timeout,
			} => {
				entry.insert(
					VOCAB.tag_key.into(),
					Value::String(VOCAB.sse.into()),
				);
				entry.insert("url".into(), Value::String(url.clone()));
				if let Some(headers) = headers {
					entry.insert("headers".into(), string_map_value(headers));
				}
				if let Some(timeout) = timeout {
					entry.insert("timeout".into(), Value::from(*timeout));
				}
			}
			McpTransport::StreamableHttp {
				url,
				headers,
				timeout,
			} => {
				entry.insert(
					VOCAB.tag_key.into(),
					Value::String(VOCAB.http.into()),
				);
				entry.insert("url".into(), Value::String(url.clone()));
				if let Some(headers) = headers {
					entry.insert("headers".into(), string_map_value(headers));
				}
				if let Some(timeout) = timeout {
					entry.insert("timeout".into(), Value::from(*timeout));
				}
			}
		}
		entry.insert("enabled".into(), Value::Bool(server.enabled));
		servers.insert(server.name.clone(), Value::Object(entry));
	}
	mcp.insert("servers".into(), Value::Object(servers));

	patch_jsonc_object(original, &root).map_err(|error| {
		invalid(format!("failed to write OpenClaw JSON: {error}"))
	})
}

fn string_map_value(map: &HashMap<String, String>) -> Value {
	Value::Object(
		map.iter()
			.map(|(key, value)| (key.clone(), Value::String(value.clone())))
			.collect(),
	)
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::models::{McpServer, McpTransport};

	#[test]
	fn parse_nested_servers_transports_and_enabled() {
		let config = parse(
			r#"{
				"mcp": {
					"servers": {
						"local": {
							"transport": "stdio",
							"command": "npx",
							"args": ["-y", "server"],
							"env": {"TOKEN": "secret"},
							"enabled": false
						},
						"events": {
							"transport": "sse",
							"url": "https://example.com/sse"
						},
						"remote": {
							"transport": "streamable-http",
							"url": "https://example.com/mcp",
							"headers": {"Authorization": "Bearer token"}
						}
					}
				}
			}"#,
		)
		.unwrap();

		assert_eq!(config.mcps.len(), 3);
		let local = config.mcps.iter().find(|mcp| mcp.name == "local").unwrap();
		assert!(!local.enabled);
		match &local.transport {
			McpTransport::Stdio {
				command, args, env, ..
			} => {
				assert_eq!(command, "npx");
				assert_eq!(args, &["-y".to_string(), "server".to_string()]);
				assert_eq!(
					env.as_ref().and_then(|env| env.get("TOKEN")),
					Some(&"secret".to_string())
				);
			}
			other => panic!("expected stdio, got {other:?}"),
		}

		let events =
			config.mcps.iter().find(|mcp| mcp.name == "events").unwrap();
		assert!(events.enabled);
		assert!(matches!(events.transport, McpTransport::Sse { .. }));
		let remote =
			config.mcps.iter().find(|mcp| mcp.name == "remote").unwrap();
		assert!(matches!(
			remote.transport,
			McpTransport::StreamableHttp { .. }
		));
	}

	#[test]
	fn serialize_uses_nested_servers_and_inline_enabled() {
		let mut local = McpServer::new(
			"local",
			McpTransport::stdio("npx", vec!["-y".to_string()]),
		);
		local.enabled = false;
		let config = AgentConfig {
			mcps: vec![
				local,
				McpServer::new(
					"remote",
					McpTransport::streamable_http("https://example.com/mcp"),
				),
			],
			skills: vec![],
			sub_agents: vec![],
		};

		let output = serialize(&config, None).unwrap();
		let value: serde_json::Value = serde_json::from_str(&output).unwrap();
		let servers = value["mcp"]["servers"].as_object().unwrap();
		assert_eq!(servers["local"]["transport"], "stdio");
		assert_eq!(servers["local"]["enabled"], false);
		assert_eq!(servers["remote"]["transport"], "streamable-http");
		assert_eq!(servers["remote"]["enabled"], true);
		assert!(value.get("mcpServers").is_none());
	}

	#[test]
	fn serialize_preserves_unknown_root_mcp_and_server_fields() {
		let original = r#"{
			"theme": "dark",
			"mcp": {
				"sessionIdleTtlMs": 60000,
				"servers": {
					"kept": {
						"transport": "stdio",
						"command": "old-command",
						"vendorFlag": true,
						"oauth": {"clientId": "client-id"}
					}
				}
			}
		}"#;
		let config = AgentConfig {
			mcps: vec![McpServer::new(
				"kept",
				McpTransport::stdio("new-command", vec![]),
			)],
			skills: vec![],
			sub_agents: vec![],
		};

		let output = serialize(&config, Some(original)).unwrap();
		let value: serde_json::Value = serde_json::from_str(&output).unwrap();
		assert_eq!(value["theme"], "dark");
		assert_eq!(value["mcp"]["sessionIdleTtlMs"], 60000);
		assert_eq!(value["mcp"]["servers"]["kept"]["vendorFlag"], true);
		assert_eq!(
			value["mcp"]["servers"]["kept"]["oauth"]["clientId"],
			"client-id"
		);
		assert_eq!(value["mcp"]["servers"]["kept"]["command"], "new-command");
	}

	/// `LEGACY_TYPE_HTTP` is the ONLY thing that keeps configs written before
	/// OpenClaw renamed the key readable. Empty it and the legacy
	/// `"type": "http"` entry falls through to the `unsupported transport`
	/// arm — and because a parse failure is whole-document, the user loses
	/// access to EVERY OpenClaw MCP server, not just that one.
	///
	/// The alias lives on the LEGACY key only: `"transport": "http"` is
	/// refused, which is why `VOCAB.http_read_aliases` is empty.
	#[test]
	fn a_legacy_type_http_entry_is_read_and_normalised() {
		let original = r#"{"mcp":{"servers":{"x":{"type":"http","url":"https://example.test/mcp"}}}}"#;
		let config = parse(original).expect(
			"a legacy `type: http` entry must stay readable, or the whole \
			 config becomes unmanageable",
		);
		assert!(matches!(
			config.mcps[0].transport,
			McpTransport::StreamableHttp { .. }
		));
		// Read, then normalised to the current spelling under the current key.
		let output = serialize(&config, Some(original)).unwrap();
		let value: serde_json::Value = serde_json::from_str(&output).unwrap();
		assert_eq!(
			value["mcp"]["servers"]["x"]["transport"],
			"streamable-http"
		);
		assert!(value["mcp"]["servers"]["x"].get("type").is_none());
	}

	/// `http_read_aliases` means "read as streamable HTTP UNDER `tag_key`".
	/// OpenClaw's is EMPTY because it reads none there — `http` lives on the
	/// legacy key (`LEGACY_TYPE_HTTP`) and `transport: "http"` is refused.
	/// A declaration the dispatch does not honour is the `single_remote: true`
	/// lie in a new shape, so bind the list to the parser instead of trusting
	/// the comment: re-adding an alias without wiring it turns this red.
	#[test]
	fn every_declared_read_alias_really_parses_under_the_tag_key() {
		for alias in VOCAB.http_read_aliases {
			let text = format!(
				r#"{{"mcp":{{"servers":{{"x":{{"{key}":"{alias}","url":"https://e.test/m"}}}}}}}}"#,
				key = VOCAB.tag_key
			);
			let config = parse(&text).unwrap_or_else(|error| {
				panic!(
					"`{}: {alias}` is declared a read alias but is refused: {error}",
					VOCAB.tag_key
				)
			});
			assert!(matches!(
				config.mcps[0].transport,
				McpTransport::StreamableHttp { .. }
			));
		}
		assert!(
			parse(
				r#"{"mcp":{"servers":{"x":{"transport":"http","url":"https://e.test/m"}}}}"#
			)
			.is_err(),
			"`transport: http` is refused today; if that changed, `http` \
			 belongs in `VOCAB.http_read_aliases`"
		);
	}

	/// OpenClaw probes `command` × `url` and nothing else. Widening that probe
	/// would refuse the WHOLE file — every OpenClaw MCP server, not just this
	/// entry — over a key that means nothing for the transport it declares.
	///
	/// CEILING, deliberately asserted only on the parse side: OpenClaw blanket-
	/// strips `TRANSPORT_KEYS`, so this `headers` IS dropped by the rewrite.
	/// That normalisation predates the policy module and is recorded in
	/// `mcp_policy`'s probe doc; it is not something this test blesses.
	#[test]
	fn a_stdio_entry_with_an_inert_remote_key_stays_readable() {
		let config = parse(
			r#"{"mcp":{"servers":{"x":{"transport":"stdio","command":"run","headers":{"A":"b"}}}}}"#,
		)
		.expect("widening the remote probe would refuse this whole file");
		assert!(matches!(
			config.mcps[0].transport,
			McpTransport::Stdio { .. }
		));
	}
}
