//! Integration tests for the no-network `install_fetched_skill_and_lock`
//! primitive (Phase 2 of the CLI sources work).
//!
//! These exercise the per-agent install + lock behavior end to end against a
//! fetched skill tree on disk. The GLOBAL lock is process-wide (keyed off
//! `XDG_STATE_HOME`), so its tests serialize through a single mutex and point
//! `XDG_STATE_HOME` at a fresh temp dir; per-agent target dirs are isolated via
//! the thread-local `set_skills_path_override`.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};

use aghub_core::adapter::set_skills_path_override;
use aghub_core::models::ResourceScope;
use aghub_core::skills::install_fetched::{
	install_fetched_skill_and_lock, FetchedSkillInstallRequest,
};
use aghub_core::skills::linker::LinkTarget;
use aghub_core::AgentType;
use tempfile::{tempdir, TempDir};

/// Serializes + isolates the GLOBAL lock by pointing `XDG_STATE_HOME` at a
/// fresh temp dir (core cannot import skill's `pub(crate)` TestLockGuard).
struct GlobalLockGuard {
	_temp: TempDir,
	old: Option<String>,
	_lock: MutexGuard<'static, ()>,
}

impl GlobalLockGuard {
	fn new() -> Self {
		static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
		let guard = LOCK
			.get_or_init(|| Mutex::new(()))
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		let temp = tempdir().unwrap();
		let old = std::env::var("XDG_STATE_HOME").ok();
		std::env::set_var("XDG_STATE_HOME", temp.path());
		Self {
			_temp: temp,
			old,
			_lock: guard,
		}
	}
}

impl Drop for GlobalLockGuard {
	fn drop(&mut self) {
		match &self.old {
			Some(v) => std::env::set_var("XDG_STATE_HOME", v),
			None => std::env::remove_var("XDG_STATE_HOME"),
		}
	}
}

/// Write `<root>/<dir>/SKILL.md` with the given frontmatter name and return the
/// SKILL.md path.
fn write_skill(root: &Path, dir: &str, name: &str) -> PathBuf {
	let skill_dir = root.join(dir);
	std::fs::create_dir_all(&skill_dir).unwrap();
	let skill_md = skill_dir.join("SKILL.md");
	std::fs::write(
		&skill_md,
		format!("---\nname: {name}\ndescription: a test skill\n---\nbody\n"),
	)
	.unwrap();
	skill_md
}

fn sample_source() -> skill::InstallLockSource {
	skill::InstallLockSource {
		source: "owner/repo".to_string(),
		source_type: "github".to_string(),
		source_url: "https://github.com/owner/repo.git".to_string(),
		ref_name: Some("main".to_string()),
	}
}

#[test]
fn isolated_copy_installs_writes_global_lock_and_per_agent_result() {
	let _g = GlobalLockGuard::new();

	// Fetched source tree containing alpha/SKILL.md.
	let fetched = tempdir().unwrap();
	let skill_md = write_skill(fetched.path(), "alpha", "alpha");

	// Isolate Claude's skills dir.
	let agent_dir = tempdir().unwrap();
	set_skills_path_override("claude", Some(agent_dir.path().to_path_buf()));

	let source = sample_source();
	let report = install_fetched_skill_and_lock(FetchedSkillInstallRequest {
		skill_file: &skill_md,
		source: &source,
		lock_skill_path: "alpha/SKILL.md".to_string(),
		ref_commit: Some("deadbeefcafef00d".to_string()),
		scope: ResourceScope::GlobalOnly,
		project_root: None,
		target_agents: &[AgentType::Claude],
		expected_name: None,
		target: LinkTarget::Absolute,
	})
	.expect("install should succeed");

	set_skills_path_override("claude", None);

	// (a) Under symlink-only, the agent dir entry is a link; the SKILL.md
	// resolves through it to the global master in ~/.agents/skills/alpha.
	let agent_skill_entry = agent_dir.path().join("alpha");
	assert!(
		agent_skill_entry.join("SKILL.md").exists(),
		"SKILL.md should be reachable via the agent dir link"
	);
	#[cfg(unix)]
	assert!(
		aghub_core::skills::linker::Linker::is_link(&agent_skill_entry),
		"agent dir entry should be a symlink under symlink-only install"
	);

	// (b) One per-agent result, installed for Claude.
	assert_eq!(report.agent_results.len(), 1);
	let r = &report.agent_results[0];
	assert_eq!(r.agent, AgentType::Claude);
	assert!(r.installed, "claude should be installed");
	assert!(r.error.is_none());
	assert_eq!(report.name, "alpha");

	// (c) The global lock read-back has alpha with the right source + a real
	// (non-placeholder) hash.
	assert!(report.wrote_lock);
	let entry = skill::lock::global::get_skill_from_lock("alpha")
		.expect("alpha should be in the global lock");
	assert_eq!(entry.source, source.source);
	let hash = entry.content_hash.expect("content_hash should be recorded");
	assert!(!hash.is_empty());
	assert_eq!(
		hash,
		skill::compute_skill_folder_hash(&fetched.path().join("alpha"))
			.unwrap()
	);
}

