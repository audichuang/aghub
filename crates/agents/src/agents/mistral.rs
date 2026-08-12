use crate::define_skill_paths;
use crate::descriptor::*;
use crate::format::toml_mistral;
use std::path::{Path, PathBuf};

fn resolve_vibe_home(
	vibe_home: Option<std::ffi::OsString>,
	home: Option<PathBuf>,
) -> Option<PathBuf> {
	vibe_home
		.filter(|value| !value.is_empty())
		.map(PathBuf::from)
		.or_else(|| home.map(|home| home.join(".vibe")))
}

fn vibe_home() -> Option<PathBuf> {
	resolve_vibe_home(std::env::var_os("VIBE_HOME"), home_dir())
}

fn mcp_global_path() -> Option<PathBuf> {
	vibe_home().map(|home| home.join("config.toml"))
}

fn mcp_project_path(root: &Path) -> Option<PathBuf> {
	Some(root.join(".vibe/config.toml"))
}

fn global_data_dir() -> Option<PathBuf> {
	vibe_home()
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
		toml_mistral::parse,
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
		toml_mistral::serialize,
	)
}

define_skill_paths! {
	symmetric: ".vibe/skills",
}

pub const DESCRIPTOR: AgentDescriptor = AgentDescriptor {
	id: "mistral",
	display_name: "Mistral Le Chat",
	mcp_parse_config: Some(toml_mistral::parse),
	mcp_serialize_config: Some(toml_mistral::serialize),
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
	cli_name: "mistral",
	validate_args: &["--version"],
	project_markers: &[".vibe"],
	skills_cli_name: Some("mistral-vibe"),
};

#[cfg(test)]
mod tests {
	use super::*;

	// Compile-time pin: Vibe has a native `disabled` field.
	const _: () = assert!(DESCRIPTOR.capabilities.mcp.enable_disable);
	use std::path::Path;

	#[test]
	fn mcp_paths_use_vibe_config_toml() {
		let runtime_vibe_home =
			resolve_vibe_home(std::env::var_os("VIBE_HOME"), home_dir());
		assert_eq!(
			mcp_global_path(),
			runtime_vibe_home
				.as_ref()
				.map(|home| home.join("config.toml"))
		);
		assert_eq!(global_data_dir(), runtime_vibe_home);
		assert_eq!(
			resolve_vibe_home(
				Some("/custom/vibe".into()),
				Some(Path::new("/home/person").to_path_buf())
			)
			.unwrap()
			.join("config.toml"),
			Path::new("/custom/vibe/config.toml")
		);
		assert_eq!(
			resolve_vibe_home(
				None,
				Some(Path::new("/home/person").to_path_buf())
			)
			.unwrap()
			.join("config.toml"),
			Path::new("/home/person/.vibe/config.toml")
		);
		assert_eq!(
			mcp_project_path(Path::new("/workspace")),
			Some(Path::new("/workspace/.vibe/config.toml").to_path_buf())
		);
	}

	#[test]
	fn descriptor_uses_vibe_native_mcp_format_and_toggle() {
		let parse = DESCRIPTOR.mcp_parse_config.unwrap();
		let config = parse(
			r#"
[[mcp_servers]]
name = "native"
transport = "stdio"
command = "uvx"
disabled = true
"#,
		)
		.unwrap();
		assert_eq!(config.mcps.len(), 1);
		assert!(!config.mcps[0].enabled);
	}
}
