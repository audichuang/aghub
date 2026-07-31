use std::path::Path;

use aghub_core::models::{AgentType, ResourceScope};

use crate::{
	skill_folder_from_lock_path, FetchError, FetchSelection, FetchedRepo,
	Fetcher, SourceRef, TokenResolution, TokenResolver,
};

pub struct FetchedSourceRequest<'a> {
	pub source: &'a str,
	pub ref_name: Option<&'a str>,
	pub skill_path: &'a str,
}

#[derive(Debug)]
pub enum FetchMutationError {
	CredentialBackendUnavailable,
	InvalidSkillPath,
	Fetch(FetchError),
}

pub struct FetchedRenameRequest<'a> {
	pub source: &'a aghub_core::skills::rename::RenameLockSource,
	pub new_name: &'a str,
}

#[derive(Debug)]
pub enum FetchRenameError {
	CredentialBackendUnavailable,
	Fetch(FetchError),
	CatalogScan,
	SkillNotFound,
}

pub struct PreparedRename {
	pub fetched: FetchedSource,
	pub source: aghub_core::skills::rename::RenameLockSource,
}

/// A selectively materialized source tree and its immutable commit identity.
/// Keeping the owning [`FetchedRepo`] intact ensures its temporary-directory
/// guard outlives every root/OID consumer.
pub struct FetchedSource {
	repo: FetchedRepo,
}

impl FetchedSource {
	pub fn from_repo(repo: FetchedRepo) -> Self {
		Self { repo }
	}

	fn root(&self) -> &Path {
		&self.repo.root
	}

	fn oid(&self) -> &str {
		self.repo.oid()
	}
}

/// Whether a lock-form skill path resolves to an existing `SKILL.md` inside
/// this fetched tree. The source root remains encapsulated by
/// [`FetchedSource`].
pub fn fetched_skill_path_exists(
	fetched: &FetchedSource,
	lock_skill_path: &str,
) -> bool {
	aghub_core::skills::update::sanitize_skill_path(
		fetched.root(),
		lock_skill_path,
	)
	.is_some()
}

/// Run the existing core rename transaction against one commit-pinned fetched
/// source without exposing its root or commit identity to the adapter.
pub fn accept_fetched_rename(
	fetched: &FetchedSource,
	request: aghub_core::skills::rename::RenameRequest<'_>,
	source: &aghub_core::skills::rename::RenameLockSource,
) -> Result<
	aghub_core::skills::rename::RenameSuccess,
	aghub_core::skills::rename::RenameError,
> {
	aghub_core::skills::rename::accept_rename(
		request,
		aghub_core::skills::rename::FetchedRename {
			repo_root: fetched.root(),
			oid: fetched.oid(),
			source,
		},
	)
}

pub struct FetchedInstallRequest<'a> {
	pub source: &'a skill::InstallLockSource,
	pub lock_skill_path: &'a str,
	pub expected_name: Option<&'a str>,
	pub scope: ResourceScope,
	pub project_root: Option<&'a Path>,
	pub target_agents: &'a [AgentType],
}

#[derive(Debug)]
pub enum InstallMutationError {
	InvalidSkillPath,
	Install(aghub_core::ConfigError),
}

/// Install one skill from a commit-pinned [`FetchedSource`]. Source-tree
/// containment, commit identity, link style, and the core install request are
/// derived here so callers cannot accidentally mix coordinates from different
/// fetches or construct a lock with the wrong OID.
pub fn install_fetched_source(
	fetched: &FetchedSource,
	request: FetchedInstallRequest<'_>,
) -> Result<
	aghub_core::skills::install_fetched::FetchedSkillInstallReport,
	InstallMutationError,
> {
	use aghub_core::skills::install_fetched::{
		install_fetched_skill_and_lock, FetchedSkillInstallRequest,
	};
	use aghub_core::skills::linker::LinkTarget;

	let skill_file = aghub_core::skills::update::sanitize_skill_path(
		fetched.root(),
		request.lock_skill_path,
	)
	.ok_or(InstallMutationError::InvalidSkillPath)?;
	install_fetched_skill_and_lock(FetchedSkillInstallRequest {
		skill_file: &skill_file,
		source: request.source,
		lock_skill_path: request.lock_skill_path.to_string(),
		ref_commit: Some(fetched.oid().to_string()),
		scope: request.scope,
		project_root: request.project_root,
		target_agents: request.target_agents,
		expected_name: request.expected_name,
		target: if matches!(request.scope, ResourceScope::ProjectOnly) {
			LinkTarget::Relative
		} else {
			LinkTarget::Absolute
		},
	})
	.map_err(InstallMutationError::Install)
}

