use super::*;
use crate::models::{AgentType, ResourceScope};
use crate::skills::prune::test_lock::env_lock;
#[cfg(unix)]
use crate::skills::removal;
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::tempdir;

struct EnvVarGuard(&'static str, Option<std::ffi::OsString>);

impl EnvVarGuard {
	fn set(key: &'static str, value: &Path) -> Self {
		let previous = std::env::var_os(key);
		std::env::set_var(key, value);
		Self(key, previous)
	}
}

impl Drop for EnvVarGuard {
	fn drop(&mut self) {
		match self.1.take() {
			Some(value) => std::env::set_var(self.0, value),
			None => std::env::remove_var(self.0),
		}
	}
}

fn isolate_env(temp: &tempfile::TempDir) -> (EnvVarGuard, EnvVarGuard) {
	let isolated_home = temp.path().join("home");
	let isolated_data = temp.path().join("data");
	fs::create_dir_all(&isolated_home).unwrap();
	fs::create_dir_all(&isolated_data).unwrap();
	(
		EnvVarGuard::set("HOME", &isolated_home),
		EnvVarGuard::set("AGHUB_DATA_DIR", &isolated_data),
	)
}

#[cfg(unix)]
fn setup_shared_fixture(root: &Path, name: &str) {
	crate::testing::master_with_claude_referrer(root, name);
	for dir in [".opencode", ".cursor", ".pi", ".grok", ".omp"] {
		let slot = root.join(dir).join("skills");
		fs::create_dir_all(&slot).unwrap();
		std::os::unix::fs::symlink(
			root.join(".aghub").join(name),
			slot.join(name),
		)
		.unwrap();
	}
	let lock_path = root.join("skills-lock.json");
	let lock_content = format!(
		r#"{{"version":1,"skills":{{"{name}":{{"source":"test","sourceType":"node_modules","computedHash":"abc123"}}}}}}"#
	);
	fs::write(lock_path, lock_content).unwrap();
}

#[cfg(unix)]
fn setup_master_and_referrers(root: &Path, name: &str, agents: &[&str]) {
	let master = root.join(".aghub").join(name);
	fs::create_dir_all(&master).unwrap();
	fs::write(
		master.join("SKILL.md"),
		format!("---\nname: {name}\ndescription: test\n---\n"),
	)
	.unwrap();
	for agent in agents {
		let skills_dir = root.join(format!(".{agent}/skills"));
		fs::create_dir_all(&skills_dir).unwrap();
		std::os::unix::fs::symlink(&master, skills_dir.join(name)).unwrap();
	}
}

#[cfg(unix)]
fn collect_normalized_tree(
	root: &Path,
	current: &Path,
	acc: &mut Vec<(PathBuf, String)>,
) {
	let read_dir = match fs::read_dir(current) {
		Ok(rd) => rd,
		Err(_) => return,
	};
	let mut entries: Vec<_> = read_dir.filter_map(|e| e.ok()).collect();
	entries.sort_by_key(|e| e.path());
	for entry in entries {
		let path = entry.path();
		let rel = path.strip_prefix(root).unwrap().to_path_buf();
		let meta = fs::symlink_metadata(&path).unwrap();
		if meta.file_type().is_symlink() {
			let target = fs::read_link(&path).unwrap();
			let norm_target = if let Ok(rel_target) = target.strip_prefix(root)
			{
				format!("<root>/{}", rel_target.display())
			} else {
				target.display().to_string()
			};
			acc.push((rel, format!("symlink -> {norm_target}")));
		} else if meta.is_dir() {
			acc.push((rel.clone(), "dir".to_string()));
			collect_normalized_tree(root, &path, acc);
		} else if meta.is_file() {
			let mut content = fs::read_to_string(&path).unwrap_or_default();
			content = content.replace(&root.display().to_string(), "<root>");
			acc.push((rel, format!("file: {content}")));
		}
	}
}

#[cfg(unix)]
#[test]
fn test_removal_ordering_independence() {
	let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());

	// Shared slot readers first
	let order_shared_first = vec![
		AgentType::Codex,
		AgentType::Antigravity,
		AgentType::Gemini,
		AgentType::Cline,
		AgentType::Copilot,
		AgentType::Kimi,
		AgentType::Amp,
		AgentType::Warp,
		AgentType::ZCode,
		AgentType::Dsh,
		AgentType::OpenCode,
		AgentType::Cursor,
		AgentType::Pi,
		AgentType::Grok,
		AgentType::Omp,
	];

	// Private readers first
	let order_private_first = vec![
		AgentType::OpenCode,
		AgentType::Cursor,
		AgentType::Pi,
		AgentType::Grok,
		AgentType::Omp,
		AgentType::Codex,
		AgentType::Antigravity,
		AgentType::Gemini,
		AgentType::Cline,
		AgentType::Copilot,
		AgentType::Kimi,
		AgentType::Amp,
		AgentType::Warp,
		AgentType::ZCode,
		AgentType::Dsh,
	];

	let temp_a = tempdir().unwrap();
	let root_a = temp_a.path().join("project");
	let res_a = {
		let _env_a = isolate_env(&temp_a);
		fs::create_dir_all(&root_a).unwrap();
		setup_shared_fixture(&root_a, "notebooklm");
		let req_a = SkillRemovalRequest {
			target: SkillRemovalTarget::ByName("notebooklm".to_string()),
			scope: ResourceScope::ProjectOnly,
			project_root: Some(root_a.clone()),
			agents: order_shared_first.clone(),
			dry_run: false,
			all_agents: false,
			prior_removed_paths: Vec::new(),
			keeps_master: false,
		};
		remove_skill_batch(&req_a)
			.expect("order_shared_first batch should succeed")
	};

	let temp_b = tempdir().unwrap();
	let root_b = temp_b.path().join("project");
	let res_b = {
		let _env_b = isolate_env(&temp_b);
		fs::create_dir_all(&root_b).unwrap();
		setup_shared_fixture(&root_b, "notebooklm");
		let req_b = SkillRemovalRequest {
			target: SkillRemovalTarget::ByName("notebooklm".to_string()),
			scope: ResourceScope::ProjectOnly,
			project_root: Some(root_b.clone()),
			agents: order_private_first.clone(),
			dry_run: false,
			all_agents: false,
			prior_removed_paths: Vec::new(),
			keeps_master: false,
		};
		remove_skill_batch(&req_b)
			.expect("order_private_first batch should succeed")
	};

	assert_eq!(res_a.rows.len(), 15);
	assert_eq!(res_b.rows.len(), 15);
	for row in &res_a.rows {
		assert_eq!(row.verdict, Verdict::Removed, "agent {:?}", row.agent);
		assert!(row.error.is_none());
	}
	for row in &res_b.rows {
		assert_eq!(row.verdict, Verdict::Removed, "agent {:?}", row.agent);
		assert!(row.error.is_none());
	}

	for agent in &order_shared_first {
		let v_a = res_a.rows.iter().find(|r| r.agent == *agent).unwrap();
		let v_b = res_b.rows.iter().find(|r| r.agent == *agent).unwrap();
		assert_eq!(v_a.verdict, v_b.verdict);
		assert_eq!(v_a.error, v_b.error);
	}

	let mut tree_a = Vec::new();
	collect_normalized_tree(&root_a, &root_a, &mut tree_a);
	tree_a.sort_by(|a, b| a.0.cmp(&b.0));

	let mut tree_b = Vec::new();
	collect_normalized_tree(&root_b, &root_b, &mut tree_b);
	tree_b.sort_by(|a, b| a.0.cmp(&b.0));

	assert_eq!(
		tree_a, tree_b,
		"root_a and root_b disk and lock state must be identical"
	);
	assert!(!tree_a.is_empty(), "tree must not be empty");

	let lock_a = fs::read_to_string(root_a.join("skills-lock.json")).unwrap();
	let lock_b = fs::read_to_string(root_b.join("skills-lock.json")).unwrap();
	assert_eq!(
		lock_a, lock_b,
		"skills-lock.json contents must match across both orderings"
	);
	assert!(
		lock_a.contains("notebooklm"),
		"lock entry for notebooklm must match across both orderings"
	);

	for &agent in &order_shared_first {
		let dirs_a = crate::create_adapter(agent)
			.get_skills_paths(Some(&root_a), ResourceScope::ProjectOnly);
		let eff_a = removal::read_effect_after(&dirs_a, "notebooklm", &[]);
		assert!(
			eff_a.survivors.is_empty(),
			"agent {:?} in a has survivors",
			agent
		);

		let dirs_b = crate::create_adapter(agent)
			.get_skills_paths(Some(&root_b), ResourceScope::ProjectOnly);
		let eff_b = removal::read_effect_after(&dirs_b, "notebooklm", &[]);
		assert!(
			eff_b.survivors.is_empty(),
			"agent {:?} in b has survivors",
			agent
		);
	}

	assert!(root_a.join(".claude/skills/notebooklm").exists());
	assert!(root_b.join(".claude/skills/notebooklm").exists());
	assert!(root_a.join(".aghub/notebooklm").exists());
	assert!(root_b.join(".aghub/notebooklm").exists());
}

