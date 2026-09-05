//! Tests for agent skills path configuration.
//!
//! Ported from xdg-config-paths.test.ts and openclaw-paths.test.ts.

use aghub_agents::agents::{amp, cursor, kimi, openclaw, opencode, pi};
use std::path::{Path, PathBuf};

// Serializes every test in this binary that reads OR mutates global env
// (`$HOME`/`XDG_*`). libtest runs a binary's tests on parallel threads of ONE
// process, and on Unix mutating env while another thread reads it is UB
// (`dirs::home_dir()` reads `$HOME`) — so the mutating test AND every
// env-reading path test below must hold this lock. Not `#[cfg(unix)]`: readers
// call it on all platforms, so it is never dead code on Windows.
fn env_lock() -> std::sync::MutexGuard<'static, ()> {
	use std::sync::{Mutex, OnceLock};
	static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
	LOCK.get_or_init(|| Mutex::new(()))
		.lock()
		.unwrap_or_else(|e| e.into_inner())
}

// ─── XDG config path tests (xdg-config-paths.test.ts) ───────────────────────

#[test]
fn test_opencode_global_config_path_not_platform_specific() {
	let _env = env_lock();
	let path = opencode::DESCRIPTOR
		.mcp_global_path
		.and_then(|path| path())
		.expect("OpenCode should have a global MCP path");
	let path_str = path.to_string_lossy();
	assert!(
		!path_str.contains("Library"),
		"OpenCode global path should not use ~/Library: {}",
		path_str
	);
	assert!(
		!path_str.contains("AppData"),
		"OpenCode global path should not use AppData: {}",
		path_str
	);
	assert!(
		!path_str.contains("Preferences"),
		"OpenCode global path should not use Preferences: {}",
		path_str
	);
}

