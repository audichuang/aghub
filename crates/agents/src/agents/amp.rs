use crate::define_skill_paths;
use crate::descriptor::*;
use crate::format::json_map;
use crate::json_map_dialect;
use std::path::{Path, PathBuf};

// Amp nests its servers under `amp.mcpServers`, tags remotes with `transport`
// and leaves stdio untagged, and toggles with `disabled`.
json_map_dialect!(json_map::Dialect {
	server_key: "amp.mcpServers",
	discriminator: Some(json_map::Discriminator {
		key: "transport",
		stdio: "",
		sse: "sse",
		http: "http",
	}),
	toggle_key: json_map::ToggleKey::Disabled,
	..json_map::MCP_SERVERS
});

const CONFIG_CANDIDATES: &[&str] = &["settings.jsonc", "settings.json"];

fn first_existing_or_default(root: &Path, default_dir: &str) -> PathBuf {
	CONFIG_CANDIDATES
		.iter()
		.map(|name| root.join(name))
		.find(|path| path.is_file())
		.unwrap_or_else(|| root.join(default_dir))
}

fn amp_config_dir() -> Option<PathBuf> {
	home_dir().map(|home| home.join(".config/amp"))
}

fn mcp_global_path() -> Option<PathBuf> {
	amp_config_dir().map(|dir| first_existing_or_default(&dir, "settings.json"))
}

fn mcp_project_path(root: &Path) -> Option<PathBuf> {
	Some(first_existing_or_default(
		&root.join(".amp"),
		"settings.json",
	))
}

fn global_data_dir() -> Option<PathBuf> {
	amp_config_dir()
}

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
	global: ".config/agents/skills",
	project: ".agents/skills",
}

pub const DESCRIPTOR: AgentDescriptor = AgentDescriptor {
	id: "amp",
	display_name: "Amp",
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
			universal: true,
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
	cli_name: "amp",
	validate_args: &["--version"],
	project_markers: &[".amp"],
	skills_cli_name: Some("amp"),
};

#[cfg(test)]
mod tests {
	use super::*;
	use crate::{AgentConfig, McpServer, McpTransport};
	use std::path::Path;

	#[test]
	fn descriptor_mcp_contract_matches_runtime() {
		assert_eq!(
			(DESCRIPTOR.mcp_project_path.unwrap())(Path::new("/workspace")),
			Some(Path::new("/workspace/.amp/settings.json").to_path_buf())
		);
	}

	#[test]
	fn mcp_path_prefers_jsonc_when_present() {
		let temp = tempfile::tempdir().unwrap();
		let amp_dir = temp.path().join(".amp");
		std::fs::create_dir_all(&amp_dir).unwrap();
		std::fs::write(amp_dir.join("settings.json"), "{}").unwrap();
		std::fs::write(amp_dir.join("settings.jsonc"), "{}").unwrap();
		assert_eq!(
			mcp_project_path(temp.path()),
			Some(amp_dir.join("settings.jsonc"))
		);
	}

	#[test]
	fn amp_uses_optional_remote_transport_without_tagging_stdio() {
		let config = AgentConfig {
			mcps: vec![
				McpServer::new("local", McpTransport::stdio("echo", vec![])),
				McpServer::new(
					"remote",
					McpTransport::streamable_http("https://example.com/mcp"),
				),
			],
			skills: vec![],
			sub_agents: vec![],
		};
		let output =
			(DESCRIPTOR.mcp_serialize_config.unwrap())(&config, None).unwrap();
		let value: serde_json::Value = serde_json::from_str(&output).unwrap();
		assert!(value["amp"]["mcpServers"]["local"]
			.get("transport")
			.is_none());
		assert_eq!(value["amp"]["mcpServers"]["remote"]["transport"], "http");
	}
}