#[cfg(unix)]
#[test]
fn test_prior_row_credit_turns_refused_into_removed() {
	let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
	let temp = tempdir().unwrap();
	let _env = isolate_env(&temp);
	let root = temp.path().join("project");
	fs::create_dir_all(&root).unwrap();
	setup_shared_fixture(&root, "notebooklm");

	let req_without_credit = SkillRemovalRequest {
		target: SkillRemovalTarget::ByName("notebooklm".to_string()),
		scope: ResourceScope::ProjectOnly,
		project_root: Some(root.clone()),
		agents: vec![AgentType::OpenCode],
		dry_run: true,
		all_agents: false,
		prior_removed_paths: Vec::new(),
		keeps_master: false,
	};
	let res_without = remove_skill_batch(&req_without_credit).unwrap();
	assert!(
		matches!(res_without.rows[0].verdict, Verdict::Refused { .. }),
		"expected Refused without prior credit, got {:?}",
		res_without.rows[0].verdict
	);

	let req_with_credit = SkillRemovalRequest {
		target: SkillRemovalTarget::ByName("notebooklm".to_string()),
		scope: ResourceScope::ProjectOnly,
		project_root: Some(root.clone()),
		agents: vec![AgentType::OpenCode],
		dry_run: true,
		all_agents: false,
		prior_removed_paths: vec![root.join(".agents/skills/notebooklm")],
		keeps_master: false,
	};
	let res_with = remove_skill_batch(&req_with_credit).unwrap();
	assert_eq!(
		res_with.rows[0].verdict,
		Verdict::Removed,
		"prior-row credit must turn row into Removed"
	);
}

#[cfg(unix)]
#[test]
fn test_internal_accumulation_credits_later_row() {
	let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
	let temp = tempdir().unwrap();
	let _env = isolate_env(&temp);
	let root = temp.path().join("project");
	fs::create_dir_all(&root).unwrap();
	setup_shared_fixture(&root, "notebooklm");

	// Disable all agents except Amp, OpenCode, and Claude (who keeps the Master).
	let ids: Vec<&str> = AgentType::ALL
		.iter()
		.map(|a| a.as_str())
		.filter(|id| *id != "opencode" && *id != "amp" && *id != "claude")
		.collect();
	let _off = crate::agent_settings::test_override::disable(&ids);

	// When asked alone, OpenCode (private reader) is Refused because .agents/skills/notebooklm still exists.
	let req_alone = SkillRemovalRequest {
		target: SkillRemovalTarget::ByName("notebooklm".to_string()),
		scope: ResourceScope::ProjectOnly,
		project_root: Some(root.clone()),
		agents: vec![AgentType::OpenCode],
		dry_run: true,
		all_agents: false,
		prior_removed_paths: Vec::new(),
		keeps_master: false,
	};
	let res_alone = remove_skill_batch(&req_alone).unwrap();
	assert!(
		matches!(res_alone.rows[0].verdict, Verdict::Refused { .. }),
		"expected Refused when asked alone, got {:?}",
		res_alone.rows[0].verdict
	);

	// When asked together (shared-slot writer Amp + private reader OpenCode),
	// earlier row (Amp) plans deletion of .agents/skills/notebooklm, crediting OpenCode internally
	// without caller-supplied prior_removed_paths.
	let req_batch = SkillRemovalRequest {
		target: SkillRemovalTarget::ByName("notebooklm".to_string()),
		scope: ResourceScope::ProjectOnly,
		project_root: Some(root.clone()),
		agents: vec![AgentType::Amp, AgentType::OpenCode],
		dry_run: true,
		all_agents: false,
		prior_removed_paths: Vec::new(),
		keeps_master: false,
	};
	let res_batch = remove_skill_batch(&req_batch).unwrap();
	let amp_row = res_batch
		.rows
		.iter()
		.find(|r| r.agent == AgentType::Amp)
		.unwrap();
	let opencode_row = res_batch
		.rows
		.iter()
		.find(|r| r.agent == AgentType::OpenCode)
		.unwrap();
	assert_eq!(amp_row.verdict, Verdict::Removed);
	assert_eq!(
		opencode_row.verdict,
		Verdict::Removed,
		"internal accumulation must turn OpenCode from Refused into Removed"
	);
}

