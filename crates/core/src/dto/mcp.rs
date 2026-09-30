//! Shared MCP wire DTO.
//!
//! [`McpView`] is the single source of truth for the field list both the CLI
//! (`get mcps` output) and the API (`McpResponse`) serialize from an
//! [`McpServer`].

use crate::models::{ConfigSource, McpServer, McpTransport};
use serde::Serialize;

/// Wire view of an [`McpServer`].
#[derive(Debug, Clone, Serialize)]
pub struct McpView {
	pub name: String,
	pub enabled: bool,
	pub transport: McpTransport,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub timeout: Option<u64>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub source: Option<ConfigSource>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub agent: Option<String>,
}

impl From<&McpServer> for McpView {
	fn from(server: &McpServer) -> Self {
		Self {
			name: server.name.clone(),
			enabled: server.enabled,
			transport: server.transport.clone(),
			timeout: server.timeout,
			source: server.config_source,
			agent: None,
		}
	}
}

impl McpView {
	/// Tag the view with the agent it was resolved for.
	pub fn with_agent(mut self, agent_id: &str) -> Self {
		self.agent = Some(agent_id.to_string());
		self
	}
}
