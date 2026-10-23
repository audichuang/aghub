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
		Ok(FetchedRepo {
			root,
			snapshot: aghub_git::RepoSnapshot {
				commit_oid: commit.to_string(),
				tree_oid: format!("{commit}-tree"),
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
#[ignore = "known bug (T5): the comparison hash still counts what the Master copier drops, so the check never reads UpToDate"]
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
#[ignore = "known bug (T5): the comparison hash still counts what the Master copier drops, so the check never reads UpToDate"]
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
