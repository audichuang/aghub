//! Unified "Sources" endpoints.
//!
//! - `GET /skills/sources` — offline, lock-only: groups installed skills by
//!   source per scope and reports a count + credential availability.
//! - `GET /skills/sources/diff` — fetches a single source ONCE and reports each
//!   of its skills as not-installed / installed-current / installed-outdated /
//!   uncheckable, so the UI can offer "install the new ones".
//!
//! The Sources list + diff-classification domain logic lives in
//! [`skill_update::sources`]; these handlers are a thin Rocket boundary that
//! resolves scope, injects a Keychain-backed [`TokenResolver`], and maps the
//! domain types to DTOs.

use rocket::http::Status;
use rocket::serde::json::Json;

use crate::credentials::resolve::{
	load_source_bindings, resolve_token_for_source,
};
use crate::dto::sources::{
	CredentialStatus, SourceDiffResponse, SourceSkillDiff, SourceSkillStateDto,
	SourceSummaryResponse, SourcesListResponse,
};
use crate::error::{ApiError, ApiResult};
use crate::extractors::{ResolvedScope, ScopeParams};
use crate::routes::credentials::load_credentials;
use skill_update::sources::{
	self, SourceDiffDeps, SourceDiffInput, SourceDiffOutcome, SourceScope,
	SourceScopeKind, SourceSkillDiff as DomainSkillDiff, SourceSummary,
};
use skill_update::{
	keychain_host_for_source, FetchError, FetchedRepo, Fetcher, GitFetcher,
	SourceRef,
};

/// Resolve a request scope into the domain's per-scope list. Mirrors the old
/// route: `Global` → `[Global]`, `Project` → `[Project]`, `All` → `[Global]`
/// plus the project scope when a project root is known.
fn scopes_for(resolved: &ResolvedScope) -> Vec<SourceScope> {
	match resolved {
		ResolvedScope::Global => vec![SourceScope::Global],
		ResolvedScope::Project { root } => {
			vec![SourceScope::Project { root: root.clone() }]
		}
		ResolvedScope::All { project_root } => {
			let mut scopes = vec![SourceScope::Global];
			if let Some(root) = project_root {
				scopes.push(SourceScope::Project { root: root.clone() });
			}
			scopes
		}
	}
}

/// Keychain-backed token resolver: re-fetches private sources using the same
/// source→credential binding logic as the rest of the API.
struct KeyringResolver;

impl skill_update::TokenResolver for KeyringResolver {
	fn resolve(&self, source: &str, _host: Option<&str>) -> Option<String> {
		token_for_source(source)
	}
}

fn token_for_source(source: &str) -> Option<String> {
	let bindings = load_source_bindings().unwrap_or_default();
	let creds = load_credentials().unwrap_or_default();
	let host = keychain_host_for_source(source);
	resolve_token_for_source(source, host.as_deref(), &bindings, &creds)
}

/// Production fetch is [`GitFetcher`]. Under `cfg(test)` an env hook
/// (`AGHUB_TEST_SOURCE_FETCH_ROOT`) lets the route-level HTTP-shape tests point
/// the diff at a local dir instead of hitting the network — preserving the old
/// route's `test_fetch_source_from_env` behavior now that the route delegates
/// to the shared `diff_source`.
struct ApiFetcher;

impl Fetcher for ApiFetcher {
	fn fetch(
		&self,
		source_ref: &SourceRef,
		token: Option<&str>,
	) -> Result<FetchedRepo, FetchError> {
		#[cfg(test)]
		if let Some(root) = std::env::var_os("AGHUB_TEST_SOURCE_FETCH_ROOT") {
			let root = std::path::PathBuf::from(root);
			return if root.is_dir() {
				Ok(FetchedRepo {
					root,
					oid: "test-fetch-root".to_string(),
					upstream_commit_time: None,
					_guard: None,
				})
			} else {
				Err(FetchError::Network)
			};
		}
		GitFetcher.fetch(source_ref, token)
	}
}

// ─────────────────────────── GET /skills/sources ───────────────────────────

#[get("/skills/sources?<query..>")]
pub fn list_sources(query: ScopeParams) -> ApiResult<SourcesListResponse> {
	let resolved = query.resolve()?;
	let scopes = scopes_for(&resolved);

	let sources = sources::list_sources(sources::SourceListInput { scopes })
		.into_iter()
		.map(map_summary_to_dto)
		.collect();

	Ok(Json(SourcesListResponse { sources }))
}

fn map_summary_to_dto(summary: SourceSummary) -> SourceSummaryResponse {
	let scope = match summary.scope {
		SourceScopeKind::Global => "global",
		SourceScopeKind::Project => "project",
	}
	.to_string();
	SourceSummaryResponse {
		source: summary.source,
		source_url: summary.source_url,
		source_type: summary.source_type,
		scope,
		skill_count: summary.skill_count,
		is_private: false,
		credential_status: CredentialStatus::NotRequired,
	}
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

#[get("/skills/sources/diff?<query..>")]
pub async fn diff_source(
	query: SourceDiffQuery,
) -> ApiResult<SourceDiffResponse> {
	let scope_params = ScopeParams {
		scope: query.scope.clone(),
		project_root: query.project_root.clone(),
	};
	let resolved = scope_params.resolve()?;
	let scopes = scopes_for(&resolved);
	let source = query.source.trim().to_string();

	// Fetch + discover + classify on a blocking thread (sync git IO, and the
	// materialized temp dir must outlive discovery + hashing). The shared
	// `diff_source` tries an unauthenticated fetch first and only resolves a
	// Keychain token after a failure.
	let input = SourceDiffInput {
		source: source.clone(),
		git_ref: query.git_ref.clone(),
		scopes,
	};
	let outcome = rocket::tokio::task::spawn_blocking(move || {
		sources::diff_source(
			input,
			SourceDiffDeps {
				fetcher: &ApiFetcher,
				resolver: &KeyringResolver,
			},
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
		SourceDiffOutcome::Ok { git_ref, skills } => {
			Ok(Json(SourceDiffResponse {
				source,
				git_ref,
				session_id: None,
				needs_credential: false,
				skills: skills.into_iter().map(map_diff_to_dto).collect(),
			}))
		}
		SourceDiffOutcome::NeedsCredential { git_ref } => {
			Ok(Json(SourceDiffResponse {
				source,
				git_ref,
				session_id: None,
				needs_credential: true,
				skills: Vec::new(),
			}))
		}
		SourceDiffOutcome::FetchFailed => Err(ApiError::new(
			Status::BadGateway,
			"Failed to fetch source repository",
			"SOURCE_FETCH_FAILED",
		)),
		SourceDiffOutcome::UncheckableSource { git_ref, .. } => {
			Ok(Json(SourceDiffResponse {
				source,
				git_ref,
				session_id: None,
				needs_credential: false,
				skills: Vec::new(),
			}))
		}
	}
}

fn map_diff_to_dto(d: DomainSkillDiff) -> SourceSkillDiff {
	SourceSkillDiff {
		name: d.name,
		skill_path: d.skill_path,
		description: d.description,
		version: d.version,
		author: d.author,
		state: SourceSkillStateDto::from(&d.state),
		previous_name: d.previous_name,
		reason: d.reason,
		installed_paths: d.installed_paths,
		upstream_commit_time: d.upstream_commit_time,
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use rocket::http::Status;
	use rocket::local::blocking::Client;
	use rocket::routes;
	use serde_json::Value;
	use std::fs;
	use std::path::Path;
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
}
