use crate::define_skill_paths;
use crate::descriptor::*;
use crate::format::json_map;
use crate::json_map_dialect;
use std::path::{Path, PathBuf};

fn resolve_copilot_home(
	copilot_home: Option<PathBuf>,
	home: Option<PathBuf>,
) -> Option<PathBuf> {
	copilot_home.or_else(|| home.map(|home| home.join(".copilot")))
}

fn resolve_project_mcp_path(
	root: &Path,
	exists: impl Fn(&Path) -> bool,
) -> PathBuf {
	let primary = root.join(".mcp.json");
	let github = root.join(".github/mcp.json");
	if exists(&primary) {
		primary
	} else if exists(&github) {
		github
	} else {
		primary
	}
}

fn copilot_home() -> Option<PathBuf> {
	resolve_copilot_home(
		std::env::var_os("COPILOT_HOME")
			.filter(|value| !value.is_empty())
			.map(PathBuf::from),
		home_dir(),
	)
}

fn mcp_global_path() -> Option<PathBuf> {
	copilot_home().map(|home| home.join("mcp-config.json"))
}

fn mcp_project_path(root: &Path) -> Option<PathBuf> {
	Some(resolve_project_mcp_path(root, Path::exists))
}

fn global_data_dir() -> Option<PathBuf> {
	copilot_home()
}

// The Copilot CLI dialect spells all three transports with `type`, but exposes
// no persisted per-server toggle in the documented file contract.
json_map_dialect!(json_map::Dialect {
	untyped_remote: json_map::UntypedRemote::StreamableHttp,
	..json_map::MCP_SERVERS
});

fn load_mcps(
	project_root: Option<&Path>,
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
	project_root: Option<&Path>,
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

define_skill_paths! {
	global: ".copilot/skills",
	project: ".agents/skills",
}

pub const DESCRIPTOR: AgentDescriptor = AgentDescriptor {
	id: "copilot",
	display_name: "GitHub Copilot",
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
	cli_name: "copilot",
	validate_args: &["--version"],
	project_markers: &[".mcp.json", ".github"],
	skills_cli_name: Some("github-copilot"),
};

#[cfg(test)]
mod tests {
	use super::*;
	use crate::{AgentConfig, McpServer, McpTransport};
	use std::path::{Path, PathBuf};

	#[test]
	fn mcp_paths_follow_copilot_cli() {
		assert_eq!(
			resolve_copilot_home(
				Some(PathBuf::from("/custom/copilot")),
				Some(PathBuf::from("/home/user")),
			),
			Some(PathBuf::from("/custom/copilot"))
		);
		assert_eq!(
			resolve_copilot_home(None, Some(PathBuf::from("/home/user"))),
			Some(PathBuf::from("/home/user/.copilot"))
		);
		// The production path is home + the CLI's config filename.
		assert_eq!(
			mcp_global_path(),
			copilot_home().map(|home| home.join("mcp-config.json"))
		);

		let root = Path::new("/workspace");
		assert_eq!(
			resolve_project_mcp_path(root, |_| true),
			root.join(".mcp.json")
		);
		assert_eq!(
			resolve_project_mcp_path(root, |path| {
				path == root.join(".github/mcp.json")
			}),
			root.join(".github/mcp.json")
		);
		assert_eq!(
			resolve_project_mcp_path(root, |_| false),
			root.join(".mcp.json")
		);
	}

	#[test]
	fn descriptor_uses_copilot_cli_and_native_json() {
		let config = AgentConfig {
			mcps: vec![McpServer::new(
				"remote",
				McpTransport::streamable_http("https://example.com/mcp"),
			)],
			skills: vec![],
			sub_agents: vec![],
		};
		let descriptor = &DESCRIPTOR;
		let output =
			(descriptor.mcp_serialize_config.unwrap())(&config, None).unwrap();
		let value: serde_json::Value = serde_json::from_str(&output).unwrap();

		assert_eq!(descriptor.cli_name, "copilot");
		assert!(!descriptor.capabilities.mcp.enable_disable);
		assert_eq!(descriptor.project_markers, &[".mcp.json", ".github"]);
		assert_eq!(value["mcpServers"]["remote"]["type"], "http");
		assert!(value.get("servers").is_none());
		let reparsed = (descriptor.mcp_parse_config.unwrap())(&output).unwrap();
		assert!(matches!(
			reparsed.mcps[0].transport,
			McpTransport::StreamableHttp { .. }
		));
	}
}
