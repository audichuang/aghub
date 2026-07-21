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

use crate::credentials::forwarding::ForwardedGitTokens;
use crate::credentials::source_auth::SourceAuth;
use crate::dto::sources::{
	CredentialStatus, SourceDiffResponse, SourceSkillDiff, SourceSkillStateDto,
	SourceSummaryResponse, SourcesListResponse,
};
use crate::error::{ApiError, ApiResult};
use crate::extractors::{ResolvedScope, ScopeParams, TrustedLocalOrigin};
use skill_update::sources::{
	self, SourceDiffDeps, SourceDiffInput, SourceDiffOutcome, SourceScope,
	SourceScopeKind, SourceSkillDiff as DomainSkillDiff, SourceSummary,
};
use skill_update::{FetchError, FetchedRepo, Fetcher, GitFetcher, SourceRef};

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

/// Test-only recorder for the token the diff fetch was invoked with. The diff
/// runs on a `spawn_blocking` thread distinct from the test thread, so this is a
/// process-global slot (not a thread-local). The `AGHUB_TEST_SOURCE_FETCH_ROOT`
/// tests serialize on the crate-wide env lock, so a single slot is sufficient.
#[cfg(test)]
static LAST_FETCH_TOKEN: std::sync::Mutex<Option<Option<String>>> =
	std::sync::Mutex::new(None);

/// Production fetch is [`GitFetcher`]. Under `cfg(test)` an env hook
/// (`AGHUB_TEST_SOURCE_FETCH_ROOT`) lets the route-level HTTP-shape tests point
/// the diff at a local dir instead of hitting the network — preserving the old
/// route's `test_fetch_source_from_env` behavior now that the route delegates
/// to the shared `diff_source`. In test mode it also records the token the fetch
/// was called with so the forwarding tests can assert which credential reached
/// the fetch.
struct ApiFetcher;

impl Fetcher for ApiFetcher {
	fn fetch(
		&self,
		source_ref: &SourceRef,
		token: Option<&str>,
		selection: skill_update::FetchSelection<'_>,
	) -> Result<FetchedRepo, FetchError> {
		#[cfg(test)]
		if let Some(root) = std::env::var_os("AGHUB_TEST_SOURCE_FETCH_ROOT") {
			// When `AGHUB_TEST_REQUIRE_TOKEN` is set, a missing token fails so the
			// forwarding tests can observe which credential reaches the fetch.
			let require_token =
				std::env::var_os("AGHUB_TEST_REQUIRE_TOKEN").is_some();
			if require_token && token.is_none() {
				return Err(FetchError::Auth);
			}
			*LAST_FETCH_TOKEN.lock().unwrap_or_else(|e| e.into_inner()) =
				Some(token.map(str::to_string));
			let root = std::path::PathBuf::from(root);
			return if root.is_dir() {
				Ok(FetchedRepo {
					root,
					snapshot: aghub_git::RepoSnapshot {
						commit_oid: "test-fetch-root".to_string(),
						tree_oid: "test-fetch-tree".to_string(),
						commit_time: None,
					},
					_guard: None,
				})
			} else {
				Err(FetchError::Network)
			};
		}
		GitFetcher.fetch(source_ref, token, selection)
	}
}

// ─────────────────────────── GET /skills/sources ───────────────────────────

