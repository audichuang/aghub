#![cfg(unix)]

use aghub_core::manager::ConfigManager;
use aghub_core::models::{AgentType, Skill};
use aghub_core::{create_adapter, PATH_OVERRIDE_VARS};
use std::ffi::OsString;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

fn env_lock() -> std::sync::MutexGuard<'static, ()> {
	static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
	LOCK.get_or_init(|| Mutex::new(()))
		.lock()
		.unwrap_or_else(|e| e.into_inner())
}

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

fn isolated_home(home: &Path) -> RestoreEnv {
	let mut keys = PATH_OVERRIDE_VARS.to_vec();
	keys.extend(["HOME", "XDG_STATE_HOME"]);
	let restore = RestoreEnv(
		keys.iter()
			.map(|key| (*key, std::env::var_os(key)))
			.collect(),
	);
	for key in keys {
		std::env::remove_var(key);
	}
	std::env::set_var("HOME", home);
	std::env::set_var("XDG_CONFIG_HOME", home.join(".config"));
	std::env::set_var("XDG_STATE_HOME", home.join(".local/state"));
	restore
}

fn write_skill(dir: &Path, name: &str) {
	std::fs::create_dir_all(dir).unwrap();
	std::fs::write(
		dir.join("SKILL.md"),
		format!("---\nname: {name}\ndescription: test\n---\n"),
	)
	.unwrap();
}

#[test]
fn deleting_cline_global_shared_referrer_keeps_cursor_grant() {
	let _lock = env_lock();
	let tmp = tempfile::tempdir().unwrap();
	let _env = isolated_home(tmp.path());
	let master = tmp.path().join(".aghub/shared-delete-regression");
	write_skill(&master, "shared-delete-regression");
	let shared = tmp.path().join(".agents/skills/shared-delete-regression");
	std::fs::create_dir_all(shared.parent().unwrap()).unwrap();
	std::os::unix::fs::symlink(&master, &shared).unwrap();

	let mut cursor =
		ConfigManager::new(create_adapter(AgentType::Cursor), true, None);
	cursor.load().unwrap();
	assert!(cursor.get_skill("shared-delete-regression").is_some());
	let mut cline =
		ConfigManager::new(create_adapter(AgentType::Cline), true, None);
	cline.load().unwrap();
	assert!(cline.get_skill("shared-delete-regression").is_some());

	let result = cline.remove_skill_planned(
		"shared-delete-regression",
		false,
		false,
		true,
	);
	assert!(
		shared.symlink_metadata().is_ok(),
		"shared Referrer was deleted"
	);
	assert!(master.join("SKILL.md").exists(), "Master was deleted");
	assert!(result.is_err(), "one agent cannot revoke a shared grant");
	cursor.load().unwrap();
	assert!(cursor.get_skill("shared-delete-regression").is_some());
}

#[test]
fn deleting_two_shared_referrers_cannot_revoke_an_unselected_reader() {
	let _lock = env_lock();
	let tmp = tempfile::tempdir().unwrap();
	let _env = isolated_home(tmp.path());
	let name = "two-shared-referrers";
	let master = tmp.path().join(".aghub").join(name);
	write_skill(&master, name);
	let shared_dir = tmp.path().join(".agents/skills");
	std::fs::create_dir_all(&shared_dir).unwrap();
	let primary = shared_dir.join(name);
	let alias = shared_dir.join("legacy-folder");
	std::os::unix::fs::symlink(&master, &primary).unwrap();
	std::os::unix::fs::symlink(&master, &alias).unwrap();

	let mut cursor =
		ConfigManager::new(create_adapter(AgentType::Cursor), true, None);
	cursor.load().unwrap();
	assert!(cursor.get_skill(name).is_some());
	let mut cline =
		ConfigManager::new(create_adapter(AgentType::Cline), true, None);
	cline.load().unwrap();
	assert!(cline.get_skill(name).is_some());

	let result = cline.remove_skill_planned(name, false, false, true);
	assert!(result.is_err(), "one agent revoked Cursor's shared grant");
	assert!(primary.symlink_metadata().is_ok(), "primary Referrer gone");
	assert!(alias.symlink_metadata().is_ok(), "alias Referrer gone");
	assert!(master.exists(), "Master gone");
	cursor.load().unwrap();
	assert!(cursor.get_skill(name).is_some());
}

