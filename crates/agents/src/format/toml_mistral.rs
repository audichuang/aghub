//! Mistral Vibe (`$VIBE_HOME/config.toml`) MCP serializer.
//!
//! Vibe stores servers as an array of tables under `mcp_servers`. The
//! normalized model has one remote transport, so both Vibe's `http` and
//! `streamable-http` parse as [`McpTransport::StreamableHttp`]. During a
//! rewrite, an existing remote server keeps its original Vibe transport tag.

use crate::errors::{ConfigError, Result};
use crate::format::mcp_policy::{
	reject_mixed_transport, MixedWording, OwnedKeys, TransportVocabulary,
};
use crate::models::{AgentConfig, McpServer, McpTransport};
use std::collections::{HashMap, HashSet};
use toml::map::Map;
use toml::Value;

/// Vibe tags transports with `transport` and has ONE remote word (`http` is a
/// spelling it still reads, and keeps on a rewrite). No word for SSE, which is
/// what makes `refuse_unwritable` refuse — this module never restates that.
const VOCAB: TransportVocabulary = TransportVocabulary {
	tag_key: "transport",
	stdio: "stdio",
	sse: "",
	http: "streamable-http",
	http_read_aliases: &["http"],
};

/// The fields each transport owns. Vibe clears the OPPOSITE family and then
/// patches individual fields, so these are used one family at a time — never
/// as a single blanket strip (`auth` is preserve-and-patched, not removed, on
/// the remote side).
const OWNED: OwnedKeys = OwnedKeys {
	stdio: &["command", "args", "env", "cwd"],
	remote: &[
		"url",
		"auth",
		"headers",
		"api_key_env",
		"api_key_header",
		"api_key_format",
	],
};

type TomlTable = Map<String, Value>;

fn invalid(message: impl Into<String>) -> ConfigError {
	ConfigError::InvalidConfig(message.into())
}

fn parse_toml(content: &str) -> Result<Value> {
	toml::from_str(content).map_err(|error| {
		invalid(format!("invalid Mistral Vibe config TOML: {error}"))
	})
}

fn required_string(
	table: &TomlTable,
	field: &str,
	server: &str,
) -> Result<String> {
	table
		.get(field)
		.and_then(Value::as_str)
		.map(str::to_string)
		.ok_or_else(|| {
			invalid(format!(
				"Mistral Vibe MCP server `{server}` field `{field}` must be a string"
			))
		})
}

fn string_array(
	value: &Value,
	server: &str,
	field: &str,
) -> Result<Vec<String>> {
	let values = value.as_array().ok_or_else(|| {
		invalid(format!(
			"Mistral Vibe MCP server `{server}` field `{field}` must be an array"
		))
	})?;
	values
		.iter()
		.map(|value| {
			value.as_str().map(str::to_string).ok_or_else(|| {
				invalid(format!(
					"Mistral Vibe MCP server `{server}` field `{field}` must contain only strings"
				))
			})
		})
		.collect()
}

fn string_map(
	value: &Value,
	server: &str,
	field: &str,
) -> Result<HashMap<String, String>> {
	let values = value.as_table().ok_or_else(|| {
		invalid(format!(
			"Mistral Vibe MCP server `{server}` field `{field}` must be a table"
		))
	})?;
	values
		.iter()
		.map(|(key, value)| {
			value
				.as_str()
				.map(|value| (key.clone(), value.to_string()))
				.ok_or_else(|| {
					invalid(format!(
						"Mistral Vibe MCP server `{server}` field `{field}`.`{key}` must be a string"
					))
				})
		})
		.collect()
}

fn map_value(values: &HashMap<String, String>) -> Value {
	let mut keys: Vec<&String> = values.keys().collect();
	keys.sort();
	let mut table = Map::new();
	for key in keys {
		table.insert(key.clone(), Value::String(values[key].clone()));
	}
	Value::Table(table)
}

