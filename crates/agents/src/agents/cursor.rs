use crate::descriptor::*;
use crate::format::json_map;
use crate::{define_mcp_paths, json_map_dialect};
use std::path::{Path, PathBuf};

// The `type` tag stays even where the vendor docs only show it for stdio:
// dropping it makes SSE indistinguishable from streamable HTTP on the next
// read, and v2.13.3 already wrote it — removing it would strand every
// config that release produced.
json_map_dialect!(json_map::Dialect {
	..json_map::MCP_SERVERS
});

define_mcp_paths! {
	symmetric: ".cursor/mcp.json",
}

// npx-`skills` layout: Cursor owns ONLY its own per-agent dir (which holds
// symlink Referrers) plus the universal `.agents/skills` Master. It must NOT
// read another agent's private dir (`.claude/skills`, `.codex/skills`) — that
// makes Cursor discover skills it does not own and plan destructive removals
// against another agent's content. Mapping mirrors upstream `agents.ts`
// (cursor → project `.agents/skills`, global `~/.cursor/skills`); the global
// Master is `~/.agents/skills` per the npx interop contract.
fn global_also_reads() -> Vec<PathBuf> {
	home_dir()
		.map(|home| vec![home.join(".agents/skills")])
		.unwrap_or_default()
}
fn project_also_reads(root: &Path) -> Vec<PathBuf> {
	vec![root.join(".agents/skills")]
}

fn global_skill_write_path() -> Option<PathBuf> {
	home_dir().map(|home| home.join(".cursor/skills"))
}

fn project_skill_write_path(root: &Path) -> Option<PathBuf> {
	Some(root.join(".cursor/skills"))
}

pub const DESCRIPTOR: AgentDescriptor = AgentDescriptor {
	id: "cursor",
	agent_type: crate::AgentType::Cursor,
	display_name: "Cursor",
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
			enable_disable: false,
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
	load_sub_agents: load_sub_agents_noop,
	save_sub_agents: save_sub_agents_noop,
	sub_agent_global_dir: None,
	sub_agent_project_dir: None,
	cli_name: "cursor",
	validate_args: &["--version"],
	project_markers: &[".cursor"],
	skills_cli_name: Some("cursor"),
};
