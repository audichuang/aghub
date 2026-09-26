use crate::descriptor::*;
use crate::format::json_map;
use crate::{define_mcp_paths, json_map_dialect};

// ZCode. Its MCP servers live under `mcp.servers` in a native `config.json` —
// `json_map`'s `server_key` is a DOTTED path, so the nesting needs no parser.
//
// The per-server toggle is spelled `enable`, NOT `enabled`: writing `false`
// switches the server off and a server WITHOUT the field counts as ENABLED.
// That one letter is why `ToggleKey` carries its spelling as data — a toggle
// aghub wrote under a name ZCode does not read is a server the user switched
// off that comes back on.
//
// TWO answers the vendor documentation does not give, chosen conservatively and
// recorded here so the next reader knows they were chosen, not attested:
//
//   * The TRANSPORT TAG. The docs name stdio, HTTP and SSE but never say which
//     field distinguishes them, and the one worked example (stdio) carries no
//     tag at all. This inherits `MCP_SERVERS` — `type: stdio | sse | http`, the
//     spelling 17 of aghub's agents already use — rather than inventing a key.
//   * An UNTAGGED remote is streamable HTTP. `InferSseFromUrl` would make a URL
//     with an `/sse/` path segment parse as SSE and the next save write
//     `type: "sse"` over it; that heuristic is a guess about a vendor whose
//     docs say nothing, so the one transport aghub can read back unchanged wins.
//
// HOW TO CLOSE THE FIRST ONE, because no test in this repo can. `MCP_SERVERS`
// READS `http`, `streamable-http` and `streamableHttp` alike, but WRITES
// `http`, and a save rebuilds EVERY server in the file — so if ZCode validates
// a different spelling, one `aghub mcps add` retags a remote server the user
// already had working, silently. The family default has been wrong twice
// already (`cline` writes `streamableHttp`, `roocode` writes `streamable-http`,
// both overriding it). The probe: put
// `{"mcp":{"servers":{"probe":{"type":"http","url":"https://example.test/mcp"}}}}`
// in `~/.zcode/cli/config.json`, open a ZCode session and check Settings → MCP
// for that server; repeat with `"streamable-http"`. Whichever one connects is
// the answer, and the fix is one line here:
// `vocab: TransportVocabulary { http: "<answer>", ..MCP_SERVERS.vocab }`.
//
// `.agents` COMPATIBILITY, deliberately NOT implemented. ZCode also reads
// `~/.agents/mcp.json` and `<root>/.agents/mcp.json` under an `mcpServers` key,
// but only as a FALLBACK: within a scope, if the `.zcode` config defines any
// server at all, the `.agents` file for that scope is skipped ENTIRELY — no
// merging. ZCode's own settings panel always writes back to the `.zcode` native
// config and never touches `.agents`, and aghub does the same. The footgun the
// vendor calls out: a user who keeps their servers only in `.agents/mcp.json`
// stops loading ALL of them the moment anything writes one server into the
// `.zcode` config — including the first `aghub mcp add`. Reading both and
// merging them would write a file ZCode reads differently than aghub does, so
// the split stays visible instead.
json_map_dialect!(json_map::Dialect {
	server_key: "mcp.servers",
	toggle_key: json_map::ToggleKey::Enabled("enable"),
	untyped_remote: json_map::UntypedRemote::StreamableHttp,
	..json_map::MCP_SERVERS
});

// The depths differ and that is the vendor's, not a typo: the USER config sits
// under `.zcode/cli/`, the WORKSPACE one directly under `.zcode/`. Both are
// named `config.json`.
define_mcp_paths! {
	global: ".zcode/cli/config.json",
	project: ".zcode/config.json",
	data_dir: ".zcode",
	strategy: parse_mcp_config, serialize_mcp_config,
}

// ZCode reads FOUR skill roots, and the private one comes first at each scope.
// Its own `zcode-configuration-guide` gives the discovery order: user
// `~/.zcode/skills` → user `~/.agents/skills` → workspace `<root>/.zcode/skills`
// → workspace `<root>/.agents/skills` → plugin roots, and "within a level,
// `.zcode` is scanned before `.agents`".
//
// So the WRITE slot is the private `.zcode/skills` — that is what makes a grant
// visible to ZCode alone — while `.agents/skills` must still be listed as a
// READ path. That second half is not cosmetic: root `AGENTS.md` keeps the whole
// shared-slot section because an agent missing from a shared dir's reader set
// is an agent `skills::shape::compat_unlink_authorized` does not count, and a
// `repair` run for a DIFFERENT agent may then detach a compat Referrer that
// ZCode is still reading. Covered reader, not read-only co-reader: ZCode has
// its own write slot at both scopes, so the quorum passes on its own coverage.
//
// NOT `universal: true`: that flag appends `$XDG_CONFIG_HOME/agents/skills`,
// which ZCode never names. Its shared root is `~/.agents/skills`, spelled here.
//
// Symlinked skill directories are supported by the vendor — importing skills
// from other agents that way is a documented feature — which is what aghub's
// symlink-only install needs.
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
