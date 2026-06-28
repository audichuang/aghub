//! Sources domain service. Extracted from `crates/api/src/routes/sources.rs`
//! so the API and the CLI share one implementation. Fetch + credentials are
//! injected via [`crate::Fetcher`] / [`crate::TokenResolver`].

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::{FetchError, Fetcher, SourceRef, TokenResolver};
use aghub_core::models::ResourceScope;
use aghub_core::skills::update::{
	compare_known_hashes, detect_rename, precheck_source, SkillUpdateStatus,
	UncheckableReason,
};

#[derive(Clone, Debug)]
pub enum SourceScope {
	Global,
	Project { root: PathBuf },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceScopeKind {
	Global,
	Project,
}

/// Which scope(s) a `list`/`diff`/`sync` invocation targets, before the
/// project root is known. Callers map their flags onto this: `-g` => `Global`,
/// `-p` => `Project`, neither => `All`. The mapper functions below turn it into
/// concrete [`SourceScope`]s, so CLI and API share one scope policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScopeSelector {
	Global,
	Project,
	All,
}

/// Why a [`ScopeSelector`] could not be resolved to a scope. The `Display`
/// strings are the exact CLI `bail!` messages so the end-to-end CLI contract is
/// preserved when these surface through `anyhow`.
#[derive(Debug)]
pub enum ScopeError {
	/// `Project` (or `-p`) was requested but no project root was detected.
	ProjectRootRequired,
	/// `All` (`--all`) is meaningless for a single write scope (`sync`).
	AllNotAllowedForWrite,
	/// `sync` was invoked with no scope flag at all.
	ScopeRequired,
}

impl std::fmt::Display for ScopeError {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			Self::ProjectRootRequired => f.write_str(
				"no project root found (need an agent marker like .claude/, \
				 .opencode/, .mcp.json, …)",
			),
			Self::AllNotAllowedForWrite => f.write_str(
				"`source sync` needs exactly one scope; --all is not allowed",
			),
			Self::ScopeRequired => f.write_str(
				"`source sync` needs a scope: pass -g (global) or -p (project)",
			),
		}
	}
}

impl std::error::Error for ScopeError {}

/// Resolve the read scopes for `list`/`diff`. `Global` => `[Global]`;
/// `Project` => `[Project { root }]` (errors when `project_root` is `None`);
/// `All` => `[Global]` plus the project scope when a root is known. Pure: the
/// caller detects the root and passes it in.
pub fn read_scopes(
	sel: ScopeSelector,
	project_root: Option<PathBuf>,
) -> Result<Vec<SourceScope>, ScopeError> {
	match sel {
		ScopeSelector::Global => Ok(vec![SourceScope::Global]),
		ScopeSelector::Project => match project_root {
			Some(root) => Ok(vec![SourceScope::Project { root }]),
			None => Err(ScopeError::ProjectRootRequired),
		},
		ScopeSelector::All => {
			let mut scopes = vec![SourceScope::Global];
			if let Some(root) = project_root {
				scopes.push(SourceScope::Project { root });
			}
			Ok(scopes)
		}
	}
}

/// Resolve the single write scope for `sync`. Exactly one of `Global`/`Project`
/// is valid; `All` is rejected. Returns the concrete [`SourceScope`] plus a
/// [`SourceScopeKind`] tag the caller maps to its own `ResourceScope`/label.
/// Pure: no IO; the caller passes the detected root.
pub fn write_scope(
	sel: ScopeSelector,
	project_root: Option<PathBuf>,
) -> Result<(SourceScope, SourceScopeKind), ScopeError> {
	match sel {
		ScopeSelector::Global => {
			Ok((SourceScope::Global, SourceScopeKind::Global))
		}
		ScopeSelector::Project => match project_root {
			Some(root) => {
				Ok((SourceScope::Project { root }, SourceScopeKind::Project))
			}
			None => Err(ScopeError::ProjectRootRequired),
		},
		ScopeSelector::All => Err(ScopeError::AllNotAllowedForWrite),
	}
}

#[derive(Clone, Debug)]
pub struct SourceSummary {
	pub source: String,
	pub source_url: String,
	pub source_type: String,
	pub scope: SourceScopeKind,
	pub skill_count: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SourceSkillState {
	NotInstalled,
	InstalledCurrent,
	InstalledOutdated,
	Renamed,
	Removed,
	Deprecated,
	Uncheckable,
}

impl SourceSkillState {
	pub fn as_wire(&self) -> &'static str {
		match self {
			Self::NotInstalled => "notInstalled",
			Self::InstalledCurrent => "installedCurrent",
			Self::InstalledOutdated => "installedOutdated",
			Self::Renamed => "renamed",
			Self::Removed => "removed",
			Self::Deprecated => "deprecated",
			Self::Uncheckable => "uncheckable",
		}
	}
}

#[derive(Clone, Debug)]
pub struct SourceSkillDiff {
	pub name: String,
	pub skill_path: String,
	pub description: Option<String>,
	pub version: Option<String>,
	pub author: Option<String>,
	pub state: SourceSkillState,
	pub previous_name: Option<String>,
	/// Wire reason string (e.g. "noPath", "local"); preserves the DTO `reason`
	/// field and the removed→noPath / uncheckable→reason signals.
	pub reason: Option<String>,
	/// Scope labels where this skill is installed ("global"/"project").
	pub installed_paths: Vec<String>,
}

/// skill_path -> installed baseline metadata.
pub(crate) struct BaselineEntry {
	pub installed_name: String,
	pub stored_hash: String,
	pub local_hashes: Vec<String>,
	pub scope_label: String,
}
pub(crate) type Baseline = BTreeMap<String, BaselineEntry>;

#[derive(Debug)]
pub enum SourceDiffOutcome {
	/// Flat skill list (API-compatible: merged baseline, classified once).
	/// Carries the resolved `git_ref` (query override → recorded ref → None)
	/// so the API response keeps the old recorded-ref fallback.
	Ok {
		git_ref: Option<String>,
		skills: Vec<SourceSkillDiff>,
	},
	/// Resolved `git_ref` (query override -> recorded ref -> None) -- same as
	/// `Ok`, so the API response keeps the recorded-ref fallback on a
	/// credential miss.
	NeedsCredential {
		git_ref: Option<String>,
	},
	FetchFailed,
	/// Local/ssh/unsupported scheme — known before any fetch. Carries the
	/// resolved git_ref too (the old route returned it on the early-out).
	UncheckableSource {
		git_ref: Option<String>,
		reason: UncheckableReason,
	},
}

pub struct SourceListInput {
	pub scopes: Vec<SourceScope>,
}

pub struct SourceDiffInput {
	pub source: String,
	pub git_ref: Option<String>,
	pub scopes: Vec<SourceScope>,
}

/// The injected fetch boundary shared by every Sources flow (`diff_source`,
/// `scan_for_sync`). Both surfaces pass the SAME pair (a `Fetcher` + a
/// `TokenResolver`); only the classification shape downstream differs, so there
/// is one deps type, not one per entry point.
pub struct SourceDeps<'a> {
	pub fetcher: &'a dyn Fetcher,
	pub resolver: &'a dyn TokenResolver,
}

/// Backwards-compatible alias for the unified [`SourceDeps`]. Kept so the API
/// route name reads naturally; new code should use `SourceDeps`.
pub type SourceDiffDeps<'a> = SourceDeps<'a>;
/// Backwards-compatible alias for the unified [`SourceDeps`]. Kept so the CLI
/// sync call site reads naturally; new code should use `SourceDeps`.
pub type SourceSyncDeps<'a> = SourceDeps<'a>;

/// Failure of the shared resolve → precheck → fetch prologue. Both
/// [`diff_source`] and [`scan_for_sync`] map it to their own output shape; it
/// carries the resolved [`ResolvedSourceMeta`] so callers that report the
/// effective ref on a failure (the API) still can.
pub enum FetchPrologueError {
	/// Local/ssh/unsupported scheme — known before any fetch.
	Uncheckable {
		meta: ResolvedSourceMeta,
		reason: UncheckableReason,
	},
	/// Authentication failed (even after a token retry).
	Auth { meta: ResolvedSourceMeta },
	/// Network/transport failure.
	Network { meta: ResolvedSourceMeta },
}

/// The ONE shared prologue every Sources fetch runs: resolve
/// `(source_type, effective_ref)` from the lock entries (no fetch), skip
/// un-fetchable schemes up front, then fetch ONCE (lazily authenticating).
/// Returns the fetched repo plus the resolved meta; both entry points map the
/// outcome onto their own result shape. This is the single resolution +
/// precheck + fetch path the duplicated `diff_source`/`scan_for_sync` prologues
/// collapsed into.
pub fn resolve_precheck_fetch(
	source: &str,
	scopes: &[SourceScope],
	explicit_ref: Option<&str>,
	deps: &SourceDeps<'_>,
) -> Result<(crate::FetchedRepo, ResolvedSourceMeta), FetchPrologueError> {
	let meta = resolve_source_meta(source, scopes, explicit_ref);

	if let Some(reason) = precheck_source(&meta.source_type, source) {
		return Err(FetchPrologueError::Uncheckable { meta, reason });
	}

	let source_ref = SourceRef {
		source: source.to_string(),
		ref_: meta.effective_ref.clone(),
	};
	match fetch_source_with_resolver(&source_ref, deps.fetcher, deps.resolver) {
		Ok(repo) => Ok((repo, meta)),
		Err(FetchError::Auth) => Err(FetchPrologueError::Auth { meta }),
		Err(FetchError::Network) => Err(FetchPrologueError::Network { meta }),
	}
}

pub fn list_sources(input: SourceListInput) -> Vec<SourceSummary> {
	let mut sources = Vec::new();
	for scope in &input.scopes {
		match scope {
			SourceScope::Global => sources.extend(global_sources()),
			SourceScope::Project { root } => {
				sources.extend(project_sources(root))
			}
		}
	}
	sources
}

