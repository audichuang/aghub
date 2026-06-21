//! Forwarded git-credential primitives.
//!
//! When a desktop client drives a *remote* aghub-api (over an SSH tunnel), the
//! remote process has no access to the user's local keyring. The client may
//! instead forward a per-request set of `source → token` pairs in the
//! `X-Aghub-Git-Tokens` header (base64-encoded JSON). These primitives parse
//! that header into a [`TokenResolver`] that the update/sources orchestration
//! can consult *before* the keyring-backed resolver.
//!
//! Security invariants:
//! - A token is never logged. The only `warn!` here (malformed header) must
//!   not include the header value.
//! - Absent / malformed header degrades to an empty map → the resolver returns
//!   `None` → callers behave exactly as they do today (backward compatible).

use std::collections::BTreeMap;

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use rocket::request::{self, FromRequest, Request};
use skill_update::TokenResolver;

use crate::credentials::resolve::{binding_keys_match_lookup, lookup_keys};

/// Header carrying the base64-encoded JSON `source → token` map.
const FORWARDED_TOKENS_HEADER: &str = "X-Aghub-Git-Tokens";

/// Request guard: the forwarded `source → token` map for this request.
///
/// The `FromRequest` outcome is **always** [`request::Outcome::Success`]; an
/// absent or malformed header degrades to an empty map (never a 400). This
/// keeps the API backward compatible: clients that do not forward tokens are
/// indistinguishable from today's behavior.
#[derive(Debug, Default, Clone)]
pub struct ForwardedGitTokens(pub(crate) BTreeMap<String, String>);

