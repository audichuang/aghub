use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// The normalized configuration structure that works across all agent types
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentConfig {
	#[serde(default)]
	pub skills: Vec<Skill>,
	#[serde(default)]
	pub mcps: Vec<McpServer>,
	#[serde(default)]
	pub sub_agents: Vec<SubAgent>,
}

impl AgentConfig {
	pub fn new() -> Self {
		Self {
			skills: Vec::new(),
			mcps: Vec::new(),
			sub_agents: Vec::new(),
		}
	}
}

impl Default for AgentConfig {
	fn default() -> Self {
		Self::new()
	}
}

/// A skill with explicit frontmatter fields
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Skill {
	pub name: String,
	#[serde(default = "default_true")]
	pub enabled: bool,
	pub description: Option<String>,
	pub author: Option<String>,
	pub version: Option<String>,
	#[serde(skip)]
	pub content: Option<String>,
	/// List of tool names this skill provides
	#[serde(default)]
	pub tools: Vec<String>,
	/// Source path relative to skills directory with ~ prefix (e.g., "~/.claude/skills/my-skill/SKILL.md")
	#[serde(skip_serializing_if = "Option::is_none", default)]
	pub source_path: Option<String>,
	/// Resolved canonical path when source_path is a symlink.
	/// None if the skill was not discovered via a symlink.
	#[serde(skip_serializing_if = "Option::is_none", default)]
	pub canonical_path: Option<String>,
	/// Which config scope this skill was loaded from (set at load time, not persisted)
	#[serde(skip)]
	pub config_source: Option<ConfigSource>,
}

impl Skill {
	pub fn new(name: impl Into<String>) -> Self {
		Self {
			name: name.into(),
			enabled: true,
			description: None,
			author: None,
			version: None,
			content: None,
			tools: Vec::new(),
			source_path: None,
			canonical_path: None,
			config_source: None,
		}
	}
}

/// MCP server configuration
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McpServer {
	pub name: String,
	#[serde(default = "default_true")]
	pub enabled: bool,
	pub transport: McpTransport,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub timeout: Option<u64>, // Timeout in seconds
	/// Which config scope this MCP was loaded from (set at load time, not persisted)
	#[serde(skip)]
	pub config_source: Option<ConfigSource>,
}

impl McpServer {
	pub fn new(name: impl Into<String>, transport: McpTransport) -> Self {
		Self {
			name: name.into(),
			enabled: true,
			transport,
			timeout: None,
			config_source: None,
		}
	}
}

/// Default remote (URL-based) transport type.
///
/// Shared by the CLI clap default, the API DTO, and desktop so the
/// "streamable-http" literal lives in exactly one place.
pub const DEFAULT_REMOTE_TRANSPORT: &str = "streamable-http";

/// Reject a `Some(0)` timeout. The single owner of the timeout rule, shared by
/// [`McpTransport::from_inputs`] and the API DTO so both surfaces agree.
pub fn reject_zero_timeout(timeout: Option<u64>) -> crate::errors::Result<()> {
	if matches!(timeout, Some(0)) {
		return Err(crate::errors::ConfigError::ValidationFailed(
			"timeout must be greater than 0".to_string(),
		));
	}
	Ok(())
}

/// Transport configuration for MCP servers
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum McpTransport {
	/// stdio-based MCP transport (command execution)
	Stdio {
		command: String,
		#[serde(default)]
		args: Vec<String>,
		/// Environment variables (only for stdio transport)
		#[serde(default)]
		env: Option<HashMap<String, String>>,
		#[serde(skip_serializing_if = "Option::is_none")]
		timeout: Option<u64>,
	},
	/// Legacy SSE-based MCP transport (HTTP server-sent events)
	/// Deprecated in favor of StreamableHttp
	Sse {
		url: String,
		/// HTTP headers as KV pairs (for SSE-based MCPs)
		#[serde(default)]
		headers: Option<HashMap<String, String>>,
		#[serde(skip_serializing_if = "Option::is_none")]
		timeout: Option<u64>,
	},
	/// Streamable HTTP transport (successor to SSE)
	/// Uses HTTP POST for client->server, streaming responses for server->client
	StreamableHttp {
		url: String,
		/// HTTP headers as KV pairs
		#[serde(default)]
		headers: Option<HashMap<String, String>>,
		#[serde(skip_serializing_if = "Option::is_none")]
		timeout: Option<u64>,
	},
}