/// Vibe types `tool_timeout_sec` as a float with `gt=0`; the normalized model
/// has whole seconds. An integral float (`30.0`) is read and emitted
/// byte-identical by [`set_timeout`]; a fractional one is REFUSED with a message
/// naming the field, because the alternatives — approximating it, or carrying a
/// value the model cannot represent — both end with a cross-agent copy silently
/// landing a different timeout.
fn model_seconds(value: &Value) -> Option<u64> {
	match value {
		Value::Integer(value) => u64::try_from(*value).ok(),
		// Whole seconds only, and below 2^53 where an f64 still holds every
		// integer. Rounding `0.5` to `1` here looked harmless, but the model is
		// what a cross-agent copy carries: the copy then landed a DIFFERENT
		// timeout, the round-trip check saw two equal whole seconds and called
		// it exact, and reconcile deleted the source holding the real value.
		// A value this model cannot carry has to be refused, not approximated.
		Value::Float(value)
			if value.is_finite()
				&& *value >= 0.0
				&& *value < (1u64 << 53) as f64
				&& value.fract() == 0.0 =>
		{
			Some(*value as u64)
		}
		_ => None,
	}
}

/// A value aghub cannot read is a value a later rewrite would DELETE, so an
/// unreadable timeout is an error rather than a silent `None`.
fn timeout(table: &TomlTable, name: &str) -> Result<Option<u64>> {
	match table.get("tool_timeout_sec") {
		None => Ok(None),
		Some(value) => model_seconds(value).map(Some).ok_or_else(|| {
			invalid(format!(
				"Mistral Vibe MCP server `{name}` field `tool_timeout_sec` must be a non-negative number"
			))
		}),
	}
}

fn parse_stdio(table: &TomlTable, name: &str) -> Result<McpTransport> {
	let command_value = table.get("command").ok_or_else(|| {
		invalid(format!(
			"Mistral Vibe MCP server `{name}` transport `stdio` requires `command`"
		))
	})?;
	let (command, mut args) = match command_value {
		Value::String(command) => (command.clone(), Vec::new()),
		Value::Array(_) => {
			let mut command = string_array(command_value, name, "command")?;
			if command.is_empty() {
				return Err(invalid(format!(
					"Mistral Vibe MCP server `{name}` field `command` cannot be empty"
				)));
			}
			let executable = command.remove(0);
			(executable, command)
		}
		_ => {
			return Err(invalid(format!(
				"Mistral Vibe MCP server `{name}` field `command` must be a string or array"
			)));
		}
	};
	if command.trim().is_empty() {
		return Err(invalid(format!(
			"Mistral Vibe MCP server `{name}` field `command` cannot be empty"
		)));
	}
	if let Some(value) = table.get("args") {
		args.extend(string_array(value, name, "args")?);
	}
	let env = table
		.get("env")
		.map(|value| string_map(value, name, "env"))
		.transpose()?;
	Ok(McpTransport::Stdio {
		command,
		args,
		env,
		timeout: timeout(table, name)?,
	})
}

fn remote_headers(
	table: &TomlTable,
	name: &str,
) -> Result<Option<HashMap<String, String>>> {
	if let Some(auth_value) = table.get("auth") {
		let auth = auth_value.as_table().ok_or_else(|| {
			invalid(format!(
				"Mistral Vibe MCP server `{name}` field `auth` must be a table"
			))
		})?;
		if let Some(headers) = auth.get("headers") {
			return string_map(headers, name, "auth.headers").map(Some);
		}
	}
	table
		.get("headers")
		.map(|value| string_map(value, name, "headers"))
		.transpose()
}

fn parse_remote(table: &TomlTable, name: &str) -> Result<McpTransport> {
	let url = required_string(table, "url", name)?;
	if url.trim().is_empty() {
		return Err(invalid(format!(
			"Mistral Vibe MCP server `{name}` field `url` cannot be empty"
		)));
	}
	Ok(McpTransport::StreamableHttp {
		url,
		headers: remote_headers(table, name)?,
		timeout: timeout(table, name)?,
	})
}

