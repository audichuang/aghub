use crate::descriptor::*;
use crate::format::json_map;
use crate::sub_agents::{
	load_scoped_sub_agents_with, save_scoped_sub_agents_with, SubAgentLayout,
};
use crate::{define_mcp_paths, json_map_dialect};
use std::path::{Path, PathBuf};

// Antigravity spells the remote endpoint `serverUrl` and toggles with
// `disabled`; its explicit WebSocket option has no counterpart in the
// normalized model (rejected on read).
// The `type` tag stays even where the vendor docs only show it for stdio:
// dropping it makes SSE indistinguishable from streamable HTTP on the next
// read, and v2.13.3 already wrote it — removing it would strand every
// config that release produced.
json_map_dialect!(json_map::Dialect {
	url_key: "serverUrl",
	legacy_url_keys: &["url"],
	toggle_key: json_map::ToggleKey::Disabled,
	..json_map::MCP_SERVERS
});

define_mcp_paths! {
	global: ".gemini/config/mcp_config.json",
	project: ".agents/mcp_config.json",
	data_dir: ".gemini/antigravity",
	strategy: parse_mcp_config, serialize_mcp_config,
}

// Antigravity's vendor docs moved the global customization root to
// `~/.gemini/config/` — skills live at `~/.gemini/config/skills/<name>/SKILL.md`
// and that dir is shared by Antigravity 2.0, the IDE and the CLI. Two older
// dirs stay READ-ONLY so nothing a shipped aghub installed is stranded:
// `.gemini/antigravity/skills` (the IDE-1.x path npx `agents.ts` still names,
// and what aghub wrote up to v2.18.x) and `.gemini/antigravity-cli/skills`
// (documented by the CLI's plugin page). Write dir FIRST: `load_skills_from_dirs`
// dedups first-dir-wins and the winner becomes `source_path`, i.e. the path
// `remove_skill` deletes and `check` hashes.
// Decision #12 in `docs/specs/2026-08-30-skills-hub-borrow-path.md` was revised
// on 2026-09-06 with the evidence; #11 still holds — all three are Antigravity's
// own dirs, never another agent's private one.
//
// Two known costs of the read-only legacy dirs, both pinned by tests rather
// than left to be rediscovered:
//  * `doctor --verify-links` inspects the WRITE slot only, so a skill an older
//    release installed into `.gemini/antigravity/skills` audits as `withheld`
//    even though Antigravity really does read it. Relink to clear it.
//  * A Referrer parked in one of those dirs cannot be removed for antigravity
//    ALONE — the planner schedules only the write dir, so `delete --yes`
//    answers `outcome: kept` with the file in place
//    (`npx_skill_path_ownership.rs::a_referrer_in_a_read_only_compat_dir_…`).
// Both beat the alternative, which was not reading the dirs and stranding the
// skills outright.
fn global_skills_paths() -> Vec<PathBuf> {
	let Some(home) = home_dir() else {
		return Vec::new();
	};
	vec![
		home.join(".gemini/config/skills"),
		home.join(".gemini/antigravity/skills"),
		home.join(".gemini/antigravity-cli/skills"),
	]
}

// `.agent/` (singular) is the vendor's own backward-compat alias for `.agents/`.
fn project_skills_paths(root: &Path) -> Vec<PathBuf> {
	vec![root.join(".agents/skills"), root.join(".agent/skills")]
}

fn global_skill_write_path() -> Option<PathBuf> {
	home_dir().map(|home| home.join(".gemini/config/skills"))
}

fn project_skill_write_path(root: &Path) -> Option<PathBuf> {
	Some(root.join(".agents/skills"))
}

// Antigravity custom sub-agents are a DIRECTORY per agent holding `agent.md`
// — `{workspace}/.agents/agents/{name}/agent.md` and
// `~/.gemini/config/agents/{name}/agent.md` — mirroring how its skills are
// `{name}/SKILL.md`. Frontmatter is `name` + `description`, exactly what
// `SubAgentFrontmatter` already models, so only the layout differs.
const SUB_AGENT_LAYOUT: SubAgentLayout = SubAgentLayout::Nested {
	file_name: "agent.md",
};

