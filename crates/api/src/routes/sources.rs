//! Unified "Sources" endpoints.
//!
//! - `GET /skills/sources` — offline, lock-only: groups installed skills by
//!   source per scope and reports a count + credential availability.
//! - `GET /skills/sources/diff` — fetches a single source ONCE and reports each
//!   of its skills as not-installed / installed-current / installed-outdated /
//!   uncheckable, so the UI can offer "install the new ones".

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use rocket::http::Status;
use rocket::serde::json::Json;

use crate::credentials::resolve::{
	load_source_bindings, resolve_token_for_source,
};
use crate::dto::sources::{
	CredentialStatus, SourceDiffResponse, SourceSkillDiff,
	SourceSummaryResponse, SourcesListResponse,
};
use crate::error::{ApiError, ApiResult};
use crate::extractors::{ResolvedScope, ScopeParams};
use crate::routes::credentials::load_credentials;
use crate::routes::skills_update::installed_skill_roots;
use crate::skills::rename::detect_rename;
use aghub_core::models::ResourceScope;
use aghub_core::skills::update::{
	compare_known_hashes, precheck_source, SkillUpdateStatus, UncheckableReason,
};
use skill_update::{
	keychain_host_for_source, FetchError, Fetcher, GitFetcher, SourceRef,
};

// ─────────────────────────── GET /skills/sources ───────────────────────────

#[get("/skills/sources?<query..>")]
pub fn list_sources(query: ScopeParams) -> ApiResult<SourcesListResponse> {
	let resolved = query.resolve()?;

	let mut sources = Vec::new();
	match resolved {
		ResolvedScope::Global => {
			sources.extend(global_sources());
		}
		ResolvedScope::Project { root } => {
			sources.extend(project_sources(&root));
		}
		ResolvedScope::All { project_root } => {
			sources.extend(global_sources());
			if let Some(root) = project_root {
				sources.extend(project_sources(&root));
			}
		}
	}

	Ok(Json(SourcesListResponse { sources }))
}

/// Group the global lock's skills by source (the global lock carries `sourceUrl`).
fn global_sources() -> Vec<SourceSummaryResponse> {
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
		.map(|(source, (source_url, source_type, skill_count))| {
			SourceSummaryResponse {
				source,
				source_url,
				source_type,
				scope: "global".to_string(),
				skill_count,
				is_private: false,
				credential_status: CredentialStatus::NotRequired,
			}
		})
		.collect()
}

/// Group a project lock's skills by source. The project lock omits `sourceUrl`,
/// so the fetch URL is reconstructed from `owner/repo` (GitHub etc.).
fn project_sources(root: &Path) -> Vec<SourceSummaryResponse> {
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
			SourceSummaryResponse {
				source,
				source_url,
				source_type,
				scope: "project".to_string(),
				skill_count,
				is_private: false,
				credential_status: CredentialStatus::NotRequired,
			}
		})
		.collect()
}

