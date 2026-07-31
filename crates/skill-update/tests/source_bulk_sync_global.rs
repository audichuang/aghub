//! Global-scope bulk resync. Its own test binary + a module-local env lock,
//! because the global lock lives under `XDG_STATE_HOME` and the installed copies
//! under `HOME` — isolating those is process-wide state that must never race
//! another binary's env-touching tests (see crates/core/AGENTS.md Testing).
//!
//! The project-scope suites cover ordering, grouping and the failure policy.
//! What ONLY exists here is the global lock's own shape: it carries `source` AND
//! `sourceUrl` as separate fields, which is exactly where the Source assertion
//! has to read the GROUPING identifier while the fetch uses the entry's own
//! coordinate. A project entry can collapse the two, so project tests cannot
//! tell the difference.

#![cfg(unix)]

use std::path::Path;
use std::sync::Mutex;

use aghub_core::models::ResourceScope;
use skill_update::mutation::{resync_locked_skills, LockedSkillsResyncRequest};
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

/// HOME + XDG_STATE_HOME in fresh tempdirs, serialized against this binary's
/// other tests. RAII restoration, so a panic cannot leak a deleted tempdir path
/// into a later test.
fn with_isolated_env<T>(f: impl FnOnce(&Path) -> T) -> T {
	let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
	let home = tempfile::tempdir().unwrap();
	let state = tempfile::tempdir().unwrap();
	// Declared AFTER the tempdirs so they drop FIRST: the env is restored to the
	// real values before these directories are deleted.
	let _home_guard = EnvVarGuard::set("HOME", home.path());
	let _state_guard = EnvVarGuard::set("XDG_STATE_HOME", state.path());
	f(home.path())
}

struct PerSourceToken;

impl TokenResolver for PerSourceToken {
	fn resolve(&self, source: &str) -> TokenResolution {
		TokenResolution::Token(format!("token-for:{source}"))
	}
}

struct RecordingFetcher {
	root: std::path::PathBuf,
	seen: Mutex<Vec<(SourceRef, Option<String>)>>,
}

impl Fetcher for RecordingFetcher {
	fn fetch(
		&self,
		source_ref: &SourceRef,
		token: Option<&str>,
		_selection: FetchSelection<'_>,
	) -> Result<FetchedRepo, FetchError> {
		self.seen
			.lock()
			.unwrap()
			.push((source_ref.clone(), token.map(str::to_string)));
		Ok(FetchedRepo {
			root: self.root.clone(),
			snapshot: aghub_git::RepoSnapshot {
				commit_oid: "global-commit".to_string(),
				tree_oid: "global-tree".to_string(),
				commit_time: None,
			},
			_guard: None,
		})
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

fn global_entry(source_url: &str, skill_path: &str) -> skill::SkillLockEntry {
	skill::SkillLockEntry {
		source: "owner/repo".to_string(),
		source_type: "github".to_string(),
		source_url: source_url.to_string(),
		ref_name: Some("main".to_string()),
		skill_path: Some(skill_path.to_string()),
		skill_folder_hash: String::new(),
		content_hash: None,
		ref_commit: None,
		installed_at: "t".to_string(),
		updated_at: "t".to_string(),
		plugin_name: None,
	}
}

/// Two global entries share the host-blind `source` the Sources view groups by,
/// but sit on different hosts. The desktop sends ONE of the two `sourceUrl`s, so
/// asserting against the entry's own coordinate would reject the other row
/// forever. Each row must still fetch from its OWN host, with its OWN token.
#[test]
fn global_source_row_spanning_two_hosts_updates_each_from_its_own_host() {
	with_isolated_env(|home| {
		let fetched = tempfile::tempdir().unwrap();
		let mut lock = skill::SkillLockFile::default();
		for (name, source_url) in [
			("alpha", "https://github.com/owner/repo.git"),
			("beta", "https://gitlab.com/owner/repo.git"),
		] {
			write_skill(
				&home.join(format!(".claude/skills/{name}")),
				name,
				"old",
			);
			write_skill(
				&fetched.path().join(format!("skills/{name}")),
				name,
				"new",
			);
			lock.skills.insert(
				name.to_string(),
				global_entry(source_url, &format!("skills/{name}/SKILL.md")),
			);
		}
		skill::lock::global::write_skill_lock(&lock).unwrap();

		let fetcher = RecordingFetcher {
			root: fetched.path().to_path_buf(),
			seen: Mutex::new(Vec::new()),
		};
		let names = vec!["alpha".to_string(), "beta".to_string()];
		let results = resync_locked_skills(
			LockedSkillsResyncRequest {
				source: Some("https://github.com/owner/repo.git"),
				names: &names,
				scope: ResourceScope::GlobalOnly,
				project_root: None,
			},
			&fetcher,
			&PerSourceToken,
		)
		.expect("both rows belong to the Source row the caller was shown");

		assert!(
			results.iter().all(|result| result.outcome.is_ok()),
			"a global Source row's own members must all be updatable: {:?}",
			results
				.iter()
				.map(|result| (&result.name, result.outcome.is_ok()))
				.collect::<Vec<_>>()
		);
		let seen = fetcher.seen.lock().unwrap();
		assert_eq!(seen.len(), 2, "distinct coordinates fetch separately");
		assert_eq!(seen[0].0.source, "https://github.com/owner/repo.git");
		assert_eq!(
			seen[0].1.as_deref(),
			Some("token-for:https://github.com/owner/repo.git"),
			"each group must carry the token resolved for ITS OWN source"
		);
		assert_eq!(seen[1].0.source, "https://gitlab.com/owner/repo.git");
		assert_eq!(
			seen[1].1.as_deref(),
			Some("token-for:https://gitlab.com/owner/repo.git")
		);
		drop(seen);

		let lock = skill::lock::global::read_skill_lock();
		for name in ["alpha", "beta"] {
			assert!(std::fs::read_to_string(
				home.join(format!(".claude/skills/{name}/SKILL.md"))
			)
			.unwrap()
			.contains("new"));
			assert_eq!(
				lock.skills[name].ref_commit.as_deref(),
				Some("global-commit"),
				"{name} lock must record the fetched commit"
			);
		}
	});
}
