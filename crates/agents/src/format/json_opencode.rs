use crate::format::mcp_policy::{
	missing_transport_error, reject_mixed_transport, RemoteVocabulary,
};
use crate::{
	errors::{ConfigError, Result},
	models::{AgentConfig, McpServer, McpTransport},
};
use aghub_json::{parse_jsonc_opt, patch_jsonc_object};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// OpenCode tags its transports `type`, and `remote` is its ONLY remote word —
/// an empty `sse` is what makes `refuse_unwritable` refuse an SSE server, so
/// this module never restates the condition. Writing one anyway would read back
/// as streamable HTTP and change the user's transport behind their back.
///
/// `tag_key` must stay equal to the `#[serde(rename)]` on `server_type` below;
/// serde cannot read a const, so `vocab_tag_key_matches_the_serialized_key`
/// pins the two together.
const VOCAB: RemoteVocabulary = RemoteVocabulary {
	tag_key: "type",
	sse: "",
	http: "remote",
	http_read_aliases: &[],
};

/// The stdio tag. NOT remote vocabulary, so it is a literal here rather than a
/// `RemoteVocabulary` field.
const LOCAL: &str = "local";

#[derive(Debug, Default, Deserialize)]
struct OpenCodeConfig {
	#[serde(rename = "$schema", default)]
	schema: Option<String>,
	#[serde(default)]
	mcp: HashMap<String, OpenCodeMcpEntry>,
	#[serde(flatten)]
	extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct OpenCodeMcpEntry {
	#[serde(rename = "type")]
	server_type: Option<String>,
	command: Option<Vec<String>>,
	url: Option<String>,
	#[serde(default = "crate::models::default_true")]
	enabled: bool,
	#[serde(alias = "env", default)]
	environment: Option<HashMap<String, String>>,
	headers: Option<HashMap<String, String>>,
	timeout: Option<u64>,
	#[serde(flatten)]
	extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Serialize)]
struct OpenCodeMcpOutput {
	#[serde(rename = "type")]
	server_type: String,
	#[serde(skip_serializing_if = "Option::is_none")]
	command: Option<Vec<String>>,
	#[serde(skip_serializing_if = "Option::is_none")]
	url: Option<String>,
	enabled: bool,
	#[serde(skip_serializing_if = "Option::is_none")]
	environment: Option<HashMap<String, String>>,
	#[serde(skip_serializing_if = "Option::is_none")]
	headers: Option<HashMap<String, String>>,
	#[serde(skip_serializing_if = "Option::is_none")]
	timeout: Option<u64>,
	#[serde(flatten)]
	extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Default, Serialize)]
struct OpenCodeConfigOutput {
	#[serde(rename = "$schema", skip_serializing_if = "Option::is_none")]
	schema: Option<String>,
	mcp: HashMap<String, OpenCodeMcpOutput>,
	#[serde(flatten)]
	extra: serde_json::Map<String, serde_json::Value>,
}

