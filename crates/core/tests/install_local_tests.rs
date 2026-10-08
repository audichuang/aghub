//! Integration tests for core local skill install (`aghub_core::skills::install_local`).
//!
//! Verifies:
//! 1. Normal install: Master in `.aghub`, Referrer symlink, lock entry `source_type: local`.
//! 2. Different owner in lock: refused before write; disk and lock untouched.
//! 3. Rollback on lock write failure: Master and Referrer cleaned up, nothing remains.
//! 4. Untracked master adoption: adopted and stamped into lock when content matches.
//! 5. Untracked master refusal: rejected when content differs.
//! 6. Custom install name: installed under requested name, frontmatter rewritten.
//! 7. Idempotent re-import: does not restamp lock with different source if unchanged.

#![cfg(unix)]

use std::path::Path;
use std::sync::Mutex;

use aghub_agents::models::AgentType;
use aghub_core::scope::WriteScope;
use aghub_core::skills::install_local::{
	install_local_skill, LocalSkillInstallRequest,
};
use aghub_core::skills::linker::Linker;

struct EnvVarGuard(&'static str, Option<std::ffi::OsString>);

impl EnvVarGuard {
	fn set(key: &'static str, val: impl AsRef<std::ffi::OsStr>) -> Self {
		let old = std::env::var_os(key);
		std::env::set_var(key, val);
		Self(key, old)
	}
}

impl Drop for EnvVarGuard {
	fn drop(&mut self) {
		match &self.1 {
			Some(val) => std::env::set_var(self.0, val),
			None => std::env::remove_var(self.0),
		}
	}
}

fn env_lock() -> &'static Mutex<()> {
	static LOCK: Mutex<()> = Mutex::new(());
	&LOCK
}

fn with_isolated_env<T>(f: impl FnOnce(&Path, &Path) -> T) -> T {
	let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
	let home = tempfile::tempdir().unwrap();
	let state = tempfile::tempdir().unwrap();
	let data = tempfile::tempdir().unwrap();
	let _home_guard = EnvVarGuard::set("HOME", home.path());
	let _state_guard = EnvVarGuard::set("XDG_STATE_HOME", state.path());
	let _data_guard = EnvVarGuard::set("AGHUB_DATA_DIR", data.path());
	f(home.path(), data.path())
}

#[test]
fn install_local_skill_links_master_and_writes_lock() {
	with_isolated_env(|home, _data| {
		let source_skill = home.join("source-skills/my-local-skill");
		std::fs::create_dir_all(&source_skill).unwrap();
		std::fs::write(
			source_skill.join("SKILL.md"),
			"---\nname: my-local-skill\ndescription: test\n---\n\n# Body\n",
		)
		.unwrap();

		let project = home.join("myproject");
		std::fs::create_dir_all(project.join(".claude/skills")).unwrap();

		let req = LocalSkillInstallRequest {
			source_path: &source_skill.join("SKILL.md"),
			scope: WriteScope::project(&project),
			target_agents: &[AgentType::Claude],
			install_name: None,
		};

		let report =
			install_local_skill(req).expect("install_local_skill must succeed");
		assert_eq!(report.skill.name, "my-local-skill");
		assert!(report.wrote_master, "must report wrote_master");
		assert!(report.wrote_lock, "must report wrote_lock");
		assert!(
			!report.already_installed,
			"fresh install is not already installed"
		);

		// Master exists in .aghub
		assert!(
			project.join(".aghub/my-local-skill/SKILL.md").exists(),
			"Master must exist in .aghub"
		);

		// Referrer is a symlink
		let referrer = project.join(".claude/skills/my-local-skill");
		assert!(Linker::is_link(&referrer), "Referrer must be a symlink");

		// Lock entry exists with source_type: local
		let lock = skill::lock::local::read_local_lock(Some(&project));
		let entry = lock
			.skills
			.get("my-local-skill")
			.expect("lock entry must exist");
		assert_eq!(entry.source_type, "local");
		assert_eq!(
			entry.source,
			source_skill.join("SKILL.md").display().to_string()
		);
	});
}

