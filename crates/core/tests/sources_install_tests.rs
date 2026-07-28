//! Integration tests for the no-network `install_fetched_skill_and_lock`
//! primitive (Phase 2 of the CLI sources work).
//!
//! These exercise the per-agent install + lock behavior end to end against a
//! fetched skill tree on disk. The GLOBAL lock and Master are process-wide
//! (keyed off `XDG_STATE_HOME` and `HOME`), so every test serializes through a
//! single mutex and points both variables at fresh temp dirs. Per-agent target
//! dirs are isolated via project roots or `set_skills_path_override`.

use std::ffi::OsString;
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

/// Serializes environment access and isolates the GLOBAL lock + Master by
/// pointing `XDG_STATE_HOME` and `HOME` at fresh temp dirs (core cannot import
/// skill's `pub(crate)` TestLockGuard).
struct GlobalLockGuard {
	/// Read through `home()` by the `#[cfg(unix)]` tests only; on Windows the
	/// field still has to exist so the TempDir outlives the guard. Allowed dead
	/// there rather than cfg'd away, so a real unix regression still fails
	/// `clippy -D warnings`.
	#[cfg_attr(windows, allow(dead_code))]
	home: TempDir,
	_state: TempDir,
	old_home: Option<OsString>,
	old_state: Option<OsString>,
	_lock: MutexGuard<'static, ()>,
}

impl GlobalLockGuard {
	fn new() -> Self {
		static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
		let guard = LOCK
			.get_or_init(|| Mutex::new(()))
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		let home = tempdir().unwrap();
		let state = tempdir().unwrap();
		let old_home = std::env::var_os("HOME");
		let old_state = std::env::var_os("XDG_STATE_HOME");
		std::env::set_var("HOME", home.path());
		std::env::set_var("XDG_STATE_HOME", state.path());
		Self {
			home,
			_state: state,
			old_home,
			old_state,
			_lock: guard,
		}
	}

	#[cfg_attr(windows, allow(dead_code))]
	fn home(&self) -> &Path {
		self.home.path()
	}
}

impl Drop for GlobalLockGuard {
	fn drop(&mut self) {
		match &self.old_home {
			Some(v) => std::env::set_var("HOME", v),
			None => std::env::remove_var("HOME"),
		}
		match &self.old_state {
			Some(v) => std::env::set_var("XDG_STATE_HOME", v),
			None => std::env::remove_var("XDG_STATE_HOME"),
		}
	}
}

/// Write `<root>/<dir>/SKILL.md` with the given frontmatter name and return the
/// SKILL.md path.
fn write_skill(root: &Path, dir: &str, name: &str) -> PathBuf {
	write_skill_with_body(root, dir, name, "body")
}

