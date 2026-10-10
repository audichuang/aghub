use crate::define_skill_paths;
use crate::descriptor::*;
use crate::format::json_map;
use crate::{define_mcp_paths, json_map_dialect};

json_map_dialect!(json_map::Dialect {
	toggle_key: json_map::ToggleKey::Disabled("disabled"),
	..json_map::MCP_SERVERS
});

define_mcp_paths! {
	symmetric: ".factory/mcp.json",
}

define_skill_paths! {
	symmetric: ".factory/skills",
}

pub const DESCRIPTOR: AgentDescriptor = AgentDescriptor {
	id: "factory",
	agent_type: crate::AgentType::Factory,
	display_name: "Factory",
	mcp_parse_config: Some(parse_mcp_config),
	mcp_serialize_config: Some(serialize_mcp_config),
	mcp_global_path: Some(mcp_global_path),
	mcp_project_path: Some(mcp_project_path),
	global_data_dir,
	capabilities: Capabilities {
		skills: SkillCapabilities { universal: false },
		mcp: McpCapabilities {
			stdio: true,
			remote: true,
			// Factory stores project-toggle overrides at user scope. Aghub's
			// current scope writer cannot express that without mutating the
			// project file, so do not advertise an unsafe toggle operation.
			enable_disable: false,
		},
	},
	global_skill_paths: Some(GlobalSkillPaths {
		write: global_skill_write_path,
		also_reads: None,
	}),
	project_skill_paths: Some(ProjectSkillPaths {
		write: project_skill_write_path,
		also_reads: None,
	}),
	load_sub_agents: load_sub_agents_noop,
	save_sub_agents: save_sub_agents_noop,
	sub_agent_global_dir: None,
	sub_agent_project_dir: None,
	cli_name: "factory",
	validate_args: &["--version"],
	project_markers: &[".factory"],
	skills_cli_name: Some("factory"),
};
