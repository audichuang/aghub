use std::path::{Path, PathBuf};

use aghub_core::models::{AgentType, ResourceScope};
use aghub_core::WriteScope;

use crate::{
	skill_folder_from_lock_path, FetchError, FetchSelection, FetchedRepo,
	Fetcher, SourceRef, TokenResolver,
};

pub(crate) struct FetchedRenameRequest<'a> {
	pub source: &'a aghub_core::skills::rename::RenameLockSource,
	pub new_name: &'a str,
}

#[derive(Debug)]
pub enum RenameMutationError {
	/// A refusal or failure from the core rename (plan or transaction).
	Rename(aghub_core::skills::rename::RenameError),
	Fetch(FetchError),
	CatalogScan,
	SkillNotFound,
}

impl RenameMutationError {
	/// Stable machine code shared by every surface; `None` where the core
	/// error has none (surfaces then report their own "no code" spelling).
	pub fn code(&self) -> Option<&'static str> {
		match self {
			Self::Rename(error) => error.code(),
			Self::Fetch(FetchError::BackendUnavailable) => {
				Some(KEYCHAIN_UNAVAILABLE_CODE)
			}
			Self::Fetch(_) => Some(SOURCE_FETCH_FAILED_CODE),
			Self::CatalogScan | Self::SkillNotFound => {
				Some(SKILL_PATH_NOT_FOUND_CODE)
			}
		}
	}

	/// Whether this failure is transient lock contention.
	pub fn retryable(&self) -> bool {
		matches!(
			self,
			Self::Rename(aghub_core::skills::rename::RenameError::Locked(_))
		)
	}
}

pub(crate) struct PreparedRename {
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
	fetched_skill_file(fetched, lock_skill_path).is_some()
}

fn fetched_skill_file(fetched: &FetchedSource, path: &str) -> Option<PathBuf> {
	let folder = skill_folder_from_lock_path(path)?;
	let marker = if folder.is_root() {
		"SKILL.md".to_string()
	} else {
		format!("{}/SKILL.md", folder.as_str())
	};
	aghub_core::skills::update::sanitize_skill_path(fetched.root(), &marker)
}

/// Run the existing core rename transaction against one commit-pinned fetched
/// source without exposing its root or commit identity to the adapter.
pub(crate) fn accept_fetched_rename(
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
	pub scope: WriteScope,
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
	let skill_file = fetched_skill_file(fetched, request.lock_skill_path)
		.ok_or(InstallMutationError::InvalidSkillPath)?;
	aghub_core::skills::install_fetched::install_fetched_skill_and_lock(
		core_install_request(fetched, &request, &skill_file),
	)
	.map_err(InstallMutationError::Install)
}

/// Advisory, lock-free dry run of [`install_fetched_source`]'s refusals. It
/// builds the identical core request and runs the identical guard; the install
/// re-checks under the mutation lock, so the answer can go stale.
pub fn preflight_fetched_source(
	fetched: &FetchedSource,
	request: FetchedInstallRequest<'_>,
) -> Result<(), InstallMutationError> {
	let skill_file = fetched_skill_file(fetched, request.lock_skill_path)
		.ok_or(InstallMutationError::InvalidSkillPath)?;
	aghub_core::skills::install_fetched::preflight_fetched_install(
		&core_install_request(fetched, &request, &skill_file),
	)
	.map_err(InstallMutationError::Install)
}

/// The ONE construction of the core install request, so the preview and the
/// install cannot disagree about what is being installed.
fn core_install_request<'a>(
	fetched: &FetchedSource,
	request: &FetchedInstallRequest<'a>,
	skill_file: &'a Path,
) -> aghub_core::skills::install_fetched::FetchedSkillInstallRequest<'a> {
	use aghub_core::skills::linker::LinkTarget;

	aghub_core::skills::install_fetched::FetchedSkillInstallRequest {
		skill_file,
		source: request.source,
		lock_skill_path: request.lock_skill_path.to_string(),
		ref_commit: Some(fetched.oid().to_string()),
		scope: request.scope.clone(),
		target_agents: request.target_agents,
		expected_name: request.expected_name,
		target: match request.scope {
			WriteScope::Project { .. } => LinkTarget::Relative,
			WriteScope::Global => LinkTarget::Absolute,
		},
	}
}