impl McpTransport {
	pub fn stdio(command: impl Into<String>, args: Vec<String>) -> Self {
		Self::Stdio {
			command: command.into(),
			args,
			env: None,
			timeout: None,
		}
	}

	pub fn stdio_with_env(
		command: impl Into<String>,
		args: Vec<String>,
		env: HashMap<String, String>,
	) -> Self {
		Self::Stdio {
			command: command.into(),
			args,
			env: Some(env),
			timeout: None,
		}
	}

	pub fn sse(url: impl Into<String>) -> Self {
		Self::Sse {
			url: url.into(),
			headers: None,
			timeout: None,
		}
	}

	pub fn sse_with_headers(
		url: impl Into<String>,
		headers: HashMap<String, String>,
	) -> Self {
		Self::Sse {
			url: url.into(),
			headers: Some(headers),
			timeout: None,
		}
	}

	pub fn streamable_http(url: impl Into<String>) -> Self {
		Self::StreamableHttp {
			url: url.into(),
			headers: None,
			timeout: None,
		}
	}

	pub fn streamable_http_with_headers(
		url: impl Into<String>,
		headers: HashMap<String, String>,
	) -> Self {
		Self::StreamableHttp {
			url: url.into(),
			headers: Some(headers),
			timeout: None,
		}
	}

	/// One validating constructor shared by the CLI and API surfaces.
	///
	/// Rejects incompatible flag combinations instead of silently dropping
	/// them, and validates `timeout`. `transport_type` is only consulted on
	/// the url path. Returns `Ok(None)` when neither `command` nor `url` is
	/// given (the caller decides whether that is an error).
	#[allow(clippy::too_many_arguments)]
	pub fn from_inputs(
		command: Option<String>,
		url: Option<String>,
		transport_type: &str,
		headers: Option<HashMap<String, String>>,
		env: Option<HashMap<String, String>>,
		timeout: Option<u64>,
	) -> crate::errors::Result<Option<McpTransport>> {
		reject_zero_timeout(timeout)?;
		if command.is_some() && url.is_some() {
			return Err(crate::errors::ConfigError::ValidationFailed(
				"--command and --url are mutually exclusive".to_string(),
			));
		}

		if let Some(command) = command {
			if headers.is_some_and(|h| !h.is_empty()) {
				return Err(crate::errors::ConfigError::ValidationFailed(
					"--header is only valid with --url".to_string(),
				));
			}
			let mut parts = command.split_whitespace().map(String::from);
			let Some(program) = parts.next() else {
				return Err(crate::errors::ConfigError::ValidationFailed(
					"command cannot be empty".to_string(),
				));
			};
			let args: Vec<String> = parts.collect();
			return Ok(Some(McpTransport::Stdio {
				command: program,
				args,
				env,
				timeout,
			}));
		}

		if let Some(url) = url {
			if env.is_some_and(|e| !e.is_empty()) {
				return Err(crate::errors::ConfigError::ValidationFailed(
					"--env is only valid with --command".to_string(),
				));
			}
			if url.trim().is_empty() {
				return Err(crate::errors::ConfigError::ValidationFailed(
					"url cannot be empty".to_string(),
				));
			}
			return match transport_type {
				"sse" => Ok(Some(McpTransport::Sse {
					url,
					headers,
					timeout,
				})),
				"streamable-http" => Ok(Some(McpTransport::StreamableHttp {
					url,
					headers,
					timeout,
				})),
				other => {
					Err(crate::errors::ConfigError::ValidationFailed(format!(
						"unknown transport type '{other}' \
						(expected sse or streamable-http)"
					)))
				}
			};
		}

		// Neither command nor url: stray --header / --env would be silently
		// dropped, so reject them instead of returning Ok(None).
		if headers.is_some_and(|h| !h.is_empty()) {
			return Err(crate::errors::ConfigError::ValidationFailed(
				"--header is only valid with --url".to_string(),
			));
		}
		if env.is_some_and(|e| !e.is_empty()) {
			return Err(crate::errors::ConfigError::ValidationFailed(
				"--env is only valid with --command".to_string(),
			));
		}

		Ok(None)
	}

