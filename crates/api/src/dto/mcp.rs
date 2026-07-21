use aghub_core::models::{reject_zero_timeout, McpServer, McpTransport};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use ts_rs::TS;

use crate::dto::common::ConfigSource;
use crate::error::ApiError;

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TransportDto {
	Stdio {
		command: String,
		#[serde(default)]
		args: Vec<String>,
		#[serde(skip_serializing_if = "Option::is_none")]
		env: Option<HashMap<String, String>>,
		#[serde(skip_serializing_if = "Option::is_none")]
		timeout: Option<u64>,
	},
	Sse {
		url: String,
		#[serde(skip_serializing_if = "Option::is_none")]
		headers: Option<HashMap<String, String>>,
		#[serde(skip_serializing_if = "Option::is_none")]
		timeout: Option<u64>,
	},
	StreamableHttp {
		url: String,
		#[serde(skip_serializing_if = "Option::is_none")]
		headers: Option<HashMap<String, String>>,
		#[serde(skip_serializing_if = "Option::is_none")]
		timeout: Option<u64>,
	},
}

impl TransportDto {
	/// Per-transport timeout (the `timeout` field inside the variant).
	fn timeout(&self) -> Option<u64> {
		match self {
			TransportDto::Stdio { timeout, .. }
			| TransportDto::Sse { timeout, .. }
			| TransportDto::StreamableHttp { timeout, .. } => *timeout,
		}
	}
}

impl From<&McpTransport> for TransportDto {
	fn from(t: &McpTransport) -> Self {
		match t {
			McpTransport::Stdio {
				command,
				args,
				env,
				timeout,
			} => TransportDto::Stdio {
				command: command.clone(),
				args: args.clone(),
				env: env.clone(),
				timeout: *timeout,
			},
			McpTransport::Sse {
				url,
				headers,
				timeout,
			} => TransportDto::Sse {
				url: url.clone(),
				headers: headers.clone(),
				timeout: *timeout,
			},
			McpTransport::StreamableHttp {
				url,
				headers,
				timeout,
			} => TransportDto::StreamableHttp {
				url: url.clone(),
				headers: headers.clone(),
				timeout: *timeout,
			},
		}
	}
}

impl From<TransportDto> for McpTransport {
	fn from(dto: TransportDto) -> Self {
		match dto {
			TransportDto::Stdio {
				command,
				args,
				env,
				timeout,
			} => McpTransport::Stdio {
				command,
				args,
				env,
				timeout,
			},
			TransportDto::Sse {
				url,
				headers,
				timeout,
			} => McpTransport::Sse {
				url,
				headers,
				timeout,
			},
			TransportDto::StreamableHttp {
				url,
				headers,
				timeout,
			} => McpTransport::StreamableHttp {
				url,
				headers,
				timeout,
			},
		}
	}
}

#[derive(Debug, Clone, Deserialize, TS)]
#[ts(export)]
pub struct CreateMcpRequest {
	pub name: String,
	pub transport: TransportDto,
	pub timeout: Option<u64>,
}

impl CreateMcpRequest {
	/// Reject zero timeouts (request-level and per-transport) via the single
	/// shared `reject_zero_timeout` rule in core, so the API agrees with the
	/// CLI. `ConfigError::ValidationFailed` maps to a 422 `VALIDATION_FAILED`.
	pub fn validate(&self) -> Result<(), ApiError> {
		reject_zero_timeout(self.timeout)?;
		reject_zero_timeout(self.transport.timeout())?;
		// Reject structurally-empty command/url via the same core seam the CLI
		// uses, so the API can't create an unusable MCP (empty command/url).
		let transport: McpTransport = self.transport.clone().into();
		transport.validate_values().map_err(ApiError::from)?;
		Ok(())
	}
}

impl From<CreateMcpRequest> for McpServer {
	fn from(req: CreateMcpRequest) -> Self {
		McpServer {
			name: req.name,
			enabled: true,
			transport: req.transport.into(),
			timeout: req.timeout,
			config_source: None,
		}
	}
}

#[derive(Debug, Deserialize, TS)]
#[ts(export)]
pub struct UpdateMcpRequest {
	pub name: Option<String>,
	pub transport: Option<TransportDto>,
	pub enabled: Option<bool>,
	pub timeout: Option<u64>,
}

impl UpdateMcpRequest {
	/// Reject zero timeouts (request-level and per-transport, when a transport
	/// is supplied) via the single shared `reject_zero_timeout` rule in core,
	/// so the API agrees with the CLI.
	pub fn validate(&self) -> Result<(), ApiError> {
		reject_zero_timeout(self.timeout)?;
		if let Some(transport) = &self.transport {
			reject_zero_timeout(transport.timeout())?;
			// Same empty command/url guard as create — an update that supplies a
			// transport must not swap in a structurally-empty one.
			let transport: McpTransport = transport.clone().into();
			transport.validate_values().map_err(ApiError::from)?;
		}
		Ok(())
	}