/// Fetch a complete catalog for rename acceptance and resolve the new name to
/// its current repo-relative path. This supports both a frontmatter-only rename
/// at the old path and a rename that moved the skill directory.
pub(crate) fn fetch_for_rename(
	request: FetchedRenameRequest<'_>,
	fetcher: &dyn Fetcher,
	resolver: &dyn TokenResolver,
) -> Result<PreparedRename, RenameMutationError> {
	let source_ref = SourceRef {
		source: request.source.source_url.clone(),
		ref_: request.source.ref_name.clone(),
	};
	let repo = crate::sources::fetch_source_with_resolver(
		&source_ref,
		fetcher,
		resolver,
		FetchSelection::CatalogSnapshot,
	)
	.map_err(RenameMutationError::Fetch)?;
	let fetched = FetchedSource { repo };
	let options = skill::scan::ScanOptions {
		max_depth: crate::repository::CATALOG_MAX_DEPTH,
		full_depth: true,
		respect_gitignore: false,
	};
	let skill_dirs =
		skill::scan::scan_skills(fetched.root(), options, Vec::new())
			.map_err(|_| RenameMutationError::CatalogScan)?;
	let matched = skill_dirs.into_iter().find(|directory| {
		skill::parser::parse(&directory.join("SKILL.md"))
			.is_ok_and(|parsed| parsed.name == request.new_name)
	});
	let directory = matched.ok_or(RenameMutationError::SkillNotFound)?;
	let relative = directory
		.strip_prefix(fetched.root())
		.map_err(|_| RenameMutationError::CatalogScan)?;
	let folder = relative.to_string_lossy().replace('\\', "/");
	let validated = skill::SkillPath::parse(&folder)
		.map_err(|_| RenameMutationError::CatalogScan)?;
	let lock_skill_path = if validated.is_root() {
		"SKILL.md".to_string()
	} else {
		format!("{}/SKILL.md", validated.as_str())
	};
	let mut source = request.source.clone();
	source.skill_path = lock_skill_path;
	Ok(PreparedRename { fetched, source })
}

pub struct LockedRenameRequest<'a> {
	pub old_name: &'a str,
	pub new_name: &'a str,
	pub scope: WriteScope,
	/// `--ref` override: fetched AND written to the new lock entry. `None`
	/// keeps the locked ref (the API always passes `None`).
	pub git_ref: Option<&'a str>,
}

/// Every rename refusal answerable WITHOUT the network, in the order both
/// surfaces report them: degenerate names, old name not in the lock, new name
/// already present. The CLI preview and [`rename_locked_skill`] both call it,
/// so a preview never green-lights what the commit refuses.
/// See docs/history/cli.md#accept-rename-preview
pub fn plan_locked_rename(
	request: &LockedRenameRequest<'_>,
) -> Result<aghub_core::skills::rename::RenameLockSource, RenameMutationError> {
	use aghub_core::skills::rename;
	rename::ensure_distinct_names(request.old_name, request.new_name)
		.map_err(RenameMutationError::Rename)?;
	let source =
		rename::rename_source_from_lock(request.old_name, &request.scope)
			.map_err(RenameMutationError::Rename)?;
	let agent_dirs = aghub_core::skills::removal::agent_skill_dirs_in_scope(
		request.scope.resource_scope(),
		request.scope.project_root(),
	);
	if rename::new_name_exists_in_scope(
		request.new_name,
		request.scope.resource_scope(),
		request.scope.project_root(),
		&agent_dirs,
	) {
		return Err(RenameMutationError::Rename(
			rename::RenameError::TargetExists(request.new_name.to_string()),
		));
	}
	Ok(source)
}

/// Accept an upstream rename end to end: plan, fetch a commit-pinned catalog,
/// then the core transaction (ADR-0001). Synchronous and blocking (network +
/// mutation lock): an async caller must run it on its blocking pool.
pub fn rename_locked_skill(
	request: LockedRenameRequest<'_>,
	fetcher: &dyn Fetcher,
	resolver: &dyn TokenResolver,
) -> Result<aghub_core::skills::rename::RenameSuccess, RenameMutationError> {
	let mut source = plan_locked_rename(&request)?;
	source.ref_name = request.git_ref.map(str::to_string).or(source.ref_name);
	let prepared = fetch_for_rename(
		FetchedRenameRequest {
			source: &source,
			new_name: request.new_name,
		},
		fetcher,
		resolver,
	)?;
	accept_fetched_rename(
		&prepared.fetched,
		aghub_core::skills::rename::RenameRequest {
			old_name: request.old_name,
			new_name: request.new_name,
			scope: request.scope,
		},
		&prepared.source,
	)
	.map_err(RenameMutationError::Rename)
}

pub struct FetchedResyncRequest<'a> {
	pub skill_path: &'a str,
	pub name: &'a str,
	pub scope: WriteScope,
	/// The coordinate the Fetched Source was fetched from. Refused unless the
	/// pre-fetch identity describes it (`EntryIdentity::describes`).
	pub source: &'a str,
	/// The entry's identity captured BEFORE the fetch
	/// (`EntryIdentity::capture` / `of_*_entry`). `None` = there was no entry
	/// then: this caller never saw the skill and has no mandate to overwrite
	/// it, so the seam refuses with SOURCE_CHANGED_DURING_FETCH.
	pub expected: Option<aghub_core::skills::lock::EntryIdentity>,
}