pub fn fetch_for_mutation(
	request: FetchedSourceRequest<'_>,
	fetcher: &dyn Fetcher,
	resolver: &dyn TokenResolver,
) -> Result<FetchedSource, FetchMutationError> {
	let token = match resolver.resolve(request.source) {
		TokenResolution::Token(token) => Some(token),
		TokenResolution::NoToken => None,
		TokenResolution::BackendUnavailable => {
			return Err(FetchMutationError::CredentialBackendUnavailable);
		}
	};
	let folder = skill_folder_from_lock_path(request.skill_path)
		.ok_or(FetchMutationError::InvalidSkillPath)?;
	let source_ref = SourceRef {
		source: request.source.to_string(),
		ref_: request.ref_name.map(str::to_string),
	};
	let repo = fetcher
		.fetch(
			&source_ref,
			token.as_deref(),
			FetchSelection::Skills(std::slice::from_ref(&folder)),
		)
		.map_err(FetchMutationError::Fetch)?;
	Ok(FetchedSource { repo })
}

/// Fetch a complete catalog for rename acceptance and resolve the new name to
/// its current repo-relative path. This supports both a frontmatter-only rename
/// at the old path and a rename that moved the skill directory.
pub fn fetch_for_rename(
	request: FetchedRenameRequest<'_>,
	fetcher: &dyn Fetcher,
	resolver: &dyn TokenResolver,
) -> Result<PreparedRename, FetchRenameError> {
	let token = match resolver.resolve(&request.source.source_url) {
		TokenResolution::Token(token) => Some(token),
		TokenResolution::NoToken => None,
		TokenResolution::BackendUnavailable => {
			return Err(FetchRenameError::CredentialBackendUnavailable);
		}
	};
	let source_ref = SourceRef {
		source: request.source.source_url.clone(),
		ref_: request.source.ref_name.clone(),
	};
	let repo = fetcher
		.fetch(
			&source_ref,
			token.as_deref(),
			FetchSelection::CatalogSnapshot,
		)
		.map_err(FetchRenameError::Fetch)?;
	let fetched = FetchedSource { repo };
	let options = skill::scan::ScanOptions {
		max_depth: crate::repository::CATALOG_MAX_DEPTH,
		full_depth: true,
		respect_gitignore: false,
	};
	let skill_dirs =
		skill::scan::scan_skills(fetched.root(), options, Vec::new())
			.map_err(|_| FetchRenameError::CatalogScan)?;
	let matched = skill_dirs.into_iter().find(|directory| {
		skill::parser::parse(&directory.join("SKILL.md"))
			.is_ok_and(|parsed| parsed.name == request.new_name)
	});
	let directory = matched.ok_or(FetchRenameError::SkillNotFound)?;
	let relative = directory
		.strip_prefix(fetched.root())
		.map_err(|_| FetchRenameError::CatalogScan)?;
	let folder = relative.to_string_lossy().replace('\\', "/");
	let validated = skill::SkillPath::parse(&folder)
		.map_err(|_| FetchRenameError::CatalogScan)?;
	let lock_skill_path = if validated.is_root() {
		"SKILL.md".to_string()
	} else {
		format!("{}/SKILL.md", validated.as_str())
	};
	let mut source = request.source.clone();
	source.skill_path = lock_skill_path;
	Ok(PreparedRename { fetched, source })
}

pub struct FetchedResyncRequest<'a> {
	pub skill_path: &'a str,
	pub name: &'a str,
	pub scope: aghub_core::models::ResourceScope,
	pub project_root: Option<&'a Path>,
	/// The entry's identity CAPTURED before this fetch
	/// (`aghub_core::skills::lock::EntryIdentity::capture`), re-verified under the
	/// mutation guard so a repointed entry cannot be overwritten with a stale
	/// fetch. Required: a caller whose capture found no entry has no mandate to
	/// overwrite that skill and must refuse instead of syncing.
	pub expected: aghub_core::skills::lock::EntryIdentity,
}

