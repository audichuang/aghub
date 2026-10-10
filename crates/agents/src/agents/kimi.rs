use crate::descriptor::*;
use crate::format::{json_map, mcp_policy};
use crate::json_map_dialect;
use std::path::PathBuf;

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

// Prefer the vendor-specific project entry; retain the shared read path for
// discovery and migration of existing installs.
fn global_skill_write_path() -> Option<std::path::PathBuf> {
	home_dir().map(|home| home.join(".config/agents/skills"))
}
fn project_also_reads(root: &std::path::Path) -> Vec<PathBuf> {
	vec![root.join(".agents/skills")]
}
fn project_skill_write_path(
	root: &std::path::Path,
) -> Option<std::path::PathBuf> {
	Some(root.join(".kimi/skills"))
}

pub const DESCRIPTOR: AgentDescriptor = AgentDescriptor {
	id: "kimi",
	agent_type: crate::AgentType::Kimi,
	display_name: "Kimi Code CLI",
	mcp_parse_config: Some(parse_mcp_config),
	mcp_serialize_config: Some(serialize_mcp_config),
	mcp_global_path: Some(mcp_global_path),
	mcp_project_path: None,
	global_data_dir,
	capabilities: Capabilities {
		skills: SkillCapabilities { universal: true },
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
		also_reads: Some(project_also_reads),
	}),
	load_sub_agents: load_sub_agents_noop,
	save_sub_agents: save_sub_agents_noop,
	sub_agent_global_dir: None,
	sub_agent_project_dir: None,
	cli_name: "kimi",
	validate_args: &["--version"],
	project_markers: &[".kimi"],
	skills_cli_name: Some("kimi-cli"),
};

#[cfg(test)]
mod tests {
	use super::*;
	use crate::models::ResourceScope;
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

		assert!(!descriptor.supports_mcp_scope(ResourceScope::ProjectOnly));
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
