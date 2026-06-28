//! Sub-agent planned-removal integration tests for aghub-core.
//!
//! Cover `remove_sub_agent_planned`'s dry-run/confirm gate and the legacy
//! `remove_sub_agent` reroute (Phase 3 #5). A project-scoped Claude manager
//! with a temp root is fully isolated — it reads/writes `<root>/.claude/agents`
//! and never touches the real home dir.

use aghub_core::{
	create_adapter,
	models::{AgentType, SubAgent},
	skills::removal::{Layout, PruneStatus},
	ConfigError, ConfigManager,
};

/// Build a project-scoped Claude manager rooted at `root` with `name` already
/// added + persisted, then reloaded so `source_path` is populated from disk.
fn manager_with_persisted_agent(
	root: &std::path::Path,
	name: &str,
) -> ConfigManager {
	let mut mgr = ConfigManager::new(
		create_adapter(AgentType::Claude),
		false,
		Some(root),
	);
	mgr.load().unwrap();
	mgr.add_sub_agent(SubAgent::new(name)).unwrap();
	// Reload so the backing `.md` file's path lands in `source_path`.
	mgr.load().unwrap();
	mgr
}

fn agent_md_path(root: &std::path::Path, name: &str) -> std::path::PathBuf {
	root.join(".claude/agents").join(format!("{name}.md"))
}

#[test]
fn remove_sub_agent_planned_dry_run_keeps_agent_and_file() {
	let tmp = tempfile::tempdir().unwrap();
	let root = tmp.path();
	let mut mgr = manager_with_persisted_agent(root, "reviewer");
	let file = agent_md_path(root, "reviewer");
	assert!(file.exists(), "precondition: backing file written");

	let outcome = mgr
		.remove_sub_agent_planned("reviewer", true, false)
		.unwrap();

	assert!(!outcome.executed, "dry-run must not execute");
	assert_eq!(outcome.plan.layout, Layout::Copy);
	assert!(!outcome.plan.needs_confirm);
	assert_eq!(outcome.prune, PruneStatus::NotRun);
	assert_eq!(
		outcome.plan.paths,
		vec![file.clone()],
		"plan path is the backing source file"
	);

	// Non-executed branch leaves state untouched: file on disk + still loaded.
	assert!(file.exists(), "dry-run must leave the file on disk");
	let mut reloaded = ConfigManager::new(
		create_adapter(AgentType::Claude),
		false,
		Some(root),
	);
	reloaded.load().unwrap();
	assert!(
		reloaded.get_sub_agent("reviewer").is_some(),
		"dry-run must not remove the agent from disk"
	);
}

#[test]
fn remove_sub_agent_planned_executes_deletes_file() {
	let tmp = tempfile::tempdir().unwrap();
	let root = tmp.path();
	let mut mgr = manager_with_persisted_agent(root, "reviewer");
	let file = agent_md_path(root, "reviewer");

	let outcome = mgr
		.remove_sub_agent_planned("reviewer", false, true)
		.unwrap();

	assert!(outcome.executed, "confirm + no dry-run must execute");
	assert_eq!(outcome.plan.paths, vec![file.clone()]);
	assert!(!file.exists(), "backing file must be deleted");
	assert!(
		mgr.get_sub_agent("reviewer").is_none(),
		"agent dropped from the in-memory config"
	);

	let mut reloaded = ConfigManager::new(
		create_adapter(AgentType::Claude),
		false,
		Some(root),
	);
	reloaded.load().unwrap();
	assert!(
		reloaded.get_sub_agent("reviewer").is_none(),
		"agent gone after reload"
	);
}

#[test]
fn remove_sub_agent_planned_config_only_has_empty_paths() {
	// An agent with no source_path (never persisted to a file) executes with an
	// empty plan path set and no fs error.
	let tmp = tempfile::tempdir().unwrap();
	let root = tmp.path();
	let mut mgr = ConfigManager::new(
		create_adapter(AgentType::Claude),
		false,
		Some(root),
	);
	mgr.load().unwrap();
	// add but do NOT reload, so source_path stays None in memory.
	mgr.add_sub_agent(SubAgent::new("ephemeral")).unwrap();
	assert!(mgr
		.get_sub_agent("ephemeral")
		.unwrap()
		.source_path
		.is_none());

	let outcome = mgr
		.remove_sub_agent_planned("ephemeral", false, true)
		.unwrap();

	assert!(outcome.executed);
	assert!(
		outcome.plan.paths.is_empty(),
		"config-only agent has no backing path to delete"
	);
	assert!(mgr.get_sub_agent("ephemeral").is_none());
}

