//! Map-based MCP configuration (`{"mcpServers": {...}}` and friends).
//!
//! A dialect is declared ONCE as a [`Dialect`] and drives BOTH [`parse`] and
//! [`serialize`]. That pairing is the whole point: when the two halves were
//! configured separately, an agent could parse a transport (or a toggle) it had
//! no way to write back, and the next save silently rewrote the user's config
//! into a different transport. A dialect with no native word for a transport
//! now refuses to write it instead of quietly downgrading it, and never parses
//! into one either.

use crate::format::mcp_policy::{
	reject_mixed_transport, MixedWording, TransportVocabulary,
};
use crate::{
	errors::{ConfigError, Result},
	models::{AgentConfig, McpServer, McpTransport},
};
use aghub_json::{parse_jsonc_opt, patch_jsonc_object};
use serde::Deserialize;
use std::collections::HashMap;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ToggleKey {
	None,
	Enabled,
	Disabled,
}

/// How a remote server with a URL but no explicit transport tag is read.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum UntypedRemote {
	InferSseFromUrl,
	StreamableHttp,
}

/// One agent's map dialect: where the servers live and how each field is spelled.
#[derive(Clone, Copy)]
pub struct Dialect {
	/// Dotted path to the server map (`"mcpServers"`, `"amp.mcpServers"`, …).
	pub server_key: &'static str,
	/// The transport words, in the SAME type the seven hand-written dialects
	/// declare. This used to be an `Option<Discriminator>` — the same fields
	/// under different names, with its own `writes_sse` — and the `None` case
	/// is now what it always meant: an all-empty vocabulary.
	pub vocab: TransportVocabulary,
	/// The URL key this dialect WRITES.
	pub url_key: &'static str,
	/// URL keys it also READS and STRIPS: spellings an older aghub wrote for
	/// this agent. A key in NEITHER list is foreign — never read, never removed,
	/// never type-checked. Hijacking an entry on a key the vendor did not define
	/// is how a Windsurf `serverUrl` server ends up pointing at someone else's
	/// endpoint, and stripping one deletes the user's data.
	pub legacy_url_keys: &'static [&'static str],
	/// A URL key whose PRESENCE declares streamable HTTP. Only Gemini has one
	/// (`httpUrl`), and Gemini consults it before `url` and before any `type`.
	pub http_url_key: Option<&'static str>,
	pub env_key: &'static str,
	pub toggle_key: ToggleKey,
	pub untyped_remote: UntypedRemote,
}

impl Dialect {
	/// Whether the dialect has a persisted per-server on/off field.
	pub const fn writes_toggle(&self) -> bool {
		!matches!(self.toggle_key, ToggleKey::None)
	}
}

/// The `mcpServers` + `type: stdio|sse|http` dialect used by most agents.
pub const MCP_SERVERS: Dialect = Dialect {
	server_key: "mcpServers",
	vocab: TransportVocabulary {
		tag_key: "type",
		stdio: "stdio",
		sse: "sse",
		http: "http",
		// The universal set EVERY json_map agent reads, whatever its own `http`
		// spelling is: Cline writes `streamableHttp` and still reads `http`, so
		// this list is shared, not narrowed per dialect.
		http_read_aliases: &["http", "streamable-http", "streamableHttp"],
	},
	url_key: "url",
	legacy_url_keys: &[],
	http_url_key: None,
	env_key: "env",
	toggle_key: ToggleKey::None,
	untyped_remote: UntypedRemote::InferSseFromUrl,
};

/// Map-based MCP server configuration ({"mcpServers": {...}} style)
#[derive(Debug, Deserialize)]
pub(crate) struct MapMcpServer {
	#[serde(rename = "type", default)]
	pub server_type: Option<String>,
	#[serde(default)]
	pub transport: Option<serde_json::Value>,
	pub command: Option<String>,
	#[serde(default)]
	pub args: Vec<serde_json::Value>,
	#[serde(alias = "environment")]
	pub env: Option<HashMap<String, serde_json::Value>>,
	pub headers: Option<HashMap<String, String>>,
	pub enabled: Option<bool>,
	pub disabled: Option<bool>,
	/// Every other key, still raw. URL keys live here so a spelling this
	/// dialect does not own is never type-checked — a foreign `httpUrl` holding
	/// an object must not fail an otherwise valid entry.
	#[serde(flatten)]
	pub rest: serde_json::Map<String, serde_json::Value>,
}

/// The URL this dialect owns, and whether the KEY itself declared HTTP.
fn owned_url(
	rest: &serde_json::Map<String, serde_json::Value>,
	dialect: &Dialect,
	name: &str,
) -> Result<(Option<String>, bool)> {
	let read = |key: &str| -> Result<Option<String>> {
		match rest.get(key) {
			None => Ok(None),
			Some(value) => value
				.as_str()
				.map(|value| Some(value.to_string()))
				.ok_or_else(|| {
					ConfigError::InvalidConfig(format!(
						"MCP server '{name}' field '{key}' must be a string"
					))
				}),
		}
	};
	if let Some(key) = dialect.http_url_key {
		if let Some(url) = read(key)? {
			return Ok((Some(url), true));
		}
	}
	if let Some(url) = read(dialect.url_key)? {
		return Ok((Some(url), false));
	}
	for key in dialect.legacy_url_keys {
		if let Some(url) = read(key)? {
			return Ok((Some(url), false));
		}
	}
	Ok((None, false))
}

