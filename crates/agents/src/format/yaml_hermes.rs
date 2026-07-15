//! Hermes (`~/.hermes/config.yaml`) MCP serializer.
//!
//! Hermes stores MCP servers under the `mcp_servers` key of one large,
//! machine-managed YAML config with many unrelated keys. Parse is strict (no
//! silent drop); serialize preserves every other top-level key and every
//! per-server field it does not own, replacing only the transport keys.

use crate::errors::{ConfigError, Result};
use crate::format::transport_policy::{
	missing_transport_error, reject_mixed_transport, remote_transport,
	transport_fields, transport_keys, FieldValue,
};
use crate::models::{AgentConfig, McpServer, McpTransport};
use serde_yaml::{Mapping, Value};
use std::collections::HashMap;

fn value_to_string_map(
	v: &Value,
	server: &str,
	field: &str,
) -> Result<HashMap<String, String>> {
	let map = v.as_mapping().ok_or_else(|| {
		ConfigError::InvalidConfig(format!(
			"Hermes MCP server `{server}` field `{field}` must be a mapping"
		))
	})?;
	let mut out = HashMap::new();
	for (k, val) in map {
		let k = k.as_str().ok_or_else(|| {
			ConfigError::InvalidConfig(format!(
				"Hermes MCP server `{server}` field `{field}` has a non-string key"
			))
		})?;
		let val = val.as_str().ok_or_else(|| {
			ConfigError::InvalidConfig(format!(
				"Hermes MCP server `{server}` field `{field}`.`{k}` must be a string"
			))
		})?;
		out.insert(k.to_string(), val.to_string());
	}
	Ok(out)
}

fn string_map_to_value(map: &HashMap<String, String>) -> Value {
	// Sort keys for deterministic, diff-stable output.
	let mut keys: Vec<&String> = map.keys().collect();
	keys.sort();
	let mut out = Mapping::new();
	for k in keys {
		out.insert(Value::String(k.clone()), Value::String(map[k].clone()));
	}
	Value::Mapping(out)
}

pub fn parse(content: &str) -> Result<AgentConfig> {
	let mut config = AgentConfig::new();
	if content.trim().is_empty() {
		return Ok(config);
	}
	let root: Value = serde_yaml::from_str(content).map_err(|e| {
		ConfigError::InvalidConfig(format!("invalid Hermes config YAML: {e}"))
	})?;
	let Some(servers_val) = root.get("mcp_servers") else {
		return Ok(config);
	};
	if servers_val.is_null() {
		return Ok(config);
	}
	let servers = servers_val.as_mapping().ok_or_else(|| {
		ConfigError::InvalidConfig("`mcp_servers` is not a mapping".to_string())
	})?;
	for (name_val, server_val) in servers {
		let name = name_val.as_str().ok_or_else(|| {
			ConfigError::InvalidConfig(
				"`mcp_servers` has a non-string server name".to_string(),
			)
		})?;
		let server = server_val.as_mapping().ok_or_else(|| {
			ConfigError::InvalidConfig(format!(
				"Hermes MCP server `{name}` is not a mapping"
			))
		})?;
		let enabled = match server.get("enabled") {
			None => true,
			Some(v) => v.as_bool().ok_or_else(|| {
				ConfigError::InvalidConfig(format!(
					"Hermes MCP server `{name}` field `enabled` must be a boolean"
				))
			})?,
		};
		// Reject mixed families on key PRESENCE before extracting any field so
		// error precedence matches the pre-extraction behaviour. Hermes has a
		// single remote transport, so `type` is not part of the remote family.
		let has_stdio = ["command", "args", "env"]
			.iter()
			.any(|k| server.contains_key(*k));
		let has_remote =
			["url", "headers"].iter().any(|k| server.contains_key(*k));
		reject_mixed_transport(has_stdio, has_remote, name, "Hermes", true)?;
		// Dispatch on presence, then extract ONLY the chosen branch's fields.
		let transport = if let Some(cmd) = server.get("command") {
			let command = cmd
				.as_str()
				.ok_or_else(|| {
					ConfigError::InvalidConfig(format!(
						"Hermes MCP server `{name}` field `command` must be a string"
					))
				})?
				.to_string();
			let args = match server.get("args") {
				None => Vec::new(),
				Some(v) => {
					let seq = v.as_sequence().ok_or_else(|| {
						ConfigError::InvalidConfig(format!(
							"Hermes MCP server `{name}` field `args` must be a sequence"
						))
					})?;
					seq.iter()
						.map(|a| {
							a.as_str().map(str::to_string).ok_or_else(|| {
								ConfigError::InvalidConfig(format!(
									"Hermes MCP server `{name}` field `args` must contain only strings"
								))
							})
						})
						.collect::<Result<Vec<_>>>()?
				}
			};
			let env = match server.get("env") {
				None => None,
				Some(v) => Some(value_to_string_map(v, name, "env")?),
			};
			McpTransport::Stdio {
				command,
				args,
				env,
				timeout: None,
			}
		} else if let Some(url_val) = server.get("url") {
			let url = url_val
				.as_str()
				.ok_or_else(|| {
					ConfigError::InvalidConfig(format!(
						"Hermes MCP server `{name}` field `url` must be a string"
					))
				})?
				.to_string();
			let headers = match server.get("headers") {
				None => None,
				Some(v) => Some(value_to_string_map(v, name, "headers")?),
			};
			// Single remote transport: `type` is not read; pass `None`.
			remote_transport(url, headers, None, true, name, "Hermes")?
		} else {
			return Err(missing_transport_error(name, "Hermes"));
		};
		config.mcps.push(McpServer {
			name: name.to_string(),
			enabled,
			transport,
			timeout: None,
			config_source: None,
		});
	}
	Ok(config)
}

