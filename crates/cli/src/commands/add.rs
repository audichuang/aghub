use crate::{eprintln_verbose, ResourceType};
use aghub_core::{
	manager::ConfigManager,
	models::{McpServer, Skill},
};
use anyhow::{anyhow, Result};
use std::path::PathBuf;

use super::parse_mcp_transport;

/// After a skill add, tell the user when the target agent reads the
/// `.agents/skills` master directly (a NativeReader) and so got the master only,
/// with no per-agent symlink — the CLI equivalent of the desktop "already
/// covered" chip.
fn note_if_native_reader(manager: &ConfigManager) {
	if manager.skill_target_is_native_reader() {
		eprintln!(
			"note: agent '{}' reads the .agents/skills master directly; \
			 no per-agent link was created (already covered)",
			manager.agent_name()
		);
	}
}

#[allow(clippy::too_many_arguments)]
pub fn execute(
	manager: &mut ConfigManager,
	resource: ResourceType,
	name: Option<String>,
	from: Option<PathBuf>,
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
	universal: bool,
) -> Result<()> {
	if universal {
		eprintln!(
			"warning: --universal is deprecated and ignored; \
			 skill installs are always symlink-only \
			 (.agents/skills master + per-agent link)"
		);
	}
	match resource {
		ResourceType::Skills => {
			if let Some(from_path) = from {
				eprintln_verbose!(
					"Importing skill from: {}",
					from_path.display()
				);
				let mut skill = manager.add_skill_from_path(&from_path)?;

				if let Some(custom_name) = name {
					eprintln_verbose!(
						"Renaming skill from '{}' to '{}'",
						skill.name,
						custom_name
					);
					manager.remove_skill(&skill.name)?;
					skill.name = custom_name;
					manager.add_skill(skill.clone())?;
				}

				eprintln_verbose!("Skill '{}' added successfully", skill.name);
				note_if_native_reader(manager);
				println!("{}", serde_json::to_string_pretty(&skill)?);
			} else {
				let skill_name = name.ok_or_else(|| {
					anyhow!("--name is required when not using --from")
				})?;
				eprintln_verbose!("Adding skill: {}", skill_name);
				let mut skill = Skill::new(skill_name);
				skill.description = description;
				skill.author = author;
				skill.version = version;
				skill.tools = tools;
				manager.add_skill(skill.clone())?;
				eprintln_verbose!("Skill added successfully");
				note_if_native_reader(manager);
				println!("{}", serde_json::to_string_pretty(&skill)?);
			}
		}
		ResourceType::Mcps => {
			let mcp_name = name
				.ok_or_else(|| anyhow!("--name is required for MCP servers"))?;

			let mcp_transport = parse_mcp_transport(
				command, url, &transport, headers, env_vars, timeout,
			)?;

			let transport = mcp_transport.ok_or_else(|| {
				anyhow!("Either --command or --url must be specified for MCP servers")
			})?;

			eprintln_verbose!("Adding MCP server: {}", mcp_name);
			let mcp = McpServer::new(mcp_name, transport);
			manager.add_mcp(mcp.clone())?;
			eprintln_verbose!("MCP server added successfully");
			println!("{}", serde_json::to_string_pretty(&mcp)?);
		}
	}

	Ok(())
}