	/// Reject structurally-empty values that would build an unusable MCP: an
	/// empty stdio command or an empty remote URL. The single shared rule for
	/// both surfaces — the CLI reaches it through `from_inputs` (which builds a
	/// transport, so its own empty-command / empty-url guards already fire) and
	/// the API calls it from `CreateMcpRequest`/`UpdateMcpRequest::validate`,
	/// which otherwise build a transport straight from JSON with no value check.
	pub fn validate_values(&self) -> Result<(), crate::errors::ConfigError> {
		use crate::errors::ConfigError;
		match self {
			McpTransport::Stdio { command, .. } => {
				if command.trim().is_empty() {
					return Err(ConfigError::ValidationFailed(
						"command cannot be empty".to_string(),
					));
				}
			}
			McpTransport::Sse { url, .. }
			| McpTransport::StreamableHttp { url, .. } => {
				if url.trim().is_empty() {
					return Err(ConfigError::ValidationFailed(
						"url cannot be empty".to_string(),
					));
				}
			}
		}
		Ok(())
	}
}

pub(crate) fn default_true() -> bool {
	true
}

/// A sub-agent entry with name, description, and system-prompt instruction.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SubAgent {
	pub name: String,
	pub description: Option<String>,
	/// The system-prompt / instruction body (not serialized — lives in the
	/// file body, not the YAML front-matter).
	#[serde(skip)]
	pub instruction: Option<String>,
	/// Absolute path to the source `.md` file (set at load time).
	#[serde(skip_serializing_if = "Option::is_none", default)]
	pub source_path: Option<String>,
	/// Which config scope this sub-agent was loaded from (set at load time).
	#[serde(skip)]
	pub config_source: Option<ConfigSource>,
}

impl SubAgent {
	pub fn new(name: impl Into<String>) -> Self {
		Self {
			name: name.into(),
			description: None,
			instruction: None,
			source_path: None,
			config_source: None,
		}
	}
}

/// Source of a resource (project-level vs global)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ConfigSource {
	Global,
	Project,
}

/// Resource discovery scope
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ResourceScope {
	/// Show only global resources (default behavior)
	#[default]
	GlobalOnly,
	/// Show only project-level resources
	ProjectOnly,
	/// Show both project and global resources
	Both,
}

/// Agent types supported by the system
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentType {
	Cursor,
	Windsurf,
	Copilot,
	Claude,
	RooCode,
	Cline,
	Gemini,
	Codex,
	Antigravity,
	Openclaw,
	OpenCode,
	// New agents
	AugmentCode,
	KiloCode,
	Amp,
	Zed,
	Kiro,
	Warp,
	Trae,
	Factory,
	Kimi,
	Mistral,
	Pi,
	JetBrainsAi,
	Hermes,
	Grok,
}

impl AgentType {
	pub const ALL: &[AgentType] = &[
		AgentType::Cursor,
		AgentType::Windsurf,
		AgentType::Copilot,
		AgentType::Claude,
		AgentType::RooCode,
		AgentType::Cline,
		AgentType::Gemini,
		AgentType::Codex,
		AgentType::Antigravity,
		AgentType::Openclaw,
		AgentType::OpenCode,
		AgentType::AugmentCode,
		AgentType::KiloCode,
		AgentType::Amp,
		AgentType::Zed,
		AgentType::Kiro,
		AgentType::Warp,
		AgentType::Trae,
		AgentType::Factory,
		AgentType::Kimi,
		AgentType::Mistral,
		AgentType::Pi,
		AgentType::JetBrainsAi,
		AgentType::Hermes,
		AgentType::Grok,
	];

