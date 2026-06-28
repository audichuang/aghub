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

#[cfg(test)]
mod tests {
	use super::*;

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
