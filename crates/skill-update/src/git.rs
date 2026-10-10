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
use crate::{
	FetchError, FetchedRepo, Fetcher, PinnedSnapshot, RefResolver, SourceRef,
	TipObservation,
};

/// Production [`Fetcher`]: resolves via [`SkillRepository`]
/// (REST→gix→system-git single owner) then materializes only the requested
/// selection.
///
/// Holds ONE [`SkillRepository`] for its lifetime so repeated fetches reuse its
/// per-snapshot caches: ref-cohorts resolving to the SAME commit (`ref=None`
/// beside the `main` it names) would otherwise re-download an identical tree.
///
/// Construct one per request. NEVER a process-wide singleton: the REST
/// backend's per-repo context holds its token, so a shared instance would let
/// another request reuse that credential.
pub struct GitFetcher {
	repo: Arc<SkillRepository>,
}

impl GitFetcher {
	pub fn new() -> Self {
		Self {
			repo: Arc::new(SkillRepository::new()),
		}
	}

	/// Wrap an already-built repository — the seam route tests use to run the
	/// real fetch composite over injected backends.
	#[doc(hidden)]
	pub fn with_repository(repo: SkillRepository) -> Self {
		Self {
			repo: Arc::new(repo),
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
		let pinned = self
			.repo
			.resolve_pinned(source_ref, token)
			.map_err(skill_repo_to_fetch_error)?;
		self.repo
			.fetch_pinned(&pinned, selection)
			.map_err(skill_repo_to_fetch_error)
	}

	/// Skips resolution entirely: the claim already names the snapshot AND the
	/// backend that produced it, so this cannot buy the tip a second time nor be
	/// routed to another source's backend slot by the commit-oid-keyed memo.
	fn fetch_pinned(
		&self,
		_source_ref: &SourceRef,
		_token: Option<&str>,
		selection: FetchSelection<'_>,
		pinned: &PinnedSnapshot,
	) -> Result<FetchedRepo, FetchError> {
		self.repo
			.fetch_pinned(pinned, selection)
			.map_err(skill_repo_to_fetch_error)
	}
}

/// Production [`RefResolver`]: the tip OID of the requested
/// branch/tag/default-branch via [`SkillRepository::resolve_tip`]. Any error maps
/// to a soft failure so the orchestrator falls through to the full fetch.
///
/// It resolves through the repository rather than its own ref advertisement:
/// on github.com REST answers in one pooled request, while a `git ls-refs`
/// handshake costs a fresh TCP+TLS connection (~0.6s per source), and the
/// preflight runs for EVERY source group.
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
	) -> Result<TipObservation, FetchError> {
		self.repo
			.resolve_tip(source_ref, token)
			.map(|(commit_oid, pinned)| TipObservation { commit_oid, pinned })
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