#[cfg(unix)]
#[test]
fn test_whole_batch_preflight_rejection_writes_nothing() {
	let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
	let temp = tempdir().unwrap();
	let _env = isolate_env(&temp);
	let root = temp.path().join("project");
	fs::create_dir_all(&root).unwrap();
	setup_shared_fixture(&root, "notebooklm");

	let lock_path = root.join("skills-lock.json");
	let initial_lock_content =
		r#"{"version":1,"skills":{"notebooklm":{"source":"test"}}}"#;
	fs::write(&lock_path, initial_lock_content).unwrap();
	let initial_lock_bytes = fs::read(&lock_path).unwrap();

	let req = SkillRemovalRequest {
		target: SkillRemovalTarget::ByName("notebooklm".to_string()),
		scope: ResourceScope::ProjectOnly,
		project_root: Some(root.clone()),
		agents: vec![AgentType::Claude, AgentType::OpenCode],
		dry_run: false,
		all_agents: false,
		prior_removed_paths: Vec::new(),
		keeps_master: false,
	};

	let err = remove_skill_batch(&req).unwrap_err();
	let err_msg = err.to_string();
	assert!(
		err_msg.contains(
			"skill removal preflight failed; no removal was performed"
		),
		"message was: {err_msg}"
	);
	assert!(
		err_msg.contains("opencode"),
		"message must list rejected target: {err_msg}"
	);
	assert!(
		err_msg.contains("location shared with other agents"),
		"message must include refusal reason text: {err_msg}"
	);

	assert_eq!(
		fs::read(&lock_path).unwrap(),
		initial_lock_bytes,
		"lock file bytes must be unchanged after preflight rejection"
	);

	assert!(root.join(".claude/skills/notebooklm").exists());
	assert!(root.join(".agents/skills/notebooklm").exists());
	assert!(root.join(".opencode/skills/notebooklm").exists());
	assert!(root.join(".aghub/notebooklm").exists());
}

#[cfg(unix)]
#[test]
fn test_disabled_agent_removal_behavior() {
	let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
	let temp = tempdir().unwrap();
	let _env = isolate_env(&temp);
	let root = temp.path().join("project");
	fs::create_dir_all(&root).unwrap();

	setup_master_and_referrers(&root, "notebooklm", &["claude", "opencode"]);

	let mut _off =
		Some(crate::agent_settings::test_override::disable(&["opencode"]));

	let req_unnamed = SkillRemovalRequest {
		target: SkillRemovalTarget::ByName("notebooklm".to_string()),
		scope: ResourceScope::ProjectOnly,
		project_root: Some(root.clone()),
		agents: vec![AgentType::Claude],
		dry_run: false,
		all_agents: false,
		prior_removed_paths: Vec::new(),
		keeps_master: false,
	};
	let res_unnamed = remove_skill_batch(&req_unnamed).unwrap();
	assert_eq!(res_unnamed.rows[0].verdict, Verdict::Removed);

	assert!(!root.join(".claude/skills/notebooklm").exists());
	assert!(root.join(".opencode/skills/notebooklm").exists());
	assert!(root.join(".aghub/notebooklm").exists());

	// all_agents=true with empty agents list: disabled OpenCode is the ONLY remaining reader.
	// Its Referrer AND the Master must stay.
	let req_all_disabled = SkillRemovalRequest {
		target: SkillRemovalTarget::ByName("notebooklm".to_string()),
		scope: ResourceScope::ProjectOnly,
		project_root: Some(root.clone()),
		agents: Vec::new(),
		dry_run: false,
		all_agents: true,
		prior_removed_paths: Vec::new(),
		keeps_master: false,
	};
	let res_disabled = remove_skill_batch(&req_all_disabled).unwrap();
	assert!(res_disabled.rows.is_empty());
	assert!(
		root.join(".opencode/skills/notebooklm").exists(),
		"disabled holder Referrer must survive all_agents expansion"
	);
	assert!(
		root.join(".aghub/notebooklm").exists(),
		"Master must survive while disabled holder is the only remaining reader"
	);

	let req_named = SkillRemovalRequest {
		target: SkillRemovalTarget::ByName("notebooklm".to_string()),
		scope: ResourceScope::ProjectOnly,
		project_root: Some(root.clone()),
		agents: vec![AgentType::OpenCode],
		dry_run: false,
		all_agents: false,
		prior_removed_paths: Vec::new(),
		keeps_master: false,
	};
	let res_named = remove_skill_batch(&req_named).unwrap();
	assert_eq!(res_named.rows[0].verdict, Verdict::Removed);

	assert!(!root.join(".opencode/skills/notebooklm").exists());
	assert!(!root.join(".aghub/notebooklm").exists());

	// Compare with the same fixture where the agent is enabled:
	// Re-create the single-reader fixture with OpenCode enabled.
	setup_master_and_referrers(&root, "notebooklm", &["opencode"]);
	drop(_off.take());

	let req_all_enabled = SkillRemovalRequest {
		target: SkillRemovalTarget::ByName("notebooklm".to_string()),
		scope: ResourceScope::ProjectOnly,
		project_root: Some(root.clone()),
		agents: Vec::new(),
		dry_run: false,
		all_agents: true,
		prior_removed_paths: Vec::new(),
		keeps_master: false,
	};
	let res_enabled = remove_skill_batch(&req_all_enabled).unwrap();
	assert_eq!(res_enabled.rows.len(), 1);
	assert_eq!(res_enabled.rows[0].agent, AgentType::OpenCode);
	assert_eq!(res_enabled.rows[0].verdict, Verdict::Removed);
	assert!(
		!root.join(".opencode/skills/notebooklm").exists(),
		"enabled holder Referrer must be removed by all_agents expansion"
	);
	assert!(
		!root.join(".aghub/notebooklm").exists(),
		"Master must be GC'd when enabled holder is removed"
	);
}

#[cfg(unix)]
#[test]
fn test_master_gc_and_prune_failure_reported_independently() {
	let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
	let temp = tempdir().unwrap();
	let _env = isolate_env(&temp);
	let root = temp.path().join("project");
	fs::create_dir_all(&root).unwrap();

	setup_master_and_referrers(&root, "notebooklm", &["claude", "cursor"]);

	let req1 = SkillRemovalRequest {
		target: SkillRemovalTarget::ByName("notebooklm".to_string()),
		scope: ResourceScope::ProjectOnly,
		project_root: Some(root.clone()),
		agents: vec![AgentType::Claude],
		dry_run: false,
		all_agents: false,
		prior_removed_paths: Vec::new(),
		keeps_master: false,
	};
	let res1 = remove_skill_batch(&req1).unwrap();
	assert_eq!(res1.rows[0].verdict, Verdict::Removed);
	assert!(!root.join(".claude/skills/notebooklm").exists());
	assert!(root.join(".cursor/skills/notebooklm").exists());
	assert!(
		root.join(".aghub/notebooklm").exists(),
		"Master must stay while Cursor still holds it"
	);

	let lock_path = root.join("skills-lock.json");
	fs::write(&lock_path, "invalid json conflict {{{{").unwrap();

	let req2 = SkillRemovalRequest {
		target: SkillRemovalTarget::ByName("notebooklm".to_string()),
		scope: ResourceScope::ProjectOnly,
		project_root: Some(root.clone()),
		agents: vec![AgentType::Cursor],
		dry_run: false,
		all_agents: false,
		prior_removed_paths: Vec::new(),
		keeps_master: false,
	};
	let res2 = remove_skill_batch(&req2).unwrap();
	assert_eq!(res2.rows[0].verdict, Verdict::Removed);
	assert!(!root.join(".cursor/skills/notebooklm").exists());
	assert!(
		!root.join(".aghub/notebooklm").exists(),
		"Master must be GC'd when last referrer is removed"
	);
	assert!(
		matches!(res2.prune, PruneStatus::Failed { .. }),
		"expected PruneStatus::Failed, got {:?}",
		res2.prune
	);
}