#[test]
#[cfg(unix)]
fn universal_writes_master_to_canonical_and_links_agent() {
	let _g = GlobalLockGuard::new();

	// Project-scope universal install so the canonical dir is a temp project
	// root (no dependence on the real home dir).
	let project = tempdir().unwrap();
	let project_root = project.path().to_path_buf();

	let fetched = tempdir().unwrap();
	let skill_md = write_skill(fetched.path(), "alpha", "alpha");

	// Claude does NOT read `.agents` at project scope, so it gets a symlink.
	let agent_dir = project_root.join(".claude/skills");
	set_skills_path_override("claude", Some(agent_dir.clone()));

	let source = sample_source();
	let report = install_fetched_skill_and_lock(FetchedSkillInstallRequest {
		skill_file: &skill_md,
		source: &source,
		lock_skill_path: "alpha/SKILL.md".to_string(),
		ref_commit: Some("cafef00ddeadbeef".to_string()),
		scope: ResourceScope::ProjectOnly,
		project_root: Some(&project_root),
		target_agents: &[AgentType::Claude],
		expected_name: None,
		target: LinkTarget::Relative,
	})
	.expect("universal install should succeed");

	set_skills_path_override("claude", None);

	// Master lives once in the canonical `.agents/skills/alpha`.
	let canonical = project_root.join(".agents/skills/alpha");
	assert!(
		canonical.join("SKILL.md").exists(),
		"master SKILL.md should be in the canonical dir"
	);

	// The agent dir is a symlink resolving to the master content.
	let link = agent_dir.join("alpha");
	assert!(
		std::fs::symlink_metadata(&link)
			.unwrap()
			.file_type()
			.is_symlink(),
		"agent dir entry should be a symlink"
	);
	assert!(
		link.join("SKILL.md").exists(),
		"symlink should resolve to the master"
	);

	assert_eq!(report.agent_results.len(), 1);
	assert!(report.agent_results[0].installed);
}

#[test]
#[cfg(unix)]
fn universal_returns_results_in_input_target_order() {
	// Regression: the universal branch must return `agent_results` strictly 1:1
	// with the input `target_agents`, in INPUT ORDER. The API zips this Vec
	// against its `valid_agents` slice positionally (skills.rs:2019), so a
	// canonical-first / linked-after grouping mislabels per-agent rows.
	//
	// We arrange a 3-agent slice [linked, canonical-dir, canonical-dir] where a
	// LINKED agent sits FIRST and the canonical-dir agents come after. The buggy
	// code pushed canonical-dir agents in-loop and the linked agents AFTER, so it
	// returned [codex, antigravity, claude] — index 0 (codex) then mislabels
	// claude's row. The fix returns the input order [claude, codex, antigravity].
	//
	// No `set_skills_path_override` here: that thread-local holds a SINGLE
	// (agent, path) slot, so overriding several agents only keeps the last. We
	// instead pick agents by their REAL project-scope write path. `codex` and
	// `antigravity` both write to `<root>/.agents/skills` (== the universal
	// canonical dir → no link), while `claude` writes to its own
	// `<root>/.claude/skills` (→ linked).
	let _g = GlobalLockGuard::new();

	let project = tempdir().unwrap();
	let project_root = project.path().to_path_buf();

	let fetched = tempdir().unwrap();
	let skill_md = write_skill(fetched.path(), "alpha", "alpha");

	// Input order: [claude(linked), codex(canonical), antigravity(canonical)].
	let target_agents =
		[AgentType::Claude, AgentType::Codex, AgentType::Antigravity];

	let source = sample_source();
	let report = install_fetched_skill_and_lock(FetchedSkillInstallRequest {
		skill_file: &skill_md,
		source: &source,
		lock_skill_path: "alpha/SKILL.md".to_string(),
		ref_commit: None,
		scope: ResourceScope::ProjectOnly,
		project_root: Some(&project_root),
		target_agents: &target_agents,
		expected_name: None,
		target: LinkTarget::Relative,
	})
	.expect("universal install should succeed");

	// 1:1 in input order: result[i].agent == target_agents[i].
	assert_eq!(report.agent_results.len(), target_agents.len());
	for (i, expected) in target_agents.iter().enumerate() {
		assert_eq!(
			report.agent_results[i].agent, *expected,
			"agent_results[{i}] must be the same agent as target_agents[{i}] \
			 (1:1 input order)"
		);
		assert!(
			report.agent_results[i].installed,
			"agent {expected:?} should be installed"
		);
		assert!(report.agent_results[i].error.is_none());
	}
}

