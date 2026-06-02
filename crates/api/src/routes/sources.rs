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
	load_source_bindings, resolve_token_for_source, SourceBindings,
};
use crate::dto::sources::{
	CredentialStatus, SourceDiffResponse, SourceSkillDiff,
	SourceSummaryResponse, SourcesListResponse,
};
use crate::error::{ApiError, ApiResult};
use crate::extractors::{ResolvedScope, ScopeParams};
use crate::routes::credentials::{load_credentials, StoredCredential};
use crate::routes::skills_update::GitFetcher;
use crate::skills::update_check::{
	keychain_host_for_source, FetchError, Fetcher, SourceRef,
};
use aghub_core::skills::update::{
	compare_known_hashes, precheck_source, SkillUpdateStatus, UncheckableReason,
};

// ─────────────────────────── GET /skills/sources ───────────────────────────

#[get("/skills/sources?<query..>")]
pub fn list_sources(query: ScopeParams) -> ApiResult<SourcesListResponse> {
	let resolved = query.resolve()?;
	let bindings = load_source_bindings().unwrap_or_default();
	let creds = load_credentials().unwrap_or_default();

	let mut sources = Vec::new();
	match resolved {
		ResolvedScope::Global => {
			sources.extend(global_sources(&bindings, &creds));
		}
		ResolvedScope::Project { root } => {
			sources.extend(project_sources(&root, &bindings, &creds));
		}
		ResolvedScope::All { project_root } => {
			sources.extend(global_sources(&bindings, &creds));
			if let Some(root) = project_root {
				sources.extend(project_sources(&root, &bindings, &creds));
			}
		}
	}

	Ok(Json(SourcesListResponse { sources }))
}

/// Group the global lock's skills by source (the global lock carries `sourceUrl`).
fn global_sources(
	bindings: &SourceBindings,
	creds: &[StoredCredential],
) -> Vec<SourceSummaryResponse> {
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
			let (credential_status, is_private) =
				credential_status_for(&source, bindings, creds);
			SourceSummaryResponse {
				source,
				source_url,
				source_type,
				scope: "global".to_string(),
				skill_count,
				is_private,
				credential_status,
			}
		})
		.collect()
}

/// Group a project lock's skills by source. The project lock omits `sourceUrl`,
/// so the fetch URL is reconstructed from `owner/repo` (GitHub etc.).
fn project_sources(
	root: &Path,
	bindings: &SourceBindings,
	creds: &[StoredCredential],
) -> Vec<SourceSummaryResponse> {
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
			let (credential_status, is_private) =
				credential_status_for(&source, bindings, creds);
			SourceSummaryResponse {
				source,
				source_url,
				source_type,
				scope: "project".to_string(),
				skill_count,
				is_private,
				credential_status,
			}
		})
		.collect()
}