#[cfg(unix)]
#[test]
fn test_preview_does_not_take_write_lock() {
	let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
	let temp = tempdir().unwrap();
	let _env = isolate_env(&temp);
	let root = temp.path().join("project");
	fs::create_dir_all(&root).unwrap();
	setup_shared_fixture(&root, "notebooklm");

	let (acquired_tx, acquired_rx) = std::sync::mpsc::channel();
	let (release_tx, release_rx) = std::sync::mpsc::channel();
	let root_buf = root.clone();

	let lock_thread = std::thread::spawn(move || {
		let _lock = crate::skills::lock::mutation_guard(
			"competing lock",
			ResourceScope::ProjectOnly,
			Some(&root_buf),
		)
		.unwrap();
		acquired_tx.send(()).unwrap();
		release_rx.recv().unwrap();
	});

	acquired_rx
		.recv_timeout(std::time::Duration::from_secs(5))
		.expect("competing lock must be acquired on another thread");

	let req_preview = SkillRemovalRequest {
		target: SkillRemovalTarget::ByName("notebooklm".to_string()),
		scope: ResourceScope::ProjectOnly,
		project_root: Some(root.clone()),
		agents: vec![AgentType::OpenCode],
		dry_run: true,
		all_agents: false,
		prior_removed_paths: vec![root.join(".agents/skills/notebooklm")],
		keeps_master: false,
	};

	let (preview_tx, preview_rx) = std::sync::mpsc::channel();
	let req_preview_clone = req_preview.clone();
	std::thread::spawn(move || {
		let res = remove_skill_batch(&req_preview_clone);
		let _ = preview_tx.send(res);
	});

	let res_preview = preview_rx
		.recv_timeout(std::time::Duration::from_secs(5))
		.expect("preview must not block on write lock held by another thread")
		.expect("preview should succeed");
	assert_eq!(res_preview.rows[0].verdict, Verdict::Removed);
	assert_eq!(res_preview.prune, PruneStatus::WouldPrune(vec![]));

	release_tx.send(()).unwrap();
	lock_thread.join().unwrap();

	let req_commit = SkillRemovalRequest {
		target: SkillRemovalTarget::ByName("notebooklm".to_string()),
		scope: ResourceScope::ProjectOnly,
		project_root: Some(root.clone()),
		agents: vec![AgentType::Claude],
		dry_run: false,
		all_agents: false,
		prior_removed_paths: Vec::new(),
		keeps_master: false,
	};
	let res_commit =
		remove_skill_batch(&req_commit).expect("commit should succeed");
	assert_eq!(res_commit.rows[0].verdict, Verdict::Removed);
	assert!(!root.join(".claude/skills/notebooklm").exists());
}

#[cfg(unix)]
#[test]
fn test_commit_follows_replan_after_disk_state_changes() {
	let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
	let temp = tempdir().unwrap();
	let _env = isolate_env(&temp);
	let root = temp.path().join("project");
	fs::create_dir_all(&root).unwrap();

	crate::testing::master_with_claude_referrer(&root, "notebooklm");
	fs::remove_file(root.join(".agents/skills/notebooklm")).unwrap();

	let (hook_tx, hook_rx) = std::sync::mpsc::channel();
	*COMMIT_PREFLIGHT_HOOK.lock().unwrap() = Some(hook_tx);

	struct HookReset;
	impl Drop for HookReset {
		fn drop(&mut self) {
			*COMMIT_PREFLIGHT_HOOK.lock().unwrap() = None;
		}
	}
	let _hook_reset = HookReset;

	// Test thread acquires the mutation write lock first.
	let test_lock = crate::skills::lock::mutation_guard(
		"test competing lock",
		ResourceScope::ProjectOnly,
		Some(&root),
	)
	.expect("test thread acquires mutation lock");

	let req_commit = SkillRemovalRequest {
		target: SkillRemovalTarget::ByName("notebooklm".to_string()),
		scope: ResourceScope::ProjectOnly,
		project_root: Some(root.clone()),
		agents: vec![AgentType::Claude],
		dry_run: false,
		all_agents: false,
		prior_removed_paths: Vec::new(),
		keeps_master: false,
	};

	// Spawn commit thread.
	// Commit will perform its unlocked preflight (which sees ONLY Claude holds notebooklm),
	// signals COMMIT_PREFLIGHT_HOOK, then attempts to acquire mutation write lock and BLOCKS!
	let (commit_tx, commit_rx) = std::sync::mpsc::channel();
	let req_commit_clone = req_commit.clone();
	let commit_thread = std::thread::spawn(move || {
		let res = remove_skill_batch(&req_commit_clone);
		let _ = commit_tx.send(res);
	});

	// Wait on hook: commit thread has completed preflight and is about to block on mutation lock.
	hook_rx
		.recv_timeout(std::time::Duration::from_secs(5))
		.expect("commit thread must signal hook after unlocked preflight");

	// While commit thread is blocked waiting for write lock, mutate disk state:
	// Cursor adds a referrer symlink to the Master!
	let cursor_dir = root.join(".cursor/skills");
	fs::create_dir_all(&cursor_dir).unwrap();
	std::os::unix::fs::symlink(
		root.join(".aghub/notebooklm"),
		cursor_dir.join("notebooklm"),
	)
	.unwrap();

	// Release the write lock so commit thread can proceed.
	drop(test_lock);

	// Commit acquires the lock, re-plans inside the lock, discovers Cursor now holds the skill,
	// removes Claude's referrer, but preserves the Master!
	let res_commit = commit_rx
		.recv_timeout(std::time::Duration::from_secs(5))
		.expect("commit thread should complete after lock release")
		.expect("commit should succeed");
	commit_thread.join().unwrap();

	assert_eq!(res_commit.rows[0].verdict, Verdict::Removed);
	assert!(
		!root.join(".claude/skills/notebooklm").exists(),
		"Claude referrer must be removed"
	);
	assert!(
		root.join(".cursor/skills/notebooklm").exists(),
		"Cursor referrer must remain"
	);
	assert!(
		root.join(".aghub/notebooklm").exists(),
		"Master must survive because Cursor was added while commit was blocked and commit re-planned inside lock"
	);
}

