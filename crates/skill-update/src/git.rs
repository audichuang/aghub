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
		let creds = https_only_token(&url, token)
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
		let upstream_commit_time = read_commit_time(&repo, oid);
		Ok(FetchedRepo {
			root,
			oid: oid.to_string(),
			upstream_commit_time,
			_guard: Some(Arc::new(materialized)),
		})
	}
}

/// Best-effort RFC 3339 author-time of the tip commit at `oid`.
///
/// Returns `None` on any failure (object missing, not a commit, unparsable
/// time) so a missing timestamp never aborts the fetch.
fn read_commit_time(
	repo: &gix::Repository,
	oid: gix::ObjectId,
) -> Option<String> {
	let commit = repo.find_object(oid).ok()?.try_into_commit().ok()?;
	// In gix 0.84 `SignatureRef.time` is the raw `&str`; `.time()` parses it
	// into a `gix_date::Time` exposing `seconds` (unix epoch) and `offset`
	// (seconds east of UTC).
	let time = commit.author().ok()?.time().ok()?;
	let offset = chrono::FixedOffset::east_opt(time.offset)?;
	let dt = chrono::DateTime::from_timestamp(time.seconds, 0)?
		.with_timezone(&offset);
	Some(dt.to_rfc3339())
}

fn normalize_fetch_url(source: &str) -> Result<String, FetchError> {
	aghub_git::resolve_remote_source(source)
		.map(|resolved| resolved.clone_url)
		.map_err(|_| FetchError::Network)
}

/// Tokens are HTTPS-only: `aghub_git::inject_credentials` rejects every
/// other scheme, so passing a token alongside an ssh/scp/git URL turns a
/// fetch that could succeed over the transport's own auth (ssh agent) into
/// a guaranteed error. Drop the token instead and let the unauthenticated
/// attempt stand.
fn https_only_token<'t>(url: &str, token: Option<&'t str>) -> Option<&'t str> {
	token.filter(|_| url.starts_with("https://"))
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
		if let Some(token) = https_only_token(&url, token) {
			opts = opts.with_credentials("x-access-token", token);
		}
		aghub_git::resolve_ref_oid(opts, source_ref.ref_.as_deref())
			.map_err(classify_fetch_error)?
			.ok_or(FetchError::Network)
	}
}

#[cfg(test)]
mod tests {
	use super::https_only_token;

	#[test]
	fn token_is_dropped_for_non_https_urls() {
		let token = Some("tok");
		assert_eq!(
			https_only_token("https://github.com/o/r.git", token),
			Some("tok")
		);
		for url in [
			"git@github.com:o/r.git",
			"ssh://git@github.com/o/r.git",
			"git://github.com/o/r.git",
			"http://github.com/o/r.git",
		] {
			assert_eq!(
				https_only_token(url, token),
				None,
				"token must not be attached to {url}"
			);
		}
	}
}