fn field_to_value(field: FieldValue) -> Value {
	match field {
		FieldValue::Str(s) => Value::String(s),
		FieldValue::List(l) => {
			Value::Sequence(l.into_iter().map(Value::String).collect())
		}
		FieldValue::Map(m) => string_map_to_value(&m),
	}
}

pub fn serialize(
	config: &AgentConfig,
	original: Option<&str>,
) -> Result<String> {
	let mut root: Value = match original {
		Some(c) if !c.trim().is_empty() => {
			serde_yaml::from_str(c).map_err(|e| {
				ConfigError::InvalidConfig(format!(
					"failed to parse existing Hermes config: {e}"
				))
			})?
		}
		_ => Value::Mapping(Mapping::new()),
	};
	let root_map = root.as_mapping_mut().ok_or_else(|| {
		ConfigError::InvalidConfig(
			"Hermes config root is not a mapping".to_string(),
		)
	})?;

	// Existing per-server entries preserve fields we do not own. A present but
	// non-mapping `mcp_servers` is malformed — refuse rather than overwrite it.
	let existing: Mapping = match root_map.get("mcp_servers") {
		None => Mapping::new(),
		Some(v) if v.is_null() => Mapping::new(),
		Some(v) => v.as_mapping().cloned().ok_or_else(|| {
			ConfigError::InvalidConfig(
				"existing `mcp_servers` is not a mapping".to_string(),
			)
		})?,
	};

	let mut servers = Mapping::new();
	for mcp in &config.mcps {
		let mut entry = match existing.get(mcp.name.as_str()) {
			None => Mapping::new(),
			Some(v) if v.is_null() => Mapping::new(),
			Some(v) => v.as_mapping().cloned().ok_or_else(|| {
				ConfigError::InvalidConfig(format!(
					"existing entry for `{}` is not a mapping",
					mcp.name
				))
			})?,
		};
		// Remove all transport-owned keys before re-inserting (avoids stale
		// keys when a server's transport changes). `single_remote = true`, so a
		// transferred Sse server serializes as the one remote transport.
		for k in transport_keys(true) {
			entry.remove(*k);
		}
		for (key, value) in transport_fields(&mcp.transport, true) {
			entry.insert(Value::String(key.to_string()), field_to_value(value));
		}
		entry.insert(
			Value::String("enabled".to_string()),
			Value::Bool(mcp.enabled),
		);
		servers.insert(Value::String(mcp.name.clone()), Value::Mapping(entry));
	}
	root_map.insert(
		Value::String("mcp_servers".to_string()),
		Value::Mapping(servers),
	);
	serde_yaml::to_string(&root).map_err(|e| {
		ConfigError::InvalidConfig(format!(
			"failed to serialize Hermes config: {e}"
		))
	})
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::models::{AgentConfig, McpServer, McpTransport};

	#[test]
	fn parse_stdio_and_remote() {
		let yaml = "
mcp_servers:
  time:
    command: uvx
    args: [\"mcp-server-time\"]
    env:
      TZ: UTC
  notion:
    url: https://mcp.notion.com/mcp
";
		let cfg = parse(yaml).unwrap();
		assert_eq!(cfg.mcps.len(), 2);
		let time = cfg.mcps.iter().find(|m| m.name == "time").unwrap();
		assert!(matches!(time.transport, McpTransport::Stdio { .. }));
		let notion = cfg.mcps.iter().find(|m| m.name == "notion").unwrap();
		assert!(matches!(
			notion.transport,
			McpTransport::StreamableHttp { .. }
		));
	}

	#[test]
	fn parse_enabled_flag() {
		let yaml = "
mcp_servers:
  on:
    command: a
  off:
    command: b
    enabled: false
";
		let cfg = parse(yaml).unwrap();
		assert!(cfg.mcps.iter().find(|m| m.name == "on").unwrap().enabled);
		assert!(!cfg.mcps.iter().find(|m| m.name == "off").unwrap().enabled);
	}

	#[test]
	fn parse_rejects_non_mapping_servers() {
		assert!(parse("mcp_servers: 5\n").is_err());
	}

	#[test]
	fn parse_rejects_non_mapping_entry() {
		assert!(parse("mcp_servers:\n  bad: 5\n").is_err());
	}

	#[test]
	fn parse_rejects_entry_without_command_or_url() {
		assert!(parse("mcp_servers:\n  bad:\n    timeout: 5\n").is_err());
	}

	#[test]
	fn parse_rejects_mixed_transport_command_and_url() {
		assert!(parse(
			"mcp_servers:\n  bad:\n    command: x\n    url: https://y\n"
		)
		.is_err());
	}

	#[test]
	fn parse_rejects_mixed_transport_url_and_args() {
		assert!(parse(
			"mcp_servers:\n  bad:\n    url: https://y\n    args: [\"a\"]\n"
		)
		.is_err());
	}

	#[test]
	fn parse_empty_is_ok() {
		assert!(parse("").unwrap().mcps.is_empty());
		assert!(parse("model: gpt\n").unwrap().mcps.is_empty());
	}

	#[test]
	fn serialize_preserves_other_top_level_keys() {
		let original = "model: gpt-x\nagent:\n  foo: bar\nmcp_servers:\n  old:\n    command: c\n";
		let cfg = AgentConfig {
			mcps: vec![McpServer::new(
				"srv",
				McpTransport::stdio("run", vec![]),
			)],
			skills: vec![],
			sub_agents: vec![],
		};
		let out = serialize(&cfg, Some(original)).unwrap();
		let v: serde_yaml::Value = serde_yaml::from_str(&out).unwrap();
		assert_eq!(v.get("model").unwrap().as_str(), Some("gpt-x"));
		assert_eq!(
			v.get("agent").unwrap().get("foo").unwrap().as_str(),
			Some("bar")
		);
		// old server replaced by the new desired set
		let servers = v.get("mcp_servers").unwrap();
		assert!(servers.get("srv").is_some());
		assert!(servers.get("old").is_none());
	}

	#[test]
	fn serialize_preserves_per_server_extra_fields() {
		let original = "mcp_servers:\n  srv:\n    command: old\n    timeout: 120\n    sampling:\n      enabled: true\n";
		let cfg = AgentConfig {
			mcps: vec![McpServer::new(
				"srv",
				McpTransport::stdio("newcmd", vec![]),
			)],
			skills: vec![],
			sub_agents: vec![],
		};
		let out = serialize(&cfg, Some(original)).unwrap();
		let v: serde_yaml::Value = serde_yaml::from_str(&out).unwrap();
		let srv = v.get("mcp_servers").unwrap().get("srv").unwrap();
		assert_eq!(srv.get("command").unwrap().as_str(), Some("newcmd"));
		assert_eq!(srv.get("timeout").unwrap().as_u64(), Some(120));
		assert!(srv.get("sampling").is_some());
	}

	#[test]
	fn serialize_removes_stale_transport_keys_on_switch() {
		// stdio server re-saved as remote must not keep command/args
		let original =
			"mcp_servers:\n  srv:\n    command: old\n    args: [\"a\"]\n";
		let cfg = AgentConfig {
			mcps: vec![McpServer::new(
				"srv",
				McpTransport::StreamableHttp {
					url: "https://x/mcp".into(),
					headers: None,
					timeout: None,
				},
			)],
			skills: vec![],
			sub_agents: vec![],
		};
		let out = serialize(&cfg, Some(original)).unwrap();
		let v: serde_yaml::Value = serde_yaml::from_str(&out).unwrap();
		let srv = v.get("mcp_servers").unwrap().get("srv").unwrap();
		assert_eq!(srv.get("url").unwrap().as_str(), Some("https://x/mcp"));
		assert!(srv.get("command").is_none());
		assert!(srv.get("args").is_none());
	}

	#[test]
	fn serialize_sse_emitted_as_url() {
		let cfg = AgentConfig {
			mcps: vec![McpServer::new(
				"legacy",
				McpTransport::Sse {
					url: "https://x/sse".into(),
					headers: None,
					timeout: None,
				},
			)],
			skills: vec![],
			sub_agents: vec![],
		};
		let out = serialize(&cfg, None).unwrap();
		let v: serde_yaml::Value = serde_yaml::from_str(&out).unwrap();
		let srv = v.get("mcp_servers").unwrap().get("legacy").unwrap();
		assert_eq!(srv.get("url").unwrap().as_str(), Some("https://x/sse"));
	}

	#[test]
	fn serialize_keeps_disabled_server() {
		let mut m = McpServer::new("srv", McpTransport::stdio("c", vec![]));
		m.enabled = false;
		let cfg = AgentConfig {
			mcps: vec![m],
			skills: vec![],
			sub_agents: vec![],
		};
		let out = serialize(&cfg, None).unwrap();
		let v: serde_yaml::Value = serde_yaml::from_str(&out).unwrap();
		let srv = v.get("mcp_servers").unwrap().get("srv").unwrap();
		assert_eq!(srv.get("enabled").unwrap().as_bool(), Some(false));
	}

	#[test]
	fn roundtrip_stable() {
		let yaml = "mcp_servers:\n  a:\n    command: x\n    args: [\"1\"]\n";
		let cfg = parse(yaml).unwrap();
		let out = serialize(&cfg, Some(yaml)).unwrap();
		let cfg2 = parse(&out).unwrap();
		assert_eq!(cfg.mcps.len(), cfg2.mcps.len());
		assert_eq!(cfg.mcps[0].name, cfg2.mcps[0].name);
	}

	#[test]
	fn parse_rejects_non_string_command() {
		assert!(parse("mcp_servers:\n  bad:\n    command: 5\n").is_err());
	}

	#[test]
	fn parse_rejects_non_bool_enabled() {
		assert!(parse(
			"mcp_servers:\n  bad:\n    command: x\n    enabled: 5\n"
		)
		.is_err());
	}

	#[test]
	fn parse_rejects_non_string_args_element() {
		assert!(parse(
			"mcp_servers:\n  bad:\n    command: x\n    args: [1, 2]\n"
		)
		.is_err());
	}

	#[test]
	fn parse_rejects_non_string_env_value() {
		assert!(parse(
			"mcp_servers:\n  bad:\n    command: x\n    env:\n      A: 1\n"
		)
		.is_err());
	}

	#[test]
	fn parse_rejects_non_string_url() {
		assert!(parse("mcp_servers:\n  bad:\n    url: 5\n").is_err());
	}

	#[test]
	fn serialize_rejects_non_mapping_existing_mcp_servers() {
		let cfg = AgentConfig {
			mcps: vec![McpServer::new("s", McpTransport::stdio("c", vec![]))],
			skills: vec![],
			sub_agents: vec![],
		};
		assert!(serialize(&cfg, Some("mcp_servers: 5\n")).is_err());
	}

	#[test]
	fn serialize_rejects_non_mapping_existing_entry() {
		let cfg = AgentConfig {
			mcps: vec![McpServer::new("s", McpTransport::stdio("c", vec![]))],
			skills: vec![],
			sub_agents: vec![],
		};
		assert!(serialize(&cfg, Some("mcp_servers:\n  s: 5\n")).is_err());
	}
}
