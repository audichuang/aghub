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
use crate::routes::skills_update::{installed_skill_roots, GitFetcher};
use crate::skills::update_check::{
	keychain_host_for_source, FetchError, Fetcher, SourceRef,
};
use aghub_core::models::ResourceScope;
use aghub_core::skills::update::{
	compare_known_hashes, precheck_source, SkillUpdateStatus, UncheckableReason,
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
		for (name, entry) in skill::get_all_locked_skills() {
			if !source_matches(want, &entry.source, Some(&entry.source_url)) {
				continue;
			}
			if source_type.is_empty() {
				source_type = entry.source_type.clone();
			}
			if let Some(skill_path) = entry.skill_path.clone() {
				let hash = entry.content_hash.clone().unwrap_or_default();
				baseline.insert(
					skill_path,
					BaselineEntry {
						stored_hash: hash,
						local_hashes: local_hashes_for_installed(
							&name,
							ResourceScope::GlobalOnly,
							None,
						),
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
			if let Some(skill_path) = entry.skill_path.clone() {
				baseline.insert(
					skill_path,
					BaselineEntry {
						stored_hash: entry.computed_hash,
						local_hashes: local_hashes_for_installed(
							&name,
							ResourceScope::ProjectOnly,
							Some(root),
						),
						scope_label: "project".to_string(),
					},
				);
			}
		}
	}

	(baseline, source_type)
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
				reason: None,
				installed_paths: Vec::new(),
			}),
			Some(entry) => {
				let skill_dir = discovered_skill_root(&d.full_path);
				let (state, reason) = classify_installed(entry, &skill_dir);
				out.push(SourceSkillDiff {
					name: d.name,
					skill_path,
					description,
					version,
					author,
					state,
					reason,
					installed_paths: vec![entry.scope_label.clone()],
				});
			}
		}
	}

	DiffOutcome::Ok(out)
}

fn discovered_skill_root(path: &Path) -> std::path::PathBuf {
	let is_skill_file = path
		.file_name()
		.is_some_and(|name| name == std::ffi::OsStr::new("SKILL.md"));
	if is_skill_file {
		path.parent()
			.map(Path::to_path_buf)
			.unwrap_or_else(|| path.to_path_buf())
	} else {
		path.to_path_buf()
	}
}

fn fetch_source_lazily_auth(
	source_ref: &SourceRef,
) -> Result<crate::skills::update_check::FetchedRepo, LazyFetchError> {
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

	match compare_known_hashes(base_hash, &fresh) {
		SkillUpdateStatus::UpToDate => ("installedCurrent".to_string(), None),
		SkillUpdateStatus::UpdateAvailable { .. } => {
			("installedOutdated".to_string(), None)
		}
		SkillUpdateStatus::Uncheckable { reason } => {
			("uncheckable".to_string(), Some(reason_str(reason)))
		}
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
	use tempfile::tempdir;

	#[test]
	fn classify_prefers_local_hash_over_stale_stored_hash() {
		let dir = tempdir().unwrap();
		fs::write(dir.path().join("SKILL.md"), b"description: x").unwrap();
		let fresh = skill::compute_skill_folder_hash(dir.path()).unwrap();
		let entry = BaselineEntry {
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
	fn discovered_skill_root_keeps_directory_paths() {
		let dir = tempdir().unwrap();
		let skill_dir = dir.path().join("skills/foo");

		assert_eq!(discovered_skill_root(&skill_dir), skill_dir);
	}

	#[test]
	fn discovered_skill_root_accepts_skill_file_paths() {
		let dir = tempdir().unwrap();
		let skill_dir = dir.path().join("skills/foo");
		let skill_file = skill_dir.join("SKILL.md");

		assert_eq!(discovered_skill_root(&skill_file), skill_dir);
	}
}
