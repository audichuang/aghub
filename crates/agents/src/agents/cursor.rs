use crate::descriptor::*;
use crate::format::json_map;
use crate::{define_mcp_paths, json_map_dialect};
use std::path::{Path, PathBuf};

// Cursor documents `type: "stdio"` for local servers, but its remote example is
// a bare `url` with no transport tag — so SSE has no native spelling here.
json_map_dialect!(json_map::Dialect {
	discriminator: Some(json_map::Discriminator {
		key: "type",
		stdio: "stdio",
		sse: "",
		http: "",
	}),
	untyped_remote: json_map::UntypedRemote::StreamableHttp,
	..json_map::MCP_SERVERS
});

define_mcp_paths! {
	symmetric: ".cursor/mcp.json",
	strategy: parse_mcp_config, serialize_mcp_config,
}

// npx-`skills` layout: Cursor owns ONLY its own per-agent dir (which holds
// symlink Referrers) plus the universal `.agents/skills` Master. It must NOT
// read another agent's private dir (`.claude/skills`, `.codex/skills`) — that
// makes Cursor discover skills it does not own and plan destructive removals
// against another agent's content. Mapping mirrors upstream `agents.ts`
// (cursor → project `.agents/skills`, global `~/.cursor/skills`); the global
// Master is `~/.agents/skills` per the npx interop contract.
fn global_skills_paths() -> Vec<PathBuf> {
	let Some(home) = home_dir() else {
		return Vec::new();
	};
	vec![home.join(".cursor/skills"), home.join(".agents/skills")]
}
fn project_skills_paths(root: &Path) -> Vec<PathBuf> {
	vec![root.join(".cursor/skills"), root.join(".agents/skills")]
}

fn global_skill_write_path() -> Option<PathBuf> {
	home_dir().map(|home| home.join(".cursor/skills"))
}

fn project_skill_write_path(root: &Path) -> Option<PathBuf> {
	Some(root.join(".cursor/skills"))
}

pub const DESCRIPTOR: AgentDescriptor = AgentDescriptor {
	id: "cursor",
	display_name: "Cursor",
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
	cli_name: "cursor",
	validate_args: &["--version"],
	project_markers: &[".cursor"],
	skills_cli_name: Some("cursor"),
};