pub const SKILL_PATH_NOT_FOUND_CODE: &str = "SKILL_PATH_NOT_FOUND";
pub const SKILL_LOCK_ENTRY_NOT_FOUND_CODE: &str = "SKILL_LOCK_ENTRY_NOT_FOUND";
pub const SOURCE_FETCH_FAILED_CODE: &str = "SOURCE_FETCH_FAILED";
pub const KEYCHAIN_UNAVAILABLE_CODE: &str = "KEYCHAIN_UNAVAILABLE";
pub const SKILL_SOURCE_VIEW_STALE_CODE: &str = "SKILL_SOURCE_VIEW_STALE";
pub const SKILL_SOURCE_MISMATCH_CODE: &str = "SKILL_SOURCE_MISMATCH";
pub const INVALID_SCOPE_CODE: &str = "INVALID_SCOPE";
pub const MISSING_PARAM_CODE: &str = "MISSING_PARAM";

#[derive(Debug)]
pub enum ResyncMutationError {
	InvalidSkillPath,
	/// No Lock entry when the fetch started; one exists now. Nothing was written.
	SourceChangedDuringFetch,
	/// The fetched coordinates are not the ones the Lock entry names. Nothing was written.
	SourceMismatch,
	Resync(aghub_core::skills::resync::ResyncError),
}

impl ResyncMutationError {
	/// Stable machine code for this resync failure.
	pub fn code(&self) -> &'static str {
		match self {
			Self::InvalidSkillPath => SKILL_PATH_NOT_FOUND_CODE,
			Self::SourceChangedDuringFetch => {
				aghub_core::skills::lock::SOURCE_CHANGED_DURING_FETCH_CODE
			}
			Self::SourceMismatch => SKILL_SOURCE_MISMATCH_CODE,
			Self::Resync(err) => err.code(),
		}
	}

	/// Whether this failure is a transient lock contention that is retryable.
	pub fn retryable(&self) -> bool {
		match self {
			Self::InvalidSkillPath
			| Self::SourceChangedDuringFetch
			| Self::SourceMismatch => false,
			Self::Resync(err) => err.retryable(),
		}
	}
}

/// Resync an installed skill from one commit-pinned [`FetchedSource`].
/// Sanitization and the lock identity both come from that same owning source;
/// the transactional swap remains entirely in `aghub-core`.
pub fn resync_fetched_source(
	fetched: &FetchedSource,
	request: FetchedResyncRequest<'_>,
) -> Result<aghub_core::skills::resync::ResyncReport, ResyncMutationError> {
	// Order matters: the appeared check runs before anything reads disk.
	let Some(expected) = request.expected else {
		return Err(ResyncMutationError::SourceChangedDuringFetch);
	};
	if !expected.describes(request.source, request.skill_path) {
		return Err(ResyncMutationError::SourceMismatch);
	}
	let skill_file = fetched_skill_file(fetched, request.skill_path)
		.ok_or(ResyncMutationError::InvalidSkillPath)?;
	let source_dir = skill_file.parent().unwrap_or_else(|| fetched.root());
	aghub_core::skills::resync::resync_installed_skill(
		aghub_core::skills::resync::ResyncRequest {
			source_dir,
			name: request.name,
			scope: request.scope,
			ref_commit: Some(fetched.oid()),
			expected,
		},
	)
	.map_err(ResyncMutationError::Resync)
}

pub struct LockedResyncRequest<'a> {
	pub name: &'a str,
	pub scope: WriteScope,
}

pub struct LockedSkillsResyncRequest<'a> {
	/// The Source GROUP the caller believes every named entry still belongs to
	/// — a Sources-row identity, NOT a repository coordinate. Nothing is ever
	/// fetched from it: each row fetches from its own entry's `sourceUrl`. Its
	/// only job is to reject a name whose entry no longer belongs to the row the
	/// caller was looking at. `None` skips the check: a caller that read the
	/// coordinates from the very Lock read this flow performs has nothing
	/// independent to assert against.
	///
	/// Judged by `sources::source_matches`, the SAME predicate the grouping
	/// uses — never a stricter one (see
	/// docs/history/skill-update.md#source-membership-has-one-definition).
	pub source_group: Option<&'a str>,
	pub names: &'a [String],
	pub scope: WriteScope,
}

#[derive(Debug)]
pub struct LockedSkillResyncResult {
	pub name: String,
	pub outcome:
		Result<aghub_core::skills::resync::ResyncReport, LockedResyncError>,
}

#[derive(Debug)]
pub enum LockedResyncError {
	LockEntryNotFound { scope: ResourceScope },
	MissingSkillPath,
	NotInstalled,
	InvalidSkillPath,
	SourceSkillNotFound,
	SourceGroupMismatch,
	Fetch(FetchError),
	Resync(aghub_core::skills::resync::ResyncError),
}