#[test]
#[cfg(unix)]
fn universal_install_universal_error_fails_all_agents() {
	// On an `install_universal` error EVERY target agent must be reported as a
	// failure (matching the old API, skills.rs:2077) — no agent (including a
	// canonical-dir agent) may be marked installed, and no lock may be written.
	//
	// We force the failure by pre-creating the canonical dir's parent
	// (`<root>/.agents`) as a REGULAR FILE: `install_universal` then fails on
	// `create_dir_all(<root>/.agents/skills)` with ENOTDIR before any master is
	// written.
	let _g = GlobalLockGuard::new();

	let project = tempdir().unwrap();
	let project_root = project.path().to_path_buf();

	// `.agents` is a file, not a dir → mkdir of `.agents/skills` fails.
	std::fs::write(project_root.join(".agents"), b"not a dir").unwrap();

	let fetched = tempdir().unwrap();
	let skill_md = write_skill(fetched.path(), "alpha", "alpha");

	// One linked agent (`claude` → `.claude/skills`) and one canonical-dir agent
	// (`codex` → `.agents/skills`), via their REAL write paths (no override — the
	// thread-local has only one slot). This proves the canonical-dir agent is NOT
	// spuriously marked installed on the error path.
	let target_agents = [AgentType::Claude, AgentType::Codex];

	let source = sample_source();
	let report = install_fetched_skill_and_lock(FetchedSkillInstallRequest {
		skill_file: &skill_md,
		source: &source,
		lock_skill_path: "alpha/SKILL.md".to_string(),
		ref_commit: None,
		scope: ResourceScope::ProjectOnly,
		project_root: Some(&project_root),
		target_agents: &target_agents,
		expected_name: None,
		target: LinkTarget::Relative,
	})
	.expect("universal install with an fs error still returns Ok with per-agent failures");

	// 1:1 in input order, ALL failed, none installed.
	assert_eq!(report.agent_results.len(), target_agents.len());
	for (i, expected) in target_agents.iter().enumerate() {
		assert_eq!(report.agent_results[i].agent, *expected);
		assert!(
			!report.agent_results[i].installed,
			"agent {expected:?} must NOT be installed on the error path"
		);
		assert!(
			report.agent_results[i].error.is_some(),
			"agent {expected:?} must carry the error message"
		);
	}

	// No lock written, no partial success.
	assert!(
		!report.wrote_lock,
		"no lock should be written when install_universal errored"
	);
	assert!(
		!report.agent_results.iter().any(|r| r.installed),
		"no partial success row may exist on the error path"
	);
	assert!(
		!skill::lock::local::read_local_lock(Some(&project_root))
			.skills
			.contains_key("alpha"),
		"no project lock entry should be written on the error path"
	);
}

#[test]
fn project_scope_writes_project_lock() {
	let _g = GlobalLockGuard::new();

	let project = tempdir().unwrap();
	let project_root = project.path().to_path_buf();

	let fetched = tempdir().unwrap();
	let skill_md = write_skill(fetched.path(), "beta", "beta");

	let agent_dir = project_root.join(".claude/skills");
	set_skills_path_override("claude", Some(agent_dir));

	let source = sample_source();
	install_fetched_skill_and_lock(FetchedSkillInstallRequest {
		skill_file: &skill_md,
		source: &source,
		lock_skill_path: "beta/SKILL.md".to_string(),
		ref_commit: None,
		scope: ResourceScope::ProjectOnly,
		project_root: Some(&project_root),
		target_agents: &[AgentType::Claude],
		expected_name: None,
		target: LinkTarget::Relative,
	})
	.expect("project install should succeed");

	set_skills_path_override("claude", None);

	let lock = skill::lock::local::read_local_lock(Some(&project_root));
	let entry = lock
		.skills
		.get("beta")
		.expect("beta should be in the project lock");
	assert_eq!(entry.source, source.source);
	assert_eq!(entry.skill_path.as_deref(), Some("beta/SKILL.md"));
}

