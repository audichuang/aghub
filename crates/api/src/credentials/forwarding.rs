//! Forwarded git-credential primitives.
//!
//! When a desktop client drives a *remote* aghub-api (over an SSH tunnel), the
//! remote process has no access to the user's local keyring. The client may
//! instead forward a per-request set of `source → { token, origin }` entries in
//! the `X-Aghub-Git-Tokens` header (base64-encoded JSON). This module parses
//! the transport shape; `credentials::source_auth` owns matching, origin
//! pinning, precedence, and keyring fallback.
//!
//! Wire contract (must match the TS encoder in
//! `crates/desktop/src/lib/git-token-forwarding.ts`):
//!
//! ```json
//! { "<sourceKey>": { "token": "<tok>",
//!   "origin": { "scheme": "...", "host": "...", "port": <number|null> } | null } }
//! ```
//!
//! The `origin` is the controller-resolved `(scheme, host, port)` the token is
//! pinned to; it lets the resolver reject handing a token to a same-host but
//! different-`(scheme,port)` request (origin pinning, D8).
//!
//! Security invariants:
//! - A token is never logged. The only `warn!` here (malformed header) must
//!   not include the header value.
//! - Absent / malformed header degrades to an empty map; Source-auth then uses
//!   the keyring path exactly as it does for requests without this header.
//! - The `origin` field is non-sensitive metadata; only the `token` is secret.

use std::collections::BTreeMap;

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use rocket::request::{self, FromRequest, Request};
use serde::Deserialize;
#[cfg(test)]
use skill_update::{TokenResolution, TokenResolver};

use crate::credentials::origin::ResolvedOrigin;
#[cfg(test)]
use crate::credentials::source_auth::{
	forwarded_token_from_entries, ForwardedCredentialPolicy,
};

/// Header carrying the base64-encoded JSON `source → { token, origin }` map.
const FORWARDED_TOKENS_HEADER: &str = "X-Aghub-Git-Tokens";

/// A single forwarded entry: the resolved token plus the origin it is pinned to.
///
/// `origin` mirrors [`ResolvedOrigin`] and is the controller-resolved
/// `(scheme, host, port)` the token was scoped to. It is `None` for sources the
/// controller could not resolve to a host-bearing URL (e.g. local paths), in
/// which case the resolver falls back to the permissive host-scoped behaviour.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ForwardedEntry {
	/// The forwarded credential token. Never logged or persisted.
	pub(crate) token: String,
	/// The origin the token is pinned to, when the controller could derive one.
	#[serde(default)]
	pub(crate) origin: Option<ForwardedOrigin>,
}

/// The wire shape of a forwarded entry's origin. Deserialized from the header
/// and converted to the crate-internal [`ResolvedOrigin`] for comparison.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ForwardedOrigin {
	pub(crate) scheme: String,
	pub(crate) host: String,
	#[serde(default)]
	pub(crate) port: Option<u16>,
}

impl From<ForwardedOrigin> for ResolvedOrigin {
	fn from(o: ForwardedOrigin) -> Self {
		ResolvedOrigin {
			// The controller already lowercases scheme/host, but normalize again
			// defensively so comparison against `origin_of` (which lowercases)
			// never fails on case alone.
			scheme: o.scheme.to_ascii_lowercase(),
			host: o.host.to_ascii_lowercase(),
			port: o.port,
		}
	}
}

/// Request guard: the forwarded `source → { token, origin }` map for this
/// request.
///
/// The `FromRequest` outcome is **always** [`request::Outcome::Success`]; an
/// absent or malformed header degrades to an empty map (never a 400). This
/// keeps the API backward compatible: clients that do not forward tokens are
/// indistinguishable from today's behavior.
#[derive(Debug, Default, Clone)]
pub struct ForwardedGitTokens(pub(crate) BTreeMap<String, ForwardedEntry>);

impl ForwardedGitTokens {
	/// Parse the raw header value into a `source → { token, origin }` map.
	///
	/// Any failure (base64 or JSON) degrades to an empty map and logs a
	/// `warn!` that never includes the header value (tokens must not leak).
	fn parse_header(raw: &str) -> BTreeMap<String, ForwardedEntry> {
		let decoded = match BASE64.decode(raw.trim()) {
			Ok(bytes) => bytes,
			Err(_) => {
				log::warn!(
					"{FORWARDED_TOKENS_HEADER}: base64 decode failed; \
					 ignoring forwarded tokens"
				);
				return BTreeMap::new();
			}
		};
		match serde_json::from_slice::<BTreeMap<String, ForwardedEntry>>(
			&decoded,
		) {
			Ok(map) => map,
			Err(_) => {
				log::warn!(
					"{FORWARDED_TOKENS_HEADER}: JSON parse failed; \
					 ignoring forwarded tokens"
				);
				BTreeMap::new()
			}
		}
	}
}

