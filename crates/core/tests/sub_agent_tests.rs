//! Sub-agent planned-removal integration tests for aghub-core.
//!
//! Cover `remove_sub_agent_planned`'s dry-run/confirm gate and the legacy
//! `remove_sub_agent` reroute (Phase 3 #5). A project-scoped Claude manager
//! with a temp root is fully isolated — it reads/writes `<root>/.claude/agents`
//! and never touches the real home dir.

use aghub_core::{
	create_adapter,
	manager::sub_agent::SubAgentPatch,
	models::{AgentType, SubAgent},
	skills::removal::{Layout, PruneStatus},
	ConfigError, ConfigManager,
};

fn project_manager(root: &std::path::Path) -> ConfigManager {
	let mut manager = ConfigManager::new(
		create_adapter(AgentType::Claude),
		false,
		Some(root),
	);
	manager.load().unwrap();
	manager
}

fn agent_with_instruction(name: &str, instruction: &str) -> SubAgent {
	let mut agent = SubAgent::new(name);
	agent.instruction = Some(instruction.to_string());
	agent
}

#[test]
fn adding_from_a_stale_manager_keeps_a_concurrent_sibling_update() {
	let tmp = tempfile::tempdir().unwrap();
	let root = tmp.path();
	project_manager(root)
		.add_sub_agent(agent_with_instruction("reviewer", "old instruction"))
		.unwrap();
	let mut stale = project_manager(root);
	let mut fresh = project_manager(root);
	fresh
		.update_sub_agent(
			"reviewer",
			SubAgentPatch {
				instruction: Some("new instruction".into()),
				..Default::default()
			},
		)
		.unwrap();
	stale
		.add_sub_agent(agent_with_instruction("other", "unrelated"))
		.unwrap();
	let content =
		std::fs::read_to_string(agent_md_path(root, "reviewer")).unwrap();
	assert!(content.contains("new instruction"), "{content}");
	assert!(!content.contains("old instruction"), "{content}");
}

#[test]
fn adding_same_name_from_a_stale_manager_does_not_overwrite() {
	let tmp = tempfile::tempdir().unwrap();
	let root = tmp.path();
	let mut stale = project_manager(root);
	let mut fresh = project_manager(root);
	fresh
		.add_sub_agent(agent_with_instruction("coder", "first committed value"))
		.unwrap();
	let error = stale
		.add_sub_agent(agent_with_instruction("coder", "stale overwrite"))
		.unwrap_err();
	assert!(matches!(error, ConfigError::ResourceExists { .. }));
	let content =
		std::fs::read_to_string(agent_md_path(root, "coder")).unwrap();
	assert!(content.contains("first committed value"), "{content}");
	assert!(!content.contains("stale overwrite"), "{content}");
}

#[test]
fn add_does_not_replace_a_differently_named_file_at_its_destination() {
	let tmp = tempfile::tempdir().unwrap();
	let root = tmp.path();
	let file = agent_md_path(root, "keeper");
	std::fs::create_dir_all(file.parent().unwrap()).unwrap();
	let original = "---\nname: blocked\n---\noriginal keeper body";
	std::fs::write(&file, original).unwrap();
	let mut manager = project_manager(root);
	assert!(manager.get_sub_agent("blocked").is_some());
	let error = manager
		.add_sub_agent(agent_with_instruction("keeper", "new body"))
		.unwrap_err();
	assert!(
		matches!(
			error,
			ConfigError::ResourceExists { .. } | ConfigError::InvalidConfig(_)
		),
		"unexpected error: {error:?}"
	);
	assert_eq!(std::fs::read_to_string(&file).unwrap(), original);
	assert!(!agent_md_path(root, "blocked").exists());
}

#[test]
fn patch_from_a_stale_manager_preserves_a_concurrent_field_update() {
	let tmp = tempfile::tempdir().unwrap();
	let root = tmp.path();
	project_manager(root)
		.add_sub_agent(agent_with_instruction("coder", "old instruction"))
		.unwrap();
	let mut stale = project_manager(root);
	let mut fresh = project_manager(root);
	fresh
		.update_sub_agent(
			"coder",
			SubAgentPatch {
				instruction: Some("new instruction".into()),
				..Default::default()
			},
		)
		.unwrap();
	stale
		.update_sub_agent(
			"coder",
			SubAgentPatch {
				description: Some("new description".into()),
				..Default::default()
			},
		)
		.unwrap();
	let content =
		std::fs::read_to_string(agent_md_path(root, "coder")).unwrap();
	assert!(content.contains("new instruction"), "{content}");
	assert!(content.contains("new description"), "{content}");
	assert!(!content.contains("old instruction"), "{content}");
}

