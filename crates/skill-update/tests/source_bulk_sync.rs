use std::path::Path;
use std::sync::Mutex;

use aghub_core::models::ResourceScope;
use skill_update::mutation::{resync_locked_skills, LockedSkillsResyncRequest};
use skill_update::{
	FetchError, FetchSelection, FetchedRepo, Fetcher, SourceRef,
	TokenResolution, TokenResolver,
};

struct Token;

impl TokenResolver for Token {
	fn resolve(&self, _source: &str) -> TokenResolution {
		TokenResolution::Token("secret".to_string())
	}
}

/// One token per source, so a fetch carrying another group's token is visible.
struct PerSourceToken;

impl TokenResolver for PerSourceToken {
	fn resolve(&self, source: &str) -> TokenResolution {
		TokenResolution::Token(format!("token-for:{source}"))
	}
}

type FetchCall = (SourceRef, Option<String>, Vec<String>);

struct RecordingFetcher {
	root: std::path::PathBuf,
	seen: Mutex<Vec<FetchCall>>,
}

impl Fetcher for RecordingFetcher {
	fn fetch(
		&self,
		source_ref: &SourceRef,
		token: Option<&str>,
		selection: FetchSelection<'_>,
	) -> Result<FetchedRepo, FetchError> {
		let FetchSelection::Skills(paths) = selection else {
			panic!("multi resync must selectively fetch skill folders");
		};
		self.seen.lock().unwrap().push((
			source_ref.clone(),
			token.map(str::to_string),
			paths.iter().map(|path| path.as_str().to_string()).collect(),
		));
		Ok(FetchedRepo {
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

#[test]
fn locked_multi_resync_fetches_one_source_and_updates_every_skill() {
	let temporary = tempfile::tempdir().unwrap();
	let project = temporary.path().join("project");
	let fetched_root = temporary.path().join("fetched");
	let names = vec!["alpha".to_string(), "beta".to_string()];

	for name in &names {
		write_skill(&project.join(".claude/skills").join(name), name, "old");
		write_skill(&fetched_root.join("skills").join(name), name, "new");
		skill::add_skill_to_local_lock(
			name,
			skill::LocalSkillLockEntry {
				source_url: Some(
					"https://git.example/owner/repo.git".to_string(),
				),
				source: "owner/repo".to_string(),
				ref_name: Some("main".to_string()),
				source_type: "git".to_string(),
				computed_hash: "old".to_string(),
				skill_path: Some(format!("skills/{name}/SKILL.md")),
				ref_commit: None,
			},
			Some(&project),
		)
		.unwrap();
	}

	let fetcher = RecordingFetcher {
		root: fetched_root,
		seen: Mutex::new(Vec::new()),
	};
	let results = resync_locked_skills(
		LockedSkillsResyncRequest {
			source_group: Some("https://git.example/owner/repo.git"),
			names: &names,
			scope: ResourceScope::ProjectOnly,
			project_root: Some(&project),
		},
		&fetcher,
		&Token,
	)
	.expect("shared-Source preflight and fetch should succeed");

	let seen = fetcher.seen.lock().unwrap();
	assert_eq!(seen.len(), 1, "one Source must be fetched exactly once");
	assert_eq!(seen[0].0.source, "https://git.example/owner/repo.git");
	assert_eq!(seen[0].0.ref_.as_deref(), Some("main"));
	assert_eq!(seen[0].1.as_deref(), Some("secret"));
	assert_eq!(
		seen[0].2,
		vec!["skills/alpha".to_string(), "skills/beta".to_string()]
	);
	drop(seen);

	assert_eq!(
		results
			.iter()
			.map(|result| result.name.as_str())
			.collect::<Vec<_>>(),
		vec!["alpha", "beta"],
		"results must preserve request order",
	);
	assert!(
		results.iter().all(|result| result.outcome.is_ok()),
		"every runtime resync should be attempted and succeed",
	);

	let lock = skill::lock::local::read_local_lock(Some(&project));
	for name in &names {
		let installed = project.join(".claude/skills").join(name);
		assert!(
			std::fs::read_to_string(installed.join("SKILL.md"))
				.unwrap()
				.contains("new"),
			"{name} installed content must be updated",
		);
		assert_eq!(
			lock.skills[name].ref_commit.as_deref(),
			Some("bulk-commit"),
			"{name} Lock entry must record the fetched commit",
		);
	}
}
#[test]
fn locked_multi_resync_fetches_each_source_ref_once_and_preserves_order() {
	let temporary = tempfile::tempdir().unwrap();
	let project = temporary.path().join("project");
	let fetched_root = temporary.path().join("fetched");
	let names = vec!["beta".to_string(), "alpha".to_string()];

	for (name, ref_name) in [("alpha", "main"), ("beta", "release")] {
		write_skill(&project.join(".claude/skills").join(name), name, "old");
		write_skill(&fetched_root.join("skills").join(name), name, "new");
		skill::add_skill_to_local_lock(
			name,
			skill::LocalSkillLockEntry {
				source_url: Some(
					"https://git.example/owner/repo.git".to_string(),
				),
				source: "owner/repo".to_string(),
				ref_name: Some(ref_name.to_string()),
				source_type: "git".to_string(),
				computed_hash: "old".to_string(),
				skill_path: Some(format!("skills/{name}/SKILL.md")),
				ref_commit: None,
			},
			Some(&project),
		)
		.unwrap();
	}

	let fetcher = RecordingFetcher {
		root: fetched_root,
		seen: Mutex::new(Vec::new()),
	};
	let results = resync_locked_skills(
		LockedSkillsResyncRequest {
			source_group: Some("https://git.example/owner/repo.git"),
			names: &names,
			scope: ResourceScope::ProjectOnly,
			project_root: Some(&project),
		},
		&fetcher,
		&Token,
	)
	.expect("each source/ref group should fetch before any writes");

	let seen = fetcher.seen.lock().unwrap();
	assert_eq!(seen.len(), 2);
	assert_eq!(seen[0].0.ref_.as_deref(), Some("release"));
	assert_eq!(seen[0].2, vec!["skills/beta".to_string()]);
	assert_eq!(seen[1].0.ref_.as_deref(), Some("main"));
	assert_eq!(seen[1].2, vec!["skills/alpha".to_string()]);
	drop(seen);
	assert_eq!(
		results
			.iter()
			.map(|result| result.name.as_str())
			.collect::<Vec<_>>(),
		vec!["beta", "alpha"],
	);
	assert!(results.iter().all(|result| result.outcome.is_ok()));
}

fn locked(project: &Path, name: &str, source: &str, source_url: Option<&str>) {
	skill::add_skill_to_local_lock(
		name,
		skill::LocalSkillLockEntry {
			source_url: source_url.map(str::to_string),
			source: source.to_string(),
			ref_name: Some("main".to_string()),
			source_type: "git".to_string(),
			computed_hash: "old".to_string(),
			skill_path: Some(format!("skills/{name}/SKILL.md")),
			ref_commit: None,
		},
		Some(project),
	)
	.unwrap();
}

/// Two hosts serving the same `owner/repo` are ONE Sources row (grouping keys on
/// the lock's host-blind `source`), so "Update all" sends both names under one
/// `source`. Each must still be fetched from ITS OWN coordinate, with ITS OWN
/// token — and neither may be rejected, which is what a stricter assertion here
/// used to do, unfixably.
#[test]
fn one_source_row_spanning_two_hosts_updates_each_from_its_own_host() {
	let temporary = tempfile::tempdir().unwrap();
	let project = temporary.path().join("project");
	let fetched_root = temporary.path().join("fetched");
	for name in ["alpha", "beta"] {
		write_skill(&project.join(".claude/skills").join(name), name, "old");
		write_skill(&fetched_root.join("skills").join(name), name, "new");
	}
	locked(
		&project,
		"alpha",
		"owner/repo",
		Some("https://github.com/owner/repo.git"),
	);
	locked(
		&project,
		"beta",
		"owner/repo",
		Some("https://gitlab.com/owner/repo.git"),
	);
	let names = vec!["alpha".to_string(), "beta".to_string()];

	let fetcher = RecordingFetcher {
		root: fetched_root,
		seen: Mutex::new(Vec::new()),
	};
	let results = resync_locked_skills(
		LockedSkillsResyncRequest {
			source_group: Some("https://github.com/owner/repo.git"),
			names: &names,
			scope: ResourceScope::ProjectOnly,
			project_root: Some(&project),
		},
		&fetcher,
		&PerSourceToken,
	)
	.expect("both rows belong to the Source row the caller was shown");

	assert!(
		results.iter().all(|result| result.outcome.is_ok()),
		"a Source row's own members must all be updatable: {:?}",
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
}

/// npx-written project locks carry no `sourceUrl`, so the entry's effective
/// source is the `owner/repo` shorthand while the Sources row advertises a
/// reconstructed clone URL — the desktop sends the latter. The assertion must
/// normalize across that, or project-scope "Update all" fails every row.
#[test]
fn reconstructed_source_url_matches_a_shorthand_only_entry() {
	let temporary = tempfile::tempdir().unwrap();
	let project = temporary.path().join("project");
	let fetched_root = temporary.path().join("fetched");
	write_skill(&project.join(".claude/skills/alpha"), "alpha", "old");
	write_skill(&fetched_root.join("skills/alpha"), "alpha", "new");
	locked(&project, "alpha", "owner/repo", None);
	let names = vec!["alpha".to_string()];

	let fetcher = RecordingFetcher {
		root: fetched_root,
		seen: Mutex::new(Vec::new()),
	};
	let results = resync_locked_skills(
		LockedSkillsResyncRequest {
			source_group: Some("https://github.com/owner/repo.git"),
			names: &names,
			scope: ResourceScope::ProjectOnly,
			project_root: Some(&project),
		},
		&fetcher,
		&Token,
	)
	.expect("a shorthand-only entry belongs to its reconstructed Source row");
	assert!(
		results[0].outcome.is_ok(),
		"npx-written entry must be updatable: {:?}",
		results[0].outcome
	);
	assert_eq!(
		fetcher.seen.lock().unwrap()[0].0.source,
		"owner/repo",
		"the fetch still uses the entry's own effective coordinate"
	);
}

/// A TFS/Azure-DevOps entry's `source` has more than two path segments, so it is
/// not GitHub shorthand and does not resolve as a remote on its own — the
/// pre-a3235178 assertion rejected it outright. A caller passing the `source`
/// field `GET /sources` reported must still match.
///
/// Honest about its reach: this matches through the `entry_source == want` arm,
/// not TFS-specific normalization (the desktop sends `row.sourceUrl`, which
/// would match through the `entry_source_url == Some(want)` arm instead). It is
/// still a real regression test — it goes red both when the assertion compares
/// the fetch coordinate instead of the grouping identifier, and when the strict
/// host comparison comes back — but do not read the name as "TFS URLs are
/// normalized here".
#[test]
fn tfs_style_source_identifier_matches_its_own_entry() {
	let temporary = tempfile::tempdir().unwrap();
	let project = temporary.path().join("project");
	let fetched_root = temporary.path().join("fetched");
	write_skill(&project.join(".claude/skills/alpha"), "alpha", "old");
	write_skill(&fetched_root.join("skills/alpha"), "alpha", "new");
	locked(
		&project,
		"alpha",
		"Coll/_git/repo",
		Some("https://tfs.example.com/tfs/Coll/_git/repo"),
	);
	let names = vec!["alpha".to_string()];

	let results = resync_locked_skills(
		LockedSkillsResyncRequest {
			source_group: Some("Coll/_git/repo"),
			names: &names,
			scope: ResourceScope::ProjectOnly,
			project_root: Some(&project),
		},
		&RecordingFetcher {
			root: fetched_root,
			seen: Mutex::new(Vec::new()),
		},
		&Token,
	)
	.expect("a TFS Source row's own member must be updatable");
	assert!(
		results[0].outcome.is_ok(),
		"TFS entry must not be rejected as a changed source: {:?}",
		results[0].outcome
	);
}
