//! Default git adapters for the update-check orchestrator: a treeless fetch
//! ([`GitFetcher`]) and an ls-refs tip resolver ([`GitRefResolver`]).
//!
//! No token is ever materialized into an error — `aghub_git` redacts URL
//! userinfo upstream — and any ref-resolution failure is a soft error so the
//! orchestrator falls through to the full fetch.

use std::sync::Arc;
use std::time::Duration;

use crate::{FetchError, FetchedRepo, Fetcher, RefResolver, SourceRef};

/// Per-fetch HTTP request timeout for the treeless clone. Generous enough for a
/// small skill repo but bounded so a stuck remote cannot hang the fetch.
const FETCH_HTTP_TIMEOUT: Duration = Duration::from_secs(30);

/// Production [`Fetcher`]: fetches the requested ref into a bare temp repo,
/// then materializes the tree into a separate temp directory for hashing/apply.
pub struct GitFetcher;

impl Fetcher for GitFetcher {
	fn fetch(
		&self,
		source_ref: &SourceRef,
		token: Option<&str>,
	) -> Result<FetchedRepo, FetchError> {
		let url = normalize_fetch_url(&source_ref.source)?;
		let creds = token
			.map(|token| aghub_git::Credentials::new("x-access-token", token));
		let (bare, oid) = aghub_git::fetch_ref_to_temp(
			&url,
			source_ref.ref_.as_deref(),
			creds.as_ref(),
			Some(FETCH_HTTP_TIMEOUT),
		)
		.map_err(classify_fetch_error)?;
		let repo = gix::open(bare.path()).map_err(|e| {
			classify_fetch_error(aghub_git::GitError::clone_failed(
				e.to_string(),
			))
		})?;
		let object = repo.find_object(oid).map_err(|e| {
			classify_fetch_error(aghub_git::GitError::clone_failed(
				e.to_string(),
			))
		})?;
		let tree = object.peel_to_tree().map_err(|e| {
			classify_fetch_error(aghub_git::GitError::clone_failed(
				e.to_string(),
			))
		})?;
		let materialized =
			tempfile::TempDir::new().map_err(|_| FetchError::Network)?;
		aghub_git::materialize_tree(&repo, tree.id, materialized.path())
			.map_err(|_| FetchError::Network)?;
		let root = materialized.path().to_path_buf();
		Ok(FetchedRepo {
			root,
			oid: oid.to_string(),
			_guard: Some(Arc::new(materialized)),
		})
	}
}

fn normalize_fetch_url(source: &str) -> Result<String, FetchError> {
	aghub_git::resolve_remote_source(source)
		.map(|resolved| resolved.clone_url)
		.map_err(|_| FetchError::Network)
}

fn classify_fetch_error(e: aghub_git::GitError) -> FetchError {
	let msg = e.to_string();
	let lower = msg.to_lowercase();
	if lower.contains("auth")
		|| lower.contains("401")
		|| lower.contains("403")
		|| lower.contains("credential")
	{
		FetchError::Auth
	} else {
		FetchError::Network
	}
}

/// Production [`RefResolver`]: a git ref advertisement (ls-refs, no object
/// download) resolving the tip OID of the requested branch/tag/default-branch.
/// Any error (incl. ref-not-found) maps to a soft failure so the orchestrator
/// falls through to the full fetch.
pub struct GitRefResolver;

impl RefResolver for GitRefResolver {
	fn resolve(
		&self,
		source_ref: &SourceRef,
		token: Option<&str>,
	) -> Result<String, FetchError> {
		let url = normalize_fetch_url(&source_ref.source)?;
		let mut opts = aghub_git::RemoteOptions::new(&url);
		if let Some(token) = token {
			opts = opts.with_credentials("x-access-token", token);
		}
		aghub_git::resolve_ref_oid(opts, source_ref.ref_.as_deref())
			.map_err(classify_fetch_error)?
			.ok_or(FetchError::Network)
	}
}