/// Group the global lock's skills by source (the global lock carries
/// `sourceUrl`).
fn global_sources() -> Vec<SourceSummary> {
	// source (owner/repo) -> (source_url, source_type, count)
	let mut by_source: BTreeMap<String, (String, String, u32)> =
		BTreeMap::new();
	for (_name, entry) in skill::get_all_locked_skills() {
		let agg = by_source.entry(entry.source.clone()).or_insert_with(|| {
			(entry.source_url.clone(), entry.source_type.clone(), 0)
		});
		agg.2 += 1;
	}
	by_source
		.into_iter()
		.map(
			|(source, (source_url, source_type, skill_count))| SourceSummary {
				source,
				source_url,
				source_type,
				scope: SourceScopeKind::Global,
				skill_count,
			},
		)
		.collect()
}

/// Group a project lock's skills by source. The project lock omits `sourceUrl`,
/// so the fetch URL is reconstructed from `owner/repo` (GitHub etc.).
fn project_sources(root: &Path) -> Vec<SourceSummary> {
	let lock = skill::read_local_lock(Some(root));
	// source (owner/repo) -> (source_type, count)
	let mut by_source: BTreeMap<String, (String, u32)> = BTreeMap::new();
	for (_name, entry) in lock.skills {
		let agg = by_source
			.entry(entry.source.clone())
			.or_insert_with(|| (entry.source_type.clone(), 0));
		agg.1 += 1;
	}
	by_source
		.into_iter()
		.map(|(source, (source_type, skill_count))| {
			let source_url = reconstruct_source_url(&source);
			SourceSummary {
				source,
				source_url,
				source_type,
				scope: SourceScopeKind::Project,
				skill_count,
			}
		})
		.collect()
}

fn reconstruct_source_url(source: &str) -> String {
	aghub_git::resolve_remote_source(source)
		.map(|resolved| resolved.clone_url)
		.unwrap_or_else(|_| source.to_string())
}

/// Fetch a source, lazily authenticating. Tries an unauthenticated fetch
/// first; on any error, resolves a token (scoped to the source + keychain host)
/// and retries ONCE with it. If no token is available the original error is
/// returned. The caller maps `Auth`→needs-credential, `Network`→fetch-failed.
pub fn fetch_source_with_resolver(
	source_ref: &SourceRef,
	fetcher: &dyn Fetcher,
	resolver: &dyn TokenResolver,
) -> Result<crate::FetchedRepo, FetchError> {
	match fetcher.fetch(source_ref, None) {
		Ok(repo) => Ok(repo),
		Err(first_error) => {
			let host = crate::keychain_host_for_source(&source_ref.source);
			let Some(token) =
				resolver.resolve(&source_ref.source, host.as_deref())
			else {
				return Err(first_error);
			};
			fetcher.fetch(source_ref, Some(&token))
		}
	}
}

/// Whether a lock entry belongs to the requested source. Matches on the
/// normalized `owner/repo` identifier first, then on a normalized clone URL so
/// global (has `sourceUrl`) and project (reconstructed) scopes match
/// symmetrically — a custom git host normalizes the same on both sides.
fn source_matches(
	want: &str,
	entry_source: &str,
	entry_source_url: Option<&str>,
) -> bool {
	if entry_source == want || entry_source_url == Some(want) {
		return true;
	}
	let want_url = reconstruct_source_url(want);
	reconstruct_source_url(entry_source) == want_url
		|| entry_source_url
			.is_some_and(|u| reconstruct_source_url(u) == want_url)
}

fn local_hashes_for_installed(
	name: &str,
	resource_scope: ResourceScope,
	project_root: Option<&Path>,
) -> Vec<String> {
	crate::installed_skill_roots(name, resource_scope, project_root)
		.into_iter()
		.filter_map(|root| skill::compute_skill_folder_hash(&root).ok())
		.collect()
}

/// Insert one scope's lock entries into a shared baseline. Reused by the merged
/// (API) and single-scope (CLI) baseline builders so the logic stays DRY. On a
/// duplicate `skill_path` the LAST inserted scope wins (the merged path inserts
/// global first, then project — so project shadows global).
fn insert_scope_entries(
	baseline: &mut Baseline,
	source_type: &mut String,
	recorded_ref: &mut Option<String>,
	scope: &SourceScope,
	want: &str,
) {
	match scope {
		SourceScope::Global => {
			for (name, entry) in skill::get_all_locked_skills() {
				if !source_matches(want, &entry.source, Some(&entry.source_url))
				{
					continue;
				}
				if source_type.is_empty() {
					*source_type = entry.source_type.clone();
				}
				if recorded_ref.is_none() {
					*recorded_ref = entry.ref_name.clone();
				}
				if let Some(skill_path) = entry.skill_path.clone() {
					let hash = entry.content_hash.clone().unwrap_or_default();
					let local_hashes = local_hashes_for_installed(
						&name,
						ResourceScope::GlobalOnly,
						None,
					);
					baseline.insert(
						skill_path,
						BaselineEntry {
							installed_name: name,
							stored_hash: hash,
							local_hashes,
							scope_label: "global".to_string(),
						},
					);
				}
			}
		}
		SourceScope::Project { root } => {
			for (name, entry) in skill::read_local_lock(Some(root)).skills {
				if !source_matches(want, &entry.source, None) {
					continue;
				}
				if source_type.is_empty() {
					*source_type = entry.source_type.clone();
				}
				if recorded_ref.is_none() {
					*recorded_ref = entry.ref_name.clone();
				}
				if let Some(skill_path) = entry.skill_path.clone() {
					let local_hashes = local_hashes_for_installed(
						&name,
						ResourceScope::ProjectOnly,
						Some(root),
					);
					baseline.insert(
						skill_path,
						BaselineEntry {
							installed_name: name,
							stored_hash: entry.computed_hash,
							local_hashes,
							scope_label: "project".to_string(),
						},
					);
				}
			}
		}
	}
}

/// API path: merge global + every project scope into ONE baseline keyed by
/// `skill_path` (project shadows global on a duplicate), and classify once.
/// Returns `(baseline, source_type, recorded_ref)`.
pub(crate) fn merged_baseline_for_source(
	scopes: &[SourceScope],
	source: &str,
) -> (Baseline, String, Option<String>) {
	let mut baseline = Baseline::new();
	let mut source_type = String::new();
	let mut recorded_ref: Option<String> = None;
	let want = source.trim();
	// Global first, then project, so project entries shadow global on a
	// duplicate `skill_path` (mirrors the old route's insert order).
	for scope in scopes {
		if matches!(scope, SourceScope::Global) {
			insert_scope_entries(
				&mut baseline,
				&mut source_type,
				&mut recorded_ref,
				scope,
				want,
			);
		}
	}
	for scope in scopes {
		if matches!(scope, SourceScope::Project { .. }) {
			insert_scope_entries(
				&mut baseline,
				&mut source_type,
				&mut recorded_ref,
				scope,
				want,
			);
		}
	}
	(baseline, source_type, recorded_ref)
}

/// CLI path: build the baseline for a SINGLE scope only.
/// Returns `(baseline, source_type, recorded_ref)`.
pub(crate) fn baseline_for_scope(
	scope: &SourceScope,
	source: &str,
) -> (Baseline, String, Option<String>) {
	let mut baseline = Baseline::new();
	let mut source_type = String::new();
	let mut recorded_ref: Option<String> = None;
	insert_scope_entries(
		&mut baseline,
		&mut source_type,
		&mut recorded_ref,
		scope,
		source.trim(),
	);
	(baseline, source_type, recorded_ref)
}

/// Discover only the recorded `source_type` + `ref_name` for a source across
/// the given scopes, WITHOUT building a baseline (no folder hashing, no fetch).
/// Mirrors the merged-baseline scan order (global first, then project) so the
/// "first non-empty wins" result matches [`merged_baseline_for_source`].
/// Returns `(source_type, recorded_ref)` — both empty/None when the source is
/// not present in any lock.
fn recorded_meta_for_source(
	scopes: &[SourceScope],
	source: &str,
) -> (String, Option<String>) {
	let mut source_type = String::new();
	let mut recorded_ref: Option<String> = None;
	let want = source.trim();
	let mut visit = |entry_source: &str,
	                 entry_source_url: Option<&str>,
	                 entry_source_type: &str,
	                 entry_ref: Option<&str>| {
		if !source_matches(want, entry_source, entry_source_url) {
			return;
		}
		if source_type.is_empty() {
			source_type = entry_source_type.to_string();
		}
		if recorded_ref.is_none() {
			recorded_ref = entry_ref.map(str::to_string);
		}
	};
	// Global first, then project — same order as `merged_baseline_for_source`.
	for scope in scopes {
		if matches!(scope, SourceScope::Global) {
			for (_name, entry) in skill::get_all_locked_skills() {
				visit(
					&entry.source,
					Some(&entry.source_url),
					&entry.source_type,
					entry.ref_name.as_deref(),
				);
			}
		}
	}
	for scope in scopes {
		if let SourceScope::Project { root } = scope {
			for (_name, entry) in skill::read_local_lock(Some(root)).skills {
				visit(
					&entry.source,
					None,
					&entry.source_type,
					entry.ref_name.as_deref(),
				);
			}
		}
	}
	(source_type, recorded_ref)
}

/// The resolved fetch metadata for a source, derived from the lock entries
/// (NO fetch). Lets the CLI — which fetches once itself — agree with the API
/// `diff_source` on `(source_type, effective_ref)` BEFORE the single fetch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedSourceMeta {
	/// The lock's recorded source type, defaulted to `"github"` when empty —
	/// the same default `diff_source` applies before its `precheck_source`.
	pub source_type: String,
	/// Explicit `--ref` override, else the source's recorded lock ref, else
	/// `None` (the upstream default branch).
	pub effective_ref: Option<String>,
}

/// Resolve `(source_type, effective_ref)` for a source across `scopes` from the
/// lock entries, with NO fetch. This is the SINGLE resolution path: the API
/// [`diff_source`] calls it internally, and the CLI calls it before its own
/// fetch so both surfaces resolve identically.
///
/// `effective_ref = explicit_ref OR recorded lock ref OR None`; `source_type`
/// is the lock's recorded type, defaulted to `"github"` when empty.
pub fn resolve_source_meta(
	source: &str,
	scopes: &[SourceScope],
	explicit_ref: Option<&str>,
) -> ResolvedSourceMeta {
	let (mut source_type, recorded_ref) =
		recorded_meta_for_source(scopes, source);
	if source_type.is_empty() {
		source_type = "github".to_string();
	}
	let effective_ref = explicit_ref.map(str::to_string).or(recorded_ref);
	ResolvedSourceMeta {
		source_type,
		effective_ref,
	}
}

