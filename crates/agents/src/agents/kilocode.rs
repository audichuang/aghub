use crate::define_skill_paths;
use crate::descriptor::*;
use crate::format::json_opencode;
use std::path::{Path, PathBuf};

const GLOBAL_CONFIG_CANDIDATES: &[&str] = &["kilo.jsonc", "kilo.json"];
const PROJECT_CONFIG_CANDIDATES: &[&str] = &[
	".kilo/kilo.jsonc",
	".kilo/kilo.json",
	"kilo.jsonc",
	"kilo.json",
];

fn existing_or_default(
	root: &Path,
	candidates: &[&str],
	default: &str,
) -> PathBuf {
	candidates
		.iter()
		.map(|candidate| root.join(candidate))
		.find(|candidate| candidate.is_file())
		.unwrap_or_else(|| root.join(default))
}

fn config_dir_from(
	xdg_config_home: Option<std::ffi::OsString>,
	home: Option<PathBuf>,
) -> Option<PathBuf> {
	xdg_config_home
		.filter(|value| !value.is_empty())
		.map(PathBuf::from)
		.or_else(|| home.map(|home| home.join(".config")))
		.map(|config| config.join("kilo"))
}

fn config_dir() -> Option<PathBuf> {
	config_dir_from(std::env::var_os("XDG_CONFIG_HOME"), home_dir())
}

fn global_config_path_from(config: Option<PathBuf>) -> Option<PathBuf> {
	config.map(|config| {
		existing_or_default(&config, GLOBAL_CONFIG_CANDIDATES, "kilo.json")
	})
}

fn mcp_global_path() -> Option<PathBuf> {
	global_config_path_from(config_dir())
}

fn mcp_project_path(root: &Path) -> Option<PathBuf> {
	Some(existing_or_default(
		root,
		PROJECT_CONFIG_CANDIDATES,
		"kilo.json",
	))
}

fn global_data_dir() -> Option<PathBuf> {
	config_dir()
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
		json_opencode::parse,
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
		json_opencode::serialize,
	)
}

define_skill_paths! {
	symmetric: ".kilocode/skills",
}

pub const DESCRIPTOR: AgentDescriptor = AgentDescriptor {
	id: "kilocode",
	display_name: "KiloCode",
	mcp_parse_config: Some(json_opencode::parse),
	mcp_serialize_config: Some(json_opencode::serialize),
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
	cli_name: "kilocode",
	validate_args: &["--version"],
	project_markers: &["kilo.json", "kilo.jsonc", ".kilo", ".kilocode"],
	skills_cli_name: Some("kilo"),
};

#[cfg(test)]
mod tests {
	use super::*;
	use std::fs;

	#[test]
	fn mcp_paths_use_canonical_kilo_precedence() {
		let project = tempfile::tempdir().unwrap();
		let root = project.path();

		assert_eq!(
			config_dir_from(
				Some("/xdg".into()),
				Some(Path::new("/home/person").to_path_buf())
			),
			Some(Path::new("/xdg/kilo").to_path_buf())
		);
		assert_eq!(
			config_dir_from(
				None,
				Some(Path::new("/home/person").to_path_buf())
			),
			Some(Path::new("/home/person/.config/kilo").to_path_buf())
		);
		assert_eq!(
			global_config_path_from(Some(root.to_path_buf())),
			Some(root.join("kilo.json"))
		);
		assert_eq!(mcp_project_path(root), Some(root.join("kilo.json")));

		fs::write(root.join("kilo.json"), "{}").unwrap();
		fs::write(root.join("kilo.jsonc"), "{}").unwrap();
		assert_eq!(
			global_config_path_from(Some(root.to_path_buf())),
			Some(root.join("kilo.jsonc"))
		);
		assert_eq!(mcp_project_path(root), Some(root.join("kilo.jsonc")));

		fs::create_dir(root.join(".kilo")).unwrap();
		fs::write(root.join(".kilo/kilo.json"), "{}").unwrap();
		assert_eq!(mcp_project_path(root), Some(root.join(".kilo/kilo.json")));

		fs::write(root.join(".kilo/kilo.jsonc"), "{}").unwrap();
		assert_eq!(mcp_project_path(root), Some(root.join(".kilo/kilo.jsonc")));
	}

	#[test]
	fn descriptor_uses_canonical_kilo_mcp_format() {
		let original = r#"{
			"$schema": "https://app.kilo.ai/config.json",
			"theme": "dark",
			"mcp": {
				"local": {
					"type": "local",
					"command": ["npx", "-y", "server"],
					"environment": {"TOKEN": "secret"},
					"enabled": false
				},
				"remote": {
					"type": "remote",
					"url": "https://example.com/mcp",
					"headers": {"Authorization": "Bearer token"},
					"enabled": true
				}
			}
		}"#;

		let parse = DESCRIPTOR.mcp_parse_config.unwrap();
		let config = parse(original).unwrap();
		assert_eq!(config.mcps.len(), 2);
		let local = config.mcps.iter().find(|mcp| mcp.name == "local").unwrap();
		assert!(!local.enabled);
		match &local.transport {
			crate::McpTransport::Stdio { command, args, .. } => {
				assert_eq!(command, "npx");
				assert_eq!(args, &["-y", "server"]);
			}
			other => panic!("expected local MCP, got {other:?}"),
		}
		let remote =
			config.mcps.iter().find(|mcp| mcp.name == "remote").unwrap();
		assert!(matches!(
			remote.transport,
			crate::McpTransport::StreamableHttp { .. }
		));

		let serialize = DESCRIPTOR.mcp_serialize_config.unwrap();
		let output = serialize(&config, Some(original)).unwrap();
		let root: serde_json::Value = serde_json::from_str(&output).unwrap();
		assert_eq!(root["theme"], "dark");
		assert_eq!(
			root["mcp"]["local"]["command"],
			serde_json::json!(["npx", "-y", "server"])
		);
		assert_eq!(root["mcp"]["local"]["enabled"], false);
		assert_eq!(root["mcp"]["remote"]["type"], "remote");
		assert_eq!(root["mcp"]["remote"]["url"], "https://example.com/mcp");
		assert!(root.get("mcpServers").is_none());

		let capabilities = DESCRIPTOR.capabilities.mcp;
		assert!(capabilities.remote);
		assert!(capabilities.enable_disable);
		let markers = DESCRIPTOR.project_markers;
		for marker in ["kilo.json", "kilo.jsonc", ".kilo", ".kilocode"] {
			assert!(
				markers.contains(&marker),
				"missing project marker {marker}"
			);
		}
	}
}
