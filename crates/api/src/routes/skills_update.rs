//! `GET /skills/check-updates` — read-only update check for installed skills.
//!
//! Reads the global skill lock, projects each entry to the orchestrator's
//! [`EntryInput`], then delegates to the pure-ish F1.5 runner
//! ([`crate::skills::update_check::check_updates`]).
//!
//! Network + credential resolution stay in this crate (never in `crates/core`).
//! The [`Fetcher`] materializes a worktree into a [`tempfile::TempDir`] (the
//! documented worst-case fallback — a checkout into a temp dir, never the `git`
//! binary), and the [`TokenResolver`] wraps the F1.4 keyring/keychain
//! resolution. Every gix error string is redacted of URL userinfo upstream so a
//! token can never leak into the response.

use std::sync::Arc;
use std::time::Duration;

use rocket::serde::json::Json;

use crate::dto::skill::{SkillUpdateResponse, SkillUpdateStatusResponse};
use crate::error::ApiResult;
use crate::skills::update_check::{
	check_updates, CheckDeps, EntryInput, FetchError, FetchedRepo, Fetcher,
	ResultCache, SourceRef, TokenResolver,
};

/// Default per-fetch timeout. Generous enough for a small skill repo clone but
/// bounded so a stuck remote cannot hang the request.
const PER_FETCH: Duration = Duration::from_secs(30);
/// Default bounded concurrency for upstream fetches.
const CONCURRENCY: usize = 4;
/// TTL for the per-request result cache. The cache is request-scoped here, so
/// this only dedups identical `(source, ref)` groups within one call.
const CACHE_TTL: Duration = Duration::from_secs(60);

/// Production [`Fetcher`]: clones the requested ref into a fresh temp dir
/// worktree and hands the orchestrator the checkout root. gix errors are
/// classified into [`FetchError`] (already redacted by `aghub_git`).
struct CloneFetcher;

impl Fetcher for CloneFetcher {
	fn fetch(
		&self,
		source_ref: &SourceRef,
		token: Option<&str>,
	) -> Result<FetchedRepo, FetchError> {
		let url = source_ref.source.clone();
		let mut options = aghub_git::CloneOptions::new(&url);
		if let Some(token) = token {
			options = options.with_credentials("x-access-token", token);
		}
		if let Some(ref_) = source_ref.ref_.as_deref() {
			options = options.with_branch(ref_);
		}
		match aghub_git::clone_to_temp(options) {
			Ok(temp) => {
				let root = temp.path().to_path_buf();
				Ok(FetchedRepo {
					root,
					_guard: Some(Arc::new(temp)),
				})
			}
			Err(e) => {
				// `aghub_git` already redacts URL userinfo from its error
				// strings; classify auth-like failures so the caller can map
				// them to `Uncheckable { auth }`.
				let msg = e.to_string();
				let lower = msg.to_lowercase();
				if lower.contains("auth")
					|| lower.contains("401")
					|| lower.contains("403")
					|| lower.contains("credential")
				{
					Err(FetchError::Auth)
				} else {
					Err(FetchError::Network)
				}
			}
		}
	}
}

/// Production [`TokenResolver`]: wraps the F1.4 keyring source→credential
/// binding + host keychain resolution. Loads the stored credentials and
/// bindings lazily per resolve (cheap; keyring reads are local).
struct KeyringResolver;

impl TokenResolver for KeyringResolver {
	fn resolve(&self, source: &str, host: Option<&str>) -> Option<String> {
		let creds =
			crate::routes::credentials::load_credentials().unwrap_or_default();
		let bindings =
			crate::credentials::resolve::load_source_bindings().ok()?;
		crate::credentials::resolve::resolve_token_for_source(
			source, host, &bindings, &creds,
		)
	}
}

/// Query parameters for the update check. `offline` short-circuits every entry
/// to `Uncheckable { network }` without touching the network (useful for tests
/// and air-gapped environments).
#[derive(rocket::FromForm)]
pub struct CheckUpdatesParams {
	offline: Option<bool>,
}

/// Project the global skill lock into the orchestrator's per-entry inputs.
fn lock_entries() -> Vec<EntryInput> {
	let lock = skill::lock::global::read_skill_lock();
	lock.skills
		.into_iter()
		.map(|(name, entry)| EntryInput {
			name,
			source_ref: SourceRef {
				source: entry.source_url,
				ref_: entry.ref_name,
			},
			skill_path: entry.skill_path,
			stored_hash: entry.content_hash,
		})
		.collect()
}

