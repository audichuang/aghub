use crate::define_skill_paths;
use crate::descriptor::*;
use crate::format::json_map;
use crate::json_map_dialect;
use std::path::{Path, PathBuf};

// Roo Code spells streamable HTTP in kebab-case and toggles with `disabled`.
json_map_dialect!(json_map::Dialect {
	discriminator: Some(json_map::Discriminator {
		key: "type",
		stdio: "stdio",
		sse: "sse",
		http: "streamable-http",
	}),
	toggle_key: json_map::ToggleKey::Disabled,
	..json_map::MCP_SERVERS
});

fn mcp_project_path(root: &Path) -> Option<PathBuf> {
	Some(root.join(".roo/mcp.json"))
}

fn global_data_dir() -> Option<PathBuf> {
	home_dir().map(|home| home.join(".roo"))
}

const MCP_GLOBAL_PATH: Option<OptionalPathFn> = None;
const MCP_PROJECT_PATH: Option<OptionalProjectPathFn> = Some(mcp_project_path);

fn load_mcps(
	project_root: Option<&Path>,
	scope: crate::ResourceScope,
) -> crate::Result<Vec<crate::McpServer>> {
	load_scoped_mcps(
		project_root,
		scope,
		MCP_GLOBAL_PATH,
		MCP_PROJECT_PATH,
		parse_mcp_config,
	)
}

fn save_mcps(
	project_root: Option<&Path>,
	scope: crate::ResourceScope,
	mcps: &[crate::McpServer],
) -> crate::Result<()> {
	save_scoped_mcps(
		project_root,
		scope,
		mcps,
		MCP_GLOBAL_PATH,
		MCP_PROJECT_PATH,
		serialize_mcp_config,
	)
}

define_skill_paths! {
	symmetric: ".roo/skills",
}

pub const DESCRIPTOR: AgentDescriptor = AgentDescriptor {
	id: "roocode",
	display_name: "RooCode",
	mcp_parse_config: Some(parse_mcp_config),
	mcp_serialize_config: Some(serialize_mcp_config),
	load_mcps,
	save_mcps,
	mcp_global_path: MCP_GLOBAL_PATH,
	mcp_project_path: MCP_PROJECT_PATH,
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
	cli_name: "roocode",
	validate_args: &["--version"],
	project_markers: &[".roo"],
	skills_cli_name: Some("roo"),
};

#[cfg(test)]
mod tests {
	use super::*;
	use std::path::Path;

	const _: () = {
		assert!(!DESCRIPTOR.capabilities.mcp.scopes.global);
		assert!(DESCRIPTOR.capabilities.mcp.scopes.project);
	};

	#[test]
	fn descriptor_mcp_contract_matches_runtime() {
		assert!(DESCRIPTOR.mcp_global_path.is_none());
		assert_eq!(
			(DESCRIPTOR.mcp_project_path.unwrap())(Path::new("/workspace")),
			Some(Path::new("/workspace/.roo/mcp.json").to_path_buf())
		);
	}
}