#[rocket::async_trait]
impl<'r> FromRequest<'r> for ForwardedGitTokens {
	type Error = std::convert::Infallible;

	async fn from_request(
		req: &'r Request<'_>,
	) -> request::Outcome<Self, Self::Error> {
		let map = req
			.headers()
			.get_one(FORWARDED_TOKENS_HEADER)
			.map(Self::parse_header)
			.unwrap_or_default();
		request::Outcome::Success(ForwardedGitTokens(map))
	}
}

/// [`TokenResolver`] backed by a forwarded `source → { token, origin }` map.
///
/// Matching is by `source`, using the SAME cross-URL-form matching the keyring
/// bindings use (`owner/repo` ⇔ `https://github.com/owner/repo.git`
/// ⇔ `git@github.com:owner/repo.git`, host-scoped) — then a `(scheme,host,port)`
/// origin pin on top. The forwarded map is keyed by source and the cross-form
/// key set already encodes the host.
#[derive(Debug, Default, Clone)]
#[cfg(test)]
pub(crate) struct ForwardedTokenResolver(BTreeMap<String, ForwardedEntry>);

#[cfg(test)]
impl TokenResolver for ForwardedTokenResolver {
	/// Resolve a forwarded token for `source`, origin-pinned to the requested
	/// source's `(scheme, host, port)`.
	///
	/// 1. Find the entry whose source matches `source` under the legacy
	///    host-scoped key match (cross-URL form, unchanged — `lookup_keys` /
	///    `binding_keys_match_lookup`).
	/// 2. Derive the requested source's origin through the shared Source-auth
	///    policy.
	/// 3. If the matched entry's `origin` is `Some` AND the requested origin is
	///    `Some` AND they do NOT match → return `None` (the [`ChainResolver`]
	///    then falls back to the keyring). Otherwise return the token.
	///    When either origin is unknown the pin is permissive — it falls back to
	///    the legacy host-scoped behaviour so a previously-working case (e.g. a
	///    plain github.com source) is never newly broken.
	fn resolve(&self, source: &str) -> TokenResolution {
		match forwarded_token_from_entries(
			self.0.iter().map(|(forwarded_source, entry)| {
				(forwarded_source.as_str(), entry)
			}),
			source,
			ForwardedCredentialPolicy::Compatible,
		) {
			Some(token) => TokenResolution::Token(token),
			None => TokenResolution::NoToken,
		}
	}
}

/// A two-stage [`TokenResolver`]: try `primary`, then fall back to `fallback`.
///
/// Borrow-based so callers can compose any pair of resolvers without taking
/// ownership; Task 3 wraps a [`ForwardedTokenResolver`] over the existing
/// keyring resolver.
#[cfg(test)]
pub(crate) struct ChainResolver<'a, P: TokenResolver> {
	primary: P,
	fallback: &'a dyn TokenResolver,
}

#[cfg(test)]
impl<'a, P: TokenResolver> ChainResolver<'a, P> {
	pub(crate) fn new(primary: P, fallback: &'a dyn TokenResolver) -> Self {
		Self { primary, fallback }
	}
}