fn get_nested<'a>(
	root: &'a serde_json::Value,
	path: &str,
) -> Option<&'a serde_json::Value> {
	path.split('.').try_fold(root, |curr, key| curr.get(key))
}

fn set_nested(
	root: &mut serde_json::Value,
	path: &str,
	value: serde_json::Value,
) -> Result<()> {
	let keys: Vec<&str> = path.split('.').collect();
	if keys.iter().any(|key| key.is_empty()) {
		return Err(ConfigError::InvalidConfig(
			"MCP server key path cannot be empty".into(),
		));
	}
	let mut curr = root;
	for key in &keys[..keys.len() - 1] {
		let obj = curr.as_object_mut().ok_or_else(|| {
			ConfigError::InvalidConfig(format!(
				"MCP server key parent '{key}' is not an object"
			))
		})?;
		curr = obj.entry(*key).or_insert_with(|| {
			serde_json::Value::Object(serde_json::Map::new())
		});
	}
	let obj = curr.as_object_mut().ok_or_else(|| {
		ConfigError::InvalidConfig(
			"MCP server key parent is not an object".into(),
		)
	})?;
	obj.insert(keys[keys.len() - 1].to_string(), value);
	Ok(())
}

/// Agents write scalars the MCP schema calls strings (a port as a bare number
/// is the common one). Coerce those rather than making one stray value cost the
/// user every OTHER server in the file.
fn scalar_to_string(value: &serde_json::Value) -> Option<String> {
	match value {
		serde_json::Value::String(text) => Some(text.clone()),
		serde_json::Value::Number(number) => Some(number.to_string()),
		serde_json::Value::Bool(flag) => Some(flag.to_string()),
		_ => None,
	}
}

fn string_list(
	values: &[serde_json::Value],
	name: &str,
	field: &str,
) -> Result<Vec<String>> {
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
	values: HashMap<String, serde_json::Value>,
	name: &str,
	field: &str,
) -> Result<HashMap<String, String>> {
	values
		.into_iter()
		.map(|(key, value)| {
			scalar_to_string(&value)
				.map(|value| (key.clone(), value))
				.ok_or_else(|| {
					ConfigError::InvalidConfig(format!(
						"MCP server '{name}' field '{field}'.'{key}' must be a scalar"
					))
				})
		})
		.collect()
}

