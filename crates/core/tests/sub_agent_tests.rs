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

// Unix-only: the ENOTDIR trick (remove_file on `<file>/agent.md`) yields a
// non-NotFound IO error only on unix. On Windows that path resolves to a
// NotFound, which the best-effort delete correctly treats as success — so the
// failure can't be forced there. The same contract is covered cross-platform
// on unix by remove_sub_agent_planned_stale_file_failure_errors_and_keeps_agent.
#[cfg(unix)]
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
	// Force the removal to fail (root-safe): park the real backing file in a
	// `0o555` dir so the tombstone rename inside it is blocked. A directory
	// swap no longer works — rename succeeds on a directory — so we block the
	// unlink/rename via dir perms, mirroring the rename-stale-file test below.
	use std::os::unix::fs::PermissionsExt;

	let tmp = tempfile::tempdir().unwrap();
	let root = tmp.path();
	if !perms_enforced(root) {
		eprintln!("skip: root bypasses 0o555");
		return;
	}
	let mut mgr = ConfigManager::new(
		create_adapter(AgentType::Claude),
		false,
		Some(root),
	);
	mgr.load().unwrap();

	// A real, existing backing file inside a dir we lock against rename/unlink.
	let locked_dir = root.join("locked");
	std::fs::create_dir(&locked_dir).unwrap();
	let file = locked_dir.join("reviewer.md");
	std::fs::write(&file, "real").unwrap();

	let mut agent = SubAgent::new("reviewer");
	agent.source_path = Some(file.to_string_lossy().into_owned());
	mgr.add_sub_agent(agent).unwrap();

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
	// Force the save to fail AFTER the target's file is removed: keep a second
	// agent loaded and pre-create ITS backing path as a directory, so the
	// save's `fs::write` to "<dir>/keeper.md" hits IsADirectory/ENOTDIR
	// deterministically (root-safe — no chmod).
	let tmp = tempfile::tempdir().unwrap();
	let root = tmp.path();
	let mut mgr = manager_with_persisted_agent(root, "reviewer");
	let target_file = agent_md_path(root, "reviewer");
	assert!(
		target_file.exists(),
		"precondition: target backing file written"
	);

	// A second agent that stays loaded; its save write is what we sabotage.
	mgr.add_sub_agent(SubAgent::new("keeper")).unwrap();
	let keeper_file = agent_md_path(root, "keeper");
	// Replace the keeper's file with a directory so `fs::write` to it fails
	// during save, AFTER the target file has already been removed.
	std::fs::remove_file(&keeper_file).unwrap();
	std::fs::create_dir(&keeper_file).unwrap();
	assert!(
		std::fs::write(&keeper_file, b"x").is_err(),
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

#[cfg(unix)]
#[test]
fn update_sub_agent_rename_stale_file_delete_failure_errors() {
	// Regression (Codex blocking): a rename writes the new-name `.md` then
	// deletes the OLD file. Because `save_scoped_sub_agents` never deletes stale
	// files, a swallowed old-file delete failure leaves the old `.md` on disk to
	// reappear as a phantom agent on reload while the API reports success. The
	// fix surfaces a non-NotFound delete failure as an actionable error.
	//
	// Isolate the OLD file's delete from the new-name write: the rename writes
	// the new `.md` into the canonical `.claude/agents` dir (must stay writable)
	// and deletes the old file at the agent's `source_path`. Park the old file
	// in a SEPARATE `0o555` dir so only the unlink is blocked, on an EXISTING
	// file. Root-skip + restore perms before asserting.
	use std::os::unix::fs::PermissionsExt;

	use aghub_core::manager::sub_agent::SubAgentPatch;

	let tmp = tempfile::tempdir().unwrap();
	let root = tmp.path();
	if !perms_enforced(root) {
		eprintln!("skip: root bypasses 0o555");
		return;
	}
	let mut mgr = ConfigManager::new(
		create_adapter(AgentType::Claude),
		false,
		Some(root),
	);
	mgr.load().unwrap();

	// A real, existing old file inside a dir we will lock against unlink.
	let locked_dir = root.join("locked");
	std::fs::create_dir(&locked_dir).unwrap();
	let old_file = locked_dir.join("reviewer.md");
	std::fs::write(&old_file, "stale").unwrap();

	let mut agent = SubAgent::new("reviewer");
	agent.source_path = Some(old_file.to_string_lossy().into_owned());
	mgr.add_sub_agent(agent).unwrap();

	let orig = std::fs::metadata(&locked_dir).unwrap().permissions();
	std::fs::set_permissions(
		&locked_dir,
		std::fs::Permissions::from_mode(0o555),
	)
	.unwrap();

	let res = mgr.update_sub_agent(
		"reviewer",
		SubAgentPatch {
			name: Some("auditor".to_string()),
			description: None,
			instruction: None,
		},
	);

	// RESTORE perms before any assertion so a failure can't leak the temp dir.
	std::fs::set_permissions(&locked_dir, orig).unwrap();

	let err = res.expect_err(
		"a failed stale-file delete on rename must surface, not be swallowed",
	);
	assert!(
		matches!(err, ConfigError::Io(_)),
		"undeletable stale file must surface an IO error, got {err:?}"
	);
	// The orphan old file is still on disk — surfacing the error lets the caller
	// know it lingers rather than silently claiming success.
	assert!(
		old_file.exists(),
		"stale file remains; the error is what tells the caller it lingers"
	);
}

#[test]
fn remove_sub_agent_planned_tombstone_cleanup_failure_is_not_clean_success() {
	// Site-3 regression: after a successful tombstone rename + save, the
	// post-success cleanup must NOT swallow a `remove_file` failure and report
	// a clean `executed:true` success. A surviving `.aghub-tomb` is litter the
	// caller should learn about, so a non-NotFound cleanup error surfaces.
	//
	// Deterministic, root-safe: point source_path at a DIRECTORY. The rename to
	// the tomb then makes the tomb a directory too, so `remove_file(tomb)` fails
	// with EISDIR while the rename + save still succeed.
	let tmp = tempfile::tempdir().unwrap();
	let root = tmp.path();
	let mut mgr = ConfigManager::new(
		create_adapter(AgentType::Claude),
		false,
		Some(root),
	);
	mgr.load().unwrap();

	// Backing "file" is actually a directory: rename moves it to a tomb dir,
	// which `remove_file` cannot delete.
	let backing_dir = root.join("backing.md");
	std::fs::create_dir(&backing_dir).unwrap();
	let tomb = root.join("backing.md.aghub-tomb");

	let mut agent = SubAgent::new("reviewer");
	agent.source_path = Some(backing_dir.to_string_lossy().into_owned());
	mgr.add_sub_agent(agent).unwrap();

	let err = mgr
		.remove_sub_agent_planned("reviewer", false, true)
		.expect_err("a tombstone cleanup failure must not be a clean success");
	assert!(
		matches!(err, ConfigError::Io(_)),
		"cleanup failure must surface an actionable IO error, got {err:?}"
	);
	// The removal itself happened (file moved out + saved): the leftover tomb
	// is exactly the litter the surfaced error tells the caller about.
	assert!(tomb.exists(), "the un-cleaned tombstone is left on disk");
	assert!(
		mgr.get_sub_agent("reviewer").is_none(),
		"the agent was genuinely removed before cleanup failed"
	);
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