#[cfg(unix)]
#[test]
fn test_in_lock_replan_refuses_when_holder_becomes_unreadable() {
	let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
	let temp = tempdir().unwrap();
	let _env = isolate_env(&temp);
	let root = temp.path().join("project");
	fs::create_dir_all(&root).unwrap();

	setup_master_and_referrers(&root, "mover", &["codex"]);

	let (hook_tx, hook_rx) = std::sync::mpsc::channel();
	*COMMIT_PREFLIGHT_HOOK.lock().unwrap() = Some(hook_tx);

	struct HookReset;
	impl Drop for HookReset {
		fn drop(&mut self) {
			*COMMIT_PREFLIGHT_HOOK.lock().unwrap() = None;
		}
	}
	let _hook_reset = HookReset;

	// Acquire mutation lock first so commit thread blocks before entering lock.
	let test_lock = crate::skills::lock::mutation_guard(
		"test competing lock",
		ResourceScope::ProjectOnly,
		Some(&root),
	)
	.expect("test thread acquires mutation lock");

	let req_commit = SkillRemovalRequest {
		target: SkillRemovalTarget::ByName("mover".to_string()),
		scope: ResourceScope::ProjectOnly,
		project_root: Some(root.clone()),
		agents: vec![AgentType::Codex],
		dry_run: false,
		all_agents: true,
		prior_removed_paths: Vec::new(),
		keeps_master: false,
	};

	let (commit_tx, commit_rx) = std::sync::mpsc::channel();
	let req_commit_clone = req_commit.clone();
	let commit_thread = std::thread::spawn(move || {
		let res = remove_skill_batch(&req_commit_clone);
		let _ = commit_tx.send(res);
	});

	// Wait for commit thread to complete unlocked preflight and block on write lock.
	hook_rx
		.recv_timeout(std::time::Duration::from_secs(5))
		.expect("commit thread must signal hook after unlocked preflight");

	// While commit thread is blocked waiting for write lock, create unreadable Windsurf skills dir (symlink loop).
	fs::create_dir_all(root.join(".windsurf")).unwrap();
	std::os::unix::fs::symlink(
		std::path::Path::new("skills"),
		root.join(".windsurf/skills"),
	)
	.unwrap();

	// Release the write lock so commit thread can proceed.
	drop(test_lock);

	let res_commit = commit_rx
		.recv_timeout(std::time::Duration::from_secs(5))
		.expect("commit thread should complete after lock release");
	commit_thread.join().unwrap();

	let err = res_commit.expect_err(
		"in-lock replan must refuse when a holder's skills directory is unreadable",
	);
	let err_msg = err.to_string();
	assert!(
		err_msg.contains(
			"cannot decide whether removing 'mover' leaves the shared"
		),
		"message must match unreadable refusal: {err_msg}"
	);
	assert!(
		err_msg.contains("windsurf"),
		"message must name unreadable agent: {err_msg}"
	);
}

#[test]
fn test_dry_run_reports_preflight_failure_in_row_error() {
	let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
	let temp = tempdir().unwrap();
	let _env = isolate_env(&temp);
	let root = temp.path().join("project");
	fs::create_dir_all(&root).unwrap();

	let req = SkillRemovalRequest {
		target: SkillRemovalTarget::ByName("notebooklm".to_string()),
		scope: ResourceScope::ProjectOnly,
		project_root: Some(root.clone()),
		agents: vec![AgentType::JetBrainsAi],
		dry_run: true,
		all_agents: false,
		prior_removed_paths: Vec::new(),
		keeps_master: false,
	};

	let res = remove_skill_batch(&req).unwrap();
	assert_eq!(res.rows.len(), 1);
	assert_eq!(res.rows[0].agent, AgentType::JetBrainsAi);
	assert_eq!(res.rows[0].verdict, Verdict::Absent);
	let err = res.rows[0]
		.error
		.as_deref()
		.expect("dry run must report preflight failure in row error");
	assert!(
		err.contains("delete jetbrains-ai (project)"),
		"unexpected error message: {err}"
	);
	assert!(
		err.contains("no project skill config"),
		"unexpected error message: {err}"
	);
	assert!(matches!(
		res.rows[0].typed_error.as_deref(),
		Some(ConfigError::UnsupportedOperation { .. })
	));
}

#[cfg(unix)]
#[test]
fn test_commit_mode_ignores_prior_removed_paths() {
	let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
	let temp = tempdir().unwrap();
	let _env = isolate_env(&temp);
	let root = temp.path().join("project");
	fs::create_dir_all(&root).unwrap();
	setup_shared_fixture(&root, "notebooklm");

	let req = SkillRemovalRequest {
		target: SkillRemovalTarget::ByName("notebooklm".to_string()),
		scope: ResourceScope::ProjectOnly,
		project_root: Some(root.clone()),
		agents: vec![AgentType::OpenCode],
		dry_run: false,
		all_agents: false,
		prior_removed_paths: vec![root.join(".agents/skills/notebooklm")],
		keeps_master: false,
	};
	// In commit mode, prior_removed_paths is ignored so preflight fails because .agents/skills/notebooklm actually exists on disk.
	let err = remove_skill_batch(&req).unwrap_err();
	assert!(err.to_string().contains("skill removal preflight failed"));
}

#[cfg(unix)]
#[test]
fn test_keeps_master_skips_unreadable_holder_scan_refusal() {
	let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
	let temp = tempdir().unwrap();
	let _env = isolate_env(&temp);
	let root = temp.path().join("project");
	fs::create_dir_all(&root).unwrap();

	setup_master_and_referrers(&root, "mover", &["codex"]);

	fs::create_dir_all(root.join(".windsurf")).unwrap();
	std::os::unix::fs::symlink(
		std::path::Path::new("skills"),
		root.join(".windsurf/skills"),
	)
	.unwrap();

	// With keeps_master: false, it would be exhaustive and fail because Windsurf is unreadable.
	let req_false = SkillRemovalRequest {
		target: SkillRemovalTarget::ByName("mover".to_string()),
		scope: ResourceScope::ProjectOnly,
		project_root: Some(root.clone()),
		agents: vec![AgentType::Codex, AgentType::Windsurf],
		dry_run: true,
		all_agents: false,
		prior_removed_paths: Vec::new(),
		keeps_master: false,
	};
	assert!(remove_skill_batch(&req_false).is_err());

	// With keeps_master: true, scan is skipped, so it does NOT fail wholesale.
	let req_true = SkillRemovalRequest {
		target: SkillRemovalTarget::ByName("mover".to_string()),
		scope: ResourceScope::ProjectOnly,
		project_root: Some(root.clone()),
		agents: vec![AgentType::Codex, AgentType::Windsurf],
		dry_run: true,
		all_agents: false,
		prior_removed_paths: Vec::new(),
		keeps_master: true,
	};
	let res = remove_skill_batch(&req_true)
		.expect("keeps_master skips unreadable scan refusal");
	assert!(res.unreadable.is_empty());
}

