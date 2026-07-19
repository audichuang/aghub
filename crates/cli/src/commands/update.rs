use crate::{eprintln_verbose, ResourceType};
use aghub_core::{
	errors::ConfigError, manager::ConfigManager, models::McpTransport,
};
use anyhow::Result;

use super::parse_mcp_transport;

/// Patch the per-transport `timeout` field in place (used when only
/// `--timeout` is given on an MCP update, with no new `--command`/`--url`).
fn set_transport_timeout(transport: &mut McpTransport, value: Option<u64>) {
	match transport {
		McpTransport::Stdio { timeout, .. }
		| McpTransport::Sse { timeout, .. }
		| McpTransport::StreamableHttp { timeout, .. } => *timeout = value,
	}
}

#[allow(clippy::too_many_arguments)]
pub fn execute(
	manager: &mut ConfigManager,
	resource: ResourceType,
	name: String,
	command: Option<String>,
	url: Option<String>,
	transport: String,
	headers: Vec<String>,
	env_vars: Vec<String>,
	timeout: Option<u64>,
	description: Option<String>,
	author: Option<String>,
	version: Option<String>,
	tools: Vec<String>,
) -> Result<serde_json::Value> {
	// The caller prints the payload (single-agent) or wraps it in the batch
	// envelope (multi-agent) — command logic stays print-free.
	let payload = match resource {
		ResourceType::Skills => {
			eprintln_verbose!("Updating skill: {}", name);
			// Get existing skill
			let existing = manager.get_skill(&name).ok_or_else(|| {
				ConfigError::resource_not_found("skill", &name)
			})?;

			let mut skill = existing.clone();

			// Update fields if provided
			if let Some(desc) = description {
				skill.description = Some(desc);
			}
			if let Some(auth) = author {
				skill.author = Some(auth);
			}
			if let Some(ver) = version {
				skill.version = Some(ver);
			}
			if !tools.is_empty() {
				skill.tools = tools;
			}

			manager.update_skill(&name, skill.clone())?;
			eprintln_verbose!("Skill updated successfully");
			// Same SkillView shape as add/describe/get; update does no
			// install prep, so native_reader stays false.
			let view = aghub_core::dto::SkillView::from(&skill);
			serde_json::to_value(&view)?
		}
		ResourceType::Mcps => {
			eprintln_verbose!("Updating MCP server: {}", name);
			// Get existing MCP
			let existing = manager.get_mcp(&name).ok_or_else(|| {
				ConfigError::resource_not_found("MCP server", &name)
			})?;

			let mut mcp = existing.clone();

			// Preserve existing timeout unless --timeout overrides it.
			let existing_timeout = match &mcp.transport {
				McpTransport::Stdio { timeout, .. } => *timeout,
				McpTransport::Sse { timeout, .. } => *timeout,
				McpTransport::StreamableHttp { timeout, .. } => *timeout,
			};
			let effective_timeout = timeout.or(existing_timeout);

			// Rebuild the transport when --command/--url are given; this also
			// validates timeout (incl. timeout==0) on that path.
			if let Some(new_transport) = parse_mcp_transport(
				command,
				url,
				&transport,
				headers,
				env_vars,
				effective_timeout,
			)? {
				mcp.transport = new_transport;
			} else if timeout.is_some() {
				// --timeout alone: patch the existing transport in place.
				set_transport_timeout(&mut mcp.transport, effective_timeout);
			}

			manager.update_mcp(&name, mcp.clone())?;
			eprintln_verbose!("MCP server updated successfully");
			serde_json::to_value(&mcp)?
		}
	};

	Ok(payload)
}