/// Internal: classify discovered repo skills against a prebuilt baseline.
/// `Baseline`/`BaselineEntry` stay `pub(crate)` so they never leak across the
/// crate boundary; cross-crate callers use [`classify_scope`] / [`diff_source`].
///
/// Discovery (`skill::discover_repo_skills`) happens here so callers pass only
/// `root`. A repo with no discoverable skills yields the baseline-only
/// `removed` rows (matching the old route's empty-discovery early-return, which
/// produced an empty diff before the changelog/removed pass ran on an empty
/// discovered set).
pub(crate) fn classify_repo_skills(
	root: &Path,
	baseline: &Baseline,
) -> Vec<SourceSkillDiff> {
	let Ok(discovered) = skill::discover_repo_skills(root, &[], true) else {
		// No SKILL.md in the repo → nothing to offer (old route returned an
		// empty diff, bypassing the removed/changelog pass).
		return Vec::new();
	};
	build_source_skill_diffs(root, discovered, baseline)
}

fn build_source_skill_diffs(
	root: &Path,
	discovered: Vec<skill::RepoDiscoveredSkill>,
	baseline: &Baseline,
) -> Vec<SourceSkillDiff> {
	use std::collections::BTreeSet;
	let mut out = Vec::with_capacity(discovered.len() + baseline.len());
	let mut seen_paths = BTreeSet::new();
	let mut indexes_by_name = BTreeMap::new();
	// Frontmatter names are not unique across skill paths; a name seen more
	// than once is ambiguous and must not be used as a rename redirect target.
	let mut ambiguous_names = BTreeSet::new();
	for d in discovered {
		let skill_path = skill::lock_skill_file_path(&d.relative_dir);
		seen_paths.insert(skill_path.clone());
		let (description, version, author) = parse_meta(&d.full_path);
		let name = d.name;

		match baseline.get(&skill_path) {
			None => {
				let (state, reason) = if is_deprecated_skill_path(&skill_path) {
					(SourceSkillState::Deprecated, None)
				} else {
					(SourceSkillState::NotInstalled, None)
				};
				if indexes_by_name.insert(name.clone(), out.len()).is_some() {
					ambiguous_names.insert(name.clone());
				}
				out.push(SourceSkillDiff {
					name,
					skill_path,
					description,
					version,
					author,
					state,
					previous_name: None,
					reason,
					installed_paths: Vec::new(),
				});
			}
			Some(entry) => {
				let skill_dir =
					aghub_core::skills::skill_source_root(&d.full_path);
				let (state, previous_name, reason) =
					classify_source_skill_diff(entry, &name, &skill_dir);
				if indexes_by_name.insert(name.clone(), out.len()).is_some() {
					ambiguous_names.insert(name.clone());
				}
				out.push(SourceSkillDiff {
					name,
					skill_path,
					description,
					version,
					author,
					state,
					previous_name,
					reason,
					installed_paths: vec![entry.scope_label.clone()],
				});
			}
		}
	}

	let successors = skill_successors_from_changelog(root);
	for (skill_path, entry) in baseline {
		if seen_paths.contains(skill_path) {
			continue;
		}
		if let Some(new_name) = successors.get(&entry.installed_name) {
			if !ambiguous_names.contains(new_name) {
				if let Some(index) = indexes_by_name.get(new_name) {
					let diff = &mut out[*index];
					// Only claim a not-yet-installed, non-deprecated successor.
					// A deprecated target keeps its signal; an already-installed
					// target means the old name is a stale leftover (→ removed).
					if diff.installed_paths.is_empty()
						&& diff.state != SourceSkillState::Deprecated
					{
						diff.state = SourceSkillState::Renamed;
						diff.previous_name = Some(entry.installed_name.clone());
						diff.reason = None;
						diff.installed_paths = vec![entry.scope_label.clone()];
						continue;
					}
				}
			}
		}
		out.push(SourceSkillDiff {
			name: entry.installed_name.clone(),
			skill_path: skill_path.clone(),
			description: None,
			version: None,
			author: None,
			state: SourceSkillState::Removed,
			previous_name: None,
			reason: Some("noPath".to_string()),
			installed_paths: vec![entry.scope_label.clone()],
		});
	}

	out
}

fn skill_successors_from_changelog(root: &Path) -> BTreeMap<String, String> {
	let Ok(content) = std::fs::read_to_string(root.join("CHANGELOG.md")) else {
		return BTreeMap::new();
	};
	let mut successors = BTreeMap::new();
	for line in content.lines() {
		for (old_name, new_name) in skill_renames_in_line(line) {
			successors.entry(old_name).or_insert(new_name);
		}
	}
	successors
}

/// Extract `(old, new)` skill-rename pairs from a single changelog line.
///
/// Anchored on the `rename`/`replace` verb and its connective, not on the last
/// two backtick terms, so trailing tokens (commit/PR refs, parentheticals)
/// cannot hijack the new name and multiple renames on one line are all
/// captured. For each verb occurrence we search only within its clause (up to
/// the next `;`/`.`) for the connective `… to …` / `… with …`; OLD is the
/// backtick term that lies between the verb and the connective, NEW is the
/// first backtick term opening at/after the connective; direction is fixed
/// (old → new). A verb or connective that falls inside a backtick term, or a
/// clause whose terms do not bracket the connective, yields nothing — so a
/// connective appearing inside an unrelated backticked token (`` `x to y` ``)
/// or in a different clause cannot fabricate a pair. This mirrors the upstream
/// "Rename the `X` skill to `Y`" changelog convention; the less common
/// "`X` was renamed to `Y`" phrasing degrades to no-pair (the lock entry is
/// then reported `removed` rather than a wrong rename). Spurious pairs are
/// further filtered by the caller, which only applies a successor whose OLD
/// matches a removed lock entry and whose NEW resolves to a discovered skill.
fn skill_renames_in_line(line: &str) -> Vec<(String, String)> {
	let lower = line.to_ascii_lowercase();
	let spans = backtick_spans(line);
	if spans.len() < 2 {
		return Vec::new();
	}
	// `to_ascii_lowercase` preserves byte length, so positions in `lower` line
	// up with byte positions in `line` / `spans`. Backticks/connectives are
	// ASCII, so every index used here is a UTF-8 char boundary.
	let inside_span =
		|pos: usize| spans.iter().any(|(s, e, _)| *s < pos && pos < *e);
	let mut out = Vec::new();
	for (verb, connective) in [("rename", " to "), ("replace", " with ")] {
		let mut from = 0;
		while let Some(rel) = lower[from..].find(verb) {
			let verb_start = from + rel;
			let after_verb = verb_start + verb.len();
			from = after_verb;
			if inside_span(verb_start) {
				continue;
			}
			// Bound the connective search to this clause so a connective in a
			// later clause cannot bind to this verb.
			let clause_end = lower[after_verb..]
				.find([';', '.'])
				.map(|r| after_verb + r)
				.unwrap_or(lower.len());
			let Some(conn_rel) = lower[after_verb..clause_end].find(connective)
			else {
				continue;
			};
			let conn_start = after_verb + conn_rel;
			let conn_end = conn_start + connective.len();
			if inside_span(conn_start) {
				continue;
			}
			let old = spans
				.iter()
				.rfind(|(s, e, _)| *s >= verb_start && *e <= conn_start);
			let new = spans.iter().find(|(s, _, _)| *s >= conn_end);
			if let (Some((_, _, old_name)), Some((_, _, new_name))) = (old, new)
			{
				if old_name != new_name {
					out.push((old_name.clone(), new_name.clone()));
				}
			}
		}
	}
	out
}

/// Byte spans `(start, end, term)` for each backtick-delimited term, where
/// `start` is the opening backtick index and `end` is one past the closing
/// backtick. Empty/whitespace-only terms are skipped.
fn backtick_spans(line: &str) -> Vec<(usize, usize, String)> {
	let mut spans = Vec::new();
	let mut i = 0;
	while i < line.len() {
		if line.as_bytes()[i] == b'`' {
			let Some(rel) = line[i + 1..].find('`') else {
				break;
			};
			let close = i + 1 + rel;
			let term = line[i + 1..close].trim();
			if !term.is_empty() {
				spans.push((i, close + 1, term.to_string()));
			}
			i = close + 1;
		} else {
			i += 1;
		}
	}
	spans
}

fn is_deprecated_skill_path(skill_path: &str) -> bool {
	skill_path.split('/').any(|part| part == "deprecated")
}

fn classify_source_skill_diff(
	entry: &BaselineEntry,
	discovered_name: &str,
	skill_dir: &Path,
) -> (SourceSkillState, Option<String>, Option<String>) {
	if detect_rename(discovered_name, &entry.installed_name).is_some() {
		return (
			SourceSkillState::Renamed,
			Some(entry.installed_name.clone()),
			None,
		);
	}
	let (state, reason) = classify_installed(entry, skill_dir);
	(state, None, reason)
}

/// Classify an already-installed skill by comparing its upstream folder hash to
/// the installed baseline. Prefer actual installed folder hashes over the
/// stored lock hash because some locks were produced by npx/JS collation, while
/// this endpoint hashes fetched source with Rust collation. Comparing local
/// Rust hashes to fetched Rust hash avoids false updates for unchanged skills.
fn classify_installed(
	entry: &BaselineEntry,
	skill_dir: &Path,
) -> (SourceSkillState, Option<String>) {
	let fresh = match skill::compute_skill_folder_hash(skill_dir) {
		Ok(hash) => hash,
		Err(_) => {
			return (SourceSkillState::Uncheckable, Some("local".to_string()))
		}
	};

	if !entry.local_hashes.is_empty() {
		if entry.local_hashes.iter().all(|hash| {
			compare_known_hashes(hash, &fresh) == SkillUpdateStatus::UpToDate
		}) {
			return (SourceSkillState::InstalledCurrent, None);
		}
		return (SourceSkillState::InstalledOutdated, None);
	}

	let baseline = if entry.stored_hash.is_empty()
		|| skill::is_placeholder_digest(&entry.stored_hash)
	{
		None
	} else {
		Some(entry.stored_hash.as_str())
	};
	let Some(base_hash) = baseline else {
		return (SourceSkillState::InstalledCurrent, None);
	};

	// The rename path short-circuits in `classify_source_skill_diff` before
	// reaching this function (it inspects `entry.installed_name` vs the
	// discovered name up front). `compare_known_hashes` itself never returns
	// `Renamed` — it only yields `UpToDate` or `UpdateAvailable` — so the
	// `Renamed` arm is unreachable here and intentionally omitted.
	match compare_known_hashes(base_hash, &fresh) {
		SkillUpdateStatus::UpToDate => {
			(SourceSkillState::InstalledCurrent, None)
		}
		SkillUpdateStatus::UpdateAvailable { .. } => {
			(SourceSkillState::InstalledOutdated, None)
		}
		SkillUpdateStatus::Uncheckable { reason } => {
			(SourceSkillState::Uncheckable, Some(reason_str(reason)))
		}
		SkillUpdateStatus::Renamed { .. } => unreachable!(
			"compare_known_hashes cannot return Renamed; rename detection \
			 happens in classify_source_skill_diff before this match"
		),
	}
}

