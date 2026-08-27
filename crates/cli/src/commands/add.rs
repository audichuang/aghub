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
				let added = manager.add_skill_from_path(&from_path)?;
				let mut skill = added.skill;
				// `--name` is implemented as import-then-rename, which only
				// works when the import actually imported something. When the
				// source's own name is ALREADY installed the import is a no-op
				// and `skill` is the pre-existing master — so the rename would
				// remove that master and re-add the OLD content under the new
				// name, discarding the source the user pointed at and deleting
				// a skill they never asked to touch. Refuse instead: nothing
				// has been written yet, so there is nothing to roll back.
				if added.already_installed && name.is_some() {
					return Err(anyhow!(
						"cannot import '{}' as a different name: a skill named \
						 '{}' (this source's own name) is already installed, so \
						 nothing was imported to rename. Delete '{}' first, or \
						 rename the skill in the source's SKILL.md.",
						from_path.display(),
						skill.name,
						skill.name
					));
				}
				// A `--name` rename REMOVES and RE-ADDS under the new name, so
				// it always writes. Inheriting the inner `already_installed`
				// would report a genuine install as a no-op.
				let renamed = name.is_some();

				if let Some(custom_name) = name {
					eprintln_verbose!(
						"Renaming skill from '{}' to '{}'",
						skill.name,
						custom_name
					);
					manager.remove_skill(&skill.name)?;
					skill.name = custom_name.clone();
					manager.add_skill(skill.clone())?;
					// Re-read the entry the rename just wrote: `skill` still
					// carries the ORIGINAL skill's paths, only its name having
					// been reassigned above.
					if let Some(installed) = manager
						.config()
						.and_then(|c| {
							c.skills.iter().find(|s| s.name == custom_name)
						})
						.cloned()
					{
						skill = installed;
					}
				} else if added.already_installed
					&& !manager.skill_target_is_native_reader()
				{
					// The install wrote NOTHING: the Master was already there.
					// Say so, because the payload below reports that untouched
					// Master and a user who just edited the source would
					// otherwise read it as a successful overwrite.
					//
					// NOT for a NativeReader: that agent reads the Master
					// directly, so it no-ops the moment ANY agent has the skill
					// — including a sibling row of this very `-a a,b` run, which
					// just installed it from this same source. There is no drift
					// to warn about there, and `note_if_native_reader` below
					// already explains the coverage.
					eprintln!(
						"note: nothing was written — the existing \
						 .agents/skills master was left as-is. To take the \
						 source's current content, delete the skill \
						 (aghub-cli delete skills {} --yes) and remove the \
						 master it reports as kept, then add it again.",
						skill.name
					);
				}

				eprintln_verbose!("Skill '{}' added successfully", skill.name);
				note_if_native_reader(manager);
				// An idempotent re-add is a no-op, and both the human verb
				// ("added" vs "already installed") and a scripted caller need
				// to tell it from a real install.
				let view = aghub_core::dto::SkillView::from(&skill)
					.with_native_reader(manager.skill_target_is_native_reader())
					.with_already_installed(
						added.already_installed && !renamed,
					);
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
				note_if_native_reader(manager);
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
					.with_native_reader(manager.skill_target_is_native_reader())
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
