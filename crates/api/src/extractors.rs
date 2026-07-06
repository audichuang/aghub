use aghub_core::models::AgentType;
use aghub_core::paths::find_project_root;
use rocket::http::Status;
use rocket::request::FromParam;
use std::path::PathBuf;

use crate::error::ApiError;

pub struct AgentParam(pub AgentType);

impl<'r> FromParam<'r> for AgentParam {
	type Error = String;

	fn from_param(param: &'r str) -> Result<Self, Self::Error> {
		param.parse::<AgentType>().map(AgentParam)
	}
}

pub enum ResolvedScope {
	Global,
	Project { root: PathBuf },
	All { project_root: Option<PathBuf> },
}

impl ResolvedScope {
	pub fn is_all(&self) -> bool {
		matches!(self, ResolvedScope::All { .. })
	}
}

#[derive(rocket::FromForm)]
pub struct ScopeParams {
	pub scope: Option<String>,
	pub project_root: Option<String>,
}

/// Resolve a possibly-relative project root to an ABSOLUTE path so the
/// universal-master canonical dir is absolute (junction targets require it —
/// spec Decision 6 / P0-C). Uses `canonicalize` when the path exists, else
/// joins onto the current dir without requiring existence.
pub fn absolutize_root(root: &str) -> PathBuf {
	let p = PathBuf::from(root);
	if p.is_absolute() {
		return p;
	}
	if let Ok(canon) = std::fs::canonicalize(&p) {
		return canon;
	}
	std::env::current_dir().map(|cwd| cwd.join(&p)).unwrap_or(p)
}

impl ScopeParams {
	/// Resolve the request scope. A MISSING `scope` defaults to `global` — this
	/// is INTENTIONALLY different from the CLI, whose unscoped `source
	/// list`/`diff` default to `All` (global + the detected project).
	///
	/// The two surfaces differ on purpose: the CLI runs in a user's working
	/// directory and can cheaply detect a project root, so "everything in
	/// scope here" (`All`) is the useful default. The API is a stateless
	/// localhost server with no meaningful cwd — `All` would have to guess a
	/// project root from the server process's directory, which is not the
	/// caller's project. The desktop client (the only real consumer) always
	/// sends an explicit `scope`, so this default only affects raw HTTP
	/// callers, for whom `global` is the safe, unambiguous choice.
	/// `routes::sources::tests::missing_scope_defaults_to_global_not_all` pins
	/// this; the CLI's `All` default lives in `resolve_read_scopes`.
	pub fn resolve(&self) -> Result<ResolvedScope, ApiError> {
		let scope = self.scope.as_deref().unwrap_or("global");
		match scope {
			"global" => Ok(ResolvedScope::Global),
			"project" => {
				let root = self.project_root.as_deref().ok_or_else(|| {
					ApiError::new(
						Status::BadRequest,
						"project_root is required when scope=project",
						"MISSING_PARAM",
					)
				})?;
				Ok(ResolvedScope::Project {
					root: absolutize_root(root),
				})
			}
			"all" => {
				let project_root =
					self.project_root.as_deref().map(PathBuf::from).or_else(
						|| {
							std::env::current_dir()
								.ok()
								.and_then(|cwd| find_project_root(&cwd))
						},
					);
				Ok(ResolvedScope::All { project_root })
			}
			other => Err(ApiError::new(
				Status::BadRequest,
				format!(
					"Unknown scope '{other}'. Use 'global', 'project', or 'all'"
				),
				"INVALID_PARAM",
			)),
		}
	}
}

/// Request guard that blocks browser cross-origin / DNS-rebinding attacks on the
/// localhost API without a shared token (a token would collide with this fork's
/// SSH-remote / multi-connection model — see api/AGENTS.md). Two header checks,
/// both LENIENT when the header is absent so non-browser clients (CLI, curl, the
/// SSH-tunnel proxy, Rocket's local test client) are unaffected — only a browser
/// reliably attaches these:
///
/// - `Origin` present and NOT a trusted local origin → 403. A malicious page's
///   cross-origin request always carries its own Origin; a same-origin webview
///   call carries the trusted `tauri://localhost` / `http://localhost:1420`.
/// - `Host` present and NOT a trusted local host → 403. Closes DNS-rebinding,
///   where the attacker page is same-origin (no Origin sent) but the Host is the
///   attacker's rebound domain. (Layer-1 CORS can't see this; upstream's guard
///   omitted the Host check.)
///
/// Only mounted on routes that touch keyring/OS credentials or act as a
/// credential-existence oracle; read-only list/get routes rely on Layer-1 CORS.
pub struct TrustedLocalOrigin;