impl LockedResyncError {
	/// Stable machine code for this locked resync failure.
	pub fn code(&self) -> &'static str {
		match self {
			Self::LockEntryNotFound { .. } => SKILL_LOCK_ENTRY_NOT_FOUND_CODE,
			Self::MissingSkillPath
			| Self::InvalidSkillPath
			| Self::SourceSkillNotFound => SKILL_PATH_NOT_FOUND_CODE,
			Self::NotInstalled => {
				aghub_core::skills::resync::ResyncError::NotInstalled.code()
			}
			Self::SourceGroupMismatch => SKILL_SOURCE_VIEW_STALE_CODE,
			Self::Fetch(FetchError::BackendUnavailable) => {
				KEYCHAIN_UNAVAILABLE_CODE
			}
			Self::Fetch(_) => SOURCE_FETCH_FAILED_CODE,
			Self::Resync(err) => err.code(),
		}
	}

	/// Whether this failure is a transient lock contention that is retryable.
	pub fn retryable(&self) -> bool {
		match self {
			Self::Resync(err) => err.retryable(),
			_ => false,
		}
	}
}

impl From<ResyncMutationError> for LockedResyncError {
	fn from(error: ResyncMutationError) -> Self {
		match error {
			ResyncMutationError::InvalidSkillPath => {
				LockedResyncError::SourceSkillNotFound
			}
			ResyncMutationError::SourceChangedDuringFetch => {
				LockedResyncError::Resync(
					aghub_core::skills::resync::ResyncError::StaleFetch(
						"lock entry appeared during the fetch".to_string(),
					),
				)
			}
			// The batch fetches from the entry's own coordinates read in the same
			// observation, so this is unreachable there; if it ever fires,
			// "refresh and retry" is the right answer.
			ResyncMutationError::SourceMismatch => {
				LockedResyncError::SourceGroupMismatch
			}
			ResyncMutationError::Resync(error) => {
				LockedResyncError::Resync(error)
			}
		}
	}
}

/// Only a request that cannot produce rows AT ALL fails as a whole: an
/// unsupported scope, or no names. Every per-entry failure — including one
/// whose fetch group failed — is an ordered row in the returned `Vec`, because
/// the named skills are INDEPENDENT of each other: aborting the batch over one
/// unresolvable entry would cost the others their update and buy no atomicity
/// (each row's own install+lock swap is already transactional under the
/// mutation lock). This is deliberately NOT
/// `aghub_core::batch::run_multi_target_mutation`'s all-or-nothing preflight —
/// that policy exists for ONE resource fanned out to many agents, where a
/// partial batch leaves the agents inconsistent with each other.
#[derive(Debug)]
pub enum LockedSkillsResyncError {
	EmptyRequest,
	Preflight(LockedResyncError),
}

#[derive(Debug)]
struct PreparedLockedResync {
	skill_path: String,
	expected: aghub_core::skills::lock::EntryIdentity,
	group_index: usize,
}

/// One requested skill, in request order: either ready to resync from its
/// fetch group, or already failed and no longer attemptable.
#[derive(Debug)]
struct ResyncRow {
	name: String,
	prepared: Result<PreparedLockedResync, LockedResyncError>,
}

struct PreparedFetchGroup {
	source_ref: SourceRef,
	folders: Vec<skill::SkillPath>,
}

/// One Lock entry's two Source identities: the coordinate its content is
/// fetched from, and the identifier the Sources view groups it under. They
/// differ whenever a lock records both `source` and `sourceUrl`, and the
/// assertion MUST use the grouping one — see [`resync_locked_skills`].
struct EntrySource {
	source_ref: SourceRef,
	grouping_source: String,
	source_type: String,
}

/// ONE read of the scope's lock, shared by every requested name. Re-reading and
/// re-parsing per name made a large batch cost O(names) full lock parses inside a
/// single blocking task — which is what forced a low cap on `names`. It also
/// makes the whole batch observe ONE snapshot, so two rows can no longer be
/// prepared from lock states that straddle another process's write.
enum ScopeLock {
	Global(std::collections::BTreeMap<String, skill::SkillLockEntry>),
	Project(std::collections::BTreeMap<String, skill::LocalSkillLockEntry>),
}

// Counts `ScopeLock::read` and `load_all_agents` calls on the CURRENT thread, so
// a test can pin the one-per-batch properties without a process-wide counter
// that concurrent tests would pollute.
#[cfg(test)]
thread_local! {
	pub(super) static LOCK_READS: std::cell::Cell<usize> =
		const { std::cell::Cell::new(0) };
	pub(super) static AGENT_SCANS: std::cell::Cell<usize> =
		const { std::cell::Cell::new(0) };
}

/// The batch's agent scan, counted. Every registered agent's config is re-read
/// from disk, so doing it per name is what made the advisory installed-check
/// cost `O(names × agents)`.
///
/// `pub(crate)` so the Sources baseline builder shares the SAME counted entry
/// point — a scan that bypassed it would be invisible to the tests pinning
/// one-scan-per-batch.
pub(crate) fn scan_agents(
	scope: &WriteScope,
) -> Vec<aghub_core::AgentResources> {
	#[cfg(test)]
	AGENT_SCANS.with(|scans| scans.set(scans.get() + 1));
	aghub_core::load_managed_agents(
		scope.resource_scope(),
		scope.project_root(),
	)
}

