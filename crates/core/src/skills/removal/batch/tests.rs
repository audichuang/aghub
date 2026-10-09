use super::*;
use crate::models::{AgentType, ResourceScope};
use crate::skills::prune::test_lock::env_lock;
#[cfg(unix)]
use crate::skills::removal;
use crate::WriteScope;
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
			scope: WriteScope::project(&root_a),
			agents: order_shared_first.clone(),
			dry_run: false,
			all_agents: false,
			prior_removed_paths: Vec::new(),
			keeps_master: false,
			plugin_roots: Vec::new(),
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
			scope: WriteScope::project(&root_b),
			agents: order_private_first.clone(),
			dry_run: false,
			all_agents: false,
			prior_removed_paths: Vec::new(),
			keeps_master: false,
			plugin_roots: Vec::new(),
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
		scope: WriteScope::project(&root),
		agents: vec![AgentType::OpenCode],
		dry_run: true,
		all_agents: false,
		prior_removed_paths: Vec::new(),
		keeps_master: false,
		plugin_roots: Vec::new(),
	};
	let res_without = remove_skill_batch(&req_without_credit).unwrap();
	assert!(
		matches!(res_without.rows[0].verdict, Verdict::Refused { .. }),
		"expected Refused without prior credit, got {:?}",
		res_without.rows[0].verdict
	);

	let req_with_credit = SkillRemovalRequest {
		target: SkillRemovalTarget::ByName("notebooklm".to_string()),
		scope: WriteScope::project(&root),
		agents: vec![AgentType::OpenCode],
		dry_run: true,
		all_agents: false,
		prior_removed_paths: vec![root.join(".agents/skills/notebooklm")],
		keeps_master: false,
		plugin_roots: Vec::new(),
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
		scope: WriteScope::project(&root),
		agents: vec![AgentType::OpenCode],
		dry_run: true,
		all_agents: false,
		prior_removed_paths: Vec::new(),
		keeps_master: false,
		plugin_roots: Vec::new(),
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
		scope: WriteScope::project(&root),
		agents: vec![AgentType::Amp, AgentType::OpenCode],
		dry_run: true,
		all_agents: false,
		prior_removed_paths: Vec::new(),
		keeps_master: false,
		plugin_roots: Vec::new(),
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
		scope: WriteScope::project(&root),
		agents: vec![AgentType::Claude, AgentType::OpenCode],
		dry_run: false,
		all_agents: false,
		prior_removed_paths: Vec::new(),
		keeps_master: false,
		plugin_roots: Vec::new(),
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
		scope: WriteScope::project(&root),
		agents: vec![AgentType::Claude],
		dry_run: false,
		all_agents: false,
		prior_removed_paths: Vec::new(),
		keeps_master: false,
		plugin_roots: Vec::new(),
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
		scope: WriteScope::project(&root),
		agents: Vec::new(),
		dry_run: false,
		all_agents: true,
		prior_removed_paths: Vec::new(),
		keeps_master: false,
		plugin_roots: Vec::new(),
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

	let single_disabled = res_disabled
		.to_single_view(false)
		.expect("to_single_view on disabled holders");
	assert_eq!(
		single_disabled.removal_view.outcome,
		crate::dto::RemovalKind::Kept,
		"projected outcome must be Kept when only disabled holders remain"
	);
	assert!(
		single_disabled.removal_view.success,
		"Kept outcome is reported as success: true"
	);
	let (_, _, unmanaged) = single_disabled.holders.to_options();
	assert_eq!(
		unmanaged,
		Some(vec!["opencode".to_string()]),
		"disabled holder must be reported in unmanaged holders"
	);

	let req_named = SkillRemovalRequest {
		target: SkillRemovalTarget::ByName("notebooklm".to_string()),
		scope: WriteScope::project(&root),
		agents: vec![AgentType::OpenCode],
		dry_run: false,
		all_agents: false,
		prior_removed_paths: Vec::new(),
		keeps_master: false,
		plugin_roots: Vec::new(),
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
		scope: WriteScope::project(&root),
		agents: Vec::new(),
		dry_run: false,
		all_agents: true,
		prior_removed_paths: Vec::new(),
		keeps_master: false,
		plugin_roots: Vec::new(),
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
		scope: WriteScope::project(&root),
		agents: vec![AgentType::Claude],
		dry_run: false,
		all_agents: false,
		prior_removed_paths: Vec::new(),
		keeps_master: false,
		plugin_roots: Vec::new(),
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
		scope: WriteScope::project(&root),
		agents: vec![AgentType::Cursor],
		dry_run: false,
		all_agents: false,
		prior_removed_paths: Vec::new(),
		keeps_master: false,
		plugin_roots: Vec::new(),
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
		scope: WriteScope::project(&root),
		agents: vec![AgentType::OpenCode],
		dry_run: true,
		all_agents: false,
		prior_removed_paths: vec![root.join(".agents/skills/notebooklm")],
		keeps_master: false,
		plugin_roots: Vec::new(),
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
		scope: WriteScope::project(&root),
		agents: vec![AgentType::Claude],
		dry_run: false,
		all_agents: false,
		prior_removed_paths: Vec::new(),
		keeps_master: false,
		plugin_roots: Vec::new(),
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
		scope: WriteScope::project(&root),
		agents: vec![AgentType::Claude],
		dry_run: false,
		all_agents: false,
		prior_removed_paths: Vec::new(),
		keeps_master: false,
		plugin_roots: Vec::new(),
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
		scope: WriteScope::project(&root),
		agents: vec![AgentType::Codex],
		dry_run: false,
		all_agents: true,
		prior_removed_paths: Vec::new(),
		keeps_master: false,
		plugin_roots: Vec::new(),
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
		scope: WriteScope::project(&root),
		agents: vec![AgentType::JetBrainsAi],
		dry_run: true,
		all_agents: false,
		prior_removed_paths: Vec::new(),
		keeps_master: false,
		plugin_roots: Vec::new(),
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
		scope: WriteScope::project(&root),
		agents: vec![AgentType::OpenCode],
		dry_run: false,
		all_agents: false,
		prior_removed_paths: vec![root.join(".agents/skills/notebooklm")],
		keeps_master: false,
		plugin_roots: Vec::new(),
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
		scope: WriteScope::project(&root),
		agents: vec![AgentType::Codex, AgentType::Windsurf],
		dry_run: true,
		all_agents: false,
		prior_removed_paths: Vec::new(),
		keeps_master: false,
		plugin_roots: Vec::new(),
	};
	assert!(remove_skill_batch(&req_false).is_err());

	// With keeps_master: true, scan is skipped, so it does NOT fail wholesale.
	let req_true = SkillRemovalRequest {
		target: SkillRemovalTarget::ByName("mover".to_string()),
		scope: WriteScope::project(&root),
		agents: vec![AgentType::Codex, AgentType::Windsurf],
		dry_run: true,
		all_agents: false,
		prior_removed_paths: Vec::new(),
		keeps_master: true,
		plugin_roots: Vec::new(),
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
		scope: WriteScope::project(&root),
		agents: Vec::new(),
		dry_run: false,
		all_agents: true,
		prior_removed_paths: Vec::new(),
		keeps_master: false,
		plugin_roots: Vec::new(),
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
		scope: WriteScope::project(&root),
		agents: vec![AgentType::Claude, AgentType::Cursor],
		dry_run: true,
		all_agents: false,
		prior_removed_paths: Vec::new(),
		keeps_master: false,
		plugin_roots: Vec::new(),
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
		scope: WriteScope::project(&root),
		agents: vec![AgentType::Cursor],
		dry_run: false,
		all_agents: false,
		prior_removed_paths: Vec::new(),
		keeps_master: false,
		plugin_roots: Vec::new(),
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
		scope: WriteScope::project(&root),
		agents: vec![AgentType::Claude],
		dry_run: false,
		all_agents: false,
		prior_removed_paths: Vec::new(),
		keeps_master: false,
		plugin_roots: Vec::new(),
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
		scope: WriteScope::Global,
		agents: vec![AgentType::Claude],
		dry_run: false,
		all_agents: false,
		prior_removed_paths: Vec::new(),
		keeps_master: false,
		plugin_roots: Vec::new(),
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
		scope: WriteScope::Global,
		agents: vec![AgentType::Claude],
		dry_run: true,
		all_agents: true,
		prior_removed_paths: Vec::new(),
		keeps_master: false,
		plugin_roots: Vec::new(),
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
		scope: WriteScope::Global,
		agents: vec![AgentType::Claude],
		dry_run: false,
		all_agents: true,
		prior_removed_paths: Vec::new(),
		keeps_master: false,
		plugin_roots: Vec::new(),
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
	assert_eq!(single.code, None);
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

#[cfg(unix)]
#[test]
fn test_by_path_matches_by_name_verdict_on_shared_slot() {
	let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
	let temp = tempdir().unwrap();
	let _env = isolate_env(&temp);
	let root = temp.path().join("project");
	fs::create_dir_all(&root).unwrap();

	let slot = root.join(".agents/skills/shared");
	fs::create_dir_all(&slot).unwrap();
	fs::write(
		slot.join("SKILL.md"),
		"---\nname: shared\ndescription: shared skill\n---\n",
	)
	.unwrap();

	// Cursor and OpenCode both read .agents/skills in project scope.
	let req_name = SkillRemovalRequest {
		target: SkillRemovalTarget::ByName("shared".to_string()),
		scope: WriteScope::project(&root),
		agents: vec![AgentType::Cursor],
		dry_run: true,
		all_agents: false,
		prior_removed_paths: Vec::new(),
		keeps_master: false,
		plugin_roots: Vec::new(),
	};
	let res_name = remove_skill_batch(&req_name).unwrap();

	let req_path = SkillRemovalRequest {
		target: SkillRemovalTarget::ByPath(slot.join("SKILL.md")),
		scope: WriteScope::project(&root),
		agents: vec![AgentType::Cursor],
		dry_run: true,
		all_agents: false,
		prior_removed_paths: Vec::new(),
		keeps_master: false,
		plugin_roots: Vec::new(),
	};
	let res_path = remove_skill_batch(&req_path).unwrap();

	assert_eq!(res_name.rows.len(), 1);
	assert_eq!(res_path.rows.len(), 1);
	assert_eq!(res_name.rows[0].verdict, res_path.rows[0].verdict);
	assert!(
		matches!(res_path.rows[0].verdict, Verdict::Refused { .. }),
		"verdict must be Refused, got {:?}",
		res_path.rows[0].verdict
	);
	assert!(res_path.rows[0].verdict.shared_master_kept());
	assert!(slot.join("SKILL.md").exists(), "dry run must not delete");
	assert_eq!(
		res_name.keepers, res_path.keepers,
		"by-path must report the same keepers as by-name"
	);
	assert!(
		!res_path.keepers.is_empty(),
		"by-path keepers must not be empty on shared slot refusal"
	);

	// Both must fail on commit with unsupported operation
	let mut commit_name = req_name.clone();
	commit_name.dry_run = false;
	let err_name = remove_skill_batch(&commit_name).unwrap_err();

	let mut commit_path = req_path.clone();
	commit_path.dry_run = false;
	let err_path = remove_skill_batch(&commit_path).unwrap_err();

	assert!(matches!(err_name, ConfigError::UnsupportedOperation { .. }));
	assert!(matches!(err_path, ConfigError::UnsupportedOperation { .. }));
	let name_targets = err_name
		.rejected_targets()
		.expect("rejected_targets on by-name");
	assert_eq!(name_targets[0].kind.as_deref(), Some("shared"));
	let readers = name_targets[0]
		.readers
		.as_ref()
		.expect("readers must be present on shared refusal");
	assert!(readers.iter().any(|r| r.agent == "opencode" && r.managed));
	assert!(!readers.iter().any(|r| r.agent == "cursor"));
	let path_targets = err_path
		.rejected_targets()
		.expect("rejected_targets on by-path");
	assert_eq!(path_targets[0].kind.as_deref(), Some("shared"));

	// 2. Non-Master copy referenced by an external referrer:
	// A private copy under .codex/skills referenced by a symlink under .claude/skills.
	let copy = root.join(".codex/skills/linked");
	fs::create_dir_all(&copy).unwrap();
	fs::write(
		copy.join("SKILL.md"),
		"---\nname: linked\ndescription: linked skill\n---\n",
	)
	.unwrap();
	let claude_skills = root.join(".claude/skills");
	fs::create_dir_all(&claude_skills).unwrap();
	std::os::unix::fs::symlink(&copy, claude_skills.join("linked")).unwrap();

	let req_name_ext = SkillRemovalRequest {
		target: SkillRemovalTarget::ByName("linked".to_string()),
		scope: WriteScope::project(&root),
		agents: vec![AgentType::Codex],
		dry_run: true,
		all_agents: false,
		prior_removed_paths: Vec::new(),
		keeps_master: false,
		plugin_roots: Vec::new(),
	};
	let res_name_ext = remove_skill_batch(&req_name_ext).unwrap();

	let req_path_ext = SkillRemovalRequest {
		target: SkillRemovalTarget::ByPath(copy.join("SKILL.md")),
		scope: WriteScope::project(&root),
		agents: vec![AgentType::Codex],
		dry_run: true,
		all_agents: false,
		prior_removed_paths: Vec::new(),
		keeps_master: false,
		plugin_roots: Vec::new(),
	};
	let res_path_ext = remove_skill_batch(&req_path_ext).unwrap();

	assert_eq!(res_name_ext.rows.len(), 1);
	assert_eq!(res_path_ext.rows.len(), 1);
	assert_eq!(res_name_ext.rows[0].verdict, res_path_ext.rows[0].verdict);
	assert!(
		matches!(res_path_ext.rows[0].verdict, Verdict::Kept { .. }),
		"verdict must be Kept, got {:?}",
		res_path_ext.rows[0].verdict
	);

	// Both must succeed on commit with outcome Kept (kept never commits or fails)
	let mut commit_name_ext = req_name_ext.clone();
	commit_name_ext.dry_run = false;
	let res_name_ext_commit = remove_skill_batch(&commit_name_ext).unwrap();

	let mut commit_path_ext = req_path_ext.clone();
	commit_path_ext.dry_run = false;
	let res_path_ext_commit = remove_skill_batch(&commit_path_ext).unwrap();

	assert_eq!(
		res_name_ext_commit.rows[0].verdict,
		res_path_ext_commit.rows[0].verdict
	);
	assert!(matches!(
		res_path_ext_commit.rows[0].verdict,
		Verdict::Kept { .. }
	));
	assert_eq!(
		res_name_ext_commit.keepers, res_path_ext_commit.keepers,
		"by-path commit must report the same keepers as by-name"
	);
	assert!(copy.join("SKILL.md").exists(), "copy dir must survive");
}

#[test]
fn test_by_path_refuses_dir_outside_allowed_roots() {
	let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
	let temp = tempdir().unwrap();
	let _env = isolate_env(&temp);
	let root = temp.path().join("project");
	let outside = temp.path().join("outside/foo");
	fs::create_dir_all(&outside).unwrap();
	fs::write(outside.join("SKILL.md"), "---\nname: foo\n---\n").unwrap();

	let req = SkillRemovalRequest {
		target: SkillRemovalTarget::ByPath(outside.join("SKILL.md")),
		scope: WriteScope::project(&root),
		agents: vec![AgentType::Claude],
		dry_run: false,
		all_agents: false,
		prior_removed_paths: Vec::new(),
		keeps_master: false,
		plugin_roots: Vec::new(),
	};
	let err = remove_skill_batch(&req)
		.expect_err("must refuse to remove a dir outside roots");
	match err {
		ConfigError::InvalidConfig(msg) => {
			assert!(
				msg.contains("not strictly inside an allow-listed skills root"),
				"unexpected error message: {msg}"
			);
		}
		other => panic!("expected InvalidConfig, got {other:?}"),
	}
	assert!(outside.exists(), "out-of-root dir must survive");
}

#[test]
fn test_by_path_removes_contained_dir() {
	let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
	let temp = tempdir().unwrap();
	let _env = isolate_env(&temp);
	let root = temp.path().join("project");
	let skills = root.join(".claude/skills");
	let foo = skills.join("foo");
	fs::create_dir_all(&foo).unwrap();
	fs::write(
		foo.join("SKILL.md"),
		"---\nname: foo\ndescription: f\n---\n",
	)
	.unwrap();

	let req = SkillRemovalRequest {
		target: SkillRemovalTarget::ByPath(foo.join("SKILL.md")),
		scope: WriteScope::project(&root),
		agents: vec![AgentType::Claude],
		dry_run: false,
		all_agents: false,
		prior_removed_paths: Vec::new(),
		keeps_master: false,
		plugin_roots: Vec::new(),
	};
	let res = remove_skill_batch(&req);
	assert!(
		res.is_ok(),
		"contained skill dir should be removed: {:?}",
		res
	);
	assert!(!foo.exists(), "a contained skill dir is removed normally");
}

#[test]
fn test_by_path_repeated_delete_returns_absent_outcome() {
	let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
	let temp = tempdir().unwrap();
	let _env = isolate_env(&temp);
	let root = temp.path().join("project");
	let skills = root.join(".claude/skills");
	let foo = skills.join("foo");
	fs::create_dir_all(&foo).unwrap();
	fs::write(
		foo.join("SKILL.md"),
		"---\nname: foo\ndescription: f\n---\n",
	)
	.unwrap();

	let req = SkillRemovalRequest {
		target: SkillRemovalTarget::ByPath(foo.join("SKILL.md")),
		scope: WriteScope::project(&root),
		agents: vec![AgentType::Claude],
		dry_run: false,
		all_agents: false,
		prior_removed_paths: Vec::new(),
		keeps_master: false,
		plugin_roots: Vec::new(),
	};
	let first = remove_skill_batch(&req).expect("first removal should succeed");
	assert!(
		!foo.exists(),
		"contained skill dir must be removed on first call"
	);
	assert_eq!(first.rows.len(), 1);
	assert_eq!(first.rows[0].outcome, crate::dto::RemovalKind::Removed);

	// Repeated by-path request on the now-absent skill
	let second =
		remove_skill_batch(&req).expect("repeated removal should succeed");
	assert_eq!(second.rows.len(), 1);
	assert_eq!(second.rows[0].outcome, crate::dto::RemovalKind::Absent);
	assert_eq!(second.rows[0].verdict, Verdict::Absent);
	assert!(
		second.rows[0].error.is_none(),
		"absent row must not have an error"
	);
	assert!(
		second
			.rows
			.iter()
			.all(|r| r.outcome == crate::dto::RemovalKind::Absent),
		"every row outcome must be Absent"
	);
	assert!(
		second.rows.iter().all(|r| r.error.is_none()),
		"every row must have no error"
	);
}

#[cfg(windows)]
#[test]
fn test_by_path_unlinks_junction_keeps_master() {
	use crate::skills::linker::create_junction;
	let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
	let temp = tempdir().unwrap();
	let _env = isolate_env(&temp);
	let root = temp.path().join("project");
	let master = root.join(".aghub/foo");
	fs::create_dir_all(&master).unwrap();
	fs::write(master.join("SKILL.md"), "---\nname: foo\n---\n").unwrap();

	let claude = root.join(".claude/skills");
	fs::create_dir_all(&claude).unwrap();
	let claude_link = claude.join("foo");
	let abs_master = master.canonicalize().unwrap();
	create_junction(&abs_master, &claude_link).unwrap();

	// A second reader (cursor) keeps the Master alive
	let cursor = root.join(".cursor/skills");
	fs::create_dir_all(&cursor).unwrap();
	let cursor_link = cursor.join("foo");
	create_junction(&abs_master, &cursor_link).unwrap();

	let req = SkillRemovalRequest {
		target: SkillRemovalTarget::ByPath(claude_link.join("SKILL.md")),
		scope: WriteScope::project(&root),
		agents: vec![AgentType::Claude],
		dry_run: false,
		all_agents: false,
		prior_removed_paths: Vec::new(),
		keeps_master: false,
		plugin_roots: Vec::new(),
	};
	remove_skill_batch(&req).unwrap();

	assert!(
		fs::symlink_metadata(&claude_link).is_err(),
		"junction must be unlinked"
	);
	assert!(
		master.join("SKILL.md").exists(),
		"shared Master must survive (remove_dir, not remove_dir_all)"
	);
}

#[test]
fn test_by_path_refuses_dotdot_escape() {
	let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
	let temp = tempdir().unwrap();
	let _env = isolate_env(&temp);
	let root = temp.path().join("project");
	let skills = root.join(".claude/skills");
	fs::create_dir_all(&skills).unwrap();
	let outside = temp.path().join("outside/foo");
	fs::create_dir_all(&outside).unwrap();
	fs::write(
		outside.join("SKILL.md"),
		"---\nname: foo\ndescription: f\n---\n",
	)
	.unwrap();

	let req = SkillRemovalRequest {
		target: SkillRemovalTarget::ByPath(
			skills.join("../../../outside/foo/SKILL.md"),
		),
		scope: WriteScope::project(&root),
		agents: vec![AgentType::Claude],
		dry_run: false,
		all_agents: false,
		prior_removed_paths: Vec::new(),
		keeps_master: false,
		plugin_roots: Vec::new(),
	};
	let err =
		remove_skill_batch(&req).expect_err("must refuse .. escaping roots");
	match err {
		ConfigError::InvalidConfig(msg) => {
			assert!(
				msg.contains("must not contain '..'"),
				"expected 'must not contain ..' error, got {msg}"
			);
		}
		other => panic!("expected InvalidConfig, got {other:?}"),
	}
	assert!(
		outside.join("SKILL.md").exists(),
		"outside skill must survive"
	);
}

#[test]
fn test_by_path_refuses_skills_root_itself() {
	let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
	let temp = tempdir().unwrap();
	let _env = isolate_env(&temp);
	let root = temp.path().join("project");
	let skills = root.join(".claude/skills");
	fs::create_dir_all(&skills).unwrap();
	fs::write(
		skills.join("SKILL.md"),
		"---\nname: skills\ndescription: skills root treated as skill\n---\n",
	)
	.unwrap();

	let mut req = SkillRemovalRequest {
		target: SkillRemovalTarget::ByPath(skills.clone()),
		scope: WriteScope::project(&root),
		agents: vec![AgentType::Claude],
		dry_run: true,
		all_agents: false,
		prior_removed_paths: Vec::new(),
		keeps_master: false,
		plugin_roots: Vec::new(),
	};
	let preview_err = remove_skill_batch(&req)
		.expect_err("must refuse skills root itself in dry-run");
	match preview_err {
		ConfigError::InvalidConfig(msg) => {
			assert!(
				msg.contains("not strictly inside an allow-listed skills root"),
				"unexpected error message: {msg}"
			);
		}
		other => panic!("expected InvalidConfig, got {other:?}"),
	}

	req.dry_run = false;
	let res = remove_skill_batch(&req)
		.expect_err("must refuse skills root itself in commit");
	match res {
		ConfigError::InvalidConfig(msg) => {
			assert!(
				msg.contains("not strictly inside an allow-listed skills root"),
				"unexpected error message: {msg}"
			);
		}
		other => panic!("expected InvalidConfig, got {other:?}"),
	}
	assert!(skills.exists(), "skills root must survive");
	assert!(
		skills.join("SKILL.md").exists(),
		"SKILL.md inside skills root must survive"
	);
}

#[test]
fn test_by_path_refuses_plugin_owned_skill_leaving_disk_unchanged() {
	let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
	let temp = tempdir().unwrap();
	let _env = isolate_env(&temp);
	let root = temp.path().join("project");
	let skills = root.join(".claude/skills");
	let plugin_skill = skills.join("my-plugin-skill");
	fs::create_dir_all(&plugin_skill).unwrap();
	fs::write(
		plugin_skill.join("SKILL.md"),
		"---\nname: my-plugin-skill\n---\n",
	)
	.unwrap();

	let req = SkillRemovalRequest {
		target: SkillRemovalTarget::ByPath(plugin_skill.join("SKILL.md")),
		scope: WriteScope::project(&root),
		agents: vec![AgentType::Claude],
		dry_run: false,
		all_agents: false,
		prior_removed_paths: Vec::new(),
		keeps_master: false,
		plugin_roots: vec![(
			"claude-official-plugin".to_string(),
			plugin_skill.clone(),
		)],
	};
	let res = remove_skill_batch(&req);
	let err = res.expect_err("must refuse plugin-owned skill");
	assert_eq!(crate::error_codes::wire_code(&err), "MANAGED_RESOURCE");
	assert!(
		plugin_skill.join("SKILL.md").exists(),
		"disk must remain unchanged"
	);
}

#[test]
fn test_by_path_refuses_plugin_skill_outside_agent_dirs_as_managed_resource() {
	let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
	let temp = tempdir().unwrap();
	let _env = isolate_env(&temp);
	let root = temp.path().join("project");
	fs::create_dir_all(root.join(".claude/skills")).unwrap();
	// Where Claude Code actually installs plugins: outside every allow-listed
	// skills root, so containment alone would answer INVALID_CONFIG.
	let plugin_root = temp.path().join("home/.claude/plugins/cache/p/1.0.0");
	let plugin_skill = plugin_root.join("skills/my-plugin-skill");
	fs::create_dir_all(&plugin_skill).unwrap();
	fs::write(
		plugin_skill.join("SKILL.md"),
		"---\nname: my-plugin-skill\n---\n",
	)
	.unwrap();

	let req = SkillRemovalRequest {
		target: SkillRemovalTarget::ByPath(plugin_skill.join("SKILL.md")),
		scope: WriteScope::project(&root),
		agents: vec![AgentType::Claude],
		dry_run: false,
		all_agents: false,
		prior_removed_paths: Vec::new(),
		keeps_master: false,
		plugin_roots: vec![("p".to_string(), plugin_root.clone())],
	};
	let err =
		remove_skill_batch(&req).expect_err("must refuse plugin-owned skill");
	assert_eq!(crate::error_codes::wire_code(&err), "MANAGED_RESOURCE");
	assert!(
		plugin_skill.join("SKILL.md").exists(),
		"disk must remain unchanged"
	);
}

#[test]
fn test_by_name_refuses_plugin_owned_skill_leaving_disk_and_lock_unchanged() {
	let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
	let temp = tempdir().unwrap();
	let _env = isolate_env(&temp);
	let root = temp.path().join("project");
	let skills = root.join(".claude/skills");
	let plugin_skill = skills.join("my-plugin-skill");
	fs::create_dir_all(&plugin_skill).unwrap();
	fs::write(
		plugin_skill.join("SKILL.md"),
		"---\nname: my-plugin-skill\ndescription: plugin skill\n---\n",
	)
	.unwrap();
	let lock_path = root.join("skills-lock.json");
	fs::write(
		&lock_path,
		r#"{"version":1,"skills":{"my-plugin-skill":{"source":"test","sourceType":"node_modules","computedHash":"abc123"}}}"#,
	)
	.unwrap();
	let lock_before = fs::read(&lock_path).unwrap();

	let req = SkillRemovalRequest {
		target: SkillRemovalTarget::ByName("my-plugin-skill".to_string()),
		scope: WriteScope::project(&root),
		agents: vec![AgentType::Claude],
		dry_run: false,
		all_agents: false,
		prior_removed_paths: Vec::new(),
		keeps_master: false,
		plugin_roots: vec![(
			"claude-official-plugin".to_string(),
			plugin_skill.clone(),
		)],
	};
	let err = remove_skill_batch(&req)
		.expect_err("must refuse plugin-owned skill by name");
	assert_eq!(crate::error_codes::wire_code(&err), "MANAGED_RESOURCE");
	assert!(
		plugin_skill.join("SKILL.md").exists(),
		"disk must remain unchanged"
	);
	assert_eq!(
		fs::read(&lock_path).unwrap(),
		lock_before,
		"lock must remain unchanged"
	);
}

#[test]
fn test_by_name_shared_refusal_excludes_all_requested_agents_from_readers() {
	let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
	let temp = tempdir().unwrap();
	let _env = isolate_env(&temp);
	let root = temp.path().join("project");
	fs::create_dir_all(&root).unwrap();

	let slot = root.join(".agents/skills/shared-skill");
	fs::create_dir_all(&slot).unwrap();
	fs::write(
		slot.join("SKILL.md"),
		"---\nname: shared-skill\ndescription: shared skill\n---\n",
	)
	.unwrap();

	// Cursor and OpenCode are the requested managed agents.
	// Disable codex (which also reads project .agents/skills), leaving the remaining
	// structural readers (amp, cline, copilot, etc.) managed so the shared refusal fires.
	let _off = crate::agent_settings::test_override::disable(&["codex"]);

	// Request by-name delete for both managed agents (Cursor and OpenCode)
	let req = SkillRemovalRequest {
		target: SkillRemovalTarget::ByName("shared-skill".to_string()),
		scope: WriteScope::project(&root),
		agents: vec![AgentType::Cursor, AgentType::OpenCode],
		dry_run: false,
		all_agents: false,
		prior_removed_paths: Vec::new(),
		keeps_master: false,
		plugin_roots: Vec::new(),
	};
	let err = remove_skill_batch(&req).unwrap_err();
	let rejected = err.rejected_targets().expect("rejected_targets on refusal");

	// Both cursor and opencode targets should exclude each other from readers (pinning Finding 2),
	// and surviving readers (like disabled codex) must be listed.
	assert_eq!(rejected.len(), 2);
	for target in rejected {
		assert_eq!(target.kind.as_deref(), Some("shared"));
		let readers = target.readers.as_ref().expect("readers must be present");
		assert!(!readers.is_empty(), "readers must be non-empty");
		// Must NOT contain requested agents cursor or opencode
		assert!(!readers.iter().any(|r| r.agent == "cursor"));
		assert!(!readers.iter().any(|r| r.agent == "opencode"));
		// Must contain disabled reader codex
		assert!(readers.iter().any(|r| r.agent == "codex" && !r.managed));
	}
}

#[test]
fn test_lock_only_skill_removal_prune_preview_and_commit_parity() {
	let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
	let temp = tempdir().unwrap();
	let _env = isolate_env(&temp);
	let root = temp.path().join("project");
	fs::create_dir_all(&root).unwrap();

	let lock_path = root.join("skills-lock.json");
	let lock_content = r#"{"version":1,"skills":{"orphan-skill":{"source":"test","sourceType":"node_modules","computedHash":"abc123"}}}"#;
	fs::write(&lock_path, lock_content).unwrap();

	// 1. Preview: lock has 'orphan-skill', nothing on disk.
	let preview_req = SkillRemovalRequest {
		target: SkillRemovalTarget::ByName("orphan-skill".to_string()),
		scope: WriteScope::project(&root),
		agents: vec![AgentType::Claude],
		dry_run: true,
		all_agents: false,
		prior_removed_paths: Vec::new(),
		keeps_master: false,
		plugin_roots: Vec::new(),
	};
	let preview_res =
		remove_skill_batch(&preview_req).expect("preview removal must succeed");

	let single_preview = preview_res
		.to_single_view(true)
		.expect("to_single_view on preview");
	let mut preview_payload =
		serde_json::to_value(&single_preview.removal_view).unwrap();
	apply_prune_fields(&mut preview_payload, &single_preview.prune);

	assert_eq!(
		preview_payload["would_prune_lock_entries"],
		serde_json::json!(["orphan-skill"]),
		"preview must advertise would_prune_lock_entries"
	);
	assert!(
		preview_payload.get("pruned_lock_entries").is_none(),
		"preview must not set pruned_lock_entries"
	);

	let lock_bytes_before = fs::read_to_string(&lock_path).unwrap();
	assert!(
		lock_bytes_before.contains("orphan-skill"),
		"preview must not mutate the lock file"
	);

	// 2. Commit: execute removal on the same lock-only skill.
	let commit_req = SkillRemovalRequest {
		target: SkillRemovalTarget::ByName("orphan-skill".to_string()),
		scope: WriteScope::project(&root),
		agents: vec![AgentType::Claude],
		dry_run: false,
		all_agents: false,
		prior_removed_paths: Vec::new(),
		keeps_master: false,
		plugin_roots: Vec::new(),
	};
	let commit_res =
		remove_skill_batch(&commit_req).expect("commit removal must succeed");

	let single_commit = commit_res
		.to_single_view(false)
		.expect("to_single_view on commit");
	let mut commit_payload =
		serde_json::to_value(&single_commit.removal_view).unwrap();
	apply_prune_fields(&mut commit_payload, &single_commit.prune);

	assert_eq!(
		commit_payload["pruned_lock_entries"],
		serde_json::json!(["orphan-skill"]),
		"commit must report pruned_lock_entries"
	);
	assert!(
		commit_payload.get("would_prune_lock_entries").is_none(),
		"commit must not set would_prune_lock_entries"
	);

	let lock_bytes_after = fs::read_to_string(&lock_path).unwrap();
	assert!(
		!lock_bytes_after.contains("orphan-skill"),
		"commit must prune orphan-skill from the lock file"
	);
}

#[cfg(unix)]
#[test]
fn test_by_name_multi_agent_removed_and_kept_folds_to_partial() {
	let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
	let temp = tempdir().unwrap();
	let _env = isolate_env(&temp);
	let root = temp.path().join("project");
	fs::create_dir_all(&root).unwrap();

	let name = "shared-skill";
	setup_master_and_referrers(&root, name, &["claude"]);

	let slot = root.join(".agents/skills").join(name);
	fs::create_dir_all(slot.parent().unwrap()).unwrap();
	std::os::unix::fs::symlink(root.join(".aghub").join(name), &slot).unwrap();

	let claude_referrer = root.join(".claude/skills").join(name);
	assert!(claude_referrer.exists());
	assert!(slot.exists());

	let req = SkillRemovalRequest {
		target: SkillRemovalTarget::ByName(name.to_string()),
		scope: WriteScope::project(&root),
		agents: vec![AgentType::Claude, AgentType::Cursor],
		dry_run: false,
		all_agents: false,
		prior_removed_paths: Vec::new(),
		keeps_master: false,
		plugin_roots: Vec::new(),
	};

	let res =
		remove_skill_batch(&req).expect("commit batch removal should succeed");
	let single = res
		.to_single_view(false)
		.expect("to_single_view must project the mixed outcome");

	assert_eq!(
		single.removal_view.outcome,
		crate::dto::RemovalKind::Partial,
		"mixed Removed and Kept must fold to Partial outcome"
	);
	assert!(
		!single.removal_view.success,
		"partial outcome must report success: false"
	);

	assert!(
		!claude_referrer.exists(),
		"Claude's private Referrer must be deleted"
	);
	assert!(
		slot.exists(),
		"Shared Referrer must survive for unnamed reader OpenCode"
	);
	assert!(
		root.join(".aghub").join(name).exists(),
		"Master must survive while shared Referrer exists"
	);
}
