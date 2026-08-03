use crate::credentials::forwarding::{ForwardedEntry, ForwardedGitTokens};
use crate::credentials::origin::{origin_of, origins_match, ResolvedOrigin};
use crate::credentials::resolve::{
	binding_keys_match_lookup, load_source_bindings, lookup_keys,
	resolve_token_for_source, SourceBindings,
};
use crate::credentials::CredentialStoreError;
use crate::error::ApiError;
use crate::routes::credentials::{load_credentials, StoredCredential};
use skill_update::{keychain_host_for_source, TokenResolution, TokenResolver};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ForwardedCredentialPolicy {
	/// Preserve legacy mutation behavior: reject only when both origins are
	/// known and different.
	Compatible,
	/// Require both origins to be known and exactly equal.
	StrictOrigin,
}

/// One in-memory view of stored credentials and their source bindings.
/// Loading is kept here so route handlers never perform keyring I/O or
/// reconstruct the source-to-credential lookup policy.
struct CredentialSnapshot {
	credentials: Vec<StoredCredential>,
	bindings: SourceBindings,
}

impl CredentialSnapshot {
	async fn load_parts() -> Result<
		(
			Result<Vec<StoredCredential>, CredentialStoreError>,
			Result<SourceBindings, CredentialStoreError>,
		),
		tokio::task::JoinError,
	> {
		// Two INDEPENDENT keyring reads. Run on separate blocking tasks rather
		// than back-to-back on one: each is a round trip to the OS credential
		// store, and when that store is slow or unreachable the cost is a
		// per-read timeout, so sequencing them doubles the stall. A real host
		// measured 5.3s here on macOS, and ~30s on a Linux box whose
		// secret-service was unreachable (two ~15s timeouts, one after the
		// other).
		let credentials = tokio::task::spawn_blocking(load_credentials);
		let bindings = tokio::task::spawn_blocking(load_source_bindings);
		Ok((credentials.await?, bindings.await?))
	}

	fn empty() -> Self {
		Self {
			credentials: Vec::new(),
			bindings: SourceBindings::default(),
		}
	}

	/// Load a snapshot for git-scan, where an indeterminate credential backend
	/// must remain a typed API error instead of degrading to an anonymous fetch.
	async fn load_required() -> Result<Self, ApiError> {
		match Self::load_parts().await {
			Ok((credentials, bindings)) => Ok(Self {
				credentials: credentials.map_err(ApiError::from)?,
				bindings: bindings.map_err(ApiError::from)?,
			}),
			Err(error) => Err(ApiError::from_join_error(
				error,
				"Credential operation failed",
				"CREDENTIAL_TASK_ERROR",
			)),
		}
	}

	fn find_credential(&self, id: &str) -> Option<&StoredCredential> {
		self.credentials
			.iter()
			.find(|credential| credential.id == id)
	}

	fn resolve(&self, source: &str) -> Option<String> {
		let host = keychain_host_for_source(source);
		resolve_token_for_source(
			source,
			host.as_deref(),
			&self.bindings,
			&self.credentials,
		)
	}

	async fn load_soft() -> (Self, bool) {
		use CredentialStoreError::Unavailable;

		match Self::load_parts().await {
			Ok((credentials, bindings)) => {
				let unavailable = matches!(credentials, Err(Unavailable(_)))
					|| matches!(bindings, Err(Unavailable(_)));
				(
					Self {
						credentials: credentials.unwrap_or_default(),
						bindings: bindings.unwrap_or_default(),
					},
					unavailable,
				)
			}
			Err(_) => (Self::empty(), true),
		}
	}
}

/// Request-scoped source authentication. Forwarded credentials always take
/// precedence; the keyring is loaded once on the blocking pool and consulted
/// only from this in-memory snapshot.
pub(crate) struct SourceAuth {
	forwarded: ForwardedGitTokens,
	snapshot: CredentialSnapshot,
	keyring_unavailable: bool,
}

impl SourceAuth {
	/// Normalize a source before scan authentication and repository access so
	/// both decisions use the same accepted Azure DevOps Server / TFS URL.
	pub(crate) fn normalize_scan_source(source: &str) -> String {
		aghub_git::normalize_tfs_clone_url(source)
	}

	/// Resolve only an origin-pinned forwarded credential for git-scan.
	/// Unknown origins are rejected on this path.
	pub(crate) fn forwarded_for_scan(
		forwarded: &ForwardedGitTokens,
		source: &str,
	) -> Option<String> {
		forwarded_token(
			forwarded,
			source,
			ForwardedCredentialPolicy::StrictOrigin,
		)
	}

	/// Resolve git-scan authentication in its complete precedence order:
	/// forwarded token, explicit credential, prior session, then the required
	/// host-scoped keyring fallback.
	///
	/// The forwarded path returns before keyring access, which is required for
	/// remote controllers whose API host has no reachable local keyring.
	pub(crate) async fn resolve_for_scan(
		forwarded: &ForwardedGitTokens,
		normalized_source: &str,
		explicit_credential_id: Option<&str>,
		prior_session: Option<(&str, Option<&str>)>,
	) -> Result<Option<String>, ApiError> {
		if let Some(token) =
			Self::forwarded_for_scan(forwarded, normalized_source)
		{
			return Ok(Some(token));
		}

		if let Some(credential_id) = explicit_credential_id {
			let snapshot = CredentialSnapshot::load_required().await?;
			let credential =
				snapshot.find_credential(credential_id).ok_or_else(|| {
					ApiError::new(
						rocket::http::Status::NotFound,
						"Credential not found",
						"CREDENTIAL_NOT_FOUND",
					)
				})?;
			require_github_credential_url(normalized_source)?;
			return Ok(Some(credential.token.clone()));
		}

		if let Some((session_source, Some(token))) = prior_session {
			if !origins_match(normalized_source, session_source) {
				return Err(ApiError::new(
					rocket::http::Status::BadRequest,
					"Session credential cannot be reused for a different host",
					"SESSION_CREDENTIAL_HOST_MISMATCH",
				));
			}
			return Ok(Some(token.to_string()));
		}

		let snapshot = CredentialSnapshot::load_required().await?;
		Ok(snapshot.resolve(normalized_source))
	}

