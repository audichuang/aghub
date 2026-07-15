//! Grok (`~/.grok/config.toml`) MCP serializer.
//!
//! Grok stores MCP servers under the `mcp_servers` key of one large TOML
//! config with many unrelated top-level tables. Parse is strict (no silent
//! drop); serialize preserves every other top-level key and every per-server
//! field it does not own, replacing only the transport keys.
//!
//! Transport mapping (verified against grok 0.2.99):
//! - stdio: `command` + `args` + optional nested `env`
//! - streamable HTTP: `url` + optional nested `headers` (no `type` key)
//! - SSE: `url` + `type = "sse"` + optional nested `headers`
//! - `enabled` is a native per-server bool (missing defaults to true)

use crate::errors::{ConfigError, Result};
use crate::format::transport_policy::{
	missing_transport_error, reject_mixed_transport, remote_transport,
	transport_fields, transport_keys, FieldValue,
};
use crate::models::{AgentConfig, McpServer, McpTransport};
use std::collections::HashMap;
use toml::map::Map;
use toml::Value;

fn value_to_string_map(
	v: &Value,
	server: &str,
	field: &str,
) -> Result<HashMap<String, String>> {
	let table = v.as_table().ok_or_else(|| {
		ConfigError::InvalidConfig(format!(
			"Grok MCP server `{server}` field `{field}` must be a table"
		))
	})?;
	let mut out = HashMap::new();
	for (k, val) in table {
		let val = val.as_str().ok_or_else(|| {
			ConfigError::InvalidConfig(format!(
				"Grok MCP server `{server}` field `{field}`.`{k}` must be a string"
			))
		})?;
		out.insert(k.clone(), val.to_string());
	}
	Ok(out)
}

fn string_map_to_value(map: &HashMap<String, String>) -> Value {
	// Sort keys for deterministic, diff-stable output.
	let mut keys: Vec<&String> = map.keys().collect();
	keys.sort();
	let mut out = Map::new();
	for k in keys {
		out.insert(k.clone(), Value::String(map[k].clone()));
	}
	Value::Table(out)
}

fn parse_toml(content: &str) -> Result<Value> {
	toml::from_str(content).map_err(|e| {
		ConfigError::InvalidConfig(format!("invalid Grok config TOML: {e}"))
	})
}