/// Extract the host from an `authority` (`host`, `host:port`, or `[::1]:port`).
fn host_from_authority(authority: &str) -> Option<&str> {
	let authority = authority.trim();
	if let Some(rest) = authority.strip_prefix('[') {
		// IPv6 literal: `[::1]:port` → `::1`
		rest.split_once(']').map(|(host, _)| host)
	} else {
		authority.split(':').next()
	}
}

fn is_trusted_local_host(host: &str) -> bool {
	matches!(
		host.to_ascii_lowercase().as_str(),
		"localhost" | "127.0.0.1" | "::1" | "tauri.localhost"
	)
}

fn origin_scheme_host(origin: &str) -> Option<(&str, &str)> {
	let (scheme, rest) = origin.trim().split_once("://")?;
	let authority = rest.split('/').next()?;
	Some((scheme, host_from_authority(authority)?))
}

fn is_trusted_local_origin(origin: &str) -> bool {
	let Some((scheme, host)) = origin_scheme_host(origin) else {
		return false;
	};
	matches!(
		scheme.to_ascii_lowercase().as_str(),
		"http" | "https" | "tauri"
	) && is_trusted_local_host(host)
}

#[rocket::async_trait]
impl<'r> rocket::request::FromRequest<'r> for TrustedLocalOrigin {
	type Error = ();

	async fn from_request(
		request: &'r rocket::Request<'_>,
	) -> rocket::request::Outcome<Self, Self::Error> {
		use rocket::request::Outcome;
		if let Some(origin) = request.headers().get_one("Origin") {
			if !is_trusted_local_origin(origin) {
				return Outcome::Error((Status::Forbidden, ()));
			}
		}
		if let Some(host) = request.headers().get_one("Host") {
			let trusted =
				host_from_authority(host).is_some_and(is_trusted_local_host);
			if !trusted {
				return Outcome::Error((Status::Forbidden, ()));
			}
		}
		Outcome::Success(Self)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn trusted_local_origins_pass_and_foreign_fail() {
		for ok in [
			"http://localhost:1420",
			"https://tauri.localhost",
			"tauri://localhost",
			"http://127.0.0.1:8000",
		] {
			assert!(is_trusted_local_origin(ok), "{ok} should be trusted");
		}
		for bad in [
			"http://evil.example",
			"https://localhost.evil.com",
			"http://127.0.0.1.evil.com",
			"ftp://localhost",
			"not-a-url",
		] {
			assert!(!is_trusted_local_origin(bad), "{bad} must be rejected");
		}
	}

	#[test]
	fn trusted_local_hosts_cover_ipv6_and_ports() {
		assert!(host_from_authority("localhost:1420")
			.is_some_and(is_trusted_local_host));
		assert!(host_from_authority("[::1]:8000")
			.is_some_and(is_trusted_local_host));
		assert!(
			host_from_authority("127.0.0.1").is_some_and(is_trusted_local_host)
		);
		// DNS-rebinding host must fail.
		assert!(!host_from_authority("evil.example:8000")
			.is_some_and(is_trusted_local_host));
	}

	#[test]
	fn resolve_project_absolutizes_relative_root() {
		let params = ScopeParams {
			scope: Some("project".to_string()),
			project_root: Some("relative/proj".to_string()),
		};
		match params.resolve().unwrap_or_else(|_| panic!("resolves")) {
			ResolvedScope::Project { root } => assert!(
				root.is_absolute(),
				"project root must be absolutized, got {}",
				root.display()
			),
			_ => panic!("expected Project scope"),
		}
	}
}