fn write_skill_with_body(
	root: &Path,
	dir: &str,
	name: &str,
	body: &str,
) -> PathBuf {
	let skill_dir = root.join(dir);
	std::fs::create_dir_all(&skill_dir).unwrap();
	let skill_md = skill_dir.join("SKILL.md");
	std::fs::write(
		&skill_md,
		format!("---\nname: {name}\ndescription: a test skill\n---\n{body}\n"),
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
fn project_existing_different_master_rejects_before_native_or_link_mutation() {
	let _g = GlobalLockGuard::new();
	let project = tempdir().unwrap();
	let project_root = project.path().to_path_buf();
	let fetched = tempdir().unwrap();
	let skill_md = write_skill_with_body(
		fetched.path(),
		"alpha",
		"alpha",
		"fetched bytes",
	);
	let master_md = write_skill_with_body(
		&project_root.join(".agents/skills"),
		"alpha",
		"alpha",
		"local bytes that must survive",
	);
	let before = std::fs::read(&master_md).unwrap();

	let result = install_fetched_skill_and_lock(FetchedSkillInstallRequest {
		skill_file: &skill_md,
		source: &sample_source(),
		lock_skill_path: "alpha/SKILL.md".to_string(),
		ref_commit: Some("deadbeef".to_string()),
		scope: ResourceScope::ProjectOnly,
		project_root: Some(&project_root),
		target_agents: &[AgentType::Codex, AgentType::Claude],
		expected_name: None,
		target: LinkTarget::Relative,
	});

	let error = result.expect_err(
		"a fetched source must not adopt a different pre-existing Master",
	);
	assert!(
		error.to_string().contains("Master")
			&& error.to_string().contains("different"),
		"error should explain the Master integrity conflict: {error}",
	);
	assert_eq!(
		std::fs::read(&master_md).unwrap(),
		before,
		"the pre-existing Master bytes must remain untouched",
	);
	assert!(
		!project_root.join(".claude/skills/alpha").exists(),
		"the NeedsLink target must not receive a Referrer",
	);
	assert!(
		!skill::lock::local::read_local_lock(Some(&project_root))
			.skills
			.contains_key("alpha"),
		"the fetched source must not be stamped into the project lock",
	);
}

#[test]
#[cfg(unix)]
fn global_existing_different_master_rejects_before_native_or_link_mutation() {
	let g = GlobalLockGuard::new();
	let fetched = tempdir().unwrap();
	let skill_md = write_skill_with_body(
		fetched.path(),
		"alpha",
		"alpha",
		"fetched bytes",
	);
	let master_md = write_skill_with_body(
		&g.home().join(".agents/skills"),
		"alpha",
		"alpha",
		"global local bytes that must survive",
	);
	let before = std::fs::read(&master_md).unwrap();

	let result = install_fetched_skill_and_lock(FetchedSkillInstallRequest {
		skill_file: &skill_md,
		source: &sample_source(),
		lock_skill_path: "alpha/SKILL.md".to_string(),
		ref_commit: Some("deadbeef".to_string()),
		scope: ResourceScope::GlobalOnly,
		project_root: None,
		target_agents: &[AgentType::Codex, AgentType::Claude],
		expected_name: None,
		target: LinkTarget::Absolute,
	});

	let error = result.expect_err(
		"a fetched source must not adopt a different global Master",
	);
	assert!(
		error.to_string().contains("Master")
			&& error.to_string().contains("different"),
		"error should explain the Master integrity conflict: {error}",
	);
	assert_eq!(
		std::fs::read(&master_md).unwrap(),
		before,
		"the pre-existing global Master bytes must remain untouched",
	);
	assert!(
		!g.home().join(".claude/skills/alpha").exists(),
		"the global NeedsLink target must not receive a Referrer",
	);
	assert!(
		skill::lock::global::get_skill_from_lock("alpha").is_none(),
		"the fetched source must not be stamped into the global lock",
	);
}

#[test]
#[cfg(unix)]
fn exact_byte_untracked_master_is_adopted_for_native_and_link_targets() {
	let _g = GlobalLockGuard::new();
	let project = tempdir().unwrap();
	let project_root = project.path().to_path_buf();
	let fetched = tempdir().unwrap();
	let skill_md = write_skill_with_body(
		fetched.path(),
		"alpha",
		"alpha",
		"identical bytes",
	);
	let master_md = write_skill_with_body(
		&project_root.join(".agents/skills"),
		"alpha",
		"alpha",
		"identical bytes",
	);
	let expected_hash = skill::compute_skill_folder_hash(
		skill_md.parent().expect("fetched skill dir"),
	)
	.unwrap();
	assert_eq!(
		skill::compute_skill_folder_hash(
			master_md.parent().expect("Master skill dir")
		)
		.unwrap(),
		expected_hash,
		"fixture must start as an exact-byte untracked Master",
	);

	let source = sample_source();
	let report = install_fetched_skill_and_lock(FetchedSkillInstallRequest {
		skill_file: &skill_md,
		source: &source,
		lock_skill_path: "alpha/SKILL.md".to_string(),
		ref_commit: Some("deadbeef".to_string()),
		scope: ResourceScope::ProjectOnly,
		project_root: Some(&project_root),
		target_agents: &[AgentType::Codex, AgentType::Claude],
		expected_name: None,
		target: LinkTarget::Relative,
	})
	.expect("an exact-byte untracked Master should be adoptable");

	assert!(report.wrote_lock, "adoption must stamp source provenance");
	assert!(
		report.agent_results.iter().all(|row| row.installed),
		"NativeReader and NeedsLink targets should both receive the adopted skill",
	);
	let referrer = project_root.join(".claude/skills/alpha");
	assert!(
		aghub_core::skills::linker::Linker::is_link(&referrer),
		"the NeedsLink target should receive a Referrer",
	);
	assert_eq!(
		skill::compute_skill_folder_hash(
			project_root.join(".agents/skills/alpha").as_path()
		)
		.unwrap(),
		expected_hash,
		"adoption must not alter the exact-byte Master",
	);
	let lock = skill::lock::local::read_local_lock(Some(&project_root));
	let entry = lock.skills.get("alpha").expect("adoption lock entry");
	assert_eq!(entry.source, source.source);
	assert_eq!(entry.computed_hash, expected_hash);
}

#[test]
#[cfg(unix)]
fn exact_byte_untracked_master_with_existing_referrer_is_adopted() {
	use std::os::unix::fs::symlink;

	let _g = GlobalLockGuard::new();
	let project = tempdir().unwrap();
	let project_root = project.path().to_path_buf();
	let fetched = tempdir().unwrap();
	let skill_md = write_skill_with_body(
		fetched.path(),
		"alpha",
		"alpha",
		"identical bytes",
	);
	let master_md = write_skill_with_body(
		&project_root.join(".agents/skills"),
		"alpha",
		"alpha",
		"identical bytes",
	);
	let referrer = project_root.join(".claude/skills/alpha");
	std::fs::create_dir_all(referrer.parent().unwrap()).unwrap();
	symlink(master_md.parent().unwrap(), &referrer).unwrap();

	let source = sample_source();
	let report = install_fetched_skill_and_lock(FetchedSkillInstallRequest {
		skill_file: &skill_md,
		source: &source,
		lock_skill_path: "alpha/SKILL.md".to_string(),
		ref_commit: Some("deadbeef".to_string()),
		scope: ResourceScope::ProjectOnly,
		project_root: Some(&project_root),
		target_agents: &[AgentType::Claude],
		expected_name: None,
		target: LinkTarget::Relative,
	})
	.expect("an exact Master with a correct Referrer should be adoptable");

	assert!(
		report.wrote_lock,
		"a successfully covered NeedsLink target must permit provenance adoption"
	);
	assert_eq!(report.agent_results.len(), 1);
	assert!(report.agent_results[0].error.is_none());
	assert!(
		skill::lock::local::read_local_lock(Some(&project_root))
			.skills
			.contains_key("alpha"),
		"adoption must persist the source lock"
	);
}

#[test]
#[cfg(unix)]
fn symlink_master_is_rejected_before_referrer_or_lock_mutation() {
	use std::os::unix::fs::symlink;

	let _g = GlobalLockGuard::new();
	let project = tempdir().unwrap();
	let project_root = project.path().to_path_buf();
	let fetched = tempdir().unwrap();
	let skill_md = write_skill_with_body(
		fetched.path(),
		"alpha",
		"alpha",
		"identical bytes",
	);
	let outside = tempdir().unwrap();
	let outside_md = write_skill_with_body(
		outside.path(),
		"alpha",
		"alpha",
		"identical bytes",
	);
	let master = project_root.join(".agents/skills/alpha");
	std::fs::create_dir_all(master.parent().unwrap()).unwrap();
	symlink(outside_md.parent().unwrap(), &master).unwrap();

	let result = install_fetched_skill_and_lock(FetchedSkillInstallRequest {
		skill_file: &skill_md,
		source: &sample_source(),
		lock_skill_path: "alpha/SKILL.md".to_string(),
		ref_commit: Some("deadbeef".to_string()),
		scope: ResourceScope::ProjectOnly,
		project_root: Some(&project_root),
		target_agents: &[AgentType::Codex, AgentType::Claude],
		expected_name: None,
		target: LinkTarget::Relative,
	});

	let error = result.expect_err("a Master slot must never be a link");
	assert!(
		error.to_string().contains("Master")
			&& error.to_string().contains("link"),
		"error should explain the unsafe Master occupant: {error}"
	);
	assert!(
		aghub_core::skills::linker::Linker::is_link(&master),
		"the rejected Master link must remain untouched"
	);
	assert_eq!(
		std::fs::read_to_string(&outside_md).unwrap(),
		"---\nname: alpha\ndescription: a test skill\n---\nidentical bytes\n"
	);
	assert!(!project_root.join(".claude/skills/alpha").exists());
	assert!(!skill::lock::local::read_local_lock(Some(&project_root))
		.skills
		.contains_key("alpha"));
}

#[test]
#[cfg(unix)]
fn master_with_nested_symlink_is_rejected_before_referrer_or_lock_mutation() {
	use std::os::unix::fs::symlink;

	let _g = GlobalLockGuard::new();
	let project = tempdir().unwrap();
	let project_root = project.path().to_path_buf();
	let fetched = tempdir().unwrap();
	let skill_md = write_skill_with_body(
		fetched.path(),
		"alpha",
		"alpha",
		"identical bytes",
	);
	let master_md = write_skill_with_body(
		&project_root.join(".agents/skills"),
		"alpha",
		"alpha",
		"identical bytes",
	);
	let outside = tempdir().unwrap();
	let outside_asset = outside.path().join("secret.txt");
	std::fs::write(&outside_asset, "must stay outside provenance").unwrap();
	let nested_link = master_md.parent().unwrap().join("assets/evil");
	std::fs::create_dir_all(nested_link.parent().unwrap()).unwrap();
	symlink(&outside_asset, &nested_link).unwrap();
	assert_eq!(
		skill::compute_skill_folder_hash(master_md.parent().unwrap()).unwrap(),
		skill::compute_skill_folder_hash(skill_md.parent().unwrap()).unwrap(),
		"the fixture proves content hashing alone cannot see the nested link"
	);

	let result = install_fetched_skill_and_lock(FetchedSkillInstallRequest {
		skill_file: &skill_md,
		source: &sample_source(),
		lock_skill_path: "alpha/SKILL.md".to_string(),
		ref_commit: Some("deadbeef".to_string()),
		scope: ResourceScope::ProjectOnly,
		project_root: Some(&project_root),
		target_agents: &[AgentType::Codex, AgentType::Claude],
		expected_name: None,
		target: LinkTarget::Relative,
	});

	let error = result.expect_err("adoption must reject every nested link");
	assert!(
		error.to_string().contains("Master")
			&& error.to_string().contains("link"),
		"error should explain the unsafe nested Master link: {error}"
	);
	assert!(
		aghub_core::skills::linker::Linker::is_link(&nested_link),
		"the rejected nested link must remain untouched"
	);
	assert_eq!(
		std::fs::read_to_string(&outside_asset).unwrap(),
		"must stay outside provenance"
	);
	assert!(!project_root.join(".claude/skills/alpha").exists());
	assert!(!skill::lock::local::read_local_lock(Some(&project_root))
		.skills
		.contains_key("alpha"));
}

#[test]
fn matching_master_with_different_project_source_owner_is_not_reassigned() {
	let _g = GlobalLockGuard::new();
	let project = tempdir().unwrap();
	let project_root = project.path().to_path_buf();
	let fetched = tempdir().unwrap();
	let skill_md = write_skill_with_body(
		fetched.path(),
		"alpha",
		"alpha",
		"identical bytes",
	);
	let master_md = write_skill_with_body(
		&project_root.join(".agents/skills"),
		"alpha",
		"alpha",
		"identical bytes",
	);
	let other_source = skill::InstallLockSource {
		source: "different-owner/different-repo".to_string(),
		source_type: "github".to_string(),
		source_url: "https://github.com/different-owner/different-repo.git"
			.to_string(),
		ref_name: Some("main".to_string()),
	};
	skill::write_project_install_lock(
		"alpha",
		&other_source,
		Some("alpha/SKILL.md".to_string()),
		master_md.parent().expect("Master skill dir"),
		&project_root,
		Some("oldcommit".to_string()),
	)
	.unwrap();
	let lock_path = project_root.join("skills-lock.json");
	let lock_before = std::fs::read(&lock_path).unwrap();
	let master_before = std::fs::read(&master_md).unwrap();

	let requested_source = sample_source();
	let result = install_fetched_skill_and_lock(FetchedSkillInstallRequest {
		skill_file: &skill_md,
		source: &requested_source,
		lock_skill_path: "alpha/SKILL.md".to_string(),
		ref_commit: Some("newcommit".to_string()),
		scope: ResourceScope::ProjectOnly,
		project_root: Some(&project_root),
		target_agents: &[AgentType::Codex, AgentType::Claude],
		expected_name: None,
		target: LinkTarget::Relative,
	});

	let error = result.expect_err(
		"a fetched install must not reassign an existing normalized source owner",
	);
	assert!(
		error.to_string().contains("different-owner/different-repo")
			&& error.to_string().contains("owner/repo"),
		"ownership conflict should name both normalized sources: {error}",
	);
	assert_eq!(
		std::fs::read(&master_md).unwrap(),
		master_before,
		"the matching Master must remain unchanged",
	);
	assert!(
		!project_root.join(".claude/skills/alpha").exists(),
		"ownership must be checked before creating a Referrer",
	);
	assert_eq!(
		std::fs::read(&lock_path).unwrap(),
		lock_before,
		"the existing lock owner and Source hash must remain byte-for-byte intact",
	);
}

#[test]
fn matching_master_with_same_repo_path_on_another_host_is_not_reassigned() {
	let _g = GlobalLockGuard::new();
	let project = tempdir().unwrap();
	let project_root = project.path().to_path_buf();
	let fetched = tempdir().unwrap();
	let skill_md = write_skill_with_body(
		fetched.path(),
		"alpha",
		"alpha",
		"identical bytes",
	);
	let master_md = write_skill_with_body(
		&project_root.join(".agents/skills"),
		"alpha",
		"alpha",
		"identical bytes",
	);
	let original_source = skill::InstallLockSource {
		source: "owner/repo".to_string(),
		source_type: "git".to_string(),
		source_url: "https://git.example.test/owner/repo.git".to_string(),
		ref_name: Some("main".to_string()),
	};
	skill::write_project_install_lock(
		"alpha",
		&original_source,
		Some("alpha/SKILL.md".to_string()),
		master_md.parent().unwrap(),
		&project_root,
		Some("oldcommit".to_string()),
	)
	.unwrap();
	let lock_path = project_root.join("skills-lock.json");
	let lock_before = std::fs::read(&lock_path).unwrap();
	let requested_source = skill::InstallLockSource {
		source: "owner/repo".to_string(),
		source_type: "git".to_string(),
		source_url: "https://another.example.test/owner/repo.git".to_string(),
		ref_name: Some("main".to_string()),
	};

	let result = install_fetched_skill_and_lock(FetchedSkillInstallRequest {
		skill_file: &skill_md,
		source: &requested_source,
		lock_skill_path: "alpha/SKILL.md".to_string(),
		ref_commit: Some("newcommit".to_string()),
		scope: ResourceScope::ProjectOnly,
		project_root: Some(&project_root),
		target_agents: &[AgentType::Codex, AgentType::Claude],
		expected_name: None,
		target: LinkTarget::Relative,
	});

	let error = result.expect_err(
		"the same owner/repo path on another host must be a different owner",
	);
	assert!(
		error.to_string().contains("source owner"),
		"error should explain the source ownership conflict: {error}"
	);
	assert!(!project_root.join(".claude/skills/alpha").exists());
	assert_eq!(std::fs::read(&lock_path).unwrap(), lock_before);
}

#[test]
fn legacy_remote_lock_without_host_identity_is_not_reassigned() {
	let _g = GlobalLockGuard::new();
	let project = tempdir().unwrap();
	let project_root = project.path().to_path_buf();
	let fetched = tempdir().unwrap();
	let skill_md = write_skill_with_body(
		fetched.path(),
		"alpha",
		"alpha",
		"identical bytes",
	);
	let master_md = write_skill_with_body(
		&project_root.join(".agents/skills"),
		"alpha",
		"alpha",
		"identical bytes",
	);
	let master_hash =
		skill::compute_skill_folder_hash(master_md.parent().unwrap()).unwrap();
	skill::add_skill_to_local_lock(
		"alpha",
		skill::LocalSkillLockEntry {
			source: "owner/repo".to_string(),
			ref_name: Some("main".to_string()),
			source_type: "git".to_string(),
			skill_path: Some("alpha/SKILL.md".to_string()),
			computed_hash: master_hash,
			ref_commit: Some("oldcommit".to_string()),
			source_url: None,
		},
		Some(&project_root),
	)
	.unwrap();
	let lock_path = project_root.join("skills-lock.json");
	let lock_before = std::fs::read(&lock_path).unwrap();
	let requested_source = skill::InstallLockSource {
		source: "owner/repo".to_string(),
		source_type: "git".to_string(),
		source_url: "https://git.example.test/owner/repo.git".to_string(),
		ref_name: Some("main".to_string()),
	};

	let result = install_fetched_skill_and_lock(FetchedSkillInstallRequest {
		skill_file: &skill_md,
		source: &requested_source,
		lock_skill_path: "alpha/SKILL.md".to_string(),
		ref_commit: Some("newcommit".to_string()),
		scope: ResourceScope::ProjectOnly,
		project_root: Some(&project_root),
		target_agents: &[AgentType::Codex, AgentType::Claude],
		expected_name: None,
		target: LinkTarget::Relative,
	});

	result.expect_err(
		"a legacy non-GitHub lock without a host cannot prove source ownership",
	);
	assert!(!project_root.join(".claude/skills/alpha").exists());
	assert_eq!(std::fs::read(&lock_path).unwrap(), lock_before);
}

#[test]
#[cfg(unix)]
fn matching_master_with_different_global_source_owner_is_not_reassigned() {
	let g = GlobalLockGuard::new();
	let fetched = tempdir().unwrap();
	let skill_md = write_skill_with_body(
		fetched.path(),
		"alpha",
		"alpha",
		"identical bytes",
	);
	let master_md = write_skill_with_body(
		&g.home().join(".agents/skills"),
		"alpha",
		"alpha",
		"identical bytes",
	);
	let other_source = skill::InstallLockSource {
		source: "different-owner/different-repo".to_string(),
		source_type: "github".to_string(),
		source_url: "https://github.com/different-owner/different-repo.git"
			.to_string(),
		ref_name: Some("main".to_string()),
	};
	skill::write_global_install_lock(
		"alpha",
		&other_source,
		Some("alpha/SKILL.md".to_string()),
		master_md.parent().expect("Master skill dir"),
		Some("oldcommit".to_string()),
	)
	.unwrap();
	let lock_path = skill::lock::global::get_skill_lock_path();
	let lock_before = std::fs::read(&lock_path).unwrap();

	let requested_source = sample_source();
	let result = install_fetched_skill_and_lock(FetchedSkillInstallRequest {
		skill_file: &skill_md,
		source: &requested_source,
		lock_skill_path: "alpha/SKILL.md".to_string(),
		ref_commit: Some("newcommit".to_string()),
		scope: ResourceScope::GlobalOnly,
		project_root: None,
		target_agents: &[AgentType::Codex, AgentType::Claude],
		expected_name: None,
		target: LinkTarget::Absolute,
	});

	let error = result.expect_err(
		"a fetched install must not reassign a global source owner",
	);
	assert!(
		error.to_string().contains("different-owner/different-repo")
			&& error.to_string().contains("owner/repo"),
		"ownership conflict should name both normalized sources: {error}",
	);
	assert!(
		!g.home().join(".claude/skills/alpha").exists(),
		"global ownership must be checked before creating a Referrer",
	);
	assert_eq!(
		std::fs::read(&lock_path).unwrap(),
		lock_before,
		"the global lock owner and Source hash must remain byte-for-byte intact",
	);
}

#[test]
#[cfg(unix)]
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
fn all_unsupported_targets_preflight_before_master_or_lock_write() {
	// Every target is predictably unsupported. The multi-target policy rejects
	// the command before materializing a Master or advancing the lock.
	let _g = GlobalLockGuard::new();
	let project = tempdir().unwrap();
	let project_root = project.path().to_path_buf();

	let fetched = tempdir().unwrap();
	let skill_md = write_skill(fetched.path(), "gamma", "gamma");

	// AugmentCode does not support skill creation in project scope. No override
	// is set for it, so a preflight regression cannot hide behind a test path.
	let source = sample_source();
	let result = install_fetched_skill_and_lock(FetchedSkillInstallRequest {
		skill_file: &skill_md,
		source: &source,
		lock_skill_path: "gamma/SKILL.md".to_string(),
		ref_commit: None,
		scope: ResourceScope::ProjectOnly,
		project_root: Some(&project_root),
		target_agents: &[AgentType::AugmentCode],
		expected_name: None,
		target: LinkTarget::Relative,
	});
	let error = result.expect_err("unsupported targets must fail preflight");
	assert!(error.to_string().contains("nothing was written"));
	assert!(
		!project_root.join(".agents/skills/gamma").exists(),
		"an all-unsupported mutation must not leave an orphan Master",
	);
	let local_lock = skill::lock::local::read_local_lock(Some(&project_root));
	assert!(
		!local_lock.skills.contains_key("gamma"),
		"no project lock entry should be written"
	);
}

#[test]
fn mixed_supported_and_unsupported_targets_preflight_before_master_write() {
	let _g = GlobalLockGuard::new();
	let project = tempdir().unwrap();
	let project_root = project.path().to_path_buf();
	let fetched = tempdir().unwrap();
	let skill_md = write_skill(fetched.path(), "mixed", "mixed");
	let claude_dir = project_root.join(".claude/skills");
	set_skills_path_override("claude", Some(claude_dir.clone()));

	let result = install_fetched_skill_and_lock(FetchedSkillInstallRequest {
		skill_file: &skill_md,
		source: &sample_source(),
		lock_skill_path: "mixed/SKILL.md".to_string(),
		ref_commit: None,
		scope: ResourceScope::ProjectOnly,
		project_root: Some(&project_root),
		target_agents: &[AgentType::Claude, AgentType::AugmentCode],
		expected_name: None,
		target: LinkTarget::Relative,
	});
	set_skills_path_override("claude", None);

	let error = result.expect_err(
		"a predictable unsupported target must reject the whole mutation",
	);
	assert!(
		error.to_string().contains("nothing was written"),
		"preflight error must state the no-write guarantee: {error}",
	);
	assert!(
		!project_root.join(".agents/skills/mixed").exists(),
		"preflight must run before the shared Master is materialized",
	);
	assert!(
		!claude_dir.join("mixed").exists(),
		"a supported earlier target must not receive a Referrer",
	);
	assert!(
		!skill::lock::local::read_local_lock(Some(&project_root))
			.skills
			.contains_key("mixed"),
		"a rejected mutation must not advance the lock",
	);
}

#[test]
fn shared_master_failure_is_attributed_to_every_agent() {
	let _g = GlobalLockGuard::new();
	let project = tempdir().unwrap();
	let project_root = project.path().to_path_buf();
	let fetched = tempdir().unwrap();
	let skill_md = write_skill(fetched.path(), "blocked", "blocked");
	let claude_dir = project_root.join("claude-skills");
	let codex_dir = project_root.join("codex-skills");
	set_skills_path_override("claude", Some(claude_dir));
	set_skills_path_override("codex", Some(codex_dir));

	// Make the shared `.agents/skills` parent impossible to create. The one
	// shared setup must fail once and attribute that same failure to both rows.
	std::fs::write(project_root.join(".agents"), "not a directory").unwrap();
	let report = install_fetched_skill_and_lock(FetchedSkillInstallRequest {
		skill_file: &skill_md,
		source: &sample_source(),
		lock_skill_path: "blocked/SKILL.md".to_string(),
		ref_commit: None,
		scope: ResourceScope::ProjectOnly,
		project_root: Some(&project_root),
		target_agents: &[AgentType::Claude, AgentType::Codex],
		expected_name: None,
		target: LinkTarget::Relative,
	})
	.expect("shared setup failures are returned as attributed rows");

	set_skills_path_override("claude", None);
	set_skills_path_override("codex", None);

	assert_eq!(report.agent_results.len(), 2);
	assert!(report.agent_results.iter().all(|row| !row.installed));
	let errors = report
		.agent_results
		.iter()
		.map(|row| row.error.as_deref().expect("attributed error"))
		.collect::<Vec<_>>();
	assert_eq!(errors[0], errors[1]);
	assert!(!report.wrote_lock);
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

/// An idempotent re-run writes nothing on disk, so without coordinate healing a
/// re-install pointed at a DIFFERENT ref (or commit, or skillPath) would leave
/// the lock pinned to the first one and every later `source sync --update` would
/// keep following the stale ref. Content and ownership are identical here, so
/// only the update coordinates may change.
#[test]
#[cfg(unix)]
fn same_owner_reinstall_heals_stale_update_coordinates() {
	let _g = GlobalLockGuard::new();

	let project = tempdir().unwrap();
	let project_root = project.path().to_path_buf();
	let fetched = tempdir().unwrap();
	let skill_md = write_skill(fetched.path(), "alpha", "alpha");
	let agent_dir = project_root.join(".claude/skills");
	set_skills_path_override("claude", Some(agent_dir.clone()));

	let on_main = sample_source();
	let mut on_tag = sample_source();
	on_tag.ref_name = Some("v1".to_string());

	// A nested fn, not a closure: the request borrows for a named lifetime that
	// a closure's inferred one cannot express.
	fn req<'a>(
		skill_md: &'a Path,
		source: &'a skill::InstallLockSource,
		project_root: &'a Path,
		agents: &'a [AgentType],
		commit: Option<String>,
		skill_path: &str,
	) -> FetchedSkillInstallRequest<'a> {
		FetchedSkillInstallRequest {
			skill_file: skill_md,
			source,
			lock_skill_path: skill_path.to_string(),
			ref_commit: commit,
			scope: ResourceScope::ProjectOnly,
			project_root: Some(project_root),
			target_agents: agents,
			expected_name: None,
			target: LinkTarget::Relative,
		}
	}
	let agents = [AgentType::Claude];

	install_fetched_skill_and_lock(req(
		&skill_md,
		&on_main,
		&project_root,
		&agents,
		Some("c1".to_string()),
		"alpha/SKILL.md",
	))
	.expect("first install should succeed");

	// Same bytes, same owner, different ref + commit: nothing changes on disk.
	let healed = install_fetched_skill_and_lock(req(
		&skill_md,
		&on_tag,
		&project_root,
		&agents,
		Some("c2".to_string()),
		"alpha/SKILL.md",
	))
	.expect("re-install from another ref should succeed");
	assert!(
		!healed.wrote_master,
		"the Master already existed; this must not be a fresh write"
	);
	assert!(
		healed.wrote_lock,
		"stale update coordinates must be healed even with no disk change"
	);

	let entry = |root: &Path| {
		skill::lock::local::read_local_lock(Some(root))
			.skills
			.get("alpha")
			.cloned()
			.expect("alpha must stay locked")
	};
	let after = entry(&project_root);
	assert_eq!(
		after.ref_name.as_deref(),
		Some("v1"),
		"ref_name must follow the ref this install actually requested"
	);
	assert_eq!(
		after.ref_commit.as_deref(),
		Some("c2"),
		"ref_commit must follow the commit this install actually fetched"
	);

	// Back to `main` WITHOUT a commit: the differing ref heals, and the recorded
	// commit must be DROPPED, not carried. `c2` only certifies the content at
	// `v1`; keeping it would let update preflight treat `main` as already proven
	// and skip the fetch.
	let dropped = install_fetched_skill_and_lock(req(
		&skill_md,
		&on_main,
		&project_root,
		&agents,
		None,
		"alpha/SKILL.md",
	))
	.expect("re-install without a commit should succeed");
	assert!(dropped.wrote_lock, "the differing ref must still heal");
	let after_drop = entry(&project_root);
	assert_eq!(
		after_drop.ref_name.as_deref(),
		Some("main"),
		"the requested ref must win"
	);
	assert_eq!(
		after_drop.ref_commit, None,
		"a commit recorded for another ref must not certify this one"
	);

	// A changed skillPath is the third coordinate and heals on its own.
	let moved = install_fetched_skill_and_lock(req(
		&skill_md,
		&on_main,
		&project_root,
		&agents,
		Some("c2".to_string()),
		"nested/alpha/SKILL.md",
	))
	.expect("re-install from a moved path should succeed");
	set_skills_path_override("claude", None);
	assert!(moved.wrote_lock, "a changed skillPath must heal");
	assert_eq!(
		entry(&project_root).skill_path.as_deref(),
		Some("nested/alpha/SKILL.md"),
		"skillPath must follow the path this install actually used"
	);

	// Nothing so far created the entry -- every one of these was a rewrite.
	assert!(
		!moved.created_lock,
		"healing a pre-existing entry is not entry creation"
	);

	// Carry-over's real case: a rewrite triggered by covering a NEW agent, with
	// the ref and skillPath unchanged and no commit supplied. The recorded commit
	// still certifies exactly these coordinates, so erasing it would needlessly
	// defeat update preflight.
	let both = [AgentType::Claude, AgentType::Codex];
	let relink = install_fetched_skill_and_lock(req(
		&skill_md,
		&on_main,
		&project_root,
		&both,
		None,
		"nested/alpha/SKILL.md",
	))
	.expect("covering another agent should succeed");
	assert!(relink.wrote_lock, "covering a new agent rewrites the entry");
	assert_eq!(
		entry(&project_root).ref_commit.as_deref(),
		Some("c2"),
		"an unchanged coordinate context must keep its recorded commit"
	);
}

/// A NativeReader agent reads the Master directly: its row reports
/// `installed: true` with NO link created, and its first skills path IS the
/// Master. Attribution must come from the linker's own `linked` set, so
/// `created_referrer_dirs` stays empty here — a rollback that re-derived dirs
/// from `installed` would delete the Master through the referrer loop, before
/// any `wrote_master` check could stop it.
#[test]
#[cfg(unix)]
fn native_reader_install_attributes_no_referrer_dir() {
	let _g = GlobalLockGuard::new();

	let project = tempdir().unwrap();
	let project_root = project.path().to_path_buf();
	let fetched = tempdir().unwrap();
	let skill_md = write_skill(fetched.path(), "alpha", "alpha");
	let source = sample_source();

	let report = install_fetched_skill_and_lock(FetchedSkillInstallRequest {
		skill_file: &skill_md,
		source: &source,
		lock_skill_path: "alpha/SKILL.md".to_string(),
		ref_commit: None,
		scope: ResourceScope::ProjectOnly,
		project_root: Some(&project_root),
		target_agents: &[AgentType::Codex],
		expected_name: None,
		target: LinkTarget::Relative,
	})
	.expect("a NativeReader install should succeed");

	let row = report
		.agent_results
		.first()
		.expect("one target agent, one row");
	assert!(row.installed, "a NativeReader row reports installed");
	assert!(row.error.is_none(), "and carries no error");
	let master = project_root.join(".agents/skills/alpha");
	assert!(master.is_dir(), "the Master must exist");
	assert!(
		report.wrote_master,
		"this call claimed and wrote the Master"
	);
	assert!(
		report.created_referrer_dirs.is_empty(),
		"no link was created, so no referrer dir may be attributed -- got {:?}",
		report.created_referrer_dirs
	);
	assert!(
		!report
			.created_referrer_dirs
			.iter()
			.any(|d| d == master.parent().unwrap()),
		"the Master's own dir must never be attributed as a referrer dir"
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
	let g = GlobalLockGuard::new();
	let home = g.home();

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

	let home_canonical = home.join(".agents/skills/alpha");
	let home_agents = home.join(".agents");

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
#[cfg(unix)]
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