pub fn parse(content: &str, dialect: &Dialect) -> Result<AgentConfig> {
	let root: serde_json::Value = parse_jsonc_opt(content)
		.map_err(|error| ConfigError::InvalidConfig(error.to_string()))?
		.unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new()));
	let mut config = AgentConfig::new();

	let servers_map = match get_nested(&root, dialect.server_key) {
		Some(value) => value.as_object().cloned().ok_or_else(|| {
			ConfigError::InvalidConfig(format!(
				"MCP server key '{key}' is not an object",
				key = dialect.server_key
			))
		})?,
		None => serde_json::Map::new(),
	};

	for (name, mut mcp_val) in servers_map {
		flatten_nested_transport(&mut mcp_val);
		let MapMcpServer {
			server_type,
			transport,
			command,
			args,
			env,
			headers,
			enabled,
			disabled,
			rest,
		} = serde_json::from_value(mcp_val).map_err(|error| {
			ConfigError::InvalidConfig(format!(
				"Invalid MCP server '{name}': {error}"
			))
		})?;
		let (url, declared_http) = owned_url(&rest, dialect, &name)?;
		// Probes the URL SPELLING THIS DIALECT OWNS (`serverUrl`, `httpUrl`, a
		// legacy `url`), which `owned_url` has already resolved — so the probe
		// stays `command` × the one URL, and a per-field "must be a string"
		// error still fires BEFORE this, as it always has.
		reject_mixed_transport(
			&["command"],
			&["url"],
			|key| match key {
				"command" => command.is_some(),
				"url" => url.is_some(),
				_ => false,
			},
			&name,
			MixedWording::CommandAndUrl,
		)?;
		let args = string_list(&args, &name, "args")?;
		let env = env.map(|env| string_map(env, &name, "env")).transpose()?;
		let nested_transport = transport.as_ref().and_then(|value| {
			value.as_str().or_else(|| {
				value.get("type").and_then(serde_json::Value::as_str)
			})
		});
		// The DIALECT'S OWN tag wins. Reading whichever key happens to be
		// present lets a foreign one decide: a Kimi entry carrying both
		// `transport: "http"` (native) and a stray `type: "sse"` would parse as
		// SSE, and the save then rewrites the native key to `sse`.
		let own_tag = match dialect.vocab.tag_key {
			"type" => server_type.as_deref(),
			"transport" => nested_transport,
			// Including `""` — a dialect with no tag at all.
			_ => None,
		};
		let tag = own_tag.or(server_type.as_deref()).or(nested_transport);
		let transport = if declared_http {
			McpTransport::StreamableHttp {
				url: url.expect("httpUrl was present"),
				headers,
				timeout: None,
			}
		} else {
			match tag {
				Some("stdio") => McpTransport::Stdio {
					command: command.ok_or_else(|| {
						ConfigError::InvalidConfig(format!(
							"MCP server '{name}' is missing command"
						))
					})?,
					args,
					env,
					timeout: None,
				},
				Some("sse") => McpTransport::Sse {
					url: url.ok_or_else(|| {
						ConfigError::InvalidConfig(format!(
							"MCP server '{name}' is missing url"
						))
					})?,
					headers,
					timeout: None,
				},
				Some(tag) if dialect.vocab.reads_http(tag) => {
					McpTransport::StreamableHttp {
						url: url.ok_or_else(|| {
							ConfigError::InvalidConfig(format!(
								"MCP server '{name}' is missing url"
							))
						})?,
						headers,
						timeout: None,
					}
				}
				Some("ws" | "websocket") => {
					return Err(ConfigError::InvalidConfig(format!(
					"MCP server '{name}' uses unsupported WebSocket transport"
				)));
				}
				None | Some(_) => {
					if let Some(command) = command {
						McpTransport::Stdio {
							command,
							args,
							env,
							timeout: None,
						}
					} else if let Some(url) = url {
						// Never infer a transport the dialect could not write back.
						let is_sse = dialect.vocab.writes_sse()
							&& dialect.untyped_remote
								== UntypedRemote::InferSseFromUrl
							&& url_has_sse_path(&url);
						if is_sse {
							McpTransport::Sse {
								url,
								headers,
								timeout: None,
							}
						} else {
							McpTransport::StreamableHttp {
								url,
								headers,
								timeout: None,
							}
						}
					} else {
						return Err(ConfigError::InvalidConfig(format!(
							"MCP server '{name}' has neither command nor url"
						)));
					}
				}
			}
		};
		// The dialect's OWN field decides. Reading the other one first lets a
		// stale/foreign key win: a Cline entry with native `disabled: true` and
		// a leftover `enabled: true` would parse as enabled, and the next save
		// writes `disabled: false` — silently turning the server back on. A
		// toggle the dialect cannot write back is not reported at all, since
		// aghub would be showing a state the user can never change.
		let enabled = match dialect.toggle_key {
			ToggleKey::None => true,
			ToggleKey::Enabled => {
				enabled.unwrap_or_else(|| !disabled.unwrap_or(false))
			}
			ToggleKey::Disabled => disabled
				.map(|disabled| !disabled)
				.unwrap_or_else(|| enabled.unwrap_or(true)),
		};
		config.mcps.push(McpServer {
			name,
			enabled,
			transport,
			timeout: None,
			config_source: None,
		});
	}

	Ok(config)
}

fn flatten_nested_transport(value: &mut serde_json::Value) {
	let Some(server) = value.as_object_mut() else {
		return;
	};
	let Some(transport) = server.get("transport").and_then(|v| v.as_object())
	else {
		return;
	};
	let transport = transport.clone();
	for key in ["type", "command", "args", "env", "url", "headers"] {
		if !server.contains_key(key) {
			if let Some(value) = transport.get(key) {
				server.insert(key.to_string(), value.clone());
			}
		}
	}
}

fn url_has_sse_path(url: &str) -> bool {
	let path = url.split(['?', '#']).next().unwrap_or(url);
	path.split('/').any(|seg| seg.eq_ignore_ascii_case("sse"))
}

