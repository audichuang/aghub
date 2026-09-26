use crate::descriptor::*;
use crate::errors::ConfigError;
use std::path::{Path, PathBuf};

// DeepSeek Harness — repo `deepseek-ai/deepseek-harness`, npm package
// `@deepseek-ai/dsh`, binary `dsh`. NOT `lessweb/deepcode-cli` ("Deep Code",
// `./.deepcode/skills`) and NOT DeepSeek-TUI (`~/.deepseek/mcp.json`); both are
// different products with different layouts.
//
// SKILL DISCOVERY ROOTS, from the upstream README's table. Lower rank wins a
// duplicate name:
//
//   100  project-dsh     <projectRoot>/.dsh/skills
//   200  project-agents  <projectRoot>/.agents/skills
//   400  user-dsh        <dshHome>/skills
//   500  user-agents     <agentsHome>/skills
//
// The PRIVATE `.dsh` slot outranks the shared `.agents` one at BOTH scopes, so
// a grant written there wins discovery — which is also aghub's own convention
// (`load_skills_from_dirs` is first-dir-wins and the winner becomes
// `source_path`, the path `remove_skill` deletes and `check` hashes). Write dir
// first in every list below.
//
// `universal: false` ON PURPOSE. dsh's shared root is `$DSH_AGENTS_HOME`,
// default `~/.agents` — NOT `$XDG_CONFIG_HOME/agents`, which is what the
// `universal` flag appends. Source: `packages/skill/skill-filesystem/src/index.ts`
// L168, `agentsHome = $DSH_AGENTS_HOME ?? ~/.agents`. The shared slot is spelled
// out in the read lists instead, so dsh joins `.agents/skills` co-readership
// without joining the XDG group.
//
// SKILL SHAPE: a `<name>/SKILL.md` directory bundle, or a flat `<name>.md` at
// the top level of a root. Nested `**/SKILL.md` is deliberately not discovered.
// The frontmatter `name:` decides the skill's name — the directory name is only
// the discovery key and a mismatch is silently tolerated. Names must match
// `^[a-z0-9]+(?:-[a-z0-9]+)*$`; anything else is dropped with a warning.
// Required frontmatter is `name` + `description`; `whenToUse`, `metadata`,
// `disable-model-invocation` and `user-invocable` are optional. No documented
// description-length or body-size limit.
//
// SYMLINKS are followed, and the shape is exactly aghub's Master/Referrer
// model: upstream's own test stages a symlinked directory and a symlinked flat
// `.md` under `~/.dsh/skills`, loads both, and reports the realpathed target as
// `path` while `resourceBase.path` stays the LINK. Broken links and `/dev/null`
// links are ignored. So the symlink-only install needs nothing special here.
//
// `.system` is reserved under the GLOBAL dsh root only — dsh skips that child.
// `skill::sanitize_name` strips leading dots (`trim_start_matches(['.', '-'])`),
// so a skill called `system` — or `.system` — lands at
// `<dshHome>/skills/system` and cannot shadow the reserved child. There is
// nothing to guard here, only something not to "fix" later by letting a
// sanitized name keep its leading dot.
//
// PER-SKILL ENABLE/DISABLE is frontmatter-only (`disable-model-invocation`,
// `user-invocable`) with no external state file. A dsh "disable" therefore edits
// the shared Master and is NOT per-agent — and it dirties the comparison hash
// `check` / `source diff` use, so the skill reports `update-available` until it
// is pushed. aghub models no skill toggle, so this is a note, not a capability.
//
// MCP: deliberately NOT supported (`capabilities.mcp` all false). Reasons, so
// nobody re-opens it by accident:
//
//   * There is no `mcpServers` map and no `mcp.json`. An MCP server is a plugin
//     row in a Cordis YAML COMPOSITION LIST —
//     `~/.dsh/profiles/<profile>/cordis.patch.yml` plus a profile-independent
//     `$DSH_HOME/cordis.patch.yml`.
//   * User scope only (no project-level MCP file is documented anywhere) and
//     PER PROFILE, so a server added to `web` is invisible to `headless`.
//     aghub's scope model has nowhere to put that.
//   * Transports are `stdio` and `streamable-http` ONLY. There is no `sse`, so
//     `McpTransport::Sse` would have to be refused.
//   * The official examples interpolate env/headers with the non-standard `!!js`
//     YAML tag (`GITHUB_TOKEN: !!js process.env.GITHUB_TOKEN`). A plain YAML
//     serializer cannot round-trip that, so a rewrite would destroy a user's
//     config — and root AGENTS.md is explicit that a value the model cannot hold
//     must be refused, not approximated.
//   * Adding a row needs the `- insert:` verb; a bare `- id:` row is the
//     OVERRIDE verb and silently no-ops (warning only) when the id matches
//     nothing.
//
// Supporting that needs its own format module and its own round-trip policy.
//
// SUB-AGENTS: not researched, so off at both scopes with the no-op I/O, rather
// than guessing a path aghub would write into and dsh would never read.
//
// PROJECT ROOT: dsh defines it as the nearest ancestor holding `.git`, falling
// back to cwd. aghub walks for agent markers and root AGENTS.md is explicit
// that `.git` alone is not enough. Where the two disagree, dsh scans
// `<gitRoot>/.dsh/skills` while aghub writes `<aghubRoot>/.dsh/skills`. `.dsh`
// is a marker below so an aghub-managed project agrees with dsh once a grant
// exists; aghub's root detection is deliberately left alone.

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

fn load_mcps(
	_: Option<&Path>,
	_: crate::ResourceScope,
) -> crate::Result<Vec<crate::McpServer>> {
	Ok(Vec::new())
}

fn save_mcps(
	_: Option<&Path>,
	_: crate::ResourceScope,
	_: &[crate::McpServer],
) -> crate::Result<()> {
	Err(ConfigError::unsupported_operation(
		"persist",
		"MCP server",
		"dsh",
	))
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
	display_name: "DeepSeek Harness",
	mcp_parse_config: None,
	mcp_serialize_config: None,
	load_mcps,
	save_mcps,
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

	/// MCP is refused, not silently dropped: a save that returned `Ok` would let
	/// a multi-agent batch report a server aghub never wrote anywhere.
	#[test]
	fn dsh_refuses_to_persist_mcp_servers() {
		assert!(load_mcps(None, crate::ResourceScope::GlobalOnly)
			.expect("reading MCP servers is an empty list, not an error")
			.is_empty());
		let error = save_mcps(None, crate::ResourceScope::GlobalOnly, &[])
			.expect_err("dsh has no writable MCP config");
		assert!(
			error.to_string().contains("dsh"),
			"the refusal must name the agent: {error}"
		);
	}
}