fn parse_meta(
	skill_md: &Path,
) -> (Option<String>, Option<String>, Option<String>) {
	match skill::parse(skill_md) {
		Ok(s) => (
			Some(s.description).filter(|d| !d.is_empty()),
			s.version,
			s.author,
		),
		Err(_) => (None, None, None),
	}
}

fn reason_str(reason: UncheckableReason) -> String {
	match reason {
		UncheckableReason::Auth => "auth",
		UncheckableReason::Network => "network",
		UncheckableReason::Local => "local",
		UncheckableReason::Ssh => "ssh",
		UncheckableReason::UnsupportedScheme => "unsupportedScheme",
		UncheckableReason::NoPath => "noPath",
		UncheckableReason::Timeout => "timeout",
	}
	.to_string()
}

/// PUBLIC CLI entry: build the baseline for one scope and classify the fetched
/// repo against it. Does NOT fetch (caller passes the already-fetched `root`),
/// so the CLI reuses one `FetchedRepo` for every scope and for install.
pub fn classify_scope(
	root: &Path,
	scope: &SourceScope,
	source: &str,
) -> Vec<SourceSkillDiff> {
	let (baseline, _src_type, _ref) = baseline_for_scope(scope, source);
	classify_repo_skills(root, &baseline)
}

/// PUBLIC API entry: merged-baseline, single-classification, flat output —
/// byte-identical to the old route. Fetches internally via `deps`.
pub fn diff_source(
	input: SourceDiffInput,
	deps: SourceDiffDeps<'_>,
) -> SourceDiffOutcome {
	let source = input.source.trim().to_string();
	let (baseline, _src_type, _recorded_ref) =
		merged_baseline_for_source(&input.scopes, &source);

	// Resolve → precheck → fetch via the ONE shared prologue (the SAME path
	// `scan_for_sync` runs). `effective_ref` = explicit query ref, else the
	// source's RECORDED ref, else None (the repo default branch). The prologue
	// carries the resolved meta back on every failure so the recorded-ref
	// fallback survives a credential miss / uncheckable early-out.
	match resolve_precheck_fetch(
		&source,
		&input.scopes,
		input.git_ref.as_deref(),
		&deps,
	) {
		Ok((repo, meta)) => SourceDiffOutcome::Ok {
			git_ref: meta.effective_ref,
			skills: classify_repo_skills(repo.root.as_path(), &baseline),
		},
		Err(FetchPrologueError::Uncheckable { meta, reason }) => {
			SourceDiffOutcome::UncheckableSource {
				git_ref: meta.effective_ref,
				reason,
			}
		}
		Err(FetchPrologueError::Auth { meta }) => {
			SourceDiffOutcome::NeedsCredential {
				git_ref: meta.effective_ref,
			}
		}
		Err(FetchPrologueError::Network { .. }) => {
			SourceDiffOutcome::FetchFailed
		}
	}
}

/// The read-only result of [`scan_for_sync`]: the fetched repo (reused for
/// every install/update) plus the classified per-skill diffs for the one write
/// scope, and the resolved fetch coordinate the caller records on the lock.
pub struct SyncScan {
	pub repo: crate::FetchedRepo,
	pub diffs: Vec<SourceSkillDiff>,
	pub git_ref: Option<String>,
	pub source_type: String,
}

/// Why [`scan_for_sync`] could not produce a [`SyncScan`]. Mirrors the
/// pre-fetch/fetch failure shapes the CLI bails on: `Uncheckable` (local/ssh/
/// unsupported scheme, known before any fetch), `NeedsCredential` (auth
/// failure), `FetchFailed` (network/transport).
#[derive(Debug)]
pub enum SyncScanError {
	Uncheckable(UncheckableReason),
	NeedsCredential,
	FetchFailed,
}

/// PUBLIC CLI entry for `sync`: resolve the fetch coordinate, precheck, fetch
/// ONCE, and classify against the single write `scope` — the read-only prologue
/// of `source sync`. The caller plans + applies (install/lock-writing stays in
/// core behind `ConfigManager`); this never touches lock files or the `.agents`
/// layout. Symmetric to [`diff_source`], but single-scope and non-merged.
pub fn scan_for_sync(
	source: &str,
	git_ref: Option<&str>,
	scope: &SourceScope,
	deps: SourceSyncDeps<'_>,
) -> Result<SyncScan, SyncScanError> {
	let source = source.trim();

	// Resolve → precheck → fetch via the ONE shared prologue (the SAME path
	// `diff_source` runs) so sync fetches/installs from the recorded ref — not
	// the default branch — and prechecks with the recorded source_type rather
	// than a hard-coded "github".
	let (repo, meta) = resolve_precheck_fetch(
		source,
		std::slice::from_ref(scope),
		git_ref,
		&deps,
	)
	.map_err(|e| match e {
		FetchPrologueError::Uncheckable { reason, .. } => {
			SyncScanError::Uncheckable(reason)
		}
		FetchPrologueError::Auth { .. } => SyncScanError::NeedsCredential,
		FetchPrologueError::Network { .. } => SyncScanError::FetchFailed,
	})?;

	let diffs = classify_scope(repo.root.as_path(), scope, source);
	Ok(SyncScan {
		repo,
		diffs,
		git_ref: meta.effective_ref,
		source_type: meta.source_type,
	})
}

#[cfg(test)]
mod list_tests {
	use super::*;
	#[test]
	fn list_sources_global_only_all_global_scope() {
		let out = list_sources(SourceListInput {
			scopes: vec![SourceScope::Global],
		});
		assert!(out.iter().all(|s| s.scope == SourceScopeKind::Global));
	}
}

#[cfg(test)]
mod classify_tests {
	use super::*;
	use std::fs;
	use tempfile::tempdir;

	fn write_skill(root: &Path, relative_dir: &str, name: &str) {
		let dir = root.join(relative_dir);
		fs::create_dir_all(&dir).unwrap();
		fs::write(
			dir.join("SKILL.md"),
			format!("---\nname: {name}\ndescription: {name} skill\n---\n"),
		)
		.unwrap();
	}

	#[test]
	fn classify_prefers_local_hash_over_stale_stored_hash() {
		let dir = tempdir().unwrap();
		fs::write(dir.path().join("SKILL.md"), b"description: x").unwrap();
		let fresh = skill::compute_skill_folder_hash(dir.path()).unwrap();
		let entry = BaselineEntry {
			installed_name: "skill".to_string(),
			stored_hash: "stale-lock-hash".to_string(),
			local_hashes: vec![fresh],
			scope_label: "project".to_string(),
		};

		assert_eq!(
			classify_installed(&entry, dir.path()),
			(SourceSkillState::InstalledCurrent, None)
		);
	}

	#[test]
	fn classify_falls_back_to_stored_hash_without_local_hash() {
		let dir = tempdir().unwrap();
		fs::write(dir.path().join("SKILL.md"), b"description: x").unwrap();
		let entry = BaselineEntry {
			installed_name: "skill".to_string(),
			stored_hash: "stale-lock-hash".to_string(),
			local_hashes: Vec::new(),
			scope_label: "project".to_string(),
		};

		assert_eq!(
			classify_installed(&entry, dir.path()),
			(SourceSkillState::InstalledOutdated, None)
		);
	}

	#[test]
	fn classify_unknown_lock_hash_as_current() {
		let dir = tempdir().unwrap();
		fs::write(dir.path().join("SKILL.md"), b"description: x").unwrap();
		let entry = BaselineEntry {
			installed_name: "skill".to_string(),
			stored_hash: skill::EMPTY_SKILLS_LOCK_DIGEST.to_string(),
			local_hashes: Vec::new(),
			scope_label: "project".to_string(),
		};

		assert_eq!(
			classify_installed(&entry, dir.path()),
			(SourceSkillState::InstalledCurrent, None)
		);
	}

	#[test]
	fn classify_outdated_when_any_installed_hash_differs() {
		let dir = tempdir().unwrap();
		fs::write(dir.path().join("SKILL.md"), b"description: x").unwrap();
		let fresh = skill::compute_skill_folder_hash(dir.path()).unwrap();
		let entry = BaselineEntry {
			installed_name: "skill".to_string(),
			stored_hash: fresh.clone(),
			local_hashes: vec![fresh, "older-install".to_string()],
			scope_label: "project".to_string(),
		};

		assert_eq!(
			classify_installed(&entry, dir.path()),
			(SourceSkillState::InstalledOutdated, None)
		);
	}

	#[test]
	fn classify_source_diff_reports_renamed_before_hash_compare() {
		let dir = tempdir().unwrap();
		fs::write(dir.path().join("SKILL.md"), b"description: x").unwrap();
		let entry = BaselineEntry {
			installed_name: "old-skill".to_string(),
			stored_hash: "stale-lock-hash".to_string(),
			local_hashes: Vec::new(),
			scope_label: "project".to_string(),
		};

		assert_eq!(
			classify_source_skill_diff(&entry, "new-skill", dir.path()),
			(
				SourceSkillState::Renamed,
				Some("old-skill".to_string()),
				None
			)
		);
	}