pub fn parse(content: &str) -> Result<AgentConfig> {
	if content.trim().is_empty() {
		return Ok(AgentConfig::new());
	}
	let root = parse_toml(content)?;
	let root = root
		.as_table()
		.ok_or_else(|| invalid("Mistral Vibe config root is not a table"))?;
	let Some(servers_value) = root.get("mcp_servers") else {
		return Ok(AgentConfig::new());
	};
	let servers = servers_value.as_array().ok_or_else(|| {
		invalid("Mistral Vibe `mcp_servers` must be an array of tables")
	})?;

	let mut config = AgentConfig::new();
	let mut names = HashSet::new();
	for (index, server_value) in servers.iter().enumerate() {
		let server = server_value.as_table().ok_or_else(|| {
			invalid(format!(
				"Mistral Vibe `mcp_servers` entry {index} must be a table"
			))
		})?;
		let name = required_string(server, "name", &format!("#{index}"))?;
		if !names.insert(name.clone()) {
			return Err(invalid(format!(
				"duplicate Mistral Vibe MCP server name `{name}`"
			)));
		}
		let disabled = match server.get("disabled") {
			None => false,
			Some(value) => value.as_bool().ok_or_else(|| {
				invalid(format!(
					"Mistral Vibe MCP server `{name}` field `disabled` must be a boolean"
				))
			})?,
		};
		reject_mixed_transport(
			&["command"],
			&["url"],
			|key| server.contains_key(key),
			&name,
			MixedWording::NamesTheProbedKeys("Mistral Vibe"),
		)?;
		let transport_name = required_string(server, VOCAB.tag_key, &name)?;
		let transport = match transport_name.as_str() {
			tag if tag == VOCAB.stdio => parse_stdio(server, &name)?,
			tag if VOCAB.reads_http(tag) => parse_remote(server, &name)?,
			other => {
				return Err(invalid(format!(
					"Mistral Vibe MCP server `{name}` has unsupported transport `{other}`"
				)));
			}
		};
		config.mcps.push(McpServer {
			name,
			enabled: !disabled,
			transport,
			timeout: None,
			config_source: None,
		});
	}
	Ok(config)
}

fn existing_servers(root: &TomlTable) -> Result<HashMap<String, TomlTable>> {
	let Some(value) = root.get("mcp_servers") else {
		return Ok(HashMap::new());
	};
	let servers = value.as_array().ok_or_else(|| {
		invalid("existing Mistral Vibe `mcp_servers` is not an array")
	})?;
	let mut by_name = HashMap::new();
	for (index, value) in servers.iter().enumerate() {
		let table = value.as_table().ok_or_else(|| {
			invalid(format!(
				"existing Mistral Vibe `mcp_servers` entry {index} is not a table"
			))
		})?;
		let name = required_string(table, "name", &format!("#{index}"))?;
		if by_name.insert(name.clone(), table.clone()).is_some() {
			return Err(invalid(format!(
				"duplicate existing Mistral Vibe MCP server name `{name}`"
			)));
		}
	}
	Ok(by_name)
}

fn clear_stdio_fields(table: &mut TomlTable) {
	for field in OWNED.stdio {
		table.remove(*field);
	}
}

fn clear_remote_fields(table: &mut TomlTable) {
	for field in OWNED.remote {
		table.remove(*field);
	}
}

fn set_remote_headers(
	table: &mut TomlTable,
	name: &str,
	headers: &Option<HashMap<String, String>>,
) -> Result<()> {
	if table.contains_key("auth") {
		let auth = table
			.get_mut("auth")
			.and_then(Value::as_table_mut)
			.ok_or_else(|| {
				invalid(format!(
					"existing Mistral Vibe MCP server `{name}` field `auth` is not a table"
				))
			})?;
		if headers.as_ref().is_some_and(|headers| !headers.is_empty())
			&& auth.get("type").and_then(Value::as_str) == Some("oauth")
		{
			return Err(invalid(format!(
				"Mistral Vibe MCP server `{name}` cannot attach static headers to OAuth auth"
			)));
		}
		match headers {
			Some(headers) => {
				auth.insert("headers".to_string(), map_value(headers));
				if !auth.contains_key("type") {
					auth.insert(
						"type".to_string(),
						Value::String("static".into()),
					);
				}
			}
			None => {
				auth.remove("headers");
			}
		}
		return Ok(());
	}

	let uses_legacy_auth =
		["headers", "api_key_env", "api_key_header", "api_key_format"]
			.iter()
			.any(|field| table.contains_key(*field));
	if uses_legacy_auth {
		match headers {
			Some(headers) => {
				table.insert("headers".to_string(), map_value(headers));
			}
			None => {
				table.remove("headers");
			}
		}
		return Ok(());
	}

	if let Some(headers) = headers {
		let mut auth = Map::new();
		auth.insert("type".to_string(), Value::String("static".into()));
		auth.insert("headers".to_string(), map_value(headers));
		table.insert("auth".to_string(), Value::Table(auth));
	}
	Ok(())
}