#[test]
fn exact_shared_read_slot_can_be_removed_for_its_full_reader_group() {
	let _lock = env_lock();
	let tmp = tempfile::tempdir().unwrap();
	let _env = isolated_home(tmp.path());
	let name = "shared-location-regression";
	let master = tmp.path().join(".aghub").join(name);
	write_skill(&master, name);
	let read_dir = tmp.path().join(".agents/skills");
	std::fs::create_dir_all(&read_dir).unwrap();
	let shared = read_dir.join(name);
	let alias = read_dir.join("old-folder");
	std::os::unix::fs::symlink(&master, &shared).unwrap();
	std::os::unix::fs::symlink(&master, &alias).unwrap();

	let mut cline =
		ConfigManager::new(create_adapter(AgentType::Cline), true, None);
	cline.load().unwrap();
	assert!(cline.get_skill(name).is_some());
	let outcome = cline
		.remove_skill_planned_at_dir_for_agents(
			name,
			&shared,
			false,
			false,
			true,
			AgentType::ALL,
		)
		.unwrap();
	assert!(outcome.executed, "full reader group must be removable");
	assert!(shared.symlink_metadata().is_err(), "Referrer survived");
	assert!(alias.symlink_metadata().is_err(), "alias Referrer survived");
	assert!(!master.exists(), "unreferenced Master survived");
}

#[test]
fn location_removal_rejects_a_directory_no_requested_agent_reads() {
	let _lock = env_lock();
	let tmp = tempfile::tempdir().unwrap();
	let _env = isolated_home(tmp.path());
	let name = "wrong-location-regression";
	let master = tmp.path().join(".aghub").join(name);
	write_skill(&master, name);
	let read_dir = tmp.path().join(".agents/skills");
	std::fs::create_dir_all(&read_dir).unwrap();
	let shared = read_dir.join(name);
	std::os::unix::fs::symlink(&master, &shared).unwrap();
	let unrelated = tmp.path().join("unrelated");
	std::fs::create_dir_all(&unrelated).unwrap();

	let mut cline =
		ConfigManager::new(create_adapter(AgentType::Cline), true, None);
	cline.load().unwrap();
	let result = cline.remove_skill_planned_at_dir_for_agents(
		name,
		&unrelated.join(name),
		false,
		false,
		true,
		AgentType::ALL,
	);
	assert!(result.is_err(), "unrelated location was accepted");
	assert!(shared.symlink_metadata().is_ok(), "Referrer was deleted");
	assert!(master.exists(), "Master was deleted");
}

#[test]
fn project_store_symlink_cannot_redirect_from_path_install() {
	let _lock = env_lock();
	let tmp = tempfile::tempdir().unwrap();
	let project = tmp.path().join("project");
	let outside = tmp.path().join("outside");
	std::fs::create_dir_all(&project).unwrap();
	std::fs::create_dir_all(&outside).unwrap();
	std::os::unix::fs::symlink(&outside, project.join(".aghub")).unwrap();
	let source = tmp.path().join("source/escaped");
	write_skill(&source, "escaped");

	let mut manager = ConfigManager::new(
		create_adapter(AgentType::Claude),
		false,
		Some(&project),
	);
	manager.load().unwrap();
	let result = manager.add_skill_from_path_universal(&source, None);
	assert!(result.is_err(), "install escaped through project/.aghub");
	assert!(
		!outside.join("escaped").exists(),
		"outside Master was written"
	);
	assert!(
		!project.join(".claude/skills/escaped").exists(),
		"grant was written"
	);
}

#[test]
fn project_store_symlink_cannot_redirect_direct_install() {
	let _lock = env_lock();
	let tmp = tempfile::tempdir().unwrap();
	let project = tmp.path().join("project");
	let outside = tmp.path().join("outside");
	std::fs::create_dir_all(&project).unwrap();
	std::fs::create_dir_all(&outside).unwrap();
	std::os::unix::fs::symlink(&outside, project.join(".aghub")).unwrap();

	let mut manager = ConfigManager::new(
		create_adapter(AgentType::Claude),
		false,
		Some(&project),
	);
	manager.load().unwrap();
	let result = manager.add_skill_universal(Skill::new("direct-escape"));
	assert!(result.is_err(), "install escaped through project/.aghub");
	assert!(
		!outside.join("direct-escape").exists(),
		"outside Master was written"
	);
	assert!(
		!project.join(".claude/skills/direct-escape").exists(),
		"grant was written"
	);
}

#[test]
fn project_store_symlink_is_not_an_allowed_mutation_root() {
	let _lock = env_lock();
	let tmp = tempfile::tempdir().unwrap();
	let project = tmp.path().join("project");
	let outside = tmp.path().join("outside");
	std::fs::create_dir_all(&project).unwrap();
	write_skill(&outside.join("escaped"), "escaped");
	std::os::unix::fs::symlink(&outside, project.join(".aghub")).unwrap();

	let roots =
		aghub_core::skills::removal::allowed_skill_roots(&[], Some(&project));
	assert!(
		aghub_core::skills::removal::assert_contained(
			&outside.join("escaped"),
			&roots
		)
		.is_none(),
		"a linked Master store must not authorize mutation outside the project",
	);
}