#[cfg(unix)]
#[test]
fn test_in_lock_replan_holder_appearing_with_all_agents() {
	let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
	let temp = tempdir().unwrap();
	let _env = isolate_env(&temp);
	let root = temp.path().join("project");
	fs::create_dir_all(&root).unwrap();

	crate::testing::master_with_claude_referrer(&root, "notebooklm");
	fs::remove_file(root.join(".agents/skills/notebooklm")).unwrap();

	let (hook_tx, hook_rx) = std::sync::mpsc::channel();
	*COMMIT_PREFLIGHT_HOOK.lock().unwrap() = Some(hook_tx);

	struct HookReset;
	impl Drop for HookReset {
		fn drop(&mut self) {
			*COMMIT_PREFLIGHT_HOOK.lock().unwrap() = None;
		}
	}
	let _hook_reset = HookReset;

	// Test thread acquires the mutation write lock first.
	let test_lock = crate::skills::lock::mutation_guard(
		"test competing lock",
		ResourceScope::ProjectOnly,
		Some(&root),
	)
	.expect("test thread acquires mutation lock");

	let req_commit = SkillRemovalRequest {
		target: SkillRemovalTarget::ByName("notebooklm".to_string()),
		scope: ResourceScope::ProjectOnly,
		project_root: Some(root.clone()),
		agents: Vec::new(),
		dry_run: false,
		all_agents: true,
		prior_removed_paths: Vec::new(),
		keeps_master: false,
	};

	let (commit_tx, commit_rx) = std::sync::mpsc::channel();
	let req_commit_clone = req_commit.clone();
	let commit_thread = std::thread::spawn(move || {
		let res = remove_skill_batch(&req_commit_clone);
		let _ = commit_tx.send(res);
	});

	// Wait on hook: commit thread has completed preflight and will block on mutation lock.
	hook_rx
		.recv_timeout(std::time::Duration::from_secs(5))
		.expect("commit thread must signal hook after unlocked preflight");

	// While commit thread is blocked, add Cursor as a holder:
	let cursor_dir = root.join(".cursor/skills");
	fs::create_dir_all(&cursor_dir).unwrap();
	std::os::unix::fs::symlink(
		root.join(".aghub/notebooklm"),
		cursor_dir.join("notebooklm"),
	)
	.unwrap();

	// Release lock so commit thread proceeds:
	drop(test_lock);

	let res_commit = commit_rx
		.recv_timeout(std::time::Duration::from_secs(5))
		.expect("commit thread should complete after lock release")
		.expect("commit should succeed");
	commit_thread.join().unwrap();

	assert!(
		res_commit
			.rows
			.iter()
			.any(|r| r.agent == AgentType::Cursor
				&& r.verdict == Verdict::Removed),
		"Cursor must be in execution results and removed: {:?}",
		res_commit.rows
	);
	assert!(
		!root.join(".cursor/skills/notebooklm").exists(),
		"Cursor referrer must be removed"
	);
	assert!(
		!root.join(".claude/skills/notebooklm").exists(),
		"Claude referrer must be removed"
	);
	assert!(
		!root.join(".aghub/notebooklm").exists(),
		"Master must be GC'd when both holders are removed"
	);
}

#[test]
fn test_row_outcome_matches_removal_view_outcome_for_all_variants() {
	use crate::dto::{removal_kind_from_outcome, RemovalKind, RemovalView};
	use crate::skills::removal::{
		Holder, Layout, PruneStatus, RemovalOutcome, RemovalPlan, Verdict,
	};

	let test_cases = vec![
		// 1. Removed in commit mode
		(
			RemovalOutcome {
				plan: RemovalPlan {
					layout: Layout::Symlink,
					paths: vec![PathBuf::from("/path/to/skill")],
					skipped: vec![],
					needs_confirm: false,
					shared_master_kept: false,
					still_read_from: vec![],
					incomplete: false,
				},
				executed: true,
				prune: PruneStatus::NotRun,
				failed_paths: vec![],
				absent: false,
				verdict: Verdict::Removed,
			},
			false, // dry_run
			RemovalKind::Removed,
		),
		// 2. Preview in dry-run mode
		(
			RemovalOutcome {
				plan: RemovalPlan {
					layout: Layout::Symlink,
					paths: vec![PathBuf::from("/path/to/skill")],
					skipped: vec![],
					needs_confirm: false,
					shared_master_kept: false,
					still_read_from: vec![],
					incomplete: false,
				},
				executed: false,
				prune: PruneStatus::NotRun,
				failed_paths: vec![],
				absent: false,
				verdict: Verdict::Removed,
			},
			true, // dry_run
			RemovalKind::Preview,
		),
		// 3. Kept in commit mode
		(
			RemovalOutcome {
				plan: RemovalPlan {
					layout: Layout::Symlink,
					paths: vec![],
					skipped: vec![PathBuf::from("/path/to/master")],
					needs_confirm: false,
					shared_master_kept: true,
					still_read_from: vec![PathBuf::from("/path/to/master")],
					incomplete: false,
				},
				executed: false,
				prune: PruneStatus::NotRun,
				failed_paths: vec![],
				absent: false,
				verdict: Verdict::Kept {
					still_read_from: vec![Holder {
						path: PathBuf::from("/path/to/master"),
						managed: true,
					}],
				},
			},
			false, // dry_run
			RemovalKind::Kept,
		),
		// 4. Kept in dry-run mode
		(
			RemovalOutcome {
				plan: RemovalPlan {
					layout: Layout::Symlink,
					paths: vec![],
					skipped: vec![PathBuf::from("/path/to/master")],
					needs_confirm: false,
					shared_master_kept: true,
					still_read_from: vec![PathBuf::from("/path/to/master")],
					incomplete: false,
				},
				executed: false,
				prune: PruneStatus::NotRun,
				failed_paths: vec![],
				absent: false,
				verdict: Verdict::Kept {
					still_read_from: vec![Holder {
						path: PathBuf::from("/path/to/master"),
						managed: true,
					}],
				},
			},
			true, // dry_run
			RemovalKind::Kept,
		),
		// 5. Partial in commit mode
		(
			RemovalOutcome {
				plan: RemovalPlan {
					layout: Layout::Symlink,
					paths: vec![PathBuf::from("/path/to/skill")],
					skipped: vec![],
					needs_confirm: false,
					shared_master_kept: false,
					still_read_from: vec![],
					incomplete: false,
				},
				executed: true,
				prune: PruneStatus::NotRun,
				failed_paths: vec![PathBuf::from("/path/to/skill")],
				absent: false,
				verdict: Verdict::Partial,
			},
			false, // dry_run
			RemovalKind::Partial,
		),
		// 6. Absent
		(
			RemovalOutcome {
				plan: RemovalPlan {
					layout: Layout::Symlink,
					paths: vec![],
					skipped: vec![],
					needs_confirm: false,
					shared_master_kept: false,
					still_read_from: vec![],
					incomplete: false,
				},
				executed: false,
				prune: PruneStatus::NotRun,
				failed_paths: vec![],
				absent: true,
				verdict: Verdict::Absent,
			},
			false, // dry_run
			RemovalKind::Absent,
		),
	];

	for (outcome, dry_run, expected_kind) in test_cases {
		let row_kind = removal_kind_from_outcome(&outcome, dry_run);
		let view_kind = RemovalView::from_outcome(&outcome, dry_run).outcome;
		assert_eq!(row_kind, expected_kind);
		assert_eq!(row_kind, view_kind);
	}
}