	pub fn as_str(&self) -> &'static str {
		match self {
			AgentType::Cursor => "cursor",
			AgentType::Windsurf => "windsurf",
			AgentType::Copilot => "copilot",
			AgentType::Claude => "claude",
			AgentType::RooCode => "roocode",
			AgentType::Cline => "cline",
			AgentType::Gemini => "gemini",
			AgentType::Codex => "codex",
			AgentType::Antigravity => "antigravity",
			AgentType::Openclaw => "openclaw",
			AgentType::OpenCode => "opencode",
			AgentType::AugmentCode => "augmentcode",
			AgentType::KiloCode => "kilocode",
			AgentType::Amp => "amp",
			AgentType::Zed => "zed",
			AgentType::Kiro => "kiro",
			AgentType::Warp => "warp",
			AgentType::Trae => "trae",
			AgentType::Factory => "factory",
			AgentType::Kimi => "kimi",
			AgentType::Mistral => "mistral",
			AgentType::Pi => "pi",
			AgentType::JetBrainsAi => "jetbrains-ai",
			AgentType::Hermes => "hermes",
			AgentType::Grok => "grok",
		}
	}

	pub fn next(&self) -> AgentType {
		let idx = Self::ALL.iter().position(|a| a == self).unwrap_or(0);
		Self::ALL[(idx + 1) % Self::ALL.len()]
	}

	/// Parse a single agent id or a comma-separated list ("claude,grok"),
	/// preserving order and dropping duplicates. Every token must be a known
	/// agent id; the error names the offending token and the valid ids.
	pub fn parse_list(s: &str) -> Result<Vec<AgentType>, String> {
		let mut out = Vec::new();
		for token in s.split(',') {
			let token = token.trim();
			if token.is_empty() {
				return Err(format!("empty agent name in '{s}'"));
			}
			let agent = token.parse::<AgentType>().map_err(|_| {
				format!(
					"unknown agent '{token}' (valid: {})",
					AgentType::ALL
						.iter()
						.map(|a| a.as_str())
						.collect::<Vec<_>>()
						.join(", ")
				)
			})?;
			if !out.contains(&agent) {
				out.push(agent);
			}
		}
		Ok(out)
	}
}

impl std::str::FromStr for AgentType {
	type Err = String;