impl ScopeLock {
	fn read(scope: &WriteScope) -> Result<Self, LockedResyncError> {
		#[cfg(test)]
		LOCK_READS.with(|reads| reads.set(reads.get() + 1));
		match scope {
			WriteScope::Global => {
				Ok(Self::Global(skill::get_all_locked_skills()))
			}
			WriteScope::Project { root } => Ok(Self::Project(
				skill::lock::local::read_local_lock(Some(root)).skills,
			)),
		}
	}
}

fn prepare_locked_resync(
	name: &str,
	lock: &ScopeLock,
	agents: &[aghub_core::AgentResources],
	scope: &WriteScope,
) -> Result<(EntrySource, PreparedLockedResync), LockedResyncError> {
	// Coordinates and identity must come from the SAME entry observation. A
	// second lookup could straddle another process's repoint and let a stale
	// fetch pass the compare-after-fetch against a different observation.
	let (entry_source, skill_path, expected) = match lock {
		ScopeLock::Global(entries) => {
			let entry = entries.get(name).cloned().ok_or(
				LockedResyncError::LockEntryNotFound {
					scope: ResourceScope::GlobalOnly,
				},
			)?;
			let expected =
				aghub_core::skills::lock::EntryIdentity::of_global_entry(
					&entry,
				);
			(
				EntrySource {
					source_ref: SourceRef {
						source: crate::sources::entry_clone_source(
							&entry.source,
							Some(&entry.source_url),
							&entry.source_type,
						),
						ref_: entry.ref_name,
					},
					grouping_source: entry.source,
					source_type: entry.source_type,
				},
				entry
					.skill_path
					.ok_or(LockedResyncError::MissingSkillPath)?,
				expected,
			)
		}
		ScopeLock::Project(entries) => {
			let entry = entries.get(name).cloned().ok_or(
				LockedResyncError::LockEntryNotFound {
					scope: ResourceScope::ProjectOnly,
				},
			)?;
			let expected =
				aghub_core::skills::lock::EntryIdentity::of_project_entry(
					&entry,
				);
			(
				EntrySource {
					source_ref: SourceRef {
						// The SAME coordinate the Sources row advertises and
						// `diff_source` fetches. Resolving the raw `source` here
						// instead made a GitLab row fetch GitHub shorthand and
						// stamp GitHub's commit into the GitLab lock entry.
						source: crate::sources::entry_clone_source(
							&entry.source,
							entry.source_url.as_deref(),
							&entry.source_type,
						),
						ref_: entry.ref_name,
					},
					grouping_source: entry.source,
					source_type: entry.source_type,
				},
				entry
					.skill_path
					.ok_or(LockedResyncError::MissingSkillPath)?,
				expected,
			)
		}
	};

	// Advisory only: the transaction resolves targets again under the lock.
	// Include withheld Masters, but skip fetching when no copy exists at all.
	if aghub_core::skills::resync::resync_targets_in(agents, name, scope)
		.map_err(LockedResyncError::Resync)?
		.is_empty()
	{
		return Err(LockedResyncError::NotInstalled);
	}

	Ok((
		entry_source,
		PreparedLockedResync {
			skill_path,
			expected,
			group_index: 0,
		},
	))
}

/// Request order, first occurrence only. A repeated name would otherwise be
/// attempted twice against ONE captured identity: the second attempt fails the
/// compare-after-fetch it cannot satisfy (the first attempt just re-stamped the
/// entry) and reports a phantom concurrent-change to the caller.
fn unique_in_order(names: &[String]) -> Vec<&String> {
	let mut seen = std::collections::HashSet::with_capacity(names.len());
	names
		.iter()
		.filter(|name| seen.insert(name.as_str()))
		.collect()
}