fn sub_agent_global_dir() -> Option<PathBuf> {
	home_dir().map(|home| home.join(".gemini/config/agents"))
}

// Only `.agents/agents`, not the `.agent/agents` alias the skills side reads:
// the sub-agent path model is ONE dir per scope (`fn(&Path) -> Option<PathBuf>`)
// with no read/write split, so a second dir would mean making that model plural
// for every sub-agent-capable agent. Not worth it for a compat alias the vendor
// no longer defaults to — revisit if workspaces actually use it.
fn sub_agent_project_dir(root: &Path) -> Option<PathBuf> {
	Some(root.join(".agents/agents"))
}

fn load_sub_agents(
	project_root: Option<&Path>,
	scope: crate::ResourceScope,
) -> crate::Result<Vec<crate::SubAgent>> {
	load_scoped_sub_agents_with(
		project_root,
		scope,
		Some(sub_agent_global_dir),
		Some(sub_agent_project_dir),
		SUB_AGENT_LAYOUT,
	)
}

fn save_sub_agents(
	project_root: Option<&Path>,
	scope: crate::ResourceScope,
	agents: &[crate::SubAgent],
) -> crate::Result<()> {
	save_scoped_sub_agents_with(
		project_root,
		scope,
		agents,
		Some(sub_agent_global_dir),
		Some(sub_agent_project_dir),
		SUB_AGENT_LAYOUT,
	)
}

