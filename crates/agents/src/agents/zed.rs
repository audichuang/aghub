use crate::descriptor::*;
use crate::format::json_map;
use crate::json_map_dialect;

// Zed's documented `context_servers` entry has no persisted per-server
// toggle, so aghub does not invent one.
// The `type` tag stays even where the vendor docs only show it for stdio:
// dropping it makes SSE indistinguishable from streamable HTTP on the next
// read, and v2.13.3 already wrote it — removing it would strand every
// config that release produced.
json_map_dialect!(json_map::Dialect {
	server_key: "context_servers",
	discriminator: Some(json_map::Discriminator {
		key: "type",
		stdio: "stdio",
		sse: "sse",
		http: "http",
	}),
	..json_map::MCP_SERVERS
});

fn zed_config_dir() -> Option<std::path::PathBuf> {
	#[cfg(target_os = "macos")]
	{
		home_dir().map(|home| home.join(".config/zed"))
	}
	#[cfg(target_os = "windows")]
	{
		dirs::config_dir().map(|dir| dir.join("Zed"))
	}
	#[cfg(not(any(target_os = "macos", target_os = "windows")))]
	{
		dirs::config_dir().map(|dir| dir.join("zed"))
	}
}

fn mcp_global_path() -> Option<std::path::PathBuf> {
	zed_config_dir().map(|dir| dir.join("settings.json"))
}

fn mcp_project_path(root: &std::path::Path) -> Option<std::path::PathBuf> {
	Some(root.join(".zed/settings.json"))
}

fn global_data_dir() -> Option<std::path::PathBuf> {
	zed_config_dir()
}

fn load_mcps(
	project_root: Option<&std::path::Path>,
	scope: crate::ResourceScope,
) -> crate::Result<Vec<crate::McpServer>> {
	load_scoped_mcps(
		project_root,
		scope,
		Some(mcp_global_path),
		Some(mcp_project_path),
		parse_mcp_config,
	)
}

fn save_mcps(
	project_root: Option<&std::path::Path>,
	scope: crate::ResourceScope,
	mcps: &[crate::McpServer],
) -> crate::Result<()> {
	save_scoped_mcps(
		project_root,
		scope,
		mcps,
		Some(mcp_global_path),
		Some(mcp_project_path),
		serialize_mcp_config,
	)
}

pub const DESCRIPTOR: AgentDescriptor = AgentDescriptor {
	id: "zed",
	display_name: "Zed",
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
				global: false,
				project: false,
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
	global_skill_paths: None,
	project_skill_paths: None,
	load_sub_agents: load_sub_agents_noop,
	save_sub_agents: save_sub_agents_noop,
	cli_name: "zed",
	validate_args: &["--version"],
	project_markers: &[".zed"],
	skills_cli_name: None,
};
