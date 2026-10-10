use crate::descriptor::*;
use crate::sub_agents::{load_scoped_sub_agents, save_scoped_sub_agents};
use std::path::{Path, PathBuf};

fn mcp_global_path() -> Option<PathBuf> {
	home_dir().map(|home| home.join(".claude.json"))
}
fn mcp_project_path(root: &Path) -> Option<PathBuf> {
	Some(root.join(".mcp.json"))
}
fn global_data_dir() -> Option<PathBuf> {
	home_dir().map(|home| home.join(".claude"))
}

fn global_skill_write_path() -> Option<PathBuf> {
	home_dir().map(|home| home.join(".claude/skills"))
}

fn project_skill_write_path(root: &Path) -> Option<PathBuf> {
	Some(root.join(".claude/skills"))
}

fn sub_agent_global_dir() -> Option<PathBuf> {
	home_dir().map(|home| home.join(".claude/agents"))
}

fn sub_agent_project_dir(root: &Path) -> Option<PathBuf> {
	Some(root.join(".claude/agents"))
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
	id: "claude",
	agent_type: crate::AgentType::Claude,
	display_name: "Claude Code",
	mcp_parse_config: Some(mcp_strategy::parse_json_map_mcp_servers),
	mcp_serialize_config: Some(mcp_strategy::serialize_json_map_mcp_servers),
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
		also_reads: None,
	}),
	project_skill_paths: Some(ProjectSkillPaths {
		write: project_skill_write_path,
		also_reads: None,
	}),
	load_sub_agents,
	save_sub_agents,
	sub_agent_global_dir: Some(sub_agent_global_dir),
	sub_agent_project_dir: Some(sub_agent_project_dir),
	cli_name: "claude",
	validate_args: &["--version"],
	project_markers: &[".claude", ".mcp.json"],
	skills_cli_name: Some("claude-code"),
};