pub const DESCRIPTOR: AgentDescriptor = AgentDescriptor {
	id: "antigravity",
	display_name: "Antigravity",
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
		sub_agents: SubAgentCapabilities {
			scopes: ScopeSupport {
				global: true,
				project: true,
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
	load_sub_agents,
	save_sub_agents,
	cli_name: "antigravity",
	validate_args: &["--version"],
	project_markers: &[".agents/mcp_config.json"],
	skills_cli_name: Some("antigravity"),
};

#[cfg(test)]
mod tests {
	use super::*;

	// Compile-time pin: the capability the tests below rely on.
	const _: () = {
		assert!(DESCRIPTOR.capabilities.mcp.remote);
		assert!(DESCRIPTOR.capabilities.mcp.enable_disable);
	};
	use crate::{AgentConfig, McpServer, McpTransport};
	use std::path::Path;

	#[test]
	fn antigravity_mcp_paths_match_runtime() {
		let home = home_dir().expect("home directory should resolve");
		assert_eq!(
			(DESCRIPTOR.mcp_global_path.unwrap())(),
			Some(home.join(".gemini/config/mcp_config.json"))
		);
		assert_eq!(
			(DESCRIPTOR.mcp_project_path.unwrap())(Path::new("/workspace")),
			Some(Path::new("/workspace/.agents/mcp_config.json").to_path_buf())
		);
		assert_eq!(DESCRIPTOR.project_markers, &[".agents/mcp_config.json"]);
	}

	// The write move is only safe because the legacy dirs stay readable, and
	// only correct because the NEW dir wins a name clash — first-dir-wins is
	// what decides `source_path`, i.e. the path `remove_skill` deletes and
	// `check` hashes. Asserting the resolved ORDER, not a count.
	#[test]
	fn antigravity_reads_the_legacy_dirs_but_prefers_the_vendor_one() {
		let home = home_dir().expect("home directory should resolve");
		assert_eq!(
			global_skills_paths(),
			vec![
				home.join(".gemini/config/skills"),
				home.join(".gemini/antigravity/skills"),
				home.join(".gemini/antigravity-cli/skills"),
			],
			"vendor dir first, then the two legacy read-only slots"
		);
		assert_eq!(
			(DESCRIPTOR.global_skill_paths.unwrap().write)(),
			Some(home.join(".gemini/config/skills")),
			"writes go to the vendor dir only"
		);
		assert_eq!(
			project_skills_paths(Path::new("/workspace")),
			vec![
				PathBuf::from("/workspace/.agents/skills"),
				PathBuf::from("/workspace/.agent/skills"),
			]
		);
		assert_eq!(
			(DESCRIPTOR.project_skill_paths.unwrap().write)(Path::new(
				"/workspace"
			)),
			Some(PathBuf::from("/workspace/.agents/skills")),
			"the project write slot must NOT move to the .agent alias"
		);
	}

	#[test]
	fn antigravity_sub_agents_live_in_per_agent_directories() {
		let home = home_dir().expect("home directory should resolve");
		assert_eq!(
			sub_agent_global_dir(),
			Some(home.join(".gemini/config/agents"))
		);
		assert_eq!(
			sub_agent_project_dir(Path::new("/workspace")),
			Some(PathBuf::from("/workspace/.agents/agents"))
		);
		assert_eq!(
			SUB_AGENT_LAYOUT,
			SubAgentLayout::Nested {
				file_name: "agent.md"
			}
		);
	}

	#[test]
	fn antigravity_uses_server_url_and_keeps_the_two_remotes_apart() {
		let mut config = AgentConfig::new();
		config.mcps = vec![
			McpServer::new(
				"api",
				McpTransport::streamable_http("https://example.test/mcp"),
			),
			// Deliberately NOT a `/sse` path: without the `type` tag this
			// would read back as streamable HTTP.
			McpServer::new(
				"events",
				McpTransport::sse("https://example.test/v1/messages"),
			),
		];

		let output = (DESCRIPTOR.mcp_serialize_config.unwrap())(&config, None)
			.expect("Antigravity MCP config should serialize");
		let value: serde_json::Value = serde_json::from_str(&output).unwrap();
		assert_eq!(
			value["mcpServers"]["api"]["serverUrl"],
			"https://example.test/mcp"
		);
		assert!(value["mcpServers"]["api"].get("url").is_none());

		let reparsed = (DESCRIPTOR.mcp_parse_config.unwrap())(&output).unwrap();
		let events = reparsed.mcps.iter().find(|m| m.name == "events").unwrap();
		assert!(
			matches!(events.transport, McpTransport::Sse { .. }),
			"SSE must survive the round trip, got {:?}",
			events.transport
		);
	}

	#[test]
	fn antigravity_roundtrip_preserves_disabled_and_unmanaged_fields() {
		let original = r#"{
			"theme": "dark",
			"mcpServers": {
				"local": {
					"command": "uvx",
					"args": ["example-server"],
					"env": {"TOKEN": "secret"},
					"cwd": "/workspace/service",
					"auth": {"audience": "example"},
					"disabled": true,
					"disabledTools": ["dangerous"]
				}
			}
		}"#;
		let parse = DESCRIPTOR.mcp_parse_config.unwrap();
		let config =
			parse(original).expect("Antigravity MCP config should parse");
		assert_eq!(config.mcps.len(), 1);
		assert!(!config.mcps[0].enabled);
		assert!(matches!(
			&config.mcps[0].transport,
			McpTransport::Stdio { command, args, env, .. }
				if command == "uvx"
					&& args == &["example-server"]
					&& env.as_ref().and_then(|env| env.get("TOKEN"))
						.map(String::as_str)
						== Some("secret")
		));

		let output =
			(DESCRIPTOR.mcp_serialize_config.unwrap())(&config, Some(original))
				.expect("Antigravity MCP config should serialize");
		let value: serde_json::Value = serde_json::from_str(&output).unwrap();
		let local = &value["mcpServers"]["local"];

		assert_eq!(local["disabled"], true);
		assert_eq!(local["cwd"], "/workspace/service");
		assert_eq!(local["auth"]["audience"], "example");
		assert_eq!(local["disabledTools"][0], "dangerous");
		assert_eq!(value["theme"], "dark");
	}
}
