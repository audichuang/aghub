//! Default git adapters for the update-check orchestrator: a selection-scoped
//! fetch ([`GitFetcher`]) and an ls-refs tip resolver ([`GitRefResolver`]).
//!
//! No token is ever materialized into an error — `aghub_git` redacts URL
//! userinfo upstream — and any ref-resolution failure is a soft error so the
//! orchestrator falls through to the full fetch.

use crate::repository::{
	skill_repo_to_fetch_error, FetchSelection, SkillRepository,
};
use crate::{
	https_only_token, FetchError, FetchedRepo, Fetcher, RefResolver, SourceRef,
};

/// Production [`Fetcher`]: resolves via [`SkillRepository`]
/// (REST→gix→system-git single owner) then materializes only the requested
/// selection.
///
/// Holds ONE [`SkillRepository`] for the fetcher's lifetime so that repeated
/// fetches through the same instance reuse its per-snapshot caches. A source
/// diff fetches once per ref-cohort, and cohorts that resolve to the SAME
/// commit (typically `ref=Some("main")` alongside `ref=None`, where `None`
/// means "the default branch" and the default branch IS `main`) would otherwise
/// re-download an identical tree — measured at ~2.9s of pure duplication per
/// source.
///
/// Construct one per request and drop it there. It must NEVER become a
/// process-wide singleton: the REST backend's per-repo context holds the token
/// it was built with, so a shared instance would let an unauthenticated
/// request — or one for a different host — reuse another caller's credential.
pub struct GitFetcher {
	repo: SkillRepository,
}

impl GitFetcher {
	pub fn new() -> Self {
		Self {
			repo: SkillRepository::new(),
		}
	}
}

impl Default for GitFetcher {
	fn default() -> Self {
		Self::new()
	}
}

impl Fetcher for GitFetcher {
	fn fetch(
		&self,
		source_ref: &SourceRef,
		token: Option<&str>,
		selection: FetchSelection<'_>,
	) -> Result<FetchedRepo, FetchError> {
		let snap = self
			.repo
			.resolve(source_ref, token)
			.map_err(skill_repo_to_fetch_error)?;
		self.repo
			.fetch(&snap, selection)
			.map_err(skill_repo_to_fetch_error)
	}
}

fn normalize_fetch_url(source: &str) -> Result<String, FetchError> {
	aghub_git::resolve_remote_source(source)
		.map(|resolved| resolved.clone_url)
		.map_err(|e| FetchError::network(e.to_string()))
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
		FetchError::network(msg)
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
			.ok_or_else(|| {
				FetchError::network(match source_ref.ref_.as_deref() {
					Some(r) => format!("remote has no ref '{r}'"),
					None => "remote advertised no default branch".to_string(),
				})
			})
	}
}

#[cfg(test)]
mod tests {
	use crate::https_only_token;

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