pub fn parse(content: &str) -> Result<AgentConfig> {
	let oc: OpenCodeConfig = parse_jsonc_opt(content)
		.map_err(|error| ConfigError::InvalidConfig(error.to_string()))?
		.unwrap_or_default();
	let mut config = AgentConfig::new();

	for (name, entry) in oc.mcp {
		// A mixed entry has to FAIL: serialization rewrites the server from the
		// parsed half, so "just ignore the other one" deletes it.
		reject_mixed_transport(
			&["command"],
			&["url"],
			|key| match key {
				"command" => entry.command.is_some(),
				"url" => entry.url.is_some(),
				_ => false,
			},
			&name,
			"OpenCode",
		)?;
		if entry.command.is_none() && entry.url.is_none() {
			return Err(missing_transport_error(&name, "OpenCode"));
		}
		// An entry aghub cannot model is an entry the next save DELETES (it
		// rewrites every server from the parsed half), so an unrecognised tag
		// is refused rather than falling through to the stdio branch — that
		// fall-through read `{"type":"sse","url":…}` as a command-less stdio
		// server and wrote `command: [""]` over the user's URL.
		let is_remote = match entry.server_type.as_deref() {
			Some(tag)
				if tag == VOCAB.http
					|| VOCAB.http_read_aliases.contains(&tag) =>
			{
				true
			}
			Some(LOCAL) => false,
			None => entry.url.is_some(),
			Some(other) => {
				return Err(ConfigError::InvalidConfig(format!(
					"OpenCode MCP server `{name}` has unknown `{key}` `{other}`",
					key = VOCAB.tag_key
				)))
			}
		};
		let transport = if is_remote {
			let Some(url) = entry.url else {
				return Err(ConfigError::InvalidConfig(format!(
					"OpenCode MCP server `{name}` is tagged `{tag}` but has no \
					 `url`",
					tag = VOCAB.http
				)));
			};
			McpTransport::StreamableHttp {
				url,
				headers: entry.headers,
				timeout: entry.timeout,
			}
		} else {
			let cmd = entry.command.unwrap_or_default();
			let Some((command, args)) = cmd.split_first() else {
				return Err(ConfigError::InvalidConfig(format!(
					"OpenCode MCP server `{name}` has no `command`"
				)));
			};
			McpTransport::Stdio {
				command: command.clone(),
				args: args.to_vec(),
				env: entry.environment,
				timeout: entry.timeout,
			}
		};
		config.mcps.push(McpServer {
			name,
			enabled: entry.enabled,
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
	let original: OpenCodeConfig = match original_content {
		Some(content) => parse_jsonc_opt(content)
			.map_err(|error| ConfigError::InvalidConfig(error.to_string()))?
			.unwrap_or_default(),
		None => OpenCodeConfig::default(),
	};
	let OpenCodeConfig {
		schema,
		mcp: original_mcps,
		extra,
	} = original;

	let mut out = OpenCodeConfigOutput {
		schema,
		mcp: HashMap::new(),
		extra,
	};

	for mcp in &config.mcps {
		// `VOCAB.sse` is empty, so this refuses every SSE server — the module
		// states WHAT IT CAN SPELL, not where to return an error.
		VOCAB.refuse_unwritable(
			&mcp.transport,
			&format!("MCP server '{}'", mcp.name),
		)?;
		let extra = original_mcps
			.get(&mcp.name)
			.map(|entry| entry.extra.clone())
			.unwrap_or_default();
		let entry = match &mcp.transport {
			McpTransport::Stdio {
				command,
				args,
				env,
				timeout,
				..
			} => {
				let mut cmd = vec![command.clone()];
				cmd.extend(args.iter().cloned());
				OpenCodeMcpOutput {
					server_type: LOCAL.to_string(),
					command: Some(cmd),
					url: None,
					enabled: mcp.enabled,
					environment: env.clone(),
					headers: None,
					timeout: *timeout,
					extra,
				}
			}
			// Sse is unreachable while `VOCAB.sse` is empty (the guard above
			// returned). It shares the arm rather than sitting in a panicking
			// one so that giving this dialect an SSE spelling is a deliberate
			// edit here, caught by `mcp_dialect_roundtrip`'s NO_NATIVE_SSE.
			McpTransport::Sse {
				url,
				headers,
				timeout,
			}
			| McpTransport::StreamableHttp {
				url,
				headers,
				timeout,
			} => OpenCodeMcpOutput {
				server_type: VOCAB.http.to_string(),
				command: None,
				url: Some(url.clone()),
				enabled: mcp.enabled,
				environment: None,
				headers: headers.clone(),
				timeout: *timeout,
				extra,
			},
		};
		out.mcp.insert(mcp.name.clone(), entry);
	}

	patch_jsonc_object(original_content, &out)
		.map_err(|error| ConfigError::InvalidConfig(error.to_string()))
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn test_opencode_native_roundtrip() {
		let original = r#"{
            "$schema": "https://opencode.ai/config.json",
            "mcp": {
                "local-srv": {"type": "local", "command": ["npx", "-y", "some-mcp"], "environment": {"TOKEN": "abc"}, "enabled": true},
                "remote-srv": {"type": "remote", "url": "https://api.example.com/mcp", "headers": {"X-Key": "val"}, "enabled": true}
            }
        }"#;
		let config = parse(original).unwrap();
		assert_eq!(config.mcps.len(), 2);
		let out = serialize(&config, Some(original)).unwrap();
		let val: serde_json::Value = serde_json::from_str(&out).unwrap();
		assert_eq!(
			val.get("$schema").and_then(|v| v.as_str()),
			Some("https://opencode.ai/config.json")
		);
		assert!(val.get("mcp").is_some());
		assert!(val.get("mcp_servers").is_none());
	}

	#[test]
	fn test_opencode_preserves_non_mcp_options_on_serialize() {
		let original = r#"{
			"$schema": "https://opencode.ai/config.json",
			"theme": "system",
			"sandbox": "workspace-write",
			"model": {
				"default": "gpt-5.4-mini"
			},
			"mcp": {
				"old-srv": {
					"type": "local",
					"command": ["old-cmd"],
					"enabled": true
				}
			}
		}"#;

		let mut config = parse(original).unwrap();
		config.mcps = vec![McpServer::new(
			"new-srv",
			McpTransport::stdio("npx", vec!["-y".to_string()]),
		)];

		let out = serialize(&config, Some(original)).unwrap();
		let val: serde_json::Value = serde_json::from_str(&out).unwrap();

		assert_eq!(val["$schema"], "https://opencode.ai/config.json");
		assert_eq!(val["theme"], "system");
		assert_eq!(val["sandbox"], "workspace-write");
		assert_eq!(val["model"]["default"], "gpt-5.4-mini");
		assert!(val["mcp"].get("new-srv").is_some());
		assert!(val["mcp"].get("old-srv").is_none());
	}

	#[test]
	fn test_opencode_jsonc_roundtrip_preserves_comments() {
		let original = r#"{
			// Keep this project note.
			"model": "anthropic/claude-sonnet-4-5",
			"mcp": {
				"old-srv": {
					"type": "local",
					"command": ["old-cmd"],
				},
			},
		}"#;

		let mut config = parse(original).unwrap();
		config.mcps = vec![McpServer::new(
			"new-srv",
			McpTransport::stdio("new-cmd", vec![]),
		)];

		let out = serialize(&config, Some(original)).unwrap();
		assert!(out.contains("// Keep this project note."));
		let reparsed = parse(&out).unwrap();
		assert_eq!(reparsed.mcps[0].name, "new-srv");
	}

	#[test]
	fn test_opencode_preserves_unmanaged_server_options() {
		let original = r#"{
			"mcp": {
				"remote-srv": {
					"type": "remote",
					"url": "https://example.com/mcp",
					"oauth": {"clientId": "client-id"}
				}
			}
		}"#;

		let config = parse(original).unwrap();
		let out = serialize(&config, Some(original)).unwrap();
		let val: serde_json::Value = serde_json::from_str(&out).unwrap();
		assert_eq!(val["mcp"]["remote-srv"]["oauth"]["clientId"], "client-id");
	}

	/// `VOCAB.tag_key` cannot be reached from `#[serde(rename)]`, so the two
	/// spellings can only be held together by looking at the bytes serde
	/// actually writes. Without this, editing the const would change nothing
	/// and go green — leaving a declaration that says one thing while the wire
	/// format says another, which is the `single_remote: true` failure again.
	#[test]
	fn vocab_tag_key_matches_the_serialized_key() {
		let config = AgentConfig {
			mcps: vec![McpServer::new("s", McpTransport::stdio("run", vec![]))],
			skills: vec![],
			sub_agents: vec![],
		};
		let out = serialize(&config, None).unwrap();
		let val: serde_json::Value = serde_json::from_str(&out).unwrap();
		assert_eq!(val["mcp"]["s"][VOCAB.tag_key], LOCAL);
		assert_eq!(
			val["mcp"]["s"].as_object().unwrap().len(),
			3,
			"only `{}`, `command` and `enabled` — an extra key means the tag \
			 moved and the assertion above stopped meaning anything: {out}",
			VOCAB.tag_key
		);
	}

	/// A tag this dialect cannot model must be REFUSED, not read as a
	/// command-less stdio server. `{"type":"sse","url":…}` used to parse into
	/// `Stdio { command: "" }` and the next save wrote `command: [""]` over
	/// the user's URL.
	#[test]
	fn an_unmodellable_entry_is_refused_instead_of_half_read() {
		let refusals = [
			// Unknown tag: the url has nowhere to go.
			r#"{"mcp":{"t":{"type":"sse","url":"https://x/mcp"}}}"#,
			r#"{"mcp":{"t":{"type":"grpc","url":"https://x/mcp"}}}"#,
			// Unknown tag WITH a command: nothing else catches this one, and
			// reading it as `local` rewrites the user's declared tag on save.
			r#"{"mcp":{"t":{"type":"grpc","command":["run"]}}}"#,
			// Tagged remote with no url — used to write `url: ""`.
			r#"{"mcp":{"t":{"type":"remote"}}}"#,
			r#"{"mcp":{"t":{"type":"remote","command":["run"]}}}"#,
			// Tagged local with no command — used to write `command: [""]`.
			r#"{"mcp":{"t":{"type":"local","url":"https://x/mcp"}}}"#,
			r#"{"mcp":{"t":{"type":"local","command":[]}}}"#,
			// Neither key at all.
			r#"{"mcp":{"t":{}}}"#,
		];
		for text in refusals {
			assert!(
				parse(text).is_err(),
				"half-read instead of refused: {text}"
			);
		}
		// Still readable: both native tags, and an untagged url.
		for text in [
			r#"{"mcp":{"t":{"type":"remote","url":"https://x/mcp"}}}"#,
			r#"{"mcp":{"t":{"type":"local","command":["run"]}}}"#,
			r#"{"mcp":{"t":{"url":"https://x/mcp"}}}"#,
			r#"{"mcp":{"t":{"command":["run"]}}}"#,
		] {
			assert!(parse(text).is_ok(), "wrongly refused: {text}");
		}
	}

	/// OpenCode probes `command` × `url` and NOTHING else. It strips nothing at
	/// all (serde's `extra` catch-all keeps what it never modelled), so an
	/// inert remote key on a local entry costs nothing to keep reading —
	/// widening the probe would refuse the WHOLE document over it.
	#[test]
	fn a_local_entry_with_an_inert_remote_key_stays_readable() {
		let config = parse(
			r#"{"mcp":{"a":{"type":"local","command":["run"],"headers":{"A":"b"}}}}"#,
		)
		.expect("widening the remote probe would refuse this whole file");
		assert!(matches!(
			config.mcps[0].transport,
			McpTransport::Stdio { .. }
		));
	}
}
