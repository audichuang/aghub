//! Agent descriptor regression tests.
//!
//! These tests hard-code the expected behavior from main branch to prevent
//! regression when refactoring agent descriptor files.
//!
//! Expected values are extracted from main branch actual descriptor definitions.

use aghub_agents::{
	agents, AgentDescriptor, AgentType, McpServer, McpTransport, ResourceScope,
};
use std::path::PathBuf;

/// Helper to get home directory for path assertions
fn home() -> PathBuf {
	dirs::home_dir().expect("home dir should exist")
}

/// Documented overrides that move an agent's global config off its default
/// path. The tables below pin the DEFAULTS, so a test that reads a global path
/// must clear these first — otherwise the suite goes red on any machine that
/// happens to export one, and a bare `XDG_CONFIG_HOME` is enough to do it.
///
/// The list itself is `aghub_agents::PATH_OVERRIDE_VARS` — every harness that
/// isolates a run shares that one const rather than hand-copying it.
const PATH_OVERRIDES: &[&str] = aghub_agents::PATH_OVERRIDE_VARS;

/// The const above is only as good as its completeness, and nothing in the type
/// system ties it to the descriptors that read the variables — so read the
/// descriptor sources and check. A new agent honouring a new `FOO_HOME` fails
/// HERE, instead of silently letting some future harness write into the
/// developer's real config (which is how the last three leaks happened).
#[test]
fn path_override_vars_covers_every_descriptor_read() {
	let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
	let mut missing: Vec<(String, String)> = Vec::new();
	let mut stack = vec![dir];
	while let Some(dir) = stack.pop() {
		for entry in std::fs::read_dir(&dir).expect("read src dir") {
			let path = entry.expect("dir entry").path();
			if path.is_dir() {
				stack.push(path);
				continue;
			}
			if path.extension().and_then(|e| e.to_str()) != Some("rs") {
				continue;
			}
			let src = std::fs::read_to_string(&path).expect("read source");
			for var in env_vars_read(&src) {
				// $HOME and the XDG bases are the platform's own, isolated by
				// every harness on their own terms (one is SET, not cleared).
				if matches!(
					var.as_str(),
					"HOME" | "XDG_DATA_HOME" | "XDG_STATE_HOME" | "APPDATA"
				) {
					continue;
				}
				if !PATH_OVERRIDES.contains(&var.as_str()) {
					missing.push((
						path.file_name()
							.unwrap()
							.to_string_lossy()
							.into_owned(),
						var,
					));
				}
			}
		}
	}
	assert!(
		missing.is_empty(),
		"these descriptor env reads are not in aghub_agents::PATH_OVERRIDE_VARS, \
		 so a harness clearing that list still leaks the developer's real \
		 config: {missing:?}"
	);
}

