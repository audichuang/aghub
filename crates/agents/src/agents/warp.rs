use crate::define_mcp_paths;
use crate::descriptor::*;

define_mcp_paths! {
	symmetric: ".warp/.mcp.json",
	strategy: mcp_strategy::parse_json_map_mcp_servers,
			  mcp_strategy::serialize_json_map_mcp_servers,
}

// Prefer the vendor-specific project entry; retain the shared read path for
// discovery and migration of existing installs.
fn global_skills_paths() -> Vec<std::path::PathBuf> {
	home_dir()
		.map(|home| vec![home.join(".agents/skills")])
		.unwrap_or_default()
}
fn global_skill_write_path() -> Option<std::path::PathBuf> {
	home_dir().map(|home| home.join(".agents/skills"))
}
fn project_skills_paths(root: &std::path::Path) -> Vec<std::path::PathBuf> {
	vec![root.join(".warp/skills"), root.join(".agents/skills")]
}
fn project_skill_write_path(
	root: &std::path::Path,
) -> Option<std::path::PathBuf> {
	Some(root.join(".warp/skills"))
}

pub const DESCRIPTOR: AgentDescriptor = AgentDescriptor {
	id: "warp",
	display_name: "Warp",
	mcp_parse_config: Some(mcp_strategy::parse_json_map_mcp_servers),
	mcp_serialize_config: Some(mcp_strategy::serialize_json_map_mcp_servers),
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
	cli_name: "warp",
	validate_args: &["--version"],
	project_markers: &[".warp"],
	skills_cli_name: Some("warp"),
};

#[cfg(test)]
mod tests {
	use super::*;
	use std::path::Path;

	#[test]
	fn descriptor_mcp_contract_matches_runtime() {
		assert_eq!(
			(DESCRIPTOR.mcp_project_path.unwrap())(Path::new("/workspace")),
			Some(Path::new("/workspace/.warp/.mcp.json").to_path_buf())
		);
	}
}