#[test]
fn zero_installs_with_no_existing_lock_writes_no_lock() {
	// Bug A: every target is a soft failure (no resolvable skills dir) AND there
	// is no pre-existing lock entry → no lock must be written. The API guards
	// the whole lock-write block behind `if installed { ... }` (skills.rs:2119).
	let _g = GlobalLockGuard::new();

	let fetched = tempdir().unwrap();
	let skill_md = write_skill(fetched.path(), "gamma", "gamma");

	// AugmentCode does not support skill creation in ANY scope, so its target
	// dir resolves to `None` → a soft failure, never an install. No override is
	// set for it, so nothing touches a real directory.
	let source = sample_source();
	let report = install_fetched_skill_and_lock(FetchedSkillInstallRequest {
		skill_file: &skill_md,
		source: &source,
		lock_skill_path: "gamma/SKILL.md".to_string(),
		ref_commit: None,
		scope: ResourceScope::GlobalOnly,
		project_root: None,
		target_agents: &[AgentType::AugmentCode],
		expected_name: None,
		target: LinkTarget::Absolute,
	})
	.expect("install with only soft failures still returns Ok");

	// No agent installed, and no pre-existing lock entry → no lock written.
	assert!(
		!report.agent_results.iter().any(|r| r.installed),
		"no agent should have installed"
	);
	assert!(
		!report.wrote_lock,
		"no lock should be written when zero agents installed and no entry \
		 pre-existed"
	);
	assert!(
		skill::lock::global::get_skill_from_lock("gamma").is_none(),
		"no global lock entry should be written"
	);
}

#[test]
#[cfg(unix)]
fn universal_idempotent_rerun_does_not_rewrite_lock() {
	// Bug B: a second universal run where the master + links already exist must
	// NOT be treated as a fresh install. The API derives the lock-write signal
	// from `wrote_master` (skills.rs:2065), so the idempotent re-run reports no
	// fresh master write and does not rewrite the lock.
	let _g = GlobalLockGuard::new();

	let project = tempdir().unwrap();
	let project_root = project.path().to_path_buf();

	let fetched = tempdir().unwrap();
	let skill_md = write_skill(fetched.path(), "alpha", "alpha");

	let agent_dir = project_root.join(".claude/skills");
	set_skills_path_override("claude", Some(agent_dir.clone()));

	let source = sample_source();
	let make_req = || FetchedSkillInstallRequest {
		skill_file: &skill_md,
		source: &source,
		lock_skill_path: "alpha/SKILL.md".to_string(),
		ref_commit: None,
		scope: ResourceScope::ProjectOnly,
		project_root: Some(&project_root),
		target_agents: &[AgentType::Claude],
		expected_name: None,
		target: LinkTarget::Relative,
	};

	let first = install_fetched_skill_and_lock(make_req())
		.expect("first universal install should succeed");
	assert!(first.wrote_lock, "first run writes the lock");

	let lock_path = project_root.join("skills-lock.json");
	let after_first = std::fs::read_to_string(&lock_path)
		.expect("lock file should exist after the first run");

	let second = install_fetched_skill_and_lock(make_req())
		.expect("second universal install should succeed");

	set_skills_path_override("claude", None);

	// The master + link already existed, so the second run is NOT a fresh
	// install: no lock rewrite.
	assert!(
		!second.wrote_lock,
		"idempotent re-run must not rewrite the lock"
	);
	let after_second = std::fs::read_to_string(&lock_path)
		.expect("lock file should still exist");
	assert_eq!(
		after_first, after_second,
		"lock content must be unchanged on an idempotent re-run"
	);
}

