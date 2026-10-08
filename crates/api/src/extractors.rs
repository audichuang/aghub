use aghub_core::models::AgentType;
use aghub_core::paths::find_project_root;
use aghub_core::WriteScope;
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

	pub fn to_write_scope(&self) -> Result<WriteScope, ApiError> {
		match self {
			ResolvedScope::Global => Ok(WriteScope::Global),
			ResolvedScope::Project { root } => {
				Ok(WriteScope::project(root.clone()))
			}
			ResolvedScope::All { .. } => Err(ApiError::new(
				Status::MethodNotAllowed,
				"scope 'all' is read-only; use 'global' or 'project' for write operations",
				"READ_ONLY_SCOPE",
			)),
		}
	}
}

#[derive(rocket::FromForm)]
pub struct ScopeParams {
	pub scope: Option<String>,
	pub project_root: Option<String>,
}

/// Resolve a possibly-relative project root to an ABSOLUTE path so the Master's
/// canonical dir is absolute (Windows junction targets require it). Uses
/// `canonicalize` when the path exists, else joins onto the current dir.
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

/// Resolve a request body's `scope` string and optional `project_root` into a
/// [`WriteScope`].
///
/// `project_root` is always converted to an absolute path.
/// Errors use the existing API codes:
/// - `INVALID_SCOPE` when scope is neither `"global"` nor `"project"`
/// - `PROJECT_ROOT_REQUIRED` when scope is `"project"` but `project_root` is missing
pub fn resolve_write_scope(
	scope: &str,
	project_root: Option<&str>,
) -> Result<WriteScope, ApiError> {
	match scope {
		"global" => Ok(WriteScope::Global),
		"project" => {
			let root = project_root
				.map(str::trim)
				.filter(|r| !r.is_empty())
				.ok_or_else(|| {
					ApiError::new(
						Status::BadRequest,
						"project scope requires project_root",
						"PROJECT_ROOT_REQUIRED",
					)
				})?;
			Ok(WriteScope::project(absolutize_root(root)))
		}
		_ => Err(ApiError::new(
			Status::BadRequest,
			format!("Invalid scope '{scope}'. Use 'global' or 'project'"),
			"INVALID_SCOPE",
		)),
	}
}

/// Parse a comma-separated list of agent IDs into [`AgentType`]s.
///
/// Uses [`AgentType::parse_list`] so empty tokens are rejected and duplicates
/// are removed in order, matching CLI `-a`.
pub fn resolve_agent_list(s: &str) -> Result<Vec<AgentType>, ApiError> {
	AgentType::parse_list(s)
		.map_err(|err| ApiError::new(Status::BadRequest, err, "INVALID_PARAM"))
}

/// Parse a slice of agent ID strings into [`AgentType`]s.
///
/// Delegates to [`resolve_agent_list`] so empty tokens and duplicate rules stay
/// identical to CLI `-a`.
pub fn resolve_agent_strings<S: AsRef<str>>(
	agents: &[S],
) -> Result<Vec<AgentType>, ApiError> {
	if agents.is_empty() {
		return Ok(Vec::new());
	}
	let joined = agents
		.iter()
		.map(|s| s.as_ref())
		.collect::<Vec<_>>()
		.join(",");
	resolve_agent_list(&joined)
}

impl ScopeParams {
	/// Resolve the request scope. A MISSING `scope` defaults to `global` —
	/// INTENTIONALLY unlike the CLI's `All` (`resolve_read_scopes`): the server's
	/// cwd is not the caller's project, so `All` would guess the wrong root.
	/// The desktop always sends a scope; this only affects raw HTTP callers.
	/// Pinned by `routes::sources::tests::missing_scope_defaults_to_global_not_all`.
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

/// Request guard against browser cross-origin / DNS-rebinding attacks on the
/// localhost API, without a shared token (see `crates/api/AGENTS.md` CORS).
/// Both checks are LENIENT when the header is absent, so non-browser clients
/// (CLI, curl, the SSH-tunnel proxy, the local test client) pass:
///
/// - `Origin` present and not a trusted local origin → 403 (cross-origin page).
/// - `Host` present and not a trusted local host → 403 (DNS-rebinding: same
///   origin, no Origin header, attacker's Host — CORS cannot see it).
///
/// Mounted on every `/api/v1` route except OPTIONS; enforced by
/// `all_routes_reject_foreign_host`.
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

	fn unwrap_ok<T>(res: Result<T, ApiError>) -> T {
		match res {
			Ok(v) => v,
			Err(e) => panic!("expected Ok, got ApiError: {}", e.body.error),
		}
	}

	fn unwrap_err<T>(res: Result<T, ApiError>) -> ApiError {
		match res {
			Ok(_) => panic!("expected Err(ApiError), got Ok"),
			Err(e) => e,
		}
	}

	#[test]
	fn resolve_write_scope_valid_global() {
		let scope = unwrap_ok(resolve_write_scope("global", None));
		assert_eq!(scope, WriteScope::Global);
	}

	#[test]
	fn resolve_write_scope_valid_project() {
		let path = if cfg!(windows) {
			"C:\\test\\proj"
		} else {
			"/test/proj"
		};
		let scope = unwrap_ok(resolve_write_scope("project", Some(path)));
		assert_eq!(scope, WriteScope::project(path));
	}

	#[test]
	fn resolve_write_scope_relative_root_becomes_absolute() {
		let scope =
			unwrap_ok(resolve_write_scope("project", Some("relative/path")));
		match scope {
			WriteScope::Project { root } => {
				assert!(
					root.is_absolute(),
					"project root must be absolutized, got {}",
					root.display()
				);
			}
			WriteScope::Global => panic!("expected Project scope"),
		}
	}

	#[test]
	fn resolve_write_scope_missing_root_returns_project_root_required() {
		let err = unwrap_err(resolve_write_scope("project", None));
		assert_eq!(err.status, Status::BadRequest);
		assert_eq!(err.body.code, "PROJECT_ROOT_REQUIRED");

		let err_empty = unwrap_err(resolve_write_scope("project", Some("   ")));
		assert_eq!(err_empty.status, Status::BadRequest);
		assert_eq!(err_empty.body.code, "PROJECT_ROOT_REQUIRED");
	}

	#[test]
	fn resolve_write_scope_unknown_scope_returns_invalid_scope() {
		let err = unwrap_err(resolve_write_scope("unknown", None));
		assert_eq!(err.status, Status::BadRequest);
		assert_eq!(err.body.code, "INVALID_SCOPE");
	}

	#[test]
	fn resolve_agent_list_empty_token_returns_invalid_param() {
		let err = unwrap_err(resolve_agent_list("claude,,grok"));
		assert_eq!(err.status, Status::BadRequest);
		assert_eq!(err.body.code, "INVALID_PARAM");

		let err_trailing = unwrap_err(resolve_agent_list("claude,"));
		assert_eq!(err_trailing.status, Status::BadRequest);
		assert_eq!(err_trailing.body.code, "INVALID_PARAM");

		let err_empty = unwrap_err(resolve_agent_list(""));
		assert_eq!(err_empty.status, Status::BadRequest);
		assert_eq!(err_empty.body.code, "INVALID_PARAM");
	}

	#[test]
	fn resolve_agent_list_duplicate_agents_deduplicated() {
		let agents = unwrap_ok(resolve_agent_list("claude,claude,grok"));
		assert_eq!(agents, vec![AgentType::Claude, AgentType::Grok]);
	}
}
