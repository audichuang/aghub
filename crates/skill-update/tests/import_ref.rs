//! An import records the remote's REAL default-branch name, and the
//! next update check reads that recorded ref as the tip it compares against.
//! A guessed `main` on a repo whose default is `develop` would report a false
//! update, so this pins the whole import → lock → check round trip.
#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use aghub_git::{
	Blob, Credentials, RepoFetchBackend, RepoSnapshot, RepoTree,
	SourceRef as GitSourceRef,
};
use skill_update::{
	FetchSelection, GitFetcher, SkillRepository, SourceRef, TokenResolution,
	TokenResolver,
};

const DEVELOP: &str = "dddddddddddddddddddddddddddddddddddddddd";
const MAIN: &str = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";

/// Serves two branches: `develop` (HEAD unless a branch is asked for) and `main`.
struct TwoBranchGix {
	develop: PathBuf,
	main: PathBuf,
	default_branch: Option<&'static str>,
}

impl TwoBranchGix {
	fn oid_for(ref_: Option<&str>) -> &'static str {
		if ref_ == Some("main") {
			MAIN
		} else {
			DEVELOP
		}
	}
}

fn copy_dir(src: &Path, dst: &Path) {
	fs::create_dir_all(dst).unwrap();
	for e in fs::read_dir(src).unwrap() {
		let e = e.unwrap();
		let p = e.path();
		let d = dst.join(e.file_name());
		if p.is_dir() {
			copy_dir(&p, &d);
		} else {
			fs::copy(&p, &d).unwrap();
		}
	}
}

impl RepoFetchBackend for TwoBranchGix {
	fn resolve(
		&self,
		src: &GitSourceRef,
		_auth: Option<&Credentials>,
	) -> aghub_git::Result<RepoSnapshot> {
		let oid = Self::oid_for(src.ref_.as_deref());
		Ok(RepoSnapshot {
			commit_oid: oid.into(),
			tree_oid: oid.into(),
			commit_time: None,
		})
	}

	fn read_tree(&self, _s: &RepoSnapshot) -> aghub_git::Result<RepoTree> {
		Ok(RepoTree {
			entries: Vec::new(),
		})
	}

	fn read_blobs(
		&self,
		_s: &RepoSnapshot,
		_o: &[String],
	) -> aghub_git::Result<Vec<Blob>> {
		Ok(Vec::new())
	}

	fn materialize(
		&self,
		snap: &RepoSnapshot,
		paths: &[&str],
		dest: &Path,
	) -> aghub_git::Result<()> {
		let base = if snap.commit_oid == MAIN {
			&self.main
		} else {
			&self.develop
		};
		for p in paths {
			copy_dir(&base.join(p), &dest.join(p));
		}
		Ok(())
	}

	fn advertise_tip(
		&self,
		src: &GitSourceRef,
		_auth: Option<&Credentials>,
	) -> aghub_git::Result<Option<String>> {
		Ok(Some(Self::oid_for(src.ref_.as_deref()).to_string()))
	}

	fn list_branches(
		&self,
		_src: &GitSourceRef,
		_auth: Option<&Credentials>,
	) -> aghub_git::Result<Vec<String>> {
		Ok(vec!["develop".into(), "main".into()])
	}

	fn default_branch(
		&self,
		_src: &GitSourceRef,
		_auth: Option<&Credentials>,
	) -> aghub_git::Result<Option<String>> {
		Ok(self.default_branch.map(String::from))
	}
}

struct NoToken;

impl TokenResolver for NoToken {
	fn resolve(&self, _source: &str) -> TokenResolution {
		TokenResolution::NoToken
	}
}

#[tokio::test(flavor = "multi_thread")]
async fn import_records_the_real_default_branch_and_check_reports_no_false_update(
) {
	let home = tempfile::tempdir().unwrap();
	let data = tempfile::tempdir().unwrap();
	// Isolate every path the import and check may touch (crates/core/AGENTS.md).
	std::env::set_var("HOME", home.path());
	std::env::set_var("AGHUB_DATA_DIR", data.path());
	std::env::set_var("XDG_CONFIG_HOME", home.path().join(".config"));
	std::env::set_var("XDG_DATA_HOME", home.path().join(".local/share"));
	std::env::set_var("XDG_STATE_HOME", home.path().join(".local/state"));

	let skill_md =
		|body: &str| format!("---\nname: alpha\ndescription: d\n---\n{body}\n");
	let fixtures = tempfile::tempdir().unwrap();
	let develop_root = fixtures.path().join("develop");
	let main_root = fixtures.path().join("main");
	fs::create_dir_all(develop_root.join("alpha")).unwrap();
	fs::create_dir_all(main_root.join("alpha")).unwrap();
	fs::write(
		develop_root.join("alpha/SKILL.md"),
		skill_md("develop body"),
	)
	.unwrap();
	fs::write(main_root.join("alpha/SKILL.md"), skill_md("main body")).unwrap();

	let cases: [(Option<&'static str>, Option<&str>); 2] =
		[(Some("develop"), Some("develop")), (None, None)];
	for (default_branch, expected) in cases {
		let project = tempfile::tempdir().unwrap();
		let gix = Arc::new(TwoBranchGix {
			develop: develop_root.clone(),
			main: main_root.clone(),
			default_branch,
		});
		let repo = SkillRepository::with_backends(None, gix.clone());
		let claim = repo
			.resolve_pinned(
				&SourceRef {
					source: "https://github.com/owner/repo.git".into(),
					ref_: None,
				},
				None,
			)
			.unwrap();

		// Decide exactly as every import surface does.
		let scope = aghub_core::WriteScope::project(project.path());
		let import_ref = skill_update::sources::import_ref(
			None,
			&skill_update::sources::recorded_refs(
				&scope,
				"https://github.com/owner/repo.git",
			),
			|| repo.import_ref(&claim),
		);
		let fetched = repo
			.fetch_pinned(
				&claim,
				FetchSelection::Skills(&[
					skill::SkillPath::parse("alpha").unwrap()
				]),
			)
			.unwrap();
		skill_update::mutation::install_fetched_source(
			&skill_update::mutation::FetchedSource::from_repo(fetched),
			skill_update::mutation::FetchedInstallRequest {
				source: &skill::InstallLockSource {
					source: "owner/repo".into(),
					source_type: "github".into(),
					source_url: "https://github.com/owner/repo.git".into(),
					ref_name: import_ref,
				},
				lock_skill_path: "alpha/SKILL.md",
				expected_name: None,
				scope,
				target_agents: &[aghub_core::models::AgentType::Claude],
				expected_ref: None,
			},
		)
		.unwrap();

		let lock = skill::lock::local::read_local_lock(Some(project.path()));
		assert_eq!(
			lock.skills["alpha"].ref_name.as_deref(),
			expected,
			"import ref for default branch {default_branch:?}"
		);

		// The check reads the recorded ref back and must find the tip it imported.
		let (entries, _) = skill_update::projection::project_lock_entries(
			false,
			Some(project.path()),
			|| skill::lock::local::read_local_lock(Some(project.path())),
		);
		let fetcher = GitFetcher::with_repository(
			SkillRepository::with_backends(None, gix.clone()),
		);
		let ref_resolver = Arc::new(fetcher.ref_resolver());
		let out = skill_update::run_update_check(
			entries,
			Arc::new(fetcher),
			ref_resolver,
			&NoToken,
			false,
		)
		.await;
		assert_eq!(out.len(), 1);
		assert_eq!(
			out[0].status,
			aghub_core::skills::update::SkillUpdateStatus::UpToDate,
			"recorded ref must name the imported tip — `main` here is a false update"
		);
	}
}
