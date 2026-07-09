//! Tauri command layer for controller-side git-credential resolution.
//!
//! The desktop frontend drives these to resolve a git token on the Mac
//! controller **in-process** (no HTTP, no `localBaseUrl`) and then forward it
//! to a remote VM for git-credential forwarding. The keyring/credential-store
//! internals live in `aghub-api`; this module is the thin glue that maps the
//! crate's [`aghub_api::ResolvedToken`] to a serializable DTO for the FE.
//!
//! Security: the resolved token is sensitive. It is NEVER logged here and only
//! ever crosses the IPC boundary back to the frontend that asked for it.

use aghub_api::{ResolvedOrigin, ResolvedToken};
use serde::Serialize;

/// The normalized origin a resolved token is pinned to, exposed to the FE.
///
/// Mirrors [`aghub_api::ResolvedOrigin`] but is a desktop-owned DTO so the IPC
/// shape (camelCase) is decoupled from the API crate's internal type.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedOriginDto {
	/// Lowercased URL scheme (e.g. `https`).
	pub scheme: String,
	/// Lowercased host (e.g. `github.com`).
	pub host: String,
	/// Effective port: the explicit port or the scheme's known default.
	pub port: Option<u16>,
}

impl From<ResolvedOrigin> for ResolvedOriginDto {
	fn from(o: ResolvedOrigin) -> Self {
		Self {
			scheme: o.scheme,
			host: o.host,
			port: o.port,
		}
	}
}

/// A resolved git token plus the origin it is pinned to, returned to the FE.
///
/// The `token` is sensitive: it is never logged and only sent back over IPC to
/// the caller that requested it.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedTokenDto {
	/// The resolved credential token. Never logged or persisted.
	pub token: String,
	/// The origin the source resolves to, when derivable. `None` for sources
	/// that do not resolve to a host-bearing URL (e.g. local paths).
	pub origin: Option<ResolvedOriginDto>,
}

impl From<ResolvedToken> for ResolvedTokenDto {
	fn from(t: ResolvedToken) -> Self {
		Self {
			token: t.token,
			origin: t.origin.map(ResolvedOriginDto::from),
		}
	}
}

/// Resolve a git token for a skill `source` on the controller, in-process.
///
/// Returns the token and the origin it is pinned to, or `None` when no
/// credential matches the source (or the keyring cannot be read). The token is
/// never logged.
#[tauri::command]
pub async fn resolve_git_token(source: String) -> Option<ResolvedTokenDto> {
	tauri::async_runtime::spawn_blocking(move || {
		aghub_api::resolve_git_token_for_source(&source)
			.map(ResolvedTokenDto::from)
	})
	.await
	.unwrap_or(None)
}

/// List the source keys that have an explicit credential binding, in-process.
///
/// Used by the desktop to enumerate sources for remote `check-updates`. Returns
/// an empty list when the binding store cannot be read.
#[tauri::command]
pub async fn list_bound_sources() -> Vec<String> {
	tauri::async_runtime::spawn_blocking(aghub_api::list_bound_sources)
		.await
		.unwrap_or_default()
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn maps_token_and_origin_to_camel_case_dto() {
		let resolved = ResolvedToken {
			token: "TOK".to_string(),
			origin: Some(ResolvedOrigin {
				scheme: "https".to_string(),
				host: "github.com".to_string(),
				port: Some(443),
			}),
		};

		let dto = ResolvedTokenDto::from(resolved);
		assert_eq!(dto.token, "TOK");
		let origin = dto.origin.expect("origin");
		assert_eq!(origin.scheme, "https");
		assert_eq!(origin.host, "github.com");
		assert_eq!(origin.port, Some(443));
	}

	#[test]
	fn maps_missing_origin_to_none() {
		let resolved = ResolvedToken {
			token: "TOK".to_string(),
			origin: None,
		};

		let dto = ResolvedTokenDto::from(resolved);
		assert_eq!(dto.token, "TOK");
		assert!(dto.origin.is_none());
	}

	#[test]
	fn dto_serializes_origin_in_camel_case() {
		let dto = ResolvedTokenDto {
			token: "TOK".to_string(),
			origin: Some(ResolvedOriginDto {
				scheme: "https".to_string(),
				host: "git.internal".to_string(),
				port: Some(8443),
			}),
		};

		let json = serde_json::to_value(&dto).expect("serialize");
		assert_eq!(json.get("token").and_then(|v| v.as_str()), Some("TOK"));
		let origin = json.get("origin").expect("origin object");
		assert_eq!(
			origin.get("host").and_then(|v| v.as_str()),
			Some("git.internal"),
		);
		assert_eq!(origin.get("port").and_then(|v| v.as_u64()), Some(8443));
	}

	#[test]
	fn dto_serializes_null_origin() {
		let dto = ResolvedTokenDto {
			token: "TOK".to_string(),
			origin: None,
		};

		let json = serde_json::to_value(&dto).expect("serialize");
		assert!(json.get("origin").expect("origin key present").is_null());
	}
}