fn set_timeout(table: &mut TomlTable, timeout: Option<u64>) -> Result<()> {
	// Leave an existing value byte-identical whenever the model still agrees
	// with it. That covers a sub-second value the whole-second model rounds up
	// (rewriting `0.5` as `1` would change the user's timeout) AND one too large
	// to re-emit as a TOML integer. Ceiling: an explicit edit to exactly the
	// value the model already holds is a no-op — the model has no way to say
	// "1, not 0.5".
	if table
		.get("tool_timeout_sec")
		.and_then(model_seconds)
		.is_some_and(|existing| Some(existing) == timeout)
	{
		return Ok(());
	}
	match timeout {
		// Vibe declares the field `gt=0`; writing 0 produces a config the vendor
		// refuses to load. aghub's own input validation rejects 0, but transfer
		// and direct `add_mcp` calls do not go through it.
		Some(0) => {
			return Err(invalid(
				"Mistral Vibe requires a positive `tool_timeout_sec`",
			));
		}
		Some(timeout) => {
			let value =
				i64::try_from(timeout).map(Value::Integer).map_err(|_| {
					invalid(
						"Mistral Vibe MCP timeout exceeds TOML integer range",
					)
				})?;
			table.insert("tool_timeout_sec".to_string(), value);
		}
		None => {
			table.remove("tool_timeout_sec");
		}
	}
	Ok(())
}

