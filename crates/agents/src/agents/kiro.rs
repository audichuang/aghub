use crate::define_skill_paths;
use crate::descriptor::*;
use crate::format::json_map;
use crate::{define_mcp_paths, json_map_dialect};

// Kiro documents `disabled` and a bare `url` for remote servers; it has no
// transport tag, so SSE cannot be expressed.
json_map_dialect!(json_map::Dialect {
	discriminator: None,
	toggle_key: json_map::ToggleKey::Disabled,
	untyped_remote: json_map::UntypedRemote::StreamableHttp,
	..json_map::MCP_SERVERS
});

define_mcp_paths! {
	global: ".kiro/settings/mcp.json",
	project: ".kiro/settings/mcp.json",
	data_dir: ".kiro",
	strategy: parse_mcp_config, serialize_mcp_config,
}

define_skill_paths! {
	symmetric: ".kiro/skills",
}

pub const DESCRIPTOR: AgentDescriptor = AgentDescriptor {
	id: "kiro",
	display_name: "Kiro",
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
			enable_disable: true,
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
	cli_name: "kiro",
	validate_args: &["--version"],
	project_markers: &[".kiro"],
	skills_cli_name: Some("kiro-cli"),
};
