//! One table, two surfaces: `verdict::judge` and the `check` adapter must agree
//! on every row. Its own binary because it pins `$AGHUB_DATA_DIR` and `$HOME`
//! (process-global env that must not race the lib's env-touching tests).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use aghub_core::skills::update::{SkillUpdateStatus, UncheckableReason};
use skill_update::verdict::Verdict;
use skill_update::{
	FetchError, FetchSelection, FetchedRepo, Fetcher, RefResolver, SourceRef,
	TipObservation, TokenResolution, TokenResolver,
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

/// HOME, XDG dirs and AGHUB_DATA_DIR in fresh tempdirs, with `claude` disabled.
/// Serialized against this binary's other env users; RAII restoration.
fn with_isolated_env<T>(f: impl FnOnce() -> T) -> T {
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
	aghub_core::agent_settings::write_disabled_agents_in(
		data.path(),
		&BTreeSet::from(["claude".to_string()]),
	)
	.unwrap();
	f()
}

struct DirFetcher {
	root: PathBuf,
}

impl Fetcher for DirFetcher {
	fn fetch(
		&self,
		_source_ref: &SourceRef,
		_token: Option<&str>,
		_selection: FetchSelection<'_>,
	) -> Result<FetchedRepo, FetchError> {
		Ok(FetchedRepo {
			root: self.root.clone(),
			snapshot: aghub_git::RepoSnapshot {
				commit_oid: "c".to_string(),
				tree_oid: "t".to_string(),
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

/// Project entries carry `ref_commit: None`, so the preflight never asks.
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

fn write_skill(directory: &Path, name: &str, body: &str) {
	std::fs::create_dir_all(directory).unwrap();
	std::fs::write(
		directory.join("SKILL.md"),
		format!("---\nname: {name}\ndescription: test\n---\n\n{body}\n"),
	)
	.unwrap();
}

#[derive(Clone, Copy, Debug)]
enum Local {
	Readable,
	Withheld,
	WithheldEdited,
	Gone,
	Ambiguous,
	DisabledDiffers,
}

struct Row {
	label: &'static str,
	local: Local,
	/// `"v1"` stores the raw hash of the v1 folder; `"placeholder"` stores
	/// `EMPTY_SKILLS_LOCK_DIGEST`.
	stored: &'static str,
	upstream: &'static str,
	verdict: &'static str,
	check: &'static str,
}

const ROWS: &[Row] = &[
	Row {
		label: "readable current",
		local: Local::Readable,
		stored: "v1",
		upstream: "v1",
		verdict: "upToDate",
		check: "upToDate",
	},
	Row {
		label: "readable behind",
		local: Local::Readable,
		stored: "v1",
		upstream: "v2",
		verdict: "updateAvailable",
		check: "updateAvailable",
	},
	Row {
		label: "withheld current",
		local: Local::Withheld,
		stored: "v1",
		upstream: "v1",
		verdict: "upToDate",
		check: "upToDate",
	},
	Row {
		label: "withheld edited, upstream unmoved",
		local: Local::WithheldEdited,
		stored: "v1",
		upstream: "v1",
		verdict: "updateAvailable",
		check: "updateAvailable",
	},
	Row {
		label: "copy gone, lock matches upstream",
		local: Local::Gone,
		stored: "v1",
		upstream: "v1",
		verdict: "uncheckable",
		check: "uncheckable:local",
	},
	Row {
		label: "copy gone, lock behind upstream",
		local: Local::Gone,
		stored: "v1",
		upstream: "v2",
		verdict: "updateAvailable",
		check: "updateAvailable",
	},
	Row {
		label: "placeholder, readable",
		local: Local::Readable,
		stored: "placeholder",
		upstream: "v1",
		verdict: "upToDate",
		check: "upToDate",
	},
	Row {
		label: "placeholder, withheld",
		local: Local::Withheld,
		stored: "placeholder",
		upstream: "v1",
		verdict: "upToDate",
		check: "upToDate",
	},
	Row {
		label: "placeholder, copy gone",
		local: Local::Gone,
		stored: "placeholder",
		upstream: "v1",
		verdict: "uncheckable",
		check: "uncheckable:local",
	},
	Row {
		label: "ambiguous, lock matches upstream",
		local: Local::Ambiguous,
		stored: "v1",
		upstream: "v1",
		verdict: "ambiguous",
		check: "uncheckable:local",
	},
	Row {
		label: "ambiguous, lock behind upstream",
		local: Local::Ambiguous,
		stored: "v1",
		upstream: "v2",
		verdict: "updateAvailable",
		check: "updateAvailable",
	},
	Row {
		label: "placeholder, ambiguous",
		local: Local::Ambiguous,
		stored: "placeholder",
		upstream: "v1",
		verdict: "ambiguous",
		check: "uncheckable:local",
	},
	Row {
		label: "disabled agent's differing copy ignored",
		local: Local::DisabledDiffers,
		stored: "v1",
		upstream: "v1",
		verdict: "upToDate",
		check: "upToDate",
	},
];

fn setup_local(project: &Path, local: Local) {
	let skills = |agent: &str| project.join(agent).join("skills/s");
	match local {
		Local::Readable => write_skill(&skills(".agents"), "s", "v1"),
		Local::Withheld => write_skill(&project.join(".aghub/s"), "s", "v1"),
		Local::WithheldEdited => {
			write_skill(&project.join(".aghub/s"), "s", "edited")
		}
		Local::Gone => {}
		Local::Ambiguous => {
			write_skill(&skills(".agents"), "s", "v1");
			write_skill(&skills(".cursor"), "s", "edited");
		}
		Local::DisabledDiffers => {
			write_skill(&skills(".agents"), "s", "v1");
			write_skill(&skills(".claude"), "s", "edited");
		}
	}
}

fn lock_with(stored: String) -> skill::lock::local::LocalSkillLockFile {
	let mut lock = skill::lock::local::LocalSkillLockFile::new();
	lock.skills.insert(
		"s".to_string(),
		skill::LocalSkillLockEntry {
			source: "owner/repo".to_string(),
			source_type: "github".to_string(),
			source_url: None,
			ref_name: Some("main".to_string()),
			skill_path: Some("SKILL.md".to_string()),
			computed_hash: stored,
			ref_commit: None,
		},
	);
	lock
}

fn verdict_kind(verdict: &Verdict) -> &'static str {
	match verdict {
		Verdict::UpToDate => "upToDate",
		Verdict::UpdateAvailable { .. } => "updateAvailable",
		Verdict::Uncheckable => "uncheckable",
		Verdict::Ambiguous => "ambiguous",
	}
}

fn status_kind(status: &SkillUpdateStatus) -> String {
	match status {
		SkillUpdateStatus::UpToDate => "upToDate".to_string(),
		SkillUpdateStatus::UpdateAvailable { .. } => {
			"updateAvailable".to_string()
		}
		SkillUpdateStatus::Uncheckable {
			reason: UncheckableReason::Local,
		} => "uncheckable:local".to_string(),
		other => format!("{other:?}"),
	}
}

fn run_row(rt: &tokio::runtime::Runtime, row: &Row) {
	let tmp = tempfile::tempdir().unwrap();
	let project = tmp.path().join("project");
	let upstream = tmp.path().join("upstream");
	std::fs::create_dir_all(&project).unwrap();
	setup_local(&project, row.local);
	write_skill(&upstream, "s", row.upstream);

	let v1 = tmp.path().join("v1");
	write_skill(&v1, "s", "v1");
	let v1_hash = skill::compute_skill_folder_hash(&v1).unwrap();
	let stored = if row.stored == "placeholder" {
		skill::EMPTY_SKILLS_LOCK_DIGEST.to_string()
	} else {
		v1_hash
	};

	let (entries, _) = skill_update::projection::project_lock_entries(
		false,
		Some(&project),
		|| lock_with(stored),
	);
	let (raw, comparison) =
		skill::compute_skill_folder_hashes(&upstream).unwrap();
	let verdict = skill_update::verdict::judge(&entries[0], &raw, &comparison);
	let out = rt.block_on(skill_update::run_update_check(
		entries,
		Arc::new(DirFetcher {
			root: upstream.clone(),
		}),
		Arc::new(NoTip),
		&NoToken,
		false,
	));

	assert_eq!(out.len(), 1, "one entry in, one result out: {}", row.label);
	assert_eq!(
		verdict_kind(&verdict),
		row.verdict,
		"verdict for row `{}` ({:?})",
		row.label,
		row.local
	);
	assert_eq!(
		status_kind(&out[0].status),
		row.check,
		"check for row `{}` ({:?})",
		row.label,
		row.local
	);
}

#[test]
fn verdict_table_matches_check_adapter() {
	let rt = tokio::runtime::Runtime::new().unwrap();
	for row in ROWS {
		with_isolated_env(|| run_row(&rt, row));
	}
}