pub fn serialize(
	config: &AgentConfig,
	original_content: Option<&str>,
	dialect: &Dialect,
) -> Result<String> {
	let mut root: serde_json::Value = if let Some(content) = original_content {
		parse_jsonc_opt(content)
			.map_err(|error| ConfigError::InvalidConfig(error.to_string()))?
			.unwrap_or_else(
				|| serde_json::Value::Object(serde_json::Map::new()),
			)
	} else {
		serde_json::Value::Object(serde_json::Map::new())
	};
	if !root.is_object() {
		return Err(ConfigError::InvalidConfig(
			"MCP config root is not an object".into(),
		));
	}
	let original_servers = match get_nested(&root, dialect.server_key) {
		None => serde_json::Map::new(),
		Some(value) => value.as_object().cloned().ok_or_else(|| {
			ConfigError::InvalidConfig(format!(
				"MCP server key '{key}' is not an object",
				key = dialect.server_key
			))
		})?,
	};
	if let Some((name, _value)) = original_servers
		.iter()
		.find(|(_, value)| !value.is_object())
	{
		return Err(ConfigError::InvalidConfig(format!(
			"MCP server '{name}' is not an object"
		)));
	}

	let mut servers_map = serde_json::Map::new();

	for mcp in &config.mcps {
		// Same sentence these 16 agents already emit, now built where every
		// other dialect builds it.
		dialect.vocab.refuse_unwritable(
			&mcp.transport,
			&format!("MCP server '{name}'", name = mcp.name),
		)?;
		// A dialect without a persisted toggle cannot represent a disabled
		// server, so it is left out rather than written back as enabled.
		if !mcp.enabled && !dialect.writes_toggle() {
			continue;
		}
		let mut entry = match original_servers.get(&mcp.name) {
			None => serde_json::Map::new(),
			Some(value) => value.as_object().cloned().ok_or_else(|| {
				ConfigError::InvalidConfig(format!(
					"MCP server '{name}' is not an object",
					name = mcp.name
				))
			})?,
		};
		for key in [
			"type",
			"transport",
			"command",
			"args",
			"env",
			"environment",
			"headers",
		] {
			entry.remove(key);
		}
		// Only the URL spellings this dialect OWNS. Removing a foreign one
		// deletes data aghub was never asked to manage.
		for key in std::iter::once(dialect.url_key)
			.chain(dialect.legacy_url_keys.iter().copied())
			.chain(dialect.http_url_key)
		{
			entry.remove(key);
		}
		match dialect.toggle_key {
			ToggleKey::None => {}
			ToggleKey::Enabled => {
				entry.remove("disabled");
				entry.insert("enabled".into(), mcp.enabled.into());
			}
			ToggleKey::Disabled => {
				entry.remove("enabled");
				entry.insert("disabled".into(), (!mcp.enabled).into());
			}
		}
		match &mcp.transport {
			McpTransport::Stdio {
				command, args, env, ..
			} => {
				insert_tag(&mut entry, &dialect.vocab, dialect.vocab.stdio);
				entry.insert("command".into(), command.clone().into());
				if !args.is_empty() {
					entry.insert("args".into(), serde_json::json!(args));
				}
				if let Some(env) = env.as_ref().filter(|env| !env.is_empty()) {
					entry
						.insert(dialect.env_key.into(), serde_json::json!(env));
				}
			}
			McpTransport::Sse { url, headers, .. } => {
				insert_tag(&mut entry, &dialect.vocab, dialect.vocab.sse);
				entry.insert(dialect.url_key.into(), url.clone().into());
				if let Some(headers) =
					headers.as_ref().filter(|headers| !headers.is_empty())
				{
					entry.insert("headers".into(), serde_json::json!(headers));
				}
			}
			McpTransport::StreamableHttp { url, headers, .. } => {
				insert_tag(&mut entry, &dialect.vocab, dialect.vocab.http);
				entry.insert(dialect.url_key.into(), url.clone().into());
				if let Some(headers) =
					headers.as_ref().filter(|headers| !headers.is_empty())
				{
					entry.insert("headers".into(), serde_json::json!(headers));
				}
			}
		}
		servers_map.insert(mcp.name.clone(), serde_json::Value::Object(entry));
	}

	set_nested(
		&mut root,
		dialect.server_key,
		serde_json::Value::Object(servers_map),
	)?;

	patch_jsonc_object(original_content, &root)
		.map_err(|error| ConfigError::InvalidConfig(error.to_string()))
}

/// Write the dialect's word for this transport, if it has one. An empty
/// spelling writes NOTHING — the same derivation `mcp_policy::transport_fields`
/// uses for the hand-written dialects, so a dialect that later gains a word
/// starts emitting it without a second edit here.
fn insert_tag(
	entry: &mut serde_json::Map<String, serde_json::Value>,
	vocab: &TransportVocabulary,
	spelling: &'static str,
) {
	if !spelling.is_empty() {
		entry.insert(vocab.tag_key.into(), spelling.into());
	}
}

/// Every `Dialect` below is a FIXTURE for this module's behaviour, and none of
/// them is any agent's coverage. They used to carry agent names — `KIMI`,
/// `WINDSURF`, `AMP` — and reading them as coverage is how `gemini.rs` lost
/// `http_url_key` outright while the "gemini" assertions here stayed green.
/// What a shipped descriptor actually puts on disk is asserted against the
/// descriptor's OWN fn pointers, in `aghub-core/tests/mcp_dialect_golden.rs`.
/// Add an agent-shaped case there, never here.
#[cfg(test)]
mod tests {
	use super::*;
	use crate::models::{McpServer, McpTransport, Skill};

	const DISABLED: Dialect = Dialect {
		toggle_key: ToggleKey::Disabled,
		..MCP_SERVERS
	};
	/// No transport tag at all — what `Option<Discriminator>::None` used to say.
	const NO_SSE: Dialect = Dialect {
		vocab: TransportVocabulary {
			tag_key: "",
			stdio: "",
			sse: "",
			http: "",
			http_read_aliases: &[],
		},
		untyped_remote: UntypedRemote::StreamableHttp,
		..MCP_SERVERS
	};