/// Resolve every requested Lock entry before fetching, group the resolvable
/// ones by their effective Source + ref, and selectively fetch each group once.
/// Nothing is written until every group has been fetched, so a fetch failure
/// cannot leave a half-updated batch; then every ready row is attempted in
/// request order against its captured compare-after-fetch identity. One row's
/// failure — at resolution, at its group's fetch, or at its own transaction —
/// never suppresses another row.
pub fn resync_locked_skills(
	request: LockedSkillsResyncRequest<'_>,
	fetcher: &dyn Fetcher,
	resolver: &dyn TokenResolver,
) -> Result<Vec<LockedSkillResyncResult>, LockedSkillsResyncError> {
	if request.names.is_empty() {
		return Err(LockedSkillsResyncError::EmptyRequest);
	}

	let lock = match ScopeLock::read(&request.scope) {
		Ok(lock) => lock,
		Err(error) => {
			return Err(LockedSkillsResyncError::Preflight(error));
		}
	};
	// ONE agent scan for the whole batch, next to the ONE lock read: the answer
	// does not vary by name.
	let agents = scan_agents(&request.scope);
	let mut groups: Vec<PreparedFetchGroup> = Vec::new();
	let names = unique_in_order(request.names);
	let mut rows = Vec::with_capacity(names.len());

	for name in names {
		let prepared =
			prepare_locked_resync(name, &lock, &agents, &request.scope)
				.and_then(|(entry, mut item)| {
					let EntrySource {
						source_ref,
						grouping_source,
						source_type,
					} = entry;
					if request.source_group.is_some_and(|group| {
						!crate::sources::source_matches(
							group,
							&grouping_source,
							Some(&source_ref.source),
							&source_type,
						)
					}) {
						return Err(LockedResyncError::SourceGroupMismatch);
					}
					let folder = skill_folder_from_lock_path(&item.skill_path)
						.ok_or(LockedResyncError::InvalidSkillPath)?;
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
					Ok(item)
				});
		rows.push(ResyncRow {
			name: name.clone(),
			prepared,
		});
	}

	// Every group is fetched BEFORE the first write, so no row can be swapped
	// while a later group is still on the network.
	let fetched_groups: Vec<Result<FetchedSource, FetchError>> = groups
		.iter()
		.map(|group| {
			crate::sources::fetch_source_with_resolver(
				&group.source_ref,
				fetcher,
				resolver,
				FetchSelection::Skills(&group.folders),
			)
			.map(FetchedSource::from_repo)
		})
		.collect();

	// Each row takes the mutation lock for its own transaction; the batch
	// deliberately does NOT hold one guard across all of them: a batch-long
	// hold queues every unrelated in-process mutation and pushes other
	// processes into their 10s bound (the API stops answering). Cost: the
	// batch is NOT atomic — another aghub landing between rows can leave this
	// Source's entries on different commits, undetected by `EntryIdentity`
	// (coordinates, not commit). Each row stays internally consistent and the
	// next check re-flags the drift: bounded and self-healing, by choice.
	Ok(rows
		.into_iter()
		.map(|ResyncRow { name, prepared }| {
			let outcome = prepared.and_then(|item| {
				let fetched = fetched_groups[item.group_index]
					.as_ref()
					.map_err(|error| LockedResyncError::Fetch(error.clone()))?;
				if !fetched_skill_path_exists(fetched, &item.skill_path) {
					return Err(LockedResyncError::SourceSkillNotFound);
				}
				resync_fetched_source(
					fetched,
					FetchedResyncRequest {
						skill_path: &item.skill_path,
						name: &name,
						scope: request.scope.clone(),
						source: &groups[item.group_index].source_ref.source,
						expected: Some(item.expected),
					},
				)
				.map_err(LockedResyncError::from)
			});
			LockedSkillResyncResult { name, outcome }
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
	// `source_group: None` — this caller has no independent Sources view to
	// check against, and checking one would mean reading the entry a SECOND
	// time: a repoint landing between the two reads would fail an update that
	// is perfectly safe to apply against the coordinates actually read here.
	let names = [request.name.to_string()];
	let results = resync_locked_skills(
		LockedSkillsResyncRequest {
			source_group: None,
			names: &names,
			scope: request.scope,
		},
		fetcher,
		resolver,
	)
	.map_err(|error| match error {
		LockedSkillsResyncError::Preflight(error) => error,
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
	use aghub_core::WriteScope;

	use super::{
		fetch_for_rename, resync_fetched_source, resync_locked_skill,
		FetchedRenameRequest, FetchedResyncRequest, FetchedSource,
		LockedResyncError, LockedResyncRequest, ResyncMutationError,
		KEYCHAIN_UNAVAILABLE_CODE, SKILL_LOCK_ENTRY_NOT_FOUND_CODE,
		SKILL_PATH_NOT_FOUND_CODE, SKILL_SOURCE_VIEW_STALE_CODE,
		SOURCE_FETCH_FAILED_CODE,
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
		for skill_path in ["skills/sync-me/SKILL.md", "skills/sync-me"] {
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
					skill_path,
					name: "sync-me",
					scope: WriteScope::project(&project),
					// The lock entry has no `source_url`, so its effective source is
					// `source` — the verbatim value a real caller's pre-fetch read
					// would have returned.
					source: "owner/repo",
					expected: Some(
						aghub_core::skills::lock::EntryIdentity::capture(
							"sync-me",
							ResourceScope::ProjectOnly,
							Some(&project),
						)
						.expect("fixture entry exists"),
					),
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
	}

	/// The batch must call [`ScopeLock::read`] and [`scan_agents`] ONCE each, not
	/// once per name. Reading the Lock per name cost O(names) full parses AND let
	/// two rows be prepared from lock states straddling another process's write;
	/// scanning the agents per name re-read every registered agent's config from
	/// disk, which is what forced a tighter cap on `names`.
	///
	/// Both counters observe the named functions only. A direct `read_local_lock`
	/// or `load_all_agents` added inside `prepare_locked_resync` would still
	/// pass, and every successful row re-parses the Lock and re-scans the agents
	/// anyway inside its own transaction — deliberately, since those reads must
	/// happen under the mutation lock. What this pins is the shape the regression
	/// actually takes.
	#[test]
	fn a_batch_reads_the_lock_exactly_once() {
		struct StubFetcher {
			root: std::path::PathBuf,
		}
		impl Fetcher for StubFetcher {
			fn fetch(
				&self,
				_source_ref: &SourceRef,
				_token: Option<&str>,
				_selection: FetchSelection<'_>,
			) -> Result<crate::FetchedRepo, FetchError> {
				Ok(crate::FetchedRepo {
					root: self.root.clone(),
					snapshot: aghub_git::RepoSnapshot {
						commit_oid: "once-commit".to_string(),
						tree_oid: "once-tree".to_string(),
						commit_time: None,
					},
					_guard: None,
				})
			}
		}

		let temporary = tempfile::tempdir().unwrap();
		let project = temporary.path().join("project");
		let fetched_root = temporary.path().join("fetched");
		let names = ["one", "two", "three"];
		for name in names {
			write_skill(
				&project.join(format!(".claude/skills/{name}")),
				name,
				"old",
			);
			write_skill(
				&fetched_root.join(format!("skills/{name}")),
				name,
				"new",
			);
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
		let owned = names.map(str::to_string);

		super::LOCK_READS.with(|reads| reads.set(0));
		super::AGENT_SCANS.with(|scans| scans.set(0));
		let results = super::resync_locked_skills(
			super::LockedSkillsResyncRequest {
				source_group: None,
				names: &owned,
				scope: WriteScope::project(&project),
			},
			&StubFetcher { root: fetched_root },
			&NoToken,
		)
		.expect("three locked skills should resync");

		assert!(results.iter().all(|row| row.outcome.is_ok()));
		assert_eq!(
			super::LOCK_READS.with(|reads| reads.get()),
			1,
			"a batch of {} names must call ScopeLock::read once",
			names.len()
		);
		assert_eq!(
			super::AGENT_SCANS.with(|scans| scans.get()),
			1,
			"a batch of {} names must scan the agents once",
			names.len()
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

		for layout in [".claude/skills/sync-me", ".aghub/sync-me"] {
			let temporary = tempfile::tempdir().unwrap();
			let project = temporary.path().join("project");
			let installed = project.join(layout);
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
					scope: WriteScope::project(&project),
				},
				&fetcher,
				&Token,
			)
			.expect("locked Resync should succeed");

			assert_eq!(report.swapped.len(), 1);
			assert_eq!(
				report.swapped[0].canonicalize().unwrap(),
				installed.canonicalize().unwrap()
			);
			if layout.starts_with(".aghub") {
				assert!(aghub_core::load_all_agents(
					ResourceScope::ProjectOnly,
					Some(&project)
				)
				.iter()
				.all(|agent| agent.skills.is_empty()));
			}
			assert!(std::fs::read_to_string(installed.join("SKILL.md"))
				.unwrap()
				.contains("new"));
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

	#[test]
	fn locked_resync_error_and_resync_error_code_and_retryability_mapping() {
		use aghub_core::skills::resync::ResyncError;

		// Table asserting code and retryable for every variant of ResyncError
		let resync_cases = [
			(
				ResyncError::Locked(std::io::Error::new(
					std::io::ErrorKind::WouldBlock,
					"lock busy",
				)),
				"SKILL_MUTATION_LOCK_BUSY",
				true,
			),
			(
				ResyncError::Locked(std::io::Error::other("permission denied")),
				"IO_ERROR",
				false,
			),
			(
				ResyncError::StaleFetch("stale".into()),
				"SKILL_SOURCE_CHANGED_DURING_FETCH",
				false,
			),
			(ResyncError::NotInstalled, "SKILL_NOT_INSTALLED", false),
			(
				ResyncError::Renamed {
					new_name: "renamed".into(),
				},
				"SKILL_RENAMED_IN_SOURCE",
				false,
			),
			(
				ResyncError::Parse("parse err".into()),
				"SKILL_PARSE_FAILED",
				false,
			),
			(
				ResyncError::Conflict("conflict".into()),
				"SKILL_UPDATE_CONFLICT",
				false,
			),
			(
				ResyncError::OutOfTree("escape".into()),
				"SKILL_TARGET_OUT_OF_TREE",
				false,
			),
			(
				ResyncError::Hash("hash err".into()),
				"SKILL_SYNC_ERROR",
				false,
			),
			(
				ResyncError::Swap("swap err".into()),
				"SKILL_SYNC_ERROR",
				false,
			),
			(
				ResyncError::LockUpdate("lock err".into()),
				"SKILL_LOCK_ERROR",
				false,
			),
		];
		for (err, expected_code, expected_retryable) in resync_cases {
			assert_eq!(err.code(), expected_code, "{err:?}.code() mismatch");
			assert_eq!(
				err.retryable(),
				expected_retryable,
				"{err:?}.retryable() mismatch"
			);
		}

		// Table asserting code and retryable for every variant of LockedResyncError
		let locked_cases: Vec<(LockedResyncError, &'static str, bool)> = vec![
			(
				LockedResyncError::LockEntryNotFound {
					scope: ResourceScope::GlobalOnly,
				},
				SKILL_LOCK_ENTRY_NOT_FOUND_CODE,
				false,
			),
			(
				LockedResyncError::MissingSkillPath,
				SKILL_PATH_NOT_FOUND_CODE,
				false,
			),
			(
				LockedResyncError::NotInstalled,
				"SKILL_NOT_INSTALLED",
				false,
			),
			(
				LockedResyncError::InvalidSkillPath,
				SKILL_PATH_NOT_FOUND_CODE,
				false,
			),
			(
				LockedResyncError::SourceSkillNotFound,
				SKILL_PATH_NOT_FOUND_CODE,
				false,
			),
			(
				LockedResyncError::SourceGroupMismatch,
				SKILL_SOURCE_VIEW_STALE_CODE,
				false,
			),
			(
				LockedResyncError::Fetch(FetchError::BackendUnavailable),
				KEYCHAIN_UNAVAILABLE_CODE,
				false,
			),
			(
				LockedResyncError::Fetch(FetchError::Auth),
				SOURCE_FETCH_FAILED_CODE,
				false,
			),
			(
				LockedResyncError::Fetch(FetchError::Network(
					"conn reset".into(),
				)),
				SOURCE_FETCH_FAILED_CODE,
				false,
			),
			(
				LockedResyncError::Resync(ResyncError::Locked(
					std::io::Error::new(std::io::ErrorKind::WouldBlock, "busy"),
				)),
				"SKILL_MUTATION_LOCK_BUSY",
				true,
			),
			(
				LockedResyncError::Resync(ResyncError::StaleFetch(
					"stale".into(),
				)),
				"SKILL_SOURCE_CHANGED_DURING_FETCH",
				false,
			),
			(
				LockedResyncError::Resync(ResyncError::NotInstalled),
				"SKILL_NOT_INSTALLED",
				false,
			),
			(
				LockedResyncError::Resync(ResyncError::Renamed {
					new_name: "renamed".into(),
				}),
				"SKILL_RENAMED_IN_SOURCE",
				false,
			),
			(
				LockedResyncError::Resync(ResyncError::Parse(
					"parse err".into(),
				)),
				"SKILL_PARSE_FAILED",
				false,
			),
			(
				LockedResyncError::Resync(ResyncError::Conflict(
					"conflict".into(),
				)),
				"SKILL_UPDATE_CONFLICT",
				false,
			),
			(
				LockedResyncError::Resync(ResyncError::OutOfTree(
					"escape".into(),
				)),
				"SKILL_TARGET_OUT_OF_TREE",
				false,
			),
			(
				LockedResyncError::Resync(ResyncError::Hash("hash err".into())),
				"SKILL_SYNC_ERROR",
				false,
			),
			(
				LockedResyncError::Resync(ResyncError::Swap("swap err".into())),
				"SKILL_SYNC_ERROR",
				false,
			),
			(
				LockedResyncError::Resync(ResyncError::LockUpdate(
					"lock err".into(),
				)),
				"SKILL_LOCK_ERROR",
				false,
			),
		];
		for (err, expected_code, expected_retryable) in locked_cases {
			assert_eq!(err.code(), expected_code, "{err:?}.code() mismatch");
			assert_eq!(
				err.retryable(),
				expected_retryable,
				"{err:?}.retryable() mismatch"
			);
		}

		// Table asserting code and retryable for ResyncMutationError
		let mutation_cases = [
			(
				ResyncMutationError::InvalidSkillPath,
				SKILL_PATH_NOT_FOUND_CODE,
				false,
			),
			(
				ResyncMutationError::Resync(ResyncError::Locked(
					std::io::Error::new(std::io::ErrorKind::WouldBlock, "busy"),
				)),
				"SKILL_MUTATION_LOCK_BUSY",
				true,
			),
			(
				ResyncMutationError::Resync(ResyncError::NotInstalled),
				"SKILL_NOT_INSTALLED",
				false,
			),
			(
				ResyncMutationError::SourceChangedDuringFetch,
				"SKILL_SOURCE_CHANGED_DURING_FETCH",
				false,
			),
			(
				ResyncMutationError::SourceMismatch,
				"SKILL_SOURCE_MISMATCH",
				false,
			),
		];
		for (err, expected_code, expected_retryable) in mutation_cases {
			assert_eq!(err.code(), expected_code, "{err:?}.code() mismatch");
			assert_eq!(
				err.retryable(),
				expected_retryable,
				"{err:?}.retryable() mismatch"
			);
		}
	}
}