#[derive(Debug)]
pub enum ResyncMutationError {
	InvalidSkillPath,
	Resync(aghub_core::skills::resync::ResyncError),
}

/// Resync an installed skill from one commit-pinned [`FetchedSource`].
/// Sanitization and the lock identity both come from that same owning source;
/// the transactional swap remains entirely in `aghub-core`.
pub fn resync_fetched_source(
	fetched: &FetchedSource,
	request: FetchedResyncRequest<'_>,
) -> Result<aghub_core::skills::resync::ResyncReport, ResyncMutationError> {
	let skill_file = aghub_core::skills::update::sanitize_skill_path(
		fetched.root(),
		request.skill_path,
	)
	.ok_or(ResyncMutationError::InvalidSkillPath)?;
	let source_dir = skill_file.parent().unwrap_or_else(|| fetched.root());
	aghub_core::skills::resync::resync_installed_skill(
		aghub_core::skills::resync::ResyncRequest {
			source_dir,
			name: request.name,
			scope: request.scope,
			project_root: request.project_root,
			ref_commit: Some(fetched.oid()),
			expected: request.expected,
		},
	)
	.map_err(ResyncMutationError::Resync)
}

pub struct LockedResyncRequest<'a> {
	pub name: &'a str,
	pub scope: ResourceScope,
	pub project_root: Option<&'a Path>,
}

pub struct LockedSkillsResyncRequest<'a> {
	pub source: &'a str,
	pub names: &'a [String],
	pub scope: ResourceScope,
	pub project_root: Option<&'a Path>,
}

#[derive(Debug)]
pub struct LockedSkillResyncResult {
	pub name: String,
	pub outcome:
		Result<aghub_core::skills::resync::ResyncReport, LockedResyncError>,
}

#[derive(Debug)]
pub enum LockedResyncError {
	UnsupportedScope(ResourceScope),
	ProjectRootRequired,
	LockEntryNotFound { scope: ResourceScope },
	MissingSkillPath,
	NotInstalled,
	CredentialBackendUnavailable,
	InvalidSkillPath,
	SourceSkillNotFound,
	SourceChanged,
	Fetch(FetchError),
	Resync(aghub_core::skills::resync::ResyncError),
}

#[derive(Debug)]
pub enum LockedSkillsResyncError {
	EmptyRequest,
	Preflight(LockedResyncError),
	ItemPreflight {
		name: String,
		error: LockedResyncError,
	},
	Fetch(FetchError),
}

#[derive(Clone, Debug)]
struct PreparedLockedResync {
	name: String,
	skill_path: String,
	expected: aghub_core::skills::lock::EntryIdentity,
	group_index: usize,
}

struct PreparedFetchGroup {
	source_ref: SourceRef,
	folders: Vec<skill::SkillPath>,
}

fn same_source(expected: &str, actual: &str) -> bool {
	if expected == actual {
		return true;
	}
	let Ok(expected) = aghub_git::resolve_remote_source(expected) else {
		return false;
	};
	let Ok(actual) = aghub_git::resolve_remote_source(actual) else {
		return false;
	};
	expected.host == actual.host
		&& expected.source.trim_end_matches(".git")
			== actual.source.trim_end_matches(".git")
}