pub fn serialize(
	config: &AgentConfig,
	original: Option<&str>,
) -> Result<String> {
	let mut root = match original {
		Some(content) if !content.trim().is_empty() => parse_toml(content)?,
		_ => Value::Table(Map::new()),
	};
	let root_table = root
		.as_table_mut()
		.ok_or_else(|| invalid("Mistral Vibe config root is not a table"))?;
	let mut existing = existing_servers(root_table)?;
	let mut managed_names = HashSet::new();
	let mut servers = Vec::with_capacity(config.mcps.len());

	for mcp in &config.mcps {
		if !managed_names.insert(mcp.name.clone()) {
			return Err(invalid(format!(
				"duplicate Mistral Vibe MCP server name `{}`",
				mcp.name
			)));
		}
		// `VOCAB.sse` is empty, so this refuses every SSE server. Placed here
		// so the duplicate-name error still outranks it, as it did when the
		// refusal lived in the transport match below.
		VOCAB.refuse_unwritable(
			&mcp.transport,
			&format!("Mistral Vibe MCP server `{}`", mcp.name),
		)?;
		let mut table = existing.remove(&mcp.name).unwrap_or_default();
		let old_transport = table
			.get(VOCAB.tag_key)
			.and_then(Value::as_str)
			.map(str::to_string);
		table.insert("name".to_string(), Value::String(mcp.name.clone()));
		table.insert("disabled".to_string(), Value::Boolean(!mcp.enabled));

		match &mcp.transport {
			McpTransport::Stdio {
				command,
				args,
				env,
				timeout,
			} => {
				clear_remote_fields(&mut table);
				table.insert(
					VOCAB.tag_key.to_string(),
					Value::String(VOCAB.stdio.into()),
				);
				table.insert(
					"command".to_string(),
					Value::String(command.clone()),
				);
				if args.is_empty() {
					table.remove("args");
				} else {
					table.insert(
						"args".to_string(),
						Value::Array(
							args.iter().cloned().map(Value::String).collect(),
						),
					);
				}
				match env {
					Some(env) => {
						table.insert("env".to_string(), map_value(env));
					}
					None => {
						table.remove("env");
					}
				}
				set_timeout(&mut table, *timeout)?;
			}
			McpTransport::StreamableHttp {
				url,
				headers,
				timeout,
			} => {
				clear_stdio_fields(&mut table);
				// An existing entry keeps the remote spelling it already had.
				let native_transport = match old_transport.as_deref() {
					Some(tag) if VOCAB.http_read_aliases.contains(&tag) => tag,
					_ => VOCAB.http,
				};
				table.insert(
					VOCAB.tag_key.to_string(),
					Value::String(native_transport.into()),
				);
				table.insert("url".to_string(), Value::String(url.clone()));
				set_remote_headers(&mut table, &mcp.name, headers)?;
				set_timeout(&mut table, *timeout)?;
			}
			// Unreachable: `refuse_unwritable` returned above while `VOCAB.sse`
			// is empty. It stays an explicit error rather than a panic so that
			// giving Vibe an SSE spelling is a deliberate edit here, caught by
			// `mcp_dialect_roundtrip`'s NO_NATIVE_SSE list.
			McpTransport::Sse { .. } => {
				return Err(invalid(format!(
					"Mistral Vibe MCP server `{}` has an unwritable transport",
					mcp.name
				)));
			}
		}
		servers.push(Value::Table(table));
	}

	root_table.insert("mcp_servers".to_string(), Value::Array(servers));
	toml::to_string(&root).map_err(|error| {
		invalid(format!("failed to serialize Mistral Vibe config: {error}"))
	})
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::models::{McpServer, McpTransport};
	use std::collections::HashMap;
	use toml::Value;

	#[test]
	fn parse_native_array_transports_auth_and_disabled() {
		let input = r#"
[[mcp_servers]]
name = "fetch"
transport = "stdio"
command = "uvx"
args = ["mcp-server-fetch"]
disabled = true
[mcp_servers.env]
LOG_LEVEL = "debug"

[[mcp_servers]]
name = "legacy-http"
transport = "http"
url = "https://example.com/http"
[mcp_servers.auth]
type = "static"
headers = { Authorization = "Bearer token" }

[[mcp_servers]]
name = "streamable"
transport = "streamable-http"
url = "https://example.com/mcp"
"#;

		let config = parse(input).unwrap();
		assert_eq!(config.mcps.len(), 3);

		let fetch = config.mcps.iter().find(|m| m.name == "fetch").unwrap();
		assert!(!fetch.enabled);
		match &fetch.transport {
			McpTransport::Stdio {
				command, args, env, ..
			} => {
				assert_eq!(command, "uvx");
				assert_eq!(args, &["mcp-server-fetch"]);
				assert_eq!(
					env.as_ref().unwrap().get("LOG_LEVEL").unwrap(),
					"debug"
				);
			}
			other => panic!("expected stdio, got {other:?}"),
		}

		for name in ["legacy-http", "streamable"] {
			let remote = config.mcps.iter().find(|m| m.name == name).unwrap();
			assert!(remote.enabled);
			assert!(matches!(
				remote.transport,
				McpTransport::StreamableHttp { .. }
			));
		}
		let legacy = config
			.mcps
			.iter()
			.find(|m| m.name == "legacy-http")
			.unwrap();
		match &legacy.transport {
			McpTransport::StreamableHttp { headers, .. } => assert_eq!(
				headers.as_ref().unwrap().get("Authorization").unwrap(),
				"Bearer token"
			),
			other => panic!("expected remote, got {other:?}"),
		}
	}

	#[test]
	fn serialize_native_array_and_disabled_inverse() {
		let mut disabled = McpServer::new(
			"off",
			McpTransport::stdio("uvx", vec!["srv".into()]),
		);
		disabled.enabled = false;
		let config = AgentConfig {
			mcps: vec![
				disabled,
				McpServer::new(
					"remote",
					McpTransport::StreamableHttp {
						url: "https://example.com/mcp".into(),
						headers: None,
						timeout: None,
					},
				),
			],
			skills: vec![],
			sub_agents: vec![],
		};

		let output = serialize(&config, None).unwrap();
		let root: Value = toml::from_str(&output).unwrap();
		let servers = root.get("mcp_servers").unwrap().as_array().unwrap();
		assert_eq!(servers.len(), 2);
		let off = servers
			.iter()
			.find(|v| v.get("name").and_then(Value::as_str) == Some("off"))
			.unwrap();
		assert_eq!(off.get("transport").and_then(Value::as_str), Some("stdio"));
		assert_eq!(off.get("disabled").and_then(Value::as_bool), Some(true));
		let remote = servers
			.iter()
			.find(|v| v.get("name").and_then(Value::as_str) == Some("remote"))
			.unwrap();
		assert_eq!(
			remote.get("transport").and_then(Value::as_str),
			Some("streamable-http")
		);
		assert_eq!(
			remote.get("disabled").and_then(Value::as_bool),
			Some(false)
		);
	}

	#[test]
	fn serialize_preserves_unknown_root_server_and_auth_fields() {
		let original = r#"
theme = "dracula"

[custom]
keep = true

[[mcp_servers]]
name = "remote"
transport = "http"
url = "https://old.example.com"
disabled = false
prompt = "keep me"
custom_server = 42
[mcp_servers.auth]
type = "static"
headers = { Old = "gone" }
api_key_env = "TOKEN"
custom_auth = "keep me too"
"#;
		let mut headers = HashMap::new();
		headers.insert("Authorization".into(), "Bearer fresh".into());
		let config = AgentConfig {
			mcps: vec![McpServer::new(
				"remote",
				McpTransport::StreamableHttp {
					url: "https://new.example.com/mcp".into(),
					headers: Some(headers),
					timeout: None,
				},
			)],
			skills: vec![],
			sub_agents: vec![],
		};

		let output = serialize(&config, Some(original)).unwrap();
		let root: Value = toml::from_str(&output).unwrap();
		assert_eq!(root.get("theme").and_then(Value::as_str), Some("dracula"));
		assert_eq!(
			root.get("custom")
				.and_then(|v| v.get("keep"))
				.and_then(Value::as_bool),
			Some(true)
		);
		let server = &root.get("mcp_servers").unwrap().as_array().unwrap()[0];
		assert_eq!(
			server.get("prompt").and_then(Value::as_str),
			Some("keep me")
		);
		assert_eq!(
			server.get("custom_server").and_then(Value::as_integer),
			Some(42)
		);
		assert_eq!(
			server.get("transport").and_then(Value::as_str),
			Some("http")
		);
		let auth = server.get("auth").unwrap();
		assert_eq!(
			auth.get("api_key_env").and_then(Value::as_str),
			Some("TOKEN")
		);
		assert_eq!(
			auth.get("custom_auth").and_then(Value::as_str),
			Some("keep me too")
		);
		assert_eq!(
			auth.get("headers")
				.and_then(|v| v.get("Authorization"))
				.and_then(Value::as_str),
			Some("Bearer fresh")
		);
	}

	#[test]
	fn an_integral_float_timeout_is_left_byte_identical() {
		// Vibe types the field as a float, so `30.0` is the ordinary shape.
		// Rewriting it as `30` would churn the file for nothing.
		let original = "[[mcp_servers]]\nname = \"server\"\ntransport = \"stdio\"\ncommand = \"old\"\ntool_timeout_sec = 30.0\n";
		let config = parse(original).unwrap();
		match &config.mcps[0].transport {
			McpTransport::Stdio { timeout, .. } => {
				assert_eq!(*timeout, Some(30))
			}
			other => panic!("expected stdio, got {other:?}"),
		}
		let output = serialize(&config, Some(original)).unwrap();
		let root: Value = toml::from_str(&output).unwrap();
		let server = &root["mcp_servers"].as_array().unwrap()[0];
		assert_eq!(
			server.get("tool_timeout_sec").and_then(Value::as_float),
			Some(30.0)
		);
	}

	#[test]
	fn a_fractional_timeout_is_refused_rather_than_approximated() {
		// The model holds whole seconds. Rounding `0.5` to `1` here is what a
		// cross-agent copy would carry, so the copy would land a DIFFERENT
		// timeout, the round-trip check would call it exact, and a reconcile
		// would then delete the source holding the real value.
		for value in ["0.5", "1.9"] {
			let original = format!(
				"[[mcp_servers]]\nname = \"server\"\ntransport = \"stdio\"\ncommand = \"c\"\ntool_timeout_sec = {value}\n"
			);
			let error = parse(&original).unwrap_err().to_string();
			assert!(
				error.contains("tool_timeout_sec"),
				"{value} must be refused by name: {error}"
			);
		}
	}

	#[test]
	fn an_existing_zero_timeout_is_preserved_rather_than_blocking_every_save() {
		// Deliberate: the file is already invalid by Vibe's `gt=0` rule, and
		// erroring here would make every unrelated save fail on it. Moving the
		// `Some(0)` rejection ahead of the unchanged-check would break this.
		let original = "[[mcp_servers]]\nname = \"server\"\ntransport = \"stdio\"\ncommand = \"old\"\ntool_timeout_sec = 0\n";
		let config = parse(original).unwrap();
		let output = serialize(&config, Some(original)).unwrap();
		let root: Value = toml::from_str(&output).unwrap();
		let server = &root["mcp_servers"].as_array().unwrap()[0];
		assert_eq!(
			server.get("tool_timeout_sec").and_then(Value::as_integer),
			Some(0)
		);
	}

	#[test]
	fn a_mixed_transport_entry_is_refused_rather_than_half_deleted() {
		// `transport = "stdio"` with a `url` is ambiguous; parsing it as stdio
		// and rewriting the entry would delete the url and its auth fields.
		let original = "[[mcp_servers]]\nname = \"s\"\ntransport = \"stdio\"\ncommand = \"c\"\nurl = \"https://remote\"\n";
		let error = parse(original).unwrap_err().to_string();
		assert!(error.contains("mixes stdio keys"), "got: {error}");
	}

	#[test]
	fn a_zero_timeout_is_never_written_fresh() {
		// Vibe declares the field `gt=0`, and transfer / add_mcp do not go
		// through aghub's input validator.
		let mut config = AgentConfig::new();
		let mut server = McpServer::new("s", McpTransport::stdio("c", vec![]));
		if let McpTransport::Stdio { timeout, .. } = &mut server.transport {
			*timeout = Some(0);
		}
		config.mcps.push(server);
		let error = serialize(&config, None).unwrap_err().to_string();
		assert!(error.contains("positive"), "got: {error}");
	}

	#[test]
	fn a_programmatic_timeout_beyond_toml_integers_is_refused_on_write() {
		// Reached through transfer / add_mcp, which do not go through the CLI
		// validator. Writing it as a float would silently widen the value.
		let mut config = AgentConfig::new();
		let mut server = McpServer::new("s", McpTransport::stdio("c", vec![]));
		if let McpTransport::Stdio { timeout, .. } = &mut server.transport {
			*timeout = Some(u64::MAX);
		}
		config.mcps.push(server);
		let error = serialize(&config, None).unwrap_err().to_string();
		assert!(error.contains("integer range"), "got: {error}");
	}

	#[test]
	fn a_timeout_beyond_exact_float_range_is_rejected_not_rounded() {
		// Past 2^53 an f64 cannot hold every integer, so aghub could neither
		// compare nor rewrite the value faithfully.
		let original = "[[mcp_servers]]\nname = \"server\"\ntransport = \"stdio\"\ncommand = \"c\"\ntool_timeout_sec = 1e19\n";
		let error = parse(original).unwrap_err().to_string();
		assert!(error.contains("tool_timeout_sec"), "got: {error}");
	}

	#[test]
	fn an_unreadable_timeout_is_rejected_rather_than_deleted() {
		let original = "[[mcp_servers]]\nname = \"server\"\ntransport = \"stdio\"\ncommand = \"c\"\ntool_timeout_sec = \"30\"\n";
		assert!(parse(original).is_err());
	}

	#[test]
	fn serialize_clears_removed_timeout() {
		let original = r#"
[[mcp_servers]]
name = "server"
transport = "stdio"
command = "old"
tool_timeout_sec = 30
"#;
		let config = AgentConfig {
			mcps: vec![McpServer::new(
				"server",
				McpTransport::stdio("new", vec![]),
			)],
			skills: vec![],
			sub_agents: vec![],
		};
		let output = serialize(&config, Some(original)).unwrap();
		let root: Value = toml::from_str(&output).unwrap();
		let server = &root["mcp_servers"].as_array().unwrap()[0];
		assert!(server.get("tool_timeout_sec").is_none());
	}

	/// A transport switch must leave NONE of the other family's keys behind.
	/// `api_key_env` is the one that matters most: it names the environment
	/// variable a credential is read from, so a leftover copy is a credential
	/// source the user already removed still wired into the config.
	///
	/// The key names are LITERALS on purpose. Iterating `OWNED` would make this
	/// agree with whatever the declaration happens to say, so deleting a name
	/// would delete its own check — the false green this test exists to stop.
	#[test]
	fn switching_transport_strips_every_key_the_other_family_owns() {
		const STDIO_OWNED: &[&str] = &["command", "args", "env", "cwd"];
		const REMOTE_OWNED: &[&str] = &[
			"url",
			"auth",
			"headers",
			"api_key_env",
			"api_key_header",
			"api_key_format",
		];

		fn only(transport: McpTransport) -> AgentConfig {
			AgentConfig {
				mcps: vec![McpServer::new("srv", transport)],
				skills: vec![],
				sub_agents: vec![],
			}
		}
		fn saved(original: &str, config: &AgentConfig) -> Value {
			let out = serialize(config, Some(original)).expect("serialize");
			let root: Value = toml::from_str(&out).expect("re-parse");
			root["mcp_servers"].as_array().expect("array")[0].clone()
		}

		// Every stdio-family key present, re-saved as remote.
		let original = concat!(
			"[[mcp_servers]]\n",
			"name = \"srv\"\n",
			"transport = \"stdio\"\n",
			"command = \"old\"\n",
			"args = [\"a\"]\n",
			"cwd = \"/tmp\"\n",
			"env = { K = \"V\" }\n"
		);
		let server = saved(
			original,
			&only(McpTransport::streamable_http("https://x/mcp")),
		);
		for key in STDIO_OWNED {
			assert!(
				server.get(key).is_none(),
				"stdio key `{key}` survived a switch to remote: {server}"
			);
		}

		// Every remote-family key present, re-saved as stdio.
		let original = concat!(
			"[[mcp_servers]]\n",
			"name = \"srv\"\n",
			"transport = \"http\"\n",
			"url = \"https://x/mcp\"\n",
			"headers = { H = \"1\" }\n",
			"api_key_env = \"TOK\"\n",
			"api_key_header = \"X-Key\"\n",
			"api_key_format = \"Bearer {key}\"\n",
			"auth = { type = \"static\" }\n"
		);
		let server = saved(original, &only(McpTransport::stdio("run", vec![])));
		for key in REMOTE_OWNED {
			assert!(
				server.get(key).is_none(),
				"remote key `{key}` survived a switch to stdio: {server}"
			);
		}
	}

	/// Vibe probes `command` × `url` and NOTHING else. It strips only the
	/// OPPOSITE family, so an inert remote key on a stdio entry costs nothing
	/// to keep reading — but widening the probe (adding `api_key_env`, say)
	/// would refuse this file, and parse failure is whole-document, so EVERY
	/// Vibe MCP server would become unmanageable.
	#[test]
	fn a_stdio_entry_with_an_inert_remote_key_stays_readable() {
		let config = parse(
			"[[mcp_servers]]\nname = \"a\"\ntransport = \"stdio\"\n\
command = \"run\"\napi_key_env = \"TOK\"\n",
		)
		.expect("widening the remote probe would refuse this whole file");
		assert!(matches!(
			config.mcps[0].transport,
			McpTransport::Stdio { .. }
		));
	}
}
