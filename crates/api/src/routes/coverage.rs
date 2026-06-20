use rocket::get;
use rocket::serde::json::Json;

use crate::dto::agent_coverage::AgentSkillCoverageDto;
use crate::error::ApiResult;
use crate::extractors::{ResolvedScope, ScopeParams};
use aghub_core::models::ResourceScope;
use aghub_core::skills::linker::classify::{classify_all, LinkNeed};
use aghub_core::skills::linker::universal_canonical_dir;

/// `GET /api/v1/skills/coverage?scope=<global|project>&project_root=<path?>`
///
/// Classifies every registered agent against the canonical `.agents/skills`
/// master SKILLS-DIR for the requested scope and returns a per-agent coverage
/// projection. Canonicalization stays server-side. `project_root` is
/// absolutized by `ScopeParams::resolve` (P0-C) before reaching the classifier.
#[get("/skills/coverage?<params..>")]
pub async fn skills_coverage(
	params: ScopeParams,
) -> ApiResult<Vec<AgentSkillCoverageDto>> {
	let resolved = params.resolve()?;
	let (scope, project_root, scope_str) = match &resolved {
		ResolvedScope::Global => (ResourceScope::GlobalOnly, None, "global"),
		ResolvedScope::Project { root } => {
			(ResourceScope::ProjectOnly, Some(root.as_path()), "project")
		}
		ResolvedScope::All { .. } => {
			return Err(crate::error::ApiError::new(
				rocket::http::Status::BadRequest,
				"scope 'all' is not supported for coverage; use 'global' or \
				 'project'",
				"INVALID_PARAM",
			));
		}
	};

	let master_skills_dir =
		universal_canonical_dir(project_root).ok_or_else(|| {
			crate::error::ApiError::new(
				rocket::http::Status::InternalServerError,
				"could not resolve the universal master skills directory",
				"COVERAGE_ERROR",
			)
		})?;

	let plans = classify_all(scope, project_root, &master_skills_dir);
	let dtos = plans
		.into_iter()
		.map(|plan| {
			let auto_covered = matches!(plan.need, LinkNeed::NativeReader);
			let needs_link = matches!(plan.need, LinkNeed::NeedsLink { .. });
			let supported = !matches!(plan.need, LinkNeed::Unsupported);
			AgentSkillCoverageDto {
				id: plan.agent_id.to_string(),
				scope: scope_str.to_string(),
				reads_master: plan.reads_master,
				writes_master: plan.writes_master,
				needs_link,
				auto_covered,
				supported,
			}
		})
		.collect();
	Ok(Json(dtos))
}

#[cfg(test)]
mod tests {
	use super::*;

	fn block_on<F: std::future::Future>(fut: F) -> F::Output {
		rocket::tokio::runtime::Builder::new_current_thread()
			.enable_all()
			.build()
			.unwrap()
			.block_on(fut)
	}

	#[test]
	fn global_scope_buckets_codex_native_claude_needs_link() {
		// This asserts on `auto_covered`, which is derived from the real
		// `dirs::home_dir()` (codex @global reads `~/.agents/skills`). Other api
		// tests transiently repoint HOME/XDG to temp dirs under `test_env_lock`;
		// hold the SAME lock here so none can race our home read mid-classify.
		let _env = crate::routes::test_env_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		let params = ScopeParams {
			scope: Some("global".to_string()),
			project_root: None,
		};
		let dtos = block_on(skills_coverage(params))
			.ok()
			.expect("handler ok")
			.into_inner();
		let codex = dtos
			.iter()
			.find(|d| d.id == "codex")
			.expect("codex present");
		assert!(codex.auto_covered, "codex @global reads .agents/skills");
		assert!(!codex.needs_link);
		assert!(codex.supported);
		let claude = dtos
			.iter()
			.find(|d| d.id == "claude")
			.expect("claude present");
		assert!(
			claude.needs_link,
			"claude @global has a private skills dir => NeedsLink"
		);
		assert!(!claude.auto_covered);
	}

	#[test]
	fn coverage_rejects_scope_all() {
		let params = ScopeParams {
			scope: Some("all".to_string()),
			project_root: None,
		};
		let err =
			block_on(skills_coverage(params)).expect_err("scope=all rejected");
		assert_eq!(err.status, rocket::http::Status::BadRequest);
	}

	#[test]
	fn coverage_route_is_mounted() {
		let client =
			rocket::local::blocking::Client::tracked(crate::build_rocket(
				rocket::Config::default(),
				crate::default_app_data_dir(),
			))
			.expect("rocket builds");
		let resp = client
			.get("/api/v1/skills/coverage?scope=global")
			.dispatch();
		assert_eq!(resp.status(), rocket::http::Status::Ok);
	}
}