#[test]
fn project_store_symlink_cannot_redirect_update() {
	let _lock = env_lock();
	let tmp = tempfile::tempdir().unwrap();
	let project = tmp.path().join("project");
	let outside = tmp.path().join("outside");
	std::fs::create_dir_all(&project).unwrap();
	let master = outside.join("escaped");
	write_skill(&master, "escaped");
	std::os::unix::fs::symlink(&outside, project.join(".aghub")).unwrap();
	let referrer = project.join(".claude/skills/escaped");
	std::fs::create_dir_all(referrer.parent().unwrap()).unwrap();
	std::os::unix::fs::symlink(project.join(".aghub/escaped"), &referrer)
		.unwrap();
	let before = std::fs::read(master.join("SKILL.md")).unwrap();

	let mut manager = ConfigManager::new(
		create_adapter(AgentType::Claude),
		false,
		Some(&project),
	);
	manager.load().unwrap();
	assert!(manager.get_skill("escaped").is_some());
	let mut updated = Skill::new("escaped");
	updated.description = Some("changed".into());
	let result = manager.update_skill("escaped", updated);
	assert!(result.is_err(), "update escaped through project/.aghub");
	assert_eq!(std::fs::read(master.join("SKILL.md")).unwrap(), before);
}

#[test]
fn project_store_symlink_cannot_redirect_repair_adoption() {
	let _lock = env_lock();
	let tmp = tempfile::tempdir().unwrap();
	let _env = isolated_home(&tmp.path().join("home"));
	let project = tmp.path().join("project");
	let outside = tmp.path().join("outside");
	std::fs::create_dir_all(project.join(".claude")).unwrap();
	std::fs::create_dir_all(&outside).unwrap();
	std::os::unix::fs::symlink(&outside, project.join(".aghub")).unwrap();
	write_skill(&project.join(".agents/skills/legacy"), "legacy");

	let result = aghub_core::skills::repair::repair_skill(
		aghub_core::models::ResourceScope::ProjectOnly,
		Some(&project),
		"legacy",
		true,
		false,
	);
	assert!(
		!matches!(
			result,
			Ok(Some(aghub_core::skills::repair::RepairReport {
				outcome: aghub_core::skills::repair::RepairOutcome::Migrated,
				..
			}))
		),
		"repair adopted through project/.aghub: {result:?}"
	);
	assert!(
		!outside.join("legacy").exists(),
		"repair wrote the Master outside the project"
	);
	assert!(
		project.join(".agents/skills/legacy/SKILL.md").is_file(),
		"the only copy must stay in place"
	);
}

/// The user's own `~/.aghub` may be a symlink into dotfiles; only a
/// project-controlled store is refused.
#[test]
fn global_store_symlink_keeps_install_update_and_delete_working() {
	let _lock = env_lock();
	let tmp = tempfile::tempdir().unwrap();
	let home = tmp.path().join("home");
	let _env = isolated_home(&home);
	let dotfiles = tmp.path().join("dotfiles/aghub");
	std::fs::create_dir_all(&dotfiles).unwrap();
	std::fs::create_dir_all(&home).unwrap();
	std::os::unix::fs::symlink(&dotfiles, home.join(".aghub")).unwrap();
	let source = tmp.path().join("source/dotted");
	write_skill(&source, "dotted");

	let mut claude =
		ConfigManager::new(create_adapter(AgentType::Claude), true, None);
	claude.load().unwrap();
	claude
		.add_skill_from_path_universal(&source, None)
		.expect("global install through a user-linked store");
	assert!(dotfiles.join("dotted/SKILL.md").is_file());

	claude.load().unwrap();
	let mut updated = claude.get_skill("dotted").unwrap().clone();
	updated.description = Some("changed".into());
	claude
		.update_skill("dotted", updated)
		.expect("global update through a user-linked store");
	assert!(std::fs::read_to_string(dotfiles.join("dotted/SKILL.md"))
		.unwrap()
		.contains("changed"));

	claude.load().unwrap();
	let outcome = claude
		.remove_skill_planned("dotted", true, false, true)
		.expect("global delete through a user-linked store");
	assert!(outcome.executed);
	assert!(
		!dotfiles.join("dotted").exists(),
		"the Master must be removed, not skipped as out-of-tree"
	);
	assert!(!home.join(".claude/skills/dotted").exists());
}