	#[test]
	fn source_diff_reports_locked_skill_removed_when_upstream_path_disappears()
	{
		let dir = tempdir().unwrap();
		let skill_dir = dir.path().join("skills/engineering/diagnosing-bugs");
		fs::create_dir_all(&skill_dir).unwrap();
		let skill_file = skill_dir.join("SKILL.md");
		fs::write(
			&skill_file,
			b"---\nname: diagnosing-bugs\ndescription: new name\n---\n",
		)
		.unwrap();

		let mut baseline = Baseline::new();
		baseline.insert(
			"skills/engineering/diagnose/SKILL.md".to_string(),
			BaselineEntry {
				installed_name: "diagnose".to_string(),
				stored_hash: "old-hash".to_string(),
				local_hashes: Vec::new(),
				scope_label: "global".to_string(),
			},
		);

		let diffs = build_source_skill_diffs(
			dir.path(),
			vec![skill::RepoDiscoveredSkill {
				name: "diagnosing-bugs".to_string(),
				full_path: skill_file,
				relative_dir: "skills/engineering/diagnosing-bugs".to_string(),
			}],
			&baseline,
		);

		let removed = diffs
			.iter()
			.find(|diff| {
				diff.skill_path == "skills/engineering/diagnose/SKILL.md"
			})
			.expect("removed lock entry should be present");
		assert_eq!(removed.name, "diagnose");
		assert_eq!(removed.state, SourceSkillState::Removed);
		assert_eq!(removed.reason.as_deref(), Some("noPath"));
		assert_eq!(removed.installed_paths, vec!["global".to_string()]);

		assert!(diffs.iter().any(|diff| {
			diff.skill_path == "skills/engineering/diagnosing-bugs/SKILL.md"
				&& diff.state == SourceSkillState::NotInstalled
		}));
	}

	#[test]
	fn source_diff_marks_deprecated_repo_skills_separately() {
		let dir = tempdir().unwrap();
		let skill_dir = dir.path().join("skills/deprecated/qa");
		fs::create_dir_all(&skill_dir).unwrap();
		let skill_file = skill_dir.join("SKILL.md");
		fs::write(&skill_file, b"---\nname: qa\ndescription: old\n---\n")
			.unwrap();

		let diffs = build_source_skill_diffs(
			dir.path(),
			vec![skill::RepoDiscoveredSkill {
				name: "qa".to_string(),
				full_path: skill_file,
				relative_dir: "skills/deprecated/qa".to_string(),
			}],
			&Baseline::new(),
		);

		assert_eq!(diffs.len(), 1);
		assert_eq!(diffs[0].name, "qa");
		assert_eq!(diffs[0].state, SourceSkillState::Deprecated);
		assert_eq!(diffs[0].skill_path, "skills/deprecated/qa/SKILL.md");
	}

	#[test]
	fn source_diff_uses_changelog_to_report_moved_skill_as_renamed() {
		let dir = tempdir().unwrap();
		fs::write(
				dir.path().join("CHANGELOG.md"),
				"- [`47bde84`](https://github.com/mattpocock/skills/commit/47bde84) \
				 Thanks - Rename the **`diagnose`** skill to \
				 **`diagnosing-bugs`**.",
			)
			.unwrap();
		let skill_dir = dir.path().join("skills/engineering/diagnosing-bugs");
		fs::create_dir_all(&skill_dir).unwrap();
		let skill_file = skill_dir.join("SKILL.md");
		fs::write(
			&skill_file,
			b"---\nname: diagnosing-bugs\ndescription: new name\n---\n",
		)
		.unwrap();

		let mut baseline = Baseline::new();
		baseline.insert(
			"skills/engineering/diagnose/SKILL.md".to_string(),
			BaselineEntry {
				installed_name: "diagnose".to_string(),
				stored_hash: "old-hash".to_string(),
				local_hashes: Vec::new(),
				scope_label: "global".to_string(),
			},
		);

		let diffs = build_source_skill_diffs(
			dir.path(),
			vec![skill::RepoDiscoveredSkill {
				name: "diagnosing-bugs".to_string(),
				full_path: skill_file,
				relative_dir: "skills/engineering/diagnosing-bugs".to_string(),
			}],
			&baseline,
		);

		let renamed = diffs
			.iter()
			.find(|diff| diff.name == "diagnosing-bugs")
			.expect("new skill should be present");
		assert_eq!(renamed.state, SourceSkillState::Renamed);
		assert_eq!(renamed.previous_name.as_deref(), Some("diagnose"));
		assert_eq!(renamed.installed_paths, vec!["global".to_string()]);
		assert!(!diffs.iter().any(|diff| {
			diff.skill_path == "skills/engineering/diagnose/SKILL.md"
		}));
	}

	#[test]
	fn source_diff_uses_changelog_to_report_replaced_skill_as_renamed() {
		let dir = tempdir().unwrap();
		fs::write(
				dir.path().join("CHANGELOG.md"),
				"- [`47bde84`](https://github.com/mattpocock/skills/commit/47bde84) \
				 Thanks - Replace **`write-a-skill`** with \
				 **`writing-great-skills`**.",
			)
			.unwrap();
		let skill_dir =
			dir.path().join("skills/productivity/writing-great-skills");
		fs::create_dir_all(&skill_dir).unwrap();
		let skill_file = skill_dir.join("SKILL.md");
		fs::write(
			&skill_file,
			b"---\nname: writing-great-skills\ndescription: new skill\n---\n",
		)
		.unwrap();

		let mut baseline = Baseline::new();
		baseline.insert(
			"skills/productivity/write-a-skill/SKILL.md".to_string(),
			BaselineEntry {
				installed_name: "write-a-skill".to_string(),
				stored_hash: "old-hash".to_string(),
				local_hashes: Vec::new(),
				scope_label: "global".to_string(),
			},
		);

		let diffs = build_source_skill_diffs(
			dir.path(),
			vec![skill::RepoDiscoveredSkill {
				name: "writing-great-skills".to_string(),
				full_path: skill_file,
				relative_dir: "skills/productivity/writing-great-skills"
					.to_string(),
			}],
			&baseline,
		);

		let renamed = diffs
			.iter()
			.find(|diff| diff.name == "writing-great-skills")
			.expect("replacement skill should be present");
		assert_eq!(renamed.state, SourceSkillState::Renamed);
		assert_eq!(renamed.previous_name.as_deref(), Some("write-a-skill"));
		assert!(!diffs.iter().any(|diff| {
			diff.skill_path == "skills/productivity/write-a-skill/SKILL.md"
		}));
	}

	#[test]
	fn changelog_rename_ignores_trailing_backtick_tokens() {
		// A trailing PR/commit ref in backticks must NOT be mistaken for the
		// new name. Old/new are anchored on the connective, not the last two
		// backtick terms.
		let dir = tempdir().unwrap();
		fs::write(
			dir.path().join("CHANGELOG.md"),
			"- Rename the `diagnose` skill to `diagnosing-bugs` (see `#123`).",
		)
		.unwrap();

		let successors = skill_successors_from_changelog(dir.path());

		assert_eq!(
			successors.get("diagnose").map(String::as_str),
			Some("diagnosing-bugs"),
			"connective-anchored parse should map diagnose -> diagnosing-bugs"
		);
		assert!(
			!successors.contains_key("diagnosing-bugs"),
			"trailing `#123` must not become a successor target"
		);
	}

	#[test]
	fn changelog_rename_handles_multiple_pairs_on_one_line() {
		// Two renames on one line: both must be captured, not just the last.
		let dir = tempdir().unwrap();
		fs::write(
			dir.path().join("CHANGELOG.md"),
			"- Rename `alpha` to `alpha-two`; rename `beta` to `beta-two`.",
		)
		.unwrap();

		let successors = skill_successors_from_changelog(dir.path());

		assert_eq!(
			successors.get("alpha").map(String::as_str),
			Some("alpha-two")
		);
		assert_eq!(
			successors.get("beta").map(String::as_str),
			Some("beta-two")
		);
	}

	#[test]
	fn build_diffs_skips_rename_when_target_name_is_ambiguous() {
		// Two discovered skills share a frontmatter name; a CHANGELOG rename
		// pointing at that name must NOT silently redirect onto an arbitrary
		// one of them. The removed lock entry is reported honestly instead.
		let dir = tempdir().unwrap();
		fs::write(
			dir.path().join("CHANGELOG.md"),
			"- Rename `legacy` to `shared`.",
		)
		.unwrap();
		write_skill(dir.path(), "skills/a/shared", "shared");
		write_skill(dir.path(), "skills/b/shared", "shared");

		let mut baseline = Baseline::new();
		baseline.insert(
			"skills/legacy/SKILL.md".to_string(),
			BaselineEntry {
				installed_name: "legacy".to_string(),
				stored_hash: "old-hash".to_string(),
				local_hashes: Vec::new(),
				scope_label: "global".to_string(),
			},
		);

		let diffs = build_source_skill_diffs(
			dir.path(),
			vec![
				skill::RepoDiscoveredSkill {
					name: "shared".to_string(),
					full_path: dir.path().join("skills/a/shared/SKILL.md"),
					relative_dir: "skills/a/shared".to_string(),
				},
				skill::RepoDiscoveredSkill {
					name: "shared".to_string(),
					full_path: dir.path().join("skills/b/shared/SKILL.md"),
					relative_dir: "skills/b/shared".to_string(),
				},
			],
			&baseline,
		);

		assert!(
			!diffs
				.iter()
				.any(|diff| diff.state == SourceSkillState::Renamed),
			"ambiguous successor name must not produce a rename redirect"
		);
		let removed = diffs
			.iter()
			.find(|diff| diff.skill_path == "skills/legacy/SKILL.md")
			.expect("removed lock entry should be present");
		assert_eq!(removed.state, SourceSkillState::Removed);
		assert_eq!(removed.name, "legacy");
	}