#[test]
fn test_opencode_global_config_respects_xdg_config_home() {
	use std::ffi::OsString;

	let _env = env_lock();
	let config_home = tempfile::tempdir().unwrap();
	// OPENCODE_CONFIG(_DIR) outrank XDG_CONFIG_HOME, so leaving either one set
	// (a developer running OpenCode does) would make this assert the wrong
	// path. Restore whatever the machine had on the way out.
	struct RestoreEnv(Vec<(&'static str, Option<OsString>)>);
	impl Drop for RestoreEnv {
		fn drop(&mut self) {
			for (key, value) in &self.0 {
				match value {
					Some(value) => std::env::set_var(key, value),
					None => std::env::remove_var(key),
				}
			}
		}
	}
	let keys = ["OPENCODE_CONFIG", "OPENCODE_CONFIG_DIR", "XDG_CONFIG_HOME"];
	let _restore = RestoreEnv(
		keys.iter()
			.map(|key| (*key, std::env::var_os(key)))
			.collect(),
	);
	for key in keys {
		std::env::remove_var(key);
	}
	std::env::set_var("XDG_CONFIG_HOME", config_home.path());
	std::fs::create_dir_all(config_home.path().join("opencode")).unwrap();
	std::fs::write(config_home.path().join("opencode/opencode.jsonc"), "{}\n")
		.unwrap();

	let path = opencode::DESCRIPTOR
		.mcp_global_path
		.and_then(|path| path())
		.unwrap();
	assert_eq!(path, config_home.path().join("opencode/opencode.jsonc"));
}

#[test]
fn test_amp_global_skills_uses_xdg() {
	let _env = env_lock();
	let paths = amp::DESCRIPTOR.global_skill_read_paths();
	let path = paths.first().expect("Should have at least one path");
	let path_str = path.to_string_lossy();
	assert!(
		path_str.contains(".config"),
		"Amp global skills path should use XDG .config dir, got: {}",
		path_str
	);
}

#[test]
fn test_amp_global_skills_not_platform_specific() {
	let _env = env_lock();
	let paths = amp::DESCRIPTOR.global_skill_read_paths();
	let path = paths.first().expect("Should have at least one path");
	let path_str = path.to_string_lossy();
	assert!(
		!path_str.contains("Library"),
		"Amp skills path should not use ~/Library: {}",
		path_str
	);
	assert!(
		!path_str.contains("AppData"),
		"Amp skills path should not use AppData: {}",
		path_str
	);
	assert!(
		!path_str.contains("Preferences"),
		"Amp skills path should not use Preferences: {}",
		path_str
	);
}

#[test]
fn test_cursor_global_skills_path() {
	let _env = env_lock();
	let paths = cursor::DESCRIPTOR.global_skill_read_paths();
	let path = paths.first().expect("Should have at least one path");
	assert!(
		path.to_string_lossy().contains(".cursor"),
		"Cursor global skills should be under ~/.cursor, got: {}",
		path.display()
	);
	assert!(
		path.ends_with("skills"),
		"Cursor global skills path should end with 'skills', got: {}",
		path.display()
	);
}

#[test]
fn test_kimi_global_mcp_path() {
	let _env = env_lock();
	let path = kimi::DESCRIPTOR
		.mcp_global_path
		.and_then(|path| path())
		.expect("Kimi should have a global MCP path");
	// Compare COMPONENTS, not the rendered string: the descriptor builds this
	// path by joining `.kimi` and `mcp.json` separately, so on Windows the
	// rendering is `.kimi\\mcp.json` and a `contains(".kimi/mcp.json")` check
	// goes red. It only ever passed because the old descriptor happened to join
	// one slash-bearing literal.
	assert_eq!(
		path.file_name().and_then(|name| name.to_str()),
		Some("mcp.json"),
		"got: {}",
		path.display()
	);
	assert_eq!(
		path.parent()
			.and_then(|parent| parent.file_name())
			.and_then(|name| name.to_str()),
		Some(".kimi"),
		"Kimi global MCP path should be <share dir>/mcp.json, got: {}",
		path.display()
	);
}

#[test]
fn test_pi_global_skills_path_uses_agent_dir() {
	let _env = env_lock();
	let paths = pi::DESCRIPTOR.global_skill_read_paths();
	let path = paths.first().expect("Should have at least one path");
	assert!(
		path.to_string_lossy().contains(".pi/agent/skills"),
		"Pi global skills should be under ~/.pi/agent/skills, got: {}",
		path.display()
	);
}

#[test]
fn test_pi_has_no_mcp_capabilities() {
	let descriptor = aghub_core::registry::iter_all()
		.find(|d| d.id == "pi")
		.unwrap();
	assert!(!descriptor.capabilities.mcp.stdio);
	assert!(!descriptor.capabilities.mcp.remote);
}

// ─── OpenClaw fallback path tests (openclaw-paths.test.ts) ──────────────────

#[test]
fn test_openclaw_prefers_openclaw_dir() {
	let home = PathBuf::from("/tmp/home");
	// All three dirs "exist"
	let exists = |p: &Path| -> bool {
		let s = p.to_string_lossy();
		s.ends_with(".openclaw")
			|| s.ends_with(".clawdbot")
			|| s.ends_with(".moltbot")
	};
	let result = openclaw::get_openclaw_skills_dirs(&home, exists);
	assert_eq!(result, vec![home.join(".openclaw/skills")]);
}

#[test]
fn test_openclaw_falls_back_to_clawdbot() {
	let home = PathBuf::from("/tmp/home");
	// Only .clawdbot and .moltbot exist
	let exists = |p: &Path| -> bool {
		let s = p.to_string_lossy();
		s.ends_with(".clawdbot") || s.ends_with(".moltbot")
	};
	let result = openclaw::get_openclaw_skills_dirs(&home, exists);
	assert_eq!(result, vec![home.join(".clawdbot/skills")]);
}

#[test]
fn test_openclaw_falls_back_to_moltbot() {
	let home = PathBuf::from("/tmp/home");
	// Only .moltbot exists
	let exists =
		|p: &Path| -> bool { p.to_string_lossy().ends_with(".moltbot") };
	let result = openclaw::get_openclaw_skills_dirs(&home, exists);
	assert_eq!(result, vec![home.join(".moltbot/skills")]);
}

#[test]
fn test_openclaw_defaults_to_openclaw_when_none_exist() {
	let home = PathBuf::from("/tmp/home");
	let result = openclaw::get_openclaw_skills_dirs(&home, |_| false);
	assert_eq!(result, vec![home.join(".openclaw/skills")]);
}

#[test]
fn test_openclaw_skills_enabled() {
	let descriptor = aghub_core::registry::iter_all()
		.find(|d| d.id == "openclaw")
		.unwrap();
	assert!(
		descriptor.capabilities.skills.scopes.global,
		"OpenClaw should have skills capability enabled"
	);
}

// ─── Regression Tests for Mutation Targeting ────────────────────────────────

// Unix-only: dirs::home_dir() reads $HOME on Unix, so we can redirect the
// global master (`~/.aghub`) into a temp dir. On Windows the profile
// comes from the known-folder API and ignores env, so this isolation would
// silently write to the real user profile again.
#[cfg(unix)]
#[test]
fn test_opencode_global_creation_persists() {
	use std::ffi::OsString;

	// Global skill install resolves the master via dirs::home_dir() →
	// `$HOME/.aghub`. Isolate HOME so real path logic still runs
	// without writing under the developer's real skill tree (this test used
	// to create one `test-skill-opencode-<millis>/` per run with no teardown).
	// This test mutates $HOME, so hold the binary's env lock to exclude other
	// env-touching tests in the same process.
	let _env = env_lock();

	let fake_home = tempfile::tempdir().unwrap();
	// Use OsString: a Unix $HOME may be non-UTF-8, and String would drop it,
	// making the Drop below wrongly `remove_var` and pollute later tests.
	let previous_home = std::env::var_os("HOME");
	std::env::set_var("HOME", fake_home.path());

	// $HOME alone does NOT isolate this agent. OpenCode's global skills dir is
	// `dirs::config_dir()/opencode/skills`, and `config_dir()` prefers
	// `XDG_CONFIG_HOME` over `$HOME/.config` — so with the developer's own
	// variable set, the install read and wrote their REAL `~/.config/opencode`
	// and came back `ResourceExists` for a name it had never seen. The agent's
	// own override outranks both, so clear that too.
	struct RestoreVar(&'static str, Option<OsString>);
	impl Drop for RestoreVar {
		fn drop(&mut self) {
			match self.1.take() {
				Some(v) => std::env::set_var(self.0, v),
				None => std::env::remove_var(self.0),
			}
		}
	}
	let _restore_config: Vec<RestoreVar> =
		["XDG_CONFIG_HOME", "OPENCODE_CONFIG_DIR", "OPENCODE_CONFIG"]
			.into_iter()
			.map(|key| {
				let saved = RestoreVar(key, std::env::var_os(key));
				std::env::remove_var(key);
				saved
			})
			.collect();

	// RAII: always restore $HOME, even if an assert panics mid-test.
	struct RestoreHome(Option<OsString>);
	impl Drop for RestoreHome {
		fn drop(&mut self) {
			match &self.0 {
				Some(v) => std::env::set_var("HOME", v),
				None => std::env::remove_var("HOME"),
			}
		}
	}
	let _restore_home = RestoreHome(previous_home);

	// RAII: best-effort remove of the skill dirs we may write under the
	// isolated HOME if an assert panics before explicit teardown (errors are
	// swallowed, and the fake_home TempDir drop is likewise best-effort — a
	// permission/IO failure could still leave residue on the panic path). No
	// real-home paths here — HOME is isolated, so writes cannot reach the
	// developer's real tree, and blindly deleting a real-home path could nuke a
	// legitimately-named dir the user happens to own.
	struct CleanupSkill {
		paths: Vec<PathBuf>,
	}
	impl Drop for CleanupSkill {
		fn drop(&mut self) {
			for path in &self.paths {
				let _ = std::fs::remove_dir_all(path);
			}
		}
	}

	let skill_name = "test-skill-opencode-persist";
	let isolated_master = fake_home.path().join(".aghub").join(skill_name);
	let isolated_agent = fake_home
		.path()
		.join(".config/opencode/skills")
		.join(skill_name);
	let _cleanup = CleanupSkill {
		paths: vec![isolated_master.clone(), isolated_agent],
	};

	// TestConfig sets a skills_path_override by default; clear it so install
	// and reload both go through the real descriptor path resolution under
	// the isolated HOME (not the TestConfig temp skills dir).
	let test =
		aghub_core::testing::TestConfig::new(aghub_core::AgentType::OpenCode)
			.unwrap();
	aghub_core::adapter::set_skills_path_override("opencode", None);

	let mut manager = test.create_manager();
	manager.load().unwrap();

	let mut skill = aghub_core::models::Skill::new(skill_name);
	skill.description = Some("desc".to_string());

	manager.add_skill(skill).unwrap();

	// Reload and check it persists
	let mut manager2 = test.create_manager();
	manager2.load().unwrap();
	assert!(
		manager2.get_skill(skill_name).is_some(),
		"Skill should survive reload"
	);

	// The write landing under the isolated `$HOME/.aghub` proves it
	// went through home-dir resolution (not the TestConfig override) and that
	// isolation redirected the write off the real user home. (It cannot by
	// itself rule out a hypothetical double-write also hitting real home —
	// HOME isolation is what guarantees the real tree stays untouched.)
	let master_md = isolated_master.join("SKILL.md");
	assert!(
		master_md.is_file(),
		"master should land under isolated $HOME/.aghub, got missing {}",
		master_md.display()
	);

	// Explicit teardown before drop; fail if anything is still present —
	// leaving junk is the bug this test guards against.
	std::fs::remove_dir_all(&isolated_master)
		.expect("explicit teardown must remove the isolated master skill dir");
	assert!(
		!isolated_master.exists(),
		"isolated master must be gone after teardown"
	);
}

#[test]
fn test_source_path_update_targets_original_directory() {
	let test =
		aghub_core::testing::TestConfig::new(aghub_core::AgentType::Codex)
			.unwrap();

	// Create a skill at the overridden skills dir
	test.create_test_skill("codex-skill", Some("original"))
		.unwrap();

	let mut manager = test.create_manager();
	manager.load().unwrap();

	let skill = manager
		.get_skill("codex-skill")
		.expect("Should load skill from test dir");

	// source_path should point to the test skills dir
	let sp = skill.source_path.as_ref().unwrap();
	assert!(
		sp.contains("codex-skill"),
		"source_path should reference the skill directory"
	);

	// Update it
	let mut updated = skill.clone();
	updated.description = Some("updated".to_string());
	manager.update_skill("codex-skill", updated).unwrap();

	// Verify the file was updated in place at the original source_path
	let skill_file = test.skills_dir().join("codex-skill/SKILL.md");
	let content = std::fs::read_to_string(skill_file).unwrap();
	assert!(
		content.contains("description: updated"),
		"Skill should be updated at original source path"
	);
}

#[test]
fn test_rename_skill_migrates_sanitized_directory() {
	let test =
		aghub_core::testing::TestConfig::new(aghub_core::AgentType::Claude)
			.unwrap();

	test.create_test_skill("alpha-skill", Some("original"))
		.unwrap();

	let mut manager = test.create_manager();
	manager.load().unwrap();

	let skill = manager.get_skill("alpha-skill").unwrap().clone();
	let mut renamed = skill;
	renamed.name = "beta-skill".to_string();
	renamed.description = Some("renamed".to_string());
	manager.update_skill("alpha-skill", renamed).unwrap();

	assert!(
		!test.skills_dir().join("alpha-skill").exists(),
		"Old directory should be removed after rename"
	);

	let content =
		std::fs::read_to_string(test.skills_dir().join("beta-skill/SKILL.md"))
			.unwrap();
	assert!(content.contains("beta-skill"));
	assert!(content.contains("renamed"));
}

#[test]
fn test_delete_skill_with_slash_removes_sanitized_directory() {
	let test =
		aghub_core::testing::TestConfig::new(aghub_core::AgentType::Claude)
			.unwrap();

	let skill_dir = test.skills_dir().join("owner-repo");
	std::fs::create_dir_all(&skill_dir).unwrap();
	std::fs::write(
		skill_dir.join("SKILL.md"),
		"---\nname: owner/repo\ndescription: test\n---\n\n# Skill\n",
	)
	.unwrap();

	let mut manager = test.create_manager();
	manager.load().unwrap();

	assert!(
		manager.get_skill("owner/repo").is_some(),
		"Should discover skill with slash in name"
	);

	manager.remove_skill("owner/repo").unwrap();

	assert!(
		!skill_dir.exists(),
		"Sanitized directory should be removed on delete"
	);
}

fn write_import_skill_with_resources(dir: &Path, name: &str, body: &str) {
	std::fs::create_dir_all(dir).unwrap();
	std::fs::write(
		dir.join("SKILL.md"),
		format!(
			"---\nname: {name}\ndescription: imported skill\n---\n\n{body}\n"
		),
	)
	.unwrap();
	std::fs::create_dir_all(dir.join("scripts")).unwrap();
	std::fs::create_dir_all(dir.join("references")).unwrap();
	std::fs::create_dir_all(dir.join("assets")).unwrap();
	std::fs::write(dir.join("scripts/setup.sh"), "echo setup").unwrap();
	std::fs::write(dir.join("references/guide.md"), "# Guide").unwrap();
	std::fs::write(dir.join("assets/logo.txt"), "logo").unwrap();
}

#[test]
fn skill_import_directory_preserves_body_and_resources() {
	aghub_core::adapter::set_skills_path_override("claude", None);
	let temp = tempfile::tempdir().unwrap();
	let project_root = temp.path().join("project");
	std::fs::create_dir_all(&project_root).unwrap();
	let source_dir = temp.path().join("source/imported-skill");
	write_import_skill_with_resources(
		&source_dir,
		"imported-skill",
		"# Real imported instructions",
	);

	let mut manager = aghub_core::ConfigManager::new(
		aghub_core::create_adapter(aghub_core::AgentType::Claude),
		false,
		Some(&project_root),
	);
	manager.load().unwrap();
	let imported = manager.add_skill_from_path(&source_dir).unwrap().skill;

	assert_eq!(imported.name, "imported-skill");
	assert!(imported
		.content
		.as_deref()
		.unwrap()
		.contains("# Real imported instructions"));

	let target_dir = project_root.join(".aghub/imported-skill");
	let target_content =
		std::fs::read_to_string(target_dir.join("SKILL.md")).unwrap();
	assert!(target_content.contains("# Real imported instructions"));
	assert!(target_dir.join("scripts/setup.sh").exists());
	assert!(target_dir.join("references/guide.md").exists());
	assert!(target_dir.join("assets/logo.txt").exists());

	let agent_link = project_root.join(".claude/skills/imported-skill");
	assert!(agent_link.join("SKILL.md").exists());
	#[cfg(unix)]
	assert!(std::fs::symlink_metadata(&agent_link)
		.unwrap()
		.file_type()
		.is_symlink());

	let mut reloaded = aghub_core::ConfigManager::new(
		aghub_core::create_adapter(aghub_core::AgentType::Claude),
		false,
		Some(&project_root),
	);
	reloaded.load().unwrap();
	let loaded = reloaded.get_skill("imported-skill").unwrap();
	assert!(loaded.source_path.as_deref().unwrap().contains("SKILL.md"));
	assert!(loaded.canonical_path.is_some());
}

// Contract pin: a SINGLE agent whose descriptor cannot resolve a skills dir
// for the scope (Hermes is global-only, so project scope classifies as
// Unsupported) is a preflight HARD error — the add must not soft-succeed and
// record a dangling config entry against a never-written Master.
#[test]
fn add_skill_from_path_unsupported_scope_errors_and_writes_nothing() {
	aghub_core::adapter::set_skills_path_override("hermes", None);
	let temp = tempfile::tempdir().unwrap();
	let project_root = temp.path().join("project");
	std::fs::create_dir_all(&project_root).unwrap();
	let source_dir = temp.path().join("source/orphan-skill");
	write_import_skill_with_resources(&source_dir, "orphan-skill", "# body");

	let mut manager = aghub_core::ConfigManager::new(
		aghub_core::create_adapter(aghub_core::AgentType::Hermes),
		false,
		Some(&project_root),
	);
	manager.load().unwrap();
	let err = manager.add_skill_from_path(&source_dir).unwrap_err();

	let msg = err.to_string();
	assert!(
		msg.contains("does not support") && msg.contains("nothing was written"),
		"preflight rejection must state the contract, got: {msg}"
	);
	assert!(
		!project_root.join(".aghub/orphan-skill").exists(),
		"a rejected preflight must not write the Master"
	);

	let mut reloaded = aghub_core::ConfigManager::new(
		aghub_core::create_adapter(aghub_core::AgentType::Hermes),
		false,
		Some(&project_root),
	);
	reloaded.load().unwrap();
	assert!(
		reloaded.get_skill("orphan-skill").is_none(),
		"a rejected add must not record a dangling config entry"
	);
}

#[test]
fn skill_import_skill_md_file_copies_sibling_resources() {
	aghub_core::adapter::set_skills_path_override("claude", None);
	let temp = tempfile::tempdir().unwrap();
	let project_root = temp.path().join("project");
	std::fs::create_dir_all(&project_root).unwrap();
	let source_dir = temp.path().join("source/md-skill");
	write_import_skill_with_resources(
		&source_dir,
		"md-skill",
		"# Body from SKILL.md path",
	);

	let mut manager = aghub_core::ConfigManager::new(
		aghub_core::create_adapter(aghub_core::AgentType::Claude),
		false,
		Some(&project_root),
	);
	manager.load().unwrap();
	let imported = manager
		.add_skill_from_path(&source_dir.join("SKILL.md"))
		.unwrap()
		.skill;

	assert_eq!(imported.name, "md-skill");
	let target_dir = project_root.join(".aghub/md-skill");
	assert!(target_dir.join("scripts/setup.sh").exists());
	assert!(target_dir.join("assets/logo.txt").exists());
	let target_content =
		std::fs::read_to_string(target_dir.join("SKILL.md")).unwrap();
	assert!(target_content.contains("# Body from SKILL.md path"));

	let agent_link = project_root.join(".claude/skills/md-skill");
	assert!(agent_link.join("SKILL.md").exists());
	#[cfg(unix)]
	assert!(std::fs::symlink_metadata(&agent_link)
		.unwrap()
		.file_type()
		.is_symlink());
}