#[test]
fn install_local_skill_refuses_when_different_owner_lock_entry_exists() {
	with_isolated_env(|home, _data| {
		let source_skill = home.join("source-skills/owned-skill");
		std::fs::create_dir_all(&source_skill).unwrap();
		std::fs::write(
			source_skill.join("SKILL.md"),
			"---\nname: owned-skill\ndescription: test\n---\n\n# Body\n",
		)
		.unwrap();

		let project = home.join("myproject");
		std::fs::create_dir_all(project.join(".claude/skills")).unwrap();

		// Pre-seed lock with a different owner (github source)
		skill::add_skill_to_local_lock(
			"owned-skill",
			skill::LocalSkillLockEntry {
				source_url: Some("https://github.com/someone/repo".to_string()),
				ref_commit: Some("abcdef1".to_string()),
				source: "someone/repo".to_string(),
				ref_name: Some("main".to_string()),
				source_type: "github".to_string(),
				computed_hash: "existinghash".to_string(),
				skill_path: Some("owned-skill/SKILL.md".to_string()),
			},
			Some(&project),
		)
		.unwrap();

		let req = LocalSkillInstallRequest {
			source_path: &source_skill.join("SKILL.md"),
			scope: WriteScope::project(&project),
			target_agents: &[AgentType::Claude],
			install_name: None,
		};

		let err =
			install_local_skill(req).expect_err("must refuse reassignment");
		let msg = err.to_string();
		assert!(
			msg.contains("already owned by source"),
			"message must explain ownership conflict: {msg}"
		);

		// Master and Referrer were NOT created
		assert!(
			!project.join(".aghub/owned-skill").exists(),
			"Master must not be created on refusal"
		);
		assert!(
			!project.join(".claude/skills/owned-skill").exists(),
			"Referrer must not be created on refusal"
		);

		// Lock entry is unchanged
		let lock = skill::lock::local::read_local_lock(Some(&project));
		let entry = lock.skills.get("owned-skill").unwrap();
		assert_eq!(entry.source_type, "github");
		assert_eq!(entry.source, "someone/repo");
	});
}

#[test]
fn install_local_skill_rolls_back_when_the_lock_write_fails() {
	use std::os::unix::fs::PermissionsExt;

	with_isolated_env(|home, _data| {
		let source_skill = home.join("source-skills/rollback-skill");
		std::fs::create_dir_all(&source_skill).unwrap();
		std::fs::write(
			source_skill.join("SKILL.md"),
			"---\nname: rollback-skill\ndescription: t\n---\n\nbody\n",
		)
		.unwrap();

		let project = home.join("myproject");
		std::fs::create_dir_all(project.join(".claude/skills")).unwrap();
		std::fs::create_dir_all(project.join(".agents/skills")).unwrap();
		std::fs::create_dir_all(project.join(".aghub")).unwrap();

		let original = std::fs::metadata(&project).unwrap().permissions();
		std::fs::set_permissions(
			&project,
			std::fs::Permissions::from_mode(0o500),
		)
		.unwrap();

		let probe = project.join(".root-probe");
		let enforced = std::fs::write(&probe, b"x").is_err();
		if !enforced {
			let _ = std::fs::remove_file(&probe);
			std::fs::set_permissions(&project, original.clone()).unwrap();
			eprintln!(
				"0o500 not enforced (root?); rollback branch NOT covered"
			);
		}

		let req = LocalSkillInstallRequest {
			source_path: &source_skill.join("SKILL.md"),
			scope: WriteScope::project(&project),
			target_agents: &[AgentType::Claude],
			install_name: None,
		};

		let result = install_local_skill(req);
		std::fs::set_permissions(&project, original).unwrap();

		if !enforced {
			result.expect("writable lock: install must succeed");
			return;
		}

		result.expect_err("a failed lock write must fail the install");
		assert!(
			!project.join(".aghub/rollback-skill").exists(),
			"the Master this call created must be rolled back"
		);
		assert!(
			!project.join(".claude/skills/rollback-skill").exists(),
			"the Referrer this call created must be rolled back"
		);
	});
}

#[test]
fn install_local_skill_adopts_an_untracked_master_when_content_matches() {
	with_isolated_env(|home, _data| {
		let source_skill = home.join("source-skills/adopted");
		std::fs::create_dir_all(&source_skill).unwrap();
		std::fs::write(
			source_skill.join("SKILL.md"),
			"---\nname: adopted\ndescription: d\n---\n\nbody\n",
		)
		.unwrap();

		let project = home.join("adopt-project");
		std::fs::create_dir_all(project.join(".claude/skills")).unwrap();

		// Pre-create untracked Master with identical content
		let master = project.join(".aghub/adopted");
		std::fs::create_dir_all(&master).unwrap();
		std::fs::write(
			master.join("SKILL.md"),
			"---\nname: adopted\ndescription: d\n---\n\nbody\n",
		)
		.unwrap();

		// Precondition: lock does not have 'adopted'
		assert!(
			!skill::lock::local::read_local_lock(Some(&project))
				.skills
				.contains_key("adopted"),
			"precondition: master is untracked"
		);

		let req = LocalSkillInstallRequest {
			source_path: &source_skill.join("SKILL.md"),
			scope: WriteScope::project(&project),
			target_agents: &[AgentType::Claude],
			install_name: None,
		};

		let report = install_local_skill(req).expect("adoption must succeed");
		assert!(
			!report.wrote_master,
			"did not write master (adopted pre-existing)"
		);
		assert!(report.wrote_lock, "must stamp lock on adoption");

		// Lock now tracks the adopted skill
		let lock = skill::lock::local::read_local_lock(Some(&project));
		let entry = lock
			.skills
			.get("adopted")
			.expect("skill must now be tracked");
		assert_eq!(entry.source_type, "local");
		assert_eq!(
			entry.source,
			source_skill.join("SKILL.md").display().to_string()
		);
	});
}