	#[test]
	fn build_diffs_does_not_overwrite_deprecated_with_rename() {
		// A CHANGELOG successor that lands on a skill living under a
		// `deprecated/` path must keep the deprecated signal; the old lock
		// entry is then reported as removed rather than renamed.
		let dir = tempdir().unwrap();
		fs::write(
			dir.path().join("CHANGELOG.md"),
			"- Rename `old-qa` to `qa`.",
		)
		.unwrap();
		write_skill(dir.path(), "skills/deprecated/qa", "qa");

		let mut baseline = Baseline::new();
		baseline.insert(
			"skills/engineering/old-qa/SKILL.md".to_string(),
			BaselineEntry {
				installed_name: "old-qa".to_string(),
				stored_hash: "old-hash".to_string(),
				local_hashes: Vec::new(),
				scope_label: "global".to_string(),
			},
		);

		let diffs = build_source_skill_diffs(
			dir.path(),
			vec![skill::RepoDiscoveredSkill {
				name: "qa".to_string(),
				full_path: dir.path().join("skills/deprecated/qa/SKILL.md"),
				relative_dir: "skills/deprecated/qa".to_string(),
			}],
			&baseline,
		);

		let qa = diffs
			.iter()
			.find(|diff| diff.skill_path == "skills/deprecated/qa/SKILL.md")
			.expect("deprecated skill should be present");
		assert_eq!(
			qa.state,
			SourceSkillState::Deprecated,
			"rename must not overwrite the deprecated state"
		);
		let removed = diffs
			.iter()
			.find(|diff| {
				diff.skill_path == "skills/engineering/old-qa/SKILL.md"
			})
			.expect("old lock entry should be present");
		assert_eq!(removed.state, SourceSkillState::Removed);
	}

	#[test]
	fn changelog_replace_with_ignores_trailing_tokens_and_handles_multiple() {
		// The `replace … with …` connective gets the same connective-anchored
		// treatment as `rename … to …`: trailing refs do not hijack the new
		// name, and multiple pairs on one line are all captured.
		let dir = tempdir().unwrap();
		fs::write(
			dir.path().join("CHANGELOG.md"),
			"- Replace `write-a-skill` with `writing-great-skills` (see `#42`); \
			 replace `old-x` with `new-x`.",
		)
		.unwrap();

		let successors = skill_successors_from_changelog(dir.path());

		assert_eq!(
			successors.get("write-a-skill").map(String::as_str),
			Some("writing-great-skills"),
		);
		assert_eq!(successors.get("old-x").map(String::as_str), Some("new-x"));
		assert!(
			!successors.contains_key("#42"),
			"trailing `#42` must not become a successor key"
		);
		assert!(!successors.values().any(|v| v == "#42"));
	}

	#[test]
	fn changelog_rename_yields_nothing_when_backticks_do_not_bracket_connective(
	) {
		// Verb + connective present, but no backtick term AFTER the connective:
		// the parser must yield no successor rather than guessing.
		let dir = tempdir().unwrap();
		fs::write(
			dir.path().join("CHANGELOG.md"),
			"- Rename `foo` `bar` to improve naming consistency.",
		)
		.unwrap();

		let successors = skill_successors_from_changelog(dir.path());

		assert!(
			successors.is_empty(),
			"no backtick after the connective => no successor, got {successors:?}"
		);
	}

	#[test]
	fn changelog_rename_ignores_connective_inside_a_backtick_term() {
		// A connective (" to "/" with ") that occurs INSIDE a backtick term, or
		// in a different clause from the rename verb, must not bind the
		// surrounding terms into a successor. Here the only "rename" clause does
		// not contain a connective, so the line yields nothing.
		let dir = tempdir().unwrap();
		fs::write(
			dir.path().join("CHANGELOG.md"),
			"- Rename docs for `alpha`; note `x to y` maps to `beta`.",
		)
		.unwrap();

		let successors = skill_successors_from_changelog(dir.path());

		assert!(
			successors.is_empty(),
			"connective inside a backtick term / foreign clause must not map, got {successors:?}"
		);
	}
}

#[cfg(test)]
mod diff_tests {
	use super::*;
	use std::fs;
	use tempfile::TempDir;

	/// A [`Fetcher`] that serves a fixed local dir as the fetched repo, ignoring
	/// the source/token. The unauthenticated fetch always succeeds, so the token
	/// resolver is never consulted.
	struct DirFetcher {
		root: std::path::PathBuf,
	}
	impl Fetcher for DirFetcher {
		fn fetch(
			&self,
			_sr: &SourceRef,
			_token: Option<&str>,
		) -> Result<crate::FetchedRepo, FetchError> {
			Ok(crate::FetchedRepo {
				root: self.root.clone(),
				oid: "test-oid".to_string(),
				_guard: None,
			})
		}
	}

	/// A [`TokenResolver`] that never has a token.
	struct NoToken;
	impl TokenResolver for NoToken {
		fn resolve(&self, _s: &str, _h: Option<&str>) -> Option<String> {
			None
		}
	}

	/// A [`Fetcher`] that records the `ref_` it was asked to fetch so a test
	/// can assert the resolved ref handed to the fetch (recorded-ref fallback).
	struct RefCapturingFetcher {
		root: std::path::PathBuf,
		seen_ref: std::sync::Mutex<Option<Option<String>>>,
	}
	impl Fetcher for RefCapturingFetcher {
		fn fetch(
			&self,
			sr: &SourceRef,
			_token: Option<&str>,
		) -> Result<crate::FetchedRepo, FetchError> {
			*self.seen_ref.lock().unwrap() = Some(sr.ref_.clone());
			Ok(crate::FetchedRepo {
				root: self.root.clone(),
				oid: "test-oid".to_string(),
				_guard: None,
			})
		}
	}

	/// Write a project lock entry recording a non-default ref for `source`.
	fn write_project_lock_entry(root: &Path, source: &str, ref_name: &str) {
		write_project_lock_entry_typed(root, source, Some(ref_name), "github");
	}

	/// Write a project lock entry with an explicit `source_type` and an optional
	/// recorded ref.
	fn write_project_lock_entry_typed(
		root: &Path,
		source: &str,
		ref_name: Option<&str>,
		source_type: &str,
	) {
		let mut lock = skill::LocalSkillLockFile::new();
		lock.skills.insert(
			"s".to_string(),
			skill::LocalSkillLockEntry {
				source: source.to_string(),
				ref_name: ref_name.map(str::to_string),
				source_type: source_type.to_string(),
				skill_path: Some("s/SKILL.md".to_string()),
				computed_hash: "h".to_string(),
				ref_commit: None,
			},
		);
		skill::write_local_lock(&lock, Some(root)).unwrap();
	}

	fn write_skill(root: &Path, relative_dir: &str, name: &str) {
		let dir = root.join(relative_dir);
		fs::create_dir_all(&dir).unwrap();
		fs::write(
			dir.join("SKILL.md"),
			format!("---\nname: {name}\ndescription: {name} skill\n---\n"),
		)
		.unwrap();
	}

	#[test]
	fn diff_source_reports_not_installed() {
		let upstream = TempDir::new().unwrap();
		write_skill(upstream.path(), "alpha", "alpha");

		let fetcher = DirFetcher {
			root: upstream.path().to_path_buf(),
		};
		let resolver = NoToken;
		let outcome = diff_source(
			SourceDiffInput {
				// A unique test source not present in any installed lock, so the
				// baseline is empty and `alpha` resolves to NotInstalled.
				source: "test-owner/diff-source-not-installed".to_string(),
				git_ref: None,
				scopes: vec![SourceScope::Global],
			},
			SourceDiffDeps {
				fetcher: &fetcher,
				resolver: &resolver,
			},
		);

		match outcome {
			SourceDiffOutcome::Ok { skills, .. } => {
				let alpha = skills
					.iter()
					.find(|s| s.name == "alpha")
					.expect("alpha should be discovered");
				assert_eq!(alpha.state, SourceSkillState::NotInstalled);
			}
			other => panic!("expected Ok, got {other:?}"),
		}
	}

	#[test]
	fn merged_baseline_surfaces_recorded_ref_for_fallback() {
		// A skill installed from a tag/feature-branch records its ref; the
		// baseline builder must surface it so `diff_source` can fall back to
		// it when no explicit ref is requested.
		let project = TempDir::new().unwrap();
		let source = "owner/recorded-ref-baseline";
		write_project_lock_entry(project.path(), source, "v1.2.3");

		let (_baseline, _source_type, recorded_ref) =
			merged_baseline_for_source(
				&[SourceScope::Project {
					root: project.path().to_path_buf(),
				}],
				source,
			);

		assert_eq!(recorded_ref.as_deref(), Some("v1.2.3"));
	}

	#[test]
	fn diff_source_falls_back_to_recorded_ref_when_input_ref_none() {
		// input git_ref None + a lock entry with a recorded ref => the resolved
		// git_ref (surfaced on the outcome AND handed to the fetch) must be the
		// recorded ref, not None / the default branch. Without the fallback the
		// fetch would receive None and the outcome would carry None.
		let upstream = TempDir::new().unwrap();
		write_skill(upstream.path(), "alpha", "alpha");
		let project = TempDir::new().unwrap();
		let source = "owner/recorded-ref-diff";
		write_project_lock_entry(project.path(), source, "v9.9.9");

		let fetcher = RefCapturingFetcher {
			root: upstream.path().to_path_buf(),
			seen_ref: std::sync::Mutex::new(None),
		};
		let resolver = NoToken;
		let outcome = diff_source(
			SourceDiffInput {
				source: source.to_string(),
				git_ref: None,
				scopes: vec![SourceScope::Project {
					root: project.path().to_path_buf(),
				}],
			},
			SourceDiffDeps {
				fetcher: &fetcher,
				resolver: &resolver,
			},
		);

		match outcome {
			SourceDiffOutcome::Ok { git_ref, .. } => {
				assert_eq!(
					git_ref.as_deref(),
					Some("v9.9.9"),
					"outcome must carry the recorded ref as the resolved ref"
				);
			}
			other => panic!("expected Ok, got {other:?}"),
		}
		assert_eq!(
			fetcher.seen_ref.lock().unwrap().clone(),
			Some(Some("v9.9.9".to_string())),
			"fetch must be issued at the recorded ref, not the default branch"
		);
	}

	#[test]
	fn resolve_source_meta_explicit_ref_wins_over_recorded() {
		// explicit > recorded: a passed `--ref` overrides the lock's recorded
		// ref entirely.
		let project = TempDir::new().unwrap();
		let source = "owner/meta-explicit";
		write_project_lock_entry(project.path(), source, "v1");

		let meta = resolve_source_meta(
			source,
			&[SourceScope::Project {
				root: project.path().to_path_buf(),
			}],
			Some("feature-x"),
		);

		assert_eq!(meta.effective_ref.as_deref(), Some("feature-x"));
		assert_eq!(meta.source_type, "github");
	}

