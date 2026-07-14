//! Hermes (`~/.hermes/config.yaml`) MCP serializer.
//!
//! Hermes stores MCP servers under the `mcp_servers` key of one large,
//! machine-managed YAML config with many unrelated keys. Parse is strict (no
//! silent drop); serialize preserves every other top-level key and every
//! per-server field it does not own, replacing only the transport keys.

use crate::errors::{ConfigError, Result};
use crate::models::{AgentConfig, McpServer, McpTransport};
use serde_yaml::{Mapping, Value};
use std::collections::HashMap;

const TRANSPORT_KEYS: [&str; 5] = ["command", "args", "env", "url", "headers"];

fn value_to_string_map(v: &Value) -> Option<HashMap<String, String>> {
	let map = v.as_mapping()?;
	let mut out = HashMap::new();
	for (k, val) in map {
		if let (Some(k), Some(val)) = (k.as_str(), val.as_str()) {
			out.insert(k.to_string(), val.to_string());
		}
	}
	Some(out)
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
		let enabled = server
			.get("enabled")
			.and_then(Value::as_bool)
			.unwrap_or(true);
		let transport = if let Some(cmd) = server.get("command") {
			let command = cmd.as_str().unwrap_or_default().to_string();
			let args = server
				.get("args")
				.and_then(Value::as_sequence)
				.map(|seq| {
					seq.iter()
						.filter_map(|v| v.as_str().map(str::to_string))
						.collect()
				})
				.unwrap_or_default();
			let env = server.get("env").and_then(value_to_string_map);
			McpTransport::Stdio {
				command,
				args,
				env,
				timeout: None,
			}
		} else if let Some(url_val) = server.get("url") {
			let url = url_val.as_str().unwrap_or_default().to_string();
			let headers = server.get("headers").and_then(value_to_string_map);
			McpTransport::StreamableHttp {
				url,
				headers,
				timeout: None,
			}
		} else {
			return Err(ConfigError::InvalidConfig(format!(
				"Hermes MCP server `{name}` has neither `command` nor `url`"
			)));
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

	// Existing per-server entries preserve fields we do not own.
	let existing: Mapping = root_map
		.get("mcp_servers")
		.and_then(Value::as_mapping)
		.cloned()
		.unwrap_or_default();

	let mut servers = Mapping::new();
	for mcp in &config.mcps {
		let mut entry = existing
			.get(mcp.name.as_str())
			.and_then(Value::as_mapping)
			.cloned()
			.unwrap_or_default();
		// Remove all transport-owned keys before re-inserting (avoids stale
		// keys when a server's transport changes).
		for k in TRANSPORT_KEYS {
			entry.remove(k);
		}
		match &mcp.transport {
			McpTransport::Stdio {
				command, args, env, ..
			} => {
				entry.insert(
					Value::String("command".to_string()),
					Value::String(command.clone()),
				);
				entry.insert(
					Value::String("args".to_string()),
					Value::Sequence(
						args.iter().map(|a| Value::String(a.clone())).collect(),
					),
				);
				if let Some(env) = env {
					entry.insert(
						Value::String("env".to_string()),
						string_map_to_value(env),
					);
				}
			}
			// Hermes has a single remote transport (`url`); serialize the
			// deprecated Sse arm identically so a transferred SSE server
			// survives and the match stays exhaustive.
			McpTransport::Sse { url, headers, .. }
			| McpTransport::StreamableHttp { url, headers, .. } => {
				entry.insert(
					Value::String("url".to_string()),
					Value::String(url.clone()),
				);
				if let Some(headers) = headers {
					entry.insert(
						Value::String("headers".to_string()),
						string_map_to_value(headers),
					);
				}
			}
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
}
