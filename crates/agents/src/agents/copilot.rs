use crate::descriptor::*;
use crate::format::json_map;
use crate::json_map_dialect;
use crate::sub_agents::{
	load_scoped_sub_agents_with, save_scoped_sub_agents_with, SubAgentLayout,
};
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

// Copilot CLI reads skills from its own dir plus the universal `.agents/skills`
// Master, and at project scope also from `.github/skills` (the repo-committed
// slot its docs name first). `.claude/skills` is documented too but deliberately
// NOT read here — decision #11 in
// `docs/specs/2026-08-30-skills-hub-borrow-path.md`: an agent never reads
// another agent's private dir, because doing so makes it discover skills it does
// not own and plan destructive removals against them.
// Write dir FIRST in each list (`load_skills_from_dirs` is first-dir-wins and the
// winner becomes `source_path`, the path `remove_skill` deletes).
fn global_skills_paths() -> Vec<PathBuf> {
	let Some(home) = home_dir() else {
		return Vec::new();
	};
	vec![home.join(".copilot/skills"), home.join(".agents/skills")]
}

fn project_skills_paths(root: &Path) -> Vec<PathBuf> {
	vec![root.join(".agents/skills"), root.join(".github/skills")]
}

fn global_skill_write_path() -> Option<PathBuf> {
	home_dir().map(|home| home.join(".copilot/skills"))
}

fn project_skill_write_path(root: &Path) -> Option<PathBuf> {
	Some(root.join(".agents/skills"))
}

// Copilot CLI custom agents: `~/.copilot/agents/` (personal) and
// `.github/agents/` (repository), each a `<name>.agent.md` file. Its reference
// says the identity is "the configuration file's name (minus `.md` or
// `.agent.md`)" — the frontmatter `name` is only a display name — so the SUFFIX
// is load-bearing on the write side: a plain `<name>.md` lands where Copilot
// never looks, and every aghub-side round-trip assertion still passes.
const SUB_AGENT_LAYOUT: SubAgentLayout = SubAgentLayout::Flat {
	suffix: ".agent.md",
};

fn sub_agent_global_dir() -> Option<PathBuf> {
	copilot_home().map(|home| home.join("agents"))
}

fn sub_agent_project_dir(root: &Path) -> Option<PathBuf> {
	Some(root.join(".github/agents"))
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
	fn copilot_reads_the_universal_master_and_the_github_slot() {
		let home = home_dir().expect("home directory should resolve");
		assert_eq!(
			global_skills_paths(),
			vec![home.join(".copilot/skills"), home.join(".agents/skills")]
		);
		assert_eq!(
			project_skills_paths(Path::new("/workspace")),
			vec![
				PathBuf::from("/workspace/.agents/skills"),
				PathBuf::from("/workspace/.github/skills"),
			],
			"the WRITE dir must stay first — first-dir-wins decides source_path"
		);
		assert_eq!(
			(DESCRIPTOR.project_skill_paths.unwrap().write)(Path::new(
				"/workspace"
			)),
			Some(PathBuf::from("/workspace/.agents/skills"))
		);
	}

	#[test]
	fn copilot_sub_agents_use_the_agent_md_suffix() {
		assert_eq!(
			sub_agent_project_dir(Path::new("/workspace")),
			Some(PathBuf::from("/workspace/.github/agents"))
		);
		assert_eq!(
			sub_agent_global_dir(),
			copilot_home().map(|home| home.join("agents"))
		);
		// A bare `.md` here lands where Copilot never looks, and every
		// aghub-side round trip stays green — so pin the suffix itself.
		assert_eq!(
			SUB_AGENT_LAYOUT,
			SubAgentLayout::Flat {
				suffix: ".agent.md"
			}
		);
	}

	// aghub wrote `"type": "stdio"` into `~/.copilot/mcp-config.json` up to now,
	// and Copilot also reads the project `.mcp.json` that Claude writes with the
	// same spelling. Whatever the vendor docs prefer (`local`), those files must
	// keep parsing — and the tag must be what DECIDES it.
	//
	// A `command`-only assertion cannot show that: `json_map`'s presence
	// fallback makes ANY tag (or none) parse as stdio when `command` is set, so
	// the same assertion stays green with the tag deleted or spelled `bogus`.
	// The falsifiable half is the REMOTE case, where the tag is the only thing
	// separating SSE from streamable HTTP.
	#[test]
	fn copilot_dispatches_on_the_type_tag_it_has_been_writing() {
		let parse = DESCRIPTOR.mcp_parse_config.unwrap();
		let stdio =
			parse(r#"{"mcpServers":{"s":{"type":"stdio","command":"c"}}}"#)
				.unwrap();
		assert!(
			matches!(stdio.mcps[0].transport, McpTransport::Stdio { .. }),
			"got {:?}",
			stdio.mcps[0].transport
		);

		// Same URL, two tags, two answers — only the tag can produce that.
		// `untyped_remote: StreamableHttp` means the path heuristic cannot.
		let sse = parse(
			r#"{"mcpServers":{"s":{"type":"sse","url":"https://x.test/mcp"}}}"#,
		)
		.unwrap();
		assert!(
			matches!(sse.mcps[0].transport, McpTransport::Sse { .. }),
			"got {:?}",
			sse.mcps[0].transport
		);
		let http = parse(
			r#"{"mcpServers":{"s":{"type":"http","url":"https://x.test/mcp"}}}"#,
		)
		.unwrap();
		assert!(
			matches!(
				http.mcps[0].transport,
				McpTransport::StreamableHttp { .. }
			),
			"got {:?}",
			http.mcps[0].transport
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
