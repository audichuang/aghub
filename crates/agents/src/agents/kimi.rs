use crate::define_skill_paths;
use crate::descriptor::*;
use crate::format::{json_map, mcp_policy};
use crate::json_map_dialect;
use std::path::{Path, PathBuf};

fn resolve_share_dir(
	kimi_share_dir: Option<PathBuf>,
	home: Option<PathBuf>,
) -> Option<PathBuf> {
	kimi_share_dir.or_else(|| home.map(|home| home.join(".kimi")))
}

fn kimi_share_dir() -> Option<PathBuf> {
	resolve_share_dir(
		std::env::var_os("KIMI_SHARE_DIR")
			.filter(|value| !value.is_empty())
			.map(PathBuf::from),
		home_dir(),
	)
}

fn mcp_global_path() -> Option<PathBuf> {
	kimi_share_dir().map(|share_dir| share_dir.join("mcp.json"))
}

fn global_data_dir() -> Option<PathBuf> {
	kimi_share_dir()
}

// Kimi CLI 1.49.0 writes `transport: "http"`; the shared parser also accepts
// the compatible `streamable-http` spelling found in existing configs.
json_map_dialect!(json_map::Dialect {
	vocab: mcp_policy::TransportVocabulary {
		tag_key: "transport",
		..json_map::MCP_SERVERS.vocab
	},
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
		None,
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
		None,
		serialize_mcp_config,
	)
}

define_skill_paths! {
	global: ".config/agents/skills",
	project: ".agents/skills",
}

pub const DESCRIPTOR: AgentDescriptor = AgentDescriptor {
	id: "kimi",
	display_name: "Kimi Code CLI",
	mcp_parse_config: Some(parse_mcp_config),
	mcp_serialize_config: Some(serialize_mcp_config),
	load_mcps,
	save_mcps,
	mcp_global_path: Some(mcp_global_path),
	mcp_project_path: None,
	global_data_dir,
	capabilities: Capabilities {
		skills: SkillCapabilities {
			scopes: ScopeSupport {
				global: true,
				project: true,
			},
			universal: true,
		},
		mcp: McpCapabilities {
			scopes: ScopeSupport {
				global: true,
				project: false,
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
	cli_name: "kimi",
	validate_args: &["--version"],
	project_markers: &[".kimi"],
	skills_cli_name: Some("kimi-cli"),
};

#[cfg(test)]
mod tests {
	use super::*;
	use crate::{AgentConfig, McpServer, McpTransport};
	use std::path::{Path, PathBuf};

	#[test]
	fn mcp_path_honors_kimi_share_dir() {
		assert_eq!(
			resolve_share_dir(
				Some(PathBuf::from("/custom/kimi")),
				Some(PathBuf::from("/home/user")),
			),
			Some(PathBuf::from("/custom/kimi"))
		);
		assert_eq!(
			resolve_share_dir(None, Some(PathBuf::from("/home/user"))),
			Some(PathBuf::from("/home/user/.kimi"))
		);
		// The production path is the share dir + Kimi's config filename.
		assert_eq!(
			mcp_global_path(),
			kimi_share_dir().map(|dir| dir.join("mcp.json"))
		);
	}

	#[test]
	fn descriptor_is_global_only_and_writes_native_http() {
		let config = AgentConfig {
			mcps: vec![McpServer::new(
				"remote",
				McpTransport::streamable_http("https://example.com/mcp"),
			)],
			skills: vec![],
			sub_agents: vec![],
		};
		let output =
			(DESCRIPTOR.mcp_serialize_config.unwrap())(&config, None).unwrap();
		let value: serde_json::Value = serde_json::from_str(&output).unwrap();
		let descriptor = &DESCRIPTOR;

		assert!(!descriptor.capabilities.mcp.scopes.project);
		assert!(!descriptor.capabilities.mcp.enable_disable);
		assert!(descriptor.mcp_project_path.is_none());
		assert_eq!(value["mcpServers"]["remote"]["transport"], "http");
		assert!(value["mcpServers"]["remote"].get("type").is_none());
		let reparsed = (descriptor.mcp_parse_config.unwrap())(&output).unwrap();
		assert!(matches!(
			reparsed.mcps[0].transport,
			McpTransport::StreamableHttp { .. }
		));
		assert_eq!(
			descriptor.mcp_path(
				Some(Path::new("/workspace")),
				crate::ResourceScope::ProjectOnly,
			),
			None
		);
	}
}
