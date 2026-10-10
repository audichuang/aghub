#![cfg(unix)]
//! The rename entry (`rename_locked_skill`) refuses a taken target before the
//! fetch, rejects a lock entry repointed during the fetch, and rolls the lock
//! and disk back when the relink fails. Its own binary because it pins `$HOME`
//! and `$AGHUB_DATA_DIR` (process-global env).

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use aghub_core::WriteScope;
use skill_update::mutation::{rename_locked_skill, LockedRenameRequest};
use skill_update::{
	FetchError, FetchSelection, FetchedRepo, Fetcher, SourceRef,
	TokenResolution, TokenResolver,
};

fn env_lock() -> &'static Mutex<()> {
	static LOCK: Mutex<()> = Mutex::new(());
	&LOCK
}

struct EnvVarGuard(&'static str, Option<std::ffi::OsString>);

impl EnvVarGuard {
	fn set(key: &'static str, path: &Path) -> Self {
		let previous = std::env::var_os(key);
		std::env::set_var(key, path);
		EnvVarGuard(key, previous)
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

/// HOME, XDG dirs and AGHUB_DATA_DIR in fresh tempdirs. `claude` stays managed,
/// so `.claude/skills` is a real target. Serialized; RAII restoration.
fn with_isolated_env(f: impl FnOnce()) {
	let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
	let home = tempfile::tempdir().unwrap();
	let xdg_state = tempfile::tempdir().unwrap();
	let xdg_data = tempfile::tempdir().unwrap();
	let xdg_config = tempfile::tempdir().unwrap();
	let data = tempfile::tempdir().unwrap();
	// Declared AFTER the tempdirs so they drop FIRST: the env is restored to the
	// real values before these directories are deleted.
	let _home_guard = EnvVarGuard::set("HOME", home.path());
	let _state_guard = EnvVarGuard::set("XDG_STATE_HOME", xdg_state.path());
	let _data_guard = EnvVarGuard::set("XDG_DATA_HOME", xdg_data.path());
	let _config_guard = EnvVarGuard::set("XDG_CONFIG_HOME", xdg_config.path());
	let _aghub_guard = EnvVarGuard::set("AGHUB_DATA_DIR", data.path());
	f();
}

fn write_skill(directory: &Path, name: &str, description: &str) {
	std::fs::create_dir_all(directory).unwrap();
	std::fs::write(
		directory.join("SKILL.md"),
		format!(
			"---\nname: {name}\ndescription: {description}\n---\n\n{description}\n"
		),
	)
	.unwrap();
}

/// Runs `on_fetch` each time it is fetched, counts fetches, then serves `root`.
struct DirFetcher {
	root: PathBuf,
	calls: AtomicUsize,
	on_fetch: Box<dyn Fn() + Send + Sync>,
}

impl Fetcher for DirFetcher {
	fn fetch(
		&self,
		_: &SourceRef,
		_: Option<&str>,
		_: FetchSelection<'_>,
	) -> Result<FetchedRepo, FetchError> {
		self.calls.fetch_add(1, Ordering::SeqCst);
		(self.on_fetch)();
		Ok(FetchedRepo {
			root: self.root.clone(),
			snapshot: aghub_git::RepoSnapshot {
				commit_oid: "new-commit".into(),
				tree_oid: "new-tree".into(),
				commit_time: None,
			},
			_guard: None,
		})
	}
}

struct NoToken;
impl TokenResolver for NoToken {
	fn resolve(&self, _source: &str) -> TokenResolution {
		TokenResolution::NoToken
	}
}

fn lock_entry(skill_path: &str) -> skill::LocalSkillLockEntry {
	skill::LocalSkillLockEntry {
		source_url: None,
		source: "owner/repo".to_string(),
		ref_name: Some("main".to_string()),
		source_type: "github".to_string(),
		computed_hash: "old".to_string(),
		skill_path: Some(skill_path.to_string()),
		ref_commit: None,
	}
}

/// An installed `old-skill` under `project` (locked), and a fetched tree that
/// carries `new-skill`.
fn fixture(temporary: &Path) -> (PathBuf, PathBuf) {
	let project = temporary.join("project");
	write_skill(
		&project.join(".claude/skills/old-skill"),
		"old-skill",
		"old",
	);
	skill::add_skill_to_local_lock(
		"old-skill",
		lock_entry("skills/old-skill/SKILL.md"),
		Some(&project),
	)
	.unwrap();
	let fetched_root = temporary.join("fetched");
	write_skill(&fetched_root.join("skills/new-skill"), "new-skill", "new");
	(project, fetched_root)
}

fn dir_fetcher(
	root: PathBuf,
	on_fetch: Box<dyn Fn() + Send + Sync>,
) -> DirFetcher {
	DirFetcher {
		root,
		calls: AtomicUsize::new(0),
		on_fetch,
	}
}

fn lock_skills(
	project: &Path,
) -> std::collections::BTreeMap<String, skill::LocalSkillLockEntry> {
	skill::lock::local::read_local_lock(Some(project)).skills
}

fn request(project: &Path) -> LockedRenameRequest<'static> {
	LockedRenameRequest {
		old_name: "old-skill",
		new_name: "new-skill",
		scope: WriteScope::project(project),
		git_ref: None,
	}
}

#[test]
fn rename_locked_skill_installs_the_new_name_and_rewrites_the_lock() {
	with_isolated_env(|| {
		let temporary = tempfile::tempdir().unwrap();
		let (project, fetched_root) = fixture(temporary.path());
		let fetcher = dir_fetcher(fetched_root, Box::new(|| {}));

		let outcome = rename_locked_skill(
			LockedRenameRequest {
				git_ref: Some("v2"),
				..request(&project)
			},
			&fetcher,
			&NoToken,
		);
		assert!(outcome.is_ok(), "a clean rename must commit");

		let installed = project.join(".claude/skills/new-skill/SKILL.md");
		assert!(std::fs::read_to_string(installed).unwrap().contains("new"));
		assert!(std::fs::symlink_metadata(
			project.join(".claude/skills/old-skill")
		)
		.is_err());
		let lock = lock_skills(&project);
		assert!(!lock.contains_key("old-skill"));
		let new_entry = &lock["new-skill"];
		assert_eq!(
			new_entry.skill_path.as_deref(),
			Some("skills/new-skill/SKILL.md")
		);
		assert_eq!(new_entry.ref_commit.as_deref(), Some("new-commit"));
		assert_eq!(new_entry.ref_name.as_deref(), Some("v2"));
		assert_eq!(fetcher.calls.load(Ordering::SeqCst), 1);
	});
}

#[test]
fn a_rename_target_that_already_exists_is_refused_before_the_fetch() {
	with_isolated_env(|| {
		let temporary = tempfile::tempdir().unwrap();
		let (project, fetched_root) = fixture(temporary.path());
		skill::add_skill_to_local_lock(
			"new-skill",
			lock_entry("skills/new-skill/SKILL.md"),
			Some(&project),
		)
		.unwrap();
		let before = lock_skills(&project);
		let fetcher = dir_fetcher(fetched_root, Box::new(|| {}));

		let Err(error) =
			rename_locked_skill(request(&project), &fetcher, &NoToken)
		else {
			panic!("a taken target must be refused");
		};
		assert_eq!(
			error.code(),
			Some(aghub_core::skills::rename::RENAME_TARGET_EXISTS_CODE)
		);
		assert_eq!(fetcher.calls.load(Ordering::SeqCst), 0);
		assert_eq!(lock_skills(&project), before);
		assert!(std::fs::read_to_string(
			project.join(".claude/skills/old-skill/SKILL.md")
		)
		.unwrap()
		.contains("old"));
	});
}

#[test]
fn a_lock_entry_repointed_during_the_fetch_is_refused_and_nothing_is_written() {
	with_isolated_env(|| {
		let temporary = tempfile::tempdir().unwrap();
		let (project, fetched_root) = fixture(temporary.path());
		// Another process repoints `old-skill` while this rename is fetching.
		let fetcher = dir_fetcher(
			fetched_root,
			Box::new({
				let project = project.clone();
				move || {
					skill::add_skill_to_local_lock(
						"old-skill",
						lock_entry("skills/elsewhere/SKILL.md"),
						Some(&project),
					)
					.unwrap();
				}
			}),
		);

		let Err(error) =
			rename_locked_skill(request(&project), &fetcher, &NoToken)
		else {
			panic!("an entry repointed mid-fetch must be refused");
		};
		assert_eq!(
			error.code(),
			Some(aghub_core::skills::lock::SOURCE_CHANGED_DURING_FETCH_CODE)
		);
		let lock = lock_skills(&project);
		assert_eq!(
			lock["old-skill"].skill_path.as_deref(),
			Some("skills/elsewhere/SKILL.md"),
			"the other writer's entry must survive untouched"
		);
		assert!(!lock.contains_key("new-skill"));
		assert!(std::fs::read_to_string(
			project.join(".claude/skills/old-skill/SKILL.md")
		)
		.unwrap()
		.contains("old"));
		assert!(std::fs::symlink_metadata(project.join(".aghub/new-skill"))
			.is_err());
		assert!(std::fs::symlink_metadata(
			project.join(".claude/skills/new-skill")
		)
		.is_err());
	});
}

#[test]
fn a_failed_relink_rolls_back_the_lock_and_disk() {
	with_isolated_env(|| {
		let temporary = tempfile::tempdir().unwrap();
		let (project, fetched_root) = fixture(temporary.path());
		let before = lock_skills(&project);

		// Lock the Claude skills dir read-only so the new-name link cannot be
		// created. Root ignores 0o500, so probe and skip rather than false-pass.
		let skills_dir = project.join(".claude/skills");
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

		let fetcher = dir_fetcher(fetched_root, Box::new(|| {}));
		let result = rename_locked_skill(request(&project), &fetcher, &NoToken);

		// Restore perms before asserting so the tempdir can be cleaned up.
		std::fs::set_permissions(&skills_dir, original).unwrap();

		assert!(
			result.is_err(),
			"the relink must fail under a read-only dir"
		);
		assert_eq!(lock_skills(&project), before);
		assert!(std::fs::read_to_string(
			project.join(".claude/skills/old-skill/SKILL.md")
		)
		.unwrap()
		.contains("old"));
		assert!(std::fs::symlink_metadata(project.join(".aghub/new-skill"))
			.is_err());
		assert!(std::fs::symlink_metadata(
			project.join(".claude/skills/new-skill")
		)
		.is_err());
	});
}
