use crate::descriptor::*;
use crate::format::json_map;
use crate::json_map_dialect;
use std::path::{Path, PathBuf};

// The `type` tag stays even where the vendor docs only show it for stdio:
// dropping it makes SSE indistinguishable from streamable HTTP on the next
// read, and v2.13.3 already wrote it — removing it would strand every
// config that release produced.
json_map_dialect!(json_map::Dialect {
	..json_map::MCP_SERVERS
});

// Trae configures MCP through its GUI (Settings > MCP); there is no documented
// hand-editable GLOBAL file — the global store is the IDE's opaque app data.
// Only the project-level `.trae/` directory (mcp.json, skills, rules) is real.
// See https://docs.trae.ai and https://github.com/trae-community/trae-mcp.
fn mcp_project_path(root: &Path) -> Option<PathBuf> {
	Some(root.join(".trae/mcp.json"))
}
fn global_data_dir() -> Option<PathBuf> {
	// Trae is a VS Code fork: its app data lives in the OS config dir —
	// ~/Library/Application Support/Trae (macOS), ~/.config/Trae (Linux),
	// %APPDATA%\Trae (Windows). Used for availability/reveal, not for writing.
	dirs::config_dir().map(|dir| dir.join("Trae"))
}
fn project_skill_write_path(root: &Path) -> Option<PathBuf> {
	Some(root.join(".trae/skills"))
}

pub const DESCRIPTOR: AgentDescriptor = AgentDescriptor {
	id: "trae",
	agent_type: crate::AgentType::Trae,
	display_name: "Trae",
	mcp_parse_config: Some(parse_mcp_config),
	mcp_serialize_config: Some(serialize_mcp_config),
	mcp_global_path: None,
	mcp_project_path: Some(mcp_project_path),
	global_data_dir,
	capabilities: Capabilities {
		skills: SkillCapabilities { universal: false },
		mcp: McpCapabilities {
			stdio: true,
			remote: true,
			enable_disable: false,
		},
	},
	global_skill_paths: None,
	project_skill_paths: Some(ProjectSkillPaths {
		write: project_skill_write_path,
		also_reads: None,
	}),
	load_sub_agents: load_sub_agents_noop,
	save_sub_agents: save_sub_agents_noop,
	sub_agent_global_dir: None,
	sub_agent_project_dir: None,
	cli_name: "trae",
	validate_args: &["--version"],
	project_markers: &[".trae"],
	skills_cli_name: Some("trae"),
};