	#[test]
	fn resolve_source_meta_recorded_ref_used_when_no_explicit() {
		// recorded > None: with no `--ref`, the recorded lock ref is used.
		let project = TempDir::new().unwrap();
		let source = "owner/meta-recorded";
		write_project_lock_entry(project.path(), source, "v2");

		let meta = resolve_source_meta(
			source,
			&[SourceScope::Project {
				root: project.path().to_path_buf(),
			}],
			None,
		);

		assert_eq!(meta.effective_ref.as_deref(), Some("v2"));
	}

	#[test]
	fn resolve_source_meta_none_when_neither_explicit_nor_recorded() {
		// No explicit ref AND no recorded ref => None (default branch). This is
		// the case where the CLI records None on install while the API records
		// the scan session's resolved default-branch name (documented residual
		// in `crates/cli/src/commands/source.rs`).
		let project = TempDir::new().unwrap();
		let source = "owner/meta-none";
		write_project_lock_entry_typed(project.path(), source, None, "github");

		let meta = resolve_source_meta(
			source,
			&[SourceScope::Project {
				root: project.path().to_path_buf(),
			}],
			None,
		);

		assert_eq!(meta.effective_ref, None);
	}

	#[test]
	fn resolve_source_meta_passes_through_recorded_source_type() {
		// A lock recording source_type "git" (not "github") must surface "git",
		// not the "github" default.
		let project = TempDir::new().unwrap();
		let source = "git@example.com:owner/meta-git.git";
		write_project_lock_entry_typed(project.path(), source, None, "git");

		let meta = resolve_source_meta(
			source,
			&[SourceScope::Project {
				root: project.path().to_path_buf(),
			}],
			None,
		);

		assert_eq!(meta.source_type, "git");
	}

	#[test]
	fn resolve_source_meta_defaults_source_type_to_github_when_absent() {
		// A source not present in any lock => empty recorded type, defaulted to
		// "github" (matching the default `diff_source` applies before its
		// `precheck_source`).
		let meta = resolve_source_meta(
			"owner/meta-absent",
			&[SourceScope::Global],
			None,
		);

		assert_eq!(meta.source_type, "github");
		assert_eq!(meta.effective_ref, None);
	}

	#[test]
	fn resolve_source_meta_local_source_prechecks_uncheckable() {
		// A lock recording source_type "local" must surface "local", which
		// `precheck_source` then rejects as uncheckable (no fetch).
		use aghub_core::skills::update::{precheck_source, UncheckableReason};
		let project = TempDir::new().unwrap();
		let source = "/some/local/path";
		write_project_lock_entry_typed(project.path(), source, None, "local");

		let meta = resolve_source_meta(
			source,
			&[SourceScope::Project {
				root: project.path().to_path_buf(),
			}],
			None,
		);

		assert_eq!(meta.source_type, "local");
		assert!(matches!(
			precheck_source(&meta.source_type, source),
			Some(UncheckableReason::Local)
		));
	}
}

#[cfg(test)]
mod scope_tests {
	use super::*;

	fn project_root() -> PathBuf {
		PathBuf::from("/tmp/aghub-scope-test")
	}

	#[test]
	fn read_scopes_global_is_global_only() {
		let scopes = read_scopes(ScopeSelector::Global, None).unwrap();
		assert!(matches!(scopes.as_slice(), [SourceScope::Global]));
	}

	#[test]
	fn read_scopes_project_with_root_is_project_only() {
		let root = project_root();
		let scopes =
			read_scopes(ScopeSelector::Project, Some(root.clone())).unwrap();
		assert!(
			matches!(scopes.as_slice(), [SourceScope::Project { root: r }] if *r == root)
		);
	}

	#[test]
	fn read_scopes_project_without_root_errs() {
		let err = read_scopes(ScopeSelector::Project, None).unwrap_err();
		assert!(matches!(err, ScopeError::ProjectRootRequired));
	}

	#[test]
	fn read_scopes_all_with_root_is_global_then_project() {
		let root = project_root();
		let scopes =
			read_scopes(ScopeSelector::All, Some(root.clone())).unwrap();
		assert!(
			matches!(scopes.as_slice(), [SourceScope::Global, SourceScope::Project { root: r }] if *r == root)
		);
	}

	#[test]
	fn read_scopes_all_without_root_is_global_only() {
		let scopes = read_scopes(ScopeSelector::All, None).unwrap();
		assert!(matches!(scopes.as_slice(), [SourceScope::Global]));
	}

	#[test]
	fn write_scope_global_is_global() {
		let (scope, kind) = write_scope(ScopeSelector::Global, None).unwrap();
		assert!(matches!(scope, SourceScope::Global));
		assert_eq!(kind, SourceScopeKind::Global);
	}

	#[test]
	fn write_scope_project_with_root_is_project() {
		let root = project_root();
		let (scope, kind) =
			write_scope(ScopeSelector::Project, Some(root.clone())).unwrap();
		assert!(matches!(scope, SourceScope::Project { root: r } if r == root));
		assert_eq!(kind, SourceScopeKind::Project);
	}

	#[test]
	fn write_scope_project_without_root_errs() {
		let err = write_scope(ScopeSelector::Project, None).unwrap_err();
		assert!(matches!(err, ScopeError::ProjectRootRequired));
	}

	#[test]
	fn write_scope_all_is_rejected() {
		let err = write_scope(ScopeSelector::All, None).unwrap_err();
		assert!(matches!(err, ScopeError::AllNotAllowedForWrite));
	}

	#[test]
	fn scope_error_display_matches_cli_strings() {
		assert_eq!(
			ScopeError::ProjectRootRequired.to_string(),
			"no project root found (need an agent marker like .claude/, \
			 .opencode/, .mcp.json, …)"
		);
		assert_eq!(
			ScopeError::AllNotAllowedForWrite.to_string(),
			"`source sync` needs exactly one scope; --all is not allowed"
		);
		assert_eq!(
			ScopeError::ScopeRequired.to_string(),
			"`source sync` needs a scope: pass -g (global) or -p (project)"
		);
	}
}

#[cfg(test)]
mod scan_tests {
	use super::*;
	use std::fs;
	use tempfile::TempDir;

	/// Serves a fixed local dir; the unauthenticated fetch always succeeds.
	struct DirFetcher {
		root: std::path::PathBuf,
	}
	impl Fetcher for DirFetcher {
		fn fetch(
			&self,
			_sr: &SourceRef,
			_token: Option<&str>,
		) -> Result<crate::FetchedRepo, FetchError> {
			Ok(crate::FetchedRepo {
				root: self.root.clone(),
				oid: "test-oid".to_string(),
				_guard: None,
			})
		}
	}

	/// Always fails with a fixed [`FetchError`]; the resolver is then consulted
	/// and (with [`NoToken`]) the retry never happens, so the first error wins.
	struct FailFetcher {
		err: &'static str, // "auth" | "network"
	}
	impl Fetcher for FailFetcher {
		fn fetch(
			&self,
			_sr: &SourceRef,
			_token: Option<&str>,
		) -> Result<crate::FetchedRepo, FetchError> {
			Err(match self.err {
				"auth" => FetchError::Auth,
				_ => FetchError::Network,
			})
		}
	}

	/// A [`TokenResolver`] that never has a token.
	struct NoToken;
	impl TokenResolver for NoToken {
		fn resolve(&self, _s: &str, _h: Option<&str>) -> Option<String> {
			None
		}
	}

	fn write_skill(root: &Path, relative_dir: &str, name: &str) {
		let dir = root.join(relative_dir);
		fs::create_dir_all(&dir).unwrap();
		fs::write(
			dir.join("SKILL.md"),
			format!("---\nname: {name}\ndescription: {name} skill\n---\n"),
		)
		.unwrap();
	}

	#[test]
	fn scan_for_sync_happy_path_classifies_not_installed() {
		let upstream = TempDir::new().unwrap();
		write_skill(upstream.path(), "alpha", "alpha");

		let fetcher = DirFetcher {
			root: upstream.path().to_path_buf(),
		};
		let resolver = NoToken;
		// A unique source absent from any installed lock => empty baseline =>
		// `alpha` resolves to NotInstalled, source_type defaults to "github".
		let scan = scan_for_sync(
			"test-owner/scan-not-installed",
			None,
			&SourceScope::Global,
			SourceSyncDeps {
				fetcher: &fetcher,
				resolver: &resolver,
			},
		)
		.expect("scan should succeed");

		let alpha = scan
			.diffs
			.iter()
			.find(|s| s.name == "alpha")
			.expect("alpha should be discovered");
		assert_eq!(alpha.state, SourceSkillState::NotInstalled);
		assert_eq!(scan.source_type, "github");
		assert_eq!(scan.git_ref, None);
		assert_eq!(scan.repo.root, upstream.path());
	}

	#[test]
	fn scan_for_sync_uncheckable_source_skips_fetch() {
		// A lock recording source_type "local" => precheck rejects it before any
		// fetch; the fetcher must never be consulted.
		let project = TempDir::new().unwrap();
		let source = "/some/local/path";
		let mut lock = skill::LocalSkillLockFile::new();
		lock.skills.insert(
			"s".to_string(),
			skill::LocalSkillLockEntry {
				source: source.to_string(),
				ref_name: None,
				source_type: "local".to_string(),
				skill_path: Some("s/SKILL.md".to_string()),
				computed_hash: "h".to_string(),
				ref_commit: None,
			},
		);
		skill::write_local_lock(&lock, Some(project.path())).unwrap();

		let fetcher = FailFetcher { err: "network" };
		let resolver = NoToken;
		let scope = SourceScope::Project {
			root: project.path().to_path_buf(),
		};
		let result = scan_for_sync(
			source,
			None,
			&scope,
			SourceSyncDeps {
				fetcher: &fetcher,
				resolver: &resolver,
			},
		);

		// `SyncScan` holds a non-Debug `FetchedRepo`, so match instead of
		// `expect_err` (which would require `T: Debug`).
		match result {
			Err(SyncScanError::Uncheckable(UncheckableReason::Local)) => {}
			Err(other) => panic!("expected Uncheckable(Local), got {other:?}"),
			Ok(_) => panic!("expected Uncheckable(Local), got Ok"),
		}
	}

