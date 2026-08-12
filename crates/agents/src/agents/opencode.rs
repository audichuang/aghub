use crate::descriptor::*;
use crate::sub_agents::{load_scoped_sub_agents, save_scoped_sub_agents};
use std::path::{Path, PathBuf};

fn config_dir_from(
	opencode_config_dir: Option<PathBuf>,
	xdg_config_home: Option<PathBuf>,
	home: Option<PathBuf>,
) -> Option<PathBuf> {
	opencode_config_dir
		.or_else(|| xdg_config_home.map(|config| config.join("opencode")))
		.or_else(|| home.map(|home| home.join(".config/opencode")))
}

fn config_dir() -> Option<PathBuf> {
	config_dir_from(
		std::env::var_os("OPENCODE_CONFIG_DIR")
			.filter(|value| !value.is_empty())
			.map(PathBuf::from),
		std::env::var_os("XDG_CONFIG_HOME")
			.filter(|value| !value.is_empty())
			.map(PathBuf::from),
		home_dir(),
	)
}

fn mcp_global_path_from(
	opencode_config: Option<PathBuf>,
	config_dir: Option<PathBuf>,
) -> Option<PathBuf> {
	opencode_config.or_else(|| {
		config_dir.map(|config| {
			existing_or_default(&config, &["opencode.json", "opencode.jsonc"])
		})
	})
}

fn existing_or_default(root: &Path, candidates: &[&str]) -> PathBuf {
	candidates
		.iter()
		.map(|path| root.join(path))
		.find(|path| path.is_file())
		.unwrap_or_else(|| root.join(candidates[0]))
}
fn mcp_global_path() -> Option<PathBuf> {
	mcp_global_path_from(
		std::env::var_os("OPENCODE_CONFIG")
			.filter(|value| !value.is_empty())
			.map(PathBuf::from),
		config_dir(),
	)
}
fn mcp_project_path(root: &Path) -> Option<PathBuf> {
	Some(existing_or_default(
		root,
		&[
			"opencode.json",
			"opencode.jsonc",
			".opencode/opencode.json",
			".opencode/opencode.jsonc",
		],
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
		mcp_strategy::PARSE_JSON_OPCODE,
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
		mcp_strategy::SERIALIZE_JSON_OPCODE,
	)
}
// npx-`skills` layout: OpenCode owns ONLY its own per-agent dir (symlink
// Referrers) plus the universal `.agents/skills` Master. Reading Claude's
// private `.claude/skills` made OpenCode misattribute universal skills through
// Claude's Referrer symlinks (source_path landing in `.claude/skills`). Mapping
// mirrors upstream `agents.ts` (opencode → project `.agents/skills`, global
// `~/.config/opencode/skills`); the global Master is `~/.agents/skills` per the
// npx interop contract.
fn global_skills_paths() -> Vec<PathBuf> {
	let Some(home) = home_dir() else {
		return Vec::new();
	};
	vec![
		config_dir()
			.unwrap_or_else(|| home.join(".config/opencode"))
			.join("skills"),
		home.join(".agents/skills"),
	]
}
fn project_skills_paths(root: &Path) -> Vec<PathBuf> {
	vec![root.join(".opencode/skills"), root.join(".agents/skills")]
}

fn global_skill_write_path() -> Option<PathBuf> {
	config_dir().map(|config| config.join("skills"))
}

fn project_skill_write_path(root: &Path) -> Option<PathBuf> {
	Some(root.join(".opencode/skills"))
}

fn sub_agent_global_dir() -> Option<PathBuf> {
	config_dir().map(|config| config.join("agents"))
}

fn sub_agent_project_dir(root: &Path) -> Option<PathBuf> {
	Some(root.join(".opencode/agents"))
}

fn load_sub_agents(
	project_root: Option<&Path>,
	scope: crate::ResourceScope,
) -> crate::Result<Vec<crate::SubAgent>> {
	load_scoped_sub_agents(
		project_root,
		scope,
		Some(sub_agent_global_dir),
		Some(sub_agent_project_dir),
	)
}

fn save_sub_agents(
	project_root: Option<&Path>,
	scope: crate::ResourceScope,
	agents: &[crate::SubAgent],
) -> crate::Result<()> {
	save_scoped_sub_agents(
		project_root,
		scope,
		agents,
		Some(sub_agent_global_dir),
		Some(sub_agent_project_dir),
	)
}

pub const DESCRIPTOR: AgentDescriptor = AgentDescriptor {
	id: "opencode",
	display_name: "OpenCode",
	mcp_parse_config: Some(mcp_strategy::PARSE_JSON_OPCODE),
	mcp_serialize_config: Some(mcp_strategy::SERIALIZE_JSON_OPCODE),
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
	cli_name: "opencode",
	validate_args: &["--version"],
	project_markers: &["opencode.json", "opencode.jsonc", ".opencode"],
	skills_cli_name: Some("opencode"),
};

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn global_config_overrides_are_respected() {
		assert_eq!(
			mcp_global_path_from(
				Some(PathBuf::from("/tmp/custom.json")),
				Some(PathBuf::from("/tmp/config")),
			),
			Some(PathBuf::from("/tmp/custom.json"))
		);
		assert_eq!(
			config_dir_from(
				Some(PathBuf::from("/tmp/custom-dir")),
				Some(PathBuf::from("/tmp/xdg")),
				Some(PathBuf::from("/home/user")),
			),
			Some(PathBuf::from("/tmp/custom-dir"))
		);
	}
}