/// Root probe: a `0o555` dir blocks creating/removing entries inside it unless
/// we are root (which bypasses the bits). Returns false so the test self-skips
/// under root/CI rather than asserting on a permission that isn't enforced.
#[cfg(unix)]
fn perms_enforced(under: &std::path::Path) -> bool {
	use std::os::unix::fs::PermissionsExt;
	let p = under.join(".perm-probe");
	std::fs::create_dir(&p).unwrap();
	std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o555))
		.unwrap();
	let blocked = std::fs::write(p.join("x"), b"x").is_err();
	std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755))
		.unwrap();
	std::fs::remove_dir_all(&p).ok();
	blocked
}

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
fn remove_sub_agent_planned_reloads_backing_path_before_delete() {
	// add_sub_agent has persisted the file even though its in-memory DTO has
	// no source_path yet. Removal must re-read the physical backing first.
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

	let file = agent_md_path(root, "ephemeral");
	assert!(file.exists());
	let outcome = mgr
		.remove_sub_agent_planned("ephemeral", false, true)
		.unwrap();

	assert!(outcome.executed);
	assert_eq!(outcome.plan.paths, vec![file.clone()]);
	assert!(!file.exists());
	assert!(mgr.get_sub_agent("ephemeral").is_none());
}

#[test]
fn remove_sub_agent_ignores_stale_in_memory_source_path() {
	// A source_path supplied before an earlier save is not the on-disk identity.
	// Removal must reload the descriptor's actual backing before deleting.
	let tmp = tempfile::tempdir().unwrap();
	let root = tmp.path();
	let mut mgr = project_manager(root);

	let blocker = root.join("not-a-dir");
	std::fs::write(&blocker, "x").unwrap();
	let mut agent = SubAgent::new("corrupt");
	agent.source_path =
		Some(blocker.join("agent.md").to_string_lossy().into_owned());
	mgr.add_sub_agent(agent).unwrap();
	let actual = agent_md_path(root, "corrupt");
	let result = mgr
		.remove_sub_agent_planned("corrupt", false, true)
		.unwrap();
	assert_eq!(result.plan.paths, vec![actual.clone()]);
	assert!(!actual.exists());
	assert_eq!(std::fs::read_to_string(blocker).unwrap(), "x");
}

#[cfg(unix)]
#[test]
fn remove_sub_agent_planned_stale_file_failure_errors_and_keeps_agent() {
	// Regression (Codex blocking): when the backing `.md` is REAL but its
	// removal fails, the manager must NOT report success. Because
	// `save_scoped_sub_agents` never deletes stale files, an orphaned `.md` left
	// behind reappears on reload — so a failed removal that mutated + saved
	// in-memory state would falsely claim the agent is gone. The transactional
	// removal moves the file out FIRST (tombstone); a non-NotFound failure
	// surfaces as an error and leaves the agent loaded + on disk (consistent).
	//
	// Force the real backing directory to refuse the tombstone rename.
	use std::os::unix::fs::PermissionsExt;

	let tmp = tempfile::tempdir().unwrap();
	let root = tmp.path();
	if !perms_enforced(root) {
		eprintln!("skip: root bypasses 0o555");
		return;
	}
	let mut mgr = manager_with_persisted_agent(root, "reviewer");
	let locked_dir = root.join(".claude/agents");
	let file = agent_md_path(root, "reviewer");

	let orig = std::fs::metadata(&locked_dir).unwrap().permissions();
	std::fs::set_permissions(
		&locked_dir,
		std::fs::Permissions::from_mode(0o555),
	)
	.unwrap();

	let res = mgr.remove_sub_agent_planned("reviewer", false, true);

	// RESTORE perms before asserting so a failure can't leak the temp dir.
	std::fs::set_permissions(&locked_dir, orig).unwrap();

	let err = res.expect_err(
		"an unremovable backing file must surface, not report success",
	);
	assert!(
		matches!(err, ConfigError::Io(_)),
		"unremovable backing file must surface an actionable IO error, \
		 got {err:?}"
	);

	// State preserved: agent still loaded + the backing file still on disk, so
	// the reported failure leaves no stale orphan that reappears on reload.
	assert!(
		mgr.get_sub_agent("reviewer").is_some(),
		"failed removal must leave the agent in memory"
	);
	assert!(file.exists(), "backing path must remain on disk");
}