#[get("/skills/sources?<query..>")]
pub fn list_sources(
	_origin: TrustedLocalOrigin,
	query: ScopeParams,
) -> ApiResult<SourcesListResponse> {
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
	forwarded: ForwardedGitTokens,
	_origin: TrustedLocalOrigin,
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
	// `diff_source` resolves forwarded/keyring credentials before its fetch.
	let input = SourceDiffInput {
		source: source.clone(),
		git_ref: query.git_ref.clone(),
		scopes,
	};
	// Forwarded tokens (header) take precedence over the local keyring: a remote
	// api has no keyring of its own, so the controller-resolved token must win.
	// An absent/empty header degrades to the keyring path (backward compatible).
	let resolver = SourceAuth::load(forwarded).await;
	let outcome = rocket::tokio::task::spawn_blocking(move || {
		sources::diff_source(
			input,
			SourceDiffDeps {
				fetcher: &ApiFetcher,
				resolver: &resolver,
			},
		)
	})
	.await
	.map_err(|e| {
		ApiError::from_join_error(
			e,
			"Source diff task failed",
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

		fn set_str(key: &'static str, value: &str) -> Self {
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

	// ─── remote git-credential forwarding (X-Aghub-Git-Tokens) ──────────────

	/// Base64-encode the forward header in the `{ token, origin }` wire shape.
	///
	/// The origin is derived from each source's resolved clone URL, mirroring
	/// what the controller forwards. This keeps the route-level tests exercising
	/// the real decode + origin-pin path end-to-end.
	fn encode_tokens(pairs: &[(&str, &str)]) -> String {
		use base64::engine::general_purpose::STANDARD as BASE64;
		use base64::Engine as _;
		let map: serde_json::Map<String, serde_json::Value> = pairs
			.iter()
			.map(|(source, token)| {
				let origin = aghub_git::resolve_remote_source(source)
					.ok()
					.and_then(|r| {
						crate::credentials::origin::origin_of(&r.clone_url)
					})
					.map(|o| {
						serde_json::json!({
							"scheme": o.scheme,
							"host": o.host,
							"port": o.port,
						})
					})
					.unwrap_or(serde_json::Value::Null);
				(
					(*source).to_string(),
					serde_json::json!({ "token": token, "origin": origin }),
				)
			})
			.collect();
		BASE64.encode(serde_json::to_vec(&map).unwrap())
	}

	/// Read + clear the recorded fetch token (the token the diff fetch was
	/// actually invoked with). `Some(Some(t))` = fetched with token `t`;
	/// `Some(None)` = fetched anonymously; `None` = fetch never recorded.
	fn take_recorded_token() -> Option<Option<String>> {
		super::LAST_FETCH_TOKEN
			.lock()
			.unwrap_or_else(|e| e.into_inner())
			.take()
	}

	/// Seed a single-skill upstream + a lock entry for `source`, returning the
	/// upstream temp dir (kept alive by the caller for the fetch root).
	fn seed_source(source: &str) -> TempDir {
		let upstream = tempdir().unwrap();
		write_skill(upstream.path(), "skills/demo", "demo");
		skill::lock::add_skill_to_lock(
			"demo",
			global_entry(source, "skills/demo/SKILL.md"),
		)
		.unwrap();
		upstream
	}

	#[test]
	fn diff_with_forwarded_header_uses_forwarded_token_for_fetch() {
		let _global = GlobalLockGuard::new();
		let source = "owner/private-repo";
		let upstream = seed_source(source);

		let _fetch_root =
			EnvVarGuard::set("AGHUB_TEST_SOURCE_FETCH_ROOT", upstream.path());
		// Require the resolved token to reach the first fetch.
		let _require = EnvVarGuard::set_str("AGHUB_TEST_REQUIRE_TOKEN", "1");
		let _ = take_recorded_token();

		let client =
			Client::tracked(rocket::build().mount("/", routes![diff_source]))
				.expect("client");
		let header = encode_tokens(&[(source, "FWD-TOKEN")]);
		let response = client
			.get(format!("/skills/sources/diff?scope=global&source={source}"))
			.header(rocket::http::Header::new("X-Aghub-Git-Tokens", header))
			.dispatch();

		assert_eq!(response.status(), Status::Ok);
		let body = response.into_string().expect("response body");
		let value: Value = serde_json::from_str(&body).expect("valid JSON");
		// The forwarded token satisfied the auth-required fetch, so the source
		// is NOT reported as needing a credential.
		assert_eq!(value["needsCredential"], false);
		// The forwarded token is exactly what reached the fetch.
		assert_eq!(
			take_recorded_token(),
			Some(Some("FWD-TOKEN".to_string())),
			"the forwarded token must reach the fetch"
		);
	}

	#[test]
	fn diff_without_header_keeps_keyring_path() {
		let _global = GlobalLockGuard::new();
		let source = "owner/private-repo";
		let upstream = seed_source(source);

		let _fetch_root =
			EnvVarGuard::set("AGHUB_TEST_SOURCE_FETCH_ROOT", upstream.path());
		// Auth required, but NO forward header → the keyring resolver runs and
		// (with an empty test keyring) yields no token → needs_credential.
		let _require = EnvVarGuard::set_str("AGHUB_TEST_REQUIRE_TOKEN", "1");
		let _ = take_recorded_token();

		let client =
			Client::tracked(rocket::build().mount("/", routes![diff_source]))
				.expect("client");
		let response = client
			.get(format!("/skills/sources/diff?scope=global&source={source}"))
			.dispatch();

		assert_eq!(response.status(), Status::Ok);
		let body = response.into_string().expect("response body");
		let value: Value = serde_json::from_str(&body).expect("valid JSON");
		// No forwarded token, empty keyring → unchanged keyring behaviour.
		assert_eq!(value["needsCredential"], true);
		// The anonymous attempt fails before the test seam records a token.
		assert_eq!(
			take_recorded_token(),
			None,
			"no token should reach the fetch without a forward header"
		);
	}

	#[test]
	fn diff_does_not_attach_cross_origin_forwarded_token() {
		let _global = GlobalLockGuard::new();
		// The request is for a github.com source, but the header carries a
		// gitlab.com source token. Host-scoped matching keeps the two apart, so
		// the github.com diff must NOT pick up the gitlab token.
		let source = "owner/repo";
		let upstream = seed_source(source);

		let _fetch_root =
			EnvVarGuard::set("AGHUB_TEST_SOURCE_FETCH_ROOT", upstream.path());
		let _require = EnvVarGuard::set_str("AGHUB_TEST_REQUIRE_TOKEN", "1");
		let _ = take_recorded_token();

		let client =
			Client::tracked(rocket::build().mount("/", routes![diff_source]))
				.expect("client");
		// Token bound to a DIFFERENT origin (gitlab.com) than the request.
		let header =
			encode_tokens(&[("https://gitlab.com/owner/repo.git", "GL-TOKEN")]);
		let response = client
			.get(format!("/skills/sources/diff?scope=global&source={source}"))
			.header(rocket::http::Header::new("X-Aghub-Git-Tokens", header))
			.dispatch();

		assert_eq!(response.status(), Status::Ok);
		let body = response.into_string().expect("response body");
		let value: Value = serde_json::from_str(&body).expect("valid JSON");
		// The cross-origin token is not attached; with an empty keyring the
		// auth-required fetch yields needs_credential, NOT a token-fed success.
		assert_eq!(value["needsCredential"], true);
		assert_eq!(
			take_recorded_token(),
			None,
			"a token for a different origin must not reach the fetch"
		);
	}
}
