//! Interface tests for the core skill-rename transaction
//! (`aghub_core::skills::rename::accept_rename`) — the deepened module's test
//! surface. Its own test binary + a module-local env lock so the HOME /
//! XDG_STATE_HOME isolation these need never races another binary's env-touching
//! tests (see crates/core/AGENTS.md Testing).
//!
//! The transaction receives an already-fetched tree, so these drive it with a
//! tempdir `repo_root` — no git, no network. They seed the global skill lock
//! (via the `skill` dev-dependency) so `accept_rename`'s lock-managed
//! precondition is satisfied, and assert BOTH the observable on-disk state
//! (dirs installed / removed / restored) AND the lock transition.

#![cfg(unix)]

use std::path::Path;
use std::sync::Mutex;

use aghub_core::skills::rename::{
	accept_rename, FetchedRename, RenameLockSource, RenameRequest, RenameScope,
};

fn env_lock() -> &'static Mutex<()> {
	static LOCK: Mutex<()> = Mutex::new(());
	&LOCK
}

/// Run `f` with HOME + XDG_STATE_HOME isolated into fresh tempdirs, serialized
/// against the other tests in this binary. Restores the prior env after.
fn with_isolated_env<T>(f: impl FnOnce(&Path) -> T) -> T {
	let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
	let home = tempfile::tempdir().unwrap();
	let state = tempfile::tempdir().unwrap();
	let old_home = std::env::var("HOME").ok();
	let old_state = std::env::var("XDG_STATE_HOME").ok();
	std::env::set_var("HOME", home.path());
	std::env::set_var("XDG_STATE_HOME", state.path());
	let result = f(home.path());
	match old_home {
		Some(v) => std::env::set_var("HOME", v),
		None => std::env::remove_var("HOME"),
	}
	match old_state {
		Some(v) => std::env::set_var("XDG_STATE_HOME", v),
		None => std::env::remove_var("XDG_STATE_HOME"),
	}
	result
}

fn source_for(skill_path: &str) -> RenameLockSource {
	RenameLockSource {
		source: "owner/repo".to_string(),
		source_type: "github".to_string(),
		source_url: "https://github.com/owner/repo".to_string(),
		ref_name: Some("main".to_string()),
		skill_path: skill_path.to_string(),
	}
}

/// A fake fetched tree with `<name_dir>/SKILL.md` declaring `declared_name`.
fn fake_repo(name_dir: &str, declared_name: &str) -> tempfile::TempDir {
	let repo = tempfile::tempdir().unwrap();
	let skill_dir = repo.path().join(name_dir);
	std::fs::create_dir_all(&skill_dir).unwrap();
	std::fs::write(
		skill_dir.join("SKILL.md"),
		format!("---\nname: {declared_name}\ndescription: x\n---\nbody\n"),
	)
	.unwrap();
	repo
}

/// Install the old skill on disk in Claude's global dir so it is a target agent.
fn install_old_skill(home: &Path, name: &str) {
	let dir = home.join(".claude/skills").join(name);
	std::fs::create_dir_all(&dir).unwrap();
	std::fs::write(
		dir.join("SKILL.md"),
		format!("---\nname: {name}\ndescription: original\n---\n"),
	)
	.unwrap();
}

/// Seed the global skill lock so `accept_rename`'s lock-managed precondition
/// holds for `name`. Writes into the isolated `XDG_STATE_HOME`.
fn seed_global_lock(name: &str, skill_path: &str) {
	let mut lock = skill::SkillLockFile::default();
	lock.skills.insert(
		name.to_string(),
		skill::SkillLockEntry {
			source: "owner/repo".to_string(),
			source_type: "github".to_string(),
			source_url: "https://github.com/owner/repo".to_string(),
			ref_name: Some("main".to_string()),
			skill_path: Some(skill_path.to_string()),
			skill_folder_hash: String::new(),
			content_hash: None,
			ref_commit: None,
			installed_at: "t".to_string(),
			updated_at: "t".to_string(),
			plugin_name: None,
		},
	);
	skill::lock::global::write_skill_lock(&lock).unwrap();
}

#[test]
fn accept_rename_installs_new_and_removes_old() {
	with_isolated_env(|home| {
		install_old_skill(home, "old-skill");
		seed_global_lock("old-skill", "new-skill/SKILL.md");
		let repo = fake_repo("new-skill", "new-skill");
		let source = source_for("new-skill/SKILL.md");

		let outcome = accept_rename(
			RenameRequest {
				old_name: "old-skill",
				new_name: "new-skill",
				scope: RenameScope::Global,
			},
			FetchedRename {
				repo_root: repo.path(),
				oid: "",
				source: &source,
			},
		)
		.expect("rename should succeed");

		assert!(!outcome.paths.is_empty(), "must report installed paths");
		assert!(
			home.join(".claude/skills/new-skill").exists(),
			"new skill dir must be installed"
		);
		assert!(
			!home.join(".claude/skills/old-skill").exists(),
			"old skill dir must be removed"
		);
		// The lock transitioned: new-skill present, old-skill gone.
		let lock = skill::lock::global::read_skill_lock();
		assert!(
			lock.skills.contains_key("new-skill"),
			"new-skill must be in the lock"
		);
		assert!(
			!lock.skills.contains_key("old-skill"),
			"old-skill must be removed from the lock"
		);
	});
}

