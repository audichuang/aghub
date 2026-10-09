use crate::{eprintln_verbose, ResourceType};
use aghub_core::{
	errors::ConfigError,
	manager::{skill::SkillPatch, ConfigManager},
	models::McpTransportEdit,
};
use anyhow::Result;

use super::{parse_env_vars, parse_headers};

/// The `update` clap flags, forwarded as one value.
pub struct UpdateArgs {
	pub command: Option<String>,
	pub url: Option<String>,
	pub transport: Option<String>,
	pub headers: Vec<String>,
	pub env_vars: Vec<String>,
	pub timeout: Option<u64>,
	pub description: Option<String>,
	pub author: Option<String>,
	pub version: Option<String>,
	pub tools: Option<Vec<String>>,
	pub instruction: Option<String>,
}

pub fn execute(
	manager: &mut ConfigManager,
	resource: ResourceType,
	name: String,
	args: UpdateArgs,
) -> Result<serde_json::Value> {
	let UpdateArgs {
		command,
		url,
		transport,
		headers,
		env_vars,
		timeout,
		description,
		author,
		version,
		tools,
		instruction,
	} = args;
	// The caller prints the payload (single-agent) or wraps it in the batch
	// envelope (multi-agent) — command logic stays print-free.
	let payload = match resource {
		ResourceType::Skills => {
			if instruction.is_some() {
				anyhow::bail!("--instruction is only valid for sub-agents");
			}
			eprintln_verbose!("Updating skill: {}", name);
			// Get existing skill
			let existing = manager.get_skill(&name).ok_or_else(|| {
				ConfigError::resource_not_found("skill", &name)
			})?;

			let skill = SkillPatch {
				description,
				author,
				version,
				tools,
				..Default::default()
			}
			.apply_to(existing.clone());

			manager.update_skill(
				&name,
				skill.clone(),
				&super::plugin_roots(),
			)?;
			eprintln_verbose!("Skill updated successfully");
			// Same SkillView shape as add/describe/get; update does no
			// install prep, so native_reader stays false.
			let view = aghub_core::dto::SkillView::from(&skill);
			serde_json::to_value(&view)?
		}
		ResourceType::Mcps => {
			if instruction.is_some() {
				anyhow::bail!("--instruction is only valid for sub-agents");
			}
			eprintln_verbose!("Updating MCP server: {}", name);
			let headers = parse_headers(headers)?;
			let env = parse_env_vars(env_vars)?;
			let edit = McpTransportEdit {
				command,
				url,
				transport_type: transport,
				headers,
				env,
				timeout,
			};
			let mcp = manager.update_mcp_with(&name, move |mcp| {
				mcp.transport = mcp.transport.apply_edit(edit)?;
				Ok(())
			})?;
			eprintln_verbose!("MCP server updated successfully");
			serde_json::to_value(&mcp)?
		}
		ResourceType::SubAgents => {
			if command.is_some() {
				anyhow::bail!("--command is not valid for sub-agents");
			}
			if url.is_some() {
				anyhow::bail!("--url is not valid for sub-agents");
			}
			if transport.is_some() {
				anyhow::bail!("--transport is not valid for sub-agents");
			}
			if !headers.is_empty() {
				anyhow::bail!("--header is not valid for sub-agents");
			}
			if !env_vars.is_empty() {
				anyhow::bail!("--env is not valid for sub-agents");
			}
			if timeout.is_some() {
				anyhow::bail!("--timeout is not valid for sub-agents");
			}
			if author.is_some() {
				anyhow::bail!("--author is not valid for sub-agents");
			}
			if version.is_some() {
				anyhow::bail!("--version is not valid for sub-agents");
			}
			if tools.is_some() {
				anyhow::bail!("--tools is not valid for sub-agents");
			}
			if description.is_none() && instruction.is_none() {
				anyhow::bail!(
					"nothing to update: pass -d and/or --instruction"
				);
			}

			eprintln_verbose!("Updating sub-agent: {}", name);
			let patch = aghub_core::manager::sub_agent::SubAgentPatch {
				name: None,
				description,
				instruction,
			};
			manager.update_sub_agent(&name, patch)?;
			eprintln_verbose!("Sub-agent updated successfully");
			let updated = manager.get_sub_agent(&name).ok_or_else(|| {
				ConfigError::resource_not_found("sub-agent", &name)
			})?;
			let view = aghub_core::dto::SubAgentView::from(updated);
			serde_json::to_value(&view)?
		}
	};

	Ok(payload)
}

#[cfg(test)]
mod tests {
	use super::*;
	use aghub_core::{
		adapters::create_adapter,
		models::{AgentType, McpServer, McpTransport},
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
			UpdateArgs {
				command: Some("new".into()),
				url: None,
				transport: None,
				headers: vec![],
				env_vars: vec![],
				timeout: None,
				description: None,
				author: None,
				version: None,
				tools: None,
				instruction: None,
			},
		)
		.unwrap();
		execute(
			&mut stale,
			ResourceType::Mcps,
			"server".into(),
			UpdateArgs {
				command: None,
				url: None,
				transport: None,
				headers: vec![],
				env_vars: vec![],
				timeout: Some(45),
				description: None,
				author: None,
				version: None,
				tools: None,
				instruction: None,
			},
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
			UpdateArgs {
				command: None,
				url: None,
				transport: None,
				headers: vec![],
				env_vars: vec![],
				timeout: Some(45),
				description: None,
				author: None,
				version: None,
				tools: None,
				instruction: None,
			},
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