/// Offline credential availability for a source: an explicit binding to an
/// existing credential is `Bound`; otherwise a stored credential matching the
/// source host is `HostMatch`; otherwise `Missing`. `is_private` is the
/// best-effort offline guess (we only hold credentials for private sources).
fn credential_status_for(
	source: &str,
	bindings: &SourceBindings,
	creds: &[StoredCredential],
) -> (CredentialStatus, bool) {
	if let Some(cred_id) = bindings.0.get(source.trim()) {
		if creds.iter().any(|c| c.id == *cred_id) {
			return (CredentialStatus::Bound, true);
		}
	}
	let host = keychain_host_for_source(source);
	if resolve_token_for_source(source, host.as_deref(), bindings, creds)
		.is_some()
	{
		return (CredentialStatus::HostMatch, true);
	}
	(CredentialStatus::Missing, false)
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

/// skill_path -> (baseline content hash, scope label)
type Baseline = BTreeMap<String, (String, String)>;

enum DiffOutcome {
	/// Private source with no usable credential; UI should offer to bind one.
	NeedsCredential,
	/// Transport/network failure fetching the source.
	FetchFailed,
	Ok(Vec<SourceSkillDiff>),
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
	let git_ref = query.git_ref.clone();

	// 1. Lock baseline (skill_path -> hash) + the source's recorded type.
	let (baseline, mut source_type) =
		lock_baseline_for_source(&resolved, &source);
	if source_type.is_empty() {
		source_type = "github".to_string();
	}

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

	// 3. Resolve a token for (possibly private) re-fetch.
	let bindings = load_source_bindings().unwrap_or_default();
	let creds = load_credentials().unwrap_or_default();
	let host = keychain_host_for_source(&source);
	let token =
		resolve_token_for_source(&source, host.as_deref(), &bindings, &creds);

	// 4. Fetch once + discover + classify on a blocking thread (sync git IO,
	//    and the materialized temp dir must outlive discovery + hashing).
	let source_for_blk = source.clone();
	let ref_for_blk = git_ref.clone();
	let outcome = rocket::tokio::task::spawn_blocking(move || {
		diff_blocking(
			&source_for_blk,
			ref_for_blk.as_deref(),
			token.as_deref(),
			&baseline,
		)
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
) -> (Baseline, String) {
	let mut baseline = Baseline::new();
	let mut source_type = String::new();
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
		for (_name, entry) in skill::get_all_locked_skills() {
			if !source_matches(want, &entry.source, Some(&entry.source_url)) {
				continue;
			}
			if source_type.is_empty() {
				source_type = entry.source_type.clone();
			}
			if let Some(skill_path) = entry.skill_path.clone() {
				let hash = entry.content_hash.clone().unwrap_or_default();
				baseline.insert(skill_path, (hash, "global".to_string()));
			}
		}
	}

	if let Some(root) = project_root {
		for (_name, entry) in skill::read_local_lock(Some(root)).skills {
			if !source_matches(want, &entry.source, None) {
				continue;
			}
			if source_type.is_empty() {
				source_type = entry.source_type.clone();
			}
			if let Some(skill_path) = entry.skill_path.clone() {
				baseline.insert(
					skill_path,
					(entry.computed_hash, "project".to_string()),
				);
			}
		}
	}

	(baseline, source_type)
}

/// Synchronous fetch → discover-all → classify. Runs on a blocking thread.
fn diff_blocking(
	source: &str,
	git_ref: Option<&str>,
	token: Option<&str>,
	baseline: &Baseline,
) -> DiffOutcome {
	let source_ref = SourceRef {
		source: source.to_string(),
		ref_: git_ref.map(|s| s.to_string()),
	};
	let fetched = match GitFetcher.fetch(&source_ref, token) {
		Ok(repo) => repo,
		Err(FetchError::Auth) => return DiffOutcome::NeedsCredential,
		Err(FetchError::Network) => return DiffOutcome::FetchFailed,
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
				reason: None,
				installed_paths: Vec::new(),
			}),
			Some((base_hash, scope_label)) => {
				let skill_dir = d
					.full_path
					.parent()
					.map(Path::to_path_buf)
					.unwrap_or_else(|| root.to_path_buf());
				let (state, reason) = classify_installed(base_hash, &skill_dir);
				out.push(SourceSkillDiff {
					name: d.name,
					skill_path,
					description,
					version,
					author,
					state,
					reason,
					installed_paths: vec![scope_label.clone()],
				});
			}
		}
	}

	DiffOutcome::Ok(out)
}

/// Classify an already-installed skill by comparing its upstream folder hash to
/// the recorded baseline. An empty/unknown baseline (legacy lock) is reported as
/// current rather than nagging a false "update available".
fn classify_installed(
	base_hash: &str,
	skill_dir: &Path,
) -> (String, Option<String>) {
	if base_hash.is_empty() {
		return ("installedCurrent".to_string(), None);
	}
	match skill::compute_skill_folder_hash(skill_dir) {
		Ok(fresh) => match compare_known_hashes(base_hash, &fresh) {
			SkillUpdateStatus::UpToDate => {
				("installedCurrent".to_string(), None)
			}
			SkillUpdateStatus::UpdateAvailable { .. } => {
				("installedOutdated".to_string(), None)
			}
			SkillUpdateStatus::Uncheckable { reason } => {
				("uncheckable".to_string(), Some(reason_str(reason)))
			}
		},
		Err(_) => ("uncheckable".to_string(), Some("local".to_string())),
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
