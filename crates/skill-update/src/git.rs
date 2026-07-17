//! Default git adapters for the update-check orchestrator: a selection-scoped
//! fetch ([`GitFetcher`]) and an ls-refs tip resolver ([`GitRefResolver`]).
//!
//! No token is ever materialized into an error — `aghub_git` redacts URL
//! userinfo upstream — and any ref-resolution failure is a soft error so the
//! orchestrator falls through to the full fetch.

use crate::repository::{
	skill_repo_to_fetch_error, FetchSelection, SkillRepository,
};
use crate::{FetchError, FetchedRepo, Fetcher, RefResolver, SourceRef};

/// Production [`Fetcher`]: resolves via [`SkillRepository`]
/// (REST→gix→system-git single owner) then materializes only the requested
/// selection.
pub struct GitFetcher;

impl Fetcher for GitFetcher {
	fn fetch(
		&self,
		source_ref: &SourceRef,
		token: Option<&str>,
		selection: FetchSelection<'_>,
	) -> Result<FetchedRepo, FetchError> {
		let repo = SkillRepository::new();
		let snap = repo
			.resolve(source_ref, token)
			.map_err(skill_repo_to_fetch_error)?;
		repo.fetch(&snap, selection)
			.map_err(skill_repo_to_fetch_error)
	}
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
