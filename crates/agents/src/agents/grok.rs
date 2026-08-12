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

fn load_mcps(
	project_root: Option<&Path>,
	scope: crate::ResourceScope,
) -> crate::Result<Vec<crate::McpServer>> {
	load_scoped_mcps(
		project_root,
		scope,
		Some(mcp_global_path),
		Some(mcp_project_path),
		mcp_strategy::parse_toml_grok_mcp_servers,
	)
}

fn save_mcps(
	project_root: Option<&Path>,
	scope: crate::ResourceScope,
	mcps: &[crate::McpServer],
) -> crate::Result<()> {
	save_scoped_mcps(
		project_root,
		scope,
		mcps,
		Some(mcp_global_path),
		Some(mcp_project_path),
		mcp_strategy::serialize_toml_grok_mcp_servers,
	)
}

fn global_skills_paths() -> Vec<PathBuf> {
	grok_home()
		.map(|home| vec![home.join("skills")])
		.unwrap_or_default()
}

fn project_skills_paths(root: &Path) -> Vec<PathBuf> {
	vec![root.join(".grok/skills")]
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
	display_name: "Grok",
	mcp_parse_config: Some(mcp_strategy::parse_toml_grok_mcp_servers),
	mcp_serialize_config: Some(mcp_strategy::serialize_toml_grok_mcp_servers),
	load_mcps,
	save_mcps,
	mcp_global_path: Some(mcp_global_path),
	mcp_project_path: Some(mcp_project_path),
	global_data_dir,
	capabilities: Capabilities {
		skills: SkillCapabilities {
			scopes: ScopeSupport {
				global: true,
				project: true,
			},
			universal: false,
		},
		mcp: McpCapabilities {
			scopes: ScopeSupport {
				global: true,
				project: true,
			},
			stdio: true,
			remote: true,
			enable_disable: true,
		},
		sub_agents: SubAgentCapabilities {
			scopes: ScopeSupport {
				global: true,
				project: true,
			},
		},
	},
	global_skill_paths: Some(GlobalSkillPaths {
		read: global_skills_paths,
		write: global_skill_write_path,
	}),
	project_skill_paths: Some(ProjectSkillPaths {
		read: project_skills_paths,
		write: project_skill_write_path,
	}),
	load_sub_agents,
	save_sub_agents,
	cli_name: "grok",
	validate_args: &["--version"],
	project_markers: &[".grok"],
	// Grok is not an `npx skills` registry target (same as hermes/zed).
	skills_cli_name: None,
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