#[test]
fn install_local_skill_refuses_to_adopt_untracked_master_when_content_differs()
{
	with_isolated_env(|home, _data| {
		let source_skill = home.join("source-skills/differ");
		std::fs::create_dir_all(&source_skill).unwrap();
		std::fs::write(
			source_skill.join("SKILL.md"),
			"---\nname: differ\ndescription: new\n---\n\nnew body\n",
		)
		.unwrap();

		let project = home.join("adopt-project");
		std::fs::create_dir_all(project.join(".claude/skills")).unwrap();

		// Pre-create untracked Master with DIFFERENT content
		let master = project.join(".aghub/differ");
		std::fs::create_dir_all(&master).unwrap();
		std::fs::write(
			master.join("SKILL.md"),
			"---\nname: differ\ndescription: old\n---\n\nold body\n",
		)
		.unwrap();

		let req = LocalSkillInstallRequest {
			source_path: &source_skill.join("SKILL.md"),
			scope: WriteScope::project(&project),
			target_agents: &[AgentType::Claude],
			install_name: None,
		};

		let err = install_local_skill(req)
			.expect_err("must refuse adoption on content mismatch");
		let msg = err.to_string();
		assert!(
			msg.contains(
				"Pre-existing Master for skill 'differ' has different content"
			),
			"message must explain content mismatch: {msg}"
		);

		// Referrer must NOT be created
		assert!(
			!project.join(".claude/skills/differ").exists(),
			"Referrer must not be created on refusal"
		);
	});
}

#[test]
fn install_local_skill_with_custom_name_installs_and_rewrites_frontmatter() {
	with_isolated_env(|home, _data| {
		let source_skill = home.join("source-skills/orig-name");
		std::fs::create_dir_all(&source_skill).unwrap();
		std::fs::write(
			source_skill.join("SKILL.md"),
			"---\nname: orig-name\ndescription: test desc\n---\n\n# Body\n",
		)
		.unwrap();

		let project = home.join("myproject");
		std::fs::create_dir_all(project.join(".claude/skills")).unwrap();

		let req = LocalSkillInstallRequest {
			source_path: &source_skill.join("SKILL.md"),
			scope: WriteScope::project(&project),
			target_agents: &[AgentType::Claude],
			install_name: Some("custom-name"),
		};

		let report =
			install_local_skill(req).expect("custom name install must succeed");
		assert_eq!(report.skill.name, "custom-name");

		// Master is at .aghub/custom-name
		let master_md = project.join(".aghub/custom-name/SKILL.md");
		assert!(master_md.exists(), "master must exist under custom name");
		let content = std::fs::read_to_string(&master_md).unwrap();
		assert!(
			content.contains("name: custom-name"),
			"frontmatter name must be rewritten: {content}"
		);

		// Referrer is linked to custom-name
		assert!(
			project.join(".claude/skills/custom-name").exists(),
			"referrer must exist under custom name"
		);

		// Lock has custom-name entry
		let lock = skill::lock::local::read_local_lock(Some(&project));
		assert!(
			lock.skills.contains_key("custom-name"),
			"lock must contain custom-name"
		);

		// Re-installing with the same custom name is refused because the name is taken
		let req2 = LocalSkillInstallRequest {
			source_path: &source_skill.join("SKILL.md"),
			scope: WriteScope::project(&project),
			target_agents: &[AgentType::Claude],
			install_name: Some("custom-name"),
		};
		let err = install_local_skill(req2)
			.expect_err("taken custom name must be rejected");
		assert!(
			err.to_string().contains("already exists"),
			"error should indicate resource exists: {err}"
		);
	});
}

