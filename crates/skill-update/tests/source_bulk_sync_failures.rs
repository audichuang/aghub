use std::path::{Path, PathBuf};

use aghub_core::models::ResourceScope;
use skill_update::mutation::{
	resync_locked_skills, LockedResyncError, LockedSkillsResyncRequest,
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

/// Fails one ref's fetch, and asserts on EVERY fetch that no installed copy has
/// been swapped yet — the batch must finish fetching before it starts writing.
struct GroupFetcher {
	root: PathBuf,
	failing_ref: &'static str,
	installed: Vec<PathBuf>,
}

impl Fetcher for GroupFetcher {
	fn fetch(
		&self,
		source_ref: &SourceRef,
		_token: Option<&str>,
		_selection: FetchSelection<'_>,
	) -> Result<skill_update::FetchedRepo, FetchError> {
		for path in &self.installed {
			let installed = std::fs::read_to_string(path.join("SKILL.md"))
				.expect("installed copy should still exist");
			assert!(
				installed.contains("old"),
				"{} was swapped while a group was still fetching",
				path.display()
			);
		}
		if source_ref.ref_.as_deref() == Some(self.failing_ref) {
			return Err(FetchError::Network);
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
fn changed_source_fails_its_rows_without_fetching_anything() {
	let temporary = tempfile::tempdir().unwrap();
	let project = temporary.path().join("project");
	prepare_project(&project);
	let names = vec!["alpha".to_string(), "beta".to_string()];

	// PanicFetcher proves the stale-Source rejection happens before any fetch:
	// with no resolvable row there is no group left to fetch at all.
	let results = resync_locked_skills(
		LockedSkillsResyncRequest {
			source_group: Some("other/repo"),
			names: &names,
			scope: ResourceScope::ProjectOnly,
			project_root: Some(&project),
		},
		&PanicFetcher,
		&NoToken,
	)
	.expect("a stale Source view belongs in the per-skill rows");
	assert_eq!(results.len(), 2);
	for (row, name) in results.iter().zip(&names) {
		assert_eq!(&row.name, name);
		assert!(matches!(
			row.outcome,
			Err(LockedResyncError::SourceGroupMismatch),
		));
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
fn missing_fetched_skill_fails_only_its_own_row() {
	let temporary = tempfile::tempdir().unwrap();
	let project = temporary.path().join("project");
	prepare_project(&project);
	let fetched = temporary.path().join("fetched");
	write_skill(&fetched.join("skills/alpha"), "alpha", "new");
	let names = vec!["alpha".to_string(), "beta".to_string()];

	let results = resync_locked_skills(
		LockedSkillsResyncRequest {
			source_group: Some("owner/repo"),
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
	.expect("a skill missing upstream must not cost the others their update");
	assert!(results[0].outcome.is_ok(), "alpha is present upstream");
	assert!(matches!(
		results[1].outcome,
		Err(LockedResyncError::SourceSkillNotFound),
	));
	assert!(std::fs::read_to_string(
		project.join(".claude/skills/alpha/SKILL.md")
	)
	.unwrap()
	.contains("new"));
	assert!(std::fs::read_to_string(
		project.join(".claude/skills/beta/SKILL.md")
	)
	.unwrap()
	.contains("old"));
	let lock = skill::lock::local::read_local_lock(Some(&project));
	assert_eq!(
		lock.skills["alpha"].ref_commit.as_deref(),
		Some("bulk-commit")
	);
	assert!(lock.skills["beta"].ref_commit.is_none());
}

#[test]
fn unresolvable_entry_fails_only_its_own_row() {
	let temporary = tempfile::tempdir().unwrap();
	let project = temporary.path().join("project");
	prepare_project(&project);
	let fetched = temporary.path().join("fetched");
	write_skill(&fetched.join("skills/alpha"), "alpha", "new");
	// "ghost" is requested but was never locked — the classic stale Sources
	// view. It must not cost alpha its update.
	let names = vec!["ghost".to_string(), "alpha".to_string()];

	let results = resync_locked_skills(
		LockedSkillsResyncRequest {
			source_group: Some("owner/repo"),
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
	.expect("an unresolvable entry belongs in its own row");
	assert_eq!(results[0].name, "ghost");
	assert!(matches!(
		results[0].outcome,
		Err(LockedResyncError::LockEntryNotFound {
			scope: ResourceScope::ProjectOnly,
		}),
	));
	assert!(results[1].outcome.is_ok(), "alpha must still be updated");
	assert!(std::fs::read_to_string(
		project.join(".claude/skills/alpha/SKILL.md")
	)
	.unwrap()
	.contains("new"));
}

#[test]
fn one_groups_fetch_failure_leaves_the_other_group_updatable() {
	let temporary = tempfile::tempdir().unwrap();
	let project = temporary.path().join("project");
	let fetched = temporary.path().join("fetched");
	let mut installed = Vec::new();
	for (name, ref_name) in [("alpha", "main"), ("beta", "release")] {
		let directory = project.join(format!(".claude/skills/{name}"));
		write_skill(&directory, name, "old");
		write_skill(&fetched.join(format!("skills/{name}")), name, "new");
		installed.push(directory);
		let mut entry =
			local_entry("owner/repo", &format!("skills/{name}/SKILL.md"));
		entry.ref_name = Some(ref_name.to_string());
		skill::add_skill_to_local_lock(name, entry, Some(&project)).unwrap();
	}
	let names = vec!["beta".to_string(), "alpha".to_string()];

	let results = resync_locked_skills(
		LockedSkillsResyncRequest {
			source_group: Some("owner/repo"),
			names: &names,
			scope: ResourceScope::ProjectOnly,
			project_root: Some(&project),
		},
		// `main` is the SECOND group fetched, so a regression that fetched
		// lazily per row would have swapped beta before this failure and the
		// fetcher's own assertion would catch it.
		&GroupFetcher {
			root: fetched,
			failing_ref: "main",
			installed,
		},
		&NoToken,
	)
	.expect("one group's network failure belongs to that group's rows");
	assert_eq!(results[0].name, "beta");
	assert!(
		results[0].outcome.is_ok(),
		"beta's own group fetched fine and must still be updated"
	);
	assert!(matches!(
		results[1].outcome,
		Err(LockedResyncError::Fetch(FetchError::Network)),
	));
	assert!(std::fs::read_to_string(
		project.join(".claude/skills/beta/SKILL.md")
	)
	.unwrap()
	.contains("new"));
	assert!(std::fs::read_to_string(
		project.join(".claude/skills/alpha/SKILL.md")
	)
	.unwrap()
	.contains("old"));
}

#[test]
fn repeated_name_is_attempted_once() {
	let temporary = tempfile::tempdir().unwrap();
	let project = temporary.path().join("project");
	prepare_project(&project);
	let fetched = temporary.path().join("fetched");
	write_skill(&fetched.join("skills/alpha"), "alpha", "new");
	let names = vec!["alpha".to_string(), "alpha".to_string()];

	let results = resync_locked_skills(
		LockedSkillsResyncRequest {
			source_group: Some("owner/repo"),
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
	.expect("a duplicated name must not manufacture a stale second attempt");
	assert_eq!(results.len(), 1, "one row per unique name");
	assert!(results[0].outcome.is_ok());
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
			source_group: Some("owner/repo"),
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

/// A genuinely repointed entry must fail ALONE. `changed_source_...` above
/// mismatches every row, so on its own it only proves SourceChanged is a row
/// rather than a whole-request error — not that it spares its siblings.
#[test]
fn repointed_entry_fails_without_costing_its_sibling() {
	let temporary = tempfile::tempdir().unwrap();
	let project = temporary.path().join("project");
	prepare_project(&project);
	// alpha now belongs to a different repository than the caller's Source.
	skill::add_skill_to_local_lock(
		"alpha",
		local_entry("elsewhere/repo", "skills/alpha/SKILL.md"),
		Some(&project),
	)
	.unwrap();
	let fetched = temporary.path().join("fetched");
	write_skill(&fetched.join("skills/alpha"), "alpha", "new");
	write_skill(&fetched.join("skills/beta"), "beta", "new");
	let names = vec!["alpha".to_string(), "beta".to_string()];

	let results = resync_locked_skills(
		LockedSkillsResyncRequest {
			source_group: Some("owner/repo"),
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
	.expect("one repointed entry must not fail the request");
	assert!(matches!(
		results[0].outcome,
		Err(LockedResyncError::SourceGroupMismatch),
	));
	assert!(results[1].outcome.is_ok(), "beta must still be updated");
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
	let lock = skill::lock::local::read_local_lock(Some(&project));
	assert!(lock.skills["alpha"].ref_commit.is_none());
	assert_eq!(
		lock.skills["beta"].ref_commit.as_deref(),
		Some("bulk-commit")
	);
}

/// A name that is locked but has nothing installed must fail WITHOUT costing a
/// remote round trip. The transaction re-checks this authoritatively under the
/// lock, so dropping the advisory check loses no error — it just moves the
/// failure to after the fetch, which is what `PanicFetcher` catches here. A
/// stale Sources view is the ordinary way to reach this.
#[test]
fn a_locked_but_uninstalled_name_fails_before_any_fetch() {
	let temporary = tempfile::tempdir().unwrap();
	let project = temporary.path().join("project");
	prepare_project(&project);
	// beta is locked and installed; alpha is locked with its install removed.
	std::fs::remove_dir_all(project.join(".claude/skills/alpha")).unwrap();
	let names = vec!["alpha".to_string()];

	let results = resync_locked_skills(
		LockedSkillsResyncRequest {
			source_group: Some("owner/repo"),
			names: &names,
			scope: ResourceScope::ProjectOnly,
			project_root: Some(&project),
		},
		&PanicFetcher,
		&NoToken,
	)
	.expect("an uninstalled entry belongs in its own row");
	assert!(matches!(
		results[0].outcome,
		Err(LockedResyncError::NotInstalled),
	));
	let lock = skill::lock::local::read_local_lock(Some(&project));
	assert!(lock.skills["alpha"].ref_commit.is_none());
}
