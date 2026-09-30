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

/// The request-level `timeout` duplicates the transport's own: desktop sends
/// both. No dialect writes the model-level field, so keeping it there would make
/// every edit "lose a field"; fold it into the transport, whose fit the dialect
/// probe actually answers. A timeout the transport already spells wins.
fn fold_timeout(
	mut transport: McpTransport,
	timeout: Option<u64>,
) -> McpTransport {
	let own = transport.timeout();
	transport.set_timeout(own.or(timeout));
	transport
}

impl From<CreateMcpRequest> for McpServer {
	fn from(req: CreateMcpRequest) -> Self {
		McpServer {
			name: req.name,
			enabled: true,
			transport: fold_timeout(req.transport.into(), req.timeout),
			timeout: None,
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

	/// Apply this update onto an existing MCP server.
	///
	/// A PUT that carries a transport is a FULL REPLACEMENT on purpose: the GUI
	/// form always sends the whole transport and omits `headers`/`env` to CLEAR
	/// them; merging would resurrect them. The CLI `update mcps` is a patch and
	/// uses `McpTransport::apply_edit` in core instead.
	pub fn apply_to(self, existing: McpServer) -> McpServer {
		McpServer {
			name: self.name.unwrap_or(existing.name),
			enabled: self.enabled.unwrap_or(existing.enabled),
			// A supplied transport carries its own timeout first; without one,
			// the request-level timeout is an edit of the existing transport's.
			transport: match self.transport {
				Some(transport) => fold_timeout(transport.into(), self.timeout),
				None => {
					let mut t = existing.transport;
					t.set_timeout(self.timeout.or(t.timeout()));
					t
				}
			},
			timeout: existing.timeout,
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
		let view = aghub_core::dto::McpView::from(s);
		McpResponse {
			name: view.name,
			enabled: view.enabled,
			transport: TransportDto::from(&view.transport),
			timeout: view.timeout,
			source: view.source.map(Into::into),
			agent: view.agent,
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
			None,
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

#[cfg(test)]
mod tests {
	use super::*;
	use aghub_core::dto::McpView;
	use aghub_core::models::{ConfigSource, McpServer, McpTransport};
	use serde_json::Value;

	// TransportDto skips None env/headers while McpTransport's own serde
	// serializes them as null, so nulls are stripped before comparing.
	fn strip_nulls(value: Value) -> Value {
		match value {
			Value::Object(map) => Value::Object(
				map.into_iter()
					.filter(|(_, v)| !v.is_null())
					.map(|(k, v)| (k, strip_nulls(v)))
					.collect(),
			),
			Value::Array(vec) => {
				Value::Array(vec.into_iter().map(strip_nulls).collect())
			}
			other => other,
		}
	}

	#[test]
	fn mcp_response_matches_core_mcp_view() {
		let mut env = HashMap::new();
		env.insert("KEY".to_string(), "VAL".to_string());

		let stdio_server = McpServer {
			name: "stdio-srv".to_string(),
			enabled: true,
			transport: McpTransport::Stdio {
				command: "echo".to_string(),
				args: vec!["hello".to_string()],
				env: Some(env),
				timeout: None,
			},
			timeout: None,
			config_source: Some(ConfigSource::Global),
		};

		let sse_server = McpServer::new(
			"sse-srv",
			McpTransport::Sse {
				url: "http://example.com/events".to_string(),
				headers: None,
				timeout: None,
			},
		);

		let mut timeout_server = McpServer::new(
			"timeout-srv",
			McpTransport::streamable_http("http://example.com/stream"),
		);
		timeout_server.timeout = Some(30);

		for s in [stdio_server, sse_server, timeout_server] {
			assert_eq!(
				strip_nulls(
					serde_json::to_value(McpResponse::from(&s)).unwrap()
				),
				strip_nulls(serde_json::to_value(McpView::from(&s)).unwrap()),
			);
		}
	}
}