#[cfg(test)]
impl<P: TokenResolver> TokenResolver for ChainResolver<'_, P> {
	fn resolve(&self, source: &str) -> TokenResolution {
		match self.primary.resolve(source) {
			TokenResolution::Token(token) => TokenResolution::Token(token),
			TokenResolution::NoToken | TokenResolution::BackendUnavailable => {
				match self.fallback.resolve(source) {
					TokenResolution::Token(token) => {
						TokenResolution::Token(token)
					}
					TokenResolution::NoToken
					| TokenResolution::BackendUnavailable => TokenResolution::NoToken,
				}
			}
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	/// Build an entry map with NO origin (permissive — legacy host-scoped pin).
	fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, ForwardedEntry> {
		pairs
			.iter()
			.map(|(k, v)| {
				(
					(*k).to_string(),
					ForwardedEntry {
						token: (*v).to_string(),
						origin: None,
					},
				)
			})
			.collect()
	}

	/// Build a single-entry map carrying an explicit origin for the source.
	fn map_with_origin(
		source: &str,
		token: &str,
		origin: ForwardedOrigin,
	) -> BTreeMap<String, ForwardedEntry> {
		let mut m = BTreeMap::new();
		m.insert(
			source.to_string(),
			ForwardedEntry {
				token: token.to_string(),
				origin: Some(origin),
			},
		);
		m
	}

	fn origin(scheme: &str, host: &str, port: Option<u16>) -> ForwardedOrigin {
		ForwardedOrigin {
			scheme: scheme.to_string(),
			host: host.to_string(),
			port,
		}
	}

	// --- header parsing ----------------------------------------------------

	#[test]
	fn empty_string_header_yields_empty_map() {
		assert!(ForwardedGitTokens::parse_header("").is_empty());
	}

	#[test]
	fn valid_base64_json_yields_map() {
		// The new `{ token, origin }` shape (origin present).
		let json = r#"{"owner/repo":{"token":"TOK1","origin":{"scheme":"https","host":"github.com","port":443}}}"#;
		let encoded = BASE64.encode(json);
		let parsed = ForwardedGitTokens::parse_header(&encoded);
		let entry = parsed.get("owner/repo").expect("entry");
		assert_eq!(entry.token, "TOK1");
		let origin = entry.origin.as_ref().expect("origin");
		assert_eq!(origin.scheme, "https");
		assert_eq!(origin.host, "github.com");
		assert_eq!(origin.port, Some(443));
	}

	#[test]
	fn valid_base64_json_with_null_origin_yields_entry() {
		// `origin: null` is the local-path / unresolvable case.
		let json = r#"{"local/skill":{"token":"TOK2","origin":null}}"#;
		let encoded = BASE64.encode(json);
		let parsed = ForwardedGitTokens::parse_header(&encoded);
		let entry = parsed.get("local/skill").expect("entry");
		assert_eq!(entry.token, "TOK2");
		assert!(entry.origin.is_none());
	}

	#[test]
	fn bad_base64_yields_empty_map() {
		// `!` is outside the standard base64 alphabet.
		assert!(ForwardedGitTokens::parse_header("not!base64!").is_empty());
	}

	#[test]
	fn bad_json_yields_empty_map() {
		// Valid base64, but the decoded bytes are not the entry-map shape.
		let encoded = BASE64.encode("[1, 2, 3]");
		assert!(ForwardedGitTokens::parse_header(&encoded).is_empty());
	}

	#[test]
	fn legacy_bare_string_shape_yields_empty_map() {
		// The OLD wire shape (`source → bare token string`) is no longer a
		// valid entry map, so it degrades to empty (never a 400). The TS encoder
		// is updated in lockstep, so this only matters for stale clients.
		let encoded = BASE64.encode(r#"{"owner/repo":"TOK1"}"#);
		assert!(ForwardedGitTokens::parse_header(&encoded).is_empty());
	}

	// --- ForwardedTokenResolver matching -----------------------------------

	#[test]
	fn resolver_returns_none_on_empty_map() {
		let r = ForwardedTokenResolver::default();
		assert_eq!(r.resolve("owner/repo"), TokenResolution::NoToken);
	}

	#[test]
	fn resolver_matches_bare_source_against_url_key() {
		// Forwarded as a full URL; looked up by the bare `owner/repo` shape.
		let r = ForwardedTokenResolver(map(&[(
			"https://github.com/owner/repo.git",
			"TOK1",
		)]));
		assert_eq!(
			r.resolve("owner/repo"),
			TokenResolution::Token("TOK1".to_string())
		);
	}

	#[test]
	fn resolver_matches_url_source_against_bare_key() {
		// Forwarded bare; looked up by an equivalent full URL.
		let r = ForwardedTokenResolver(map(&[("owner/repo", "TOK1")]));
		assert_eq!(
			r.resolve("https://github.com/owner/repo.git"),
			TokenResolution::Token("TOK1".to_string())
		);
	}

	#[test]
	fn resolver_does_not_cross_hosts() {
		// A GitHub-forwarded token must not match a GitLab lookup of the same
		// `owner/repo` shape — host is encoded in the key set.
		let r = ForwardedTokenResolver(map(&[("owner/repo", "GHTOK")]));
		// `owner/repo` resolves to github.com; gitlab URL must miss.
		assert_eq!(
			r.resolve("https://gitlab.com/owner/repo.git"),
			TokenResolution::NoToken
		);
	}

	// --- origin pinning (D8) -----------------------------------------------

	#[test]
	fn resolver_rejects_same_host_different_port() {
		// Entry pinned to a self-hosted forge on port 8443; the request is for
		// the SAME host on a DIFFERENT port. Host-scoped matching would match,
		// but the origin pin must reject (→ None → keyring fallback).
		let r = ForwardedTokenResolver(map_with_origin(
			"https://git.internal:8443/owner/repo.git",
			"TOK",
			origin("https", "git.internal", Some(8443)),
		));
		assert_eq!(
			r.resolve("https://git.internal:9090/owner/repo.git"),
			TokenResolution::NoToken,
			"a token pinned to one port must not be reused on another"
		);
	}

	#[test]
	fn resolver_accepts_matching_origin() {
		// The positive counterpart: the SAME `(scheme, host, port)` matches.
		let r = ForwardedTokenResolver(map_with_origin(
			"https://git.internal:8443/owner/repo.git",
			"TOK",
			origin("https", "git.internal", Some(8443)),
		));
		assert_eq!(
			r.resolve("https://git.internal:8443/owner/repo.git"),
			TokenResolution::Token("TOK".to_string())
		);
	}

	#[test]
	fn resolver_accepts_matching_origin_default_port_folded() {
		// Entry carries the explicit default https port; the request omits it.
		// `origin_of` folds 443 in, so both normalize to the same origin.
		let r = ForwardedTokenResolver(map_with_origin(
			"owner/repo",
			"TOK",
			origin("https", "github.com", Some(443)),
		));
		assert_eq!(
			r.resolve("https://github.com/owner/repo.git"),
			TokenResolution::Token("TOK".to_string())
		);
	}

	#[test]
	fn resolver_unknown_entry_origin_is_permissive() {
		// Entry has NO origin (local/unresolvable on the controller). The pin is
		// permissive: the host-scoped match alone returns the token (legacy).
		let r = ForwardedTokenResolver(map(&[("owner/repo", "TOK")]));
		assert_eq!(
			r.resolve("https://github.com/owner/repo.git"),
			TokenResolution::Token("TOK".to_string())
		);
	}

	// --- ChainResolver precedence ------------------------------------------

	struct StubResolver(Option<String>);
	impl TokenResolver for StubResolver {
		fn resolve(&self, _source: &str) -> TokenResolution {
			match &self.0 {
				Some(token) => TokenResolution::Token(token.clone()),
				None => TokenResolution::NoToken,
			}
		}
	}

	#[test]
	fn chain_falls_back_to_keyring_on_origin_mismatch() {
		// The origin-mismatch path must hand off to the keyring fallback rather
		// than leaking the wrong-origin forwarded token.
		let primary = ForwardedTokenResolver(map_with_origin(
			"https://git.internal:8443/owner/repo.git",
			"FWD",
			origin("https", "git.internal", Some(8443)),
		));
		let fallback = StubResolver(Some("KEYRING".to_string()));
		let chain = ChainResolver::new(primary, &fallback);
		assert_eq!(
			chain.resolve("https://git.internal:9090/owner/repo.git"),
			TokenResolution::Token("KEYRING".to_string())
		);
	}

	#[test]
	fn chain_prefers_primary_hit() {
		let primary = ForwardedTokenResolver(map(&[("owner/repo", "FWD")]));
		let fallback = StubResolver(Some("KEYRING".to_string()));
		let chain = ChainResolver::new(primary, &fallback);
		assert_eq!(
			chain.resolve("owner/repo"),
			TokenResolution::Token("FWD".to_string())
		);
	}

	#[test]
	fn chain_falls_back_on_primary_miss() {
		let primary = ForwardedTokenResolver::default();
		let fallback = StubResolver(Some("KEYRING".to_string()));
		let chain = ChainResolver::new(primary, &fallback);
		assert_eq!(
			chain.resolve("owner/repo"),
			TokenResolution::Token("KEYRING".to_string())
		);
	}

	#[test]
	fn chain_returns_none_when_both_miss() {
		let primary = ForwardedTokenResolver::default();
		let fallback = StubResolver(None);
		let chain = ChainResolver::new(primary, &fallback);
		assert_eq!(chain.resolve("owner/repo"), TokenResolution::NoToken);
	}
}