#[test]
fn remove_sub_agent_planned_save_failure_restores_deleted_file() {
	// Regression (Codex blocking): the delete was NOT rollback-safe. It removed
	// the backing file FIRST, then mutated memory and saved. If the save fails
	// after the file is already gone, the API returns an error but the file is
	// permanently lost — the user is told it failed yet their sub-agent is
	// destroyed. The fix makes delete+save transactional: on save failure the
	// removed file is restored and the agent stays loaded, so a reported failure
	// means nothing changed.
	//
	// The loader accepts a frontmatter name different from the filename.
	// `keeper.md` declares `blocked`, while `blocked.md` is a directory. After
	// the target moves to a tombstone, saving the remaining `blocked` entry
	// must fail against that real directory (root-safe, no injected state).
	let tmp = tempfile::tempdir().unwrap();
	let root = tmp.path();
	let mut mgr = manager_with_persisted_agent(root, "reviewer");
	let target_file = agent_md_path(root, "reviewer");
	assert!(
		target_file.exists(),
		"precondition: target backing file written"
	);

	let keeper_file = agent_md_path(root, "keeper");
	std::fs::write(
		&keeper_file,
		"---\nname: blocked\n---\nkeeper instructions",
	)
	.unwrap();
	let blocked_file = agent_md_path(root, "blocked");
	std::fs::create_dir(&blocked_file).unwrap();
	mgr.load().unwrap();
	assert!(mgr.get_sub_agent("blocked").is_some());
	assert!(
		std::fs::write(&blocked_file, b"x").is_err(),
		"precondition: save write to keeper must fail"
	);

	let err = mgr
		.remove_sub_agent_planned("reviewer", false, true)
		.unwrap_err();
	// A save failure after file removal must surface as an error (not silent
	// success). The write to the directory-shaped keeper path fails either as a
	// raw IO error or, via the sub-agent symlink/overwrite hardening, as an
	// InvalidConfig "refusing to overwrite unsafe file" — both are save
	// failures that must trigger the transactional restore below.
	assert!(
		matches!(err, ConfigError::Io(_) | ConfigError::InvalidConfig(_)),
		"a save failure after file removal must surface as an error, \
		 got {err:?}"
	);

	// Transactional: the target file is RESTORED and the agent stays loaded, so
	// the reported failure means no data was lost.
	assert!(
		target_file.exists(),
		"save failure must restore the deleted backing file (no data loss)"
	);
	assert!(
		mgr.get_sub_agent("reviewer").is_some(),
		"save failure must leave the agent loaded in memory"
	);
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
fn renaming_to_an_existing_sub_agent_refuses_to_overwrite_it() {
	let tmp = tempfile::tempdir().unwrap();
	let root = tmp.path();
	let mut mgr = project_manager(root);
	mgr.add_sub_agent(agent_with_instruction("reviewer", "reviewer body"))
		.unwrap();
	mgr.add_sub_agent(agent_with_instruction("auditor", "auditor body"))
		.unwrap();
	let error = mgr
		.update_sub_agent(
			"reviewer",
			SubAgentPatch {
				name: Some("auditor".to_string()),
				..Default::default()
			},
		)
		.unwrap_err();
	assert!(matches!(error, ConfigError::ResourceExists { .. }));
	assert!(std::fs::read_to_string(agent_md_path(root, "reviewer"))
		.unwrap()
		.contains("reviewer body"));
	assert!(std::fs::read_to_string(agent_md_path(root, "auditor"))
		.unwrap()
		.contains("auditor body"));
}

#[test]
fn rename_to_a_new_file_keeps_the_body_and_removes_the_old_path() {
	let tmp = tempfile::tempdir().unwrap();
	let root = tmp.path();
	let mut manager = project_manager(root);
	manager
		.add_sub_agent(agent_with_instruction(
			"reviewer",
			"original instructions",
		))
		.unwrap();
	manager
		.update_sub_agent(
			"reviewer",
			SubAgentPatch {
				name: Some("auditor".into()),
				..Default::default()
			},
		)
		.unwrap();
	assert!(!agent_md_path(root, "reviewer").exists());
	let content =
		std::fs::read_to_string(agent_md_path(root, "auditor")).unwrap();
	assert!(content.contains("original instructions"), "{content}");
}

#[test]
fn rename_that_maps_to_the_same_file_keeps_the_only_copy() {
	let tmp = tempfile::tempdir().unwrap();
	let root = tmp.path();
	let mut manager = project_manager(root);
	manager
		.add_sub_agent(agent_with_instruction("Reviewer", "keep this body"))
		.unwrap();
	let file = agent_md_path(root, "reviewer");
	assert!(file.exists());
	manager
		.update_sub_agent(
			"Reviewer",
			SubAgentPatch {
				name: Some("reviewer".into()),
				..Default::default()
			},
		)
		.unwrap();
	let content = std::fs::read_to_string(&file).unwrap();
	assert!(content.contains("name: reviewer"), "{content}");
	assert!(content.contains("keep this body"), "{content}");
}

#[test]
fn existing_recovery_tombstone_is_not_overwritten() {
	// A previous interrupted removal may leave the only copy in this fixed
	// tombstone path. Refuse the next removal rather than rename over it.
	let tmp = tempfile::tempdir().unwrap();
	let root = tmp.path();
	let mut mgr = manager_with_persisted_agent(root, "reviewer");
	let source = agent_md_path(root, "reviewer");
	let tomb = source.with_extension("md.aghub-tomb");
	std::fs::write(&tomb, "older unrecovered data").unwrap();
	let error = mgr
		.remove_sub_agent_planned("reviewer", false, true)
		.unwrap_err();
	assert!(matches!(error, ConfigError::InvalidConfig(_)));
	assert_eq!(
		std::fs::read_to_string(&tomb).unwrap(),
		"older unrecovered data"
	);
	assert!(source.exists());
	assert!(mgr.get_sub_agent("reviewer").is_some());
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
