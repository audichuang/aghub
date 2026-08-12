use crate::define_skill_paths;
use crate::descriptor::*;
use crate::format::json_map;
use crate::{define_mcp_paths, json_map_dialect};

json_map_dialect!(json_map::Dialect {
	toggle_key: json_map::ToggleKey::Disabled,
	..json_map::MCP_SERVERS
});

define_mcp_paths! {
	symmetric: ".factory/mcp.json",
	strategy: parse_mcp_config, serialize_mcp_config,
}

define_skill_paths! {
	symmetric: ".factory/skills",
}

pub const DESCRIPTOR: AgentDescriptor = AgentDescriptor {
	id: "factory",
	display_name: "Factory",
	mcp_parse_config: Some(parse_mcp_config),
	mcp_serialize_config: Some(serialize_mcp_config),
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
			// Factory stores project-toggle overrides at user scope. Aghub's
			// current scope writer cannot express that without mutating the
			// project file, so do not advertise an unsafe toggle operation.
			enable_disable: false,
		},
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
	cli_name: "factory",
	validate_args: &["--version"],
	project_markers: &[".factory"],
	skills_cli_name: Some("factory"),
};
