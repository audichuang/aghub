use crate::define_skill_paths;
use crate::descriptor::*;
use crate::format::{json_map, mcp_policy};
use crate::json_map_dialect;
use std::path::{Path, PathBuf};

// Amp nests its servers under `amp.mcpServers`, tags remotes with `transport`
// and leaves stdio untagged, and toggles with `disabled`.
json_map_dialect!(json_map::Dialect {
	server_key: "amp.mcpServers",
	vocab: mcp_policy::TransportVocabulary {
		tag_key: "transport",
		stdio: "",
		..json_map::MCP_SERVERS.vocab
	},
	toggle_key: json_map::ToggleKey::Disabled("disabled"),
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

define_skill_paths! {
	global: ".config/agents/skills",
	project: ".agents/skills",
}

pub const DESCRIPTOR: AgentDescriptor = AgentDescriptor {
	id: "amp",
	agent_type: crate::AgentType::Amp,
	display_name: "Amp",
	mcp_parse_config: Some(parse_mcp_config),
	mcp_serialize_config: Some(serialize_mcp_config),
	mcp_global_path: Some(mcp_global_path),
	mcp_project_path: Some(mcp_project_path),
	global_data_dir,
	capabilities: Capabilities {
		skills: SkillCapabilities { universal: true },
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