pub fn parse(content: &str) -> Result<AgentConfig> {
	let mut config = AgentConfig::new();
	if content.trim().is_empty() {
		return Ok(config);
	}
	let root = parse_toml(content)?;
	let Some(servers_val) = root.get("mcp_servers") else {
		return Ok(config);
	};
	let servers = servers_val.as_table().ok_or_else(|| {
		ConfigError::InvalidConfig("`mcp_servers` is not a table".to_string())
	})?;
	for (name, server_val) in servers {
		let server = server_val.as_table().ok_or_else(|| {
			ConfigError::InvalidConfig(format!(
				"Grok MCP server `{name}` is not a table"
			))
		})?;
		let enabled = match server.get("enabled") {
			None => true,
			Some(v) => v.as_bool().ok_or_else(|| {
				ConfigError::InvalidConfig(format!(
					"Grok MCP server `{name}` field `enabled` must be a boolean"
				))
			})?,
		};
		// Reject mixed families on key PRESENCE before extracting any field so
		// error precedence matches the pre-extraction behaviour.
		let has_stdio = ["command", "args", "env"]
			.iter()
			.any(|k| server.contains_key(*k));
		let has_remote = ["url", "headers", "type"]
			.iter()
			.any(|k| server.contains_key(*k));
		reject_mixed_transport(has_stdio, has_remote, name, "Grok", false)?;
		// Dispatch on presence, then extract ONLY the chosen branch's fields.
		let transport = if let Some(cmd) = server.get("command") {
			let command = cmd
				.as_str()
				.ok_or_else(|| {
					ConfigError::InvalidConfig(format!(
						"Grok MCP server `{name}` field `command` must be a string"
					))
				})?
				.to_string();
			let args = match server.get("args") {
				None => Vec::new(),
				Some(v) => {
					let arr = v.as_array().ok_or_else(|| {
						ConfigError::InvalidConfig(format!(
							"Grok MCP server `{name}` field `args` must be an array"
						))
					})?;
					arr.iter()
						.map(|a| {
							a.as_str().map(str::to_string).ok_or_else(|| {
								ConfigError::InvalidConfig(format!(
									"Grok MCP server `{name}` field `args` must contain only strings"
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
						"Grok MCP server `{name}` field `url` must be a string"
					))
				})?
				.to_string();
			let headers = match server.get("headers") {
				None => None,
				Some(v) => Some(value_to_string_map(v, name, "headers")?),
			};
			let type_key = match server.get("type") {
				None => None,
				Some(v) => Some(
					v.as_str()
						.ok_or_else(|| {
							ConfigError::InvalidConfig(format!(
								"Grok MCP server `{name}` field `type` must be a string"
							))
						})?
						.to_string(),
				),
			};
			remote_transport(url, headers, type_key, false, name, "Grok")?
		} else {
			return Err(missing_transport_error(name, "Grok"));
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

fn field_to_value(field: FieldValue) -> Value {
	match field {
		FieldValue::Str(s) => Value::String(s),
		FieldValue::List(l) => {
			Value::Array(l.into_iter().map(Value::String).collect())
		}
		FieldValue::Map(m) => string_map_to_value(&m),
	}
}

pub fn serialize(
	config: &AgentConfig,
	original: Option<&str>,
) -> Result<String> {
	let mut root: Value = match original {
		Some(c) if !c.trim().is_empty() => parse_toml(c)?,
		_ => Value::Table(Map::new()),
	};
	let root_map = root.as_table_mut().ok_or_else(|| {
		ConfigError::InvalidConfig(
			"Grok config root is not a table".to_string(),
		)
	})?;

	// Existing per-server entries preserve fields we do not own. A present but
	// non-table `mcp_servers` is malformed — refuse rather than overwrite it.
	let existing: Map<String, Value> = match root_map.get("mcp_servers") {
		None => Map::new(),
		Some(v) => v.as_table().cloned().ok_or_else(|| {
			ConfigError::InvalidConfig(
				"existing `mcp_servers` is not a table".to_string(),
			)
		})?,
	};

	let mut servers = Map::new();
	for mcp in &config.mcps {
		let mut entry = match existing.get(&mcp.name) {
			None => Map::new(),
			Some(v) => v.as_table().cloned().ok_or_else(|| {
				ConfigError::InvalidConfig(format!(
					"existing entry for `{}` is not a table",
					mcp.name
				))
			})?,
		};
		// Remove all transport-owned keys before re-inserting (avoids stale
		// keys when a server's transport changes, including `type`).
		for k in transport_keys(false) {
			entry.remove(*k);
		}
		for (key, value) in transport_fields(&mcp.transport, false) {
			entry.insert(key.to_string(), field_to_value(value));
		}
		entry.insert("enabled".to_string(), Value::Boolean(mcp.enabled));
		servers.insert(mcp.name.clone(), Value::Table(entry));
	}
	root_map.insert("mcp_servers".to_string(), Value::Table(servers));
	toml::to_string(&root).map_err(|e| {
		ConfigError::InvalidConfig(format!(
			"failed to serialize Grok config: {e}"
		))
	})
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::models::{AgentConfig, McpServer, McpTransport};
	use std::collections::HashMap;

	#[test]
	fn parse_stdio_http_and_sse() {
		let toml = r#"
[mcp_servers.withenv]
command = "mycmd"
args = ["--flag"]
enabled = true
[mcp_servers.withenv.env]
TOKEN = "abc"

[mcp_servers.ssesrv]
url = "https://mcp.example.com/sse"
type = "sse"
enabled = true
[mcp_servers.ssesrv.headers]
Authorization = "Bearer xyz"

[mcp_servers.httpsrv]
url = "https://mcp.example.com/mcp"
enabled = true
"#;
		let cfg = parse(toml).unwrap();
		assert_eq!(cfg.mcps.len(), 3);

		let withenv = cfg.mcps.iter().find(|m| m.name == "withenv").unwrap();
		match &withenv.transport {
			McpTransport::Stdio {
				command, args, env, ..
			} => {
				assert_eq!(command, "mycmd");
				assert_eq!(args, &["--flag".to_string()]);
				assert_eq!(
					env.as_ref().unwrap().get("TOKEN").map(String::as_str),
					Some("abc")
				);
			}
			_ => panic!("expected Stdio"),
		}

		let ssesrv = cfg.mcps.iter().find(|m| m.name == "ssesrv").unwrap();
		match &ssesrv.transport {
			McpTransport::Sse { url, headers, .. } => {
				assert_eq!(url, "https://mcp.example.com/sse");
				assert_eq!(
					headers
						.as_ref()
						.unwrap()
						.get("Authorization")
						.map(String::as_str),
					Some("Bearer xyz")
				);
			}
			_ => panic!("expected Sse"),
		}

		let httpsrv = cfg.mcps.iter().find(|m| m.name == "httpsrv").unwrap();
		assert!(matches!(
			httpsrv.transport,
			McpTransport::StreamableHttp { .. }
		));
	}

	#[test]
	fn parse_enabled_flag() {
		let toml = r#"
[mcp_servers.on]
command = "a"
[mcp_servers.off]
command = "b"
enabled = false
"#;
		let cfg = parse(toml).unwrap();
		assert!(cfg.mcps.iter().find(|m| m.name == "on").unwrap().enabled);
		assert!(!cfg.mcps.iter().find(|m| m.name == "off").unwrap().enabled);
	}

	#[test]
	fn parse_rejects_non_table_servers() {
		assert!(parse("mcp_servers = 5\n").is_err());
	}

	#[test]
	fn parse_rejects_non_table_entry() {
		assert!(parse("[mcp_servers]\nbad = 5\n").is_err());
	}

	#[test]
	fn parse_rejects_entry_without_command_or_url() {
		assert!(parse("[mcp_servers.bad]\ntimeout = 5\n").is_err());
	}

	#[test]
	fn parse_empty_is_ok() {
		assert!(parse("").unwrap().mcps.is_empty());
		assert!(parse("[cli]\nfoo = true\n").unwrap().mcps.is_empty());
	}

	#[test]
	fn serialize_preserves_other_top_level_keys() {
		let original = r#"
[cli]
model = "gpt-x"

[ui]
theme = "dark"

[mcp_servers.old]
command = "c"
"#;
		let cfg = AgentConfig {
			mcps: vec![McpServer::new(
				"srv",
				McpTransport::stdio("run", vec![]),
			)],
			skills: vec![],
			sub_agents: vec![],
		};
		let out = serialize(&cfg, Some(original)).unwrap();
		let v: Value = toml::from_str(&out).unwrap();
		assert_eq!(
			v.get("cli").unwrap().get("model").unwrap().as_str(),
			Some("gpt-x")
		);
		assert_eq!(
			v.get("ui").unwrap().get("theme").unwrap().as_str(),
			Some("dark")
		);
		let servers = v.get("mcp_servers").unwrap();
		assert!(servers.get("srv").is_some());
		assert!(servers.get("old").is_none());
	}

	#[test]
	fn serialize_preserves_per_server_extra_fields() {
		let original = r#"
[mcp_servers.srv]
command = "old"
timeout = 120
sampling = true
"#;
		let cfg = AgentConfig {
			mcps: vec![McpServer::new(
				"srv",
				McpTransport::stdio("newcmd", vec![]),
			)],
			skills: vec![],
			sub_agents: vec![],
		};
		let out = serialize(&cfg, Some(original)).unwrap();
		let v: Value = toml::from_str(&out).unwrap();
		let srv = v.get("mcp_servers").unwrap().get("srv").unwrap();
		assert_eq!(srv.get("command").unwrap().as_str(), Some("newcmd"));
		assert_eq!(srv.get("timeout").unwrap().as_integer(), Some(120));
		assert!(srv.get("sampling").is_some());
	}

	#[test]
	fn serialize_removes_stale_transport_keys_on_switch() {
		// stdio server re-saved as remote must not keep command/args
		let original = r#"
[mcp_servers.srv]
command = "old"
args = ["a"]
"#;
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
		let v: Value = toml::from_str(&out).unwrap();
		let srv = v.get("mcp_servers").unwrap().get("srv").unwrap();
		assert_eq!(srv.get("url").unwrap().as_str(), Some("https://x/mcp"));
		assert!(srv.get("command").is_none());
		assert!(srv.get("args").is_none());
		assert!(srv.get("type").is_none());
	}

	#[test]
	fn serialize_sse_emits_type_sse() {
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
		let v: Value = toml::from_str(&out).unwrap();
		let srv = v.get("mcp_servers").unwrap().get("legacy").unwrap();
		assert_eq!(srv.get("url").unwrap().as_str(), Some("https://x/sse"));
		assert_eq!(srv.get("type").unwrap().as_str(), Some("sse"));
	}

	#[test]
	fn serialize_http_has_no_type_key() {
		let cfg = AgentConfig {
			mcps: vec![McpServer::new(
				"http",
				McpTransport::StreamableHttp {
					url: "https://x/mcp".into(),
					headers: None,
					timeout: None,
				},
			)],
			skills: vec![],
			sub_agents: vec![],
		};
		let out = serialize(&cfg, None).unwrap();
		let v: Value = toml::from_str(&out).unwrap();
		let srv = v.get("mcp_servers").unwrap().get("http").unwrap();
		assert_eq!(srv.get("url").unwrap().as_str(), Some("https://x/mcp"));
		assert!(srv.get("type").is_none());
	}

	#[test]
	fn serialize_removes_type_when_switching_sse_to_http() {
		let original = r#"
[mcp_servers.srv]
url = "https://x/sse"
type = "sse"
enabled = true
"#;
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
		let v: Value = toml::from_str(&out).unwrap();
		let srv = v.get("mcp_servers").unwrap().get("srv").unwrap();
		assert!(srv.get("type").is_none());
		assert_eq!(srv.get("url").unwrap().as_str(), Some("https://x/mcp"));
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
		let v: Value = toml::from_str(&out).unwrap();
		let srv = v.get("mcp_servers").unwrap().get("srv").unwrap();
		assert_eq!(srv.get("enabled").unwrap().as_bool(), Some(false));
	}

	#[test]
	fn roundtrip_stable() {
		let toml = r#"
[mcp_servers.a]
command = "x"
args = ["1"]
enabled = true
"#;
		let cfg = parse(toml).unwrap();
		let out = serialize(&cfg, Some(toml)).unwrap();
		let cfg2 = parse(&out).unwrap();
		assert_eq!(cfg.mcps.len(), cfg2.mcps.len());
		assert_eq!(cfg.mcps[0].name, cfg2.mcps[0].name);
		assert!(matches!(cfg2.mcps[0].transport, McpTransport::Stdio { .. }));
	}

	#[test]
	fn roundtrip_sse_and_http_distinct() {
		let mut headers = HashMap::new();
		headers.insert("Authorization".into(), "Bearer t".into());
		let cfg = AgentConfig {
			mcps: vec![
				McpServer::new(
					"sse",
					McpTransport::Sse {
						url: "https://x/sse".into(),
						headers: Some(headers.clone()),
						timeout: None,
					},
				),
				McpServer::new(
					"http",
					McpTransport::StreamableHttp {
						url: "https://x/mcp".into(),
						headers: Some(headers),
						timeout: None,
					},
				),
			],
			skills: vec![],
			sub_agents: vec![],
		};
		let out = serialize(&cfg, None).unwrap();
		let cfg2 = parse(&out).unwrap();
		let sse = cfg2.mcps.iter().find(|m| m.name == "sse").unwrap();
		let http = cfg2.mcps.iter().find(|m| m.name == "http").unwrap();
		assert!(matches!(sse.transport, McpTransport::Sse { .. }));
		assert!(matches!(
			http.transport,
			McpTransport::StreamableHttp { .. }
		));
	}

	#[test]
	fn parse_rejects_non_string_command() {
		assert!(parse("[mcp_servers.bad]\ncommand = 5\n").is_err());
	}

	#[test]
	fn parse_rejects_non_bool_enabled() {
		assert!(
			parse("[mcp_servers.bad]\ncommand = \"x\"\nenabled = 5\n").is_err()
		);
	}

	#[test]
	fn parse_rejects_non_string_args_element() {
		assert!(parse("[mcp_servers.bad]\ncommand = \"x\"\nargs = [1, 2]\n")
			.is_err());
	}

	#[test]
	fn parse_rejects_non_string_env_value() {
		assert!(parse(
			"[mcp_servers.bad]\ncommand = \"x\"\n[mcp_servers.bad.env]\nA = 1\n"
		)
		.is_err());
	}

	#[test]
	fn parse_rejects_non_string_url() {
		assert!(parse("[mcp_servers.bad]\nurl = 5\n").is_err());
	}

	#[test]
	fn serialize_rejects_non_table_existing_mcp_servers() {
		let cfg = AgentConfig {
			mcps: vec![McpServer::new("s", McpTransport::stdio("c", vec![]))],
			skills: vec![],
			sub_agents: vec![],
		};
		assert!(serialize(&cfg, Some("mcp_servers = 5\n")).is_err());
	}

	#[test]
	fn serialize_rejects_non_table_existing_entry() {
		let cfg = AgentConfig {
			mcps: vec![McpServer::new("s", McpTransport::stdio("c", vec![]))],
			skills: vec![],
			sub_agents: vec![],
		};
		assert!(serialize(&cfg, Some("[mcp_servers]\ns = 5\n")).is_err());
	}

	#[test]
	fn parse_http_type_is_streamable() {
		let cfg = parse(
			"[mcp_servers.s]\nurl = \"https://x/mcp\"\ntype = \"http\"\n",
		)
		.unwrap();
		assert!(matches!(
			cfg.mcps[0].transport,
			McpTransport::StreamableHttp { .. }
		));
	}

	#[test]
	fn parse_rejects_unknown_type() {
		assert!(parse(
			"[mcp_servers.s]\nurl = \"https://x\"\ntype = \"weird\"\n"
		)
		.is_err());
	}

	#[test]
	fn parse_rejects_non_string_type() {
		assert!(
			parse("[mcp_servers.s]\nurl = \"https://x\"\ntype = 5\n").is_err()
		);
	}

	#[test]
	fn parse_rejects_mixed_transport_keys() {
		assert!(parse(
			"[mcp_servers.s]\ncommand = \"x\"\nurl = \"https://y\"\n"
		)
		.is_err());
	}

	#[test]
	fn parse_rejects_scalar_args() {
		assert!(parse("[mcp_servers.s]\ncommand = \"x\"\nargs = 5\n").is_err());
	}

	#[test]
	fn parse_rejects_non_table_env() {
		assert!(parse("[mcp_servers.s]\ncommand = \"x\"\nenv = 5\n").is_err());
	}
}
