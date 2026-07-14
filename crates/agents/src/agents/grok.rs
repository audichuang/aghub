use crate::define_mcp_paths;
use crate::define_skill_paths;
use crate::descriptor::*;

// Symmetric layout (verified against grok 0.2.99):
//   global  ~/.grok/config.toml
//   project <root>/.grok/config.toml
//   skills  ~/.grok/skills / <root>/.grok/skills
// global_data_dir is the parent of the config file → ~/.grok
define_mcp_paths! {
	symmetric: ".grok/config.toml",
	strategy: mcp_strategy::parse_toml_grok_mcp_servers,
			  mcp_strategy::serialize_toml_grok_mcp_servers,
}

define_skill_paths! {
	symmetric: ".grok/skills",
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
		// Sub-agents are a follow-up; leave off for this scope.
		sub_agents: SubAgentCapabilities {
			scopes: ScopeSupport {
				global: false,
				project: false,
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
	load_sub_agents: load_sub_agents_noop,
	save_sub_agents: save_sub_agents_noop,
	cli_name: "grok",
	validate_args: &["--version"],
	project_markers: &[".grok"],
	// Grok is not an `npx skills` registry target (same as hermes/zed).
	skills_cli_name: None,
};
