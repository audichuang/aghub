use crate::descriptor::*;
use std::path::PathBuf;

// Hermes home: `~/.hermes` on POSIX/WSL2, `%LOCALAPPDATA%\hermes` on native
// Windows. Both arms are compiled on every platform (cfg blocks inside one fn)
// so there is no unused-fn / Windows-clippy gap.
fn resolve_hermes_home(
	override_dir: Option<PathBuf>,
	default_dir: Option<PathBuf>,
) -> Option<PathBuf> {
	override_dir
		.filter(|path| !path.as_os_str().is_empty())
		.or(default_dir)
}

fn hermes_home() -> Option<PathBuf> {
	let override_dir = std::env::var_os("HERMES_HOME").map(PathBuf::from);
	#[cfg(windows)]
	{
		resolve_hermes_home(
			override_dir,
			dirs::data_local_dir().map(|d| d.join("hermes")),
		)
	}
	#[cfg(not(windows))]
	{
		resolve_hermes_home(override_dir, home_dir().map(|h| h.join(".hermes")))
	}
}

fn mcp_global_path() -> Option<PathBuf> {
	hermes_home().map(|h| h.join("config.yaml"))
}

fn global_data_dir() -> Option<PathBuf> {
	hermes_home()
}

fn global_skills_write() -> Option<PathBuf> {
	hermes_home().map(|h| h.join("skills"))
}

pub const DESCRIPTOR: AgentDescriptor = AgentDescriptor {
	id: "hermes",
	agent_type: crate::AgentType::Hermes,
	display_name: "Hermes",
	mcp_parse_config: Some(mcp_strategy::parse_yaml_hermes_mcp_servers),
	mcp_serialize_config: Some(mcp_strategy::serialize_yaml_hermes_mcp_servers),
	mcp_global_path: Some(mcp_global_path),
	mcp_project_path: None,
	global_data_dir,
	capabilities: Capabilities {
		skills: SkillCapabilities {
			scopes: ScopeSupport {
				global: true,
				project: false,
			},
			universal: false,
		},
		mcp: McpCapabilities {
			scopes: ScopeSupport {
				global: true,
				project: false,
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
		write: global_skills_write,
		also_reads: None,
	}),
	project_skill_paths: None,
	load_sub_agents: load_sub_agents_noop,
	save_sub_agents: save_sub_agents_noop,
	sub_agent_global_dir: None,
	sub_agent_project_dir: None,
	cli_name: "hermes",
	validate_args: &["--version"],
	project_markers: &[],
	skills_cli_name: None,
};

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn hermes_home_override_wins() {
		assert_eq!(
			resolve_hermes_home(
				Some(PathBuf::from("/custom/hermes")),
				Some(PathBuf::from("/home/user/.hermes")),
			),
			Some(PathBuf::from("/custom/hermes"))
		);
	}
}
