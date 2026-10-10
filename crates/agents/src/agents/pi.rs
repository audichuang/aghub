use crate::descriptor::*;
use std::path::{Path, PathBuf};

fn global_data_dir() -> Option<PathBuf> {
	home_dir().map(|home| home.join(".pi/agent"))
}
// Pi's own skills doc lists the universal Master slot alongside its private dir
// at BOTH scopes: global `~/.pi/agent/skills` + `~/.agents/skills`, project
// `.pi/skills` + `.agents/skills`. Own dir FIRST (first-dir-wins decides
// `source_path`). Pi's configurable compat scanning of `~/.claude/skills` /
// `~/.codex/skills` is deliberately not modelled — decision #11.
fn global_also_reads() -> Vec<PathBuf> {
	home_dir()
		.map(|home| vec![home.join(".agents/skills")])
		.unwrap_or_default()
}
fn project_also_reads(root: &Path) -> Vec<PathBuf> {
	vec![root.join(".agents/skills")]
}

fn global_skill_write_path() -> Option<PathBuf> {
	home_dir().map(|home| home.join(".pi/agent/skills"))
}

fn project_skill_write_path(root: &Path) -> Option<PathBuf> {
	Some(root.join(".pi/skills"))
}

pub const DESCRIPTOR: AgentDescriptor = AgentDescriptor {
	id: "pi",
	agent_type: crate::AgentType::Pi,
	display_name: "Pi Coding Agent",
	mcp_parse_config: None,
	mcp_serialize_config: None,
	mcp_global_path: None,
	mcp_project_path: None,
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
				global: false,
				project: false,
			},
			stdio: false,
			remote: false,
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
	cli_name: "pi",
	validate_args: &["--version"],
	project_markers: &[".pi"],
	skills_cli_name: Some("pi"),
};
