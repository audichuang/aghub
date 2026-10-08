use crate::{eprintln_verbose, ResourceType};
use aghub_core::{
	manager::ConfigManager,
	models::{McpServer, Skill, SubAgent},
};
use anyhow::{anyhow, Result};
use std::path::PathBuf;

use super::parse_mcp_transport;

/// After a skill add, tell the user when the Referrer just written is SHARED —
/// several agents read the same directory, so this grant reached all of them.
fn note_shared_slot(manager: &ConfigManager) {
	let shared = manager.skill_target_shares_with();
	if !shared.is_empty() {
		eprintln!(
			// `shared_with` names the agents that WRITE this directory; more may
			// read it, so "including". Removing it from one alone is refused.
			"note: agent '{}' writes this skill into a directory other agents \
			 read too (including {}); they get it as well, and it cannot be \
			 removed from just one of them — name them together in one -a \
			 list, or use --all-agents",
			manager.agent_name(),
			shared.join(", ")
		);
	}
}

/// The `add` clap flags, forwarded as one value.
pub struct AddArgs {
	pub name: Option<String>,
	pub from: Option<PathBuf>,
	pub command: Option<String>,
	pub url: Option<String>,
	pub transport: String,
	pub headers: Vec<String>,
	pub env_vars: Vec<String>,
	pub timeout: Option<u64>,
	pub description: Option<String>,
	pub author: Option<String>,
	pub version: Option<String>,
	pub tools: Vec<String>,
	pub instruction: Option<String>,
	pub universal: bool,
}

pub fn execute(
	manager: &mut ConfigManager,
	resource: ResourceType,
	args: AddArgs,
) -> Result<serde_json::Value> {
	let AddArgs {
		name,
		from,
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
		universal,
	} = args;
	if universal && resource == ResourceType::SubAgents {
		anyhow::bail!("--universal is not valid for sub-agents");
	}
	if universal {
		eprintln!(
			"warning: --universal is deprecated and ignored; \
			 skill installs are always symlink-only \
			 (.aghub master + per-agent link)"
		);
	}
	// The caller prints the payload (single-agent) or wraps it in the batch
	// envelope (multi-agent) — command logic stays print-free.
	let payload = match resource {
		ResourceType::Skills => {
			if instruction.is_some() {
				anyhow::bail!("--instruction is only valid for sub-agents");
			}
			if let Some(from_path) = from {
				eprintln_verbose!(
					"Importing skill from: {}",
					from_path.display()
				);
				let agent_type = manager.agent_type();
				let added = aghub_core::skills::install_local::install_local_skill(
					aghub_core::skills::install_local::LocalSkillInstallRequest {
						source_path: &from_path,
						scope: manager.write_scope()?.clone(),
						target_agents: &[agent_type],
						install_name: name.as_deref(),
					},
				)?;
				let skill = added.skill;

				if added.already_installed {
					// The install wrote NOTHING: the Master was already there.
					// Say so, because the payload below reports that untouched
					// Master and a user who just edited the source would
					// otherwise read it as a successful overwrite.
					eprintln!(
						"note: nothing was written — the existing \
						 master was left as-is. To take the \
						 source's current content, delete the skill \
						 (aghub-cli delete skills {} --yes) and remove the \
						 master it reports as kept, then add it again.",
						skill.name
					);
				}

				eprintln_verbose!("Skill '{}' added successfully", skill.name);
				note_shared_slot(manager);
				// An idempotent re-add is a no-op, and both the human verb
				// ("added" vs "already installed") and a scripted caller need
				// to tell it from a real install.
				let view = aghub_core::dto::SkillView::from(&skill)
					.with_shared_with(
						manager
							.skill_target_shares_with()
							.iter()
							.map(|s| (*s).to_string())
							.collect(),
					)
					// An explicit `--name` that is taken is an ERROR, so a
					// successful rename never reports `already_installed`.
					.with_already_installed(added.already_installed);
				serde_json::to_value(&view)?
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
				let added = manager.add_skill(skill)?;
				eprintln_verbose!("Skill added successfully");
				note_shared_slot(manager);
				// Serialize the skill the manager reports on disk, NOT the
				// request, and carry `already_installed`: a re-add is an
				// idempotent no-op. See docs/history/cli.md#manual-add-reports-disk
				if added.already_installed {
					eprintln!(
						"note: nothing was written — skill '{}' is already \
						 installed for this agent; use `aghub-cli update \
						 skills {}` to change its metadata",
						added.skill.name, added.skill.name
					);
				}
				let view = aghub_core::dto::SkillView::from(&added.skill)
					.with_shared_with(
						manager
							.skill_target_shares_with()
							.iter()
							.map(|s| (*s).to_string())
							.collect(),
					)
					.with_already_installed(added.already_installed);
				serde_json::to_value(&view)?
			}
		}
		ResourceType::Mcps => {
			if instruction.is_some() {
				anyhow::bail!("--instruction is only valid for sub-agents");
			}
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
			manager.add_mcp_exact(mcp.clone())?;
			eprintln_verbose!("MCP server added successfully");
			serde_json::to_value(&mcp)?
		}
		ResourceType::SubAgents => {
			if command.is_some() {
				anyhow::bail!("--command is not valid for sub-agents");
			}
			if url.is_some() {
				anyhow::bail!("--url is not valid for sub-agents");
			}
			// ponytail: the default hides an explicit -t of the default value
			if transport != aghub_core::models::DEFAULT_REMOTE_TRANSPORT {
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
			if from.is_some() {
				anyhow::bail!("--from is not valid for sub-agents");
			}
			if author.is_some() {
				anyhow::bail!("--author is not valid for sub-agents");
			}
			if version.is_some() {
				anyhow::bail!("--version is not valid for sub-agents");
			}
			if !tools.is_empty() {
				anyhow::bail!("--tools is not valid for sub-agents");
			}

			let name = name
				.ok_or_else(|| anyhow!("--name is required for sub-agents"))?;
			let description = description.ok_or_else(|| {
				anyhow!("--description is required for sub-agents")
			})?;
			let instruction = instruction.ok_or_else(|| {
				anyhow!("--instruction is required for sub-agents")
			})?;

			eprintln_verbose!("Adding sub-agent: {}", name);
			let agent = SubAgent {
				name,
				description: Some(description),
				instruction: Some(instruction),
				source_path: None,
				config_source: None,
				extra_frontmatter: Default::default(),
			};
			let view = aghub_core::dto::SubAgentView::from(&agent);
			manager.add_sub_agent(agent)?;
			eprintln_verbose!("Sub-agent added successfully");
			serde_json::to_value(&view)?
		}
	};

	Ok(payload)
}