	#[test]
	fn scan_for_sync_auth_error_maps_to_needs_credential() {
		let fetcher = FailFetcher { err: "auth" };
		let resolver = NoToken;
		let result = scan_for_sync(
			"owner/scan-auth",
			None,
			&SourceScope::Global,
			SourceSyncDeps {
				fetcher: &fetcher,
				resolver: &resolver,
			},
		);

		match result {
			Err(SyncScanError::NeedsCredential) => {}
			Err(other) => panic!("expected NeedsCredential, got {other:?}"),
			Ok(_) => panic!("expected NeedsCredential, got Ok"),
		}
	}

	#[test]
	fn scan_for_sync_network_error_maps_to_fetch_failed() {
		let fetcher = FailFetcher { err: "network" };
		let resolver = NoToken;
		let result = scan_for_sync(
			"owner/scan-network",
			None,
			&SourceScope::Global,
			SourceSyncDeps {
				fetcher: &fetcher,
				resolver: &resolver,
			},
		);

		match result {
			Err(SyncScanError::FetchFailed) => {}
			Err(other) => panic!("expected FetchFailed, got {other:?}"),
			Ok(_) => panic!("expected FetchFailed, got Ok"),
		}
	}

	#[test]
	fn scan_for_sync_explicit_ref_is_surfaced() {
		// An explicit git_ref overrides the (absent) recorded ref and is both
		// fetched and surfaced on the scan.
		let upstream = TempDir::new().unwrap();
		write_skill(upstream.path(), "alpha", "alpha");
		let fetcher = DirFetcher {
			root: upstream.path().to_path_buf(),
		};
		let resolver = NoToken;
		let scan = scan_for_sync(
			"owner/scan-explicit-ref",
			Some("feature-x"),
			&SourceScope::Global,
			SourceSyncDeps {
				fetcher: &fetcher,
				resolver: &resolver,
			},
		)
		.expect("scan should succeed");

		assert_eq!(scan.git_ref.as_deref(), Some("feature-x"));
	}

	// --- central retry seam: fetch_source_with_resolver (finding #5) ------

	/// Fails the UNAUTHENTICATED fetch, then succeeds ONLY when handed the
	/// expected token on the retry. Records every token it was offered so a test
	/// can assert the retry actually carried the resolved token. Also records the
	/// `(source, host)` the resolver was asked for, via a shared cell.
	struct TokenGatedFetcher {
		root: std::path::PathBuf,
		expected: &'static str,
		seen_tokens: std::sync::Mutex<Vec<Option<String>>>,
	}
	impl Fetcher for TokenGatedFetcher {
		fn fetch(
			&self,
			_sr: &SourceRef,
			token: Option<&str>,
		) -> Result<crate::FetchedRepo, FetchError> {
			self.seen_tokens
				.lock()
				.unwrap()
				.push(token.map(str::to_string));
			match token {
				Some(t) if t == self.expected => Ok(crate::FetchedRepo {
					root: self.root.clone(),
					oid: "test-oid".to_string(),
					_guard: None,
				}),
				// No token (first attempt) or a wrong token (retry) → Auth.
				_ => Err(FetchError::Auth),
			}
		}
	}

	/// A resolver that always supplies a fixed token and records the
	/// `(source, host)` arguments it was called with.
	struct RecordingResolver {
		token: Option<String>,
		seen_args: std::sync::Mutex<Option<(String, Option<String>)>>,
	}
	impl TokenResolver for RecordingResolver {
		fn resolve(&self, source: &str, host: Option<&str>) -> Option<String> {
			*self.seen_args.lock().unwrap() =
				Some((source.to_string(), host.map(str::to_string)));
			self.token.clone()
		}
	}

	#[test]
	fn fetch_with_resolver_retries_with_resolved_token() {
		// finding #5: first (unauth) fetch fails; the resolver supplies the
		// expected token; the ONE retry succeeds.
		let upstream = TempDir::new().unwrap();
		write_skill(upstream.path(), "alpha", "alpha");
		let fetcher = TokenGatedFetcher {
			root: upstream.path().to_path_buf(),
			expected: "GOODTOK",
			seen_tokens: std::sync::Mutex::new(Vec::new()),
		};
		let resolver = RecordingResolver {
			token: Some("GOODTOK".to_string()),
			seen_args: std::sync::Mutex::new(None),
		};
		let sr = SourceRef {
			source: "owner/retry-ok".to_string(),
			ref_: None,
		};

		let repo = fetch_source_with_resolver(&sr, &fetcher, &resolver)
			.expect("retry with resolved token must succeed");
		assert_eq!(repo.root, upstream.path());

		// Exactly two attempts: unauth (None) then the resolved token.
		let seen = fetcher.seen_tokens.lock().unwrap();
		assert_eq!(seen.len(), 2);
		assert_eq!(seen[0], None);
		assert_eq!(seen[1], Some("GOODTOK".to_string()));

		// The resolver was asked for THIS source and its keychain host.
		let args = resolver.seen_args.lock().unwrap().clone();
		let (source, host) = args.expect("resolver must be consulted");
		assert_eq!(source, "owner/retry-ok");
		assert_eq!(host.as_deref(), Some("github.com"));
	}

	#[test]
	fn fetch_with_resolver_no_token_returns_first_error() {
		// finding #5: unauth fetch fails and the resolver has NO token → the
		// original error is returned and the fetch is NOT retried.
		let upstream = TempDir::new().unwrap();
		let fetcher = TokenGatedFetcher {
			root: upstream.path().to_path_buf(),
			expected: "GOODTOK",
			seen_tokens: std::sync::Mutex::new(Vec::new()),
		};
		let resolver = NoToken;
		let sr = SourceRef {
			source: "owner/retry-no-token".to_string(),
			ref_: None,
		};

		match fetch_source_with_resolver(&sr, &fetcher, &resolver) {
			Err(FetchError::Auth) => {}
			Err(other) => panic!("expected Auth, got {other:?}"),
			Ok(_) => panic!("expected the first error, got Ok"),
		}
		// Only the unauth attempt happened; no retry without a token.
		assert_eq!(fetcher.seen_tokens.lock().unwrap().len(), 1);
	}

	#[test]
	fn fetch_with_resolver_wrong_token_retry_fails() {
		// finding #5: the resolver supplies a WRONG token; the single retry is
		// attempted and fails — there is no second retry.
		let upstream = TempDir::new().unwrap();
		let fetcher = TokenGatedFetcher {
			root: upstream.path().to_path_buf(),
			expected: "GOODTOK",
			seen_tokens: std::sync::Mutex::new(Vec::new()),
		};
		let resolver = RecordingResolver {
			token: Some("WRONGTOK".to_string()),
			seen_args: std::sync::Mutex::new(None),
		};
		let sr = SourceRef {
			source: "owner/retry-wrong".to_string(),
			ref_: None,
		};

		match fetch_source_with_resolver(&sr, &fetcher, &resolver) {
			Err(FetchError::Auth) => {}
			Err(other) => panic!("expected Auth, got {other:?}"),
			Ok(_) => panic!("expected wrong-token retry to fail"),
		}
		// Two attempts total: unauth (None) + the one wrong-token retry.
		let seen = fetcher.seen_tokens.lock().unwrap();
		assert_eq!(seen.len(), 2);
		assert_eq!(seen[1], Some("WRONGTOK".to_string()));
	}

	#[test]
	fn diff_source_retries_with_resolved_token() {
		// finding #5: the retry seam works end-to-end through diff_source — an
		// unauth fetch failure + a resolver token yields a successful diff.
		let upstream = TempDir::new().unwrap();
		write_skill(upstream.path(), "alpha", "alpha");
		let fetcher = TokenGatedFetcher {
			root: upstream.path().to_path_buf(),
			expected: "GOODTOK",
			seen_tokens: std::sync::Mutex::new(Vec::new()),
		};
		let resolver = RecordingResolver {
			token: Some("GOODTOK".to_string()),
			seen_args: std::sync::Mutex::new(None),
		};

		let outcome = diff_source(
			SourceDiffInput {
				source: "owner/diff-retry".to_string(),
				git_ref: None,
				scopes: vec![SourceScope::Global],
			},
			SourceDiffDeps {
				fetcher: &fetcher,
				resolver: &resolver,
			},
		);

		match outcome {
			SourceDiffOutcome::Ok { skills, .. } => {
				assert!(skills.iter().any(|s| s.name == "alpha"));
			}
			other => panic!("expected Ok after retry, got {other:?}"),
		}
	}

	#[test]
	fn diff_source_no_token_maps_to_needs_credential() {
		// finding #5: an unauth Auth failure with no resolver token surfaces as
		// NeedsCredential (the caller's auth-needed mapping), not FetchFailed.
		let upstream = TempDir::new().unwrap();
		let fetcher = TokenGatedFetcher {
			root: upstream.path().to_path_buf(),
			expected: "GOODTOK",
			seen_tokens: std::sync::Mutex::new(Vec::new()),
		};
		let resolver = NoToken;

		let outcome = diff_source(
			SourceDiffInput {
				source: "owner/diff-needs-cred".to_string(),
				git_ref: None,
				scopes: vec![SourceScope::Global],
			},
			SourceDiffDeps {
				fetcher: &fetcher,
				resolver: &resolver,
			},
		);

		match outcome {
			SourceDiffOutcome::NeedsCredential { .. } => {}
			other => panic!("expected NeedsCredential, got {other:?}"),
		}
	}

	#[test]
	fn scan_for_sync_retries_with_resolved_token() {
		// finding #5: the retry seam works through scan_for_sync too.
		let upstream = TempDir::new().unwrap();
		write_skill(upstream.path(), "alpha", "alpha");
		let fetcher = TokenGatedFetcher {
			root: upstream.path().to_path_buf(),
			expected: "GOODTOK",
			seen_tokens: std::sync::Mutex::new(Vec::new()),
		};
		let resolver = RecordingResolver {
			token: Some("GOODTOK".to_string()),
			seen_args: std::sync::Mutex::new(None),
		};

		let scan = scan_for_sync(
			"owner/scan-retry",
			None,
			&SourceScope::Global,
			SourceSyncDeps {
				fetcher: &fetcher,
				resolver: &resolver,
			},
		)
		.expect("scan should succeed after retry");
		assert_eq!(scan.repo.root, upstream.path());
		// Unauth attempt + resolved-token retry.
		assert_eq!(fetcher.seen_tokens.lock().unwrap().len(), 2);
	}
}
