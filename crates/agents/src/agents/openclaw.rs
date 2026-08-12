use crate::descriptor::*;
use crate::format::json_openclaw;
use std::path::{Path, PathBuf};

fn resolve_mcp_global_path(
	config_path: Option<PathBuf>,
	state_dir: Option<PathBuf>,
	home: Option<PathBuf>,
) -> Option<PathBuf> {
	if let Some(path) = config_path {
		return Some(expand_home(path, home.as_deref()));
	}
	if let Some(path) = state_dir {
		return Some(expand_home(path, home.as_deref()).join("openclaw.json"));
	}
	home.map(|home| home.join(".openclaw/openclaw.json"))
}

fn expand_home(path: PathBuf, home: Option<&Path>) -> PathBuf {
	let Some(home) = home else {
		return path;
	};
	match path.strip_prefix("~") {
		Ok(rest) => home.join(rest),
		Err(_) => path,
	}
}

fn env_path(name: &str) -> Option<PathBuf> {
	std::env::var(name)
		.ok()
		.map(|value| value.trim().to_string())
		.filter(|value| !value.is_empty())
		.map(PathBuf::from)
}

fn mcp_global_path() -> Option<PathBuf> {
	resolve_mcp_global_path(
		env_path("OPENCLAW_CONFIG_PATH"),
		env_path("OPENCLAW_STATE_DIR"),
		home_dir(),
	)
}
fn global_data_dir() -> Option<PathBuf> {
	env_path("OPENCLAW_STATE_DIR")
		.map(|path| expand_home(path, home_dir().as_deref()))
		.or_else(|| home_dir().map(|home| home.join(".openclaw")))
}
fn load_mcps(
	project_root: Option<&Path>,
	scope: crate::ResourceScope,
) -> crate::Result<Vec<crate::McpServer>> {
	load_scoped_mcps(
		project_root,
		scope,
		Some(mcp_global_path),
		None,
		json_openclaw::parse,
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
		None,
		json_openclaw::serialize,
	)
}

/// Return the global skills directories for OpenClaw, checking fallback dirs.
///
/// Priority: `.openclaw` → `.clawdbot` → `.moltbot`, defaulting to `.openclaw`.
/// The `exists` parameter allows dependency injection for testing.
pub fn get_openclaw_skills_dirs(
	home: &Path,
	exists: impl Fn(&Path) -> bool,
) -> Vec<PathBuf> {
	for dir in [".openclaw", ".clawdbot", ".moltbot"] {
		if exists(&home.join(dir)) {
			return vec![home.join(dir).join("skills")];
		}
	}
	vec![home.join(".openclaw/skills")]
}

fn global_skills_paths() -> Vec<PathBuf> {
	let Some(home) = home_dir() else {
		return Vec::new();
	};
	let mut paths = get_openclaw_skills_dirs(&home, |p| p.exists());

	// Dynamic discovery: which openclaw → canonicalize → parent/skills
	// This allows finding skills from npm global installation or other symlinked locations
	if let Ok(cli_path) = which::which("openclaw") {
		if let Ok(real_path) = cli_path.canonicalize() {
			// real_path might be: /opt/homebrew/lib/node_modules/openclaw/openclaw.mjs
			// skills dir should be: /opt/homebrew/lib/node_modules/openclaw/skills/
			if let Some(parent) = real_path.parent() {
				let npm_skills_dir = parent.join("skills");
				if npm_skills_dir.exists() {
					paths.push(npm_skills_dir);
				}
			}
		}
	}

	paths
}
fn global_skill_write_path() -> Option<PathBuf> {
	home_dir().map(|home| {
		get_openclaw_skills_dirs(&home, |p| p.exists())
			.into_iter()
			.next()
			.unwrap_or_else(|| home.join(".openclaw/skills"))
	})
}

pub const DESCRIPTOR: AgentDescriptor = AgentDescriptor {
	id: "openclaw",
	display_name: "OpenClaw",
	mcp_parse_config: Some(json_openclaw::parse),
	mcp_serialize_config: Some(json_openclaw::serialize),
	load_mcps,
	save_mcps,
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
		read: global_skills_paths,
		write: global_skill_write_path,
	}),
	project_skill_paths: None,
	load_sub_agents: load_sub_agents_noop,
	save_sub_agents: save_sub_agents_noop,
	cli_name: "openclaw",
	validate_args: &["--version"],
	project_markers: &[".openclaw"],
	skills_cli_name: Some("openclaw"),
};

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn mcp_path_defaults_to_openclaw_config() {
		assert_eq!(
			resolve_mcp_global_path(
				None,
				None,
				Some(PathBuf::from("/home/user")),
			),
			Some(PathBuf::from("/home/user/.openclaw/openclaw.json"))
		);
	}

	#[test]
	fn mcp_path_honors_config_then_state_dir_overrides() {
		let home = Some(PathBuf::from("/home/user"));
		assert_eq!(
			resolve_mcp_global_path(
				Some(PathBuf::from("/custom/openclaw.json")),
				Some(PathBuf::from("/state")),
				home.clone(),
			),
			Some(PathBuf::from("/custom/openclaw.json"))
		);
		assert_eq!(
			resolve_mcp_global_path(None, Some(PathBuf::from("/state")), home,),
			Some(PathBuf::from("/state/openclaw.json"))
		);
	}

	#[test]
	fn descriptor_uses_native_mcp_shape_and_enabled_toggle() {
		let parse = DESCRIPTOR.mcp_parse_config.unwrap();
		let config = parse(
			r#"{
				"mcp": {
					"servers": {
						"notebooklm": {
							"transport": "stdio",
							"command": "nblm-mcp",
							"enabled": false
						}
					}
				}
			}"#,
		)
		.unwrap();

		assert_eq!(config.mcps.len(), 1);
		assert!(!config.mcps[0].enabled);
		let descriptor = &DESCRIPTOR;
		assert!(descriptor.capabilities.mcp.enable_disable);
	}
}