fn prepare_locked_resync(
	name: &str,
	scope: ResourceScope,
	project_root: Option<&Path>,
) -> Result<(SourceRef, PreparedLockedResync), LockedResyncError> {
	// Coordinates and identity must come from the SAME entry read. A second
	// lookup could straddle another process's repoint and let a stale fetch pass
	// the compare-after-fetch against a different observation.
	let (source_ref, skill_path, expected) = match scope {
		ResourceScope::GlobalOnly => {
			let entry = skill::lock::global::get_skill_from_lock(name).ok_or(
				LockedResyncError::LockEntryNotFound {
					scope: ResourceScope::GlobalOnly,
				},
			)?;
			let expected =
				aghub_core::skills::lock::EntryIdentity::of_global_entry(
					&entry,
				);
			(
				SourceRef {
					source: entry.source_url,
					ref_: entry.ref_name,
				},
				entry
					.skill_path
					.ok_or(LockedResyncError::MissingSkillPath)?,
				expected,
			)
		}
		ResourceScope::ProjectOnly => {
			let root =
				project_root.ok_or(LockedResyncError::ProjectRootRequired)?;
			let lock = skill::lock::local::read_local_lock(Some(root));
			let entry = lock.skills.get(name).cloned().ok_or(
				LockedResyncError::LockEntryNotFound {
					scope: ResourceScope::ProjectOnly,
				},
			)?;
			let expected =
				aghub_core::skills::lock::EntryIdentity::of_project_entry(
					&entry,
				);
			(
				SourceRef {
					source: entry.source_url.unwrap_or(entry.source),
					ref_: entry.ref_name,
				},
				entry
					.skill_path
					.ok_or(LockedResyncError::MissingSkillPath)?,
				expected,
			)
		}
		ResourceScope::Both => {
			return Err(LockedResyncError::UnsupportedScope(
				ResourceScope::Both,
			));
		}
	};

	if aghub_core::skills::removal::installed_skill_roots(
		name,
		scope,
		project_root,
	)
	.is_empty()
	{
		return Err(LockedResyncError::NotInstalled);
	}

	Ok((
		source_ref,
		PreparedLockedResync {
			name: name.to_string(),
			skill_path,
			expected,
			group_index: 0,
		},
	))
}

/// Resolve every requested Lock entry before fetching, group entries by their
/// effective Source + ref, and selectively fetch each group once. All groups
/// are fetched and validated before any installed copy is mutated; afterwards
/// every runtime Resync is attempted in request order with its captured
/// compare-after-fetch identity.
pub fn resync_locked_skills(
	request: LockedSkillsResyncRequest<'_>,
	fetcher: &dyn Fetcher,
	resolver: &dyn TokenResolver,
) -> Result<Vec<LockedSkillResyncResult>, LockedSkillsResyncError> {
	match request.scope {
		ResourceScope::Both => {
			return Err(LockedSkillsResyncError::Preflight(
				LockedResyncError::UnsupportedScope(ResourceScope::Both),
			));
		}
		ResourceScope::ProjectOnly if request.project_root.is_none() => {
			return Err(LockedSkillsResyncError::Preflight(
				LockedResyncError::ProjectRootRequired,
			));
		}
		_ => {}
	}
	if request.names.is_empty() {
		return Err(LockedSkillsResyncError::EmptyRequest);
	}

	let mut groups: Vec<PreparedFetchGroup> = Vec::new();
	let mut prepared = Vec::with_capacity(request.names.len());

	for name in request.names {
		let (source_ref, mut item) =
			prepare_locked_resync(name, request.scope, request.project_root)
				.map_err(|error| LockedSkillsResyncError::ItemPreflight {
					name: name.clone(),
					error,
				})?;
		if !same_source(request.source, &source_ref.source) {
			return Err(LockedSkillsResyncError::ItemPreflight {
				name: name.clone(),
				error: LockedResyncError::SourceChanged,
			});
		}
		let folder = skill_folder_from_lock_path(&item.skill_path)
			.ok_or(LockedResyncError::InvalidSkillPath)
			.map_err(|error| LockedSkillsResyncError::ItemPreflight {
				name: name.clone(),
				error,
			})?;
		let group_index = if let Some(index) = groups
			.iter()
			.position(|group| group.source_ref == source_ref)
		{
			index
		} else {
			groups.push(PreparedFetchGroup {
				source_ref,
				folders: Vec::new(),
			});
			groups.len() - 1
		};
		let group = &mut groups[group_index];
		if !group
			.folders
			.iter()
			.any(|seen| seen.as_str() == folder.as_str())
		{
			group.folders.push(folder);
		}
		item.group_index = group_index;
		prepared.push(item);
	}

	let mut fetched_groups = Vec::with_capacity(groups.len());
	for group in &groups {
		let token = match resolver.resolve(&group.source_ref.source) {
			TokenResolution::Token(token) => Some(token),
			TokenResolution::NoToken => None,
			TokenResolution::BackendUnavailable => {
				return Err(LockedSkillsResyncError::Preflight(
					LockedResyncError::CredentialBackendUnavailable,
				));
			}
		};
		let repo = fetcher
			.fetch(
				&group.source_ref,
				token.as_deref(),
				FetchSelection::Skills(&group.folders),
			)
			.map_err(LockedSkillsResyncError::Fetch)?;
		fetched_groups.push(FetchedSource { repo });
	}

	if let Some(item) = prepared.iter().find(|item| {
		!fetched_skill_path_exists(
			&fetched_groups[item.group_index],
			&item.skill_path,
		)
	}) {
		return Err(LockedSkillsResyncError::ItemPreflight {
			name: item.name.clone(),
			error: LockedResyncError::SourceSkillNotFound,
		});
	}

	let report = aghub_core::batch::run_multi_target_mutation(
		&prepared,
		|_| Ok::<(), LockedResyncError>(()),
		|item| {
			resync_fetched_source(
				&fetched_groups[item.group_index],
				FetchedResyncRequest {
					skill_path: &item.skill_path,
					name: &item.name,
					scope: request.scope,
					project_root: request.project_root,
					expected: item.expected.clone(),
				},
			)
			.map_err(|error| match error {
				ResyncMutationError::InvalidSkillPath => {
					LockedResyncError::SourceSkillNotFound
				}
				ResyncMutationError::Resync(error) => {
					LockedResyncError::Resync(error)
				}
			})
		},
	)
	.expect("all predictable failures were rejected before batch execution");

	Ok(report
		.results
		.into_iter()
		.map(|row| LockedSkillResyncResult {
			name: row.target.name,
			outcome: row.result,
		})
		.collect())
}