#[test]
#[cfg(unix)]
fn unsupported_scope_rejected_before_any_write() {
	// Bug C: `ResourceScope::Both` is unsupported and must be rejected at the TOP
	// of the function, before the universal master is copied — no partial side
	// effect. For a non-Project scope the universal canonical resolves under the
	// HOME dir (`~/.agents/skills`), so we isolate HOME to a temp dir (the
	// GlobalLockGuard mutex serializes env mutation) and assert that path stays
	// untouched. Before the fix the master is copied THERE before the Err.
	let _g = GlobalLockGuard::new();

	let home = tempdir().unwrap();
	let old_home = std::env::var("HOME").ok();
	std::env::set_var("HOME", home.path());

	let fetched = tempdir().unwrap();
	let skill_md = write_skill(fetched.path(), "alpha", "alpha");

	let source = sample_source();
	let err = install_fetched_skill_and_lock(FetchedSkillInstallRequest {
		skill_file: &skill_md,
		source: &source,
		lock_skill_path: "alpha/SKILL.md".to_string(),
		ref_commit: None,
		scope: ResourceScope::Both,
		project_root: None,
		target_agents: &[AgentType::Claude],
		expected_name: None,
		target: LinkTarget::Absolute,
	})
	.expect_err("Combined scope must be refused");

	let home_canonical = home.path().join(".agents/skills/alpha");
	let home_agents = home.path().join(".agents");
	match &old_home {
		Some(v) => std::env::set_var("HOME", v),
		None => std::env::remove_var("HOME"),
	}

	assert!(
		err.to_string().contains("scope")
			|| err.to_string().contains("Combined"),
		"error should be the unsupported-scope message, got: {err}"
	);

	// No partial master was written to the universal canonical dir.
	assert!(
		!home_canonical.exists(),
		"no partial universal master should be written before the scope check"
	);
	assert!(
		!home_agents.exists(),
		"the .agents dir must not be created at all on a rejected scope"
	);
}

#[test]
fn rename_guard_rejects_mismatch_and_writes_nothing() {
	let _g = GlobalLockGuard::new();

	// Frontmatter name is "beta" but the caller expected "alpha".
	let fetched = tempdir().unwrap();
	let skill_md = write_skill(fetched.path(), "alpha", "beta");

	let agent_dir = tempdir().unwrap();
	set_skills_path_override("claude", Some(agent_dir.path().to_path_buf()));

	let source = sample_source();
	let err = install_fetched_skill_and_lock(FetchedSkillInstallRequest {
		skill_file: &skill_md,
		source: &source,
		lock_skill_path: "alpha/SKILL.md".to_string(),
		ref_commit: None,
		scope: ResourceScope::GlobalOnly,
		project_root: None,
		target_agents: &[AgentType::Claude],
		expected_name: Some("alpha"),
		target: LinkTarget::Absolute,
	})
	.expect_err("rename mismatch must be refused");

	set_skills_path_override("claude", None);

	// Shared rename message mentions the new (found) name.
	assert!(
		err.to_string().contains("renamed"),
		"error should be the shared rename message, got: {err}"
	);

	// Nothing was written: no copy, no lock entry.
	assert!(
		!agent_dir.path().join("alpha").exists()
			&& !agent_dir.path().join("beta").exists(),
		"no skill dir should be created on a refused rename"
	);
	assert!(
		skill::lock::global::get_skill_from_lock("beta").is_none(),
		"no global lock entry should be written on a refused rename"
	);
}

#[test]
#[cfg(unix)]
fn lock_written_when_master_written_but_all_agent_links_fail() {
	use std::os::unix::fs::PermissionsExt;

	let _g = GlobalLockGuard::new();

	// SAFETY: getuid has no preconditions and only reads process identity.
	if unsafe { libc::getuid() } == 0 {
		return;
	}

	let project = tempdir().unwrap();
	let project_root = project.path().to_path_buf();

	let fetched = tempdir().unwrap();
	let skill_md = write_skill(fetched.path(), "alpha", "alpha");

	let agent_dir = project_root.join(".claude/skills");
	std::fs::create_dir_all(&agent_dir).unwrap();
	std::fs::set_permissions(
		&agent_dir,
		std::fs::Permissions::from_mode(0o555),
	)
	.unwrap();

	set_skills_path_override("claude", Some(agent_dir.clone()));

	let source = sample_source();
	let report = install_fetched_skill_and_lock(FetchedSkillInstallRequest {
		skill_file: &skill_md,
		source: &source,
		lock_skill_path: "alpha/SKILL.md".to_string(),
		ref_commit: None,
		scope: ResourceScope::ProjectOnly,
		project_root: Some(&project_root),
		target_agents: &[AgentType::Claude],
		expected_name: None,
		target: LinkTarget::Relative,
	})
	.expect("install should return Ok with per-agent link failures");

	set_skills_path_override("claude", None);

	assert_eq!(report.agent_results.len(), 1);
	let agent_result = &report.agent_results[0];
	assert_eq!(agent_result.agent, AgentType::Claude);
	assert!(
		!agent_result.installed,
		"claude should not install when the per-agent link fails"
	);
	assert!(
		agent_result.error.is_some(),
		"link failure should be reported on the agent row"
	);
	assert!(
		report.wrote_lock,
		"Decision 11: a freshly written master must write the lock"
	);

	let lock = skill::lock::local::read_local_lock(Some(&project_root));
	assert!(
		lock.skills.contains_key("alpha"),
		"project lock should contain the freshly materialized master"
	);

	std::fs::set_permissions(
		&agent_dir,
		std::fs::Permissions::from_mode(0o755),
	)
	.unwrap();
}