#[test]
fn remove_sub_agent_planned_failed_delete_errors_without_mutating() {
	// A backing-file deletion failure must surface as an actionable error and
	// NOT mutate in-memory state. Sub-agents differ from skills: there is no
	// post-delete save that cleans up stale files, so a "report skipped and
	// succeed" contract would leave the file on disk to reappear on reload.
	//
	// ENOTDIR trick: point the agent's source_path at `<file>/agent.md` where
	// `<file>` is a regular file, so `remove_file` fails with NotADirectory
	// deterministically (root-safe — no chmod).
	let tmp = tempfile::tempdir().unwrap();
	let root = tmp.path();
	let mut mgr = ConfigManager::new(
		create_adapter(AgentType::Claude),
		false,
		Some(root),
	);
	mgr.load().unwrap();

	let blocker = root.join("not-a-dir");
	std::fs::write(&blocker, "x").unwrap();
	let undeletable = blocker.join("agent.md");
	assert!(
		std::fs::remove_file(&undeletable).is_err(),
		"precondition: ENOTDIR makes remove_file fail"
	);

	let mut agent = SubAgent::new("corrupt");
	agent.source_path = Some(undeletable.to_string_lossy().into_owned());
	mgr.add_sub_agent(agent).unwrap();

	let err = mgr
		.remove_sub_agent_planned("corrupt", false, true)
		.unwrap_err();
	assert!(
		matches!(err, ConfigError::Io(_)),
		"undeletable backing file must surface an IO error, got {err:?}"
	);
	assert!(
		mgr.get_sub_agent("corrupt").is_some(),
		"a failed delete must not drop the agent from memory"
	);
}

#[test]
fn remove_sub_agent_planned_stale_file_failure_errors_and_keeps_agent() {
	// Regression (Codex blocking): when the backing `.md` is REAL (written by
	// add/save) but its deletion fails, the manager must NOT report success.
	// Because `save_scoped_sub_agents` never deletes stale files, an orphaned
	// `.md` left behind reappears on reload — so a failed delete that mutated
	// + saved in-memory state would falsely claim the agent is gone while it
	// silently returns. The fix deletes the file FIRST: a non-NotFound error
	// surfaces as an error and leaves the agent loaded + on disk (consistent).
	//
	// Force the delete to fail without chmod (root-safe): replace the backing
	// file with a directory at the same path so `remove_file` hits ENOTDIR/
	// IsADirectory deterministically.
	let tmp = tempfile::tempdir().unwrap();
	let root = tmp.path();
	let mut mgr = manager_with_persisted_agent(root, "reviewer");
	let file = agent_md_path(root, "reviewer");
	assert!(file.exists(), "precondition: real backing file written");

	// Swap the file for a directory so remove_file can never succeed.
	std::fs::remove_file(&file).unwrap();
	std::fs::create_dir(&file).unwrap();
	assert!(
		std::fs::remove_file(&file).is_err(),
		"precondition: remove_file fails on a directory"
	);

	let err = mgr
		.remove_sub_agent_planned("reviewer", false, true)
		.unwrap_err();
	assert!(
		matches!(err, ConfigError::Io(_)),
		"undeletable backing file must surface an actionable IO error, \
		 got {err:?}"
	);

	// State preserved: agent still loaded in memory, and the backing path is
	// still on disk. Crucially the delete happens BEFORE the in-memory removal
	// + save, so the failure leaves no stale orphan that a later save would
	// fail to clean up and that would reappear on reload.
	assert!(
		mgr.get_sub_agent("reviewer").is_some(),
		"failed delete must leave the agent in memory"
	);
	assert!(file.exists(), "backing path must remain on disk");
}

#[test]
fn remove_sub_agent_planned_missing_is_not_found() {
	let tmp = tempfile::tempdir().unwrap();
	let root = tmp.path();
	let mut mgr = ConfigManager::new(
		create_adapter(AgentType::Claude),
		false,
		Some(root),
	);
	mgr.load().unwrap();

	let err = mgr
		.remove_sub_agent_planned("ghost", false, true)
		.unwrap_err();
	assert!(
		matches!(err, ConfigError::ResourceNotFound { .. }),
		"absent agent must surface ResourceNotFound, got {err:?}"
	);
}

#[test]
fn remove_sub_agent_wrapper_still_deletes_immediately() {
	// Guards the reroute: the legacy `-> Result<()>` wrapper must keep deleting.
	let tmp = tempfile::tempdir().unwrap();
	let root = tmp.path();
	let mut mgr = manager_with_persisted_agent(root, "reviewer");
	let file = agent_md_path(root, "reviewer");

	mgr.remove_sub_agent("reviewer").unwrap();

	assert!(!file.exists(), "wrapper must delete the backing file");
	assert!(mgr.get_sub_agent("reviewer").is_none());
}

#[test]
fn remove_sub_agent_wrapper_missing_is_not_found() {
	let tmp = tempfile::tempdir().unwrap();
	let root = tmp.path();
	let mut mgr = ConfigManager::new(
		create_adapter(AgentType::Claude),
		false,
		Some(root),
	);
	mgr.load().unwrap();

	let err = mgr.remove_sub_agent("ghost").unwrap_err();
	assert!(matches!(err, ConfigError::ResourceNotFound { .. }));
}
