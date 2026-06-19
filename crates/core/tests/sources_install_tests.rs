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
	SkillInstallLayout,
};
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
		layout: SkillInstallLayout::IsolatedCopy,
		expected_name: None,
		use_relative_links: false,
	})
	.expect("install should succeed");

	set_skills_path_override("claude", None);

	// (a) The skill landed in the agent's skills dir.
	assert!(
		agent_dir.path().join("alpha/SKILL.md").exists(),
		"alpha/SKILL.md should be copied into the agent dir"
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
