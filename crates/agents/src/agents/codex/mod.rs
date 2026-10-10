mod mcp;
mod sub_agent;
pub use sub_agent::has_unmanaged_fields as sub_agent_has_unmanaged_fields;

use crate::descriptor::*;
use std::path::{Path, PathBuf};

fn global_data_dir() -> Option<PathBuf> {
	mcp::global_dir()
}

fn global_also_reads() -> Vec<PathBuf> {
	let Some(home) = home_dir() else {
		return Vec::new();
	};
	let paths = vec![home.join(".agents/skills")];
	#[cfg(not(target_os = "windows"))]
	let paths = {
		let mut p = paths;
		p.push(PathBuf::from("/etc/codex/skills"));
		p
	};
	paths
}

fn project_also_reads(root: &Path) -> Vec<PathBuf> {
	vec![root.join(".agents/skills")]
}

fn global_skill_write_path() -> Option<PathBuf> {
	mcp::global_dir().map(|root| root.join("skills"))
}

fn project_skill_write_path(root: &Path) -> Option<PathBuf> {
	Some(root.join(".codex/skills"))
}

pub const DESCRIPTOR: AgentDescriptor = AgentDescriptor {
	id: "codex",
	agent_type: crate::AgentType::Codex,
	display_name: "OpenAI Codex",
	mcp_parse_config: Some(mcp_strategy::PARSE_TOML),
	mcp_serialize_config: Some(mcp_strategy::SERIALIZE_TOML),
	mcp_global_path: Some(mcp::global_path),
	mcp_project_path: Some(mcp::project_path),
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
				global: true,
				project: true,
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
	load_sub_agents: sub_agent::load,
	save_sub_agents: sub_agent::save,
	sub_agent_global_dir: Some(sub_agent::global_dir),
	sub_agent_project_dir: Some(sub_agent::project_dir),
	cli_name: "codex",
	validate_args: &["--version"],
	project_markers: &[".codex"],
	skills_cli_name: Some("codex"),
};