/// Resolve one locked skill's source, fetch its selected folder, and delegate
/// the transactional install/lock update to the existing Fetched Source seam.
pub fn resync_locked_skill(
	request: LockedResyncRequest<'_>,
	fetcher: &dyn Fetcher,
	resolver: &dyn TokenResolver,
) -> Result<aghub_core::skills::resync::ResyncReport, LockedResyncError> {
	let (source_ref, _) = prepare_locked_resync(
		request.name,
		request.scope,
		request.project_root,
	)?;
	let names = [request.name.to_string()];
	let results = resync_locked_skills(
		LockedSkillsResyncRequest {
			source: &source_ref.source,
			names: &names,
			scope: request.scope,
			project_root: request.project_root,
		},
		fetcher,
		resolver,
	)
	.map_err(|error| match error {
		LockedSkillsResyncError::Preflight(error) => error,
		LockedSkillsResyncError::ItemPreflight { error, .. } => error,
		LockedSkillsResyncError::Fetch(error) => {
			LockedResyncError::Fetch(error)
		}
		LockedSkillsResyncError::EmptyRequest => {
			unreachable!("single-item batch cannot be empty")
		}
	})?;
	results
		.into_iter()
		.next()
		.expect("single-item batch must return one outcome")
		.outcome
}

#[cfg(test)]
mod tests {
	use std::path::Path;
	use std::sync::Mutex;

	use crate::{
		FetchError, FetchSelection, Fetcher, SourceRef, TokenResolution,
		TokenResolver,
	};
	use aghub_core::models::ResourceScope;

	use super::{
		fetch_for_mutation, fetch_for_rename, resync_fetched_source,
		resync_locked_skill, FetchMutationError, FetchedRenameRequest,
		FetchedResyncRequest, FetchedSource, FetchedSourceRequest,
		LockedResyncRequest,
	};

	struct NoToken;
	impl TokenResolver for NoToken {
		fn resolve(&self, _source: &str) -> TokenResolution {
			TokenResolution::NoToken
		}
	}

