//! The check's verdict and the content an update installs must agree, and the
//! check must compare the same bytes the install materializes. Each test drives
//! the real install path into a tempdir project, then the real check.
//!
//! Its own binary because it pins `$HOME`, `XDG_*` and `$AGHUB_DATA_DIR`;
//! unix-only because Windows ignores `$HOME` and links need privileges.
#![cfg(unix)]

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use aghub_core::skills::update::SkillUpdateStatus;
use aghub_core::WriteScope;
use skill_update::mutation::{
	install_fetched_source, FetchedInstallRequest, FetchedSource,
};
use skill_update::{
	CheckOutput, FetchError, FetchSelection, FetchedRepo, Fetcher, RefResolver,
	SourceRef, TipObservation, TokenResolution, TokenResolver,
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

/// HOME, XDG dirs and AGHUB_DATA_DIR in fresh tempdirs (no agent selection
/// file, so every agent is managed). Serialized; RAII restoration.
fn with_isolated_env<T>(f: impl FnOnce() -> T) -> T {
	let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
	let home = tempfile::tempdir().unwrap();
	let xdg_state = tempfile::tempdir().unwrap();
	let xdg_data = tempfile::tempdir().unwrap();
	let xdg_config = tempfile::tempdir().unwrap();
	let data = tempfile::tempdir().unwrap();
	// Declared AFTER the tempdirs so they drop FIRST.
	let _home_guard = EnvVarGuard::set("HOME", home.path());
	let _state_guard = EnvVarGuard::set("XDG_STATE_HOME", xdg_state.path());
	let _data_guard = EnvVarGuard::set("XDG_DATA_HOME", xdg_data.path());
	let _config_guard = EnvVarGuard::set("XDG_CONFIG_HOME", xdg_config.path());
	let _aghub_guard = EnvVarGuard::set("AGHUB_DATA_DIR", data.path());
	f()
}

/// Serves one `(root, commit)` per call; the last one repeats. Models a tip
/// that moves between two fetches.
struct SequenceFetcher {
	tips: Mutex<VecDeque<(PathBuf, &'static str)>>,
}

impl SequenceFetcher {
	fn new(tips: Vec<(PathBuf, &'static str)>) -> Self {
		Self {
			tips: Mutex::new(tips.into()),
		}
	}
}

impl Fetcher for SequenceFetcher {
	fn fetch(
		&self,
		_source_ref: &SourceRef,
		_token: Option<&str>,
		_selection: FetchSelection<'_>,
	) -> Result<FetchedRepo, FetchError> {
		let mut tips = self.tips.lock().unwrap();
		let (root, commit) = if tips.len() > 1 {
			tips.pop_front().unwrap()
		} else {
			tips.front().cloned().unwrap()
		};
		Ok(FetchedRepo::new(
			root,
			aghub_git::RepoSnapshot {
				commit_oid: commit.to_string(),
				tree_oid: format!("{commit}-tree"),
				commit_time: None,
			},
			None,
		))
	}
}

struct NoToken;

impl TokenResolver for NoToken {
	fn resolve(&self, _source: &str) -> TokenResolution {
		TokenResolution::NoToken
	}
}

/// Every resolve fails, so the check always falls through to the fetch.
struct NoTip;

impl RefResolver for NoTip {
	fn resolve(
		&self,
		_source_ref: &SourceRef,
		_token: Option<&str>,
	) -> Result<TipObservation, FetchError> {
		Err(FetchError::network("unused"))
	}
}

const SOURCE_URL: &str = "https://git.example/owner/repo.git";

fn write_skill(directory: &Path, name: &str, body: &str) {
	std::fs::create_dir_all(directory).unwrap();
	std::fs::write(
		directory.join("SKILL.md"),
		format!("---\nname: {name}\ndescription: test\n---\n\n{body}\n"),
	)
	.unwrap();
}

/// Install `skills/<name>` from `root` (recorded as `commit`) into `project`
/// through the real fetched-install seam.
fn install(project: &Path, root: &Path, commit: &str, name: &str) {
	let fetcher = SequenceFetcher::new(vec![(root.to_path_buf(), "unused")]);
	let mut repo = fetcher
		.fetch(
			&SourceRef {
				source: SOURCE_URL.into(),
				ref_: Some("main".into()),
			},
			None,
			FetchSelection::CatalogSnapshot,
		)
		.unwrap();
	repo.snapshot.commit_oid = commit.to_string();
	install_fetched_source(
		&FetchedSource::from_repo(repo),
		FetchedInstallRequest {
			source: &skill::InstallLockSource {
				source: "owner/repo".into(),
				source_type: "git".into(),
				source_url: SOURCE_URL.into(),
				ref_name: Some("main".into()),
			},
			lock_skill_path: &format!("skills/{name}/SKILL.md"),
			expected_name: None,
			scope: WriteScope::project(project),
			target_agents: &[aghub_core::models::AgentType::Claude],
			expected_ref: None,
		},
	)
	.unwrap();
}

fn check(project: &Path, fetcher: Arc<dyn Fetcher>) -> Vec<CheckOutput> {
	let (entries, _) = skill_update::projection::project_lock_entries(
		false,
		Some(project),
		|| skill::lock::local::read_local_lock(Some(project)),
	);
	tokio::runtime::Builder::new_current_thread()
		.enable_all()
		.build()
		.unwrap()
		.block_on(skill_update::run_update_check(
			entries,
			fetcher,
			Arc::new(NoTip),
			&NoToken,
			false,
		))
}

fn only_status(out: &[CheckOutput]) -> &SkillUpdateStatus {
	assert_eq!(out.len(), 1, "one entry in, one row out: {out:?}");
	&out[0].status
}

fn installed_body(project: &Path, name: &str) -> String {
	std::fs::read_to_string(
		project.join(".claude/skills").join(name).join("SKILL.md"),
	)
	.unwrap()
}

/// Run the check, then resync every `UpdateAvailable` row with the commit the
/// check judged it against — `apply-update --outdated`'s composition.
fn check_then_resync(
	project: &Path,
	fetcher: Arc<SequenceFetcher>,
) -> Vec<skill_update::mutation::LockedSkillResyncResult> {
	let out = check(project, fetcher.clone());
	assert!(
		matches!(only_status(&out), SkillUpdateStatus::UpdateAvailable { .. }),
		"{out:?}"
	);
	let judged = out
		.iter()
		.filter_map(|o| Some((o.key.name.clone(), o.judged_commit.clone()?)))
		.collect();
	let names = vec!["alpha".to_string()];
	skill_update::mutation::resync_judged_skills(
		skill_update::mutation::LockedSkillsResyncRequest {
			source_group: None,
			names: &names,
			scope: WriteScope::project(project),
		},
		&judged,
		fetcher.as_ref(),
		&NoToken,
	)
	.unwrap()
}

/// `apply-update --outdated`: the check judges commit `c0`, the tip moves to
/// `c1` before the resync fetches. The row must be refused with nothing
/// written — never silently install `c1`, which nobody judged.
#[test]
fn outdated_resync_never_installs_a_tip_the_check_did_not_judge() {
	with_isolated_env(|| {
		let tmp = tempfile::tempdir().unwrap();
		let project = tmp.path().join("project");
		std::fs::create_dir_all(&project).unwrap();
		let old = tmp.path().join("old");
		let judged = tmp.path().join("judged");
		let moved = tmp.path().join("moved");
		write_skill(&old.join("skills/alpha"), "alpha", "old");
		write_skill(&judged.join("skills/alpha"), "alpha", "judged");
		write_skill(&moved.join("skills/alpha"), "alpha", "moved");
		install(&project, &old, "c-old", "alpha");

		// The check sees `c0`, every later fetch sees `c1`.
		let fetcher =
			Arc::new(SequenceFetcher::new(vec![(judged, "c0"), (moved, "c1")]));
		let results = check_then_resync(&project, fetcher);

		let error = results[0].outcome.as_ref().expect_err("must refuse");
		assert_eq!(error.code(), "SKILL_SOURCE_CHANGED_DURING_FETCH");
		assert!(installed_body(&project, "alpha").contains("old"));
		let lock = skill::lock::local::read_local_lock(Some(&project));
		assert_eq!(lock.skills["alpha"].ref_commit.as_deref(), Some("c-old"));
	});
}

/// The tip did not move: the judged commit installs as before.
#[test]
fn outdated_resync_installs_the_judged_commit() {
	with_isolated_env(|| {
		let tmp = tempfile::tempdir().unwrap();
		let project = tmp.path().join("project");
		std::fs::create_dir_all(&project).unwrap();
		let old = tmp.path().join("old");
		let judged = tmp.path().join("judged");
		write_skill(&old.join("skills/alpha"), "alpha", "old");
		write_skill(&judged.join("skills/alpha"), "alpha", "judged");
		install(&project, &old, "c-old", "alpha");

		let fetcher = Arc::new(SequenceFetcher::new(vec![(judged, "c0")]));
		let results = check_then_resync(&project, fetcher);

		assert!(results[0].outcome.is_ok(), "{results:?}");
		assert!(installed_body(&project, "alpha").contains("judged"));
		let lock = skill::lock::local::read_local_lock(Some(&project));
		assert_eq!(lock.skills["alpha"].ref_commit.as_deref(), Some("c0"));
	});
}

/// Upstream ships `metadata.json`; the Master copier leaves it out (npx
/// `copyDirectory` parity). The next check against the SAME upstream must
/// read `UpToDate`, or `apply-update` can never converge.
#[test]
fn metadata_json_upstream_converges_after_install() {
	with_isolated_env(|| {
		let tmp = tempfile::tempdir().unwrap();
		let project = tmp.path().join("project");
		std::fs::create_dir_all(&project).unwrap();
		let upstream = tmp.path().join("upstream");
		write_skill(&upstream.join("skills/alpha"), "alpha", "body");
		std::fs::write(upstream.join("skills/alpha/metadata.json"), "{}")
			.unwrap();
		install(&project, &upstream, "c0", "alpha");
		assert!(
			!project.join(".aghub/alpha/metadata.json").exists(),
			"precondition: the Master copier excludes metadata.json"
		);

		let out = check(
			&project,
			Arc::new(SequenceFetcher::new(vec![(upstream, "c0")])),
		);
		assert_eq!(only_status(&out), &SkillUpdateStatus::UpToDate);
	});
}

/// Upstream commits `__pycache__/x.cpython-312.pyc` with no source `.py`; the
/// copier drops the whole `__pycache__` directory.
#[test]
fn orphan_pycache_upstream_converges_after_install() {
	with_isolated_env(|| {
		let tmp = tempfile::tempdir().unwrap();
		let project = tmp.path().join("project");
		std::fs::create_dir_all(&project).unwrap();
		let upstream = tmp.path().join("upstream");
		write_skill(&upstream.join("skills/alpha"), "alpha", "body");
		std::fs::create_dir_all(upstream.join("skills/alpha/__pycache__"))
			.unwrap();
		std::fs::write(
			upstream.join("skills/alpha/__pycache__/x.cpython-312.pyc"),
			"bytecode",
		)
		.unwrap();
		install(&project, &upstream, "c0", "alpha");
		assert!(
			!project.join(".aghub/alpha/__pycache__").exists(),
			"precondition: the Master copier excludes __pycache__"
		);

		let out = check(
			&project,
			Arc::new(SequenceFetcher::new(vec![(upstream, "c0")])),
		);
		assert_eq!(only_status(&out), &SkillUpdateStatus::UpToDate);
	});
}

/// A lock `skillPath` without the `/SKILL.md` suffix is accepted by
/// `skill_folder_from_lock_path`; the probe must still hash THAT folder.
#[test]
#[ignore = "known bug: probe_skill_hash_in_repo hashes parent() of a suffix-less skillPath (owned by T1)"]
fn suffixless_lock_skill_path_hashes_the_skill_folder() {
	with_isolated_env(|| {
		let tmp = tempfile::tempdir().unwrap();
		let project = tmp.path().join("project");
		std::fs::create_dir_all(&project).unwrap();
		let upstream = tmp.path().join("upstream");
		write_skill(&upstream.join("skills/alpha"), "alpha", "body");
		install(&project, &upstream, "c0", "alpha");
		let mut lock = skill::lock::local::read_local_lock(Some(&project));
		lock.skills.get_mut("alpha").unwrap().skill_path =
			Some("skills/alpha".into());
		skill::write_local_lock(&lock, Some(&project)).unwrap();

		let out = check(
			&project,
			Arc::new(SequenceFetcher::new(vec![(upstream, "c0")])),
		);
		assert_eq!(only_status(&out), &SkillUpdateStatus::UpToDate);
	});
}

/// Install and resync are two Master materializers; fed the same upstream
/// (with files npx `copyDirectory` drops) they must write the same Master.
#[test]
fn install_and_resync_write_the_same_master() {
	with_isolated_env(|| {
		let tmp = tempfile::tempdir().unwrap();
		let project = tmp.path().join("project");
		std::fs::create_dir_all(&project).unwrap();
		let upstream = tmp.path().join("upstream");
		let folder = upstream.join("skills/alpha");
		write_skill(&folder, "alpha", "body");
		std::fs::write(folder.join("metadata.json"), "{}").unwrap();
		std::fs::create_dir_all(folder.join("__pycache__")).unwrap();
		std::fs::write(folder.join("__pycache__/x.cpython-312.pyc"), "bc")
			.unwrap();
		install(&project, &upstream, "c0", "alpha");
		let master = project.join(".aghub/alpha");
		let installed = skill::compute_skill_folder_hash(&master).unwrap();

		let names = vec!["alpha".to_string()];
		let results = skill_update::mutation::resync_locked_skills(
			skill_update::mutation::LockedSkillsResyncRequest {
				source_group: None,
				names: &names,
				scope: WriteScope::project(&project),
			},
			&SequenceFetcher::new(vec![(upstream, "c0")]),
			&NoToken,
		)
		.unwrap();
		assert!(results[0].outcome.is_ok(), "{results:?}");

		assert!(!master.join("metadata.json").exists());
		assert!(!master.join("__pycache__").exists());
		assert_eq!(
			skill::compute_skill_folder_hash(&master).unwrap(),
			installed
		);
		let lock = skill::lock::local::read_local_lock(Some(&project));
		assert_eq!(
			lock.skills["alpha"].computed_hash,
			skill::compute_skill_folder_hash(&folder).unwrap(),
			"the lock keeps the raw source hash (npx parity)"
		);
	});
}

/// `skills/alpha` with `body`, plus what the Master copier drops: a
/// `metadata.json` and an orphan `__pycache__` bytecode file.
fn write_upstream_with_dropped_files(root: &Path, body: &str) {
	let folder = root.join("skills/alpha");
	write_skill(&folder, "alpha", body);
	std::fs::write(folder.join("metadata.json"), "{}").unwrap();
	std::fs::create_dir_all(folder.join("__pycache__")).unwrap();
	std::fs::write(folder.join("__pycache__/x.cpython-312.pyc"), "bc").unwrap();
}

fn diff_state(
	project: &Path,
	upstream: &Path,
) -> skill_update::sources::SourceSkillState {
	let outcome = skill_update::sources::diff_source(
		skill_update::sources::SourceDiffInput {
			source: "owner/repo".to_string(),
			git_ref: None,
			scopes: vec![WriteScope::project(project)],
		},
		skill_update::sources::SourceDiffDeps {
			fetcher: &SequenceFetcher::new(vec![(
				upstream.to_path_buf(),
				"c0",
			)]),
			resolver: &NoToken,
		},
	);
	let skills = match outcome {
		skill_update::sources::SourceDiffOutcome::Ok { skills, .. } => skills,
		other => panic!("{other:?}"),
	};
	let row = skills
		.iter()
		.find(|d| d.name == "alpha")
		.expect("alpha row");
	row.state.clone()
}

/// The copier's exclusions must not blind the check: once the dropped files
/// stop counting, a real `SKILL.md` change upstream still reads as an update.
#[test]
fn check_ignores_dropped_files_but_still_sees_real_changes() {
	with_isolated_env(|| {
		let tmp = tempfile::tempdir().unwrap();
		let project = tmp.path().join("project");
		std::fs::create_dir_all(&project).unwrap();
		let upstream = tmp.path().join("upstream");
		let edited = tmp.path().join("edited");
		write_upstream_with_dropped_files(&upstream, "body");
		write_upstream_with_dropped_files(&edited, "new body");
		install(&project, &upstream, "c0", "alpha");

		let out = check(
			&project,
			Arc::new(SequenceFetcher::new(vec![(upstream, "c0")])),
		);
		assert_eq!(only_status(&out), &SkillUpdateStatus::UpToDate);
		let out = check(
			&project,
			Arc::new(SequenceFetcher::new(vec![(edited, "c1")])),
		);
		assert!(
			matches!(
				only_status(&out),
				SkillUpdateStatus::UpdateAvailable { .. }
			),
			"{out:?}"
		);
	});
}

/// `source diff` judges with the same projection as `check`.
#[test]
fn source_diff_ignores_dropped_files_but_still_sees_real_changes() {
	use skill_update::sources::SourceSkillState;
	with_isolated_env(|| {
		let tmp = tempfile::tempdir().unwrap();
		let project = tmp.path().join("project");
		std::fs::create_dir_all(&project).unwrap();
		let upstream = tmp.path().join("upstream");
		let edited = tmp.path().join("edited");
		write_upstream_with_dropped_files(&upstream, "body");
		write_upstream_with_dropped_files(&edited, "new body");
		install(&project, &upstream, "c0", "alpha");

		assert_eq!(
			diff_state(&project, &upstream),
			SourceSkillState::InstalledCurrent
		);
		assert_eq!(
			diff_state(&project, &edited),
			SourceSkillState::InstalledOutdated
		);
	});
}

/// `skills/alpha` whose root `SKILL.md` is a link to an in-folder
/// `docs/SKILL.md`: containment holds, but the Master copier skips links.
fn write_upstream_with_linked_skill_md(root: &Path) {
	let folder = root.join("skills/alpha");
	write_skill(&folder.join("docs"), "alpha", "linked");
	std::os::unix::fs::symlink("docs/SKILL.md", folder.join("SKILL.md"))
		.unwrap();
}

/// A resync from a source whose `SKILL.md` is a link would swap in a Master
/// with no `SKILL.md` (the copier skips links) and still advance the lock.
/// It must be refused before anything is touched.
#[test]
fn a_linked_skill_md_resync_is_refused_and_the_master_is_kept() {
	with_isolated_env(|| {
		let tmp = tempfile::tempdir().unwrap();
		let project = tmp.path().join("project");
		std::fs::create_dir_all(&project).unwrap();
		let v0 = tmp.path().join("v0");
		write_skill(&v0.join("skills/alpha"), "alpha", "v0 body");
		install(&project, &v0, "c0", "alpha");
		let master = project.join(".aghub/alpha");
		let master_before = skill::compute_skill_folder_hash(&master).unwrap();
		let lock_file = project.join("skills-lock.json");
		let lock_before = std::fs::read(&lock_file).unwrap();
		let v1 = tmp.path().join("v1");
		write_upstream_with_linked_skill_md(&v1);

		let names = vec!["alpha".to_string()];
		let results = skill_update::mutation::resync_locked_skills(
			skill_update::mutation::LockedSkillsResyncRequest {
				source_group: None,
				names: &names,
				scope: WriteScope::project(&project),
			},
			&SequenceFetcher::new(vec![(v1, "c1")]),
			&NoToken,
		)
		.unwrap();

		let Err(error) = &results[0].outcome else {
			panic!("a linked SKILL.md must be refused: {results:?}");
		};
		assert!(
			format!("{error:?}").contains("not a regular file"),
			"refused for the link, not for a missing path: {error:?}"
		);
		assert!(master.join("SKILL.md").is_file());
		assert!(installed_body(&project, "alpha").contains("v0 body"));
		assert_eq!(
			skill::compute_skill_folder_hash(&master).unwrap(),
			master_before
		);
		assert_eq!(std::fs::read(&lock_file).unwrap(), lock_before);
	});
}

/// A fresh fetched install from the same source must be refused with no
/// Master, no Referrer and no lock entry left behind.
#[test]
fn a_linked_skill_md_fresh_install_is_refused_with_no_residue() {
	with_isolated_env(|| {
		let tmp = tempfile::tempdir().unwrap();
		let project = tmp.path().join("project");
		std::fs::create_dir_all(&project).unwrap();
		let upstream = tmp.path().join("upstream");
		write_upstream_with_linked_skill_md(&upstream);
		let fetcher = SequenceFetcher::new(vec![(upstream, "c0")]);
		let repo = fetcher
			.fetch(
				&SourceRef {
					source: SOURCE_URL.into(),
					ref_: Some("main".into()),
				},
				None,
				FetchSelection::CatalogSnapshot,
			)
			.unwrap();

		let error = install_fetched_source(
			&FetchedSource::from_repo(repo),
			FetchedInstallRequest {
				source: &skill::InstallLockSource {
					source: "owner/repo".into(),
					source_type: "git".into(),
					source_url: SOURCE_URL.into(),
					ref_name: Some("main".into()),
				},
				lock_skill_path: "skills/alpha/SKILL.md",
				expected_name: None,
				scope: WriteScope::project(&project),
				target_agents: &[aghub_core::models::AgentType::Claude],
				expected_ref: None,
			},
		)
		.expect_err("a linked SKILL.md must be refused");

		assert!(
			format!("{error:?}").contains("not a regular file"),
			"refused for the link, not for a missing path: {error:?}"
		);
		assert!(!project.join(".aghub/alpha").exists());
		assert!(std::fs::symlink_metadata(
			project.join(".claude/skills/alpha")
		)
		.is_err());
		assert!(!skill::lock::local::read_local_lock(Some(&project))
			.skills
			.contains_key("alpha"));
	});
}
