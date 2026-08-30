use crate::format::mcp_policy::{
	reject_mixed_transport, MixedWording, OwnedKeys, TransportVocabulary,
};
use crate::{
	errors::{ConfigError, Result},
	models::{AgentConfig, McpServer, McpTransport},
};
use std::collections::HashMap;

/// Codex has NO transport tag at all — a remote entry is a bare `url` — and so
/// no word for SSE either. Every field being empty is the honest description of
/// that format, and `sse` being empty is what makes `refuse_unwritable` refuse.
///
/// Only `sse` is read here; the other three have no call site, and that is the
/// point rather than an oversight. An EMPTY field claims nothing, so it cannot
/// come to disagree with the parser the way `single_remote: true` did in three
/// modules at once. Give Codex a tag and the fields stop being empty and start
/// being read on the same edit.
const VOCAB: TransportVocabulary = TransportVocabulary {
	tag_key: "",
	stdio: "",
	sse: "",
	http: "",
	http_read_aliases: &[],
};

/// Deliberately ASYMMETRIC: writing a remote server drops `cwd` and the
/// experimental env keys, while writing a stdio server keeps them and drops the
/// remote auth family instead. Each arm strips the OPPOSITE family, so these two
/// lists are never unioned.
const OWNED: OwnedKeys = OwnedKeys {
	stdio: &[
		"command",
		"args",
		"env",
		"env_vars",
		"cwd",
		"experimental_environment",
	],
	remote: &[
		"url",
		"http_headers",
		"env_http_headers",
		"bearer_token_env_var",
		"auth",
	],
};

fn parse_toml(content: &str) -> Result<toml::Value> {
	toml::from_str(content).map_err(|e| {
		ConfigError::InvalidConfig(format!("Failed to parse TOML: {e}"))
	})
}

/// Codex writes these as strings; a scalar in their place is a user typo worth
/// coercing. Anything else is DROPPED data — and `serialize` keeps only the
/// names it was handed, so a dropped field is a field the next save erases.
fn scalar_to_string(value: &toml::Value) -> Option<String> {
	match value {
		toml::Value::String(text) => Some(text.clone()),
		toml::Value::Integer(number) => Some(number.to_string()),
		toml::Value::Float(number) => Some(number.to_string()),
		toml::Value::Boolean(flag) => Some(flag.to_string()),
		_ => None,
	}
}

fn string_list(
	value: &toml::Value,
	name: &str,
	field: &str,
) -> Result<Vec<String>> {
	let values = value.as_array().ok_or_else(|| {
		ConfigError::InvalidConfig(format!(
			"MCP server '{name}' field '{field}' must be an array"
		))
	})?;
	values
		.iter()
		.map(|value| {
			scalar_to_string(value).ok_or_else(|| {
				ConfigError::InvalidConfig(format!(
					"MCP server '{name}' field '{field}' must contain only scalars"
				))
			})
		})
		.collect()
}

fn string_map(
	value: &toml::Value,
	name: &str,
	field: &str,
) -> Result<HashMap<String, String>> {
	let values = value.as_table().ok_or_else(|| {
		ConfigError::InvalidConfig(format!(
			"MCP server '{name}' field '{field}' must be a table"
		))
	})?;
	values
		.iter()
		.map(|(key, value)| {
			scalar_to_string(value)
				.map(|value| (key.clone(), value))
				.ok_or_else(|| {
					ConfigError::InvalidConfig(format!(
						"MCP server '{name}' field '{field}'.'{key}' must be a scalar"
					))
				})
		})
		.collect()
}

