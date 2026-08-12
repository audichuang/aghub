use crate::define_skill_paths;
use crate::descriptor::*;
use crate::format::json_map;
use crate::{define_mcp_paths, json_map_dialect};

// Gemini spells streamable HTTP as `httpUrl` (handled by the shared parser); a
// bare `url` is auto-detected by Gemini itself, so aghub keeps the default
// path-based inference rather than forcing `type: "http"` onto an entry the
// user left open — that would pin a `/sse` endpoint to the wrong transport.
json_map_dialect!(json_map::Dialect {
	..json_map::MCP_SERVERS
});

define_mcp_paths! {
	symmetric: ".gemini/settings.json",
	strategy: parse_mcp_config, serialize_mcp_config,
}

define_skill_paths! {
	global: ".gemini/skills",
	project: ".agents/skills",
}

pub const DESCRIPTOR: AgentDescriptor = AgentDescriptor {
	id: "gemini",
	display_name: "Gemini CLI",
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
	cli_name: "gemini",
	validate_args: &["--version"],
	project_markers: &[".gemini"],
	skills_cli_name: Some("gemini-cli"),
};
