//! Unified "Sources" endpoints.
//!
//! - `GET /skills/sources` — offline, lock-only: groups installed skills by
//!   source per scope and reports a count + credential availability.
//! - `GET /skills/sources/diff` — fetches a single source ONCE and reports each
//!   of its skills as not-installed / installed-current / installed-outdated /
//!   uncheckable, so the UI can offer "install the new ones".

use std::collections::BTreeMap;
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

	let mut out = Vec::with_capacity(discovered.len());
	for d in discovered {
		let skill_path = skill::lock_skill_file_path(&d.relative_dir);
		let (description, version, author) = parse_meta(&d.full_path);

		match baseline.get(&skill_path) {
			None => out.push(SourceSkillDiff {
				name: d.name,
				skill_path,
				description,
				version,
				author,
				state: "notInstalled".to_string(),
				previous_name: None,
				reason: None,
				installed_paths: Vec::new(),
			}),
			Some(entry) => {
				let skill_dir =
					aghub_core::skills::skill_source_root(&d.full_path);
				let (state, previous_name, reason) =
					classify_source_skill_diff(entry, &d.name, &skill_dir);
				out.push(SourceSkillDiff {
					name: d.name,
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

	DiffOutcome::Ok(out)
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
}