/// Pull `"FOO"` out of `env::var("FOO")`, `env::var_os("FOO")` and the
/// `env_path("FOO")` wrapper. Deliberately literal-only — a variable read
/// through a runtime-computed name could not be pinned by a const list anyway.
#[cfg(test)]
fn env_vars_read(src: &str) -> Vec<String> {
	let mut out = Vec::new();
	for opener in ["env::var(\"", "env::var_os(\"", "env_path(\""] {
		let mut rest = src;
		while let Some(at) = rest.find(opener) {
			rest = &rest[at + opener.len()..];
			if let Some(end) = rest.find('"') {
				let name = &rest[..end];
				if name.chars().all(|c| c.is_ascii_uppercase() || c == '_')
					&& !name.is_empty()
				{
					out.push(name.to_string());
				}
			}
		}
	}
	out
}
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct DefaultEnv {
	_guard: std::sync::MutexGuard<'static, ()>,
	saved: Vec<(&'static str, Option<std::ffi::OsString>)>,
}

impl Drop for DefaultEnv {
	fn drop(&mut self) {
		for (key, value) in &self.saved {
			match value {
				Some(value) => std::env::set_var(key, value),
				None => std::env::remove_var(key),
			}
		}
	}
}

/// Resolve global paths as if no override were set. Serialised against every
/// other test that does the same.
fn default_env() -> DefaultEnv {
	let guard = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
	let saved = PATH_OVERRIDES
		.iter()
		.map(|key| (*key, std::env::var_os(key)))
		.collect::<Vec<_>>();
	for (key, _) in &saved {
		std::env::remove_var(key);
	}
	DefaultEnv {
		_guard: guard,
		saved,
	}
}

/// Hermes lives under `%LOCALAPPDATA%` on Windows, not in a home dotfolder —
/// mirrors `agents::hermes::hermes_home`. Asserting `~/.hermes` unconditionally
/// only goes red on the Windows CI leg, which `just preflight` cannot reach.
fn hermes_home() -> Option<PathBuf> {
	#[cfg(windows)]
	{
		dirs::data_local_dir().map(|dir| dir.join("hermes"))
	}
	#[cfg(not(windows))]
	{
		dirs::home_dir().map(|home| home.join(".hermes"))
	}
}

fn zed_config_dir() -> Option<PathBuf> {
	#[cfg(target_os = "macos")]
	{
		Some(home().join(".config/zed"))
	}
	#[cfg(target_os = "windows")]
	{
		dirs::config_dir().map(|dir| dir.join("Zed"))
	}
	#[cfg(not(any(target_os = "macos", target_os = "windows")))]
	{
		dirs::config_dir().map(|dir| dir.join("zed"))
	}
}

/// Every shipped descriptor, paired with the `AgentType` its own id names.
///
/// DERIVED from `agents::ALL_DESCRIPTORS`, never hand-listed: a fourth
/// hand-written roster is how "add an agent" turned into "update three lists
/// and hope", and a matrix that carried its own copy stayed green while the
/// registry was missing the agent entirely.
fn all_descriptors() -> Vec<(AgentType, &'static AgentDescriptor)> {
	agents::ALL_DESCRIPTORS
		.iter()
		.map(|descriptor| {
			let agent: AgentType = descriptor.id.parse().unwrap_or_else(|_| {
				panic!("descriptor id '{}' is not an AgentType", descriptor.id)
			});
			(agent, *descriptor)
		})
		.collect()
}

#[test]
fn descriptor_matrix_covers_every_agent_type() {
	let descriptors = all_descriptors();
	let actual: Vec<_> = descriptors.iter().map(|(agent, _)| *agent).collect();
	assert_eq!(actual.len(), AgentType::ALL.len());
	for agent in AgentType::ALL {
		assert!(actual.contains(agent), "missing descriptor for {agent:?}");
	}
	for (agent, descriptor) in descriptors {
		assert_eq!(descriptor.id, agent.as_str());
	}
}

// =============================================================================
// CLI Name Tests (from main branch actual values)
// =============================================================================

#[test]
fn test_cli_names() {
	// Expected values from main branch descriptor files
	let expected: [(AgentType, &str); 26] = [
		(AgentType::Claude, "claude"),
		(AgentType::Codex, "codex"),
		(AgentType::Openclaw, "openclaw"),
		(AgentType::OpenCode, "opencode"),
		(AgentType::Gemini, "gemini"),
		(AgentType::Cline, "cline"),
		(AgentType::Copilot, "copilot"),
		(AgentType::Cursor, "cursor"),
		(AgentType::Antigravity, "antigravity"),
		(AgentType::Kiro, "kiro"),
		(AgentType::Windsurf, "windsurf"),
		(AgentType::Trae, "trae"),
		(AgentType::Zed, "zed"),
		(AgentType::JetBrainsAi, "jetbrains"),
		(AgentType::RooCode, "roocode"),
		(AgentType::Kimi, "kimi"),
		(AgentType::Mistral, "mistral"),
		(AgentType::Pi, "pi"),
		(AgentType::AugmentCode, "augmentcode"),
		(AgentType::KiloCode, "kilocode"), // main branch: "kilocode"
		(AgentType::Amp, "amp"),
		(AgentType::Warp, "warp"),
		(AgentType::Factory, "factory"),
		(AgentType::Hermes, "hermes"),
		(AgentType::Grok, "grok"),
		(AgentType::Omp, "omp"),
	];

	for (agent_type, desc) in all_descriptors() {
		if let Some((_, name)) = expected.iter().find(|(t, _)| *t == agent_type)
		{
			assert_eq!(
				desc.cli_name, *name,
				"cli_name mismatch for {:?}",
				agent_type
			);
		}
	}
}

// =============================================================================
// Skills CLI Name Tests (from main branch actual values)
// =============================================================================

#[test]
fn test_skills_cli_names() {
	let expected: [(AgentType, Option<&str>); 26] = [
		(AgentType::Claude, Some("claude-code")), // main branch: "claude-code"
		(AgentType::Codex, Some("codex")),
		(AgentType::Openclaw, Some("openclaw")),
		(AgentType::OpenCode, Some("opencode")),
		(AgentType::Gemini, Some("gemini-cli")),
		(AgentType::Cline, Some("cline")),
		(AgentType::Copilot, Some("github-copilot")),
		(AgentType::Cursor, Some("cursor")),
		(AgentType::Antigravity, Some("antigravity")),
		(AgentType::Kiro, Some("kiro-cli")),
		(AgentType::Windsurf, Some("windsurf")),
		(AgentType::Trae, Some("trae")),
		(AgentType::Zed, None),
		(AgentType::JetBrainsAi, None),
		(AgentType::RooCode, Some("roo")),
		(AgentType::Kimi, Some("kimi-cli")),
		(AgentType::Mistral, Some("mistral-vibe")),
		(AgentType::Pi, Some("pi")),
		(AgentType::AugmentCode, Some("augment")),
		(AgentType::KiloCode, Some("kilo")), // main branch: "kilo"
		(AgentType::Amp, Some("amp")),
		(AgentType::Warp, Some("warp")),
		(AgentType::Factory, Some("factory")),
		(AgentType::Hermes, None),
		(AgentType::Grok, Some("grok")),
		(AgentType::Omp, None),
	];

	for (agent_type, desc) in all_descriptors() {
		if let Some((_, name)) = expected.iter().find(|(t, _)| *t == agent_type)
		{
			assert_eq!(
				desc.skills_cli_name, *name,
				"skills_cli_name mismatch for {:?}",
				agent_type
			);
		}
	}
}

// =============================================================================
// Display Name Tests (from main branch actual values)
// =============================================================================

#[test]
fn test_display_names() {
	let expected: [(AgentType, &str); 26] = [
		(AgentType::Claude, "Claude Code"), // main branch: "Claude Code"
		(AgentType::Codex, "OpenAI Codex"),
		(AgentType::Openclaw, "OpenClaw"),
		(AgentType::OpenCode, "OpenCode"),
		(AgentType::Gemini, "Gemini CLI"),
		(AgentType::Cline, "Cline"),
		(AgentType::Copilot, "GitHub Copilot"),
		(AgentType::Cursor, "Cursor"),
		(AgentType::Antigravity, "Antigravity"),
		(AgentType::Kiro, "Kiro"),
		(AgentType::Windsurf, "Windsurf"),
		(AgentType::Trae, "Trae"),
		(AgentType::Zed, "Zed"),
		(AgentType::JetBrainsAi, "JetBrains AI"),
		(AgentType::RooCode, "RooCode"),
		(AgentType::Kimi, "Kimi Code CLI"),
		(AgentType::Mistral, "Mistral Le Chat"),
		(AgentType::Pi, "Pi Coding Agent"),
		(AgentType::AugmentCode, "AugmentCode"),
		(AgentType::KiloCode, "KiloCode"),
		(AgentType::Amp, "Amp"),
		(AgentType::Warp, "Warp"),
		(AgentType::Factory, "Factory"),
		(AgentType::Hermes, "Hermes"),
		(AgentType::Grok, "Grok"),
		(AgentType::Omp, "Oh My Pi"),
	];

	for (agent_type, desc) in all_descriptors() {
		if let Some((_, name)) = expected.iter().find(|(t, _)| *t == agent_type)
		{
			assert_eq!(
				desc.display_name, *name,
				"display_name mismatch for {:?}",
				agent_type
			);
		}
	}
}

// =============================================================================
// Project Markers Tests (from main branch actual values)
// =============================================================================

#[test]
fn test_project_markers() {
	let expected: [(AgentType, &[&str]); 26] = [
		(AgentType::Claude, &[".claude", ".mcp.json"]), // main branch has both
		(AgentType::Codex, &[".codex"]),
		(AgentType::Openclaw, &[".openclaw"]),
		(
			AgentType::OpenCode,
			&["opencode.json", "opencode.jsonc", ".opencode"],
		),
		(AgentType::Gemini, &[".gemini"]),
		(AgentType::Cline, &[".cline"]),
		(AgentType::Copilot, &[".mcp.json", ".github"]),
		(AgentType::Cursor, &[".cursor"]),
		(AgentType::Antigravity, &[".agents/mcp_config.json"]),
		(AgentType::Kiro, &[".kiro"]),
		(AgentType::Windsurf, &[".windsurf"]),
		(AgentType::Trae, &[".trae"]),
		(AgentType::Zed, &[".zed"]),
		(AgentType::JetBrainsAi, &[]),
		(AgentType::RooCode, &[".roo"]),
		(AgentType::Kimi, &[".kimi"]),
		(AgentType::Mistral, &[".vibe"]),
		(AgentType::Pi, &[".pi"]),
		(AgentType::AugmentCode, &[".augment"]),
		(
			AgentType::KiloCode,
			&["kilo.json", "kilo.jsonc", ".kilo", ".kilocode"],
		),
		(AgentType::Amp, &[".amp"]),
		(AgentType::Warp, &[".warp"]),
		(AgentType::Factory, &[".factory"]),
		(AgentType::Hermes, &[]),
		(AgentType::Grok, &[".grok"]),
		(AgentType::Omp, &[".omp"]),
	];

	for (agent_type, desc) in all_descriptors() {
		if let Some((_, markers)) =
			expected.iter().find(|(t, _)| *t == agent_type)
		{
			assert_eq!(
				desc.project_markers, *markers,
				"project_markers mismatch for {:?}",
				agent_type
			);
		}
	}
}

// =============================================================================
// MCP Global Path Contract
// =============================================================================

#[test]
fn test_mcp_global_paths() {
	let _env = default_env();
	let expected: [(AgentType, Option<&str>); 26] = [
		(AgentType::Claude, Some(".claude.json")),
		(AgentType::Codex, Some(".codex/config.toml")),
		(AgentType::Openclaw, Some(".openclaw/openclaw.json")),
		(AgentType::OpenCode, Some(".config/opencode/opencode.json")),
		(AgentType::Gemini, Some(".gemini/settings.json")),
		(
			AgentType::Cline,
			Some(".cline/data/settings/cline_mcp_settings.json"),
		),
		(AgentType::Copilot, Some(".copilot/mcp-config.json")),
		(AgentType::Cursor, Some(".cursor/mcp.json")),
		(
			AgentType::Antigravity,
			Some(".gemini/config/mcp_config.json"),
		),
		(AgentType::Kiro, Some(".kiro/settings/mcp.json")),
		(
			AgentType::Windsurf,
			Some(".codeium/windsurf/mcp_config.json"),
		),
		(AgentType::Trae, None), // global MCP is GUI-managed (no file)
		(AgentType::Zed, Some(".config/zed/settings.json")),
		(AgentType::JetBrainsAi, None), // MCP is GUI-only (no file)
		(AgentType::RooCode, None),
		(AgentType::Kimi, Some(".kimi/mcp.json")),
		(AgentType::Mistral, Some(".vibe/config.toml")),
		(AgentType::Pi, None), // Pi has no MCP
		(AgentType::AugmentCode, Some(".augment/settings.json")),
		(AgentType::KiloCode, Some(".config/kilo/kilo.json")),
		(AgentType::Amp, Some(".config/amp/settings.json")),
		(AgentType::Factory, Some(".factory/mcp.json")),
		(AgentType::Warp, Some(".warp/.mcp.json")),
		// Platform-dependent home — asserted explicitly below.
		(AgentType::Hermes, Some("")),
		(AgentType::Grok, Some(".grok/config.toml")),
		(AgentType::Omp, Some(".omp/agent/mcp.json")),
	];

	for (agent_type, desc) in all_descriptors() {
		let expected_path = expected
			.iter()
			.find(|(t, _)| *t == agent_type)
			.map(|(_, p)| *p);

		match expected_path {
			Some(Some(path)) => {
				assert!(
					desc.mcp_global_path.is_some(),
					"mcp_global_path should be Some for {:?}",
					agent_type
				);
				let actual = desc.mcp_global_path.unwrap()();
				if agent_type == AgentType::Hermes {
					assert_eq!(
						actual,
						hermes_home().map(|home| home.join("config.yaml")),
						"Hermes global path follows its platform home"
					);
				} else if agent_type == AgentType::Zed {
					assert_eq!(
						actual,
						zed_config_dir().map(|dir| dir.join("settings.json")),
						"Zed global path should use the platform config directory"
					);
				} else if matches!(
					agent_type,
					AgentType::OpenCode | AgentType::KiloCode
				) || agent_type == AgentType::Amp
				{
					let allowed = match agent_type {
						AgentType::OpenCode => [
							home().join(".config/opencode/opencode.json"),
							home().join(".config/opencode/opencode.jsonc"),
						],
						AgentType::KiloCode => [
							home().join(".config/kilo/kilo.json"),
							home().join(".config/kilo/kilo.jsonc"),
						],
						AgentType::Amp => [
							home().join(".config/amp/settings.json"),
							home().join(".config/amp/settings.jsonc"),
						],
						_ => unreachable!(),
					};
					assert!(
						allowed.into_iter().any(|path| actual == Some(path)),
						"global path must use a supported filename: {actual:?}"
					);
				} else {
					assert_eq!(
						actual,
						Some(home().join(path)),
						"mcp_global_path mismatch for {:?}",
						agent_type
					);
				}
			}
			Some(None) => {
				assert!(
					desc.mcp_global_path.is_none(),
					"mcp_global_path should be None for {:?}",
					agent_type
				);
			}
			None => {}
		}
	}
}

// =============================================================================
// MCP Project Path Contract
// =============================================================================

#[test]
fn test_mcp_project_paths() {
	let expected: [(AgentType, Option<&str>); 26] = [
		(AgentType::Claude, Some(".mcp.json")),
		(AgentType::Codex, Some(".codex/config.toml")),
		(AgentType::Openclaw, None), // Openclaw has no project MCP path
		(AgentType::OpenCode, Some("opencode.json")),
		(AgentType::Gemini, Some(".gemini/settings.json")),
		(AgentType::Cline, None),
		(AgentType::Copilot, Some(".mcp.json")),
		(AgentType::Cursor, Some(".cursor/mcp.json")),
		(AgentType::Antigravity, Some(".agents/mcp_config.json")),
		(AgentType::Kiro, Some(".kiro/settings/mcp.json")),
		(AgentType::Windsurf, None),
		(AgentType::Trae, Some(".trae/mcp.json")),
		(AgentType::Zed, Some(".zed/settings.json")),
		(AgentType::JetBrainsAi, None), // MCP is GUI-only (no file)
		(AgentType::RooCode, Some(".roo/mcp.json")),
		(AgentType::Kimi, None),
		(AgentType::Mistral, Some(".vibe/config.toml")),
		(AgentType::Pi, None),          // Pi has no MCP
		(AgentType::AugmentCode, None), // CLI has no project MCP file
		(AgentType::KiloCode, Some("kilo.json")),
		(AgentType::Amp, Some(".amp/settings.json")),
		(AgentType::Factory, Some(".factory/mcp.json")),
		(AgentType::Warp, Some(".warp/.mcp.json")),
		(AgentType::Hermes, None),
		(AgentType::Grok, Some(".grok/config.toml")),
		(AgentType::Omp, Some(".omp/mcp.json")),
	];

	let root = PathBuf::from("/project");

	for (agent_type, desc) in all_descriptors() {
		let expected_path = expected
			.iter()
			.find(|(t, _)| *t == agent_type)
			.map(|(_, p)| *p);

		match expected_path {
			Some(Some(path)) => {
				assert!(
					desc.mcp_project_path.is_some(),
					"mcp_project_path should be Some for {:?}",
					agent_type
				);
				let actual = desc.mcp_project_path.unwrap()(&root);
				if matches!(agent_type, AgentType::KiloCode | AgentType::Amp) {
					let candidates = match agent_type {
						AgentType::KiloCode => {
							[root.join("kilo.json"), root.join("kilo.jsonc")]
						}
						AgentType::Amp => [
							root.join(".amp/settings.json"),
							root.join(".amp/settings.jsonc"),
						],
						_ => unreachable!(),
					};
					assert!(
						candidates.into_iter().any(|path| actual == Some(path)),
						"project path must use a supported filename for {:?}: {actual:?}",
						agent_type
					);
				} else {
					assert_eq!(
						actual,
						Some(root.join(path)),
						"mcp_project_path mismatch for {:?}",
						agent_type
					);
				}
			}
			Some(None) => {
				assert!(
					desc.mcp_project_path.is_none(),
					"mcp_project_path should be None for {:?}",
					agent_type
				);
			}
			None => {}
		}
	}
}

#[test]
fn opencode_project_mcp_uses_an_existing_supported_config() {
	for relative in [
		"opencode.json",
		"opencode.jsonc",
		".opencode/opencode.json",
		".opencode/opencode.jsonc",
	] {
		let project = tempfile::tempdir().unwrap();
		let path = project.path().join(relative);
		std::fs::create_dir_all(path.parent().unwrap()).unwrap();
		std::fs::write(&path, "{}\n").unwrap();

		let actual = agents::opencode::DESCRIPTOR.mcp_project_path.unwrap()(
			project.path(),
		);
		assert_eq!(actual, Some(path), "existing {relative} must be reused");
	}
}

#[test]
fn opencode_project_mcp_defaults_to_root_config() {
	let project = tempfile::tempdir().unwrap();
	let server = McpServer::new(
		"notebooklm",
		McpTransport::stdio(
			"doppler",
			vec![
				"run".into(),
				"-p".into(),
				"notebooklm".into(),
				"-c".into(),
				"prd".into(),
				"--".into(),
				"nblm-mcp".into(),
				"--transport".into(),
				"stdio".into(),
			],
		),
	);

	(agents::opencode::DESCRIPTOR.save_mcps)(
		Some(project.path()),
		ResourceScope::ProjectOnly,
		&[server],
	)
	.unwrap();

	let config = project.path().join("opencode.json");
	let json: serde_json::Value =
		serde_json::from_str(&std::fs::read_to_string(config).unwrap())
			.unwrap();
	assert_eq!(
		json["mcp"]["notebooklm"]["command"],
		serde_json::json!([
			"doppler",
			"run",
			"-p",
			"notebooklm",
			"-c",
			"prd",
			"--",
			"nblm-mcp",
			"--transport",
			"stdio"
		])
	);
	assert!(!project.path().join(".opencode/settings.json").exists());
}

// =============================================================================
// Global Data Dir Tests (from main branch actual values)
// =============================================================================

#[test]
fn test_global_data_dirs() {
	let _env = default_env();
	let expected: [(AgentType, Option<&str>); 22] = [
		(AgentType::Claude, Some(".claude")),
		(AgentType::Codex, Some(".codex")),
		(AgentType::Openclaw, Some(".openclaw")),
		(AgentType::OpenCode, Some(".config/opencode")),
		(AgentType::Gemini, Some(".gemini")),
		(AgentType::Cline, Some(".cline")),
		(AgentType::Copilot, Some(".copilot")),
		(AgentType::Cursor, Some(".cursor")),
		(AgentType::Antigravity, Some(".gemini/antigravity")),
		(AgentType::Kiro, Some(".kiro")),
		(AgentType::Windsurf, Some(".codeium/windsurf")),
		// Zed, Trae, and JetBrainsAi use the OS config dir, not a home dotfolder —
		// asserted explicitly after the loop.
		(AgentType::RooCode, Some(".roo")),
		(AgentType::Kimi, Some(".kimi")),
		(AgentType::Mistral, Some(".vibe")),
		(AgentType::Pi, Some(".pi/agent")),
		(AgentType::AugmentCode, Some(".augment")),
		(AgentType::KiloCode, Some(".config/kilo")),
		(AgentType::Amp, Some(".config/amp")),
		(AgentType::Warp, Some(".warp")),
		(AgentType::Factory, Some(".factory")),
		// Hermes uses a platform-dependent home — asserted after the loop.
		(AgentType::Grok, Some(".grok")),
		(AgentType::Omp, Some(".omp/agent")),
	];

	for (agent_type, desc) in all_descriptors() {
		let expected_dir = expected
			.iter()
			.find(|(t, _)| *t == agent_type)
			.map(|(_, p)| *p);

		match expected_dir {
			Some(Some(path)) => {
				let actual = (desc.global_data_dir)();
				assert_eq!(
					actual,
					Some(home().join(path)),
					"global_data_dir mismatch for {:?}",
					agent_type
				);
			}
			Some(None) => {
				assert_eq!(
					(desc.global_data_dir)(),
					None,
					"global_data_dir should be None for {:?}",
					agent_type
				);
			}
			None => {}
		}
	}

	// Trae and JetBrains AI store data in the OS config dir (Application
	// Support on macOS, .config on Linux), not a home dotfolder.
	use aghub_agents::agents;
	assert_eq!(
		(agents::trae::DESCRIPTOR.global_data_dir)(),
		dirs::config_dir().map(|c| c.join("Trae")),
		"trae global_data_dir should be the OS config dir"
	);
	assert_eq!(
		(agents::jetbrains_ai::DESCRIPTOR.global_data_dir)(),
		dirs::config_dir().map(|c| c.join("JetBrains")),
		"jetbrains-ai global_data_dir should be the OS config dir"
	);
	assert_eq!(
		(agents::zed::DESCRIPTOR.global_data_dir)(),
		zed_config_dir(),
		"zed global_data_dir should be the OS config dir"
	);
	assert_eq!(
		(agents::hermes::DESCRIPTOR.global_data_dir)(),
		hermes_home(),
		"hermes global_data_dir follows its platform home"
	);
}

// =============================================================================
// MCP Capabilities Tests (from main branch actual values)
// =============================================================================

#[test]
fn test_mcp_capabilities_stdio() {
	let expected: [(AgentType, bool); 26] = [
		(AgentType::Claude, true),
		(AgentType::Codex, true),
		(AgentType::Openclaw, true),
		(AgentType::OpenCode, true),
		(AgentType::Gemini, true),
		(AgentType::Cline, true),
		(AgentType::Copilot, true),
		(AgentType::Cursor, true),
		(AgentType::Antigravity, true),
		(AgentType::Kiro, true),
		(AgentType::Windsurf, true),
		(AgentType::Trae, true),
		(AgentType::Zed, true),
		(AgentType::JetBrainsAi, false), // GUI-only, no file-based MCP
		(AgentType::RooCode, true),
		(AgentType::Kimi, true),
		(AgentType::Mistral, true),
		(AgentType::Pi, false), // Pi has no MCP
		(AgentType::AugmentCode, true),
		(AgentType::KiloCode, true),
		(AgentType::Amp, true),
		(AgentType::Warp, true),
		(AgentType::Factory, true),
		(AgentType::Hermes, true),
		(AgentType::Grok, true),
		(AgentType::Omp, true),
	];

	for (agent_type, desc) in all_descriptors() {
		let (_, val) = expected
			.iter()
			.find(|(t, _)| *t == agent_type)
			.expect("every descriptor must have a capability contract");
		{
			assert_eq!(
				desc.capabilities.mcp.stdio, *val,
				"mcp.stdio mismatch for {:?}",
				agent_type
			);
		}
	}
}

#[test]
fn test_mcp_capabilities_remote() {
	let expected: [(AgentType, bool); 26] = [
		(AgentType::Claude, true),
		(AgentType::Codex, true),
		(AgentType::Openclaw, true),
		(AgentType::OpenCode, true),
		(AgentType::Gemini, true),
		(AgentType::Cline, true),
		(AgentType::Copilot, true),
		(AgentType::Cursor, true),
		(AgentType::Antigravity, true),
		(AgentType::Kiro, true),
		(AgentType::Windsurf, true),
		(AgentType::Trae, true),
		(AgentType::Zed, true),
		(AgentType::JetBrainsAi, false), // GUI-only, no file-based MCP
		(AgentType::RooCode, true),
		(AgentType::Kimi, true),
		(AgentType::Mistral, true),
		(AgentType::Pi, false), // Pi has no MCP
		(AgentType::AugmentCode, true),
		(AgentType::KiloCode, true),
		(AgentType::Amp, true),
		(AgentType::Warp, true),
		(AgentType::Factory, true),
		(AgentType::Hermes, true),
		(AgentType::Grok, true),
		(AgentType::Omp, true),
	];

	for (agent_type, desc) in all_descriptors() {
		let (_, val) = expected
			.iter()
			.find(|(t, _)| *t == agent_type)
			.expect("every descriptor must have a capability contract");
		{
			assert_eq!(
				desc.capabilities.mcp.remote, *val,
				"mcp.remote mismatch for {:?}",
				agent_type
			);
		}
	}
}

#[test]
fn test_mcp_capabilities_scopes_global() {
	let expected: [(AgentType, bool); 26] = [
		(AgentType::Claude, true),
		(AgentType::Codex, true),
		(AgentType::Openclaw, true),
		(AgentType::OpenCode, true),
		(AgentType::Gemini, true),
		(AgentType::Cline, true),
		(AgentType::Copilot, true),
		(AgentType::Cursor, true),
		(AgentType::Antigravity, true),
		(AgentType::Kiro, true),
		(AgentType::Windsurf, true),
		(AgentType::Trae, false), // global MCP is GUI-managed
		(AgentType::Zed, true),
		(AgentType::JetBrainsAi, false), // GUI-only, no file-based MCP
		(AgentType::RooCode, false),
		(AgentType::Kimi, true),
		(AgentType::Mistral, true),
		(AgentType::Pi, false), // Pi has no MCP
		(AgentType::AugmentCode, true),
		(AgentType::KiloCode, true),
		(AgentType::Amp, true),
		(AgentType::Warp, true),
		(AgentType::Factory, true),
		(AgentType::Hermes, true),
		(AgentType::Grok, true),
		(AgentType::Omp, true),
	];

	for (agent_type, desc) in all_descriptors() {
		let (_, val) = expected
			.iter()
			.find(|(t, _)| *t == agent_type)
			.expect("every descriptor must have a capability contract");
		{
			assert_eq!(
				desc.capabilities.mcp.scopes.global, *val,
				"mcp.scopes.global mismatch for {:?}",
				agent_type
			);
		}
	}
}

#[test]
fn test_mcp_capabilities_scopes_project() {
	let expected: [(AgentType, bool); 26] = [
		(AgentType::Claude, true),
		(AgentType::Codex, true),
		(AgentType::Openclaw, false), // Openclaw has no project MCP
		(AgentType::OpenCode, true),
		(AgentType::Gemini, true),
		(AgentType::Cline, false),
		(AgentType::Copilot, true),
		(AgentType::Cursor, true),
		(AgentType::Antigravity, true),
		(AgentType::Kiro, true),
		(AgentType::Windsurf, false),
		(AgentType::Trae, true),
		(AgentType::Zed, true),
		(AgentType::JetBrainsAi, false), // GUI-only, no file-based MCP
		(AgentType::RooCode, true),
		(AgentType::Kimi, false),
		(AgentType::Mistral, true),
		(AgentType::Pi, false),          // Pi has no MCP
		(AgentType::AugmentCode, false), // CLI has no project MCP file
		(AgentType::KiloCode, true),
		(AgentType::Amp, true),
		(AgentType::Warp, true),
		(AgentType::Factory, true),
		(AgentType::Hermes, false),
		(AgentType::Grok, true),
		(AgentType::Omp, true),
	];

	for (agent_type, desc) in all_descriptors() {
		let (_, val) = expected
			.iter()
			.find(|(t, _)| *t == agent_type)
			.expect("every descriptor must have a capability contract");
		{
			assert_eq!(
				desc.capabilities.mcp.scopes.project, *val,
				"mcp.scopes.project mismatch for {:?}",
				agent_type
			);
		}
	}
}

#[test]
fn test_mcp_capabilities_enable_disable() {
	let expected: [(AgentType, bool); 26] = [
		(AgentType::Claude, false),
		(AgentType::Codex, true),
		(AgentType::Openclaw, true),
		(AgentType::OpenCode, true),
		(AgentType::Gemini, false),
		(AgentType::Cline, true),
		(AgentType::Copilot, false),
		(AgentType::Cursor, false),
		(AgentType::Antigravity, true),
		(AgentType::Kiro, true),
		(AgentType::Windsurf, false),
		(AgentType::Trae, false),
		// Zed's documented `context_servers` entry has no per-server toggle,
		// so aghub does not invent one.
		(AgentType::Zed, false),
		(AgentType::JetBrainsAi, false),
		(AgentType::RooCode, true),
		(AgentType::Kimi, false),
		(AgentType::Mistral, true),
		(AgentType::Pi, false),
		(AgentType::AugmentCode, false),
		(AgentType::KiloCode, true),
		(AgentType::Amp, true),
		(AgentType::Warp, false),
		(AgentType::Factory, false),
		(AgentType::Hermes, true),
		(AgentType::Grok, true),
		(AgentType::Omp, true),
	];

	for (agent_type, desc) in all_descriptors() {
		let expected = expected
			.iter()
			.find(|(agent, _)| *agent == agent_type)
			.map(|(_, value)| *value)
			.expect("every descriptor must have an MCP capability contract");
		assert_eq!(
			desc.capabilities.mcp.enable_disable, expected,
			"mcp.enable_disable mismatch for {:?}",
			agent_type
		);
	}
}

// =============================================================================
// Skills Capabilities Tests (from main branch actual values)
// =============================================================================

#[test]
fn test_skills_capabilities_scopes_global() {
	let expected: [(AgentType, bool); 26] = [
		(AgentType::Claude, true),
		(AgentType::Codex, true),
		(AgentType::Openclaw, true),
		(AgentType::OpenCode, true),
		(AgentType::Gemini, true),
		(AgentType::Cline, true),
		(AgentType::Copilot, true),
		(AgentType::Cursor, true),
		(AgentType::Antigravity, true),
		(AgentType::Kiro, true),
		(AgentType::Windsurf, true),
		(AgentType::Trae, false), // global skills are not attested
		(AgentType::Zed, false),  // Zed has no global skills
		(AgentType::JetBrainsAi, false),
		(AgentType::RooCode, true),
		(AgentType::Kimi, true),
		(AgentType::Mistral, true),
		(AgentType::Pi, true),
		(AgentType::AugmentCode, true),
		(AgentType::KiloCode, true),
		(AgentType::Amp, true),
		(AgentType::Warp, true),
		(AgentType::Factory, true),
		(AgentType::Hermes, true),
		(AgentType::Grok, true),
		(AgentType::Omp, true),
	];

	for (agent_type, desc) in all_descriptors() {
		let (_, val) = expected
			.iter()
			.find(|(t, _)| *t == agent_type)
			.expect("every descriptor must have a capability contract");
		{
			assert_eq!(
				desc.capabilities.skills.scopes.global, *val,
				"skills.scopes.global mismatch for {:?}",
				agent_type
			);
		}
	}
}

#[test]
fn test_skills_capabilities_scopes_project() {
	let expected: [(AgentType, bool); 26] = [
		(AgentType::Claude, true),
		(AgentType::Codex, true),
		(AgentType::Openclaw, false), // Openclaw has no project skills
		(AgentType::OpenCode, true),
		(AgentType::Gemini, true),
		(AgentType::Cline, true),
		(AgentType::Copilot, true),
		(AgentType::Cursor, true),
		(AgentType::Antigravity, true),
		(AgentType::Kiro, true),
		(AgentType::Windsurf, true),
		(AgentType::Trae, true),
		(AgentType::Zed, false), // Zed has no project skills
		(AgentType::JetBrainsAi, false),
		(AgentType::RooCode, true),
		(AgentType::Kimi, true),
		(AgentType::Mistral, true),
		(AgentType::Pi, true),
		(AgentType::AugmentCode, true),
		(AgentType::KiloCode, true),
		(AgentType::Amp, true),
		(AgentType::Warp, true),
		(AgentType::Factory, true),
		(AgentType::Hermes, false),
		(AgentType::Grok, true),
		(AgentType::Omp, true),
	];

	for (agent_type, desc) in all_descriptors() {
		let (_, val) = expected
			.iter()
			.find(|(t, _)| *t == agent_type)
			.expect("every descriptor must have a capability contract");
		{
			assert_eq!(
				desc.capabilities.skills.scopes.project, *val,
				"skills.scopes.project mismatch for {:?}",
				agent_type
			);
		}
	}
}

#[test]
fn test_skills_capabilities_universal() {
	let expected: [(AgentType, bool); 26] = [
		(AgentType::Claude, false),
		(AgentType::Codex, false),
		(AgentType::Openclaw, false),
		(AgentType::OpenCode, false),
		(AgentType::Gemini, false),
		(AgentType::Cline, false),
		(AgentType::Copilot, false),
		(AgentType::Cursor, false),
		(AgentType::Antigravity, false),
		(AgentType::Kiro, false),
		(AgentType::Windsurf, false),
		(AgentType::Trae, false),
		(AgentType::Zed, false),
		(AgentType::JetBrainsAi, false),
		(AgentType::RooCode, false),
		(AgentType::Kimi, true), // Kimi has universal skills
		(AgentType::Mistral, false),
		(AgentType::Pi, false),
		(AgentType::AugmentCode, false),
		(AgentType::KiloCode, false),
		(AgentType::Amp, true), // Amp has universal skills
		(AgentType::Warp, false),
		(AgentType::Factory, false),
		(AgentType::Hermes, false),
		(AgentType::Grok, false),
		(AgentType::Omp, false),
	];

	for (agent_type, desc) in all_descriptors() {
		let (_, val) = expected
			.iter()
			.find(|(t, _)| *t == agent_type)
			.expect("every descriptor must have a capability contract");
		{
			assert_eq!(
				desc.capabilities.skills.universal, *val,
				"skills.universal mismatch for {:?}",
				agent_type
			);
		}
	}
}

// =============================================================================
// Sub-Agent Capabilities Tests (from main branch actual values)
// =============================================================================

#[test]
fn test_sub_agent_capabilities_scopes_global() {
	let expected: [(AgentType, bool); 26] = [
		(AgentType::Claude, true), // Claude has global sub-agents
		(AgentType::Codex, true),
		(AgentType::Openclaw, false),
		(AgentType::OpenCode, true),
		(AgentType::Gemini, false),
		(AgentType::Cline, false),
		(AgentType::Copilot, true),
		(AgentType::Cursor, false),
		(AgentType::Antigravity, true),
		(AgentType::Kiro, false),
		(AgentType::Windsurf, false),
		(AgentType::Trae, false),
		(AgentType::Zed, false),
		(AgentType::JetBrainsAi, false),
		(AgentType::RooCode, false),
		(AgentType::Kimi, false),
		(AgentType::Mistral, false),
		(AgentType::Pi, false),
		(AgentType::AugmentCode, false),
		(AgentType::KiloCode, false),
		(AgentType::Amp, false),
		(AgentType::Warp, false),
		(AgentType::Factory, false),
		(AgentType::Hermes, false),
		(AgentType::Grok, true),
		(AgentType::Omp, false),
	];

	for (agent_type, desc) in all_descriptors() {
		let (_, val) = expected
			.iter()
			.find(|(t, _)| *t == agent_type)
			.expect("every descriptor must have a capability contract");
		{
			assert_eq!(
				desc.capabilities.sub_agents.scopes.global, *val,
				"sub_agents.scopes.global mismatch for {:?}",
				agent_type
			);
		}
	}
}

#[test]
fn test_sub_agent_capabilities_scopes_project() {
	let expected: [(AgentType, bool); 26] = [
		(AgentType::Claude, true), // Claude has project sub-agents
		(AgentType::Codex, true),
		(AgentType::Openclaw, false),
		(AgentType::OpenCode, true),
		(AgentType::Gemini, false),
		(AgentType::Cline, false),
		(AgentType::Copilot, true),
		(AgentType::Cursor, false),
		(AgentType::Antigravity, true),
		(AgentType::Kiro, false),
		(AgentType::Windsurf, false),
		(AgentType::Trae, false),
		(AgentType::Zed, false),
		(AgentType::JetBrainsAi, false),
		(AgentType::RooCode, false),
		(AgentType::Kimi, false),
		(AgentType::Mistral, false),
		(AgentType::Pi, false),
		(AgentType::AugmentCode, false),
		(AgentType::KiloCode, false),
		(AgentType::Amp, false),
		(AgentType::Warp, false),
		(AgentType::Factory, false),
		(AgentType::Hermes, false),
		(AgentType::Grok, true),
		(AgentType::Omp, false),
	];

	for (agent_type, desc) in all_descriptors() {
		let (_, val) = expected
			.iter()
			.find(|(t, _)| *t == agent_type)
			.expect("every descriptor must have a capability contract");
		{
			assert_eq!(
				desc.capabilities.sub_agents.scopes.project, *val,
				"sub_agents.scopes.project mismatch for {:?}",
				agent_type
			);
		}
	}
}

// =============================================================================
// Global Skill Paths Tests (from main branch actual values)
// =============================================================================

#[test]
fn test_global_skill_paths() {
	let _env = default_env();
	// Most agents have single skill path, Claude has dynamic plugin discovery
	let expected: [(AgentType, Option<&[&str]>); 26] = [
		// Claude: dynamic plugin discovery, base path is .claude/skills
		(AgentType::Claude, Some(&[".claude/skills"])),
		(
			AgentType::Codex,
			Some(&[".codex/skills", ".agents/skills", "/etc/codex/skills"]),
		),
		(AgentType::Openclaw, Some(&[".openclaw/skills"])),
		(
			AgentType::OpenCode,
			// Own dir + universal Master only — no foreign-agent private dirs.
			Some(&[".config/opencode/skills", ".agents/skills"]),
		),
		(AgentType::Gemini, Some(&[".gemini/skills"])),
		(AgentType::Cline, Some(&[".agents/skills"])),
		(
			AgentType::Copilot,
			// Own dir + universal Master only — no foreign-agent private dirs.
			Some(&[".copilot/skills", ".agents/skills"]),
		),
		(
			AgentType::Cursor,
			// Own dir + universal Master only — no foreign-agent private dirs.
			Some(&[".cursor/skills", ".agents/skills"]),
		),
		(
			AgentType::Antigravity,
			// Vendor moved the global root to `.gemini/config/`; the two older
			// dirs stay READ-only so shipped installs are not stranded.
			Some(&[
				".gemini/config/skills",
				".gemini/antigravity/skills",
				".gemini/antigravity-cli/skills",
			]),
		),
		(AgentType::Kiro, Some(&[".kiro/skills"])),
		(AgentType::Windsurf, Some(&[".codeium/windsurf/skills"])),
		(AgentType::Trae, None),
		(AgentType::Zed, None), // Zed has no skills
		(AgentType::JetBrainsAi, None),
		(AgentType::RooCode, Some(&[".roo/skills"])),
		(
			AgentType::Kimi,
			Some(&[".config/agents/skills", ".config/agents/skills"]),
		), // universal=true adds extra path
		(AgentType::Mistral, Some(&[".vibe/skills"])),
		(AgentType::Pi, Some(&[".pi/agent/skills", ".agents/skills"])),
		(AgentType::AugmentCode, Some(&[".augment/skills"])),
		(AgentType::KiloCode, Some(&[".kilocode/skills"])),
		(
			AgentType::Amp,
			Some(&[".config/agents/skills", ".config/agents/skills"]),
		), // universal=true adds extra path
		(AgentType::Warp, Some(&[".agents/skills"])),
		(AgentType::Factory, Some(&[".factory/skills"])),
		(AgentType::Grok, Some(&[".grok/skills", ".agents/skills"])),
		(
			AgentType::Omp,
			Some(&[".omp/agent/skills", ".agents/skills"]),
		),
		// Hermes has a platform-dependent home — asserted after the loop.
		(AgentType::Hermes, Some(&[])),
	];

	for (agent_type, desc) in all_descriptors() {
		{
			let (_, paths) = expected
				.iter()
				.find(|(t, _)| *t == agent_type)
				.expect("every descriptor must have a skill-path contract");
			if agent_type == AgentType::Hermes {
				assert_eq!(
					desc.global_skill_read_paths(),
					hermes_home()
						.map(|home| vec![home.join("skills")])
						.unwrap_or_default(),
					"Hermes global skills follow its platform home"
				);
				continue;
			}
			match paths {
				Some(path_strs) => {
					assert!(
						desc.global_skill_paths.is_some(),
						"global_skill_paths should be Some for {:?}",
						agent_type
					);
					let actual = desc.global_skill_read_paths();
					// For Claude, only check the first path since plugins are dynamic
					// For Openclaw, dynamic npm discovery, just check first path
					if agent_type == AgentType::Claude {
						assert!(
							actual.first() == Some(&home().join(".claude/skills")),
							"global_skill_read_paths first path mismatch for {:?}",
							agent_type
						);
					} else if agent_type == AgentType::Openclaw {
						assert!(
							actual.first() == Some(&home().join(".openclaw/skills")),
							"global_skill_read_paths first path mismatch for {:?}",
							agent_type
						);
					} else if agent_type == AgentType::Codex {
						// /etc/codex/skills is a Unix-only system path
						#[allow(unused_mut)]
						let mut expected_paths = vec![
							home().join(".codex/skills"),
							home().join(".agents/skills"),
						];
						#[cfg(not(target_os = "windows"))]
						expected_paths.push(PathBuf::from("/etc/codex/skills"));
						assert_eq!(
							actual, expected_paths,
							"global_skill_read_paths mismatch for {:?}",
							agent_type
						);
					} else {
						let expected_paths: Vec<PathBuf> =
							path_strs.iter().map(|p| home().join(*p)).collect();
						assert_eq!(
							actual, expected_paths,
							"global_skill_read_paths mismatch for {:?}",
							agent_type
						);
					}
				}
				None => {
					assert!(
						desc.global_skill_paths.is_none(),
						"global_skill_paths should be None for {:?}",
						agent_type
					);
				}
			}
		}
	}
}

// =============================================================================
// Project Skill Paths Tests (from main branch actual values)
// =============================================================================

#[test]
fn test_project_skill_paths() {
	let root = PathBuf::from("/project");

	let expected: [(AgentType, Option<&[&str]>); 26] = [
		(AgentType::Claude, Some(&[".claude/skills"])),
		(AgentType::Codex, Some(&[".agents/skills"])),
		(AgentType::Openclaw, None), // Openclaw has no project skills
		(
			AgentType::OpenCode,
			Some(&[".opencode/skills", ".agents/skills"]),
		),
		(AgentType::Gemini, Some(&[".agents/skills"])),
		(AgentType::Cline, Some(&[".agents/skills"])),
		(
			AgentType::Copilot,
			Some(&[".agents/skills", ".github/skills"]),
		),
		(
			AgentType::Cursor,
			// Own dir + universal Master only — no foreign-agent private dirs.
			Some(&[".cursor/skills", ".agents/skills"]),
		),
		(
			AgentType::Antigravity,
			// `.agent/` is the vendor's own backward-compat alias.
			Some(&[".agents/skills", ".agent/skills"]),
		),
		(AgentType::Kiro, Some(&[".kiro/skills"])),
		(AgentType::Windsurf, Some(&[".windsurf/skills"])),
		(AgentType::Trae, Some(&[".trae/skills"])),
		(AgentType::Zed, None), // Zed has no skills
		(AgentType::JetBrainsAi, None),
		(AgentType::RooCode, Some(&[".roo/skills"])),
		(AgentType::Kimi, Some(&[".agents/skills", ".agents/skills"])), // universal=true adds extra .agents/skills
		(AgentType::Mistral, Some(&[".vibe/skills"])),
		(AgentType::Pi, Some(&[".pi/skills", ".agents/skills"])),
		(AgentType::AugmentCode, Some(&[".augment/skills"])),
		(AgentType::KiloCode, Some(&[".kilocode/skills"])),
		(AgentType::Amp, Some(&[".agents/skills", ".agents/skills"])), // universal=true adds extra .agents/skills
		(AgentType::Warp, Some(&[".agents/skills"])),
		(AgentType::Factory, Some(&[".factory/skills"])),
		(AgentType::Grok, Some(&[".grok/skills", ".agents/skills"])),
		(AgentType::Omp, Some(&[".omp/skills", ".agents/skills"])),
		(AgentType::Hermes, None),
	];

	for (agent_type, desc) in all_descriptors() {
		{
			let (_, paths) = expected
				.iter()
				.find(|(t, _)| *t == agent_type)
				.expect("every descriptor must have a skill-path contract");
			match paths {
				Some(path_strs) => {
					assert!(
						desc.project_skill_paths.is_some(),
						"project_skill_paths should be Some for {:?}",
						agent_type
					);
					let actual = desc.project_skill_read_paths(&root);
					let expected_paths: Vec<PathBuf> =
						path_strs.iter().map(|p| root.join(*p)).collect();
					assert_eq!(
						actual, expected_paths,
						"project_skill_read_paths mismatch for {:?}",
						agent_type
					);
				}
				None => {
					assert!(
						desc.project_skill_paths.is_none(),
						"project_skill_paths should be None for {:?}",
						agent_type
					);
				}
			}
		}
	}
}
