//! Public, controller-side credential resolution for callers outside this
//! crate (the desktop `src-tauri` layer drives these for remote git-credential
//! forwarding).
//!
//! These are thin wrappers over the crate-private [`resolve`] helpers: the
//! keyring/credential-store internals stay private; only the two functions and
//! the [`ResolvedToken`]/[`ResolvedOrigin`] types are exported from `lib.rs`.
//!
//! Security: a token is only ever returned to the caller in-memory. It is never
//! logged here and never written to disk or any lock file.

use crate::credentials::origin::{origin_of, ResolvedOrigin};
use crate::credentials::resolve::{
	load_source_bindings, resolve_token_for_source, SourceBindings,
};
use crate::routes::credentials::{load_credentials, StoredCredential};

/// A resolved token plus the origin it is pinned to. The origin lets a remote
/// caller verify a forwarded token is only ever sent back to the exact
/// `(scheme, host, port)` it was scoped to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedToken {
	/// The resolved credential token. Never logged or persisted.
	pub token: String,
	/// The origin the source resolves to, when derivable. `None` for sources
	/// that do not resolve to a host-bearing URL (e.g. local paths).
	pub origin: Option<ResolvedOrigin>,
}

/// Resolve a git token for a skill `source`, returning the token and the origin
/// it is pinned to.
///
/// Loads the local credential bindings + stored credentials from the keyring,
/// then delegates the pure resolution + origin derivation to the testable inner
/// [`resolve_token_with`]. Returns `None` when the keyring cannot be read or no
/// credential matches the source.
///
/// This reads the OS keyring, so it is not unit-tested directly; the pure inner
/// fn is.
pub fn resolve_git_token_for_source(source: &str) -> Option<ResolvedToken> {
	let bindings = load_source_bindings().ok()?;
	let creds = load_credentials().ok()?;
	resolve_token_with(source, &bindings, &creds)
}

/// Testable core of [`resolve_git_token_for_source`]: given already-loaded
/// bindings + credentials, resolve a token and derive the source origin.
///
/// The host passed to the resolver is taken from the derived origin so the
/// host-fallback resolution path and the returned `origin` agree on a single
/// normalized host. No keyring or network access.
pub(crate) fn resolve_token_with(
	source: &str,
	bindings: &SourceBindings,
	creds: &[StoredCredential],
) -> Option<ResolvedToken> {
	let origin = origin_of_source(source);
	let host = origin.as_ref().map(|o| o.host.clone());
	let token =
		resolve_token_for_source(source, host.as_deref(), bindings, creds)?;
	Some(ResolvedToken { token, origin })
}

/// Derive the normalized origin for a source by resolving it through
/// `aghub_git` (which understands shorthands like `owner/repo`) and then
/// normalizing its `clone_url`. `None` for sources that do not resolve to a
/// host-bearing URL.
fn origin_of_source(source: &str) -> Option<ResolvedOrigin> {
	let resolved = aghub_git::resolve_remote_source(source).ok()?;
	origin_of(&resolved.clone_url)
}

/// List the source keys that have an explicit credential binding.
///
/// Reads the binding store from the keyring; returns an empty list when the
/// store cannot be read (degrade rather than error). Used by the desktop to
/// enumerate sources for remote `check-updates`.
pub fn list_bound_sources() -> Vec<String> {
	load_source_bindings()
		.map(bound_source_keys)
		.unwrap_or_default()
}

/// Pure projection of a [`SourceBindings`] to its source keys. Separated out so
/// it can be unit-tested without touching the keyring.
pub(crate) fn bound_source_keys(bindings: SourceBindings) -> Vec<String> {
	bindings.0.into_keys().collect()
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::credentials::resolve::SourceBindings;

	fn cred(id: &str, name: &str, token: &str) -> StoredCredential {
		StoredCredential {
			id: id.into(),
			name: name.into(),
			token: token.into(),
		}
	}

	#[test]
	fn resolve_returns_token_and_origin_when_bound() {
		let mut b = SourceBindings::default();
		b.0.insert("owner/repo".into(), "c1".into());
		let creds = vec![cred("c1", "github.com", "TOK1")];

		let resolved =
			resolve_token_with("owner/repo", &b, &creds).expect("token");
		assert_eq!(resolved.token, "TOK1");
		let origin = resolved.origin.expect("origin");
		assert_eq!(origin.scheme, "https");
		assert_eq!(origin.host, "github.com");
		assert_eq!(origin.port, Some(443));
	}

	#[test]
	fn resolve_returns_token_via_host_fallback() {
		// No binding, but a credential named after the host resolves.
		let b = SourceBindings::default();
		let creds = vec![cred("c1", "github.com", "TOK2")];

		let resolved =
			resolve_token_with("https://github.com/owner/repo.git", &b, &creds)
				.expect("token");
		assert_eq!(resolved.token, "TOK2");
		assert_eq!(resolved.origin.unwrap().host, "github.com");
	}

	#[test]
	fn resolve_returns_none_when_unbound() {
		let b = SourceBindings::default();
		let creds = vec![cred("c1", "gitlab.com", "X")];

		assert!(resolve_token_with("owner/repo", &b, &creds).is_none());
	}

	#[test]
	fn resolve_does_not_leak_cross_host_token() {
		// A github.com binding must not satisfy a gitlab.com lookup, and with
		// no gitlab.com host credential the host fallback cannot rescue it.
		let mut b = SourceBindings::default();
		b.0.insert("owner/repo".into(), "c_github".into());
		let creds = vec![cred("c_github", "github.com", "GHTOK")];

		assert!(resolve_token_with(
			"https://gitlab.com/owner/repo.git",
			&b,
			&creds,
		)
		.is_none());
	}

	#[test]
	fn resolve_origin_pins_self_hosted_port() {
		// A self-hosted forge on a non-default port keeps its port in the
		// pinned origin.
		let b = SourceBindings::default();
		let creds = vec![cred("c1", "git.internal", "TOK")];

		let resolved = resolve_token_with(
			"https://git.internal:8443/owner/repo.git",
			&b,
			&creds,
		)
		.expect("token");
		let origin = resolved.origin.expect("origin");
		assert_eq!(origin.host, "git.internal");
		assert_eq!(origin.port, Some(8443));
	}

	#[test]
	fn bound_source_keys_returns_bound_keys() {
		let mut b = SourceBindings::default();
		b.0.insert("b/source".into(), "c2".into());
		b.0.insert("a/source".into(), "c1".into());

		let keys = bound_source_keys(b);
		// BTreeMap iteration is sorted, so the order is deterministic.
		assert_eq!(keys, vec!["a/source".to_string(), "b/source".to_string()]);
	}

	#[test]
	fn bound_source_keys_empty_when_no_bindings() {
		assert!(bound_source_keys(SourceBindings::default()).is_empty());
	}
}