	struct CatalogFetcher {
		root: std::path::PathBuf,
	}
	impl Fetcher for CatalogFetcher {
		fn fetch(
			&self,
			_source_ref: &SourceRef,
			_token: Option<&str>,
			selection: FetchSelection<'_>,
		) -> Result<crate::FetchedRepo, FetchError> {
			assert!(matches!(selection, FetchSelection::CatalogSnapshot));
			Ok(crate::FetchedRepo {
				root: self.root.clone(),
				snapshot: aghub_git::RepoSnapshot {
					commit_oid: "moved-commit".to_string(),
					tree_oid: "moved-tree".to_string(),
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

	struct UnavailableResolver;

	impl TokenResolver for UnavailableResolver {
		fn resolve(&self, _source: &str) -> TokenResolution {
			TokenResolution::BackendUnavailable
		}
	}

	struct CountingFetcher(Mutex<usize>);

	impl Fetcher for CountingFetcher {
		fn fetch(
			&self,
			_source_ref: &SourceRef,
			_token: Option<&str>,
			_selection: FetchSelection<'_>,
		) -> Result<crate::FetchedRepo, FetchError> {
			*self.0.lock().unwrap() += 1;
			Err(FetchError::Network)
		}
	}

	#[test]
	fn unavailable_credentials_fail_before_fetching_a_source() {
		let fetcher = CountingFetcher(Mutex::new(0));
		let result = fetch_for_mutation(
			FetchedSourceRequest {
				source: "owner/private-source",
				ref_name: Some("main"),
				skill_path: "skills/private/SKILL.md",
			},
			&fetcher,
			&UnavailableResolver,
		);

		assert!(matches!(
			result,
			Err(FetchMutationError::CredentialBackendUnavailable)
		));
		assert_eq!(
			*fetcher.0.lock().unwrap(),
			0,
			"credential failure must precede Fetched Source materialization",
		);
	}

	#[test]
	fn rename_fetch_resolves_a_skill_that_moved_to_a_new_repo_path() {
		let temporary = tempfile::tempdir().unwrap();
		let fetched_root = temporary.path().join("fetched");
		write_skill(
			&fetched_root.join("one/two/three/four/five/six/renamed"),
			"renamed-skill",
			"moved",
		);
		let source = aghub_core::skills::rename::RenameLockSource {
			source: "owner/repo".to_string(),
			source_type: "github".to_string(),
			source_url: "https://github.com/owner/repo".to_string(),
			ref_name: Some("main".to_string()),
			skill_path: "old/location/SKILL.md".to_string(),
			// This test drives the FETCH only; the identity is never compared.
			captured:
				aghub_core::skills::lock::EntryIdentity::unchecked_for_tests(
					"https://github.com/owner/repo",
					Some("old/location/SKILL.md".to_string()),
					Some("main".to_string()),
				),
		};

		let prepared = fetch_for_rename(
			FetchedRenameRequest {
				source: &source,
				new_name: "renamed-skill",
			},
			&CatalogFetcher { root: fetched_root },
			&NoToken,
		)
		.expect("moved rename should resolve the new path");

		assert_eq!(
			prepared.source.skill_path,
			"one/two/three/four/five/six/renamed/SKILL.md"
		);
		assert_eq!(prepared.fetched.oid(), "moved-commit");
	}

	#[test]
	fn resync_uses_the_fetched_source_content_and_commit_identity() {
		let temporary = tempfile::tempdir().unwrap();
		let project = temporary.path().join("project");
		let installed = project.join(".claude/skills/sync-me");
		write_skill(&installed, "sync-me", "old");
		skill::add_skill_to_local_lock(
			"sync-me",
			skill::LocalSkillLockEntry {
				source_url: None,
				source: "owner/repo".to_string(),
				ref_name: Some("main".to_string()),
				source_type: "github".to_string(),
				computed_hash: "old".to_string(),
				skill_path: Some("skills/sync-me/SKILL.md".to_string()),
				ref_commit: None,
			},
			Some(&project),
		)
		.unwrap();

		let fetched_root = temporary.path().join("fetched");
		write_skill(&fetched_root.join("skills/sync-me"), "sync-me", "new");
		let fetched = FetchedSource {
			repo: crate::FetchedRepo {
				root: fetched_root,
				snapshot: aghub_git::RepoSnapshot {
					commit_oid: "new-commit".to_string(),
					tree_oid: "new-tree".to_string(),
					commit_time: None,
				},
				_guard: None,
			},
		};

		let report = resync_fetched_source(
			&fetched,
			FetchedResyncRequest {
				skill_path: "skills/sync-me/SKILL.md",
				name: "sync-me",
				scope: ResourceScope::ProjectOnly,
				project_root: Some(&project),
				// The lock entry has no `source_url`, so its effective source is
				// `source` — the verbatim value a real caller's pre-fetch read
				// would have returned.
				expected: aghub_core::skills::lock::EntryIdentity::capture(
					"sync-me",
					ResourceScope::ProjectOnly,
					Some(&project),
				)
				.expect("fixture entry exists"),
			},
		)
		.expect("Fetched Source should Resync the installed skill");

		assert!(report.swapped.iter().any(|path| path == &installed));
		assert!(std::fs::read_to_string(installed.join("SKILL.md"))
			.unwrap()
			.contains("new"));
		let lock = skill::lock::local::read_local_lock(Some(&project));
		assert_eq!(
			lock.skills["sync-me"].ref_commit.as_deref(),
			Some("new-commit"),
		);
	}

	#[test]
	fn locked_resync_owns_source_lookup_fetch_and_resync() {
		struct Token;
		impl TokenResolver for Token {
			fn resolve(&self, _source: &str) -> TokenResolution {
				TokenResolution::Token("secret".to_string())
			}
		}

		struct RecordingFetcher {
			root: std::path::PathBuf,
			seen: Mutex<Option<(SourceRef, Option<String>)>>,
		}
		impl Fetcher for RecordingFetcher {
			fn fetch(
				&self,
				source_ref: &SourceRef,
				token: Option<&str>,
				selection: FetchSelection<'_>,
			) -> Result<crate::FetchedRepo, FetchError> {
				assert!(matches!(
					selection,
					FetchSelection::Skills(paths)
						if paths.len() == 1
							&& paths[0].as_str() == "skills/sync-me"
				));
				*self.seen.lock().unwrap() =
					Some((source_ref.clone(), token.map(str::to_string)));
				Ok(crate::FetchedRepo {
					root: self.root.clone(),
					snapshot: aghub_git::RepoSnapshot {
						commit_oid: "locked-commit".to_string(),
						tree_oid: "locked-tree".to_string(),
						commit_time: None,
					},
					_guard: None,
				})
			}
		}

		let temporary = tempfile::tempdir().unwrap();
		let project = temporary.path().join("project");
		let installed = project.join(".claude/skills/sync-me");
		write_skill(&installed, "sync-me", "old");
		skill::add_skill_to_local_lock(
			"sync-me",
			skill::LocalSkillLockEntry {
				source_url: Some(
					"https://git.example/owner/repo.git".to_string(),
				),
				source: "owner/repo".to_string(),
				ref_name: Some("main".to_string()),
				source_type: "git".to_string(),
				computed_hash: "old".to_string(),
				skill_path: Some("skills/sync-me/SKILL.md".to_string()),
				ref_commit: None,
			},
			Some(&project),
		)
		.unwrap();
		let fetched_root = temporary.path().join("locked-fetched");
		write_skill(&fetched_root.join("skills/sync-me"), "sync-me", "new");
		let fetcher = RecordingFetcher {
			root: fetched_root,
			seen: Mutex::new(None),
		};

		let report = resync_locked_skill(
			LockedResyncRequest {
				name: "sync-me",
				scope: ResourceScope::ProjectOnly,
				project_root: Some(&project),
			},
			&fetcher,
			&Token,
		)
		.expect("locked Resync should succeed");

		assert!(report.swapped.iter().any(|path| path == &installed));
		let seen = fetcher.seen.lock().unwrap();
		let (source_ref, token) = seen.as_ref().expect("fetch call");
		assert_eq!(source_ref.source, "https://git.example/owner/repo.git");
		assert_eq!(source_ref.ref_.as_deref(), Some("main"));
		assert_eq!(token.as_deref(), Some("secret"));
		let lock = skill::lock::local::read_local_lock(Some(&project));
		assert_eq!(
			lock.skills["sync-me"].ref_commit.as_deref(),
			Some("locked-commit"),
		);
	}
}