	#[test]
	fn test_parse_stdio() {
		let json = r#"{
            "mcpServers": {
                "filesystem": {
                    "type": "stdio",
                    "command": "npx",
                    "args": ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]
                },
                "github": {
                    "type": "stdio",
                    "command": "npx",
                    "args": ["-y", "@modelcontextprotocol/server-github"],
                    "env": {"GITHUB_TOKEN": "secret"}
                }
            }
        }"#;
		let config = parse(json, &MCP_SERVERS).unwrap();
		assert_eq!(config.mcps.len(), 2);
		let fs = config.mcps.iter().find(|m| m.name == "filesystem").unwrap();
		assert!(matches!(fs.transport, McpTransport::Stdio { .. }));
		let gh = config.mcps.iter().find(|m| m.name == "github").unwrap();
		assert!(matches!(gh.transport, McpTransport::Stdio { .. }));
	}

	#[test]
	fn test_parse_sse() {
		let json = r#"{"mcpServers": {"remote-server": {"type": "sse", "url": "http://localhost:3000/sse", "headers": {"Authorization": "Bearer token"}}}}"#;
		let config = parse(json, &MCP_SERVERS).unwrap();
		assert_eq!(config.mcps.len(), 1);
		assert!(matches!(config.mcps[0].transport, McpTransport::Sse { .. }));
	}

	#[test]
	fn test_parse_streamable_http() {
		let json = r#"{"mcpServers": {"http-server": {"type": "http", "url": "http://localhost:3000/mcp"}}}"#;
		let config = parse(json, &MCP_SERVERS).unwrap();
		assert_eq!(config.mcps.len(), 1);
		assert!(matches!(
			config.mcps[0].transport,
			McpTransport::StreamableHttp { .. }
		));
	}

	/// The three spellings all 16 `json_map` agents read as streamable HTTP.
	/// Pinned as LITERALS, never read off the dialect: a test that iterates
	/// `http_read_aliases` passes vacuously the moment the list is emptied.
	const UNIVERSAL_HTTP_TAGS: [&str; 3] =
		["http", "streamable-http", "streamableHttp"];

	/// `http_read_aliases` is what the parser READS, not decoration. The `/sse`
	/// URL is the load-bearing half: drop a spelling from the vocabulary and the
	/// entry no longer matches the HTTP arm, falls through to
	/// `InferSseFromUrl`, and parses as SSE — a transport the next save then
	/// writes back over the user's HTTP server.
	#[test]
	fn every_universal_http_tag_outranks_an_sse_shaped_url() {
		// Cline's shape: a different `http` spelling, the SAME read set — so
		// dropping `"http"` from the aliases shows up here even though
		// `MCP_SERVERS` would still match it as its own spelling.
		const WRITES_STREAMABLE_HTTP: Dialect = Dialect {
			vocab: TransportVocabulary {
				http: "streamableHttp",
				..MCP_SERVERS.vocab
			},
			..MCP_SERVERS
		};
		for dialect in [&MCP_SERVERS, &WRITES_STREAMABLE_HTTP] {
			for tag in UNIVERSAL_HTTP_TAGS {
				let json = format!(
					r#"{{"mcpServers":{{"s":{{"type":"{tag}","url":"https://example.com/sse"}}}}}}"#
				);
				let config = parse(&json, dialect).unwrap();
				assert!(
					matches!(
						config.mcps[0].transport,
						McpTransport::StreamableHttp { .. }
					),
					"tag `{tag}` parsed as {:?}, not streamable HTTP",
					config.mcps[0].transport
				);
			}
		}
	}

	#[test]
	fn test_parse_infers_transport_from_url() {
		let json = r#"{
            "mcpServers": {
                "inferred-http": {"url": "http://localhost:3000/mcp"},
                "inferred-sse": {"url": "http://localhost:3001/sse"},
                "inferred-sse-sub": {"url": "http://localhost:3002/sse/events"},
                "inferred-stream": {"url": "http://localhost:3003/stream/events"}
            }
        }"#;
		let config = parse(json, &MCP_SERVERS).unwrap();
		assert_eq!(config.mcps.len(), 4);
		let http = config
			.mcps
			.iter()
			.find(|m| m.name == "inferred-http")
			.unwrap();
		assert!(matches!(
			http.transport,
			McpTransport::StreamableHttp { .. }
		));
		let sse = config
			.mcps
			.iter()
			.find(|m| m.name == "inferred-sse")
			.unwrap();
		assert!(matches!(sse.transport, McpTransport::Sse { .. }));
		let sse_sub = config
			.mcps
			.iter()
			.find(|m| m.name == "inferred-sse-sub")
			.unwrap();
		assert!(matches!(sse_sub.transport, McpTransport::Sse { .. }));
		let stream = config
			.mcps
			.iter()
			.find(|m| m.name == "inferred-stream")
			.unwrap();
		assert!(matches!(
			stream.transport,
			McpTransport::StreamableHttp { .. }
		));
	}

	#[test]
	fn parse_never_infers_a_transport_the_dialect_cannot_write() {
		let config = parse(
			r#"{"mcpServers": {"events": {"url": "https://example.com/sse"}}}"#,
			&NO_SSE,
		)
		.unwrap();
		assert!(matches!(
			config.mcps[0].transport,
			McpTransport::StreamableHttp { .. }
		));
	}

	#[test]
	fn serialize_refuses_sse_when_the_dialect_has_no_word_for_it() {
		let config = AgentConfig {
			mcps: vec![McpServer::new(
				"events",
				McpTransport::sse("https://example.com/events"),
			)],
			skills: vec![],
			sub_agents: vec![],
		};
		let error = serialize(&config, None, &NO_SSE).unwrap_err();
		assert!(error.to_string().contains("cannot express"), "got: {error}");
		// The same server is fine on a dialect that CAN spell SSE.
		let output = serialize(&config, None, &MCP_SERVERS).unwrap();
		assert!(output.contains("\"sse\""));
	}

	#[test]
	fn parse_ignores_a_toggle_the_dialect_cannot_write_back() {
		let json =
			r#"{"mcpServers": {"s": {"command": "echo", "disabled": true}}}"#;
		// No native toggle: reporting "disabled" would show a state the user
		// could never change.
		assert!(parse(json, &MCP_SERVERS).unwrap().mcps[0].enabled);
		// Native toggle: honoured.
		assert!(!parse(json, &DISABLED).unwrap().mcps[0].enabled);
	}

	#[test]
	fn the_dialects_own_tag_outranks_a_foreign_one() {
		const OWN_TAG_TRANSPORT: Dialect = Dialect {
			vocab: TransportVocabulary {
				tag_key: "transport",
				..MCP_SERVERS.vocab
			},
			untyped_remote: UntypedRemote::StreamableHttp,
			..MCP_SERVERS
		};
		// Native `transport` says http; a stray `type` says sse. Letting the
		// foreign key win would rewrite the native one to `sse` on save.
		let json = r#"{"mcpServers":{"s":{"transport":"http","type":"sse","url":"https://host/mcp"}}}"#;
		let config = parse(json, &OWN_TAG_TRANSPORT).unwrap();
		assert!(matches!(
			config.mcps[0].transport,
			McpTransport::StreamableHttp { .. }
		));
		let output =
			serialize(&config, Some(json), &OWN_TAG_TRANSPORT).unwrap();
		let value: serde_json::Value = serde_json::from_str(&output).unwrap();
		assert_eq!(value["mcpServers"]["s"]["transport"], "http");

		// The `type` dialect reads the same file the other way round.
		assert!(matches!(
			parse(json, &MCP_SERVERS).unwrap().mcps[0].transport,
			McpTransport::Sse { .. }
		));
	}

	#[test]
	fn an_unowned_url_key_is_neither_read_nor_removed_nor_type_checked() {
		const OWN_URL_KEY: Dialect = Dialect {
			url_key: "serverUrl",
			legacy_url_keys: &["url"],
			..MCP_SERVERS
		};
		// The foreign key holds an OBJECT: capturing it as a typed string would
		// fail an otherwise valid entry, so this input is what proves the raw
		// capture is doing its job.
		let foreign = r#"{"mcpServers":{"s":{"type":"sse","serverUrl":"https://events","httpUrl":{"plugin":"metadata"}}}}"#;
		let config = parse(foreign, &OWN_URL_KEY).unwrap();
		match &config.mcps[0].transport {
			McpTransport::Sse { url, .. } => assert_eq!(url, "https://events"),
			other => panic!("expected sse, got {other:?}"),
		}
		let output = serialize(&config, Some(foreign), &OWN_URL_KEY).unwrap();
		let value: serde_json::Value = serde_json::from_str(&output).unwrap();
		assert_eq!(value["mcpServers"]["s"]["serverUrl"], "https://events");
		assert_eq!(
			value["mcpServers"]["s"]["httpUrl"]["plugin"], "metadata",
			"an unowned key is unmanaged data, not ours to delete"
		);
	}

	#[test]
	fn a_legacy_url_key_is_read_then_normalised_to_the_written_one() {
		const OWN_URL_KEY: Dialect = Dialect {
			url_key: "serverUrl",
			legacy_url_keys: &["url"],
			..MCP_SERVERS
		};
		// v2.13.3 wrote `url` for this agent; those entries must still load.
		let legacy =
			r#"{"mcpServers":{"s":{"type":"http","url":"https://api/mcp"}}}"#;
		let config = parse(legacy, &OWN_URL_KEY).unwrap();
		let output = serialize(&config, Some(legacy), &OWN_URL_KEY).unwrap();
		let value: serde_json::Value = serde_json::from_str(&output).unwrap();
		assert_eq!(value["mcpServers"]["s"]["serverUrl"], "https://api/mcp");
		assert!(
			value["mcpServers"]["s"].get("url").is_none(),
			"the legacy spelling must not survive alongside the canonical one"
		);
	}

	#[test]
	fn parse_coerces_scalar_env_and_args_instead_of_failing_the_file() {
		let config = parse(
			r#"{"mcpServers": {
				"good": {"command": "echo"},
				"typo": {"command": "srv", "args": ["-p", 3000], "env": {"PORT": 3000, "DEBUG": true}}
			}}"#,
			&MCP_SERVERS,
		)
		.unwrap();
		assert_eq!(config.mcps.len(), 2);
		let typo = config.mcps.iter().find(|m| m.name == "typo").unwrap();
		match &typo.transport {
			McpTransport::Stdio { args, env, .. } => {
				assert_eq!(args, &["-p".to_string(), "3000".to_string()]);
				let env = env.as_ref().unwrap();
				assert_eq!(env.get("PORT").unwrap(), "3000");
				assert_eq!(env.get("DEBUG").unwrap(), "true");
			}
			other => panic!("expected stdio, got {other:?}"),
		}
	}

	#[test]
	fn parse_still_rejects_a_structurally_ambiguous_server() {
		// Dropping these silently would delete them on the next save.
		for json in [
			r#"{"mcpServers": {"a": {}}}"#,
			r#"{"mcpServers": {"a": {"command": "c", "url": "https://x/mcp"}}}"#,
			r#"{"mcpServers": {"a": {"type": "websocket", "url": "wss://x"}}}"#,
			r#"{"mcpServers": {"a": {"command": "c", "args": [["nested"]]}}}"#,
		] {
			assert!(parse(json, &MCP_SERVERS).is_err(), "accepted: {json}");
		}
	}

	#[test]
	fn test_serialize_stdio() {
		let config = crate::models::AgentConfig {
			mcps: vec![McpServer::new(
				"test",
				McpTransport::stdio("echo", vec!["hello".to_string()]),
			)],
			skills: vec![Skill {
				name: "my-skill".to_string(),
				enabled: true,
				description: Some("A test skill".to_string()),
				author: Some("test".to_string()),
				version: Some("1.0.0".to_string()),
				content: None,
				tools: vec!["tool1".to_string()],
				source_path: None,
				canonical_path: None,
				config_source: None,
			}],
			sub_agents: vec![],
		};
		let json = serialize(&config, None, &MCP_SERVERS).unwrap();
		assert!(json.contains("mcpServers"));
		assert!(json.contains("test"));
		assert!(json.contains("\"type\": \"stdio\""));
		assert!(!json.contains("my-skill"));
	}

	#[test]
	fn test_disabled_resources_not_serialized() {
		let config = crate::models::AgentConfig {
			mcps: vec![
				McpServer {
					name: "kept".to_string(),
					enabled: true,
					transport: McpTransport::stdio("echo", vec![]),
					timeout: None,
					config_source: None,
				},
				McpServer {
					name: "dropped".to_string(),
					enabled: false,
					transport: McpTransport::stdio("echo", vec![]),
					timeout: None,
					config_source: None,
				},
			],
			skills: vec![],
			sub_agents: vec![],
		};
		let json = serialize(&config, None, &MCP_SERVERS).unwrap();
		assert!(json.contains("kept"));
		assert!(!json.contains("dropped"));
	}

	#[test]
	fn test_custom_server_key() {
		const SERVERS: Dialect = Dialect {
			server_key: "servers",
			..MCP_SERVERS
		};
		let json = r#"{"servers": {"my-mcp": {"type": "stdio", "command": "npx", "args": ["-y", "some-mcp"]}}}"#;
		let config = parse(json, &SERVERS).unwrap();
		assert_eq!(config.mcps.len(), 1);
		let out = serialize(&config, Some(json), &SERVERS).unwrap();
		let val: serde_json::Value = serde_json::from_str(&out).unwrap();
		assert!(val.get("servers").is_some());
		assert!(val.get("mcpServers").is_none());
	}

	#[test]
	fn test_serialize_preserves_non_mcp_fields() {
		let original = r#"{
			"$schema": "https://example.com/settings.schema.json",
			"theme": "night",
			"features": {
				"autocomplete": true
			},
			"mcpServers": {
				"old": {
					"type": "stdio",
					"command": "old-cmd"
				}
			}
		}"#;
		let mut config = parse(original, &MCP_SERVERS).unwrap();
		config.mcps = vec![McpServer::new(
			"new",
			McpTransport::stdio("new-cmd", vec!["--flag".to_string()]),
		)];

		let out = serialize(&config, Some(original), &MCP_SERVERS).unwrap();
		let val: serde_json::Value = serde_json::from_str(&out).unwrap();

		assert_eq!(val["$schema"], "https://example.com/settings.schema.json");
		assert_eq!(val["theme"], "night");
		assert_eq!(val["features"]["autocomplete"], true);
		assert!(val["mcpServers"].get("new").is_some());
		assert!(val["mcpServers"].get("old").is_none());
	}

	#[test]
	fn test_serialize_preserves_nested_non_mcp_fields() {
		const NESTED_SERVER_KEY: Dialect = Dialect {
			server_key: "amp.mcpServers",
			..MCP_SERVERS
		};
		let original = r#"{
			"amp": {
				"mode": "strict",
				"telemetry": {
					"enabled": false
				},
				"mcpServers": {
					"old": {
						"type": "stdio",
						"command": "old-cmd"
					}
				}
			},
			"otherSetting": 42
		}"#;
		let mut config = parse(original, &NESTED_SERVER_KEY).unwrap();
		config.mcps = vec![McpServer::new(
			"new",
			McpTransport::stdio("new-cmd", vec![]),
		)];

		let out =
			serialize(&config, Some(original), &NESTED_SERVER_KEY).unwrap();
		let val: serde_json::Value = serde_json::from_str(&out).unwrap();

		assert_eq!(val["amp"]["mode"], "strict");
		assert_eq!(val["amp"]["telemetry"]["enabled"], false);
		assert_eq!(val["otherSetting"], 42);
		assert!(val["amp"]["mcpServers"].get("new").is_some());
		assert!(val["amp"]["mcpServers"].get("old").is_none());
	}

	#[test]
	fn test_serialize_preserves_unmanaged_server_fields() {
		let original = r#"{
			"mcpServers": {
				"remote": {
					"type": "http",
					"url": "https://old.example/mcp",
					"oauth": {"clientId": "native-client"},
					"disabledTools": ["dangerous"],
					"timeout": 30000
				}
			}
		}"#;
		let mut config = parse(original, &MCP_SERVERS).unwrap();
		if let McpTransport::StreamableHttp { url, .. } =
			&mut config.mcps[0].transport
		{
			*url = "https://new.example/mcp".into();
		}

		let output = serialize(&config, Some(original), &MCP_SERVERS).unwrap();
		let value: serde_json::Value = serde_json::from_str(&output).unwrap();
		let remote = &value["mcpServers"]["remote"];
		assert_eq!(remote["url"], "https://new.example/mcp");
		assert_eq!(remote["oauth"]["clientId"], "native-client");
		assert_eq!(remote["disabledTools"][0], "dangerous");
		assert_eq!(remote["timeout"], 30000);
	}

	#[test]
	fn test_jsonc_roundtrip_preserves_comments() {
		let original = r#"{
			// Keep the user's explanation.
			"mcpServers": {
				"local": {
					"command": "old-command",
				},
			},
		}"#;
		let mut config = parse(original, &MCP_SERVERS).unwrap();
		config.mcps[0].transport = McpTransport::stdio("new-command", vec![]);

		let output = serialize(&config, Some(original), &MCP_SERVERS).unwrap();
		assert!(output.contains("// Keep the user's explanation."));
		assert!(output.contains("new-command"));
	}

	#[test]
	fn test_untyped_remote_can_default_to_streamable_http() {
		const HTTP_DEFAULT: Dialect = Dialect {
			untyped_remote: UntypedRemote::StreamableHttp,
			..MCP_SERVERS
		};
		let config = parse(
			r#"{"mcpServers":{"remote":{"url":"https://example.com/sse"}}}"#,
			&HTTP_DEFAULT,
		)
		.unwrap();

		assert!(matches!(
			config.mcps[0].transport,
			McpTransport::StreamableHttp { .. }
		));
	}

	#[test]
	fn test_parse_nested_transport_and_disabled() {
		let config = parse(
			r#"{
				"mcpServers": {
					"cline": {
						"disabled": true,
						"transport": {
							"type": "streamableHttp",
							"url": "https://example.com/mcp",
							"headers": {"Authorization": "Bearer token"}
						}
					}
				}
			}"#,
			&DISABLED,
		)
		.unwrap();

		assert!(!config.mcps[0].enabled);
		assert!(matches!(
			config.mcps[0].transport,
			McpTransport::StreamableHttp { .. }
		));
	}

	#[test]
	fn test_native_disabled_option_keeps_disabled_server() {
		const KEBAB_HTTP: Dialect = Dialect {
			vocab: TransportVocabulary {
				http: "streamable-http",
				..MCP_SERVERS.vocab
			},
			toggle_key: ToggleKey::Disabled,
			..MCP_SERVERS
		};
		let mut server = McpServer::new(
			"off",
			McpTransport::streamable_http("https://example.com/mcp"),
		);
		server.enabled = false;
		let config = AgentConfig {
			mcps: vec![server],
			skills: vec![],
			sub_agents: vec![],
		};
		let output = serialize(&config, None, &KEBAB_HTTP).unwrap();
		let value: serde_json::Value = serde_json::from_str(&output).unwrap();

		assert_eq!(value["mcpServers"]["off"]["disabled"], true);
		assert_eq!(value["mcpServers"]["off"]["type"], "streamable-http");
	}

	#[test]
	fn serialize_rejects_malformed_existing_server_data() {
		let config = AgentConfig {
			mcps: vec![McpServer::new(
				"broken",
				McpTransport::stdio("echo", vec![]),
			)],
			skills: vec![],
			sub_agents: vec![],
		};
		let error = serialize(
			&config,
			Some("{\"mcpServers\": {\"broken\": \"not-an-object\"}}"),
			&MCP_SERVERS,
		)
		.unwrap_err();
		assert!(error.to_string().contains("not an object"));
	}

	#[test]
	fn serialize_rejects_malformed_server_container() {
		let config = AgentConfig {
			mcps: vec![],
			skills: vec![],
			sub_agents: vec![],
		};
		let error =
			serialize(&config, Some("{\"mcpServers\": []}"), &MCP_SERVERS)
				.unwrap_err();
		assert!(error.to_string().contains("is not an object"));
	}
}