fn reconstruct_source_url(source: &str) -> String {
	aghub_git::resolve_remote_source(source)
		.map(|resolved| resolved.clone_url)
		.unwrap_or_else(|_| source.to_string())
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

// ──────────────────────── GET /skills/sources/diff ─────────────────────────

/// Query for the single-source diff. `scope`/`project_root` mirror `ScopeParams`;
/// `source` is the source identifier (owner/repo or URL); `git_ref` overrides the
/// branch/tag (defaults to the source's recorded ref / the repo default branch).
#[derive(rocket::FromForm)]
pub struct SourceDiffQuery {
	scope: Option<String>,
	project_root: Option<String>,
	source: String,
	git_ref: Option<String>,
}

struct BaselineEntry {
	installed_name: String,
	stored_hash: String,
	local_hashes: Vec<String>,
	scope_label: String,
}

/// skill_path -> installed baseline metadata
type Baseline = BTreeMap<String, BaselineEntry>;

enum DiffOutcome {
	/// Private source with no usable credential; UI should offer to bind one.
	NeedsCredential,
	/// Transport/network failure fetching the source.
	FetchFailed,
	Ok(Vec<SourceSkillDiff>),
}

enum LazyFetchError {
	NeedsCredential,
	FetchFailed,
}

#[get("/skills/sources/diff?<query..>")]
pub async fn diff_source(
	query: SourceDiffQuery,
) -> ApiResult<SourceDiffResponse> {
	let scope_params = ScopeParams {
		scope: query.scope.clone(),
		project_root: query.project_root.clone(),
	};
	let resolved = scope_params.resolve()?;
	let source = query.source.trim().to_string();

	// 1. Lock baseline (skill_path -> hash) + the source's recorded type + ref.
	let (baseline, mut source_type, recorded_ref) =
		lock_baseline_for_source(&resolved, &source);
	if source_type.is_empty() {
		source_type = "github".to_string();
	}

	// Use the explicitly requested ref, else the source's RECORDED ref, else the
	// repo default branch (None). Without this fallback, a skill installed from a
	// tag or feature branch is diffed against the default branch's content,
	// producing spurious notInstalled / installedOutdated results.
	let git_ref = query.git_ref.clone().or(recorded_ref);

	// 2. Skip sources we cannot fetch (local/ssh/unsupported) up front.
	if precheck_source(&source_type, &source).is_some() {
		return Ok(Json(SourceDiffResponse {
			source,
			git_ref,
			session_id: None,
			needs_credential: false,
			skills: Vec::new(),
		}));
	}

	// 3. Fetch + discover + classify on a blocking thread (sync git IO, and the
	//    materialized temp dir must outlive discovery + hashing). The fetch path
	//    tries public/unauthenticated first and only touches Keychain after an
	//    unauthenticated fetch failure.
	let source_for_blk = source.clone();
	let ref_for_blk = git_ref.clone();
	let outcome = rocket::tokio::task::spawn_blocking(move || {
		diff_blocking(&source_for_blk, ref_for_blk.as_deref(), &baseline)
	})
	.await
	.map_err(|e| {
		ApiError::new(
			Status::InternalServerError,
			format!("diff task panicked: {e}"),
			"DIFF_TASK_PANIC",
		)
	})?;

	match outcome {
		DiffOutcome::NeedsCredential => Ok(Json(SourceDiffResponse {
			source,
			git_ref,
			session_id: None,
			needs_credential: true,
			skills: Vec::new(),
		})),
		DiffOutcome::FetchFailed => Err(ApiError::new(
			Status::BadGateway,
			"Failed to fetch source repository",
			"SOURCE_FETCH_FAILED",
		)),
		DiffOutcome::Ok(skills) => Ok(Json(SourceDiffResponse {
			source,
			git_ref,
			session_id: None,
			needs_credential: false,
			skills,
		})),
	}
}

/// Build the installed-skill baseline (by repo-relative `skillPath`) for a
/// source within the resolved scope, plus the source's recorded `sourceType`.
fn lock_baseline_for_source(
	resolved: &ResolvedScope,
	source: &str,
) -> (Baseline, String, Option<String>) {
	let mut baseline = Baseline::new();
	let mut source_type = String::new();
	// First non-None recorded ref among the source's lock entries. A single
	// source may in theory mix refs across skills, but the endpoint fetches one
	// ref; the recorded ref is still far better than blindly using the default
	// branch for a tag/feature-branch install.
	let mut recorded_ref: Option<String> = None;
	let want = source.trim();

	let include_global =
		matches!(resolved, ResolvedScope::Global | ResolvedScope::All { .. });
	let project_root: Option<&Path> = match resolved {
		ResolvedScope::Project { root } => Some(root.as_path()),
		ResolvedScope::All {
			project_root: Some(root),
		} => Some(root.as_path()),
		_ => None,
	};

	if include_global {
		for (name, entry) in skill::get_all_locked_skills() {
			if !source_matches(want, &entry.source, Some(&entry.source_url)) {
				continue;
			}
			if source_type.is_empty() {
				source_type = entry.source_type.clone();
			}
			if recorded_ref.is_none() {
				recorded_ref = entry.ref_name.clone();
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

	if let Some(root) = project_root {
		for (name, entry) in skill::read_local_lock(Some(root)).skills {
			if !source_matches(want, &entry.source, None) {
				continue;
			}
			if source_type.is_empty() {
				source_type = entry.source_type.clone();
			}
			if recorded_ref.is_none() {
				recorded_ref = entry.ref_name.clone();
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

	(baseline, source_type, recorded_ref)
}

fn local_hashes_for_installed(
	name: &str,
	resource_scope: ResourceScope,
	project_root: Option<&Path>,
) -> Vec<String> {
	installed_skill_roots(name, resource_scope, project_root)
		.into_iter()
		.filter_map(|root| skill::compute_skill_folder_hash(&root).ok())
		.collect()
}

/// Synchronous fetch → discover-all → classify. Runs on a blocking thread.
fn diff_blocking(
	source: &str,
	git_ref: Option<&str>,
	baseline: &Baseline,
) -> DiffOutcome {
	let source_ref = SourceRef {
		source: source.to_string(),
		ref_: git_ref.map(|s| s.to_string()),
	};
	let fetched = match fetch_source_lazily_auth(&source_ref) {
		Ok(repo) => repo,
		Err(LazyFetchError::NeedsCredential) => {
			return DiffOutcome::NeedsCredential;
		}
		Err(LazyFetchError::FetchFailed) => return DiffOutcome::FetchFailed,
	};
	let root = fetched.root.as_path();

	let discovered = match skill::discover_repo_skills(root, &[], true) {
		Ok(skills) => skills,
		// No SKILL.md in the repo → nothing to offer.
		Err(_) => return DiffOutcome::Ok(Vec::new()),
	};

	DiffOutcome::Ok(build_source_skill_diffs(root, discovered, baseline))
}

fn build_source_skill_diffs(
	root: &Path,
	discovered: Vec<skill::RepoDiscoveredSkill>,
	baseline: &Baseline,
) -> Vec<SourceSkillDiff> {
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
				let state = if is_deprecated_skill_path(&skill_path) {
					"deprecated".to_string()
				} else {
					"notInstalled".to_string()
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
					reason: None,
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
						&& diff.state != "deprecated"
					{
						diff.state = "renamed".to_string();
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
			state: "removed".to_string(),
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
) -> (String, Option<String>, Option<String>) {
	if let Some(_new_name) =
		detect_rename(discovered_name, &entry.installed_name)
	{
		return (
			"renamed".to_string(),
			Some(entry.installed_name.clone()),
			None,
		);
	}
	let (state, reason) = classify_installed(entry, skill_dir);
	(state, None, reason)
}

fn fetch_source_lazily_auth(
	source_ref: &SourceRef,
) -> Result<skill_update::FetchedRepo, LazyFetchError> {
	#[cfg(test)]
	if let Some(result) = test_fetch_source_from_env() {
		return result;
	}

	let fetcher = GitFetcher;
	match fetcher.fetch(source_ref, None) {
		Ok(repo) => Ok(repo),
		Err(error) => {
			let Some(token) = token_for_source(&source_ref.source) else {
				return match error {
					FetchError::Auth => Err(LazyFetchError::NeedsCredential),
					FetchError::Network => Err(LazyFetchError::FetchFailed),
				};
			};
			match fetcher.fetch(source_ref, Some(&token)) {
				Ok(repo) => Ok(repo),
				Err(FetchError::Auth) => Err(LazyFetchError::NeedsCredential),
				Err(FetchError::Network) => Err(LazyFetchError::FetchFailed),
			}
		}
	}
}

#[cfg(test)]
fn test_fetch_source_from_env(
) -> Option<Result<skill_update::FetchedRepo, LazyFetchError>> {
	let root = std::env::var_os("AGHUB_TEST_SOURCE_FETCH_ROOT")?;
	let root = std::path::PathBuf::from(root);
	Some(if root.is_dir() {
		Ok(skill_update::FetchedRepo {
			root,
			oid: "test-fetch-root".to_string(),
			_guard: None,
		})
	} else {
		Err(LazyFetchError::FetchFailed)
	})
}

fn token_for_source(source: &str) -> Option<String> {
	let bindings = load_source_bindings().unwrap_or_default();
	let creds = load_credentials().unwrap_or_default();
	let host = keychain_host_for_source(source);
	resolve_token_for_source(source, host.as_deref(), &bindings, &creds)
}

/// Classify an already-installed skill by comparing its upstream folder hash to
/// the installed baseline. Prefer actual installed folder hashes over the
/// stored lock hash because some locks were produced by npx/JS collation, while
/// this endpoint hashes fetched source with Rust collation. Comparing local
/// Rust hashes to fetched Rust hash avoids false updates for unchanged skills.
fn classify_installed(
	entry: &BaselineEntry,
	skill_dir: &Path,
) -> (String, Option<String>) {
	let fresh = match skill::compute_skill_folder_hash(skill_dir) {
		Ok(hash) => hash,
		Err(_) => {
			return ("uncheckable".to_string(), Some("local".to_string()))
		}
	};

	if !entry.local_hashes.is_empty() {
		if entry.local_hashes.iter().all(|hash| {
			compare_known_hashes(hash, &fresh) == SkillUpdateStatus::UpToDate
		}) {
			return ("installedCurrent".to_string(), None);
		}
		return ("installedOutdated".to_string(), None);
	}

	let baseline = if entry.stored_hash.is_empty()
		|| skill::is_placeholder_digest(&entry.stored_hash)
	{
		None
	} else {
		Some(entry.stored_hash.as_str())
	};
	let Some(base_hash) = baseline else {
		return ("installedCurrent".to_string(), None);
	};

	// The rename path short-circuits in `classify_source_skill_diff` before
	// reaching this function (it inspects `entry.installed_name` vs the
	// discovered name up front). `compare_known_hashes` itself never returns
	// `Renamed` — it only yields `UpToDate` or `UpdateAvailable` — so the
	// `Renamed` arm is unreachable here and intentionally omitted.
	match compare_known_hashes(base_hash, &fresh) {
		SkillUpdateStatus::UpToDate => ("installedCurrent".to_string(), None),
		SkillUpdateStatus::UpdateAvailable { .. } => {
			("installedOutdated".to_string(), None)
		}
		SkillUpdateStatus::Uncheckable { reason } => {
			("uncheckable".to_string(), Some(reason_str(reason)))
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

#[cfg(test)]
mod tests {
	use super::*;
	use rocket::http::Status;
	use rocket::local::blocking::Client;
	use rocket::routes;
	use serde_json::Value;
	use std::fs;
	use std::sync::MutexGuard;
	use tempfile::{tempdir, TempDir};

	/// Serializes + isolates the GLOBAL lock by pointing `XDG_STATE_HOME` at a
	/// fresh temp dir. Uses the crate-wide `test_env_lock()` (the SAME mutex as
	/// `with_isolated_state`/`with_isolated_env`) so it never races other api
	/// tests that mutate `XDG_STATE_HOME` in the shared test process.
	struct GlobalLockGuard {
		_temp: TempDir,
		old: Option<String>,
		_lock: MutexGuard<'static, ()>,
	}

	impl GlobalLockGuard {
		fn new() -> Self {
			let guard = crate::routes::test_env_lock()
				.lock()
				.unwrap_or_else(|e| e.into_inner());
			let temp = tempdir().unwrap();
			let old = std::env::var("XDG_STATE_HOME").ok();
			std::env::set_var("XDG_STATE_HOME", temp.path());
			Self {
				_temp: temp,
				old,
				_lock: guard,
			}
		}
	}

	impl Drop for GlobalLockGuard {
		fn drop(&mut self) {
			match &self.old {
				Some(v) => std::env::set_var("XDG_STATE_HOME", v),
				None => std::env::remove_var("XDG_STATE_HOME"),
			}
		}
	}

	struct EnvVarGuard {
		key: &'static str,
		old: Option<String>,
	}

	impl EnvVarGuard {
		fn set(key: &'static str, value: &Path) -> Self {
			let old = std::env::var(key).ok();
			std::env::set_var(key, value);
			Self { key, old }
		}
	}

	impl Drop for EnvVarGuard {
		fn drop(&mut self) {
			match &self.old {
				Some(v) => std::env::set_var(self.key, v),
				None => std::env::remove_var(self.key),
			}
		}
	}

	fn global_entry(source: &str, skill_path: &str) -> skill::SkillLockEntry {
		skill::SkillLockEntry {
			source: source.to_string(),
			source_type: "github".to_string(),
			source_url: format!("https://github.com/{source}.git"),
			ref_name: None,
			skill_path: Some(skill_path.to_string()),
			skill_folder_hash: "old-tree-hash".to_string(),
			content_hash: Some("old-content-hash".to_string()),
			ref_commit: None,
			installed_at: "2026-01-01T00:00:00Z".to_string(),
			updated_at: "2026-01-01T00:00:00Z".to_string(),
			plugin_name: None,
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
	fn lock_baseline_captures_recorded_ref_for_fallback() {
		// A skill installed from a tag/feature-branch records its ref; the diff
		// must fall back to it (not the default branch) when no ref is requested,
		// else a non-default-ref install is diffed against the wrong content.
		let _g = GlobalLockGuard::new();
		skill::lock::add_skill_to_lock(
			"s",
			skill::SkillLockEntry {
				source: "owner/repo".to_string(),
				source_type: "github".to_string(),
				source_url: "https://github.com/owner/repo".to_string(),
				ref_name: Some("v1.2.3".to_string()),
				skill_path: Some("s/SKILL.md".to_string()),
				skill_folder_hash: "h".to_string(),
				content_hash: None,
				ref_commit: None,
				installed_at: "t".to_string(),
				updated_at: "t".to_string(),
				plugin_name: None,
			},
		)
		.unwrap();

		let (_baseline, _source_type, recorded_ref) =
			lock_baseline_for_source(&ResolvedScope::Global, "owner/repo");

		assert_eq!(recorded_ref.as_deref(), Some("v1.2.3"));
	}

	#[test]
	fn diff_source_route_reports_breaking_skill_source_changes() {
		let _global = GlobalLockGuard::new();
		let source = "e2e-source";
		let upstream = tempdir().unwrap();
		fs::write(
				upstream.path().join("CHANGELOG.md"),
				"- [`47bde84`](https://github.com/mattpocock/skills/commit/47bde84) \
				 Thanks - Rename the **`diagnose`** skill to \
				 **`diagnosing-bugs`**.",
			)
			.unwrap();
		write_skill(
			upstream.path(),
			"skills/engineering/diagnosing-bugs",
			"diagnosing-bugs",
		);
		write_skill(upstream.path(), "skills/deprecated/qa", "qa");

		skill::lock::add_skill_to_lock(
			"diagnose",
			global_entry(source, "skills/engineering/diagnose/SKILL.md"),
		)
		.unwrap();
		skill::lock::add_skill_to_lock(
			"obsolete",
			global_entry(source, "skills/misc/obsolete/SKILL.md"),
		)
		.unwrap();

		let _fetch_root =
			EnvVarGuard::set("AGHUB_TEST_SOURCE_FETCH_ROOT", upstream.path());
		let client =
			Client::tracked(rocket::build().mount("/", routes![diff_source]))
				.expect("client");

		let response = client
			.get(format!("/skills/sources/diff?scope=global&source={source}"))
			.dispatch();

		assert_eq!(response.status(), Status::Ok);
		let body = response.into_string().expect("response body");
		let value: Value = serde_json::from_str(&body).expect("valid JSON");
		assert_eq!(value["needsCredential"], false);

		let skills = value["skills"]
			.as_array()
			.expect("skills should be an array");
		let renamed = skills
			.iter()
			.find(|skill| skill["name"] == "diagnosing-bugs")
			.expect("renamed skill should be returned");
		assert_eq!(renamed["state"], "renamed");
		assert_eq!(renamed["previousName"], "diagnose");
		assert_eq!(renamed["installedPaths"], serde_json::json!(["global"]));

		let deprecated = skills
			.iter()
			.find(|skill| skill["name"] == "qa")
			.expect("deprecated repo skill should be returned");
		assert_eq!(deprecated["state"], "deprecated");
		assert_eq!(deprecated["skillPath"], "skills/deprecated/qa/SKILL.md");

		let removed = skills
			.iter()
			.find(|skill| skill["skillPath"] == "skills/misc/obsolete/SKILL.md")
			.expect("removed locked skill should be returned");
		assert_eq!(removed["name"], "obsolete");
		assert_eq!(removed["state"], "removed");
		assert_eq!(removed["reason"], "noPath");
		assert_eq!(removed["installedPaths"], serde_json::json!(["global"]));
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
			("installedCurrent".to_string(), None)
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
			("installedOutdated".to_string(), None)
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
			("installedCurrent".to_string(), None)
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
			("installedOutdated".to_string(), None)
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
			("renamed".to_string(), Some("old-skill".to_string()), None)
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
		assert_eq!(removed.state, "removed");
		assert_eq!(removed.reason.as_deref(), Some("noPath"));
		assert_eq!(removed.installed_paths, vec!["global".to_string()]);

		assert!(diffs.iter().any(|diff| {
			diff.skill_path == "skills/engineering/diagnosing-bugs/SKILL.md"
				&& diff.state == "notInstalled"
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
		assert_eq!(diffs[0].state, "deprecated");
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
		assert_eq!(renamed.state, "renamed");
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
		assert_eq!(renamed.state, "renamed");
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
			!diffs.iter().any(|diff| diff.state == "renamed"),
			"ambiguous successor name must not produce a rename redirect"
		);
		let removed = diffs
			.iter()
			.find(|diff| diff.skill_path == "skills/legacy/SKILL.md")
			.expect("removed lock entry should be present");
		assert_eq!(removed.state, "removed");
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
			qa.state, "deprecated",
			"rename must not overwrite the deprecated state"
		);
		let removed = diffs
			.iter()
			.find(|diff| {
				diff.skill_path == "skills/engineering/old-qa/SKILL.md"
			})
			.expect("old lock entry should be present");
		assert_eq!(removed.state, "removed");
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
