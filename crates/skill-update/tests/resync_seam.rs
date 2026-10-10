//! The Resync seam refuses pre-fetch identity problems itself: an entry that
//! appeared during the fetch, and coordinates the entry does not name. Its own
//! binary because it pins `$AGHUB_DATA_DIR` and `$HOME` (process-global env).

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use aghub_core::models::ResourceScope;
use aghub_core::skills::lock::EntryIdentity;
use aghub_core::WriteScope;
use skill_update::mutation::{
	resync_fetched_source, FetchedResyncRequest, FetchedSource,
	SKILL_SOURCE_MISMATCH_CODE,
};
use skill_update::{
	FetchError, FetchSelection, FetchedRepo, Fetcher, SourceRef,
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

/// Runs `on_fetch` each time it is fetched, then serves `root` as the tree.
struct DirFetcher {
	root: PathBuf,
	on_fetch: Box<dyn Fn() + Send + Sync>,
}

impl Fetcher for DirFetcher {
	fn fetch(
		&self,
		_: &SourceRef,
		_: Option<&str>,
		_: FetchSelection<'_>,
	) -> Result<FetchedRepo, FetchError> {
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

fn sync_me_entry() -> skill::LocalSkillLockEntry {
	skill::LocalSkillLockEntry {
		source_url: None,
		source: "owner/repo".to_string(),
		ref_name: Some("main".to_string()),
		source_type: "github".to_string(),
		computed_hash: "old".to_string(),
		skill_path: Some("skills/sync-me/SKILL.md".to_string()),
		ref_commit: None,
	}
}

/// An installed `old` copy under `project`, and a fetched tree carrying `new`.
fn fixture(temporary: &Path) -> (PathBuf, PathBuf) {
	let project = temporary.join("project");
	write_skill(&project.join(".claude/skills/sync-me"), "sync-me", "old");
	let fetched_root = temporary.join("fetched");
	write_skill(&fetched_root.join("skills/sync-me"), "sync-me", "new");
	(project, fetched_root)
}

fn installed_skill_md(project: &Path) -> String {
	std::fs::read_to_string(project.join(".claude/skills/sync-me/SKILL.md"))
		.unwrap()
}

fn lock_skills(
	project: &Path,
) -> std::collections::BTreeMap<String, skill::LocalSkillLockEntry> {
	skill::lock::local::read_local_lock(Some(project)).skills
}

#[test]
fn an_entry_that_appeared_during_the_fetch_is_refused_and_nothing_is_written() {
	with_isolated_env(|| {
		let temporary = tempfile::tempdir().unwrap();
		let (project, fetched_root) = fixture(temporary.path());
		// No entry when the capture happens, so the caller never saw the skill.
		let expected = EntryIdentity::capture(
			"sync-me",
			ResourceScope::ProjectOnly,
			Some(&project),
		);
		assert!(expected.is_none());

		// Another process installs the skill while this one is fetching.
		let fetcher = DirFetcher {
			root: fetched_root,
			on_fetch: Box::new({
				let project = project.clone();
				move || {
					skill::add_skill_to_local_lock(
						"sync-me",
						sync_me_entry(),
						Some(&project),
					)
					.unwrap();
				}
			}),
		};
		let repo = fetcher
			.fetch(
				&SourceRef {
					source: "https://github.com/owner/repo".to_string(),
					ref_: Some("main".to_string()),
				},
				None,
				FetchSelection::CatalogSnapshot,
			)
			.unwrap();
		let fetched = FetchedSource::from_repo(repo);
		let before = lock_skills(&project);

		let Err(error) = resync_fetched_source(
			&fetched,
			FetchedResyncRequest {
				skill_path: "skills/sync-me/SKILL.md",
				name: "sync-me",
				scope: WriteScope::project(&project),
				source: "https://github.com/owner/repo",
				expected,
			},
		) else {
			panic!("an entry that appeared mid-fetch must be refused");
		};
		assert_eq!(
			error.code(),
			aghub_core::skills::lock::SOURCE_CHANGED_DURING_FETCH_CODE
		);
		assert_eq!(lock_skills(&project), before);
		assert!(installed_skill_md(&project).contains("old"));
	});
}

#[test]
fn fetched_coordinates_the_entry_does_not_name_are_refused_and_nothing_is_written(
) {
	with_isolated_env(|| {
		let temporary = tempfile::tempdir().unwrap();
		let (project, fetched_root) = fixture(temporary.path());
		skill::add_skill_to_local_lock(
			"sync-me",
			sync_me_entry(),
			Some(&project),
		)
		.unwrap();
		let expected = EntryIdentity::capture(
			"sync-me",
			ResourceScope::ProjectOnly,
			Some(&project),
		);
		assert!(expected.is_some());

		let fetcher = DirFetcher {
			root: fetched_root,
			on_fetch: Box::new(|| {}),
		};
		let fetched = FetchedSource::from_repo(
			fetcher
				.fetch(
					&SourceRef {
						source: "https://github.com/someone-else/repo.git"
							.to_string(),
						ref_: Some("main".to_string()),
					},
					None,
					FetchSelection::CatalogSnapshot,
				)
				.unwrap(),
		);
		let before = lock_skills(&project);

		let Err(error) = resync_fetched_source(
			&fetched,
			FetchedResyncRequest {
				skill_path: "skills/sync-me/SKILL.md",
				name: "sync-me",
				scope: WriteScope::project(&project),
				source: "https://github.com/someone-else/repo.git",
				expected,
			},
		) else {
			panic!("coordinates the entry does not name must be refused");
		};
		assert_eq!(error.code(), SKILL_SOURCE_MISMATCH_CODE);
		assert_eq!(lock_skills(&project), before);
		assert!(installed_skill_md(&project).contains("old"));
	});
}

#[test]
fn a_matching_capture_and_source_resyncs() {
	with_isolated_env(|| {
		let temporary = tempfile::tempdir().unwrap();
		let (project, fetched_root) = fixture(temporary.path());
		skill::add_skill_to_local_lock(
			"sync-me",
			sync_me_entry(),
			Some(&project),
		)
		.unwrap();
		let expected = EntryIdentity::capture(
			"sync-me",
			ResourceScope::ProjectOnly,
			Some(&project),
		);

		let fetcher = DirFetcher {
			root: fetched_root,
			on_fetch: Box::new(|| {}),
		};
		let fetched = FetchedSource::from_repo(
			fetcher
				.fetch(
					&SourceRef {
						source: "https://github.com/owner/repo".to_string(),
						ref_: Some("main".to_string()),
					},
					None,
					FetchSelection::CatalogSnapshot,
				)
				.unwrap(),
		);

		let result = resync_fetched_source(
			&fetched,
			FetchedResyncRequest {
				skill_path: "skills/sync-me/SKILL.md",
				name: "sync-me",
				scope: WriteScope::project(&project),
				source: "https://github.com/owner/repo",
				expected,
			},
		);
		assert!(result.is_ok(), "a matching capture must resync");
		assert!(installed_skill_md(&project).contains("new"));
		assert_eq!(
			lock_skills(&project)["sync-me"].ref_commit.as_deref(),
			Some("new-commit"),
		);
	});
}