	fn from_str(s: &str) -> Result<Self, Self::Err> {
		match s.to_lowercase().as_str() {
			"cursor" => Ok(AgentType::Cursor),
			"windsurf" => Ok(AgentType::Windsurf),
			"copilot" => Ok(AgentType::Copilot),
			"claude" => Ok(AgentType::Claude),
			"roocode" | "roo" => Ok(AgentType::RooCode),
			"cline" => Ok(AgentType::Cline),
			"gemini" => Ok(AgentType::Gemini),
			"codex" => Ok(AgentType::Codex),
			"antigravity" => Ok(AgentType::Antigravity),
			"openclaw" => Ok(AgentType::Openclaw),
			"opencode" => Ok(AgentType::OpenCode),
			"augmentcode" | "augment" => Ok(AgentType::AugmentCode),
			"kilocode" | "kilo" => Ok(AgentType::KiloCode),
			"amp" => Ok(AgentType::Amp),
			"zed" => Ok(AgentType::Zed),
			"kiro" => Ok(AgentType::Kiro),
			"warp" => Ok(AgentType::Warp),
			"trae" => Ok(AgentType::Trae),
			"factory" => Ok(AgentType::Factory),
			"kimi" | "kimi-cli" => Ok(AgentType::Kimi),
			"mistral" => Ok(AgentType::Mistral),
			"pi" => Ok(AgentType::Pi),
			"jetbrains-ai" | "jetbrains" | "jb" => Ok(AgentType::JetBrainsAi),
			"hermes" => Ok(AgentType::Hermes),
			"grok" => Ok(AgentType::Grok),
			_ => Err(format!("Unknown agent type: {s}")),
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn test_mcp_server_stdio() {
		let mcp = McpServer::new(
			"filesystem",
			McpTransport::stdio(
				"npx",
				vec![
					"-y".to_string(),
					"@modelcontextprotocol/server-filesystem".to_string(),
					"/tmp".to_string(),
				],
			),
		);

		let json = serde_json::to_string(&mcp).unwrap();
		assert!(json.contains("\"type\":\"stdio\""));
		assert!(json.contains("\"command\":\"npx\""));
	}

	#[test]
	fn test_mcp_server_stdio_with_env() {
		let mut env = HashMap::new();
		env.insert("API_KEY".to_string(), "secret".to_string());

		let mcp = McpServer::new(
			"custom-server",
			McpTransport::stdio_with_env(
				"my-server",
				vec!["--port".to_string()],
				env,
			),
		);

		let json = serde_json::to_string(&mcp).unwrap();
		assert!(json.contains("\"type\":\"stdio\""));
		assert!(json.contains("\"API_KEY\""));
	}

	#[test]
	fn test_mcp_server_sse_with_headers() {
		let mut headers = HashMap::new();
		headers.insert("Authorization".to_string(), "Bearer token".to_string());

		let mcp = McpServer::new(
			"custom-server",
			McpTransport::sse_with_headers("http://localhost:3000", headers),
		);

		let json = serde_json::to_string(&mcp).unwrap();
		assert!(json.contains("\"type\":\"sse\""));
		assert!(json.contains("\"url\":\"http://localhost:3000\""));
		assert!(json.contains("\"Authorization\""));
	}

	#[test]
	fn test_mcp_server_streamable_http_with_headers() {
		let mut headers = HashMap::new();
		headers.insert("Authorization".to_string(), "Bearer token".to_string());
		headers.insert("X-API-Key".to_string(), "secret-key".to_string());

		let mcp = McpServer::new(
			"streamable-server",
			McpTransport::streamable_http_with_headers(
				"http://localhost:3000/mcp",
				headers,
			),
		);

		let json = serde_json::to_string(&mcp).unwrap();
		assert!(json.contains("\"type\":\"streamable_http\""));
		assert!(json.contains("\"url\":\"http://localhost:3000/mcp\""));
		assert!(json.contains("\"Authorization\""));
		assert!(json.contains("\"X-API-Key\""));

		// Test round-trip
		let deserialized: McpServer = serde_json::from_str(&json).unwrap();
		assert_eq!(mcp, deserialized);
	}

	#[test]
	fn test_mcp_server_streamable_http_basic() {
		let mcp = McpServer::new(
			"basic-http",
			McpTransport::streamable_http("http://localhost:8080/mcp"),
		);

		let json = serde_json::to_string(&mcp).unwrap();
		assert!(json.contains("\"type\":\"streamable_http\""));
		assert!(json.contains("\"url\":\"http://localhost:8080/mcp\""));
	}

	#[test]
	fn test_mcp_server_with_timeout() {
		let transport = McpTransport::Stdio {
			command: "npx".to_string(),
			args: vec!["-y".to_string()],
			env: None,
			timeout: Some(30),
		};
		let mcp = McpServer {
			name: "test".to_string(),
			enabled: true,
			transport,
			timeout: Some(60),
			config_source: None,
		};

		let json = serde_json::to_string(&mcp).unwrap();
		assert!(json.contains("\"timeout\":60"));
	}

	fn headers(pairs: &[(&str, &str)]) -> HashMap<String, String> {
		pairs
			.iter()
			.map(|(k, v)| (k.to_string(), v.to_string()))
			.collect()
	}

	#[test]
	fn from_inputs_stdio_rejects_headers() {
		let err = McpTransport::from_inputs(
			Some("npx".to_string()),
			None,
			DEFAULT_REMOTE_TRANSPORT,
			Some(headers(&[("A", "B")])),
			None,
			None,
		)
		.unwrap_err();
		assert!(err.to_string().contains("--header"));
	}

	#[test]
	fn from_inputs_url_rejects_env() {
		let err = McpTransport::from_inputs(
			None,
			Some("http://h".to_string()),
			DEFAULT_REMOTE_TRANSPORT,
			None,
			Some(headers(&[("K", "V")])),
			None,
		)
		.unwrap_err();
		assert!(err.to_string().contains("--env"));
	}

	#[test]
	fn from_inputs_rejects_command_and_url() {
		let err = McpTransport::from_inputs(
			Some("npx".to_string()),
			Some("http://h".to_string()),
			DEFAULT_REMOTE_TRANSPORT,
			None,
			None,
			None,
		)
		.unwrap_err();
		assert!(err.to_string().contains("mutually exclusive"));
	}

	#[test]
	fn from_inputs_rejects_zero_timeout() {
		let err = McpTransport::from_inputs(
			None,
			Some("http://h".to_string()),
			DEFAULT_REMOTE_TRANSPORT,
			None,
			None,
			Some(0),
		)
		.unwrap_err();
		assert!(err.to_string().contains("timeout"));
	}

	#[test]
	fn from_inputs_unknown_transport_type_errs() {
		let err = McpTransport::from_inputs(
			None,
			Some("http://h".to_string()),
			"bogus",
			None,
			None,
			None,
		)
		.unwrap_err();
		assert!(err.to_string().contains("bogus"));
	}

	#[test]
	fn from_inputs_url_default_streamable_and_sse_ok() {
		let streamable = McpTransport::from_inputs(
			None,
			Some("http://h".to_string()),
			DEFAULT_REMOTE_TRANSPORT,
			Some(headers(&[("A", "B")])),
			None,
			Some(30),
		)
		.unwrap()
		.unwrap();
		assert!(matches!(
			streamable,
			McpTransport::StreamableHttp {
				timeout: Some(30),
				..
			}
		));

		let sse = McpTransport::from_inputs(
			None,
			Some("http://h".to_string()),
			"sse",
			None,
			None,
			None,
		)
		.unwrap()
		.unwrap();
		assert!(matches!(sse, McpTransport::Sse { .. }));
	}

	#[test]
	fn from_inputs_none_when_no_command_or_url() {
		let result = McpTransport::from_inputs(
			None,
			None,
			DEFAULT_REMOTE_TRANSPORT,
			None,
			None,
			None,
		)
		.unwrap();
		assert!(result.is_none());
	}

	#[test]
	fn from_inputs_rejects_stray_headers_without_url() {
		// --header with neither --url nor --command must error, not be
		// silently dropped via the Ok(None) path.
		let err = McpTransport::from_inputs(
			None,
			None,
			DEFAULT_REMOTE_TRANSPORT,
			Some(headers(&[("A", "B")])),
			None,
			None,
		)
		.unwrap_err();
		assert!(err.to_string().contains("--header"));
	}

	#[test]
	fn from_inputs_rejects_stray_env_without_command() {
		let err = McpTransport::from_inputs(
			None,
			None,
			DEFAULT_REMOTE_TRANSPORT,
			None,
			Some(headers(&[("K", "V")])),
			None,
		)
		.unwrap_err();
		assert!(err.to_string().contains("--env"));
	}

	#[test]
	fn from_inputs_stdio_empty_command_errs() {
		let err = McpTransport::from_inputs(
			Some("   ".to_string()),
			None,
			DEFAULT_REMOTE_TRANSPORT,
			None,
			None,
			None,
		)
		.unwrap_err();
		assert!(err.to_string().contains("command"));
	}

	#[test]
	fn from_inputs_stdio_splits_command_and_keeps_env() {
		let env = headers(&[("TOKEN", "x")]);
		let transport = McpTransport::from_inputs(
			Some("npx -y server".to_string()),
			None,
			DEFAULT_REMOTE_TRANSPORT,
			None,
			Some(env.clone()),
			Some(45),
		)
		.unwrap()
		.unwrap();
		match transport {
			McpTransport::Stdio {
				command,
				args,
				env: got_env,
				timeout,
			} => {
				assert_eq!(command, "npx");
				assert_eq!(args, vec!["-y", "server"]);
				assert_eq!(got_env, Some(env));
				assert_eq!(timeout, Some(45));
			}
			other => panic!("expected stdio, got {other:?}"),
		}
	}

	#[test]
	fn from_inputs_stdio_empty_headers_map_allowed() {
		// An empty (non-None) headers map must not trip the stdio guard.
		let transport = McpTransport::from_inputs(
			Some("npx".to_string()),
			None,
			DEFAULT_REMOTE_TRANSPORT,
			Some(HashMap::new()),
			None,
			None,
		)
		.unwrap()
		.unwrap();
		assert!(matches!(transport, McpTransport::Stdio { .. }));
	}

	#[test]
	fn from_inputs_url_empty_env_map_allowed() {
		let transport = McpTransport::from_inputs(
			None,
			Some("http://h".to_string()),
			DEFAULT_REMOTE_TRANSPORT,
			None,
			Some(HashMap::new()),
			None,
		)
		.unwrap()
		.unwrap();
		assert!(matches!(transport, McpTransport::StreamableHttp { .. }));
	}

	#[test]
	fn test_agent_config_default() {
		let config = AgentConfig::new();
		assert!(config.skills.is_empty());
		assert!(config.mcps.is_empty());
	}

	#[test]
	fn hermes_agent_type_roundtrip() {
		use std::str::FromStr;
		let a = AgentType::from_str("hermes").unwrap();
		assert_eq!(a, AgentType::Hermes);
		assert_eq!(a.as_str(), "hermes");
		assert!(AgentType::ALL.contains(&AgentType::Hermes));
	}

	#[test]
	fn grok_agent_type_roundtrip() {
		use std::str::FromStr;
		let a = AgentType::from_str("grok").unwrap();
		assert_eq!(a, AgentType::Grok);
		assert_eq!(a.as_str(), "grok");
		assert!(AgentType::ALL.contains(&AgentType::Grok));
	}

	#[test]
	fn parse_list_single_multi_and_dedup() {
		assert_eq!(
			AgentType::parse_list("claude").unwrap(),
			vec![AgentType::Claude]
		);
		// Order preserved, whitespace trimmed, duplicates dropped.
		assert_eq!(
			AgentType::parse_list("grok, claude,grok").unwrap(),
			vec![AgentType::Grok, AgentType::Claude]
		);
	}

	#[test]
	fn parse_list_rejects_unknown_and_empty_tokens() {
		let err = AgentType::parse_list("claude,nonesuch").unwrap_err();
		assert!(err.contains("nonesuch"), "err must name the token: {err}");
		assert!(err.contains("claude"), "err must list valid ids: {err}");
		assert!(AgentType::parse_list("claude,,grok").is_err());
		assert!(AgentType::parse_list("").is_err());
	}

	#[test]
	fn from_inputs_url_empty_string_errs() {
		// `--url ""` must be rejected, mirroring the empty-command guard.
		let err = McpTransport::from_inputs(
			None,
			Some("".to_string()),
			DEFAULT_REMOTE_TRANSPORT,
			None,
			None,
			None,
		)
		.unwrap_err();
		assert!(matches!(
			err,
			crate::errors::ConfigError::ValidationFailed(msg) if msg.contains("url")
		));
	}

	#[test]
	fn validate_values_rejects_empty_command_and_url() {
		let empty_cmd = McpTransport::Stdio {
			command: "  ".to_string(),
			args: vec![],
			env: None,
			timeout: None,
		};
		assert!(empty_cmd.validate_values().is_err());

		let empty_url = McpTransport::StreamableHttp {
			url: "".to_string(),
			headers: None,
			timeout: None,
		};
		assert!(empty_url.validate_values().is_err());

		let ok = McpTransport::Stdio {
			command: "npx".to_string(),
			args: vec![],
			env: None,
			timeout: None,
		};
		assert!(ok.validate_values().is_ok());
	}
}
