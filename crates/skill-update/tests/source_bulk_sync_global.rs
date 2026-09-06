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

fn project_entry(
	source: &str,
	source_type: &str,
	source_url: Option<&str>,
	skill_path: &str,
) -> skill::LocalSkillLockEntry {
	skill::LocalSkillLockEntry {
		source: source.to_string(),
		ref_name: Some("main".to_string()),
		source_type: source_type.to_string(),
		skill_path: Some(skill_path.to_string()),
		computed_hash: "hash".to_string(),
		ref_commit: None,
		source_url: source_url.map(str::to_string),
	}
}

/// Grouping keys on the repository ORIGIN, so two hosts serving the same
/// `owner/repo` are two Sources rows. A batch naming host A's row updates A's
/// entry and refuses B's. What makes this global-only: a global entry carries
/// `source` AND `sourceUrl` as separate fields, so the membership check reads a
/// different field than the fetch does — a project entry can collapse the two.
#[test]
fn global_source_row_covers_only_its_own_origin() {
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
				source_group: Some("https://github.com/owner/repo.git"),
				names: &names,
				scope: ResourceScope::GlobalOnly,
				project_root: None,
				force_unsafe: false,
			},
			&fetcher,
			&PerSourceToken,
		)
		.expect("a foreign-origin entry belongs in its own row");

		assert_eq!(results[0].name, "alpha");
		assert!(
			results[0].outcome.is_ok(),
			"alpha is this row's own entry: {:?}",
			results[0].outcome
		);
		assert!(
			matches!(
				results[1].outcome,
				Err(skill_update::mutation::LockedResyncError::SourceGroupMismatch)
			),
			"beta sits on another origin: {:?}",
			results[1].outcome
		);
		let seen = fetcher.seen.lock().unwrap();
		assert_eq!(seen.len(), 1, "only this row's origin may be fetched");
		assert_eq!(seen[0].0.source, "https://github.com/owner/repo.git");
		assert_eq!(
			seen[0].1.as_deref(),
			Some("token-for:https://github.com/owner/repo.git"),
			"the fetch must carry the token resolved for ITS OWN source"
		);
		drop(seen);

		let lock = skill::lock::global::read_skill_lock();
		assert!(std::fs::read_to_string(
			home.join(".claude/skills/alpha/SKILL.md")
		)
		.unwrap()
		.contains("new"));
		assert_eq!(
			lock.skills["alpha"].ref_commit.as_deref(),
			Some("global-commit")
		);
		assert!(std::fs::read_to_string(
			home.join(".claude/skills/beta/SKILL.md")
		)
		.unwrap()
		.contains("old"));
		assert!(lock.skills["beta"].ref_commit.is_none());
	});
}

/// The GROUPING, not just the membership predicate: two forges serving one
/// `owner/repo` must surface as TWO Sources rows, each advertising its own
/// `sourceUrl`. That is what makes a row's diff and its apply resolve to the
/// same repository — the host-blind key kept one row whose `sourceUrl` was
/// whichever entry landed first, so the other host's skills were judged against
/// a tree they do not come from.
#[test]
fn two_forges_serving_one_path_are_two_source_rows() {
	with_isolated_env(|_home| {
		let mut lock = skill::SkillLockFile::default();
		for (name, source_url) in [
			("alpha", "https://github.com/owner/repo.git"),
			("beta", "https://gitlab.com/owner/repo.git"),
			("gamma", "https://github.com/owner/repo.git"),
		] {
			lock.skills.insert(
				name.to_string(),
				global_entry(source_url, &format!("skills/{name}/SKILL.md")),
			);
		}
		skill::lock::global::write_skill_lock(&lock).unwrap();

		let rows = skill_update::sources::list_sources(
			skill_update::sources::SourceListInput {
				scopes: vec![skill_update::sources::SourceScope::Global],
			},
		);

		assert_eq!(
			rows.len(),
			2,
			"one row per origin, got: {:?}",
			rows.iter()
				.map(|row| (&row.source, &row.source_url, row.skill_count))
				.collect::<Vec<_>>()
		);
		let github = rows
			.iter()
			.find(|row| row.source_url.contains("github.com"))
			.expect("a github row");
		let gitlab = rows
			.iter()
			.find(|row| row.source_url.contains("gitlab.com"))
			.expect("a gitlab row");
		assert_eq!(github.skill_count, 2, "alpha + gamma");
		assert_eq!(gitlab.skill_count, 1, "beta");
		// The reported `source` is the lock's own host-blind identifier, so both
		// rows carry the SAME one — that is deliberate: it is what credential
		// bindings, the skill list's source groups and `source diff <x>` all
		// already match against, and unlike an origin it is a coordinate a caller
		// can feed back. Row UNIQUENESS lives in `source_url`.
		assert_eq!(
			github.source, gitlab.source,
			"the lock identifier is shared"
		);
		assert_ne!(
			github.source_url, gitlab.source_url,
			"a row must be uniquely identifiable — the desktop keys its list on \
			 this"
		);
	});
}

