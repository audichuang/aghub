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
/// against the other tests in this binary. Restoration is RAII, so a panic
/// inside `f` cannot leak a dropped tempdir path into a later test.
fn with_isolated_env<T>(f: impl FnOnce(&Path) -> T) -> T {
	let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
	let home = tempfile::tempdir().unwrap();
	let state = tempfile::tempdir().unwrap();
	// Declared AFTER the tempdirs so they drop FIRST: the env is restored to
	// the real values before these directories are deleted.
	let _home_guard = EnvVarGuard::set("HOME", home.path());
	let _state_guard = EnvVarGuard::set("XDG_STATE_HOME", state.path());
	f(home.path())
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
	install_old_skill_under(home, ".claude/skills", name);
}

/// Install the old skill on disk under an arbitrary agent-relative global
/// skills subdir, rooted at `home` -- the caller passes a SEPARATE tempdir
/// from the main `home` (e.g. Amp/Kimi's `agents/skills` under its own
/// isolated `XDG_CONFIG_HOME`) to keep that agent's read path decoupled from
/// the write path being blocked in the same test.
fn install_old_skill_under(home: &Path, sub: &str, name: &str) {
	let dir = home.join(sub).join(name);
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

/// Restores one env var to its prior value when dropped -- including on a
/// panic partway through the test (e.g. a `create_dir_all(..).unwrap()` or a
/// panic inside `accept_rename`) -- so a failure can never leave a later test
/// in this binary pointed at an already-dropped tempdir. Assumes the caller
/// already holds `env_lock()` (via `with_isolated_env`).
struct EnvVarGuard(&'static str, Option<std::ffi::OsString>);

impl EnvVarGuard {
	fn set(key: &'static str, path: &Path) -> Self {
		let prev = std::env::var_os(key);
		std::env::set_var(key, path);
		EnvVarGuard(key, prev)
	}
}

impl Drop for EnvVarGuard {
	fn drop(&mut self) {
		match self.1.take() {
			Some(v) => std::env::set_var(self.0, v),
			None => std::env::remove_var(self.0),
		}
	}
}

/// Fix C: with TWO target agents, if ONE agent's install hits a genuine
/// runtime failure while the OTHER (Claude) installs successfully, the WHOLE
/// transaction must roll back -- Claude's half succeeding must never let the
/// transaction reach Step 8 and remove the failing agent's old skill too.
/// Before the fix, the rollback only fired when NOT A SINGLE agent installed,
/// so this partial-failure case fell through to Step 8.
///
/// The failing agent is Amp (`capabilities.skills.universal: true`): it is
/// DISCOVERED via its read-only universal-XDG append path
/// ($XDG_CONFIG_HOME/agents/skills, isolated to its own tempdir below), but
/// its WRITE target is a DIFFERENT, own `~/.config/agents/skills` dir, which
/// is blocked read-only. Old-skill removal touches only the (writable) XDG
/// path, decoupling it from the (blocked) write path -- unlike blocking a
/// single shared per-agent dir, this does NOT also trip the pre-existing
/// removal-failure guard, so it isolates Fix C specifically: without the fix,
/// this scenario returns `Ok(..)` and silently drops Amp's skill.
#[test]
fn accept_rename_rolls_back_when_one_of_two_agents_fails_to_install() {
	use std::os::unix::fs::PermissionsExt;
	with_isolated_env(|home| {
		install_old_skill(home, "old-skill"); // Claude: will install fine.

		// Amp/Kimi's universal-XDG read path, isolated to its OWN tempdir
		// (separate from `home`) so it is unaffected by the write-path block
		// below. Amp/Kimi are discovered as having "old-skill" here.
		let xdg_config = tempfile::tempdir().unwrap();
		let _xdg_guard = EnvVarGuard::set("XDG_CONFIG_HOME", xdg_config.path());
		let xdg_skills = xdg_config.path().join("agents/skills");
		install_old_skill_under(
			xdg_config.path(),
			"agents/skills",
			"old-skill",
		);

		seed_global_lock("old-skill", "new-skill/SKILL.md");
		let repo = fake_repo("new-skill", "new-skill");
		let source = source_for("new-skill/SKILL.md");

		// Block Amp/Kimi's OWN write target (`~/.config/agents/skills`, a
		// DIFFERENT physical dir than the XDG read path above): no write
		// permission on its parent, so its `new-skill` link can never be
		// created. Root ignores 0o500, so probe and skip rather than
		// false-pass.
		let write_parent = home.join(".config/agents");
		std::fs::create_dir_all(&write_parent).unwrap();
		std::fs::set_permissions(
			&write_parent,
			std::fs::Permissions::from_mode(0o500),
		)
		.unwrap();
		// XDG_CONFIG_HOME restore is handled by `_xdg_guard`'s Drop (panic-
		// safe); this closure only restores the permissions this test itself
		// set.
		let restore_perms = |write_parent: &Path| {
			std::fs::set_permissions(
				write_parent,
				std::fs::Permissions::from_mode(0o755),
			)
			.unwrap();
		};
		let probe = write_parent.join(".rename-root-probe");
		if std::fs::write(&probe, b"x").is_ok() {
			let _ = std::fs::remove_file(&probe);
			restore_perms(&write_parent);
			eprintln!("skipping under root: 0o500 is not enforced");
			return;
		}

		// File identity of an old-name file that the transaction must NEVER
		// touch on an install-stage failure. Asserted below: a rollback that
		// restores the snapshot would remove_dir_all this dir and re-copy it
		// from the backup -- swallowing any error in that copy, which is how an
		// intact old skill gets destroyed by its own rollback.
		//
		// The open handle is load-bearing, not decoration: it pins the inode so
		// the kernel cannot hand that number to a re-copied file. Neither field
		// is sound alone -- on ext4 the inode WAS observed being reused
		// immediately after unlink, and a coarse-timestamp filesystem could
		// preserve mtime within one tick.
		let claude_skill_md = home.join(".claude/skills/old-skill/SKILL.md");
		let pinned = std::fs::File::open(&claude_skill_md).unwrap();
		let identity = |m: &std::fs::Metadata| {
			use std::os::unix::fs::MetadataExt;
			(m.dev(), m.ino(), m.mtime(), m.mtime_nsec())
		};
		let before = identity(&pinned.metadata().unwrap());

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

		// Restore perms before asserting so cleanup can proceed.
		restore_perms(&write_parent);

		let err = result.expect_err(
			"a partial install failure must fail the whole transaction",
		);
		// Pin the SPECIFIC guard this test protects (Step 7's per-agent
		// error check): a future regression that instead failed the
		// transaction at a different step (e.g. Step 8's removal guard)
		// must NOT keep this test green.
		assert!(
			matches!(
				err,
				aghub_core::skills::rename::RenameError::InstallFailed(_)
			),
			"expected InstallFailed from the Step 7 per-agent guard, got \
			 {err:?}"
		);
		// What makes this test fail on a revert is `contains("agent '")`:
		// pre-fix the message was the raw per-agent detail with no agent id.
		// The path assertion below is a FORWARD guard, not proof of the
		// redaction -- this fixture's failure is `LinkError::Io`, whose
		// transparent Display ("Permission denied (os error 13)") is already
		// path-free. It fires for the path-bearing `LinkError` forms ("could
		// not link <link> -> <target>") if a future change stops redacting
		// them (root AGENTS.md: never return an internal path in an API
		// error).
		let msg = err.message();
		assert!(
			msg.contains("agent '"),
			"message should name the failing agent: {msg}"
		);
		assert!(
			!msg.contains('/'),
			"install-failure message must not leak a filesystem path: {msg}"
		);
		// The bug this pins: Claude's successful half must NOT let the old
		// skill disappear for the agent whose install failed.
		assert!(
			home.join(".claude/skills/old-skill").exists(),
			"old skill must remain for the agent that installed fine"
		);
		assert!(
			xdg_skills.join("old-skill").exists(),
			"old skill must remain for the agent whose install failed"
		);
		// Stronger than `exists()`: an install-stage failure must leave the old
		// name BYTE-FOR-BYTE untouched, not delete-and-restore it. Same inode
		// and mtime proves `restore_snapshot` never ran on this path -- if a
		// change routes install-stage failures back through the full rollback,
		// the file is re-copied and this fails.
		assert_eq!(
			identity(&std::fs::metadata(&claude_skill_md).unwrap()),
			before,
			"the old skill file must not be removed and re-copied by rollback"
		);
		drop(pinned);
		assert!(
			!home.join(".claude/skills/new-skill").exists(),
			"new skill must be rolled back for every agent, including the \
			 one that installed fine"
		);
		assert!(
			!xdg_skills.join("new-skill").exists(),
			"new skill must never appear on the failing agent's read path"
		);
		assert!(
			!home.join(".config/agents/skills/new-skill").exists(),
			"new skill must never appear on the failing agent's write path"
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