	pub(crate) async fn load(forwarded: ForwardedGitTokens) -> Self {
		let (snapshot, keyring_unavailable) =
			CredentialSnapshot::load_soft().await;
		Self {
			forwarded,
			snapshot,
			keyring_unavailable,
		}
	}

	#[cfg(test)]
	pub(crate) fn for_test(
		forwarded: ForwardedGitTokens,
		keyring_unavailable: bool,
	) -> Self {
		Self {
			forwarded,
			snapshot: CredentialSnapshot::empty(),
			keyring_unavailable,
		}
	}

	#[cfg(test)]
	pub(crate) fn require_github_credential_url_for_test(
		source: &str,
	) -> Result<(), ApiError> {
		require_github_credential_url(source)
	}

	#[cfg(test)]
	pub(crate) fn same_origin_for_test(a: &str, b: &str) -> bool {
		origins_match(a, b)
	}
}

impl TokenResolver for SourceAuth {
	fn resolve(&self, source: &str) -> TokenResolution {
		if let Some(token) = forwarded_token(
			&self.forwarded,
			source,
			ForwardedCredentialPolicy::Compatible,
		) {
			return TokenResolution::Token(token);
		}
		if self.keyring_unavailable {
			return TokenResolution::BackendUnavailable;
		}
		match self.snapshot.resolve(source) {
			Some(token) => TokenResolution::Token(token),
			None => TokenResolution::NoToken,
		}
	}
}

fn forwarded_token(
	forwarded: &ForwardedGitTokens,
	source: &str,
	policy: ForwardedCredentialPolicy,
) -> Option<String> {
	forwarded_token_from_entries(
		forwarded.0.iter().map(|(forwarded_source, entry)| {
			(forwarded_source.as_str(), entry)
		}),
		source,
		policy,
	)
}

pub(super) fn forwarded_token_from_entries<'a>(
	mut entries: impl Iterator<Item = (&'a str, &'a ForwardedEntry)>,
	source: &str,
	policy: ForwardedCredentialPolicy,
) -> Option<String> {
	let source_keys = lookup_keys(source);
	if source_keys.is_empty() {
		return None;
	}

	match policy {
		ForwardedCredentialPolicy::Compatible => {
			let (_, entry) = entries.find(|(forwarded_source, _)| {
				binding_keys_match_lookup(forwarded_source, &source_keys)
			})?;
			if let (Some(entry_origin), Some(requested_origin)) = (
				entry.origin.clone().map(ResolvedOrigin::from),
				source_origin(source),
			) {
				if entry_origin != requested_origin {
					return None;
				}
			}
			Some(entry.token.clone())
		}
		ForwardedCredentialPolicy::StrictOrigin => {
			let requested_origin = source_origin(source)?;
			entries.find_map(|(forwarded_source, entry)| {
				if !binding_keys_match_lookup(forwarded_source, &source_keys) {
					return None;
				}
				let entry_origin = entry
					.origin
					.clone()
					.map(ResolvedOrigin::from)
					.or_else(|| source_origin(forwarded_source))?;
				(entry_origin == requested_origin).then(|| entry.token.clone())
			})
		}
	}
}

fn source_origin(source: &str) -> Option<ResolvedOrigin> {
	let resolved = aghub_git::resolve_remote_source(source).ok()?;
	origin_of(&resolved.clone_url)
}

fn require_github_credential_url(url: &str) -> Result<(), ApiError> {
	let reject = || {
		ApiError::new(
			rocket::http::Status::BadRequest,
			"GitHub credentials can only be used with github.com HTTPS URLs",
			"INVALID_GITHUB_CREDENTIAL_URL",
		)
	};

	let github_https = ResolvedOrigin {
		scheme: "https".to_string(),
		host: "github.com".to_string(),
		port: Some(443),
	};
	match origin_of(url) {
		Some(origin) if origin == github_https => Ok(()),
		_ => Err(reject()),
	}
}

#[cfg(test)]
mod tests {
	use std::collections::BTreeMap;

	use crate::credentials::forwarding::{ForwardedEntry, ForwardedGitTokens};

	use super::{forwarded_token, ForwardedCredentialPolicy, SourceAuth};

	#[test]
	fn unknown_origin_is_compatible_for_mutations_but_strict_for_scan() {
		let forwarded = ForwardedGitTokens(BTreeMap::from([(
			"./local-skill".to_string(),
			ForwardedEntry {
				token: "secret".to_string(),
				origin: None,
			},
		)]));

		assert_eq!(
			forwarded_token(
				&forwarded,
				"./local-skill",
				ForwardedCredentialPolicy::Compatible,
			),
			Some("secret".to_string()),
		);
		assert_eq!(
			SourceAuth::forwarded_for_scan(&forwarded, "./local-skill"),
			None,
		);
	}

	#[test]
	fn scan_source_normalization_is_owned_by_source_auth() {
		assert_eq!(
			SourceAuth::normalize_scan_source(
				"https://tfs.example/Collection/_git/repo.git"
			),
			"https://tfs.example/Collection/_git/repo",
		);
	}
}
