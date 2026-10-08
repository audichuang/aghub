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
fn shared_referrer_keep_plans_no_paths_and_preview_matches_execution() {
	use aghub_core::dto::removal::RemovalKind;

	let _lock = env_lock();
	let tmp = tempfile::tempdir().unwrap();
	let _env = isolated_home(tmp.path());
	let name = "shared-keep-no-paths";
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

	let preview = cline
		.remove_skill_planned(name, false, true, false)
		.unwrap();
	assert!(
		preview.plan.shared_master_kept && preview.plan.paths.is_empty(),
		"a shared-referrer keep must plan no paths, got: {:?}",
		preview.plan
	);
	assert!(matches!(
		aghub_core::dto::removal::RemovalView::from_outcome(&preview, true)
			.outcome,
		RemovalKind::Kept
	));

	let result = cline.remove_skill_planned(name, false, false, true);
	assert!(result.is_err());
	assert!(primary.symlink_metadata().is_ok(), "primary Referrer gone");
	assert!(alias.symlink_metadata().is_ok(), "alias Referrer gone");
	assert!(master.exists(), "Master gone");
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
		&aghub_core::WriteScope::project(&project),
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

/// `--all-agents` leaves a dir only disabled agents read alone, then refuses
/// because the skill is still served from it. The refusal has to name that
/// cause — "still discoverable" alone sends the user hunting for a bug.
#[test]
fn all_agents_refusal_names_the_disabled_agent_that_still_holds_the_skill() {
	let _lock = env_lock();
	let tmp = tempfile::tempdir().unwrap();
	let _env = isolated_home(tmp.path());
	let data = tmp.path().join("data");
	let prev = std::env::var_os("AGHUB_DATA_DIR");
	std::env::set_var("AGHUB_DATA_DIR", &data);
	let _data = RestoreEnv(vec![("AGHUB_DATA_DIR", prev)]);

	let name = "held-by-disabled";
	let master = tmp.path().join(".aghub").join(name);
	write_skill(&master, name);
	for dir in [".claude/skills", ".cursor/skills"] {
		let referrer = tmp.path().join(dir).join(name);
		std::fs::create_dir_all(referrer.parent().unwrap()).unwrap();
		std::os::unix::fs::symlink(&master, &referrer).unwrap();
	}
	aghub_core::agent_settings::write_disabled_agents_in(
		&data,
		&["cursor".to_string()].into_iter().collect(),
	)
	.unwrap();

	let mut claude =
		ConfigManager::new(create_adapter(AgentType::Claude), true, None);
	claude.load().unwrap();
	let err = claude
		.remove_skill_planned(name, true, false, true)
		.expect_err("a disabled agent's Referrer keeps the Master alive");

	let message = err.to_string();
	assert!(
		message.contains("disabled agent")
			&& message.contains(".cursor/skills"),
		"refusal must name the disabled agent's dir: {message}"
	);
	assert!(
		master.join("SKILL.md").exists(),
		"the Master must survive the refusal"
	);
}

#[test]
fn single_agent_refusal_names_the_unselected_readers_of_the_shared_slot() {
	let _lock = env_lock();
	let tmp = tempfile::tempdir().unwrap();
	let _env = isolated_home(tmp.path());
	let data = tmp.path().join("data");
	let prev = std::env::var_os("AGHUB_DATA_DIR");
	std::env::set_var("AGHUB_DATA_DIR", &data);
	let _data = RestoreEnv(vec![("AGHUB_DATA_DIR", prev)]);

	let name = "unselected-readers-slot";
	let master = tmp.path().join(".aghub").join(name);
	write_skill(&master, name);
	let shared_dir = tmp.path().join(".agents/skills");
	std::fs::create_dir_all(&shared_dir).unwrap();
	let referrer = shared_dir.join(name);
	std::os::unix::fs::symlink(&master, &referrer).unwrap();

	let disabled_set: std::collections::BTreeSet<String> = AgentType::ALL
		.iter()
		.copied()
		.filter(|&a| a != AgentType::Cline && a != AgentType::Cursor)
		.map(|a| aghub_core::registry::get(a).id.to_string())
		.collect();
	aghub_core::agent_settings::write_disabled_agents_in(&data, &disabled_set)
		.unwrap();

	let expected_readers = vec!["cursor"];
	let actual_readers = aghub_core::skills::removal::skill_dir_readers_outside(
		&shared_dir,
		aghub_core::models::ResourceScope::GlobalOnly,
		None,
		&[AgentType::Cline],
	);
	assert_eq!(actual_readers, expected_readers);

	let mut cline =
		ConfigManager::new(create_adapter(AgentType::Cline), true, None);
	cline.load().unwrap();
	let err = cline
		.remove_skill_planned(name, false, false, true)
		.expect_err("removing shared skill for single agent must be refused");

	let message = err.to_string();
	for expected_id in &expected_readers {
		assert!(
			message.contains(expected_id),
			"refusal message must contain expected id '{expected_id}': {message}"
		);
	}
	assert!(
		!message.contains("(disabled)"),
		"refusal message must not contain '(disabled)': {message}"
	);
	assert!(
		message.contains("--all-agents"),
		"refusal message must contain '--all-agents': {message}"
	);
	assert!(
		referrer.symlink_metadata().is_ok(),
		"Referrer symlink must still exist"
	);
	assert!(
		master.join("SKILL.md").exists(),
		"Master SKILL.md must still exist"
	);
}
