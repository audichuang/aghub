use crate::descriptor::*;
use std::path::{Path, PathBuf};

// DeepSeek Harness (`@deepseek-ai/dsh`) — not Deep Code, not DeepSeek-TUI.
// Private `.dsh/skills` outranks the shared `.agents/skills` at both scopes, so
// the write dir goes first. `universal: false`: dsh's shared root is `~/.agents`
// (`$DSH_AGENTS_HOME`), not XDG. MCP is deliberately unsupported (per-profile
// Cordis YAML with `!!js` tags cannot round-trip); sub-agents not researched.
// See docs/descriptors/dsh.md.

/// `$VAR` wins over `~/<leaf>`; an empty value counts as unset.
fn resolve_root(
	override_dir: Option<PathBuf>,
	home: Option<PathBuf>,
	leaf: &str,
) -> Option<PathBuf> {
	override_dir.or_else(|| home.map(|home| home.join(leaf)))
}

/// Named `env_path` deliberately: `descriptor_regression`'s
/// `path_override_vars_covers_every_descriptor_read` scans this source for
/// `env::var("`, `env::var_os("` and `env_path("` call sites. A wrapper under
/// any other name hides the read, and the guard then passes by seeing nothing.
fn env_path(name: &str) -> Option<PathBuf> {
	std::env::var_os(name)
		.filter(|value| !value.is_empty())
		.map(PathBuf::from)
}

fn dsh_home() -> Option<PathBuf> {
	resolve_root(env_path("DSH_HOME"), home_dir(), ".dsh")
}

fn dsh_agents_home() -> Option<PathBuf> {
	resolve_root(env_path("DSH_AGENTS_HOME"), home_dir(), ".agents")
}

fn global_data_dir() -> Option<PathBuf> {
	dsh_home()
}

fn global_skills_paths() -> Vec<PathBuf> {
	[dsh_home(), dsh_agents_home()]
		.into_iter()
		.flatten()
		.map(|root| root.join("skills"))
		.collect()
}

fn project_skills_paths(root: &Path) -> Vec<PathBuf> {
	vec![root.join(".dsh/skills"), root.join(".agents/skills")]
}

fn global_skill_write_path() -> Option<PathBuf> {
	dsh_home().map(|root| root.join("skills"))
}

fn project_skill_write_path(root: &Path) -> Option<PathBuf> {
	Some(root.join(".dsh/skills"))
}

pub const DESCRIPTOR: AgentDescriptor = AgentDescriptor {
	id: "dsh",
	agent_type: crate::AgentType::Dsh,
	display_name: "DeepSeek Harness",
	mcp_parse_config: None,
	mcp_serialize_config: None,
	mcp_global_path: None,
	mcp_project_path: None,
	global_data_dir,
	capabilities: Capabilities {
		skills: SkillCapabilities {
			scopes: ScopeSupport {
				global: true,
				project: true,
			},
			// `$DSH_AGENTS_HOME` (default `~/.agents`) is NOT the XDG group.
			universal: false,
		},
		mcp: McpCapabilities {
			scopes: ScopeSupport {
				global: false,
				project: false,
			},
			stdio: false,
			remote: false,
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
	sub_agent_global_dir: None,
	sub_agent_project_dir: None,
	cli_name: "dsh",
	validate_args: &["--version"],
	project_markers: &[".dsh"],
	// Not attested in the `vercel-labs/skills` registry.
	skills_cli_name: None,
};

#[cfg(test)]
mod tests {
	use super::*;

	/// The env overrides, without touching the process environment: the
	/// resolvers are pure once the two inputs are injected.
	#[test]
	fn dsh_roots_prefer_their_env_vars_over_home() {
		assert_eq!(
			resolve_root(
				Some(PathBuf::from("/custom/dsh")),
				Some(PathBuf::from("/home/user")),
				".dsh",
			),
			Some(PathBuf::from("/custom/dsh"))
		);
		assert_eq!(
			resolve_root(None, Some(PathBuf::from("/home/user")), ".dsh"),
			Some(PathBuf::from("/home/user/.dsh"))
		);
		assert_eq!(
			resolve_root(None, Some(PathBuf::from("/home/user")), ".agents"),
			Some(PathBuf::from("/home/user/.agents"))
		);
		// No home and no override is the only case that resolves to nothing —
		// an empty variable is dropped one layer up, in `env_path`.
		assert_eq!(resolve_root(None, None, ".dsh"), None);
	}

	/// The PATH LISTS, not the capability flags — a flag can be right while the
	/// paths are wrong. Read order is dsh's own rank order (private before
	/// shared), and the write path must be the head of each read list.
	#[test]
	fn dsh_reads_its_private_slot_first_and_the_shared_one_second() {
		let dsh = dsh_home().expect("dsh home should resolve");
		let agents = dsh_agents_home().expect("agents home should resolve");
		let root = Path::new("/workspace");

		assert_eq!(
			DESCRIPTOR.global_skill_read_paths(),
			vec![dsh.join("skills"), agents.join("skills")],
			"rank 400 user-dsh before rank 500 user-agents"
		);
		assert_eq!(
			DESCRIPTOR.project_skill_read_paths(root),
			vec![
				PathBuf::from("/workspace/.dsh/skills"),
				PathBuf::from("/workspace/.agents/skills"),
			],
			"rank 100 project-dsh before rank 200 project-agents"
		);
		assert_eq!(
			(DESCRIPTOR.global_skill_paths.unwrap().write)(),
			Some(dsh.join("skills")),
			"grants go to the PRIVATE slot, so dsh can hold a skill the other \
			 `.agents/skills` readers do not"
		);
		assert_eq!(
			(DESCRIPTOR.project_skill_paths.unwrap().write)(root),
			Some(PathBuf::from("/workspace/.dsh/skills"))
		);
		// First-dir-wins decides `source_path`; the write dir must lead.
		assert_eq!(
			DESCRIPTOR.global_skill_read_paths().first(),
			(DESCRIPTOR.global_skill_paths.unwrap().write)().as_ref()
		);
		assert_eq!(
			DESCRIPTOR.project_skill_read_paths(root).first(),
			(DESCRIPTOR.project_skill_paths.unwrap().write)(root).as_ref()
		);

		// Co-readership of the shared slot is the load-bearing half: dsh joins
		// the `compat_unlink_authorized` reader quorum at both scopes. The
		// global side is pinned against the RESOLVED root, not the literal
		// `.agents/skills` — an ambient `$DSH_AGENTS_HOME` legitimately moves
		// it, and this test has no `default_env()` to clear one.
		assert!(
			DESCRIPTOR
				.global_skill_read_paths()
				.contains(&agents.join("skills")),
			"dsh reads the shared global slot"
		);
		assert!(
			DESCRIPTOR
				.project_skill_read_paths(root)
				.iter()
				.any(|path| path.ends_with(".agents/skills")),
			"dsh reads the shared project slot"
		);
		// The XDG dir is in NEITHER list — that is what `universal: false`
		// buys, and it is the one thing the flag alone cannot show.
		for paths in [
			DESCRIPTOR.global_skill_read_paths(),
			DESCRIPTOR.project_skill_read_paths(root),
		] {
			assert!(
				!paths
					.iter()
					.any(|path| path.ends_with(".config/agents/skills")),
				"`$DSH_AGENTS_HOME` is ~/.agents, never the XDG dir: {paths:?}"
			);
		}
	}
}
