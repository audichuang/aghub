use crate::{eprintln_verbose, ResourceType};
use aghub_core::{
	manager::ConfigManager,
	models::{McpServer, Skill},
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
			"note: agent '{}' shares its skills directory with {}; they can \
			 read this skill too, and removing it from one removes it from all \
			 of them (their own agents provide no separate directory)",
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
		universal,
	} = args;
	if universal {
		eprintln!(
			"warning: --universal is deprecated and ignored; \
			 skill installs are always symlink-only \
			 (.agents/skills master + per-agent link)"
		);
	}
	// The caller prints the payload (single-agent) or wraps it in the batch
	// envelope (multi-agent) — command logic stays print-free.
	let payload = match resource {
		ResourceType::Skills => {
			if let Some(from_path) = from {
				eprintln_verbose!(
					"Importing skill from: {}",
					from_path.display()
				);
				// `--name` is written by the install itself: duplicate check,
				// copy and frontmatter fix are one span under one lock, and a
				// conflict is refused BEFORE any write, so no rollback is
				// needed. See docs/history/cli.md#add-from-with-name
				let added = manager.add_skill_from_path_universal(
					&from_path,
					name.as_deref(),
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
	};

	Ok(payload)
}
