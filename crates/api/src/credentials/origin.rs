//! Normalized request origin `(scheme, host, port)` for credential pinning.
//!
//! A credential (or a session token) must only ever be sent back to the exact
//! origin it was scoped to. Pinning on host alone is too loose once we support
//! self-hosted forges on non-default ports (a token for
//! `https://git.internal:8443` must not leak to `https://git.internal:9090`),
//! so we compare a normalized `(scheme, host, port)` triple instead.
//!
//! Normalization rules:
//! - `scheme` and `host` are lowercased (URL spec: both are case-insensitive).
//! - `port` is resolved to the scheme's known default when omitted, so
//!   `https://github.com` and `https://github.com:443` compare equal.

/// A normalized origin used to pin a token to the place it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedOrigin {
	/// Lowercased URL scheme (e.g. `https`).
	pub scheme: String,
	/// Lowercased host (e.g. `github.com`).
	pub host: String,
	/// Effective port: the explicit port, or the scheme's known default
	/// (`Some(443)` for `https`). `None` only when neither is available.
	pub port: Option<u16>,
}

/// Parse a URL into its normalized `(scheme, host, port)` origin.
///
/// Returns `None` when the input does not parse as a URL or has no host (so a
/// bare path or an opaque scheme like `mailto:` yields `None` rather than a
/// half-populated origin).
pub fn origin_of(url: &str) -> Option<ResolvedOrigin> {
	let parsed = url::Url::parse(url).ok()?;
	let host = parsed.host_str()?;
	Some(ResolvedOrigin {
		scheme: parsed.scheme().to_ascii_lowercase(),
		host: host.to_ascii_lowercase(),
		// `port_or_known_default` folds the scheme's default port in, so
		// `https://h` and `https://h:443` resolve to the same origin.
		port: parsed.port_or_known_default(),
	})
}

/// `true` iff both URLs parse to a host-bearing origin and their normalized
/// `(scheme, host, port)` triples are equal.
///
/// A parse failure or a missing host on either side yields `false`. The same
/// host on a different port does NOT match (that is the whole point of pinning
/// on the full origin rather than just the host).
pub fn origins_match(a: &str, b: &str) -> bool {
	match (origin_of(a), origin_of(b)) {
		(Some(a), Some(b)) => a == b,
		_ => false,
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn origin_of_normalizes_scheme_and_host_case() {
		let o = origin_of("HTTPS://GitHub.com/owner/repo.git").unwrap();
		assert_eq!(o.scheme, "https");
		assert_eq!(o.host, "github.com");
		assert_eq!(o.port, Some(443));
	}

	#[test]
	fn origin_of_folds_default_port() {
		// `https://h` and `https://h:443` must produce the same origin.
		assert_eq!(
			origin_of("https://github.com/x"),
			origin_of("https://github.com:443/y"),
		);
	}

	#[test]
	fn origin_of_returns_none_for_non_url() {
		assert!(origin_of("not a url").is_none());
	}

	#[test]
	fn origin_of_returns_none_without_host() {
		// `mailto:` has no host component.
		assert!(origin_of("mailto:user@example.com").is_none());
	}

	#[test]
	fn origins_match_github_https_forms() {
		// github.com over HTTPS in equivalent forms must match.
		assert!(origins_match(
			"https://github.com/owner/repo.git",
			"https://github.com:443/owner/other.git",
		));
	}

	#[test]
	fn origins_match_false_for_different_host() {
		assert!(!origins_match(
			"https://github.com/a",
			"https://evil.example/a",
		));
	}

	#[test]
	fn origins_match_false_for_same_host_different_port() {
		// The negative the whole generalization exists for: same host, same
		// scheme, different explicit port must NOT match.
		assert!(!origins_match(
			"https://git.internal:8443/a.git",
			"https://git.internal:9090/b.git",
		));
	}

	#[test]
	fn origins_match_false_for_different_scheme() {
		// http vs https on the same host is a different origin.
		assert!(!origins_match(
			"http://github.com/a",
			"https://github.com/a",
		));
	}

	#[test]
	fn origins_match_false_on_parse_failure() {
		assert!(!origins_match("not a url", "https://github.com/a"));
	}
}
