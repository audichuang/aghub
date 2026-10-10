use crate::descriptor::*;
use crate::sub_agents::{load_scoped_sub_agents, save_scoped_sub_agents};
use std::path::{Path, PathBuf};

// Symmetric layout (verified against grok 0.2.99):
//   global  ~/.grok/config.toml
//   project <root>/.grok/config.toml
//   skills  ~/.grok/skills / <root>/.grok/skills
//   agents  ~/.grok/agents / <root>/.grok/agents
// global_data_dir is the parent of the config file → ~/.grok
fn resolve_grok_home(
	override_dir: Option<std::ffi::OsString>,
	home: Option<PathBuf>,
) -> Option<PathBuf> {
	override_dir
		.filter(|value| !value.is_empty())
		.map(PathBuf::from)
		.or_else(|| home.map(|home| home.join(".grok")))
}

fn grok_home() -> Option<PathBuf> {
	resolve_grok_home(std::env::var_os("GROK_HOME"), home_dir())
}

fn mcp_global_path() -> Option<PathBuf> {
	grok_home().map(|home| home.join("config.toml"))
}

fn mcp_project_path(root: &Path) -> Option<PathBuf> {
	Some(root.join(".grok/config.toml"))
}

fn global_data_dir() -> Option<PathBuf> {
	grok_home()
}

// Grok's own docs (`~/.grok/docs/user-guide/08-skills.md`) say it scans
// `.agents/skills/` "at each tier (alongside `.grok/`)" — so the universal
// Master slot is read at BOTH global and project scope, not just project.
// Its `[compat.claude]`/`[compat.cursor]` scanning of `~/.claude/skills` and
// `~/.cursor/skills` is deliberately NOT modelled: decision #11 in
// `docs/specs/2026-08-30-skills-hub-borrow-path.md` — an agent never reads
// another agent's private dir. Own dir stays FIRST (first-dir-wins decides
// `source_path`, i.e. what `remove_skill_planned` deletes).
fn global_also_reads() -> Vec<PathBuf> {
	home_dir()
		.map(|home| vec![home.join(".agents/skills")])
		.unwrap_or_default()
}

fn project_also_reads(root: &Path) -> Vec<PathBuf> {
	vec![root.join(".agents/skills")]
}

fn global_skill_write_path() -> Option<PathBuf> {
	grok_home().map(|home| home.join("skills"))
}

fn project_skill_write_path(root: &Path) -> Option<PathBuf> {
	Some(root.join(".grok/skills"))
}

fn sub_agent_global_dir() -> Option<PathBuf> {
	grok_home().map(|home| home.join("agents"))
}

fn sub_agent_project_dir(root: &Path) -> Option<PathBuf> {
	Some(root.join(".grok/agents"))
}

fn load_sub_agents(
	project_root: Option<&Path>,
	scope: crate::ResourceScope,
) -> crate::Result<Vec<crate::SubAgent>> {
	load_scoped_sub_agents(
		project_root,
		scope,
		Some(sub_agent_global_dir),
		Some(sub_agent_project_dir),
	)
}

fn save_sub_agents(
	project_root: Option<&Path>,
	scope: crate::ResourceScope,
	agents: &[crate::SubAgent],
) -> crate::Result<()> {
	save_scoped_sub_agents(
		project_root,
		scope,
		agents,
		Some(sub_agent_global_dir),
		Some(sub_agent_project_dir),
	)
}

pub const DESCRIPTOR: AgentDescriptor = AgentDescriptor {
	id: "grok",
	agent_type: crate::AgentType::Grok,
	display_name: "Grok",
	mcp_parse_config: Some(mcp_strategy::parse_toml_grok_mcp_servers),
	mcp_serialize_config: Some(mcp_strategy::serialize_toml_grok_mcp_servers),
	mcp_global_path: Some(mcp_global_path),
	mcp_project_path: Some(mcp_project_path),
	global_data_dir,
	capabilities: Capabilities {
		skills: SkillCapabilities { universal: false },
		mcp: McpCapabilities {
			stdio: true,
			remote: true,
			enable_disable: true,
		},
	},
	global_skill_paths: Some(GlobalSkillPaths {
		write: global_skill_write_path,
		also_reads: Some(global_also_reads),
	}),
	project_skill_paths: Some(ProjectSkillPaths {
		write: project_skill_write_path,
		also_reads: Some(project_also_reads),
	}),
	load_sub_agents,
	save_sub_agents,
	sub_agent_global_dir: Some(sub_agent_global_dir),
	sub_agent_project_dir: Some(sub_agent_project_dir),
	cli_name: "grok",
	validate_args: &["--version"],
	project_markers: &[".grok"],
	// `vercel-labs/skills` registers grok as `grok`
	// (`skillsDir: '.grok/skills'`, `globalSkillsDir: join(grokHome, 'skills')`).
	// Inert metadata today: nothing in the workspace builds a command from
	// `skills_cli_name` — only the descriptor regression table reads it.
	skills_cli_name: Some("grok"),
};

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn grok_home_override_wins() {
		assert_eq!(
			resolve_grok_home(
				Some("/custom/grok".into()),
				Some(PathBuf::from("/home/user")),
			),
			Some(PathBuf::from("/custom/grok"))
		);
	}
}