#[cfg(unix)]
#[test]
fn test_master_reclaimed_preview_commit_and_surviving_master() {
	let dir = tempdir().unwrap();
	let root = dir.path().to_path_buf();
	let master = root.join(".aghub/reclaim-test");

	setup_master_and_referrers(&root, "reclaim-test", &["claude", "cursor"]);

	// 1. Exhaustive preview: both holders named in dry_run mode.
	// Master is NOT reclaimed (nothing written), but would_reclaim_master is reported in batch view.
	let req_preview = SkillRemovalRequest {
		target: SkillRemovalTarget::ByName("reclaim-test".to_string()),
		scope: ResourceScope::ProjectOnly,
		project_root: Some(root.clone()),
		agents: vec![AgentType::Claude, AgentType::Cursor],
		dry_run: true,
		all_agents: false,
		prior_removed_paths: Vec::new(),
		keeps_master: false,
	};
	let resp_preview = remove_skill_batch(&req_preview).unwrap();
	assert!(resp_preview.would_reclaim_master);
	assert!(!resp_preview.master_reclaimed);
	let view_preview = resp_preview.to_batch_view("reclaim-test", true);
	for r in &view_preview.results {
		let output = r.output.as_ref().unwrap();
		assert_eq!(output["master_reclaimed"], false);
		assert_eq!(output["would_reclaim_master"], true);
	}
	assert!(master.exists(), "master must survive preview");

	// 2. Non-exhaustive commit: only one of the two holders named.
	// Master survives because Claude still holds it.
	let req_non_exhaustive = SkillRemovalRequest {
		target: SkillRemovalTarget::ByName("reclaim-test".to_string()),
		scope: ResourceScope::ProjectOnly,
		project_root: Some(root.clone()),
		agents: vec![AgentType::Cursor],
		dry_run: false,
		all_agents: false,
		prior_removed_paths: Vec::new(),
		keeps_master: false,
	};
	let resp_non_exhaustive = remove_skill_batch(&req_non_exhaustive).unwrap();
	assert!(!resp_non_exhaustive.would_reclaim_master);
	assert!(!resp_non_exhaustive.master_reclaimed);
	let view_non_exhaustive =
		resp_non_exhaustive.to_batch_view("reclaim-test", false);
	for r in &view_non_exhaustive.results {
		let output = r.output.as_ref().unwrap();
		assert_eq!(output["master_reclaimed"], false);
		assert!(output.get("would_reclaim_master").is_none());
	}
	assert!(
		master.exists(),
		"master must survive when Claude still reads it"
	);

	// 3. Exhaustive commit: remaining holder named.
	// Master is reclaimed on disk, master_reclaimed is true.
	let req_exhaustive = SkillRemovalRequest {
		target: SkillRemovalTarget::ByName("reclaim-test".to_string()),
		scope: ResourceScope::ProjectOnly,
		project_root: Some(root.clone()),
		agents: vec![AgentType::Claude],
		dry_run: false,
		all_agents: false,
		prior_removed_paths: Vec::new(),
		keeps_master: false,
	};
	let resp_exhaustive = remove_skill_batch(&req_exhaustive).unwrap();
	assert!(!resp_exhaustive.would_reclaim_master);
	assert!(resp_exhaustive.master_reclaimed);
	let view_exhaustive = resp_exhaustive.to_batch_view("reclaim-test", false);
	for r in &view_exhaustive.results {
		let output = r.output.as_ref().unwrap();
		assert_eq!(output["master_reclaimed"], true);
		assert!(output.get("would_reclaim_master").is_none());
	}
	assert!(!master.exists(), "master must be gone on disk");

	// 4. Exhaustive preview when Master is already gone:
	// would_reclaim_master must be false because Master does not exist.
	let mut req_preview_no_master = req_preview.clone();
	req_preview_no_master.all_agents = true;
	let resp_preview_no_master =
		remove_skill_batch(&req_preview_no_master).unwrap();
	assert!(
		!resp_preview_no_master.would_reclaim_master,
		"would_reclaim_master must be false when Master does not exist"
	);
	let view_no_master =
		resp_preview_no_master.to_batch_view("reclaim-test", true);
	assert_eq!(view_no_master.results.len(), 2);
	for r in &view_no_master.results {
		let output = r.output.as_ref().unwrap();
		assert_eq!(output["master_reclaimed"], false);
		assert!(output.get("would_reclaim_master").is_none());
	}
}

