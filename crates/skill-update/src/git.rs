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
		let snapshot = aghub_git::RepoSnapshot {
			commit_oid: oid.to_string(),
			tree_oid: tree.id.to_string(),
			commit_time: read_commit_time(&repo, oid),
		};
		Ok(FetchedRepo {
			root,
			snapshot,
			_guard: Some(Arc::new(materialized)),
		})
	}
}

/// [`Fetcher`] that tries gix first and, only if that fails, falls back to the
/// system `git` binary (so OS credential helpers — Windows Credential Manager,
/// GCM, NTLM/Kerberos for TFS/Azure DevOps — can authenticate a private repo
/// that has no `GIT_PASSWORD`/keyring token).
///
/// For Kind-2 callers ONLY — those that pass the already-resolved final token
/// in a single `fetch(token)` (the check orchestrator, apply-update,
/// accept-rename). Because gix runs first WITH that token, `GIT_PASSWORD`
/// always takes precedence over system-git; the fallback only fires after the
/// token attempt fails (or when there is no token). It must NOT wrap the
/// unauth-first `fetch_source_with_resolver` (Kind 1) — that would fire
/// system-git before the token retry. See `fetch_source_with_resolver`, which
/// sequences the fallback explicitly after its retry instead.
pub struct GitFetcherWithFallback;

impl Fetcher for GitFetcherWithFallback {
	fn fetch(
		&self,
		source_ref: &SourceRef,
		token: Option<&str>,
	) -> Result<FetchedRepo, FetchError> {
		GitFetcher
			.fetch(source_ref, token)
			.or_else(|e| fetch_via_system_git(source_ref).map_err(|_| e))
	}
}

/// Last-resort fetch through the system `git` binary + OS credential helpers.
/// Returns `Err` (leaving the caller's original gix error to surface) unless
/// the URL is an https NON-github host and `git` is installed — github is left
/// to gix so we never shell out to a dev machine's `gh`/GCM helper, and the
/// https gate keeps ssh/other schemes on their own transport auth.
///
/// The clone is a full shallow checkout; downstream skill discovery skips
/// `.git` (gitignore-aware) and hashing is per-skill-folder, so the working
/// tree is used directly as the fetched root.
pub(crate) fn fetch_via_system_git(
	source_ref: &SourceRef,
) -> Result<FetchedRepo, FetchError> {
	let url = normalize_fetch_url(&source_ref.source)?;
	if !should_try_system_git(&url) {
		return Err(FetchError::Network);
	}
	let clone = aghub_git::system_git::clone_to_temp_system_git(
		&url,
		source_ref.ref_.as_deref(),
	)
	.map_err(classify_fetch_error)?;
	// snapshot is best-effort (a shallow clone still has HEAD); a miss only
	// loses the next preflight optimization, never correctness.
	let snapshot = gix::open(clone.path())
		.ok()
		.and_then(|repo| {
			let head = repo.head_id().ok()?.detach();
			let tree = repo.find_object(head).ok()?.peel_to_tree().ok()?;
			Some(aghub_git::RepoSnapshot {
				commit_oid: head.to_string(),
				tree_oid: tree.id.to_string(),
				commit_time: read_commit_time(&repo, head),
			})
		})
		.unwrap_or_default();
	let root = clone.path().to_path_buf();
	Ok(FetchedRepo {
		root,
		snapshot,
		_guard: Some(Arc::new(clone)),
	})
}

/// Gate for the system-git fallback: https + NON-github host + `git` installed.
/// The host check runs BEFORE `system_git_available()` (which shells out to
/// `git --version`) so a github source short-circuits without spawning a
/// process — keeps unit tests with github `owner/repo` sources hermetic.
fn should_try_system_git(url: &str) -> bool {
	url.starts_with("https://")
		&& host_of_url(url)
			.is_some_and(|h| h != "github.com" && !h.ends_with(".github.com"))
		&& aghub_git::system_git::system_git_available()
}

/// Lowercased host of an `scheme://[user@]host[:port]/…` URL (userinfo/port
/// stripped). `None` when there is no `://` or authority.
fn host_of_url(url: &str) -> Option<String> {
	let after_scheme = url.split_once("://")?.1;
	let authority = after_scheme.split(['/', '?', '#']).next()?;
	let authority = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
	let host = if let Some(rest) = authority.strip_prefix('[') {
		rest.split_once(']')?.0
	} else {
		authority.split(':').next()?
	};
	(!host.is_empty()).then(|| host.to_ascii_lowercase())
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
	use super::{host_of_url, https_only_token, should_try_system_git};

	/// The gix→system-git + OS-credential-helper fallback must survive the
	/// shallow-fetch change: a non-GitHub HTTPS host (TFS / Azure DevOps /
	/// self-hosted GitLab) is still admitted to the system-git path, while
	/// github and non-https are excluded. FAILS if the non-github fallback is
	/// dropped or the gate is broken.
	#[test]
	fn system_git_fallback_retained_for_non_github_https() {
		// github short-circuits BEFORE probing the git binary → never falls back
		// to a dev machine's gh/GCM helper.
		assert!(!should_try_system_git("https://github.com/o/r.git"));
		assert!(!should_try_system_git("https://api.github.com/o/r.git"));
		// Non-https keeps its own transport auth (ssh agent, etc.).
		assert!(!should_try_system_git("git@tfs.corp.local:o/r.git"));
		assert!(!should_try_system_git("ssh://git@tfs.corp.local/o/r.git"));
		// A non-github HTTPS host is admitted to the fallback exactly when a git
		// binary exists — i.e. it is NOT excluded the way github is. This is the
		// TFS/Azure/self-hosted-GitLab credential-helper path (Decision 13).
		assert_eq!(
			should_try_system_git(
				"https://tfs.corp.local/collection/_git/repo"
			),
			aghub_git::system_git::system_git_available(),
			"non-github https must still reach the system-git fallback"
		);
	}

	#[test]
	fn host_of_url_extracts_lowercased_host_without_userinfo_or_port() {
		assert_eq!(
			host_of_url("https://dev.azure.example/org/_git/repo").as_deref(),
			Some("dev.azure.example")
		);
		assert_eq!(
			host_of_url("https://User:tok@Tfs.Corp.LOCAL:8443/a/b").as_deref(),
			Some("tfs.corp.local")
		);
		assert_eq!(
			host_of_url("https://[::1]:443/a/b").as_deref(),
			Some("::1")
		);
		assert_eq!(host_of_url("not-a-url"), None);
	}

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