/// The forgeable-interface guard: a skill installed on disk but NOT lock-managed
/// must be refused (fabricated `RenameLockSource` cannot rename an unmanaged
/// skill), and nothing may be mutated.
#[test]
fn accept_rename_refuses_a_skill_that_is_not_lock_managed() {
	with_isolated_env(|home| {
		install_old_skill(home, "old-skill"); // on disk, but NO lock entry seeded
		let repo = fake_repo("new-skill", "new-skill");
		let source = source_for("new-skill/SKILL.md");

		let err = accept_rename(
			RenameRequest {
				old_name: "old-skill",
				new_name: "new-skill",
				scope: RenameScope::Global,
			},
			FetchedRename {
				repo_root: repo.path(),
				oid: "",
				source: &source,
			},
		)
		.expect_err("an unmanaged (unlocked) skill must not be renamed");
		assert!(
			matches!(
				err,
				aghub_core::skills::rename::RenameError::NotLocked(_)
			),
			"expected NotLocked, got {err:?}"
		);
		// Nothing mutated: old present, new never created.
		assert!(home.join(".claude/skills/old-skill").exists());
		assert!(!home.join(".claude/skills/new-skill").exists());
		assert!(!home.join(".agents/skills/new-skill").exists());
	});
}

#[test]
fn accept_rename_rejects_name_mismatch_without_mutating() {
	with_isolated_env(|home| {
		install_old_skill(home, "old-skill");
		// The fetched SKILL.md declares a DIFFERENT name than requested.
		let repo = fake_repo("new-skill", "something-else");
		let source = source_for("new-skill/SKILL.md");

		let err = accept_rename(
			RenameRequest {
				old_name: "old-skill",
				new_name: "new-skill",
				scope: RenameScope::Global,
			},
			FetchedRename {
				repo_root: repo.path(),
				oid: "",
				source: &source,
			},
		)
		.expect_err("a name mismatch must be rejected");
		assert!(
			matches!(
				err,
				aghub_core::skills::rename::RenameError::NameMismatch { .. }
			),
			"expected NameMismatch, got {err:?}"
		);
		// Nothing installed, old skill untouched.
		assert!(home.join(".claude/skills/old-skill").exists());
		assert!(!home.join(".claude/skills/new-skill").exists());
	});
}

#[test]
fn accept_rename_rolls_back_when_old_dir_cannot_be_removed() {
	use std::os::unix::fs::PermissionsExt;
	with_isolated_env(|home| {
		install_old_skill(home, "old-skill");
		seed_global_lock("old-skill", "new-skill/SKILL.md");
		let repo = fake_repo("new-skill", "new-skill");
		let source = source_for("new-skill/SKILL.md");

		// Lock the Claude skills dir read-only so a mutation under it fails.
		// Root ignores 0o500, so probe and skip rather than false-pass.
		let skills_dir = home.join(".claude/skills");
		let original = std::fs::metadata(&skills_dir).unwrap().permissions();
		std::fs::set_permissions(
			&skills_dir,
			std::fs::Permissions::from_mode(0o500),
		)
		.unwrap();
		let probe = skills_dir.join(".rename-root-probe");
		if std::fs::write(&probe, b"x").is_ok() {
			let _ = std::fs::remove_file(&probe);
			std::fs::set_permissions(&skills_dir, original).unwrap();
			eprintln!("skipping under root: 0o500 is not enforced");
			return;
		}

		let result = accept_rename(
			RenameRequest {
				old_name: "old-skill",
				new_name: "new-skill",
				scope: RenameScope::Global,
			},
			FetchedRename {
				repo_root: repo.path(),
				oid: "",
				source: &source,
			},
		);

		// Restore perms before asserting so the tempdir can be cleaned up.
		std::fs::set_permissions(&skills_dir, original).unwrap();

		assert!(
			result.is_err(),
			"must fail when the new-name link cannot be created"
		);
		// The safety contract: old skill survives (dir + content + lock), and
		// every new-name path is cleaned up.
		assert!(
			home.join(".claude/skills/old-skill/SKILL.md").exists(),
			"old skill content must remain after a rolled-back transaction"
		);
		assert!(
			!home.join(".agents/skills/new-skill").exists(),
			"the freshly-created new-name master must be rolled back"
		);
		let lock = skill::lock::global::read_skill_lock();
		assert!(
			lock.skills.contains_key("old-skill"),
			"old-skill must remain locked after rollback"
		);
		assert!(
			!lock.skills.contains_key("new-skill"),
			"new-skill must not be left in the lock"
		);
	});
}