#[cfg(unix)]
#[test]
fn test_orphan_master_reclaimed_with_all_agents_and_kept_without() {
	let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
	let temp = tempdir().unwrap();
	let (_h, _d) = isolate_env(&temp);
	let home = temp.path().join("home");
	let name = "orphan-skill";
	let master = home.join(".aghub").join(name);
	fs::create_dir_all(&master).unwrap();
	fs::write(
		master.join("SKILL.md"),
		format!("---\nname: {name}\ndescription: test\n---\n"),
	)
	.unwrap();

	// 1. Without all_agents: orphan master must be kept
	let req_without = SkillRemovalRequest {
		target: SkillRemovalTarget::ByName(name.to_string()),
		scope: ResourceScope::GlobalOnly,
		project_root: None,
		agents: vec![AgentType::Claude],
		dry_run: false,
		all_agents: false,
		prior_removed_paths: Vec::new(),
		keeps_master: false,
	};
	let res_without = remove_skill_batch(&req_without).unwrap();
	assert!(
		master.exists(),
		"orphan master must be kept when all_agents is false"
	);
	assert!(!res_without.master_reclaimed);

	// 2. Preview with all_agents: orphan master must be planned for removal
	let req_preview = SkillRemovalRequest {
		target: SkillRemovalTarget::ByName(name.to_string()),
		scope: ResourceScope::GlobalOnly,
		project_root: None,
		agents: vec![AgentType::Claude],
		dry_run: true,
		all_agents: true,
		prior_removed_paths: Vec::new(),
		keeps_master: false,
	};
	let res_preview = remove_skill_batch(&req_preview).unwrap();
	assert_eq!(
		res_preview.rows[0].outcome,
		crate::dto::RemovalKind::Preview,
		"orphan master must be planned as Preview when all_agents is true"
	);
	assert!(
		res_preview.rows[0].paths.contains(&master),
		"orphan master path must be planned for removal in preview"
	);
	assert!(master.exists(), "preview must not remove master from disk");

	// 3. Commit with all_agents: orphan master must be reclaimed
	let req_with = SkillRemovalRequest {
		target: SkillRemovalTarget::ByName(name.to_string()),
		scope: ResourceScope::GlobalOnly,
		project_root: None,
		agents: vec![AgentType::Claude],
		dry_run: false,
		all_agents: true,
		prior_removed_paths: Vec::new(),
		keeps_master: false,
	};
	let res_with = remove_skill_batch(&req_with).unwrap();
	assert_eq!(
		res_with.rows[0].outcome,
		crate::dto::RemovalKind::Removed,
		"commit with all_agents=true must report Removed outcome for orphan master"
	);
	assert!(
		!master.exists(),
		"orphan master must be reclaimed when all_agents is true"
	);
	assert!(res_with.master_reclaimed);
}

#[cfg(unix)]
#[test]
fn test_to_single_view_aggregates_rows_and_holders() {
	let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
	let temp = tempdir().unwrap();
	let (_h, _d) = isolate_env(&temp);
	let home = temp.path().join("home");
	let name = "single-view-test";
	setup_master_and_referrers(&home, name, &["claude", "cursor"]);

	let req = SkillRemovalRequest {
		target: SkillRemovalTarget::ByName(name.to_string()),
		scope: ResourceScope::GlobalOnly,
		project_root: None,
		agents: vec![AgentType::Claude],
		dry_run: false,
		all_agents: false,
		prior_removed_paths: Vec::new(),
		keeps_master: false,
	};
	let res = remove_skill_batch(&req).unwrap();
	let single = res.to_single_view(false).unwrap();
	assert!(single.removal_view.success);
	assert_eq!(
		single.removal_view.outcome,
		crate::dto::RemovalKind::Removed
	);
	assert!(!single.holders.is_empty());
	assert!(single.holders.all.contains(&"cursor".to_string()));
}

#[test]
fn test_to_single_view_on_partial_row_projects_success_false() {
	let row = SkillRemovalRow {
		agent: AgentType::Claude,
		verdict: Verdict::Partial,
		outcome: crate::dto::RemovalKind::Partial,
		error: Some("some path failed".to_string()),
		typed_error: Some(std::sync::Arc::new(ConfigError::InvalidConfig(
			"some path failed".to_string(),
		))),
		is_load_error: false,
		still_read_from: Vec::new(),
		paths: vec![PathBuf::from("/a/b")],
		skipped: Vec::new(),
		executed: true,
		needs_confirm: false,
	};
	let resp = SkillRemovalResponse {
		rows: vec![row],
		prune: PruneStatus::NotRun,
		keepers: Vec::new(),
		unreadable: Vec::new(),
		master_reclaimed: false,
		would_reclaim_master: false,
	};
	let single = resp
		.to_single_view(false)
		.expect("to_single_view must not treat Partial as fatal");
	assert!(!single.removal_view.success);
	assert_eq!(
		single.removal_view.outcome,
		crate::dto::RemovalKind::Partial
	);
	assert_eq!(single.removal_view.paths, vec!["/a/b".to_string()]);
}

#[test]
fn test_to_single_view_dry_run_propagates_plan_error() {
	let row = SkillRemovalRow {
		agent: AgentType::Cursor,
		verdict: Verdict::Absent,
		outcome: crate::dto::RemovalKind::Absent,
		error: Some("delete cursor: unsupported".to_string()),
		typed_error: Some(std::sync::Arc::new(ConfigError::unsupported_op(
			"repair needed",
		))),
		is_load_error: false,
		still_read_from: Vec::new(),
		paths: Vec::new(),
		skipped: Vec::new(),
		executed: false,
		needs_confirm: false,
	};
	let resp = SkillRemovalResponse {
		rows: vec![row],
		prune: PruneStatus::NotRun,
		keepers: Vec::new(),
		unreadable: Vec::new(),
		master_reclaimed: false,
		would_reclaim_master: false,
	};
	let err = resp
		.to_single_view(true)
		.expect_err("dry run must propagate plan error");
	assert!(matches!(err, ConfigError::UnsupportedOperation { .. }));
}

#[test]
fn test_clone_config_error_preserves_json() {
	let raw_json_err =
		serde_json::from_str::<serde_json::Value>("{invalid").unwrap_err();
	let expected_msg = raw_json_err.to_string();
	let err = ConfigError::Json(raw_json_err);
	let cloned = clone_config_error(&err);
	match &cloned {
		ConfigError::Json(e) => {
			assert_eq!(e.to_string(), expected_msg);
		}
		other => panic!("expected ConfigError::Json, got {other:?}"),
	}
}

#[cfg(unix)]
#[test]
fn test_dry_run_discloses_orphan_lock_prune() {
	let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
	let temp = tempdir().unwrap();
	let (_h, _d) = isolate_env(&temp);
	let root = temp.path().join("proj");
	fs::create_dir_all(root.join(".claude")).unwrap();

	let lock_path = root.join("skills-lock.json");
	let initial_raw = r#"{"version":1,"skills":{"orphan-skill":{"source":"o/r","sourceType":"github","computedHash":"deadbeef"}}}"#;
	fs::write(&lock_path, initial_raw).unwrap();

	let req = SkillRemovalRequest {
		target: SkillRemovalTarget::ByName("orphan-skill".to_string()),
		scope: ResourceScope::ProjectOnly,
		project_root: Some(root.clone()),
		agents: vec![AgentType::Claude],
		dry_run: true,
		all_agents: false,
		prior_removed_paths: Vec::new(),
		keeps_master: false,
	};
	let res = remove_skill_batch(&req).unwrap();
	match res.prune {
		PruneStatus::WouldPrune(ref keys) => {
			assert!(keys.contains(&"orphan-skill".to_string()));
		}
		other => panic!("expected WouldPrune, got {other:?}"),
	}
	assert_eq!(fs::read_to_string(&lock_path).unwrap(), initial_raw);
}