impl ForwardedGitTokens {
	/// Parse the raw header value into a `source → token` map.
	///
	/// Any failure (base64 or JSON) degrades to an empty map and logs a
	/// `warn!` that never includes the header value (tokens must not leak).
	fn parse_header(raw: &str) -> BTreeMap<String, String> {
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
		match serde_json::from_slice::<BTreeMap<String, String>>(&decoded) {
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

	/// Build a [`ForwardedTokenResolver`] over this map.
	pub(crate) fn into_resolver(self) -> ForwardedTokenResolver {
		ForwardedTokenResolver(self.0)
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

/// [`TokenResolver`] backed by a forwarded `source → token` map.
///
/// Matching is by `source` only, using the SAME cross-URL-form matching the
/// keyring bindings use (`owner/repo` ⇔ `https://github.com/owner/repo.git`
/// ⇔ `git@github.com:owner/repo.git`, host-scoped). The `host` argument is
/// intentionally unused — the forwarded map is keyed by source, and the
/// cross-form key set already encodes the host.
#[derive(Debug, Default, Clone)]
pub(crate) struct ForwardedTokenResolver(BTreeMap<String, String>);

impl TokenResolver for ForwardedTokenResolver {
	/// Matches with the legacy host-scoped keyring strictness on purpose:
	/// `lookup_keys` / `binding_keys_match_lookup` are host-scoped, not
	/// port/scheme-specific. The stricter `(scheme, host, port)` origin pin
	/// is applied additionally by `git_scan_skills` (`forwarded_token_for_url`
	/// in `routes/skills.rs`), so the asymmetry here is deliberate.
	fn resolve(&self, source: &str, _host: Option<&str>) -> Option<String> {
		let source_keys = lookup_keys(source);
		self.0
			.iter()
			.find(|(forwarded_source, _)| {
				binding_keys_match_lookup(forwarded_source, &source_keys)
			})
			.map(|(_, token)| token.clone())
	}
}

/// A two-stage [`TokenResolver`]: try `primary`, then fall back to `fallback`.
///
/// Borrow-based so callers can compose any pair of resolvers without taking
/// ownership; Task 3 wraps a [`ForwardedTokenResolver`] over the existing
/// keyring resolver.
pub(crate) struct ChainResolver<'a, P: TokenResolver> {
	primary: P,
	fallback: &'a dyn TokenResolver,
}

impl<'a, P: TokenResolver> ChainResolver<'a, P> {
	pub(crate) fn new(primary: P, fallback: &'a dyn TokenResolver) -> Self {
		Self { primary, fallback }
	}
}

impl<P: TokenResolver> TokenResolver for ChainResolver<'_, P> {
	fn resolve(&self, source: &str, host: Option<&str>) -> Option<String> {
		self.primary
			.resolve(source, host)
			.or_else(|| self.fallback.resolve(source, host))
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
		pairs
			.iter()
			.map(|(k, v)| ((*k).to_string(), (*v).to_string()))
			.collect()
	}

	// --- header parsing ----------------------------------------------------

	#[test]
	fn empty_string_header_yields_empty_map() {
		assert!(ForwardedGitTokens::parse_header("").is_empty());
	}

	#[test]
	fn valid_base64_json_yields_map() {
		let json = r#"{"owner/repo":"TOK1"}"#;
		let encoded = BASE64.encode(json);
		let parsed = ForwardedGitTokens::parse_header(&encoded);
		assert_eq!(parsed.get("owner/repo").map(String::as_str), Some("TOK1"));
	}

	#[test]
	fn bad_base64_yields_empty_map() {
		// `!` is outside the standard base64 alphabet.
		assert!(ForwardedGitTokens::parse_header("not!base64!").is_empty());
	}

	#[test]
	fn bad_json_yields_empty_map() {
		// Valid base64, but the decoded bytes are not a JSON string→string map.
		let encoded = BASE64.encode("[1, 2, 3]");
		assert!(ForwardedGitTokens::parse_header(&encoded).is_empty());
	}

	// --- ForwardedTokenResolver matching -----------------------------------

	#[test]
	fn resolver_returns_none_on_empty_map() {
		let r = ForwardedTokenResolver::default();
		assert_eq!(r.resolve("owner/repo", Some("github.com")), None);
	}

	#[test]
	fn resolver_matches_bare_source_against_url_key() {
		// Forwarded as a full URL; looked up by the bare `owner/repo` shape.
		let r = ForwardedTokenResolver(map(&[(
			"https://github.com/owner/repo.git",
			"TOK1",
		)]));
		assert_eq!(r.resolve("owner/repo", None), Some("TOK1".to_string()));
	}

	#[test]
	fn resolver_matches_url_source_against_bare_key() {
		// Forwarded bare; looked up by an equivalent full URL.
		let r = ForwardedTokenResolver(map(&[("owner/repo", "TOK1")]));
		assert_eq!(
			r.resolve("https://github.com/owner/repo.git", Some("github.com")),
			Some("TOK1".to_string())
		);
	}

	#[test]
	fn resolver_does_not_cross_hosts() {
		// A GitHub-forwarded token must not match a GitLab lookup of the same
		// `owner/repo` shape — host is encoded in the key set.
		let r = ForwardedTokenResolver(map(&[("owner/repo", "GHTOK")]));
		// `owner/repo` resolves to github.com; gitlab URL must miss.
		assert_eq!(
			r.resolve("https://gitlab.com/owner/repo.git", Some("gitlab.com")),
			None
		);
	}

	// --- ChainResolver precedence ------------------------------------------

	struct StubResolver(Option<String>);
	impl TokenResolver for StubResolver {
		fn resolve(&self, _s: &str, _h: Option<&str>) -> Option<String> {
			self.0.clone()
		}
	}

	#[test]
	fn chain_prefers_primary_hit() {
		let primary = ForwardedTokenResolver(map(&[("owner/repo", "FWD")]));
		let fallback = StubResolver(Some("KEYRING".to_string()));
		let chain = ChainResolver::new(primary, &fallback);
		assert_eq!(
			chain.resolve("owner/repo", Some("github.com")),
			Some("FWD".to_string())
		);
	}

	#[test]
	fn chain_falls_back_on_primary_miss() {
		let primary = ForwardedTokenResolver::default();
		let fallback = StubResolver(Some("KEYRING".to_string()));
		let chain = ChainResolver::new(primary, &fallback);
		assert_eq!(
			chain.resolve("owner/repo", Some("github.com")),
			Some("KEYRING".to_string())
		);
	}

	#[test]
	fn chain_returns_none_when_both_miss() {
		let primary = ForwardedTokenResolver::default();
		let fallback = StubResolver(None);
		let chain = ChainResolver::new(primary, &fallback);
		assert_eq!(chain.resolve("owner/repo", Some("github.com")), None);
	}
}
