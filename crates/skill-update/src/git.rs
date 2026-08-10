//! Default git adapters for the update-check orchestrator: a selection-scoped
//! fetch ([`GitFetcher`]) and a no-object tip resolver ([`GitRefResolver`]).
//!
//! No token is ever materialized into an error — `aghub_git` redacts URL
//! userinfo upstream — and any ref-resolution failure is a soft error so the
//! orchestrator falls through to the full fetch.

use std::sync::Arc;

use crate::repository::{
	skill_repo_to_fetch_error, FetchSelection, SkillRepository,
};
use crate::{FetchError, FetchedRepo, Fetcher, RefResolver, SourceRef};

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
	repo: Arc<SkillRepository>,
}

impl GitFetcher {
	pub fn new() -> Self {
		Self {
			repo: Arc::new(SkillRepository::new()),
		}
	}

	/// A [`RefResolver`] over THIS fetcher's repository, so the tip the preflight
	/// reads is resolved by the same composite (and the same token context) that
	/// a following fetch would use. Callers that want the preflight must build it
	/// from the fetcher rather than standing up a second resolver.
	pub fn ref_resolver(&self) -> GitRefResolver {
		GitRefResolver {
			repo: Arc::clone(&self.repo),
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

/// Production [`RefResolver`]: the tip OID of the requested
/// branch/tag/default-branch via [`SkillRepository::resolve_tip`]. Any error maps
/// to a soft failure so the orchestrator falls through to the full fetch.
///
/// It resolves through the repository rather than owning a ref advertisement of
/// its own because on github.com REST answers in one request on the pooled HTTP
/// client, while a `git ls-refs` handshake — cheap in BYTES, expensive in TIME —
/// costs a fresh TCP+TLS connection plus the whole heads+tags advertisement,
/// measured at ~0.6s per source, every time. The preflight runs for EVERY source
/// group, including the all-clear case where nothing else touches the network, so
/// that difference was most of a check's wall clock.
///
/// Build it with [`GitFetcher::ref_resolver`] so it shares the fetcher's
/// repository: the same fallback owner and the same token context decide the tip
/// and the fetch.
pub struct GitRefResolver {
	repo: Arc<SkillRepository>,
}

impl RefResolver for GitRefResolver {
	fn resolve(
		&self,
		source_ref: &SourceRef,
		token: Option<&str>,
	) -> Result<String, FetchError> {
		self.repo
			.resolve_tip(source_ref, token)
			.map_err(skill_repo_to_fetch_error)
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