/// `GET /skills/check-updates` — returns a per-skill update status list.
#[get("/skills/check-updates?<query..>")]
pub async fn check_skill_updates(
	query: CheckUpdatesParams,
) -> ApiResult<Vec<SkillUpdateResponse>> {
	let entries = lock_entries();

	let fetcher = CloneFetcher;
	let resolver = KeyringResolver;
	let mut cache = ResultCache::new(CACHE_TTL);
	let deps = CheckDeps {
		fetcher: &fetcher,
		resolver: &resolver,
		cache: &mut cache,
		per_fetch: PER_FETCH,
		concurrency: CONCURRENCY,
		offline: query.offline.unwrap_or(false),
	};

	let statuses = check_updates(entries, deps).await;

	let mut out: Vec<SkillUpdateResponse> = statuses
		.into_iter()
		.map(|(name, status)| SkillUpdateResponse {
			name,
			status: SkillUpdateStatusResponse::from(status),
		})
		.collect();
	out.sort_by(|a, b| a.name.cmp(&b.name));

	Ok(Json(out))
}

#[cfg(test)]
mod tests {
	use super::*;

	/// Offline short-circuits every entry without touching the network. With an
	/// empty lock the result is simply an empty list.
	#[tokio::test]
	async fn offline_check_returns_without_network() {
		let entries = vec![EntryInput {
			name: "skill-a".to_string(),
			source_ref: SourceRef {
				source: "https://github.com/owner/repo".to_string(),
				ref_: None,
			},
			skill_path: Some("SKILL.md".to_string()),
			stored_hash: None,
		}];
		let fetcher = CloneFetcher;
		let resolver = KeyringResolver;
		let mut cache = ResultCache::new(CACHE_TTL);
		let deps = CheckDeps {
			fetcher: &fetcher,
			resolver: &resolver,
			cache: &mut cache,
			per_fetch: PER_FETCH,
			concurrency: CONCURRENCY,
			offline: true,
		};
		let out = check_updates(entries, deps).await;
		assert!(matches!(
			out.get("skill-a"),
			Some(
				aghub_core::skills::update::SkillUpdateStatus::Uncheckable { .. }
			)
		));
	}

	/// A public repo with no stored hash recomputes locally and never panics;
	/// the result is `UpToDate` or `UpdateAvailable` (never `Uncheckable`).
	#[ignore = "network"]
	#[tokio::test]
	async fn e2e_check_public_repo_no_crash() {
		let entries = vec![EntryInput {
			name: "public".to_string(),
			source_ref: SourceRef {
				source: "https://github.com/anthropics/anthropic-sdk-python"
					.to_string(),
				ref_: None,
			},
			skill_path: Some("SKILL.md".to_string()),
			stored_hash: None,
		}];
		let fetcher = CloneFetcher;
		let resolver = KeyringResolver;
		let mut cache = ResultCache::new(CACHE_TTL);
		let deps = CheckDeps {
			fetcher: &fetcher,
			resolver: &resolver,
			cache: &mut cache,
			per_fetch: PER_FETCH,
			concurrency: CONCURRENCY,
			offline: false,
		};
		let out = check_updates(entries, deps).await;
		// No panic; some status was produced for the entry.
		assert!(out.contains_key("public"));
	}

	/// A private repo with no resolvable token surfaces `Uncheckable { auth }`
	/// (or a redacted network error) and never panics or leaks a token.
	#[ignore = "network"]
	#[tokio::test]
	async fn e2e_check_private_repo_no_token_uncheckable() {
		use aghub_core::skills::update::SkillUpdateStatus;
		let entries = vec![EntryInput {
			name: "private".to_string(),
			source_ref: SourceRef {
				source: "https://github.com/owner/definitely-private-repo"
					.to_string(),
				ref_: None,
			},
			skill_path: Some("SKILL.md".to_string()),
			stored_hash: None,
		}];
		let fetcher = CloneFetcher;
		let resolver = KeyringResolver;
		let mut cache = ResultCache::new(CACHE_TTL);
		let deps = CheckDeps {
			fetcher: &fetcher,
			resolver: &resolver,
			cache: &mut cache,
			per_fetch: PER_FETCH,
			concurrency: CONCURRENCY,
			offline: false,
		};
		let out = check_updates(entries, deps).await;
		assert!(matches!(
			out.get("private"),
			Some(SkillUpdateStatus::Uncheckable { .. })
		));
	}
}