pub fn parse(content: &str) -> Result<AgentConfig> {
	let doc = parse_toml(content)?;
	let mut config = AgentConfig::new();

	let Some(servers) = doc.get("mcp_servers").and_then(|v| v.as_table())
	else {
		return Ok(config);
	};

	// Skipping a server aghub cannot read is not harmless: `serialize` retains
	// only the names it was handed, so the next unrelated save DELETES the
	// skipped entry from the user's config. Reject the file instead.
	for (name, entry) in servers {
		let table = entry.as_table().ok_or_else(|| {
			ConfigError::InvalidConfig(format!(
				"MCP server '{name}' is not a table"
			))
		})?;
		let command = match table.get("command") {
			None => None,
			Some(value) => Some(value.as_str().ok_or_else(|| {
				ConfigError::InvalidConfig(format!(
					"MCP server '{name}' field 'command' must be a string"
				))
			})?),
		};
		let url = match table.get("url") {
			None => None,
			Some(value) => Some(value.as_str().ok_or_else(|| {
				ConfigError::InvalidConfig(format!(
					"MCP server '{name}' field 'url' must be a string"
				))
			})?),
		};
		// Presence here means "present AND a string": the per-field type errors
		// above already fired, so this keeps Codex's original precedence while
		// the message itself is derived from the two keys actually probed.
		reject_mixed_transport(
			&["command"],
			&["url"],
			|key| match key {
				"command" => command.is_some(),
				"url" => url.is_some(),
				_ => false,
			},
			name,
			MixedWording::NamesTheProbedKeys("Codex"),
		)?;
		if command.is_none() && url.is_none() {
			return Err(ConfigError::InvalidConfig(format!(
				"MCP server '{name}' has neither command nor url"
			)));
		}
		// Same rule as the fields above: a value read as `None` is a value the
		// next save DELETES, so an unreadable timeout is an error.
		let timeout = match table.get("tool_timeout_sec") {
			None => None,
			Some(value) => Some(
				value
					.as_integer()
					.and_then(|value| u64::try_from(value).ok())
					.ok_or_else(|| {
						ConfigError::InvalidConfig(format!(
							"MCP server '{name}' field 'tool_timeout_sec' must be a non-negative integer"
						))
					})?,
			),
		};
		let enabled = match table.get("enabled") {
			None => true,
			Some(value) => value.as_bool().ok_or_else(|| {
				ConfigError::InvalidConfig(format!(
					"MCP server '{name}' field 'enabled' must be a boolean"
				))
			})?,
		};
		let args = match table.get("args") {
			None => Vec::new(),
			Some(value) => string_list(value, name, "args")?,
		};
		let env = match table.get("env") {
			None => None,
			Some(value) => Some(string_map(value, name, "env")?),
		}
		.filter(|map: &HashMap<String, String>| !map.is_empty());
		let headers = match table.get("http_headers") {
			None => None,
			Some(value) => Some(string_map(value, name, "http_headers")?),
		}
		.filter(|map: &HashMap<String, String>| !map.is_empty());

		let transport = match (command, url) {
			(Some(command), None) => McpTransport::Stdio {
				command: command.to_string(),
				args,
				env,
				timeout,
			},
			(None, Some(url)) => McpTransport::StreamableHttp {
				url: url.to_string(),
				headers,
				timeout,
			},
			// Both guarded above, before any field was extracted.
			(None, None) | (Some(_), Some(_)) => unreachable!(),
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
	original_content: Option<&str>,
) -> Result<String> {
	let mut doc = match original_content {
		Some(c) if !c.trim().is_empty() => parse_toml(c)?,
		_ => toml::Value::Table(toml::map::Map::new()),
	};

	let root = doc.as_table_mut().ok_or_else(|| {
		ConfigError::InvalidConfig("root is not a table".into())
	})?;

	// Get or create the mcp_servers table.
	if !root.contains_key("mcp_servers") {
		root.insert(
			"mcp_servers".to_string(),
			toml::Value::Table(toml::map::Map::new()),
		);
	}
	let servers = root
		.get_mut("mcp_servers")
		.and_then(|v| v.as_table_mut())
		.ok_or_else(|| {
			ConfigError::InvalidConfig("mcp_servers is not a table".into())
		})?;

	// Collect the names aghub manages this round.
	let managed: std::collections::HashSet<String> =
		config.mcps.iter().map(|m| m.name.clone()).collect();

	// Remove servers that aghub no longer tracks.
	servers.retain(|name, _| managed.contains(name));

	// Upsert each server: merge aghub fields into existing entry.
	for mcp in &config.mcps {
		let entry = servers
			.entry(&mcp.name)
			.or_insert_with(|| toml::Value::Table(toml::map::Map::new()));
		let table = match entry.as_table_mut() {
			Some(t) => t,
			None => continue,
		};

		if mcp.enabled {
			table.remove("enabled");
		} else {
			table.insert("enabled".to_string(), toml::Value::Boolean(false));
		}

		// `VOCAB.sse` is empty — Codex's remote entry is a bare `url` with no
		// transport tag, so an SSE server written here would come back as
		// streamable HTTP. Placed at the match so a non-table existing entry
		// is still skipped first, exactly as before.
		VOCAB.refuse_unwritable(
			&mcp.transport,
			&format!("MCP server '{}'", mcp.name),
		)?;

		match &mcp.transport {
			McpTransport::Stdio {
				command,
				args,
				env,
				timeout,
			} => {
				table.remove("type");
				for key in OWNED.remote {
					table.remove(*key);
				}
				table.insert(
					"command".to_string(),
					toml::Value::String(command.clone()),
				);
				if args.is_empty() {
					table.remove("args");
				} else {
					table.insert(
						"args".to_string(),
						toml::Value::Array(
							args.iter()
								.map(|arg| toml::Value::String(arg.clone()))
								.collect(),
						),
					);
				}
				match env {
					Some(env) if !env.is_empty() => {
						table.insert(
							"env".to_string(),
							toml::Value::Table(
								env.iter()
									.map(|(key, value)| {
										(
											key.clone(),
											toml::Value::String(value.clone()),
										)
									})
									.collect(),
							),
						);
					}
					_ => {
						table.remove("env");
					}
				}
				set_timeout(table, *timeout)?;
			}
			// Unreachable: `refuse_unwritable` returned just above while
			// `VOCAB.sse` is empty. It stays an explicit error rather than a
			// panic so that giving Codex an SSE spelling is a deliberate edit
			// here, caught by `mcp_dialect_roundtrip`'s NO_NATIVE_SSE list.
			McpTransport::Sse { .. } => {
				return Err(ConfigError::InvalidConfig(format!(
					"MCP server '{name}' has an unwritable transport",
					name = mcp.name
				)));
			}
			McpTransport::StreamableHttp {
				url,
				headers,
				timeout,
			} => {
				table.remove("type");
				for key in OWNED.stdio {
					table.remove(*key);
				}
				table.insert(
					"url".to_string(),
					toml::Value::String(url.clone()),
				);
				match headers {
					Some(headers) if !headers.is_empty() => {
						table.insert(
							"http_headers".to_string(),
							toml::Value::Table(
								headers
									.iter()
									.map(|(key, value)| {
										(
											key.clone(),
											toml::Value::String(value.clone()),
										)
									})
									.collect(),
							),
						);
					}
					_ => {
						table.remove("http_headers");
					}
				}
				set_timeout(table, *timeout)?;
			}
		}
	}

	// Remove empty mcp_servers table.
	if servers.is_empty() {
		root.remove("mcp_servers");
	}

	toml::to_string_pretty(&doc)
		.map_err(|e| ConfigError::InvalidConfig(e.to_string()))
}

fn set_timeout(
	table: &mut toml::map::Map<String, toml::Value>,
	timeout: Option<u64>,
) -> Result<()> {
	match timeout {
		Some(timeout) => {
			let timeout = i64::try_from(timeout).map_err(|_| {
				ConfigError::InvalidConfig(
					"MCP timeout exceeds TOML integer range".into(),
				)
			})?;
			table.insert(
				"tool_timeout_sec".to_string(),
				toml::Value::Integer(timeout),
			);
		}
		None => {
			table.remove("tool_timeout_sec");
		}
	}
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::models::{McpServer, McpTransport};

	#[test]
	fn parse_basic_servers() {
		let content = r#"
model = "o3"

[mcp_servers.filesystem]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]

[mcp_servers.chrome]
command = "/usr/local/bin/chrome-mcp"
env = { DISPLAY = ":0" }
"#;
		let config = parse(content).unwrap();
		assert_eq!(config.mcps.len(), 2);
		let fs = config.mcps.iter().find(|m| m.name == "filesystem").unwrap();
		match &fs.transport {
			McpTransport::Stdio { command, args, .. } => {
				assert_eq!(command, "npx");
				assert_eq!(args.len(), 3);
			}
			_ => panic!("Expected Stdio"),
		}
	}

	#[test]
	fn parse_codex_format_with_type_and_tools() {
		let content = r#"
[mcp_servers.pencil]
type = "stdio"
command = "/path/to/pencil"
args = ["--app", "desktop"]

[mcp_servers.playwright]
command = "npx"
args = ["@playwright/mcp@latest"]

[mcp_servers.playwright.tools.browser_navigate]
approval_mode = "approve"

[mcp_servers.playwright.tools.browser_click]
approval_mode = "approve"
"#;
		let config = parse(content).unwrap();
		assert_eq!(config.mcps.len(), 2);

		let pencil = config.mcps.iter().find(|m| m.name == "pencil").unwrap();
		match &pencil.transport {
			McpTransport::Stdio { command, args, .. } => {
				assert_eq!(command, "/path/to/pencil");
				assert_eq!(args, &["--app", "desktop"]);
			}
			_ => panic!("Expected Stdio"),
		}
	}

	#[test]
	fn roundtrip_preserves_non_mcp_fields() {
		let original = r#"
model_provider = "custom"
model = "gpt-5.4"

[mcp_servers.old]
command = "old-cmd"
"#;
		let config = parse(original).unwrap();
		let mut updated = config;
		updated.mcps.clear();
		updated.mcps.push(McpServer::new(
			"new-mcp",
			McpTransport::stdio("new-cmd", vec![]),
		));
		let output = serialize(&updated, Some(original)).unwrap();
		assert!(output.contains("model_provider"));
		assert!(output.contains("gpt-5.4"));
		assert!(!output.contains("old-cmd"));
		assert!(output.contains("new-mcp"));
	}

	#[test]
	fn roundtrip_preserves_tools_and_type() {
		let original = r#"
[mcp_servers.playwright]
command = "npx"
args = ["@playwright/mcp@latest"]

[mcp_servers.playwright.tools.browser_navigate]
approval_mode = "approve"

[mcp_servers.playwright.tools.browser_click]
approval_mode = "approve"
"#;
		let config = parse(original).unwrap();
		let output = serialize(&config, Some(original)).unwrap();
		assert!(output.contains("browser_navigate"));
		assert!(output.contains("browser_click"));
		assert!(output.contains("approval_mode"));
	}

	#[test]
	fn switching_transport_removes_stale_type_discriminator() {
		let original = r#"
[mcp_servers.server]
type = "stdio"
command = "old-command"
"#;
		let config = AgentConfig {
			mcps: vec![McpServer::new(
				"server",
				McpTransport::streamable_http("https://example.com/mcp"),
			)],
			skills: vec![],
			sub_agents: vec![],
		};
		let output = serialize(&config, Some(original)).unwrap();
		let parsed: toml::Value = toml::from_str(&output).unwrap();
		let server = &parsed["mcp_servers"]["server"];
		assert!(server.get("type").is_none());
		assert_eq!(
			server["url"],
			toml::Value::String("https://example.com/mcp".into())
		);
		assert!(server.get("command").is_none());
	}

	#[test]
	fn add_server_preserves_existing_tools() {
		let original = r#"
[mcp_servers.playwright]
command = "npx"
args = ["@playwright/mcp@latest"]

[mcp_servers.playwright.tools.browser_navigate]
approval_mode = "approve"
"#;
		let mut config = parse(original).unwrap();
		config.mcps.push(McpServer::new(
			"new-mcp",
			McpTransport::stdio("new-cmd", vec!["arg1".into()]),
		));
		let output = serialize(&config, Some(original)).unwrap();
		// New server added.
		assert!(output.contains("new-mcp"));
		assert!(output.contains("new-cmd"));
		// Existing tools preserved.
		assert!(output.contains("browser_navigate"));
		assert!(output.contains("approval_mode"));
	}

	#[test]
	fn no_mcp_servers_section_parses_empty() {
		let content = r#"
model = "gpt-5.4"
"#;
		let config = parse(content).unwrap();
		assert!(config.mcps.is_empty());
	}

	#[test]
	fn parse_rejects_a_subfield_it_cannot_read_and_coerces_scalars() {
		// A dropped subfield is a DELETED subfield: `serialize` rewrites the
		// entry from what parse produced.
		let coerced = parse(
			r#"
[mcp_servers.ok]
command = "echo"
args = ["--port", 8080]
env = { PORT = 3000, DEBUG = true }
"#,
		)
		.unwrap();
		match &coerced.mcps[0].transport {
			McpTransport::Stdio { args, env, .. } => {
				assert_eq!(args, &["--port".to_string(), "8080".to_string()]);
				let env = env.as_ref().unwrap();
				assert_eq!(env.get("PORT").unwrap(), "3000");
				assert_eq!(env.get("DEBUG").unwrap(), "true");
			}
			other => panic!("expected stdio, got {other:?}"),
		}

		for bad in [
			"[mcp_servers.a]\ncommand = \"echo\"\nargs = [[\"nested\"]]\n",
			"[mcp_servers.a]\ncommand = \"echo\"\nenv = { K = [1] }\n",
			"[mcp_servers.a]\ncommand = \"echo\"\nenabled = \"false\"\n",
			"[mcp_servers.a]\nurl = \"https://h/mcp\"\nhttp_headers = { R = [1] }\n",
			"[mcp_servers.a]\ncommand = \"echo\"\ntool_timeout_sec = \"30\"\n",
			"[mcp_servers.a]\ncommand = 123\n",
			"[mcp_servers.a]\nother = true\n",
		] {
			assert!(parse(bad).is_err(), "accepted: {bad}");
		}
	}

	#[test]
	fn parse_codex_streamable_http_and_disabled_state() {
		let content = r#"
[mcp_servers.figma]
url = "https://mcp.figma.com/mcp"
http_headers = { "X-Figma-Region" = "us-east-1" }
bearer_token_env_var = "FIGMA_OAUTH_TOKEN"
tool_timeout_sec = 45
enabled = false
"#;

		let config = parse(content).unwrap();
		let server = &config.mcps[0];
		assert!(!server.enabled);
		assert_eq!(
			server.transport,
			McpTransport::StreamableHttp {
				url: "https://mcp.figma.com/mcp".into(),
				headers: Some(HashMap::from([(
					"X-Figma-Region".into(),
					"us-east-1".into(),
				)])),
				timeout: Some(45),
			}
		);
	}

	#[test]
	fn serialize_codex_streamable_http_preserves_auth_options() {
		let original = r#"
[mcp_servers.figma]
url = "https://old.example.com/mcp"
bearer_token_env_var = "FIGMA_OAUTH_TOKEN"
auth = "oauth"
required = true
enabled = false
"#;
		let config = AgentConfig {
			mcps: vec![McpServer {
				name: "figma".into(),
				enabled: true,
				transport: McpTransport::StreamableHttp {
					url: "https://mcp.figma.com/mcp".into(),
					headers: Some(HashMap::from([(
						"X-Figma-Region".into(),
						"us-east-1".into(),
					)])),
					timeout: Some(45),
				},
				timeout: None,
				config_source: None,
			}],
			skills: vec![],
			sub_agents: vec![],
		};

		let output = serialize(&config, Some(original)).unwrap();
		let value: toml::Value = toml::from_str(&output).unwrap();
		let server = &value["mcp_servers"]["figma"];
		assert_eq!(server["url"].as_str(), Some("https://mcp.figma.com/mcp"));
		assert_eq!(
			server["http_headers"]["X-Figma-Region"].as_str(),
			Some("us-east-1")
		);
		assert_eq!(server["tool_timeout_sec"].as_integer(), Some(45));
		assert_eq!(
			server["bearer_token_env_var"].as_str(),
			Some("FIGMA_OAUTH_TOKEN")
		);
		assert_eq!(server["auth"].as_str(), Some("oauth"));
		assert_eq!(server["required"].as_bool(), Some(true));
		assert!(server.get("command").is_none());
		assert!(server.get("enabled").is_none());
	}

	/// A transport switch must leave NONE of the other family's keys behind: a
	/// stale `cwd` or `bearer_token_env_var` is read back as a live setting for
	/// a transport that no longer exists.
	///
	/// The key names here are LITERALS on purpose. Iterating `OWNED` would make
	/// the test agree with whatever the declaration happens to say — deleting a
	/// name would delete its own check, which is exactly the false green this
	/// test exists to prevent.
	#[test]
	fn switching_transport_strips_every_key_the_other_family_owns() {
		const STDIO_OWNED: &[&str] = &[
			"command",
			"args",
			"env",
			"env_vars",
			"cwd",
			"experimental_environment",
		];
		const REMOTE_OWNED: &[&str] = &[
			"url",
			"http_headers",
			"env_http_headers",
			"bearer_token_env_var",
			"auth",
		];

		fn only(name: &str, transport: McpTransport) -> AgentConfig {
			AgentConfig {
				mcps: vec![McpServer::new(name, transport)],
				skills: vec![],
				sub_agents: vec![],
			}
		}
		fn saved(original: &str, config: &AgentConfig) -> toml::Value {
			let out = serialize(config, Some(original)).expect("serialize");
			toml::from_str(&out).expect("re-parse")
		}

		// Every stdio-family key present, re-saved as remote.
		let original = "[mcp_servers.srv]\ncommand = \"old\"\nargs = \
[\"a\"]\nenv_vars = \"x\"\ncwd = \"/tmp\"\nexperimental_environment = \
\"x\"\n\n[mcp_servers.srv.env]\nK = \"V\"\n";
		let value = saved(
			original,
			&only("srv", McpTransport::streamable_http("https://x/mcp")),
		);
		for key in STDIO_OWNED {
			assert!(
				value["mcp_servers"]["srv"].get(key).is_none(),
				"stdio key `{key}` survived a switch to remote"
			);
		}

		// Every remote-family key present, re-saved as stdio.
		let original = "[mcp_servers.srv]\nurl = \"https://x/mcp\"\n\
env_http_headers = \"x\"\nbearer_token_env_var = \"TOK\"\n\n\
[mcp_servers.srv.http_headers]\nH = \"1\"\n\n[mcp_servers.srv.auth]\n\
kind = \"oauth\"\n";
		let value =
			saved(original, &only("srv", McpTransport::stdio("run", vec![])));
		for key in REMOTE_OWNED {
			assert!(
				value["mcp_servers"]["srv"].get(key).is_none(),
				"remote key `{key}` survived a switch to stdio"
			);
		}
	}

	/// Codex probes `command` × `url` and NOTHING else, and the narrowness is
	/// the deliberate half of `mcp_policy`'s probe rule: each arm strips only
	/// the OPPOSITE family, so an inert remote key on a stdio entry costs
	/// nothing to keep reading. Widening the probe (adding `http_headers`,
	/// say) would refuse this file — and parse failure is whole-document, so
	/// EVERY Codex MCP server would become unmanageable over a key that does
	/// nothing for the transport the entry declares.
	#[test]
	fn a_stdio_entry_with_an_inert_remote_key_stays_readable() {
		let config = parse(
			"[mcp_servers.a]\ncommand = \"run\"\n\n\
[mcp_servers.a.http_headers]\nX = \"Y\"\n",
		)
		.expect("widening the remote probe would refuse this whole file");
		assert!(matches!(
			config.mcps[0].transport,
			McpTransport::Stdio { .. }
		));
	}
}