#[test]
fn reimport_from_different_local_path_is_a_noop_and_does_not_restamp_lock() {
	with_isolated_env(|home, _data| {
		let first = home.join("first-source/dup-skill");
		std::fs::create_dir_all(&first).unwrap();
		std::fs::write(
			first.join("SKILL.md"),
			"---\nname: dup-skill\ndescription: first\n---\n\nfirst body\n",
		)
		.unwrap();

		let project = home.join("reimport-project");
		std::fs::create_dir_all(project.join(".claude/skills")).unwrap();

		let req1 = LocalSkillInstallRequest {
			source_path: &first.join("SKILL.md"),
			scope: WriteScope::project(&project),
			target_agents: &[AgentType::Claude],
			install_name: None,
		};
		let rep1 = install_local_skill(req1).expect("first install succeeds");
		assert!(!rep1.already_installed);
		assert!(rep1.wrote_master);
		assert!(rep1.wrote_lock);

		let locked_after_first =
			skill::lock::local::read_local_lock(Some(&project))
				.skills
				.get("dup-skill")
				.cloned()
				.expect("first import writes the lock");

		// Second source with same skill name
		let second = home.join("second-source/dup-skill");
		std::fs::create_dir_all(&second).unwrap();
		std::fs::write(
			second.join("SKILL.md"),
			"---\nname: dup-skill\ndescription: second\n---\n\nsecond body\n",
		)
		.unwrap();

		let req2 = LocalSkillInstallRequest {
			source_path: &second.join("SKILL.md"),
			scope: WriteScope::project(&project),
			target_agents: &[AgentType::Claude],
			install_name: None,
		};
		let rep2 = install_local_skill(req2)
			.expect("reimporting existing skill must succeed as a no-op");
		assert!(
			rep2.already_installed,
			"second import must report already_installed"
		);
		assert!(!rep2.wrote_master, "master must not be rewritten");
		assert!(!rep2.wrote_lock, "lock must not be restamped");

		// Master is untouched
		let master =
			std::fs::read_to_string(project.join(".aghub/dup-skill/SKILL.md"))
				.unwrap();
		assert!(
			master.contains("first body") && !master.contains("second body"),
			"master must retain first body: {master}"
		);

		// Lock entry is untouched
		let locked_after_second =
			skill::lock::local::read_local_lock(Some(&project))
				.skills
				.get("dup-skill")
				.cloned()
				.expect("lock entry must remain");
		assert_eq!(locked_after_second.source, locked_after_first.source);
		assert_eq!(
			locked_after_second.computed_hash,
			locked_after_first.computed_hash
		);
	});
}

#[test]
fn install_local_skill_refuses_unparseable_preexisting_master_as_invalid_config(
) {
	with_isolated_env(|home, _data| {
		let source_skill = home.join("source-skills/unparseable");
		std::fs::create_dir_all(&source_skill).unwrap();
		std::fs::write(
			source_skill.join("SKILL.md"),
			"---\nname: unparseable\ndescription: valid\n---\n\nbody\n",
		)
		.unwrap();

		let project = home.join("corrupt-project");
		std::fs::create_dir_all(project.join(".claude/skills")).unwrap();

		let master = project.join(".aghub/unparseable");
		std::fs::create_dir_all(&master).unwrap();
		std::fs::write(master.join("SKILL.md"), "not frontmatter at all\n")
			.unwrap();

		let req = LocalSkillInstallRequest {
			source_path: &source_skill.join("SKILL.md"),
			scope: WriteScope::project(&project),
			target_agents: &[AgentType::Claude],
			install_name: None,
		};

		let err = install_local_skill(req)
			.expect_err("must refuse corrupt pre-existing master");
		assert!(
			matches!(err, aghub_core::ConfigError::InvalidConfig(_)),
			"must return InvalidConfig, got: {err:?}"
		);
		assert!(
			!project.join(".claude/skills/unparseable").exists(),
			"referrer must not be created"
		);
	});
}

