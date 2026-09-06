use crate::descriptor::*;
use crate::format::{json_map, mcp_policy};
use crate::{define_mcp_paths, json_map_dialect};
use std::path::{Path, PathBuf};

// Oh My Pi (`can1357/oh-my-pi`, a fork of `earendil-works/pi`). Its native MCP
// config is an `mcpServers` map whose transport tag is `transport` (NOT `type`),
// accepting exactly `stdio` | `sse` | `http`:
//
//     if ("transport" in e && (e.transport === "stdio" || e.transport === "sse"
//                              || e.transport === "http"))
//
// and an untagged entry resolves by which field is present:
//
//     e.transport ?? (e.command ? "stdio" : e.url ? "http" : "stdio")
//
// so an untagged remote is streamable HTTP, never SSE. The per-server toggle is
// a native `enabled` bool — omp's own importers propagate `enabled: false` from
// every foreign config it reads, so dropping it would remount a server the user
// switched off. Fields omp owns that aghub does not (`cwd`, `oauth`, `auth`,
// `requestIdFormat`, `envPolicy`) survive because `json_map` rewrites only the
// transport keys.
json_map_dialect!(json_map::Dialect {
	vocab: mcp_policy::TransportVocabulary {
		tag_key: "transport",
		..json_map::MCP_SERVERS.vocab
	},
	toggle_key: json_map::ToggleKey::Enabled,
	untyped_remote: json_map::UntypedRemote::StreamableHttp,
	..json_map::MCP_SERVERS
});

// omp reads a project `mcp.json`/`.mcp.json` at the repo root too, but those are
// the SHARED files Claude and Copilot already own here. Pointing omp at them
// would enrol it in the verified-unfixed `reconcile mcp --remove` bug (an agent
// that shares a backing file and is named nowhere in the command still loses the
// server). omp's own `.omp/mcp.json` is the only project file aghub writes.
define_mcp_paths! {
	global: ".omp/agent/mcp.json",
	project: ".omp/mcp.json",
	data_dir: ".omp/agent",
	strategy: parse_mcp_config, serialize_mcp_config,
}

// Skills: own dir plus the universal `.agents/skills` Master, at BOTH scopes —
// omp walks `[".agent", ".agents"]` from cwd to the repo root and at the user
// level. Own dir FIRST (first-dir-wins decides `source_path`, i.e. the path
// `remove_skill` deletes). The singular `.agent/skills` slot and omp's
// first-run import of `.claude` / `.cursor` / `.codex` / … are deliberately not
// modelled — decision #11: never read another agent's private dir.
fn global_skills_paths() -> Vec<PathBuf> {
	match home_dir() {
		Some(home) => {
			vec![home.join(".omp/agent/skills"), home.join(".agents/skills")]
		}
		None => Vec::new(),
	}
}

fn project_skills_paths(root: &Path) -> Vec<PathBuf> {
	vec![root.join(".omp/skills"), root.join(".agents/skills")]
}

fn global_skill_write_path() -> Option<PathBuf> {
	home_dir().map(|home| home.join(".omp/agent/skills"))
}

fn project_skill_write_path(root: &Path) -> Option<PathBuf> {
	Some(root.join(".omp/skills"))
}

pub const DESCRIPTOR: AgentDescriptor = AgentDescriptor {
	id: "omp",
	display_name: "Oh My Pi",
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
		// omp DOES have sub-agents (`~/.omp/agent/agents/<name>.md`), but its
		// frontmatter carries `tools` / `model` / `spawns` / `thinkingLevel` /
		// `output` / `blocking`. aghub now preserves unowned frontmatter keys on
		// save, so the data-loss objection is closed — what is still missing is
		// any attested source for the PROJECT-scope dir. Ship read/write off
		// until that is verified rather than guessing a path aghub would write
		// into and omp would never read.
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
	cli_name: "omp",
	validate_args: &["--version"],
	project_markers: &[".omp"],
	// Not in the `vercel-labs/skills` registry (checked 2026-09-06).
	skills_cli_name: None,
};

#[cfg(test)]
mod tests {
	use super::*;
	use crate::{AgentConfig, McpServer, McpTransport};

	#[test]
	fn omp_paths_match_runtime() {
		let home = home_dir().expect("home directory should resolve");
		assert_eq!(
			(DESCRIPTOR.mcp_global_path.unwrap())(),
			Some(home.join(".omp/agent/mcp.json"))
		);
		assert_eq!(
			(DESCRIPTOR.mcp_project_path.unwrap())(Path::new("/workspace")),
			Some(Path::new("/workspace/.omp/mcp.json").to_path_buf())
		);
		assert_eq!(
			global_skills_paths(),
			vec![home.join(".omp/agent/skills"), home.join(".agents/skills")]
		);
		assert_eq!(
			project_skills_paths(Path::new("/workspace")),
			vec![
				PathBuf::from("/workspace/.omp/skills"),
				PathBuf::from("/workspace/.agents/skills"),
			]
		);
	}

	#[test]
	fn omp_tags_transport_and_keeps_a_disabled_server() {
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
			.expect("omp MCP config should serialize");
		let value: serde_json::Value = serde_json::from_str(&output).unwrap();
		// `transport`, not `type` — the whole point of omp's dialect.
		assert_eq!(value["mcpServers"]["local"]["transport"], "stdio");
		assert!(value["mcpServers"]["local"].get("type").is_none());
		assert_eq!(value["mcpServers"]["api"]["transport"], "http");
		// A disabled server must SURVIVE the write with its flag, not vanish.
		assert_eq!(value["mcpServers"]["local"]["enabled"], false);
		assert_eq!(value["mcpServers"]["api"]["enabled"], true);

		let reparsed = (DESCRIPTOR.mcp_parse_config.unwrap())(&output).unwrap();
		let local = reparsed.mcps.iter().find(|m| m.name == "local").unwrap();
		assert!(!local.enabled, "disabled flag must round-trip");
	}

	#[test]
	fn omp_reads_an_untagged_remote_as_streamable_http() {
		// `e.transport ?? (e.command ? "stdio" : e.url ? "http" : "stdio")` —
		// an `/sse/`-shaped path does NOT make it SSE.
		let config = (DESCRIPTOR.mcp_parse_config.unwrap())(
			r#"{"mcpServers":{"s":{"url":"https://example.test/sse/stream"}}}"#,
		)
		.unwrap();
		assert!(
			matches!(
				config.mcps[0].transport,
				McpTransport::StreamableHttp { .. }
			),
			"got {:?}",
			config.mcps[0].transport
		);
	}

	#[test]
	fn omp_preserves_fields_it_does_not_own() {
		let original = r#"{
			"mcpServers": {
				"local": {
					"transport": "stdio",
					"command": "uvx",
					"cwd": "/workspace/service",
					"requestIdFormat": "string",
					"enabled": true
				}
			}
		}"#;
		let parse = DESCRIPTOR.mcp_parse_config.unwrap();
		let config = parse(original).expect("omp MCP config should parse");
		let output =
			(DESCRIPTOR.mcp_serialize_config.unwrap())(&config, Some(original))
				.expect("omp MCP config should serialize");
		let value: serde_json::Value = serde_json::from_str(&output).unwrap();
		assert_eq!(value["mcpServers"]["local"]["cwd"], "/workspace/service");
		assert_eq!(
			value["mcpServers"]["local"]["requestIdFormat"], "string",
			"omp-specific keys must survive an aghub rewrite"
		);
	}
}
