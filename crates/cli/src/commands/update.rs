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
			// Parse input errors before taking the mutation lock. The existing
			// server and its inherited timeout are read only after fresh reload.
			let parsed_transport = parse_mcp_transport(
				command, url, &transport, headers, env_vars, timeout,
			)?;
			let mcp = manager.update_mcp_with(&name, move |mcp| {
				let inherited_timeout = match &mcp.transport {
					McpTransport::Stdio { timeout, .. }
					| McpTransport::Sse { timeout, .. }
					| McpTransport::StreamableHttp { timeout, .. } => *timeout,
				};
				if let Some(mut new_transport) = parsed_transport {
					if timeout.is_none() {
						set_transport_timeout(
							&mut new_transport,
							inherited_timeout,
						);
					}
					mcp.transport = new_transport;
				} else if timeout.is_some() {
					set_transport_timeout(&mut mcp.transport, timeout);
				}
				Ok(())
			})?;
			eprintln_verbose!("MCP server updated successfully");
			serde_json::to_value(&mcp)?
		}
	};

	Ok(payload)
}

#[cfg(test)]
mod tests {
	use super::*;
	use aghub_core::{
		adapters::create_adapter,
		models::{AgentType, McpServer},
	};

	#[test]
	fn mcp_timeout_patch_preserves_a_command_changed_after_its_manager_loaded()
	{
		let project = tempfile::tempdir().unwrap();
		let path = project.path().join(".codex/config.toml");
		std::fs::create_dir_all(path.parent().unwrap()).unwrap();
		std::fs::write(&path, "").unwrap();
		let manager = || {
			ConfigManager::new(
				create_adapter(AgentType::Codex),
				false,
				Some(project.path()),
			)
		};
		let mut seed = manager();
		seed.load().unwrap();
		seed.add_mcp(McpServer::new(
			"server",
			McpTransport::stdio("old", vec![]),
		))
		.unwrap();
		let mut first = manager();
		let mut stale = manager();
		first.load().unwrap();
		stale.load().unwrap();

		execute(
			&mut first,
			ResourceType::Mcps,
			"server".into(),
			Some("new".into()),
			None,
			"stdio".into(),
			vec![],
			vec![],
			None,
			None,
			None,
			None,
			vec![],
		)
		.unwrap();
		execute(
			&mut stale,
			ResourceType::Mcps,
			"server".into(),
			None,
			None,
			"stdio".into(),
			vec![],
			vec![],
			Some(45),
			None,
			None,
			None,
			vec![],
		)
		.unwrap();

		let mut observed = manager();
		observed.load().unwrap();
		let actual = observed.get_mcp("server").unwrap();
		assert!(
			matches!(&actual.transport, McpTransport::Stdio { command, timeout: Some(45), .. } if command == "new"),
			"both independent updates must survive: {actual:?}"
		);
	}

	#[test]
	fn mcp_timeout_patch_refuses_cursor_when_timeout_cannot_persist() {
		let project = tempfile::tempdir().unwrap();
		let manager = || {
			ConfigManager::new(
				create_adapter(AgentType::Cursor),
				false,
				Some(project.path()),
			)
		};
		let mut seed = manager();
		seed.load().unwrap();
		seed.add_mcp(McpServer::new(
			"server",
			McpTransport::stdio("echo", vec![]),
		))
		.unwrap();
		let path = project.path().join(".cursor/mcp.json");
		let original = std::fs::read_to_string(&path).unwrap();
		let mut updater = manager();
		updater.load().unwrap();
		let error = execute(
			&mut updater,
			ResourceType::Mcps,
			"server".into(),
			None,
			None,
			"stdio".into(),
			vec![],
			vec![],
			Some(45),
			None,
			None,
			None,
			vec![],
		)
		.expect_err("unpersistable timeout must fail");
		assert!(
			error.to_string().contains("without losing fields"),
			"{error}"
		);
		assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
		let mut observed = manager();
		observed.load().unwrap();
		let actual = observed.get_mcp("server").unwrap();
		assert!(matches!(
			&actual.transport,
			McpTransport::Stdio { timeout: None, .. }
		));
	}
}
