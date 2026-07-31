use std::path::{Path, PathBuf};

use aghub_core::models::ResourceScope;
use skill_update::mutation::{
	resync_locked_skills, LockedResyncError, LockedSkillsResyncError,
	LockedSkillsResyncRequest,
};
use skill_update::{
	FetchError, FetchSelection, Fetcher, SourceRef, TokenResolution,
	TokenResolver,
};

struct NoToken;

impl TokenResolver for NoToken {
	fn resolve(&self, _source: &str) -> TokenResolution {
		TokenResolution::NoToken
	}
}

struct PanicFetcher;

impl Fetcher for PanicFetcher {
	fn fetch(
		&self,
		_source_ref: &SourceRef,
		_token: Option<&str>,
		_selection: FetchSelection<'_>,
	) -> Result<skill_update::FetchedRepo, FetchError> {
		panic!("source-change preflight must run before fetch")
	}
}

struct FixtureFetcher {
	root: PathBuf,
	repoint_alpha_in: Option<PathBuf>,
}

impl Fetcher for FixtureFetcher {
	fn fetch(
		&self,
		_source_ref: &SourceRef,
		_token: Option<&str>,
		_selection: FetchSelection<'_>,
	) -> Result<skill_update::FetchedRepo, FetchError> {
		if let Some(project) = &self.repoint_alpha_in {
			skill::add_skill_to_local_lock(
				"alpha",
				local_entry("other/repo", "skills/alpha/SKILL.md"),
				Some(project),
			)
			.expect("the simulated concurrent repoint should succeed");
		}
		Ok(skill_update::FetchedRepo {
			root: self.root.clone(),
			snapshot: aghub_git::RepoSnapshot {
				commit_oid: "bulk-commit".to_string(),
				tree_oid: "bulk-tree".to_string(),
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

fn local_entry(source: &str, skill_path: &str) -> skill::LocalSkillLockEntry {
	skill::LocalSkillLockEntry {
		source_url: None,
		source: source.to_string(),
		ref_name: Some("main".to_string()),
		source_type: "github".to_string(),
		computed_hash: "old".to_string(),
		skill_path: Some(skill_path.to_string()),
		ref_commit: None,
	}
}

fn prepare_project(project: &Path) {
	for name in ["alpha", "beta"] {
		write_skill(
			&project.join(format!(".claude/skills/{name}")),
			name,
			"old",
		);
		skill::add_skill_to_local_lock(
			name,
			local_entry("owner/repo", &format!("skills/{name}/SKILL.md")),
			Some(project),
		)
		.unwrap();
	}
}

#[test]
fn changed_source_aborts_before_fetch_or_any_write() {
	let temporary = tempfile::tempdir().unwrap();
	let project = temporary.path().join("project");
	prepare_project(&project);
	let names = vec!["alpha".to_string(), "beta".to_string()];

	let error = resync_locked_skills(
		LockedSkillsResyncRequest {
			source: "other/repo",
			names: &names,
			scope: ResourceScope::ProjectOnly,
			project_root: Some(&project),
		},
		&PanicFetcher,
		&NoToken,
	)
	.expect_err("a request for a stale source must reject the whole batch");
	assert!(matches!(
		error,
		LockedSkillsResyncError::ItemPreflight {
			ref name,
			error: LockedResyncError::SourceChanged,
		} if name == "alpha"
	));
	for name in &names {
		let installed = std::fs::read_to_string(
			project.join(format!(".claude/skills/{name}/SKILL.md")),
		)
		.unwrap();
		assert!(installed.contains("old"), "{name} must remain unchanged");
	}
	let lock = skill::lock::local::read_local_lock(Some(&project));
	assert!(lock.skills.values().all(
		|entry| entry.source == "owner/repo" && entry.ref_commit.is_none()
	));
}
#[test]
fn missing_fetched_skill_aborts_before_any_installed_copy_changes() {
	let temporary = tempfile::tempdir().unwrap();
	let project = temporary.path().join("project");
	prepare_project(&project);
	let fetched = temporary.path().join("fetched");
	write_skill(&fetched.join("skills/alpha"), "alpha", "new");
	let names = vec!["alpha".to_string(), "beta".to_string()];

	let error = resync_locked_skills(
		LockedSkillsResyncRequest {
			source: "owner/repo",
			names: &names,
			scope: ResourceScope::ProjectOnly,
			project_root: Some(&project),
		},
		&FixtureFetcher {
			root: fetched,
			repoint_alpha_in: None,
		},
		&NoToken,
	)
	.expect_err("a missing selected skill must reject the whole batch");
	assert!(matches!(
		error,
		LockedSkillsResyncError::ItemPreflight {
			ref name,
			error: LockedResyncError::SourceSkillNotFound,
		} if name == "beta"
	));
	for name in &names {
		let installed = std::fs::read_to_string(
			project.join(format!(".claude/skills/{name}/SKILL.md")),
		)
		.unwrap();
		assert!(installed.contains("old"), "{name} must remain unchanged");
	}
	let lock = skill::lock::local::read_local_lock(Some(&project));
	assert!(lock.skills.values().all(|entry| entry.ref_commit.is_none()));
}

#[test]
fn stale_first_entry_does_not_prevent_later_runtime_attempts() {
	let temporary = tempfile::tempdir().unwrap();
	let project = temporary.path().join("project");
	prepare_project(&project);
	let fetched = temporary.path().join("fetched");
	write_skill(&fetched.join("skills/alpha"), "alpha", "new");
	write_skill(&fetched.join("skills/beta"), "beta", "new");
	let names = vec!["alpha".to_string(), "beta".to_string()];

	let results = resync_locked_skills(
		LockedSkillsResyncRequest {
			source: "owner/repo",
			names: &names,
			scope: ResourceScope::ProjectOnly,
			project_root: Some(&project),
		},
		&FixtureFetcher {
			root: fetched,
			repoint_alpha_in: Some(project.clone()),
		},
		&NoToken,
	)
	.expect("runtime failures belong to their ordered result rows");
	assert!(matches!(
		results[0].outcome,
		Err(LockedResyncError::Resync(
			aghub_core::skills::resync::ResyncError::StaleFetch(_)
		))
	));
	assert!(results[1].outcome.is_ok(), "beta must still be attempted");
	assert!(std::fs::read_to_string(
		project.join(".claude/skills/alpha/SKILL.md")
	)
	.unwrap()
	.contains("old"));
	assert!(std::fs::read_to_string(
		project.join(".claude/skills/beta/SKILL.md")
	)
	.unwrap()
	.contains("new"));
}