	pub fn apply_to(self, existing: McpServer) -> McpServer {
		McpServer {
			name: self.name.unwrap_or(existing.name),
			enabled: self.enabled.unwrap_or(existing.enabled),
			transport: self
				.transport
				.map(Into::into)
				.unwrap_or(existing.transport),
			timeout: self.timeout.or(existing.timeout),
			config_source: existing.config_source,
		}
	}
}

#[derive(Debug, Serialize, TS)]
#[ts(export)]
pub struct McpResponse {
	pub name: String,
	pub enabled: bool,
	pub transport: TransportDto,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub timeout: Option<u64>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub source: Option<ConfigSource>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub agent: Option<String>,
}

impl From<McpServer> for McpResponse {
	fn from(s: McpServer) -> Self {
		McpResponse::from(&s)
	}
}

impl From<&McpServer> for McpResponse {
	fn from(s: &McpServer) -> Self {
		McpResponse {
			name: s.name.clone(),
			enabled: s.enabled,
			transport: TransportDto::from(&s.transport),
			timeout: s.timeout,
			source: s.config_source.map(Into::into),
			agent: None,
		}
	}
}

impl From<(McpServer, &str)> for McpResponse {
	fn from((s, agent_id): (McpServer, &str)) -> Self {
		McpResponse {
			agent: Some(agent_id.to_string()),
			..McpResponse::from(s)
		}
	}
}

/// Multi-agent MCP create (the desktop's multi-select) — one request mapped
/// onto the SHARED core batch policy (`aghub_core::batch`): preflight before
/// any write, attempt every agent, per-agent attribution back.
#[derive(Debug, Clone, Deserialize, TS)]
#[ts(export)]
pub struct BatchCreateMcpRequest {
	pub agents: Vec<String>,
	pub mcp: CreateMcpRequest,
}

/// Mirrors `aghub_core::batch::AgentOpResultView` byte-for-byte — the same
/// wire shape the CLI prints for `-a a,b` batches (see the transfer DTO
/// precedent and the test below).
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
pub struct AgentOpResultResponse {
	pub agent: String,
	pub ok: bool,
	// `skip_serializing_if` means the key is ABSENT (not null) on the wire,
	// so the TS side must be optional too or the generated type is unsound.
	#[serde(skip_serializing_if = "Option::is_none")]
	#[ts(optional, type = "unknown")]
	pub output: Option<serde_json::Value>,
	#[serde(skip_serializing_if = "Option::is_none")]
	#[ts(optional)]
	pub error: Option<String>,
}

/// Mirrors `aghub_core::batch::AgentBatchView`.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
pub struct AgentBatchResponse {
	pub success_count: usize,
	pub failed_count: usize,
	pub results: Vec<AgentOpResultResponse>,
}

impl From<aghub_core::batch::AgentBatchView> for AgentBatchResponse {
	fn from(view: aghub_core::batch::AgentBatchView) -> Self {
		AgentBatchResponse {
			success_count: view.success_count,
			failed_count: view.failed_count,
			results: view
				.results
				.into_iter()
				.map(|r| AgentOpResultResponse {
					agent: r.agent,
					ok: r.ok,
					output: r.output,
					error: r.error,
				})
				.collect(),
		}
	}
}

#[cfg(test)]
mod batch_dto_tests {
	use super::*;
	use aghub_core::batch::run_mcp_agent_mutation;
	use aghub_core::models::{AgentType, ResourceScope};

	/// The API DTO (ts-rs) and the shared core `AgentBatchView` (which the
	/// CLI serializes) must emit BYTE-IDENTICAL JSON — the single-source
	/// contract, same as the transfer batch precedent.
	#[test]
	fn batch_dto_matches_shared_core_view_byte_for_byte() {
		let view = run_mcp_agent_mutation(
			&[AgentType::Claude, AgentType::Grok],
			ResourceScope::GlobalOnly,
			false,
			|agent| match agent {
				AgentType::Claude => Ok(serde_json::json!({ "name": "multi" })),
				_ => Err("boom".to_string()),
			},
		)
		.expect("both agents support global MCPs");
		let view_json = serde_json::to_string(&view).unwrap();
		let dto_json =
			serde_json::to_string(&AgentBatchResponse::from(view)).unwrap();
		assert_eq!(
			dto_json, view_json,
			"API DTO and shared core view must serialize identically"
		);
	}

	/// `skip_serializing_if` omits absent keys on the wire, so the GENERATED
	/// TypeScript must declare `output`/`error` optional — a required field
	/// there is an unsound public contract the byte-identical test above
	/// cannot catch (it only compares Rust-side JSON).
	#[test]
	fn agent_op_result_ts_decl_marks_omittable_fields_optional() {
		use ts_rs::TS;
		let decl = AgentOpResultResponse::decl(&ts_rs::Config::default());
		assert!(
			decl.contains("output?"),
			"output must be optional in TS: {decl}"
		);
		assert!(
			decl.contains("error?"),
			"error must be optional in TS: {decl}"
		);
	}
}