/// Project locks use the same origin grouping even though `sourceUrl` is
/// optional. The local entry is deliberately the same apparent repo path with
/// an explicit relative marker: it must never join either fetched repository.
#[test]
fn project_two_forges_serving_one_path_are_two_source_rows() {
	let project = tempfile::tempdir().unwrap();
	let mut lock = skill::LocalSkillLockFile::default();
	for (name, source_url) in [
		("alpha", "https://github.com/owner/repo.git"),
		("beta", "https://gitlab.com/owner/repo.git"),
		("gamma", "https://github.com/owner/repo.git"),
	] {
		lock.skills.insert(
			name.to_string(),
			project_entry(
				"owner/repo",
				if source_url.contains("gitlab.com") {
					"gitlab"
				} else {
					"github"
				},
				Some(source_url),
				&format!("skills/{name}/SKILL.md"),
			),
		);
	}
	lock.skills.insert(
		"local".to_string(),
		project_entry("./owner/repo", "local", None, "skills/local/SKILL.md"),
	);
	skill::lock::local::write_local_lock(&lock, Some(project.path())).unwrap();

	let rows = skill_update::sources::list_sources(
		skill_update::sources::SourceListInput {
			scopes: vec![skill_update::sources::SourceScope::Project {
				root: project.path().to_path_buf(),
			}],
		},
	);

	assert_eq!(
		rows.len(),
		3,
		"one row per remote origin plus local: {:?}",
		rows.iter()
			.map(|row| (&row.source_url, &row.source_type, row.skill_count))
			.collect::<Vec<_>>()
	);
	let github = rows
		.iter()
		.find(|row| row.source_url == "https://github.com/owner/repo.git")
		.expect("a github row");
	let gitlab = rows
		.iter()
		.find(|row| row.source_url == "https://gitlab.com/owner/repo.git")
		.expect("a gitlab row");
	let local = rows
		.iter()
		.find(|row| row.source_url == "./owner/repo")
		.expect("a local row");
	assert_eq!(github.skill_count, 2, "alpha + gamma");
	assert_eq!(gitlab.skill_count, 1, "beta");
	assert_eq!(local.skill_count, 1, "local");
	assert_eq!(github.source_type, "github");
	assert_eq!(gitlab.source_type, "gitlab");
	assert_eq!(local.source_type, "local");
}

/// A path with an explicit relative marker is not GitHub shorthand. Legacy or
/// hand-edited locks can contain one, and merging it into the remote row makes
/// that row's remote diff classify the local member as removed.
#[test]
fn relative_source_keeps_a_row_separate_from_github_shorthand() {
	with_isolated_env(|_home| {
		let mut lock = skill::SkillLockFile::default();
		lock.skills.insert(
			"local".to_string(),
			skill::SkillLockEntry {
				source: "./owner/repo".to_string(),
				source_type: "local".to_string(),
				source_url: "./owner/repo".to_string(),
				..global_entry("./owner/repo", "skills/local/SKILL.md")
			},
		);
		lock.skills.insert(
			"remote".to_string(),
			global_entry(
				"https://github.com/owner/repo.git",
				"skills/remote/SKILL.md",
			),
		);
		skill::lock::global::write_skill_lock(&lock).unwrap();

		let rows = skill_update::sources::list_sources(
			skill_update::sources::SourceListInput {
				scopes: vec![skill_update::sources::SourceScope::Global],
			},
		);

		assert_eq!(
			rows.len(),
			2,
			"relative and remote entries must not share a row: {:?}",
			rows.iter()
				.map(|row| (&row.source_url, row.skill_count))
				.collect::<Vec<_>>()
		);
		assert!(rows.iter().any(|row| {
			row.source_url == "./owner/repo"
				&& row.source_type == "local"
				&& row.skill_count == 1
		}));
		assert!(rows.iter().any(|row| {
			row.source_url == "https://github.com/owner/repo.git"
				&& row.source_type == "github"
				&& row.skill_count == 1
		}));
	});
}

/// npx and legacy project locks can omit `sourceUrl`, but still record the
/// provider. That type must keep a GitLab `group/repo` separate from the same
/// path on GitHub and reconstruct the matching fetch URL.
#[test]
fn source_type_selects_forge_when_project_source_url_is_missing() {
	let project = tempfile::tempdir().unwrap();
	let mut lock = skill::LocalSkillLockFile::default();
	lock.skills.insert(
		"github-skill".to_string(),
		project_entry("group/repo", "github", None, "skills/github/SKILL.md"),
	);
	lock.skills.insert(
		"gitlab-skill".to_string(),
		project_entry("group/repo", "gitlab", None, "skills/gitlab/SKILL.md"),
	);
	skill::lock::local::write_local_lock(&lock, Some(project.path())).unwrap();

	let rows = skill_update::sources::list_sources(
		skill_update::sources::SourceListInput {
			scopes: vec![skill_update::sources::SourceScope::Project {
				root: project.path().to_path_buf(),
			}],
		},
	);

	assert_eq!(
		rows.len(),
		2,
		"provider types must produce separate origins: {:?}",
		rows.iter()
			.map(|row| (&row.source_url, &row.source_type, row.skill_count))
			.collect::<Vec<_>>()
	);
	assert!(rows.iter().any(|row| {
		row.source_url == "https://github.com/group/repo.git"
			&& row.source_type == "github"
			&& row.skill_count == 1
	}));
	assert!(rows.iter().any(|row| {
		row.source_url == "https://gitlab.com/group/repo.git"
			&& row.source_type == "gitlab"
			&& row.skill_count == 1
	}));
}