#[test]
fn install_local_skill_refuses_when_agent_slot_occupied_by_real_directory() {
	with_isolated_env(|home, _data| {
		let source_skill = home.join("source-skills/occupied-slot");
		std::fs::create_dir_all(&source_skill).unwrap();
		std::fs::write(
			source_skill.join("SKILL.md"),
			"---\nname: occupied-slot\ndescription: valid\n---\n\nbody\n",
		)
		.unwrap();

		let project = home.join("occupied-project");
		let agent_slot = project.join(".claude/skills/occupied-slot");
		std::fs::create_dir_all(&agent_slot).unwrap();
		std::fs::write(
			agent_slot.join("SKILL.md"),
			"real occupant directory\n",
		)
		.unwrap();

		let req = LocalSkillInstallRequest {
			source_path: &source_skill.join("SKILL.md"),
			scope: WriteScope::project(&project),
			target_agents: &[AgentType::Claude],
			install_name: None,
		};

		let err = install_local_skill(req).expect_err(
			"must refuse when agent slot is occupied by real directory",
		);
		assert!(
			matches!(err, aghub_core::ConfigError::ResourceExists { .. }),
			"must return ResourceExists, got: {err:?}"
		);
		assert!(
			!project.join(".aghub/occupied-slot").exists(),
			"Master must be rolled back and not left behind"
		);

		let lock = skill::lock::local::read_local_lock(Some(&project));
		assert!(
			!lock.skills.contains_key("occupied-slot"),
			"lock entry must not be written when slot is occupied"
		);
		assert_eq!(
			std::fs::read_to_string(agent_slot.join("SKILL.md")).unwrap(),
			"real occupant directory\n",
			"existing directory occupant must be preserved"
		);
	});
}

/// A corrupt lock must not break a re-import that writes nothing.
///
/// Ported from the former API route test `import_skill_no_op_survives_a_corrupt_lock`.
/// Asserts both halves: the no-op still succeeds, and a new install (different skill)
/// still refuses while leaving the corrupt lock byte-identical.
#[test]
fn install_local_skill_no_op_survives_a_corrupt_lock() {
	with_isolated_env(|home, _data| {
		let source_skill = home.join("source-skills/dup-skill");
		std::fs::create_dir_all(&source_skill).unwrap();
		std::fs::write(
			source_skill.join("SKILL.md"),
			"---\nname: dup-skill\ndescription: test\n---\n\nbody\n",
		)
		.unwrap();

		let project = home.join("myproject");
		std::fs::create_dir_all(project.join(".claude/skills")).unwrap();

		let req1 = LocalSkillInstallRequest {
			source_path: &source_skill.join("SKILL.md"),
			scope: WriteScope::project(&project),
			target_agents: &[AgentType::Claude],
			install_name: None,
		};
		let rep1 = install_local_skill(req1).expect("first install succeeds");
		assert!(!rep1.already_installed);
		assert!(rep1.wrote_master);
		assert!(rep1.wrote_lock);

		// Now corrupt the lock, exactly as an unresolved merge would.
		let lock_path = project.join("skills-lock.json");
		let corrupt = format!(
			"<<<<<<< HEAD\n{}",
			std::fs::read_to_string(&lock_path).unwrap()
		);
		std::fs::write(&lock_path, &corrupt).unwrap();

		// Re-import the same NAME from DIFFERENT content. The Master is
		// already there and does not match, so the install resolves this to
		// "no-op, write no lock" — it must not be refused on the lock's
		// account.
		let variant = home.join("source-skills-b/dup-skill");
		std::fs::create_dir_all(&variant).unwrap();
		std::fs::write(
			variant.join("SKILL.md"),
			"---\nname: dup-skill\ndescription: test\n---\n\ndifferent\n",
		)
		.unwrap();

		let req2 = LocalSkillInstallRequest {
			source_path: &variant.join("SKILL.md"),
			scope: WriteScope::project(&project),
			target_agents: &[AgentType::Claude],
			install_name: None,
		};
		let rep2 = install_local_skill(req2).expect(
			"a re-import writes nothing, so a corrupt lock must not fail it",
		);
		assert!(
			rep2.already_installed,
			"re-import must report already_installed"
		);
		assert!(
			!rep2.wrote_master,
			"must not write master on no-op re-import"
		);
		assert!(!rep2.wrote_lock, "must not write lock on no-op re-import");

		// The other half: a NEW skill still refuses before materializing.
		let fresh = home.join("source-skills/fresh-skill");
		std::fs::create_dir_all(&fresh).unwrap();
		std::fs::write(
			fresh.join("SKILL.md"),
			"---\nname: fresh-skill\ndescription: test\n---\n\nbody\n",
		)
		.unwrap();

		let req_fresh = LocalSkillInstallRequest {
			source_path: &fresh.join("SKILL.md"),
			scope: WriteScope::project(&project),
			target_agents: &[AgentType::Claude],
			install_name: None,
		};
		install_local_skill(req_fresh)
			.expect_err("a real install must refuse while the lock is corrupt");
		assert!(
			!project.join(".aghub/fresh-skill").exists(),
			"the refusal must happen before the Master is written"
		);
		assert_eq!(
			std::fs::read_to_string(&lock_path).unwrap(),
			corrupt,
			"the corrupt lock must be left exactly as found"
		);
	});
}
