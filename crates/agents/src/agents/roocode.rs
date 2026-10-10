use crate::define_skill_paths;
use crate::descriptor::*;
use crate::format::{json_map, mcp_policy};
use crate::json_map_dialect;
use std::path::{Path, PathBuf};

// Roo Code spells streamable HTTP in kebab-case and toggles with `disabled`.
json_map_dialect!(json_map::Dialect {
	vocab: mcp_policy::TransportVocabulary {
		http: "streamable-http",
		..json_map::MCP_SERVERS.vocab
	},
	toggle_key: json_map::ToggleKey::Disabled("disabled"),
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

define_skill_paths! {
	symmetric: ".roo/skills",
}

pub const DESCRIPTOR: AgentDescriptor = AgentDescriptor {
	id: "roocode",
	agent_type: crate::AgentType::RooCode,
	display_name: "RooCode",
	mcp_parse_config: Some(parse_mcp_config),
	mcp_serialize_config: Some(serialize_mcp_config),
	mcp_global_path: MCP_GLOBAL_PATH,
	mcp_project_path: MCP_PROJECT_PATH,
	global_data_dir,
	capabilities: Capabilities {
		skills: SkillCapabilities { universal: false },
		mcp: McpCapabilities {
			stdio: true,
			remote: true,
			enable_disable: true,
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
	load_sub_agents: load_sub_agents_noop,
	save_sub_agents: save_sub_agents_noop,
	sub_agent_global_dir: None,
	sub_agent_project_dir: None,
	cli_name: "roocode",
	validate_args: &["--version"],
	project_markers: &[".roo"],
	skills_cli_name: Some("roo"),
};

#[cfg(test)]
mod tests {
	use super::*;
	use crate::models::ResourceScope;
	use std::path::Path;

	const _: () = {
		assert!(!DESCRIPTOR.supports_mcp_scope(ResourceScope::GlobalOnly));
		assert!(DESCRIPTOR.supports_mcp_scope(ResourceScope::ProjectOnly));
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
