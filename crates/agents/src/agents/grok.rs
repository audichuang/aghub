use crate::define_mcp_paths;
use crate::define_skill_paths;
use crate::descriptor::*;
use crate::sub_agents::{load_scoped_sub_agents, save_scoped_sub_agents};
use std::path::{Path, PathBuf};

// Symmetric layout (verified against grok 0.2.99):
//   global  ~/.grok/config.toml
//   project <root>/.grok/config.toml
//   skills  ~/.grok/skills / <root>/.grok/skills
//   agents  ~/.grok/agents / <root>/.grok/agents
// global_data_dir is the parent of the config file → ~/.grok
define_mcp_paths! {
	symmetric: ".grok/config.toml",
	strategy: mcp_strategy::parse_toml_grok_mcp_servers,
			  mcp_strategy::serialize_toml_grok_mcp_servers,
}

define_skill_paths! {
	symmetric: ".grok/skills",
}

fn sub_agent_global_dir() -> Option<PathBuf> {
	home_dir().map(|home| home.join(".grok/agents"))
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
