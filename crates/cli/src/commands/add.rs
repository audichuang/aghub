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
///
/// This replaces the old "already covered" note. That note described the leak as
/// a feature: those agents got the skill because storing it granted it, and the
/// user had no way to opt out. Now they get it only when their slot is written,
/// and the ones sharing that slot are named.
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
) -> Result<serde_json::Value> {
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
				// `--name` is NOT a second step. It used to be
				// import-then-`update_skill`-rename, which released the
				// mutation lock between the halves and stranded the imported
				// skill whenever the rename failed. The install now writes the
				// requested name directly, so the duplicate check, the copy and
				// the frontmatter fix are one span under one lock — and a
				// conflict is refused BEFORE anything is written, which is why
				// this flow needs no rollback of its own.
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
					//
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
					// No `&& !renamed` correction any more: an explicit
					// `--name` that finds the name taken is now an ERROR, so a
					// successful rename can never report `already_installed`.
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
				// Serialize the skill the manager reports on disk, NOT the one
				// that was requested, and carry `already_installed` through.
				// This branch used to build the view from the request and hard-
				// code `already_installed: false`, on a comment claiming a
				// manual add always errors on a duplicate. It does not: two of
				// `add_skill_universal`'s branches are idempotent no-ops, so a
				// re-add with a changed --description printed "added skill",
				// echoed the NEW description back, and left the Master alone.
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
			manager.add_mcp(mcp.clone())?;
			eprintln_verbose!("MCP server added successfully");
			serde_json::to_value(&mcp)?
		}
	};

	Ok(payload)
}