#[test]
#[cfg(unix)]
fn conflict_fold_real_dir_and_foreign_link_are_not_clobbered() {
	use aghub_core::skills::linker::Linker;
	use std::fs;
	use std::os::unix::fs::symlink;

	let _g = GlobalLockGuard::new();

	let fetched = tempdir().unwrap();
	let skill_md = write_skill(fetched.path(), "my-skill", "my-skill");
	let source = sample_source();

	// Sub-case A: a pre-existing real directory occupies the agent slot.
	let project_a = tempdir().unwrap();
	let project_root_a = project_a.path().to_path_buf();
	let agent_dir_a = project_root_a.join(".claude/skills");
	let occupied_dir_a = agent_dir_a.join("my-skill");
	fs::create_dir_all(&occupied_dir_a).unwrap();
	fs::write(occupied_dir_a.join("sentinel.txt"), "sentinel").unwrap();

	set_skills_path_override("claude", Some(agent_dir_a.clone()));

	let report_a = install_fetched_skill_and_lock(FetchedSkillInstallRequest {
		skill_file: &skill_md,
		source: &source,
		lock_skill_path: "my-skill/SKILL.md".to_string(),
		ref_commit: None,
		scope: ResourceScope::ProjectOnly,
		project_root: Some(&project_root_a),
		target_agents: &[AgentType::Claude],
		expected_name: None,
		target: LinkTarget::Relative,
	})
	.expect("install should fold the real-dir conflict");

	set_skills_path_override("claude", None);

	assert_eq!(report_a.agent_results.len(), 1);
	assert!(!report_a.agent_results[0].installed);
	assert!(report_a.agent_results[0]
		.error
		.as_deref()
		.unwrap_or("")
		.contains("occupies"));
	assert_eq!(
		fs::read_to_string(agent_dir_a.join("my-skill/sentinel.txt")).unwrap(),
		"sentinel"
	);

	// Sub-case B: a pre-existing foreign symlink occupies the agent slot.
	let project_b = tempdir().unwrap();
	let project_root_b = project_b.path().to_path_buf();
	let unrelated = tempdir().unwrap();
	let unrelated_dir = unrelated.path().to_path_buf();
	fs::write(unrelated_dir.join("unrelated.txt"), "unrelated").unwrap();

	let agent_dir_b = project_root_b.join(".claude/skills");
	fs::create_dir_all(&agent_dir_b).unwrap();
	let link_b = agent_dir_b.join("my-skill");
	symlink(&unrelated_dir, &link_b).unwrap();

	set_skills_path_override("claude", Some(agent_dir_b.clone()));

	let report_b = install_fetched_skill_and_lock(FetchedSkillInstallRequest {
		skill_file: &skill_md,
		source: &source,
		lock_skill_path: "my-skill/SKILL.md".to_string(),
		ref_commit: None,
		scope: ResourceScope::ProjectOnly,
		project_root: Some(&project_root_b),
		target_agents: &[AgentType::Claude],
		expected_name: None,
		target: LinkTarget::Relative,
	})
	.expect("install should fold the foreign-link conflict");

	set_skills_path_override("claude", None);

	assert_eq!(report_b.agent_results.len(), 1);
	assert!(!report_b.agent_results[0].installed);
	assert!(report_b.agent_results[0]
		.error
		.as_deref()
		.unwrap_or("")
		.contains("occupies"));
	assert!(
		Linker::is_link(&link_b),
		"foreign symlink should still be a symlink"
	);
	assert!(
		link_b.join("unrelated.txt").exists(),
		"foreign symlink should still resolve to unrelated content"
	);
}
