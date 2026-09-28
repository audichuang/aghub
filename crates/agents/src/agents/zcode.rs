use crate::descriptor::*;
use crate::format::json_map;
use crate::{define_mcp_paths, json_map_dialect};

// ZCode: `json_map` under the nested `mcp.servers` key; the toggle is spelled
// `enable` (missing = enabled). The `type` tag and "untagged remote = streamable
// HTTP" are chosen, not vendor-attested; the `.agents/mcp.json` fallback is
// deliberately not implemented. See docs/descriptors/zcode.md (incl. the probe
// that settles the HTTP tag spelling).
json_map_dialect!(json_map::Dialect {
	server_key: "mcp.servers",
	toggle_key: json_map::ToggleKey::Enabled("enable"),
	untyped_remote: json_map::UntypedRemote::StreamableHttp,
	..json_map::MCP_SERVERS
});

// User and workspace `config.json` sit at different depths — the vendor's layout.
define_mcp_paths! {
	global: ".zcode/cli/config.json",
	project: ".zcode/config.json",
	data_dir: ".zcode",
	strategy: parse_mcp_config, serialize_mcp_config,
}

// Write the private `.zcode/skills` first; `.agents/skills` must stay a READ
// path so `compat_unlink_authorized` counts ZCode as a reader of the shared slot.
// Not `universal: true` — ZCode names `~/.agents/skills`, never XDG.
// See docs/descriptors/zcode.md#skill-roots.
fn global_skills_paths() -> Vec<std::path::PathBuf> {
	match home_dir() {
		Some(home) => {
			vec![home.join(".zcode/skills"), home.join(".agents/skills")]
		}
		None => Vec::new(),
	}
}

fn project_skills_paths(root: &std::path::Path) -> Vec<std::path::PathBuf> {
	vec![root.join(".zcode/skills"), root.join(".agents/skills")]
}

fn global_skill_write_path() -> Option<std::path::PathBuf> {
	home_dir().map(|home| home.join(".zcode/skills"))
}

fn project_skill_write_path(
	root: &std::path::Path,
) -> Option<std::path::PathBuf> {
	Some(root.join(".zcode/skills"))
}

pub const DESCRIPTOR: AgentDescriptor = AgentDescriptor {
	id: "zcode",
	display_name: "ZCode",
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
			enable_disable: true,
		},
		// No sub-agent story is documented. Off rather than guessing a path
		// aghub would write into and ZCode would never read.
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
	sub_agent_global_dir: None,
	sub_agent_project_dir: None,
	cli_name: "zcode",
	validate_args: &["--version"],
	project_markers: &[".zcode"],
	// Not attested in the `vercel-labs/skills` registry.
	skills_cli_name: None,
};

#[cfg(test)]
mod tests {
	use super::*;
	use crate::{AgentConfig, McpServer, McpTransport};
	use std::path::{Path, PathBuf};

	#[test]
	fn zcode_paths_match_the_vendor_layout() {
		let home = home_dir().expect("home directory should resolve");
		// The asymmetry is the whole point: user config under `.zcode/cli/`,
		// workspace config directly under `.zcode/`.
		assert_eq!(
			(DESCRIPTOR.mcp_global_path.unwrap())(),
			Some(home.join(".zcode/cli/config.json"))
		);
		assert_eq!(
			(DESCRIPTOR.mcp_project_path.unwrap())(Path::new("/workspace")),
			Some(PathBuf::from("/workspace/.zcode/config.json"))
		);
		// READ both, in the vendor's discovery order — private first, shared
		// second — and WRITE only the private one. A grant must be visible to
		// ZCode alone, but leaving the shared dir out of the READ list is what
		// makes `compat_unlink_authorized` miscount ZCode as a non-reader and
		// lets another agent's `repair` detach a Referrer ZCode still uses.
		assert_eq!(
			global_skills_paths(),
			vec![home.join(".zcode/skills"), home.join(".agents/skills")]
		);
		assert_eq!(
			project_skills_paths(Path::new("/workspace")),
			vec![
				PathBuf::from("/workspace/.zcode/skills"),
				PathBuf::from("/workspace/.agents/skills"),
			]
		);
		assert_eq!(
			global_skill_write_path(),
			Some(home.join(".zcode/skills")),
			"a grant must land in the private slot, never the shared one"
		);
		assert_eq!(
			project_skill_write_path(Path::new("/workspace")),
			Some(PathBuf::from("/workspace/.zcode/skills"))
		);
		// `universal: true` would append `$XDG_CONFIG_HOME/agents/skills`,
		// which ZCode never names. Its shared root is `~/.agents/skills`, and
		// that one is spelled out above.
		assert!(!DESCRIPTOR
			.global_skill_read_paths()
			.iter()
			.any(|path| path.ends_with("config/agents/skills")));
	}

	#[test]
	fn zcode_nests_servers_and_spells_the_toggle_enable() {
		let mut config = AgentConfig::new();
		config.mcps = vec![
			McpServer::new("local", McpTransport::stdio("run-local", vec![])),
			McpServer::new(
				"api",
				McpTransport::streamable_http("https://example.test/mcp"),
			),
		];
		config.mcps[0].enabled = false;

		let output = (DESCRIPTOR.mcp_serialize_config.unwrap())(&config, None)
			.expect("zcode MCP config should serialize");
		let value: serde_json::Value = serde_json::from_str(&output).unwrap();
		assert_eq!(value["mcp"]["servers"]["local"]["type"], "stdio");
		assert_eq!(value["mcp"]["servers"]["api"]["type"], "http");
		// `enable`, not `enabled` — ZCode never reads the latter.
		assert_eq!(value["mcp"]["servers"]["local"]["enable"], false);
		assert_eq!(value["mcp"]["servers"]["api"]["enable"], true);
		assert!(value["mcp"]["servers"]["local"].get("enabled").is_none());

		let reparsed = (DESCRIPTOR.mcp_parse_config.unwrap())(&output).unwrap();
		let local = reparsed.mcps.iter().find(|m| m.name == "local").unwrap();
		assert!(!local.enabled, "the disabled flag must round-trip");
	}

	#[test]
	fn zcode_ignores_the_canonical_toggle_spellings() {
		// A server with no `enable` field is ENABLED, whatever `enabled` /
		// `disabled` say — ZCode reads neither, so honouring one would report
		// an on/off state the vendor does not have and the next save would
		// make it real.
		let original = r#"{
			"mcp": {
				"servers": {
					"kept": {
						"command": "run",
						"disabled": true,
						"enabled": false,
						"cwd": "/workspace"
					}
				}
			}
		}"#;
		let parse = DESCRIPTOR.mcp_parse_config.unwrap();
		let config = parse(original).expect("zcode MCP config should parse");
		assert!(config.mcps[0].enabled);

		let output =
			(DESCRIPTOR.mcp_serialize_config.unwrap())(&config, Some(original))
				.expect("zcode MCP config should serialize");
		let value: serde_json::Value = serde_json::from_str(&output).unwrap();
		let entry = &value["mcp"]["servers"]["kept"];
		assert_eq!(entry["enable"], true);
		// Unread keys are unmanaged data, not ours to delete.
		assert_eq!(entry["disabled"], true);
		assert_eq!(entry["enabled"], false);
		assert_eq!(entry["cwd"], "/workspace");
	}
}
