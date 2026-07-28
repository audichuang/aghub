use aghub_cc_plugins::claude::ClaudePluginManager;
#[cfg(test)]
use aghub_core::create_adapter;
use aghub_core::{
	errors::ConfigError,
	load_all_agents,
	models::{AgentType, ResourceScope, Skill},
	registry, transfer,
};
use rocket::http::Status;
use rocket::serde::json::Json;
use std::{
	path::{Path, PathBuf},
	time::Duration,
};
use tokio::time::timeout;

use crate::{
	credentials::forwarding::ForwardedGitTokens,
	credentials::source_auth::SourceAuth,
	dto::integrations::{
		CodeEditorType, EditSkillFolderRequest, OpenSkillFolderRequest,
	},
	dto::skill::{
		CreateSkillRequest, DeleteSkillByPathRequest,
		DeleteSkillByPathResponse, GitCredentialStatus,
		GitCredentialStatusQuery, GitCredentialStatusResponse,
		GitInstallRequest, GitInstallResponse, GitInstallResultEntry,
		GitScanRequest, GitScanResponse, GitScanSkillEntry, GitSyncRequest,
		GitSyncResponse, GlobalSkillLockResponse, InstallSkillRequest,
		InstallSkillResponse, LocalSkillLockEntryResponse, ProjectLockQuery,
		ProjectSkillLockResponse, PruneLockRequest, PruneLockResponse,
		SkillContentQuery, SkillLockEntryResponse, SkillResponse,
		SkillTreeNodeKind, SkillTreeNodeResponse, SkillTreeQuery,
		SkillUsageResponse, UpdateSkillRequest, ValidationError,
	},
	dto::transfer::{
		OperationBatchResponse, ReconcileRequest, TransferRequest,
	},
	error::{ApiCreated, ApiError, ApiResult},
	extractors::{AgentParam, ResolvedScope, ScopeParams, TrustedLocalOrigin},
	routes::{
		build_manager_from_resolved, require_writable_scope,
		resolved_to_resource_scope,
	},
	skills::rename::{skill_renamed_message, SKILL_RENAMED_CODE},
	source_sessions::{
		PinnedSourceFetchError, PinnedSourceSession, PinnedSourceSessions,
	},
};
use skill_update::TokenResolver;

#[derive(rocket::FromForm)]
pub(crate) struct SkillListParams {
	scope: Option<String>,
	project_root: Option<String>,
	include_managed: Option<bool>,
}

#[derive(rocket::FromForm)]
pub struct DeleteSkillParams {
	scope: Option<String>,
	project_root: Option<String>,
	confirm: Option<bool>,
	all_agents: Option<bool>,
}

impl DeleteSkillParams {
	fn resolve_scope(
		&self,
	) -> Result<crate::extractors::ResolvedScope, ApiError> {
		ScopeParams {
			scope: self.scope.clone(),
			project_root: self.project_root.clone(),
		}
		.resolve()
	}
}

impl SkillListParams {
	fn resolve_scope(
		&self,
	) -> Result<crate::extractors::ResolvedScope, ApiError> {
		ScopeParams {
			scope: self.scope.clone(),
			project_root: self.project_root.clone(),
		}
		.resolve()
	}

	fn include_managed(&self) -> bool {
		self.include_managed.unwrap_or(false)
	}
}

fn expand_tilde_path(path: &str) -> std::path::PathBuf {
	if path.starts_with("~/") {
		dirs::home_dir()
			.map(|home| home.join(&path[2..]))
			.unwrap_or_else(|| path.into())
	} else {
		path.into()
	}
}

async fn detect_plugin_for_path(path: &std::path::Path) -> Option<String> {
	let plugins = ClaudePluginManager::new().await.ok()?;
	plugins
		.plugin_owning_path(path)
		.map(|plugin| plugin.display_name.clone())
}

async fn list_branches_for_scan<F>(
	cached_branches: Option<Vec<String>>,
	fetcher: F,
) -> Result<Vec<String>, ApiError>
where
	F: FnOnce() -> aghub_git::Result<Vec<String>> + Send + 'static,
{
	if let Some(cached) = cached_branches {
		return Ok(cached);
	}

	tokio::task::spawn_blocking(fetcher)
		.await
		.map_err(|e| {
			ApiError::from_join_error(
				e,
				"Branch listing task failed",
				"BRANCHES_ERROR",
			)
		})?
		.map_err(|e| {
			ApiError::new(
				Status::BadRequest,
				format!("Failed to list remote branches: {e}"),
				"BRANCHES_ERROR",
			)
		})
}

#[post("/skills/transfer", data = "<body>")]
pub fn transfer_skill_route(
	_origin: TrustedLocalOrigin,
	body: Json<TransferRequest>,
) -> ApiResult<OperationBatchResponse> {
	let req = body.into_inner();
	let source = req.source.to_core()?;
	let destinations = req
		.destinations
		.iter()
		.map(|target| target.to_core())
		.collect::<Result<Vec<_>, _>>()?;
	let result = transfer::transfer_skill(source, destinations)
		.map_err(ApiError::from)?;
	Ok(Json(result.into()))
}

#[post("/skills/reconcile", data = "<body>")]
pub fn reconcile_skill_route(
	_origin: TrustedLocalOrigin,
	body: Json<ReconcileRequest>,
) -> ApiResult<OperationBatchResponse> {
	let req = body.into_inner();
	let source = req.source.to_core()?;

	let added: Vec<AgentType> = req
		.added
		.unwrap_or_default()
		.iter()
		.map(|agent_str| {
			agent_str.parse().map_err(|_| {
				ApiError::new(
					rocket::http::Status::BadRequest,
					format!("Unknown agent '{agent_str}'"),
					"INVALID_PARAM",
				)
			})
		})
		.collect::<Result<Vec<AgentType>, _>>()?;

	let removed: Vec<AgentType> = req
		.removed
		.unwrap_or_default()
		.iter()
		.map(|agent_str| {
			agent_str.parse().map_err(|_| {
				ApiError::new(
					rocket::http::Status::BadRequest,
					format!("Unknown agent '{agent_str}'"),
					"INVALID_PARAM",
				)
			})
		})
		.collect::<Result<Vec<AgentType>, _>>()?;

	let result = transfer::reconcile_skill(source, added, removed)
		.map_err(ApiError::from)?;

	Ok(Json(result.into()))
}

#[delete("/skills/by-path", data = "<body>")]
pub async fn delete_skill_by_path(
	_origin: TrustedLocalOrigin,
	body: Json<DeleteSkillByPathRequest>,
) -> ApiResult<DeleteSkillByPathResponse> {
	let req = body.into_inner();

	let skill_path = expand_tilde_path(&req.source_path);
	let skill_dir = if skill_path.is_dir() {
		skill_path
	} else {
		skill_path
			.parent()
			.map(|p| p.to_path_buf())
			.unwrap_or(skill_path)
	};

	let resource_scope = match req.scope.as_str() {
		"global" => aghub_core::models::ResourceScope::GlobalOnly,
		"project" => aghub_core::models::ResourceScope::ProjectOnly,
		_ => {
			return Ok(Json(DeleteSkillByPathResponse {
				success: false,
				error: Some(format!("Invalid scope: {}", req.scope)),
				..Default::default()
			}));
		}
	};

	if resource_scope == aghub_core::models::ResourceScope::ProjectOnly
		&& req.project_root.is_none()
	{
		return Ok(Json(DeleteSkillByPathResponse {
			success: false,
			error: Some(
				"project_root is required when scope is 'project'".to_string(),
			),
			..Default::default()
		}));
	}

	let project_root = req
		.project_root
		.as_ref()
		.map(|r| crate::extractors::absolutize_root(r));

	let mut validation_errors = Vec::new();

	for agent_str in &req.agents {
		let agent: AgentType = match agent_str.parse() {
			Ok(a) => a,
			Err(_) => {
				validation_errors.push(ValidationError {
					agent: agent_str.clone(),
					reason: format!("Unknown agent: {agent_str}"),
				});
				continue;
			}
		};

		let adapter = aghub_core::create_adapter(agent);
		let skills_paths =
			adapter.get_skills_paths(project_root.as_deref(), resource_scope);

		let is_valid = skills_paths
			.iter()
			.any(|sp| skill_dir.starts_with(sp) || skill_dir == *sp);

		if !is_valid {
			let valid_paths: Vec<String> = skills_paths
				.iter()
				.map(|p| p.display().to_string())
				.collect();
			validation_errors.push(ValidationError {
				agent: agent_str.clone(),
				reason: format!(
					"Path '{}' is not in agent's skills directories: {}",
					skill_dir.display(),
					valid_paths.join(", ")
				),
			});
		}
	}

	if !validation_errors.is_empty() {
		return Ok(Json(DeleteSkillByPathResponse {
			success: false,
			error: Some("Validation failed for one or more agents".to_string()),
			validation_errors: Some(validation_errors),
			..Default::default()
		}));
	}

	if !skill_dir.exists() {
		// Idempotent: nothing on disk to remove.
		return Ok(Json(DeleteSkillByPathResponse {
			success: true,
			dry_run: !req.confirm.unwrap_or(false),
			deleted_path: Some(skill_dir.display().to_string()),
			..Default::default()
		}));
	}

	if let Some(plugin_name) = detect_plugin_for_path(&skill_dir).await {
		return Ok(Json(DeleteSkillByPathResponse {
			success: false,
			error: Some(format!(
				"Cannot delete plugin-managed skill from plugin '{plugin_name}'"
			)),
			..Default::default()
		}));
	}

	// Containment guard (canonicalize-escape protection): the resolved dir must
	// stay inside an allow-listed skills root, even if `skill_dir` is a symlink.
	let agent_dirs: Vec<std::path::PathBuf> = req
		.agents
		.iter()
		.filter_map(|a| a.parse::<AgentType>().ok())
		.flat_map(|a| {
			aghub_core::create_adapter(a)
				.get_skills_paths(project_root.as_deref(), resource_scope)
		})
		.collect();
	let roots = aghub_core::skills::removal::allowed_skill_roots(
		&agent_dirs,
		project_root.as_deref(),
	);
	if aghub_core::skills::removal::assert_contained(&skill_dir, &roots)
		.is_none()
	{
		return Ok(Json(DeleteSkillByPathResponse {
			success: false,
			error: Some(
				"Refusing to delete: resolved path is outside the \
				 allow-listed skills roots"
					.to_string(),
			),
			skipped: vec![skill_dir.display().to_string()],
			..Default::default()
		}));
	}

	let confirm = req.confirm.unwrap_or(false);
	let dry_run = !confirm;
	let skill_file = skill_dir.join("SKILL.md");
	let skill_name = skill::parser::parse(&skill_file)
		.map(|parsed| parsed.name)
		.unwrap_or_else(|_| {
			skill_dir
				.file_name()
				.and_then(|n| n.to_str())
				.unwrap_or_default()
				.to_string()
		});
	let Some(first_agent) =
		req.agents.iter().find_map(|a| a.parse::<AgentType>().ok())
	else {
		return Ok(Json(DeleteSkillByPathResponse {
			success: false,
			error: Some("No valid agent was provided".to_string()),
			..Default::default()
		}));
	};
	let resolved = match resource_scope {
		ResourceScope::GlobalOnly => ResolvedScope::Global,
		ResourceScope::ProjectOnly => ResolvedScope::Project {
			root: project_root.clone().expect("validated project root"),
		},
		ResourceScope::Both => {
			return Ok(Json(DeleteSkillByPathResponse {
				success: false,
				error: Some("scope 'all' is not writable".to_string()),
				..Default::default()
			}));
		}
	};
	let mut manager =
		build_manager_from_resolved(&AgentParam(first_agent), &resolved)?;
	if let Err(error) = manager.load() {
		return Ok(Json(DeleteSkillByPathResponse {
			success: false,
			error: Some(format!("Failed to load agent skills: {error}")),
			..Default::default()
		}));
	}
	// Hold the interprocess mutation lock for every executing path below, taken
	// BEFORE the layout and referrer inspection that decides what to delete.
	// The copy-layout branch further down does its own referrer sweep and
	// `execute_removal` instead of going through `remove_skill_planned`, so
	// without this it would read "no other agent references this master", then
	// delete a master another aghub process re-linked in the meantime. A dry-run
	// mutates nothing and takes no lock.
	let _mutation_guard = if dry_run {
		None
	} else {
		match aghub_core::skills::lock::mutation_guard(
			"delete skill by path",
			resource_scope,
			project_root.as_deref(),
		) {
			Ok(guard) => Some(guard),
			Err(error) => {
				return Ok(Json(DeleteSkillByPathResponse {
					success: false,
					error: Some(error.to_string()),
					..Default::default()
				}));
			}
		}
	};

	let path_is_link = aghub_core::skills::linker::Linker::is_link(&skill_dir);
	let canonical_layout = manager
		.get_skill(&skill_name)
		.and_then(|skill| skill.canonical_path.as_ref())
		.is_some()
		|| path_is_link;

	if !canonical_layout {
		// Guard: this non-link branch bypasses `plan_removal`'s referrer
		// sweep (`Linker::is_link` covers symlinks AND Windows junctions),
		// so re-apply it here. If the targeted dir is a shared universal
		// master that ANOTHER in-scope agent still symlinks into (discovered
		// as a real dir by a direct `.agents/skills` reader, so
		// canonical_path=None), refuse to `remove_dir_all` it — that would
		// orphan the live symlink and lose the skill for every other agent.
		let all_in_scope =
			aghub_core::skills::removal::agent_skill_dirs_in_scope(
				resource_scope,
				project_root.as_deref(),
			);
		let safe_name = skill::sanitize::sanitize_name(&skill_name);
		if aghub_core::skills::removal::dir_has_external_referrer(
			&skill_dir,
			&all_in_scope,
			&safe_name,
		) {
			return Ok(Json(DeleteSkillByPathResponse {
				success: true,
				dry_run,
				skipped: vec![skill_dir.display().to_string()],
				..Default::default()
			}));
		}
		let plan = aghub_core::skills::removal::RemovalPlan {
			layout: aghub_core::skills::removal::Layout::Copy,
			paths: vec![skill_dir.clone()],
			skipped: vec![],
			needs_confirm: false,
		};
		if dry_run {
			return Ok(Json(delete_response_from_outcome(
				aghub_core::skills::removal::RemovalOutcome {
					plan,
					executed: false,
					prune: aghub_core::skills::removal::PruneStatus::NotRun,
				},
			)));
		}
		let report =
			match aghub_core::skills::removal::execute_removal(&plan, &roots) {
				Ok(report) => report,
				Err(e) => {
					return Ok(Json(DeleteSkillByPathResponse {
						success: false,
						error: Some(format!("Failed to delete: {e}")),
						..Default::default()
					}));
				}
			};
		let mut executed_plan = plan;
		executed_plan.paths = report.removed;
		executed_plan.skipped.extend(report.skipped);
		executed_plan
			.skipped
			.extend(report.failed.into_iter().map(|(path, _)| path));
		// Reconcile the per-scope lock against disk through the SAME core-owned
		// seam the manager uses, and report its real PruneStatus (not NotRun) so
		// `pruned_lock_entries`/`prune_error` truthfully reflect the cleanup.
		let prune = aghub_core::skills::prune::prune_lock_for_scope(
			resource_scope,
			project_root.as_deref(),
		);
		return Ok(Json(delete_response_from_outcome(
			aghub_core::skills::removal::RemovalOutcome {
				plan: executed_plan,
				executed: true,
				prune,
			},
		)));
	}

	match manager.remove_skill_planned(&skill_name, false, dry_run, confirm) {
		// `remove_skill_planned` already prunes the lock (core-owned seam) and
		// records the status in `outcome.prune`; no route-level re-prune.
		Ok(outcome) => Ok(Json(delete_response_from_outcome(outcome))),
		Err(e) => Ok(Json(DeleteSkillByPathResponse {
			success: false,
			error: Some(format!("Failed to delete: {e}")),
			..Default::default()
		})),
	}
}

/// Disk-reconciled, lock-only prune (renamed to avoid colliding with
/// `transfer::reconcile_skill` / `POST /skills/reconcile`). Defaults to a
/// dry-run; `confirm: true` writes. Any disk-scan error aborts the prune and is
/// reported in `error` with the lock left untouched.
#[post("/skills/prune-lock", data = "<body>")]
pub fn prune_lock_route(
	_origin: TrustedLocalOrigin,
	body: Json<PruneLockRequest>,
) -> ApiResult<PruneLockResponse> {
	use aghub_core::skills::prune::{
		preview_prune, prune_lock_scanning, PruneScope,
	};
	let req = body.into_inner();

	let scope = match req.scope.as_str() {
		"global" => PruneScope::Global,
		"project" => PruneScope::Project,
		other => {
			return Ok(Json(PruneLockResponse {
				pruned: vec![],
				dry_run: true,
				error: Some(format!("Invalid scope: {other}")),
			}));
		}
	};
	let project_root = req.project_root.as_ref().map(std::path::PathBuf::from);
	let dry_run = !req.confirm.unwrap_or(false);

	let result = if dry_run {
		preview_prune(scope, project_root.as_deref())
	} else {
		prune_lock_scanning(scope, project_root.as_deref())
	};

	match result {
		Ok(pruned) => Ok(Json(PruneLockResponse {
			pruned,
			dry_run,
			error: None,
		})),
		Err(e) => Ok(Json(PruneLockResponse {
			pruned: vec![],
			dry_run,
			error: Some(e.to_string()),
		})),
	}
}

fn get_parent_folder(path: std::path::PathBuf) -> std::path::PathBuf {
	path.parent().map(|p| p.to_path_buf()).unwrap_or(path)
}

fn get_skill_root(path: std::path::PathBuf) -> std::path::PathBuf {
	let is_skill_file = path
		.file_name()
		.is_some_and(|name| name == std::ffi::OsStr::new("SKILL.md"));
	if is_skill_file {
		get_parent_folder(path)
	} else {
		path
	}
}

fn delete_response_from_outcome(
	outcome: aghub_core::skills::removal::RemovalOutcome,
) -> DeleteSkillByPathResponse {
	DeleteSkillByPathResponse {
		success: true,
		dry_run: !outcome.executed,
		executed: outcome.executed,
		needs_confirm: outcome.plan.needs_confirm,
		paths: outcome
			.plan
			.paths
			.iter()
			.map(|p| p.display().to_string())
			.collect(),
		skipped: outcome
			.plan
			.skipped
			.iter()
			.map(|p| p.display().to_string())
			.collect(),
		deleted_path: outcome
			.executed
			.then(|| {
				outcome.plan.paths.first().map(|p| p.display().to_string())
			})
			.flatten(),
		pruned_lock_entries: match &outcome.prune {
			aghub_core::skills::removal::PruneStatus::NotRun => None,
			aghub_core::skills::removal::PruneStatus::Pruned(keys) => {
				Some(keys.clone())
			}
			aghub_core::skills::removal::PruneStatus::Failed {
				pruned, ..
			} => Some(pruned.clone()),
		},
		prune_error: match &outcome.prune {
			aghub_core::skills::removal::PruneStatus::Failed {
				reason, ..
			} => Some(reason.clone()),
			_ => None,
		},
		error: None,
		validation_errors: None,
	}
}

#[cfg(test)]
fn resolve_git_install_target_dir(
	agent_type: AgentType,
	resource_scope: ResourceScope,
	project_root: Option<&std::path::PathBuf>,
) -> Option<std::path::PathBuf> {
	create_adapter(agent_type)
		.target_skills_dir(project_root.map(|p| p.as_path()), resource_scope)
}

fn parse_install_scope(scope: &str) -> Result<ResourceScope, ApiError> {
	match scope {
		"global" => Ok(ResourceScope::GlobalOnly),
		"project" => Ok(ResourceScope::ProjectOnly),
		other => Err(ApiError::new(
			Status::BadRequest,
			format!("Invalid scope '{other}'. Use 'global' or 'project'"),
			"INVALID_PARAM",
		)),
	}
}

fn map_remote_source_error(error: aghub_git::SourceError) -> ApiError {
	ApiError::new(
		Status::BadRequest,
		error.to_string(),
		"INVALID_SKILL_SOURCE",
	)
}

#[cfg(test)]
fn map_repo_discovery_error(error: skill::RepoDiscoveryError) -> ApiError {
	match error {
		skill::RepoDiscoveryError::NoSkillsFound
		| skill::RepoDiscoveryError::SkillsNotFound { .. } => ApiError::new(
			Status::NotFound,
			error.to_string(),
			"SKILLS_NOT_FOUND",
		),
		skill::RepoDiscoveryError::Scan(_) => ApiError::new(
			Status::InternalServerError,
			error.to_string(),
			"SCAN_ERROR",
		),
		skill::RepoDiscoveryError::RelativePath { .. } => ApiError::new(
			Status::InternalServerError,
			error.to_string(),
			"SKILL_PATH_ERROR",
		),
	}
}

#[cfg(test)]
fn file_install_source(
	source: &str,
) -> Result<Option<(String, skill::InstallLockSource)>, ApiError> {
	let trimmed = source.trim();
	let Ok(url) = url::Url::parse(trimmed) else {
		return Ok(None);
	};
	if url.scheme() != "file" {
		return Ok(None);
	}
	let path = url.to_file_path().map_err(|_| {
		ApiError::new(
			Status::BadRequest,
			format!("Invalid file skill source '{trimmed}'"),
			"INVALID_SKILL_SOURCE",
		)
	})?;
	let clone_url = trimmed.to_string();
	Ok(Some((
		clone_url.clone(),
		skill::InstallLockSource {
			source: path.display().to_string(),
			source_type: "local".to_string(),
			source_url: clone_url,
			ref_name: None,
		},
	)))
}

fn install_lock_source_from_resolved(
	source: &aghub_git::ResolvedRemoteSource,
	ref_name: Option<String>,
) -> skill::InstallLockSource {
	skill::InstallLockSource {
		source: source.lock_source(),
		source_type: source.source_type.as_str().to_string(),
		source_url: source.source_url.clone(),
		ref_name,
	}
}

fn write_skill_install_lock(
	skill_name: &str,
	resource_scope: ResourceScope,
	project_root: Option<&Path>,
	source: &skill::InstallLockSource,
	lock_skill_path: Option<String>,
	source_dir: &Path,
	ref_commit: Option<String>,
) -> Result<(), ApiError> {
	match resource_scope {
		ResourceScope::GlobalOnly => {
			skill::write_global_install_lock(
				skill_name,
				source,
				lock_skill_path,
				source_dir,
				ref_commit,
			)
			.map_err(|e| {
				ApiError::new(
					Status::InternalServerError,
					format!("Failed to update global skill lock: {e}"),
					"SKILL_LOCK_ERROR",
				)
			})?;
		}
		ResourceScope::ProjectOnly => {
			let cwd = project_root.ok_or_else(|| {
				ApiError::new(
					Status::BadRequest,
					"project_path is required for project skill installs",
					"INVALID_PARAM",
				)
			})?;
			skill::write_project_install_lock(
				skill_name,
				source,
				lock_skill_path,
				source_dir,
				cwd,
				ref_commit,
			)
			.map_err(|e| {
				ApiError::new(
					Status::InternalServerError,
					format!("Failed to update project skill lock: {e}"),
					"SKILL_LOCK_ERROR",
				)
			})?;
		}
		ResourceScope::Both => {
			return Err(ApiError::new(
				Status::BadRequest,
				"Combined skill scope is not supported for installs",
				"INVALID_PARAM",
			));
		}
	}

	Ok(())
}

/// Test-only full-clone helper for the `file://` install fallback. Production
/// install goes through [`SkillRepository`] partial fetch instead.
#[cfg(test)]
fn clone_skill_source_to_temp(
	clone_url: &str,
	is_file_source: bool,
) -> Result<tempfile::TempDir, String> {
	if !is_file_source {
		return aghub_git::clone_to_temp(aghub_git::CloneOptions::new(
			clone_url,
		))
		.map_err(|e| e.to_string());
	}

	let temp_dir = tempfile::TempDir::new().map_err(|e| e.to_string())?;
	let mut prep = gix::clone::PrepareFetch::new(
		clone_url,
		temp_dir.path(),
		gix::create::Kind::WithWorktree,
		Default::default(),
		Default::default(),
	)
	.map_err(|e| e.to_string())?;
	let (mut checkout, _) = prep
		.fetch_then_checkout(
			gix::progress::Discard,
			&gix::interrupt::IS_INTERRUPTED,
		)
		.map_err(|e| format!("Fetch failed: {e}"))?;
	checkout
		.main_worktree(gix::progress::Discard, &gix::interrupt::IS_INTERRUPTED)
		.map_err(|e| format!("Checkout failed: {e}"))?;
	Ok(temp_dir)
}

fn detect_available_editor() -> Option<CodeEditorType> {
	crate::editor_detection::detect_any_installed_editor()
}

/// Build the skill file tree rooted at `path`.
///
/// Symlinks are NOT blanket-rejected: this fork's universal-install layout
/// intentionally symlinks `<agent>/skills/<name>` at the `.agents/skills`
/// master, so the master must show up in the tree. For each symlink entry we
/// canonicalize the target and only recurse into it when it stays inside one of
/// the allow-listed `roots`; a symlink escaping the roots is skipped silently
/// (it never errors the whole tree). The caller has already asserted the
/// top-level `path` is contained, so it is rendered even if it is itself a link.
fn build_skill_tree_node(
	path: &std::path::Path,
	roots: &[PathBuf],
) -> Result<SkillTreeNodeResponse, ApiError> {
	let metadata = std::fs::metadata(path).map_err(|e| {
		ApiError::new(
			Status::NotFound,
			format!("Failed to read skill path metadata: {e}"),
			"SKILL_PATH_NOT_FOUND",
		)
	})?;

	let name = path
		.file_name()
		.map(|name| name.to_string_lossy().to_string())
		.unwrap_or_else(|| path.display().to_string());

	if metadata.is_dir() {
		let mut entries: Vec<_> = std::fs::read_dir(path)
			.map_err(|e| {
				ApiError::new(
					Status::NotFound,
					format!("Failed to read skill directory: {e}"),
					"SKILL_DIRECTORY_NOT_FOUND",
				)
			})?
			.filter_map(|entry| entry.ok())
			// Skip symlink entries whose canonical target escapes the roots,
			// instead of erroring the whole tree (hides escaping links while
			// keeping in-tree universal-install links).
			.filter(|entry| entry_allowed(&entry.path(), roots))
			.collect();

		entries.sort_by(|a, b| {
			let a_is_dir =
				a.file_type().map(|kind| kind.is_dir()).unwrap_or(false);
			let b_is_dir =
				b.file_type().map(|kind| kind.is_dir()).unwrap_or(false);

			b_is_dir.cmp(&a_is_dir).then_with(|| {
				a.file_name()
					.to_string_lossy()
					.to_lowercase()
					.cmp(&b.file_name().to_string_lossy().to_lowercase())
			})
		});

		let children = entries
			.into_iter()
			.map(|entry| build_skill_tree_node(&entry.path(), roots))
			.collect::<Result<Vec<_>, _>>()?;

		return Ok(SkillTreeNodeResponse {
			name,
			path: path.display().to_string(),
			kind: SkillTreeNodeKind::Directory,
			children,
		});
	}

	Ok(SkillTreeNodeResponse {
		name,
		path: path.display().to_string(),
		kind: SkillTreeNodeKind::File,
		children: Vec::new(),
	})
}

/// A directory entry is renderable in the skill tree if it is a real
/// (non-symlink) entry, OR a symlink whose canonical target stays inside one of
/// the allow-listed skills `roots` (the universal-install case). Escaping
/// symlinks are silently excluded so they cannot leak out-of-tree paths.
fn entry_allowed(path: &std::path::Path, roots: &[PathBuf]) -> bool {
	// Recognize a windows junction too (is_symlink() == false for junctions);
	// without this a junction entry would skip the containment guard (P1-E2).
	if !aghub_core::skills::linker::Linker::is_link(path) {
		return true;
	}
	aghub_core::skills::removal::assert_contained(path, roots).is_some()
}

fn check_skills_supported(
	agent: &AgentParam,
	scope: ResourceScope,
) -> Result<(), ApiError> {
	let descriptor = registry::get(agent.0);
	if !descriptor.supports_skill_scope(scope) {
		return Err(ApiError::new(
			Status::UnprocessableEntity,
			format!(
				"Agent '{}' does not support skills in {:?} scope",
				descriptor.id, scope
			),
			"UNSUPPORTED_OPERATION",
		));
	}
	Ok(())
}

fn check_skills_mutable(
	agent: &AgentParam,
	scope: ResourceScope,
) -> Result<(), ApiError> {
	check_skills_supported(agent, scope)?;
	Ok(())
}

#[get("/agents/<agent>/skills?<scope..>")]
pub fn list_skills(
	_origin: TrustedLocalOrigin,
	agent: AgentParam,
	scope: ScopeParams,
) -> ApiResult<Vec<SkillResponse>> {
	let resolved = scope.resolve()?;
	let (resource_scope, _) = resolved_to_resource_scope(&resolved);
	check_skills_supported(&agent, resource_scope)?;
	let mut manager = build_manager_from_resolved(&agent, &resolved)?;

	if resolved.is_all() {
		let (skills, _, _) =
			manager.load_both_annotated().map_err(ApiError::from)?;
		let items = skills.iter().map(SkillResponse::from).collect();
		return Ok(Json(items));
	}

	let config = manager.load().map_err(ApiError::from)?;
	let skills = config.skills.iter().map(SkillResponse::from).collect();
	Ok(Json(skills))
}

/// Usage counts for the installed global Claude skills, from Claude Code's
/// `skillUsage` map. Never-dispatched skills surface as `usage_count: 0`;
/// sorted least-used first. Claude-only (no other agent keeps a counter).
#[get("/skills/usage")]
pub fn list_skill_usage(
	_origin: TrustedLocalOrigin,
) -> ApiResult<Vec<SkillUsageResponse>> {
	let rows = aghub_core::skills::usage::list_claude_skill_usage()
		.into_iter()
		.map(SkillUsageResponse::from)
		.collect();
	Ok(Json(rows))
}

#[post("/agents/<agent>/skills?<scope..>", data = "<body>")]
pub async fn create_skill(
	_origin: TrustedLocalOrigin,
	agent: AgentParam,
	scope: ScopeParams,
	body: Json<CreateSkillRequest>,
) -> ApiCreated<SkillResponse> {
	let resolved = scope.resolve()?;
	let (resource_scope, _) = resolved_to_resource_scope(&resolved);
	check_skills_mutable(&agent, resource_scope)?;
	require_writable_scope(&resolved)?;
	let mut manager = build_manager_from_resolved(&agent, &resolved)?;
	match manager.load() {
		Ok(_) => {}
		Err(ConfigError::NotFound { .. }) => manager.init_empty_config(),
		Err(e) => return Err(ApiError::from(e)),
	}
	let skill = Skill::from(body.into_inner());
	let response = SkillResponse::from(&skill);
	manager.add_skill(skill).map_err(ApiError::from)?;
	Ok((Status::Created, Json(response)))
}

#[post("/agents/<agent>/skills/import?<scope..>", data = "<body>")]
pub fn import_skill(
	_origin: TrustedLocalOrigin,
	agent: AgentParam,
	scope: ScopeParams,
	body: Json<crate::dto::skill::ImportSkillRequest>,
) -> ApiResult<SkillResponse> {
	let resolved = scope.resolve()?;
	let (resource_scope, project_root) = resolved_to_resource_scope(&resolved);
	check_skills_mutable(&agent, resource_scope)?;
	require_writable_scope(&resolved)?;
	let mut manager = build_manager_from_resolved(&agent, &resolved)?;
	let request = body.into_inner();

	// Load configuration before adding skill
	manager.load().map_err(ApiError::from)?;

	// ONE transaction for materialize + hash + lock write. Without this the
	// manager takes the lock for the Master and releases it, then the lock write
	// below takes it again — and another process removing the skill in between
	// leaves this route writing a lock entry for a skill that is gone.
	let _mutation_guard = aghub_core::skills::lock::mutation_guard(
		"import skill",
		resource_scope,
		project_root.as_deref(),
	)
	.map_err(|e| ApiError::internal(e.to_string()))?;

	let imported = manager
		.add_skill_from_path(std::path::Path::new(&request.path))
		.map_err(ApiError::from)?;
	// Hash the local source folder (the SKILL.md's directory).
	let source_dir = get_skill_root(expand_tilde_path(&request.path));
	write_skill_install_lock(
		&imported.name,
		resource_scope,
		project_root.as_deref(),
		&skill::InstallLockSource {
			source: request.path.clone(),
			source_type: "local".to_string(),
			source_url: request.path,
			ref_name: None,
		},
		None,
		&source_dir,
		// Local installs have no upstream commit OID.
		None,
	)?;

	Ok(Json(SkillResponse::from(&imported)))
}

#[get("/agents/<agent>/skills/<name>?<scope..>")]
pub fn get_skill(
	_origin: TrustedLocalOrigin,
	agent: AgentParam,
	name: &str,
	scope: ScopeParams,
) -> ApiResult<SkillResponse> {
	let resolved = scope.resolve()?;
	let (resource_scope, _) = resolved_to_resource_scope(&resolved);
	check_skills_supported(&agent, resource_scope)?;
	let mut manager = build_manager_from_resolved(&agent, &resolved)?;

	if resolved.is_all() {
		let (skills, _, _) =
			manager.load_both_annotated().map_err(ApiError::from)?;
		let skill =
			skills.iter().find(|s| s.name == name).ok_or_else(|| {
				ApiError::from(ConfigError::resource_not_found("skill", name))
			})?;
		return Ok(Json(SkillResponse::from(skill)));
	}

	manager.load().map_err(ApiError::from)?;
	let skill = manager.get_skill(name).ok_or_else(|| {
		ApiError::from(ConfigError::resource_not_found("skill", name))
	})?;
	Ok(Json(SkillResponse::from(skill)))
}

#[put("/agents/<agent>/skills/<name>?<scope..>", data = "<body>")]
pub async fn update_skill(
	_origin: TrustedLocalOrigin,
	agent: AgentParam,
	name: &str,
	scope: ScopeParams,
	body: Json<UpdateSkillRequest>,
) -> ApiResult<SkillResponse> {
	let resolved = scope.resolve()?;
	let (resource_scope, _) = resolved_to_resource_scope(&resolved);
	check_skills_mutable(&agent, resource_scope)?;
	require_writable_scope(&resolved)?;
	let mut manager = build_manager_from_resolved(&agent, &resolved)?;
	manager.load().map_err(ApiError::from)?;
	let existing = manager
		.get_skill(name)
		.ok_or_else(|| {
			ApiError::from(ConfigError::resource_not_found("skill", name))
		})?
		.clone();
	ensure_skill_not_plugin_managed(&existing, "update").await?;
	let updated = body.into_inner().apply_to(existing);
	let response = SkillResponse::from(&updated);
	manager
		.update_skill(name, updated)
		.map_err(ApiError::from)?;
	Ok(Json(response))
}

#[delete("/agents/<agent>/skills/<name>?<params..>")]
pub async fn delete_skill(
	_origin: TrustedLocalOrigin,
	agent: AgentParam,
	name: &str,
	params: DeleteSkillParams,
) -> ApiResult<DeleteSkillByPathResponse> {
	let resolved = params.resolve_scope()?;
	let (resource_scope, _) = resolved_to_resource_scope(&resolved);
	check_skills_mutable(&agent, resource_scope)?;
	require_writable_scope(&resolved)?;
	let mut manager = build_manager_from_resolved(&agent, &resolved)?;
	match manager.load() {
		Ok(_) => {}
		Err(ConfigError::NotFound { .. }) => {
			return Ok(Json(DeleteSkillByPathResponse {
				success: true,
				dry_run: !params.confirm.unwrap_or(false),
				executed: false,
				..Default::default()
			}));
		}
		Err(e) => return Err(ApiError::from(e)),
	}
	if let Some(skill) = manager.get_skill(name) {
		ensure_skill_not_plugin_managed(skill, "delete").await?;
	}
	let confirm = params.confirm.unwrap_or(false);
	let dry_run = !confirm;
	match manager.remove_skill_planned(
		name,
		params.all_agents.unwrap_or(false),
		dry_run,
		confirm,
	) {
		// `remove_skill_planned` already prunes the lock and records the status
		// in `outcome.prune`; no route-level re-prune.
		Ok(outcome) => Ok(Json(delete_response_from_outcome(outcome))),
		Err(ConfigError::ResourceNotFound { .. }) => {
			Ok(Json(DeleteSkillByPathResponse {
				success: true,
				dry_run,
				executed: false,
				..Default::default()
			}))
		}
		Err(e) => Err(ApiError::from(e)),
	}
}

#[post("/agents/<agent>/skills/<name>/enable?<scope..>")]
pub async fn enable_skill(
	_origin: TrustedLocalOrigin,
	agent: AgentParam,
	name: &str,
	scope: ScopeParams,
) -> ApiResult<SkillResponse> {
	let resolved = scope.resolve()?;
	let (resource_scope, _) = resolved_to_resource_scope(&resolved);
	check_skills_supported(&agent, resource_scope)?;
	require_writable_scope(&resolved)?;
	let mut manager = build_manager_from_resolved(&agent, &resolved)?;
	manager.load().map_err(ApiError::from)?;
	if let Some(skill) = manager.get_skill(name) {
		ensure_skill_not_plugin_managed(skill, "enable").await?;
	}
	manager.enable_skill(name).map_err(ApiError::from)?;
	let skill = manager.get_skill(name).expect("skill present after enable");
	Ok(Json(SkillResponse::from(skill)))
}

#[post("/agents/<agent>/skills/<name>/disable?<scope..>")]
pub async fn disable_skill(
	_origin: TrustedLocalOrigin,
	agent: AgentParam,
	name: &str,
	scope: ScopeParams,
) -> ApiResult<SkillResponse> {
	let resolved = scope.resolve()?;
	let (resource_scope, _) = resolved_to_resource_scope(&resolved);
	check_skills_supported(&agent, resource_scope)?;
	require_writable_scope(&resolved)?;
	let mut manager = build_manager_from_resolved(&agent, &resolved)?;
	manager.load().map_err(ApiError::from)?;
	if let Some(skill) = manager.get_skill(name) {
		ensure_skill_not_plugin_managed(skill, "disable").await?;
	}
	manager.disable_skill(name).map_err(ApiError::from)?;
	let skill = manager
		.get_skill(name)
		.expect("skill present after disable");
	Ok(Json(SkillResponse::from(skill)))
}

/// Reject mutations on skills owned by a Claude plugin.
async fn ensure_skill_not_plugin_managed(
	skill: &Skill,
	action: &str,
) -> Result<(), ApiError> {
	if let Some(plugin_name) = detect_plugin_for_path_if_present(skill).await {
		return Err(ApiError::new(
			Status::BadRequest,
			format!(
				"Cannot {action} skill '{}' managed by plugin '{plugin_name}'",
				skill.name
			),
			"MANAGED_RESOURCE",
		));
	}
	Ok(())
}

async fn detect_plugin_for_path_if_present(skill: &Skill) -> Option<String> {
	let source_path = skill
		.canonical_path
		.as_deref()
		.or(skill.source_path.as_deref())?;
	let full_path = expand_tilde_path(source_path);
	detect_plugin_for_path(&full_path).await
}

fn is_plugin_managed_skill(
	skill: &Skill,
	plugins: &[aghub_cc_plugins::claude::ClaudePluginInfo],
) -> bool {
	let source_path = skill
		.canonical_path
		.as_deref()
		.or(skill.source_path.as_deref());
	let Some(path) = source_path else {
		return false;
	};
	let full_path = expand_tilde_path(path);
	plugins.iter().any(|plugin| plugin.owns_path(&full_path))
}

#[get("/agents/all/skills?<params..>")]
pub(crate) async fn list_all_agents_skills(
	_origin: TrustedLocalOrigin,
	params: SkillListParams,
) -> ApiResult<Vec<SkillResponse>> {
	let include_managed = params.include_managed();
	let resolved = params.resolve_scope()?;
	let (resource_scope, project_root) = resolved_to_resource_scope(&resolved);
	let detected_plugins = ClaudePluginManager::new()
		.await
		.map(|manager| manager.list_plugins().to_vec())
		.unwrap_or_default();
	let items = load_all_agents(resource_scope, project_root.as_deref())
		.into_iter()
		.flat_map(|ar| {
			let agent_id = ar.agent_id;
			let plugins = &detected_plugins;
			ar.skills.into_iter().filter_map(move |skill| {
				if !include_managed && is_plugin_managed_skill(&skill, plugins)
				{
					return None;
				}
				Some(SkillResponse::from_agent_skill(skill, agent_id))
			})
		})
		.collect();
	Ok(Json(items))
}

/// Errors from the resolve/list/select/fetch stage of skill install.
enum InstallFetchError {
	Repo(skill_update::SkillRepoError),
	SkillsNotFound { missing: String, available: String },
	NoSkillsFound,
	InvalidPath,
}

/// Map shared-policy selections to `(catalog_name, SkillPath)` pairs.
fn select_catalog_paths(
	catalog: &[skill_update::CatalogSkill],
	requested: &[String],
	install_all: bool,
) -> Result<Vec<(String, skill::SkillPath)>, InstallFetchError> {
	let selected =
		skill::select_repo_skills(catalog, requested, install_all, |skill| {
			skill.name.as_str()
		})
		.map_err(|error| match error {
			skill::RepoSkillSelectionError::NoSkillsFound => {
				InstallFetchError::NoSkillsFound
			}
			skill::RepoSkillSelectionError::SkillsNotFound {
				missing,
				available,
			} => InstallFetchError::SkillsNotFound { missing, available },
		})?;

	selected
		.into_iter()
		.map(|skill| {
			let path = skill::SkillPath::parse(&skill.skill_path)
				.map_err(|_| InstallFetchError::InvalidPath)?;
			Ok((skill.name.clone(), path))
		})
		.collect()
}

fn map_install_fetch_error(e: InstallFetchError) -> ApiError {
	match e {
		InstallFetchError::Repo(err) => map_skill_repo_error(err),
		InstallFetchError::SkillsNotFound { missing, available } => {
			ApiError::new(
				Status::NotFound,
				format!(
					"Requested skills not found: {missing}. Available skills: {available}"
				),
				"SKILLS_NOT_FOUND",
			)
		}
		InstallFetchError::NoSkillsFound => ApiError::new(
			Status::NotFound,
			"No skills found in source repository".to_string(),
			"SKILLS_NOT_FOUND",
		),
		InstallFetchError::InvalidPath => ApiError::new(
			Status::BadRequest,
			"skill_path must be a relative path inside the cloned repository",
			"SKILL_PATH_INVALID",
		),
	}
}

#[post("/skills/install", data = "<body>")]
pub async fn install_skill(
	_origin: TrustedLocalOrigin,
	body: Json<InstallSkillRequest>,
	forwarded: ForwardedGitTokens,
	repositories: &rocket::State<crate::state::SkillRepositoryFactory>,
) -> ApiResult<InstallSkillResponse> {
	// Build SkillRepository off the async worker: ReqwestTransport creates a
	// blocking reqwest client (nested runtime) that panics when constructed
	// inside a current_thread executor (the unit-test `block_on` helper).
	let repositories = repositories.inner().clone();
	let repo = tokio::task::spawn_blocking(move || repositories.create())
		.await
		.map_err(|e| {
			ApiError::from_join_error(e, "Clone task failed", "CLONE_ERROR")
		})?;
	install_skill_route_with_repo(body.into_inner(), forwarded, repo).await
}

/// Production route core with an injectable repository. Keeping forwarded
/// credential resolution here lets the route test exercise the same seam as
/// Rocket's `POST /skills/install` handler.
pub(crate) async fn install_skill_route_with_repo(
	req: InstallSkillRequest,
	forwarded: ForwardedGitTokens,
	repo: std::sync::Arc<skill_update::SkillRepository>,
) -> ApiResult<InstallSkillResponse> {
	let resolver = SourceAuth::load(forwarded).await;
	let token = match resolver.resolve(&req.source) {
		skill_update::TokenResolution::Token(token) => Some(token),
		skill_update::TokenResolution::NoToken => None,
		skill_update::TokenResolution::BackendUnavailable => {
			return Err(crate::credentials::CredentialStoreError::Unavailable(
				"credential backend unreachable".to_string(),
			)
			.into());
		}
	};
	install_skill_with_repo(req, repo, token).await
}

const INVALID_FETCHED_SKILL_PATH: &str =
	"skill_path must be a relative path inside the fetched repository";

fn fetched_install_error_message(
	error: skill_update::mutation::InstallMutationError,
) -> String {
	match error {
		skill_update::mutation::InstallMutationError::InvalidSkillPath => {
			INVALID_FETCHED_SKILL_PATH.to_string()
		}
		skill_update::mutation::InstallMutationError::Install(error) => {
			ApiError::from(error).body.error
		}
	}
}

/// Compatibility adapter for the test-only `file://` full-clone fallback.
/// Production fetched Sources always go through
/// `skill_update::mutation::install_fetched_source`.
#[cfg(test)]
fn install_test_clone(
	root: &Path,
	ref_commit: Option<&str>,
	lock_skill_path: &str,
	source: &skill::InstallLockSource,
	scope: ResourceScope,
	project_root: Option<&Path>,
	target_agents: &[AgentType],
) -> Result<
	aghub_core::skills::install_fetched::FetchedSkillInstallReport,
	String,
> {
	let skill_file =
		aghub_core::skills::update::sanitize_skill_path(root, lock_skill_path)
			.ok_or_else(|| INVALID_FETCHED_SKILL_PATH.to_string())?;
	aghub_core::skills::install_fetched::install_fetched_skill_and_lock(
		aghub_core::skills::install_fetched::FetchedSkillInstallRequest {
			skill_file: &skill_file,
			source,
			lock_skill_path: lock_skill_path.to_string(),
			ref_commit: ref_commit.map(str::to_string),
			scope,
			project_root,
			target_agents,
			expected_name: None,
			target: if matches!(scope, ResourceScope::ProjectOnly) {
				aghub_core::skills::linker::LinkTarget::Relative
			} else {
				aghub_core::skills::linker::LinkTarget::Absolute
			},
		},
	)
	.map_err(|error| ApiError::from(error).body.error)
}

/// Core of `POST /skills/install` with an injectable [`SkillRepository`].
///
/// Production path: resolve one snapshot, list the catalog, map requested skill
/// NAMES → [`SkillPath`]s, then partial-fetch only those folders. The test-only
/// `file://` fallback still full-clones via gix.
pub(crate) async fn install_skill_with_repo(
	req: InstallSkillRequest,
	repo: std::sync::Arc<skill_update::SkillRepository>,
	token: Option<String>,
) -> ApiResult<InstallSkillResponse> {
	let resource_scope = parse_install_scope(&req.scope)?;

	let project_root = req
		.project_path
		.as_ref()
		.map(|r| crate::extractors::absolutize_root(r));
	if resource_scope == ResourceScope::ProjectOnly && project_root.is_none() {
		return Err(ApiError::new(
			Status::BadRequest,
			"project_path is required for project skill installs",
			"INVALID_PARAM",
		));
	}

	// Raw agent ids are part of the predictable target preflight. If any id is
	// unknown, attribute the rejection to every requested agent in request order
	// and stop before source materialization or install writes.
	let parsed_agents = req
		.agents
		.iter()
		.map(|agent_str| {
			(
				agent_str.clone(),
				agent_str
					.parse::<AgentType>()
					.map_err(|_| format!("Unknown agent '{agent_str}'")),
			)
		})
		.collect::<Vec<_>>();
	if parsed_agents.iter().any(|(_, agent)| agent.is_err()) {
		let agents = parsed_agents
			.into_iter()
			.map(|(agent, parsed)| GitInstallResultEntry {
				name: String::new(),
				agent,
				success: false,
				error: Some(parsed.err().unwrap_or_else(|| {
					"Another requested agent is invalid; nothing was written"
						.to_string()
				})),
			})
			.collect();
		return Ok(Json(InstallSkillResponse {
			success: false,
			agents,
		}));
	}
	let target_agents = parsed_agents
		.into_iter()
		.filter_map(|(agent_str, agent)| {
			agent.ok().map(|agent| (agent_str, agent))
		})
		.collect::<Vec<_>>();

	// Owns the materialized source through the install loop. The production
	// variant keeps root + immutable commit identity behind one deep interface.
	enum InstallMaterialization {
		Fetched(skill_update::mutation::FetchedSource),
		#[cfg(test)]
		Clone {
			temp_dir: tempfile::TempDir,
			ref_commit: Option<String>,
		},
	}

	// (catalog name, npx-form lock skill path)
	type InstallItem = (String, String);

	let (lock_source, items, materialization): (
		skill::InstallLockSource,
		Vec<InstallItem>,
		InstallMaterialization,
	) = match aghub_git::resolve_remote_source(&req.source) {
		Ok(resolved) => {
			let lock_source =
				install_lock_source_from_resolved(&resolved, None);
			let source_ref = skill_update::SourceRef {
				source: req.source.clone(),
				ref_: None,
			};
			let install_all = req.install_all.unwrap_or(false);
			let requested = req.skills.clone();
			let repo_for_task = repo.clone();
			let token_for_task = token;

			let (selected, fetched) = match timeout(
				Duration::from_secs(300),
				tokio::task::spawn_blocking(move || {
					let snapshot = repo_for_task
						.resolve(&source_ref, token_for_task.as_deref())
						.map_err(InstallFetchError::Repo)?;
					let catalog = repo_for_task
						.list(&snapshot)
						.map_err(InstallFetchError::Repo)?;
					let selected = select_catalog_paths(
						&catalog.skills,
						&requested,
						install_all,
					)?;
					let paths: Vec<skill::SkillPath> =
						selected.iter().map(|(_, p)| p.clone()).collect();
					let fetched = repo_for_task
						.fetch(
							&snapshot,
							skill_update::FetchSelection::Skills(&paths),
						)
						.map_err(InstallFetchError::Repo)?;
					Ok::<_, InstallFetchError>((selected, fetched))
				}),
			)
			.await
			{
				Ok(Ok(Ok(v))) => v,
				Ok(Ok(Err(e))) => return Err(map_install_fetch_error(e)),
				Ok(Err(e)) => {
					return Err(ApiError::from_join_error(
						e,
						"Clone task failed",
						"CLONE_ERROR",
					));
				}
				Err(_) => {
					return Err(ApiError::new(
						Status::RequestTimeout,
						"Skills installation timed out after 5 minutes"
							.to_string(),
						"SKILLS_INSTALL_TIMEOUT",
					));
				}
			};

			let items: Vec<InstallItem> = selected
				.into_iter()
				.map(|(name, skill_path)| {
					let lock_skill_path =
						skill::lock_skill_file_path(skill_path.as_str());
					(name, lock_skill_path)
				})
				.collect();

			(
				lock_source,
				items,
				InstallMaterialization::Fetched(
					skill_update::mutation::FetchedSource::from_repo(fetched),
				),
			)
		}
		Err(error) => {
			#[cfg(test)]
			{
				if let Some((clone_url, lock_source)) =
					file_install_source(&req.source)?
				{
					let clone_url_for_task = clone_url.clone();
					let temp_dir = match timeout(
						Duration::from_secs(300),
						tokio::task::spawn_blocking(move || {
							clone_skill_source_to_temp(
								&clone_url_for_task,
								true,
							)
						}),
					)
					.await
					{
						Ok(Ok(Ok(temp_dir))) => temp_dir,
						Ok(Ok(Err(e))) => {
							return Err(ApiError::new(
								Status::BadRequest,
								format!("Failed to clone skill source: {e}"),
								"CLONE_FAILED",
							));
						}
						Ok(Err(e)) => {
							return Err(ApiError::from_join_error(
								e,
								"Clone task failed",
								"CLONE_ERROR",
							));
						}
						Err(_) => {
							return Err(ApiError::new(
								Status::RequestTimeout,
								"Skills installation timed out after 5 minutes"
									.to_string(),
								"SKILLS_INSTALL_TIMEOUT",
							));
						}
					};

					let selected_skills = skill::discover_repo_skills(
						temp_dir.path(),
						&req.skills,
						req.install_all.unwrap_or(false),
					)
					.map_err(map_repo_discovery_error)?;

					let ref_commit = gix::open(temp_dir.path())
						.ok()
						.and_then(|r| r.head_id().ok().map(|id| id.detach()))
						.map(|oid| oid.to_string());

					let items: Vec<InstallItem> = selected_skills
						.into_iter()
						.map(|s| {
							let lock_skill_path =
								skill::lock_skill_file_path(&s.relative_dir);
							(s.name, lock_skill_path)
						})
						.collect();

					(
						lock_source,
						items,
						InstallMaterialization::Clone {
							temp_dir,
							ref_commit,
						},
					)
				} else {
					return Err(map_remote_source_error(error));
				}
			}
			#[cfg(not(test))]
			return Err(map_remote_source_error(error));
		}
	};

	let agent_types: Vec<AgentType> =
		target_agents.iter().map(|(_, a)| *a).collect();

	let mut agent_rows: Vec<GitInstallResultEntry> = Vec::new();
	let mut any_installed = false;
	for (name, lock_skill_path) in &items {
		let installed = match &materialization {
			InstallMaterialization::Fetched(fetched) => {
				skill_update::mutation::install_fetched_source(
					fetched,
					skill_update::mutation::FetchedInstallRequest {
						source: &lock_source,
						lock_skill_path,
						expected_name: None,
						scope: resource_scope,
						project_root: project_root.as_deref(),
						target_agents: &agent_types,
					},
				)
				.map_err(fetched_install_error_message)
			}
			#[cfg(test)]
			InstallMaterialization::Clone {
				temp_dir,
				ref_commit,
			} => install_test_clone(
				temp_dir.path(),
				ref_commit.as_deref(),
				lock_skill_path,
				&lock_source,
				resource_scope,
				project_root.as_deref(),
				&agent_types,
			),
		};
		match installed {
			Ok(report) => {
				for ((agent_str, _), agent_result) in
					target_agents.iter().zip(report.agent_results)
				{
					let success = agent_result.error.is_none();
					any_installed |= agent_result.installed;
					agent_rows.push(GitInstallResultEntry {
						name: if success {
							report.name.clone()
						} else {
							name.clone()
						},
						agent: agent_str.clone(),
						success,
						error: agent_result.error,
					});
				}
			}
			Err(message) => {
				for (agent_str, _) in &target_agents {
					agent_rows.push(GitInstallResultEntry {
						name: name.clone(),
						agent: agent_str.clone(),
						success: false,
						error: Some(message.clone()),
					});
				}
			}
		}
	}

	let success = any_installed && agent_rows.iter().all(|r| r.success);
	Ok(Json(InstallSkillResponse {
		success,
		agents: agent_rows,
	}))
}

#[post("/skills/open", format = "json", data = "<request>")]
pub async fn open_skill_folder(
	_origin: TrustedLocalOrigin,
	request: Json<OpenSkillFolderRequest>,
) -> Result<(), String> {
	let req = request.into_inner();
	let path = expand_tilde_path(&req.skill_path);
	let folder = get_parent_folder(path);

	match open::that(&folder) {
		Ok(_) => Ok(()),
		Err(e) => Err(format!("Failed to open folder: {e}")),
	}
}

#[post("/skills/edit", format = "json", data = "<request>")]
pub async fn edit_skill_folder(
	_origin: TrustedLocalOrigin,
	request: Json<EditSkillFolderRequest>,
) -> Result<(), String> {
	let req = request.into_inner();
	let path = expand_tilde_path(&req.skill_path);
	let folder = get_parent_folder(path);

	match detect_available_editor() {
		Some(editor) => {
			let mut cmd = std::process::Command::new(editor.cli_command());
			cmd.arg(&folder);
			#[cfg(windows)]
			{
				use std::os::windows::process::CommandExt;
				cmd.creation_flags(crate::CREATE_NO_WINDOW);
			}
			match cmd.spawn() {
				Ok(_) => Ok(()),
				Err(e) => Err(format!("Failed to open editor: {e}")),
			}
		}
		None => {
			let editor_names: Vec<&str> = CodeEditorType::all()
				.iter()
				.map(|e| e.display_name())
				.collect();
			Err(format!(
				"No supported code editor found. Please install {}.",
				editor_names.join(", ")
			))
		}
	}
}

/// Resolve the allow-listed skills roots for a (scope, project_root) pair.
fn skill_read_roots(
	resource_scope: ResourceScope,
	project_root: Option<&Path>,
) -> Vec<PathBuf> {
	let agent_dirs = aghub_core::skills::removal::agent_skill_dirs_in_scope(
		resource_scope,
		project_root,
	);
	aghub_core::skills::removal::allowed_skill_roots(&agent_dirs, project_root)
}

/// Resolve the allow-listed skills roots for a (scope, project_root) pair and
/// assert `path` canonicalizes to inside one of them. Mirrors the containment
/// guard used by `delete_skill_by_path`, so content/tree reads cannot escape
/// the skills tree (incl. via `..` or a symlink whose target is out of tree).
///
/// A path that does NOT exist yields `Status::NotFound` (with `not_found_code`)
/// rather than Forbidden, so a missing/just-deleted skill reads as 404 — only a
/// path that EXISTS yet resolves outside the roots is a 403. Mirrors how
/// `removal::assert_targets_strictly_contained` distinguishes not-found targets.
fn assert_skill_read_allowed(
	path: &Path,
	resource_scope: ResourceScope,
	project_root: Option<&Path>,
	not_found_code: &'static str,
) -> Result<PathBuf, ApiError> {
	let roots = skill_read_roots(resource_scope, project_root);
	if let Some(canonical) =
		aghub_core::skills::removal::assert_contained(path, &roots)
	{
		return Ok(canonical);
	}
	// `assert_contained` canonicalizes and returns None on ENOENT. Distinguish
	// "does not exist" (→ 404) from "exists but escapes the roots" (→ 403).
	if !path.exists() {
		return Err(ApiError::new(
			Status::NotFound,
			"Skill path not found",
			not_found_code,
		));
	}
	Err(ApiError::new(
		Status::Forbidden,
		"Refusing to read: resolved path is outside the \
		 allow-listed skills roots",
		"SKILL_PATH_OUTSIDE_ROOT",
	))
}

#[get("/skills/content?<query..>")]
pub fn get_skill_content(
	_origin: TrustedLocalOrigin,
	query: SkillContentQuery,
) -> ApiResult<String> {
	let resolved = ScopeParams {
		scope: query.scope.clone(),
		project_root: query.project_root.clone(),
	}
	.resolve()?;
	let (resource_scope, project_root) = resolved_to_resource_scope(&resolved);

	let path = expand_tilde_path(&query.path);
	let safe_path = assert_skill_read_allowed(
		&path,
		resource_scope,
		project_root.as_deref(),
		"SKILL_FILE_NOT_FOUND",
	)?;

	let content = std::fs::read_to_string(&safe_path).map_err(|e| {
		ApiError::new(
			Status::NotFound,
			format!("Failed to read skill file: {e}"),
			"SKILL_FILE_NOT_FOUND",
		)
	})?;

	let skill = skill::parser::parse_skill_md(&content).map_err(|e| {
		ApiError::new(
			Status::BadRequest,
			format!("Invalid skill format: {e}"),
			"INVALID_SKILL_FORMAT",
		)
	})?;

	Ok(Json(skill.content))
}

#[get("/skills/tree?<query..>")]
pub fn get_skill_tree(
	_origin: TrustedLocalOrigin,
	query: SkillTreeQuery,
) -> ApiResult<SkillTreeNodeResponse> {
	let resolved = ScopeParams {
		scope: query.scope.clone(),
		project_root: query.project_root.clone(),
	}
	.resolve()?;
	let (resource_scope, project_root) = resolved_to_resource_scope(&resolved);

	let path = expand_tilde_path(&query.path);
	let root = get_skill_root(path);
	let safe_root = assert_skill_read_allowed(
		&root,
		resource_scope,
		project_root.as_deref(),
		"SKILL_PATH_NOT_FOUND",
	)?;
	// Thread the allow-listed roots down so symlink ENTRIES (e.g. the
	// universal-install `<agent>/skills/foo -> .agents/skills/foo`) are
	// included when their canonical target stays inside the roots, and silently
	// skipped (not 400'd) when they escape.
	let roots = skill_read_roots(resource_scope, project_root.as_deref());
	let tree = build_skill_tree_node(&safe_root, &roots)?;
	Ok(Json(tree))
}

#[get("/skills/lock/global")]
pub fn get_global_skill_lock(
	_origin: TrustedLocalOrigin,
) -> ApiResult<GlobalSkillLockResponse> {
	let lock = skill::lock::global::read_skill_lock();
	let skills: Vec<SkillLockEntryResponse> = lock
		.skills
		.into_iter()
		.map(|(name, entry)| SkillLockEntryResponse {
			name,
			source: entry.source,
			source_type: entry.source_type,
			source_url: entry.source_url,
			skill_path: entry.skill_path,
			skill_folder_hash: entry.skill_folder_hash,
			content_hash: entry.content_hash,
			installed_at: entry.installed_at,
			updated_at: entry.updated_at,
			plugin_name: entry.plugin_name,
		})
		.collect();

	Ok(Json(GlobalSkillLockResponse {
		version: lock.version,
		skills,
		last_selected_agents: lock.last_selected_agents,
	}))
}

#[get("/skills/lock/project?<query..>")]
pub fn get_project_skill_lock(
	_origin: TrustedLocalOrigin,
	query: ProjectLockQuery,
) -> ApiResult<ProjectSkillLockResponse> {
	let cwd = query.project_path.as_deref().map(std::path::Path::new);
	let lock = skill::lock::local::read_local_lock(cwd);
	let skills: Vec<LocalSkillLockEntryResponse> = lock
		.skills
		.into_iter()
		.map(|(name, entry)| LocalSkillLockEntryResponse {
			name,
			source: entry.source,
			source_type: entry.source_type,
			computed_hash: entry.computed_hash,
		})
		.collect();

	Ok(Json(ProjectSkillLockResponse {
		version: lock.version,
		skills,
	}))
}

#[cfg(test)]
fn require_github_credential_url(url: &str) -> Result<(), ApiError> {
	SourceAuth::require_github_credential_url_for_test(url)
}

#[cfg(test)]
fn same_origin(a: &str, b: &str) -> bool {
	SourceAuth::same_origin_for_test(a, b)
}

fn current_platform() -> &'static str {
	if cfg!(target_os = "windows") {
		"windows"
	} else if cfg!(target_os = "macos") {
		"macos"
	} else if cfg!(target_os = "linux") {
		"linux"
	} else {
		"other"
	}
}

/// Non-interactive pre-flight: can the machine running aghub-api resolve a Git
/// credential for this URL via the system credential helpers? Mirrors the clone
/// path (same `.git` normalization + `useHttpPath`) so its verdict predicts
/// whether an unattended scan will authenticate.
#[get("/skills/git/credential-status?<query..>")]
pub async fn git_credential_status(
	_origin: TrustedLocalOrigin,
	query: GitCredentialStatusQuery,
) -> ApiResult<GitCredentialStatusResponse> {
	let url = aghub_git::normalize_tfs_clone_url(&query.url);

	// Control characters could inject into the line-based credential protocol;
	// embedded userinfo would leak a secret through the request URL. Reject both.
	if url.contains(|c: char| c.is_control()) {
		return Err(ApiError::new(
			Status::BadRequest,
			"URL contains control characters",
			"INVALID_URL",
		));
	}
	let parsed = url::Url::parse(&url).ok();
	if parsed
		.as_ref()
		.is_some_and(|u| !u.username().is_empty() || u.password().is_some())
	{
		return Err(ApiError::new(
			Status::BadRequest,
			"URL must not embed credentials",
			"URL_HAS_CREDENTIALS",
		));
	}
	let host = parsed.and_then(|u| u.host_str().map(str::to_string));

	let probe_url = url.clone();
	let status = tokio::task::spawn_blocking(move || {
		if !aghub_git::system_git_available() {
			GitCredentialStatus::GitUnavailable
		} else if aghub_git::probe_credential(&probe_url) {
			GitCredentialStatus::Available
		} else {
			GitCredentialStatus::Missing
		}
	})
	.await
	.map_err(|e| {
		ApiError::from_join_error(
			e,
			"Credential probe task failed",
			"CREDENTIAL_PROBE_ERROR",
		)
	})?;

	Ok(Json(GitCredentialStatusResponse {
		status,
		platform: current_platform().to_string(),
		host,
	}))
}

#[post("/skills/git/scan", data = "<body>")]
pub async fn git_scan_skills(
	_origin: TrustedLocalOrigin,
	body: Json<GitScanRequest>,
	sessions: &rocket::State<PinnedSourceSessions>,
	forwarded: ForwardedGitTokens,
) -> ApiResult<GitScanResponse> {
	let mut req = body.into_inner();
	// Azure DevOps Server / TFS rejects the trailing `.git` on `/_git/<repo>`
	// URLs (TF401019). Normalize once here so every downstream use — credential
	// resolution, clone, branch listing, session identity — uses the accepted
	// URL form.
	req.url = SourceAuth::normalize_scan_source(&req.url);

	let existing_session = req
		.session_id
		.as_deref()
		.and_then(|session_id| sessions.active(session_id));
	let prior_session = existing_session
		.as_ref()
		.map(|session| (session.url(), session.credential_token()));
	let credential_token = SourceAuth::resolve_for_scan(
		&forwarded,
		&req.url,
		req.credential_id.as_deref(),
		prior_session,
	)
	.await?;

	// Retrieve cached branches from existing session if re-scanning
	let cached_branches: Option<Vec<String>> = existing_session
		.as_ref()
		.map(|session| session.branches().to_vec());

	// Skill-aware catalog scan: resolve + list only (no whole-repo clone).
	// The same `SkillRepository` instance is retained on the session so a later
	// install/sync `fetch` reuses the backend memo for this commit.
	let repo = std::sync::Arc::new(skill_update::SkillRepository::new());
	let source_ref = skill_update::SourceRef {
		source: req.url.clone(),
		ref_: req.branch.clone(),
	};
	let token_for_scan = credential_token.clone();
	let repo_for_scan = repo.clone();
	let (snapshot, skills) = tokio::task::spawn_blocking(move || {
		scan_repo_catalog(
			&repo_for_scan,
			&source_ref,
			token_for_scan.as_deref(),
		)
	})
	.await
	.map_err(|e| {
		ApiError::from_join_error(e, "Scan task failed", "SCAN_ERROR")
	})?
	.map_err(map_skill_repo_error)?;

	// List remote branches (use cache from previous session if
	// available to avoid an extra network call on branch switch)
	let branch_url = req.url.clone();
	let credential_token_for_branches = credential_token.clone();
	let branches = list_branches_for_scan(cached_branches, move || {
		match credential_token_for_branches {
			Some(token) => aghub_git::list_remote_branches(
				aghub_git::RemoteOptions::new(&branch_url)
					.with_credentials("x-access-token", token),
			),
			// No token: try gix unauthenticated, then fall back to the system
			// `git` binary so the OS credential helper authenticates — matching
			// the scan path so branch listing succeeds for the same repos.
			None => match aghub_git::list_remote_branches(
				aghub_git::RemoteOptions::new(&branch_url),
			) {
				Ok(branches) => Ok(branches),
				Err(_) if aghub_git::system_git_available() => {
					aghub_git::list_remote_branches_system_git(&branch_url)
				}
				Err(e) => Err(e),
			},
		}
	})
	.await?;

	// No local clone HEAD: prefer the request branch, else guess main/master.
	let current_branch = req.branch.clone().unwrap_or_else(|| {
		["main", "master"]
			.iter()
			.find(|b| branches.contains(&b.to_string()))
			.map(|b| b.to_string())
			.unwrap_or_default()
	});

	// Store the commit-pinned repository handle until install/sync.
	let session_id = uuid::Uuid::new_v4().to_string();
	// The session module owns the 10-minute lifetime and eviction policy. The
	// browse-then-install window stays tight so credentials and any internal gix
	// shallow-clone cache are not retained longer than needed.
	let session = PinnedSourceSession::new(
		repo,
		snapshot,
		req.url,
		credential_token,
		branches.clone(),
		current_branch.clone(),
	);
	if let Some(old_session_id) = req.session_id.as_deref() {
		sessions.replace(old_session_id, session_id.clone(), session);
	} else {
		sessions.insert(session_id.clone(), session);
	}

	Ok(Json(GitScanResponse {
		session_id,
		skills,
		branches,
		current_branch,
	}))
}

/// Scan core: resolve a source tip and list skill catalog entries without
/// materializing the whole repository (resolve + list only; no fetch).
pub(crate) fn scan_repo_catalog(
	repo: &skill_update::SkillRepository,
	source_ref: &skill_update::SourceRef,
	token: Option<&str>,
) -> Result<
	(aghub_git::RepoSnapshot, Vec<GitScanSkillEntry>),
	skill_update::SkillRepoError,
> {
	let snapshot = repo.resolve(source_ref, token)?;
	let catalog = repo.list(&snapshot)?;
	let skills = catalog
		.skills
		.into_iter()
		.map(|c| GitScanSkillEntry {
			name: c.name,
			description: c.description.unwrap_or_default(),
			author: c.author,
			version: c.version,
			path: c.skill_path, // repo-relative FOLDER ("" for a root skill)
		})
		.collect();
	Ok((snapshot, skills))
}

fn map_skill_repo_error(e: skill_update::SkillRepoError) -> ApiError {
	use skill_update::SkillRepoError;
	match e {
		SkillRepoError::Auth => ApiError::new(
			Status::BadRequest,
			"Failed to access repository: authentication required",
			"CLONE_FAILED",
		),
		SkillRepoError::Network => ApiError::new(
			Status::BadRequest,
			"Failed to access repository",
			"CLONE_FAILED",
		),
		SkillRepoError::RootSkillTooLarge => ApiError::new(
			Status::BadRequest,
			"Root skill exceeds size bounds",
			"ROOT_SKILL_TOO_LARGE",
		),
	}
}

fn map_pinned_source_fetch_error(
	error: PinnedSourceFetchError,
	timeout_message: &str,
) -> ApiError {
	match error {
		PinnedSourceFetchError::Repository(error) => {
			map_skill_repo_error(error)
		}
		PinnedSourceFetchError::Task(error) => {
			ApiError::from_join_error(error, "Fetch task failed", "CLONE_ERROR")
		}
		PinnedSourceFetchError::Timeout => ApiError::new(
			Status::RequestTimeout,
			timeout_message.to_string(),
			"SKILLS_INSTALL_TIMEOUT",
		),
	}
}

/// Compatibility adapter for the existing strict scan-policy tests.
#[cfg(test)]
fn forwarded_token_for_url(
	forwarded: &ForwardedGitTokens,
	url: &str,
) -> Option<String> {
	SourceAuth::forwarded_for_scan(forwarded, url)
}

/// Partition `agents` (raw strings from the request) into valid/invalid
/// entries in request order. Invalid entries carry the error message to
/// surface back to the caller.
///
/// Valid means the id parses. Scope support is intentionally NOT filtered
/// here: the deep install seam must see every known requested target so its
/// all-target preflight can reject a mixed list before writing the Master.
/// Invalid means an unknown raw agent id.
#[allow(clippy::type_complexity)]
fn partition_install_agents_in_request_order(
	agents: &[String],
	_scope: ResourceScope,
	_project_root: Option<&std::path::Path>,
) -> (Vec<(String, AgentType)>, Vec<(String, String)>) {
	let mut valid: Vec<(String, AgentType)> = Vec::new();
	let mut invalid: Vec<(String, String)> = Vec::new();
	for agent_str in agents {
		match agent_str.parse::<AgentType>() {
			Ok(agent_type) => valid.push((agent_str.clone(), agent_type)),
			Err(_) => {
				invalid.push((
					agent_str.clone(),
					format!("Unknown agent '{agent_str}'"),
				));
			}
		}
	}
	(valid, invalid)
}

#[post("/skills/git/install", data = "<body>")]
pub async fn git_install_skills(
	_origin: TrustedLocalOrigin,
	body: Json<GitInstallRequest>,
	sessions: &rocket::State<PinnedSourceSessions>,
) -> ApiResult<GitInstallResponse> {
	let req = body.into_inner();

	let session = sessions.claim(&req.session_id).ok_or_else(|| {
		ApiError::new(
			Status::NotFound,
			"Session not found or expired",
			"SESSION_NOT_FOUND",
		)
	})?;
	let ref_name = (!session.current_branch().is_empty())
		.then(|| session.current_branch().to_string());
	let resolved = aghub_git::resolve_remote_source(session.url())
		.map_err(map_remote_source_error)?;
	let source = install_lock_source_from_resolved(&resolved, ref_name);

	let resource_scope = parse_install_scope(&req.scope)?;

	let project_root: Option<std::path::PathBuf> = req
		.project_root
		.as_ref()
		.map(|r| crate::extractors::absolutize_root(r));

	// Reject absolute / `..` paths BEFORE any fetch or install write.
	// Security: out-of-tree paths must fail with 400 without I/O.
	let validated_paths: Vec<skill::SkillPath> = req
		.skill_paths
		.iter()
		.map(|p| skill::SkillPath::parse(p))
		.collect::<Result<_, _>>()
		.map_err(|_| {
			ApiError::new(
				Status::BadRequest,
				"skill_path must be a relative path inside the cloned repository",
				"SKILL_PATH_INVALID",
			)
		})?;

	// Fetch once for all selected skill folders (partial materialization).
	let fetched =
		session
			.fetch_skills(&validated_paths)
			.await
			.map_err(|error| {
				map_pinned_source_fetch_error(
					error,
					"Skills installation timed out after 5 minutes",
				)
			})?;
	let fetched = skill_update::mutation::FetchedSource::from_repo(fetched);

	let mut results = Vec::new();

	let (valid_agents, invalid_agents) =
		partition_install_agents_in_request_order(
			&req.agents,
			resource_scope,
			project_root.as_deref(),
		);

	if !invalid_agents.is_empty() {
		for skill_path in &req.skill_paths {
			for agent_str in &req.agents {
				let error = invalid_agents
					.iter()
					.find(|(invalid, _)| invalid == agent_str)
					.map(|(_, error)| error.clone())
					.unwrap_or_else(|| {
						"Another requested agent is invalid; nothing was written"
							.to_string()
					});
				results.push(GitInstallResultEntry {
					name: skill_path.clone(),
					agent: agent_str.clone(),
					success: false,
					error: Some(error),
				});
			}
		}
		// This remains a successfully handled request, so retain the route's
		// exclusive pinned-session consumption semantics.
		session.consume();
		return Ok(Json(GitInstallResponse { results }));
	}

	let target_agents: Vec<AgentType> =
		valid_agents.iter().map(|(_, agent)| *agent).collect();

	for (skill_path, validated) in req.skill_paths.iter().zip(&validated_paths)
	{
		let lock_skill_path = skill::lock_skill_file_path(validated.as_str());
		match skill_update::mutation::install_fetched_source(
			&fetched,
			skill_update::mutation::FetchedInstallRequest {
				source: &source,
				lock_skill_path: &lock_skill_path,
				expected_name: None,
				scope: resource_scope,
				project_root: project_root.as_deref(),
				target_agents: &target_agents,
			},
		) {
			Ok(report) => {
				for ((agent_str, _), agent_result) in
					valid_agents.iter().zip(report.agent_results)
				{
					let success = agent_result.error.is_none();
					results.push(GitInstallResultEntry {
						// Successful rows carry the parsed skill name (as the old
						// route did); failures keep the requested `skill_path`.
						name: if success {
							report.name.clone()
						} else {
							skill_path.clone()
						},
						agent: agent_str.clone(),
						success,
						error: agent_result.error,
					});
				}
			}
			// A per-skill failure (e.g. parse error) is reported as per-agent
			// failure rows and never aborts the whole request — matching the old
			// route, where `install_git_skill_*` errors became failure entries.
			Err(error) => {
				let message = fetched_install_error_message(error);
				for (agent_str, _) in &valid_agents {
					results.push(GitInstallResultEntry {
						name: skill_path.clone(),
						agent: agent_str.clone(),
						success: false,
						error: Some(message.clone()),
					});
				}
			}
		}
	}

	// Successful request permanently consumes the exclusive session claim.
	session.consume();

	Ok(Json(GitInstallResponse { results }))
}

/// Replace existing skill installations in-place from a previously-scanned
/// git session. Targets are derived from the installed skill name on the server;
/// client-provided paths are accepted only for backward-compatible requests.
#[post("/skills/git/sync", data = "<body>")]
pub async fn git_sync_skill(
	_origin: TrustedLocalOrigin,
	body: Json<GitSyncRequest>,
	sessions: &rocket::State<PinnedSourceSessions>,
) -> ApiResult<GitSyncResponse> {
	let req = body.into_inner();

	let session = sessions.claim(&req.session_id).ok_or_else(|| {
		ApiError::new(
			Status::NotFound,
			"Session not found or expired",
			"SESSION_NOT_FOUND",
		)
	})?;

	// Lock path (`"<dir>/SKILL.md"` or `"SKILL.md"`) → skill-folder SkillPath.
	let folder = skill_update::skill_folder_from_lock_path(&req.skill_path)
		.ok_or_else(|| {
			ApiError::new(
				Status::BadRequest,
				"skill_path must be a relative path inside the cloned repository",
				"SKILL_PATH_INVALID",
			)
		})?;

	// Fetch only the selected skill folder.
	let fetched = session
		.fetch_skills(std::slice::from_ref(&folder))
		.await
		.map_err(|error| {
			map_pinned_source_fetch_error(
				error,
				"Skills sync timed out after 5 minutes",
			)
		})?;
	let fetched = skill_update::mutation::FetchedSource::from_repo(fetched);
	// Preserve the route's historical precedence: a missing fetched skill is
	// reported before request scope/lock validation. The mutation seam repeats
	// this containment check at the write boundary.
	if !skill_update::mutation::fetched_skill_path_exists(
		&fetched,
		&req.skill_path,
	) {
		return Err(ApiError::new(
			Status::NotFound,
			format!(
				"Skill path '{}' not found in cloned repository",
				req.skill_path
			),
			"SKILL_PATH_NOT_FOUND",
		));
	}

	let project_root = req.project_root.as_deref().map(PathBuf::from);
	let resource_scope = match req.scope.as_str() {
		"global" => ResourceScope::GlobalOnly,
		"project" if project_root.is_some() => ResourceScope::ProjectOnly,
		"project" => {
			return Err(ApiError::new(
				Status::BadRequest,
				"project_root is required when scope is project",
				"MISSING_PARAM",
			));
		}
		_ => {
			return Err(ApiError::new(
				Status::BadRequest,
				"scope must be global or project",
				"INVALID_SCOPE",
			));
		}
	};

	let locked = match resource_scope {
		ResourceScope::GlobalOnly => {
			skill::lock::global::get_skill_from_lock(&req.name).is_some()
		}
		ResourceScope::ProjectOnly => {
			let root = project_root
				.as_deref()
				.expect("project root validated for project scope");
			skill::lock::local::read_local_lock(Some(root))
				.skills
				.contains_key(&req.name)
		}
		ResourceScope::Both => false,
	};
	if !locked {
		return Err(ApiError::new(
			Status::NotFound,
			format!("Skill '{}' is not present in the lock", req.name),
			"SKILL_LOCK_ENTRY_NOT_FOUND",
		));
	}

	// The post-session transaction (rename guard → containment → swap → lock) is
	// the shared core resync; the route owns only the session lifecycle.
	use crate::skills::resync::safe_resync_error;
	use aghub_core::skills::resync::ResyncError;
	use skill_update::mutation::{
		resync_fetched_source, FetchedResyncRequest, ResyncMutationError,
	};
	let report = resync_fetched_source(
		&fetched,
		FetchedResyncRequest {
			skill_path: &req.skill_path,
			name: &req.name,
			scope: resource_scope,
			project_root: project_root.as_deref(),
			// No pre-fetch identity to compare: this route syncs from a session
			// whose source the USER picked, it never read the entry's coordinates.
			// Comparing the session URL would prove nothing and would reject every
			// npx-written entry (no `sourceUrl`). The mutation guard still covers
			// the swap + lock write.
			expected_source: None,
		},
	)
	.map_err(|e| match e {
		ResyncMutationError::InvalidSkillPath => ApiError::new(
			Status::NotFound,
			format!(
				"Skill path '{}' not found in cloned repository",
				req.skill_path
			),
			"SKILL_PATH_NOT_FOUND",
		),
		ResyncMutationError::Resync(ResyncError::NotInstalled) => {
			ApiError::new(
				Status::NotFound,
				format!(
					"Skill '{}' is locked but no installed copy was found",
					req.name
				),
				"SKILL_NOT_INSTALLED",
			)
		}
		ResyncMutationError::Resync(ResyncError::Renamed { new_name }) => {
			ApiError::new(
				Status::BadRequest,
				skill_renamed_message(&req.name, &new_name),
				SKILL_RENAMED_CODE,
			)
		}
		ResyncMutationError::Resync(error) => {
			let mapped = safe_resync_error(&error);
			ApiError::new(mapped.status, mapped.message, mapped.code)
		}
	})?;

	// Successful request permanently consumes the exclusive session claim.
	session.consume();

	Ok(Json(GitSyncResponse {
		success: true,
		name: Some(req.name.clone()),
		updated_hash: Some(report.updated_hash),
		error: None,
	}))
}

#[cfg(test)]
mod tests {
	use super::*;
	use aghub_core::transfer::{
		reconcile_skill, InstallScope, ResourceLocator,
	};
	use tempfile::tempdir;

	// ---- F2.5: delete (containment/dry-run/confirm/prune) + prune-lock ------

	const ORPHAN_LOCK_JSON: &str = r#"{"version":3,"skills":{"orphan":{"source":"o/r","sourceType":"github","sourceUrl":"https://github.com/o/r","skillFolderHash":"","installedAt":"t","updatedAt":"t"}}}"#;

	/// Run `f` with HOME + XDG_STATE_HOME pointed at fresh temp dirs (serialized
	/// via env_lock) so the global lock + agent skills dirs are fully isolated.
	fn with_isolated_env<T>(
		f: impl FnOnce(&std::path::Path, &std::path::Path) -> T,
	) -> T {
		let _g = crate::routes::test_env_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		let home = tempdir().unwrap();
		let state = tempdir().unwrap();
		let old_home = std::env::var("HOME").ok();
		let old_xdg = std::env::var("XDG_STATE_HOME").ok();
		std::env::set_var("HOME", home.path());
		std::env::set_var("XDG_STATE_HOME", state.path());
		let result = f(home.path(), state.path());
		match old_home {
			Some(v) => std::env::set_var("HOME", v),
			None => std::env::remove_var("HOME"),
		}
		match old_xdg {
			Some(v) => std::env::set_var("XDG_STATE_HOME", v),
			None => std::env::remove_var("XDG_STATE_HOME"),
		}
		result
	}

	// ── T08 session helpers (distinct names from t08_desktop_partial_fetch) ──

	/// Test-only gix-slot backend: `materialize` copies `base.join(path)` into
	/// the fetch dest. Mirrors skill-update's LocalDirBackend under unique names.
	struct SessionLocalBackend {
		base: std::path::PathBuf,
	}

	impl SessionLocalBackend {
		fn new(base: impl Into<std::path::PathBuf>) -> Self {
			Self { base: base.into() }
		}
	}

	fn session_copy_tree(src: &std::path::Path, dst: &std::path::Path) {
		std::fs::create_dir_all(dst).unwrap();
		for entry in std::fs::read_dir(src).unwrap() {
			let entry = entry.unwrap();
			let from = entry.path();
			let to = dst.join(entry.file_name());
			if from.is_dir() {
				session_copy_tree(&from, &to);
			} else {
				std::fs::copy(&from, &to).unwrap();
			}
		}
	}

	impl aghub_git::RepoFetchBackend for SessionLocalBackend {
		fn resolve(
			&self,
			_source: &aghub_git::SourceRef,
			_auth: Option<&aghub_git::Credentials>,
		) -> aghub_git::Result<aghub_git::RepoSnapshot> {
			Ok(aghub_git::RepoSnapshot {
				commit_oid: "9999999999999999999999999999999999999999".into(),
				tree_oid: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
				commit_time: None,
			})
		}
		fn read_tree(
			&self,
			_s: &aghub_git::RepoSnapshot,
		) -> aghub_git::Result<aghub_git::RepoTree> {
			Ok(aghub_git::RepoTree {
				entries: Vec::new(),
			})
		}
		fn read_blobs(
			&self,
			_s: &aghub_git::RepoSnapshot,
			_o: &[String],
		) -> aghub_git::Result<Vec<aghub_git::Blob>> {
			Ok(Vec::new())
		}
		fn materialize(
			&self,
			_s: &aghub_git::RepoSnapshot,
			paths: &[&str],
			dest: &std::path::Path,
		) -> aghub_git::Result<()> {
			for p in paths {
				if p.is_empty() {
					session_copy_tree(&self.base, dest);
				} else {
					session_copy_tree(&self.base.join(p), &dest.join(p));
				}
			}
			Ok(())
		}
	}

	/// Build a session whose later `fetch` materializes from `fixture_root`.
	/// Calls `resolve` so the repository's commit→backend memo is populated.
	fn session_from_fixture(
		fixture_root: &std::path::Path,
		url: &str,
		current_branch: &str,
	) -> PinnedSourceSession {
		let backend =
			std::sync::Arc::new(SessionLocalBackend::new(fixture_root));
		let repo = std::sync::Arc::new(
			skill_update::SkillRepository::with_backends(None, backend),
		);
		let ref_ = if current_branch.is_empty() {
			None
		} else {
			Some(current_branch.to_string())
		};
		let snapshot = repo
			.resolve(
				&skill_update::SourceRef {
					source: url.to_string(),
					ref_,
				},
				None,
			)
			.expect("resolve fixture session");
		PinnedSourceSession::new(
			repo,
			snapshot,
			url.to_string(),
			None,
			if current_branch.is_empty() {
				vec![]
			} else {
				vec![current_branch.to_string()]
			},
			current_branch.to_string(),
		)
	}

	/// Dummy session for guards / path-validation paths that never fetch.
	fn dummy_git_session(
		url: &str,
		credential_token: Option<String>,
	) -> PinnedSourceSession {
		PinnedSourceSession::new(
			std::sync::Arc::new(skill_update::SkillRepository::new()),
			aghub_git::RepoSnapshot {
				commit_oid: "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef".into(),
				tree_oid: "cafebabecafebabecafebabecafebabeaaaaaaaa".into(),
				commit_time: None,
			},
			url.to_string(),
			credential_token,
			vec![],
			String::new(),
		)
	}

	#[test]
	fn git_install_rejects_an_expired_pinned_source_session() {
		let app_data = tempdir().unwrap();
		let client =
			rocket::local::blocking::Client::tracked(crate::build_rocket(
				rocket::Config::default(),
				app_data.path().to_path_buf(),
			))
			.expect("client");
		let sessions = client
			.rocket()
			.state::<PinnedSourceSessions>()
			.expect("sessions state");
		let mut expired =
			dummy_git_session("https://github.com/acme/skills.git", None);
		expired.set_created_at(
			std::time::Instant::now()
				- std::time::Duration::from_secs(10 * 60 + 1),
		);
		sessions.insert("expired".to_string(), expired);

		let response = client
			.post("/api/v1/skills/git/install")
			.json(&serde_json::json!({
				"session_id": "expired",
				"skill_paths": ["music"],
				"agents": ["claude"],
				"scope": "global",
				"project_root": null
			}))
			.dispatch();

		assert_eq!(response.status(), Status::NotFound);
		let body: serde_json::Value = serde_json::from_str(
			&response.into_string().expect("response body"),
		)
		.expect("json body");
		assert_eq!(body["code"], "SESSION_NOT_FOUND");
	}

	#[test]
	fn git_sync_rejects_an_expired_pinned_source_session() {
		let app_data = tempdir().unwrap();
		let client =
			rocket::local::blocking::Client::tracked(crate::build_rocket(
				rocket::Config::default(),
				app_data.path().to_path_buf(),
			))
			.expect("client");
		let sessions = client
			.rocket()
			.state::<PinnedSourceSessions>()
			.expect("sessions state");
		let mut expired =
			dummy_git_session("https://github.com/acme/skills.git", None);
		expired.set_created_at(
			std::time::Instant::now()
				- std::time::Duration::from_secs(10 * 60 + 1),
		);
		sessions.insert("expired".to_string(), expired);

		let response = client
			.post("/api/v1/skills/git/sync")
			.json(&serde_json::json!({
				"session_id": "expired",
				"name": "music",
				"scope": "global",
				"project_root": null,
				"skill_path": "music/SKILL.md",
				"source_paths": []
			}))
			.dispatch();

		assert_eq!(response.status(), Status::NotFound);
		let body: serde_json::Value = serde_json::from_str(
			&response.into_string().expect("response body"),
		)
		.expect("json body");
		assert_eq!(body["code"], "SESSION_NOT_FOUND");
	}

	#[cfg(unix)]
	fn block_on<F: std::future::Future>(fut: F) -> F::Output {
		rocket::tokio::runtime::Builder::new_current_thread()
			.enable_all()
			.build()
			.unwrap()
			.block_on(fut)
	}

	// These by-path delete tests fake the home directory via env overrides. On
	// Windows `dirs::home_dir()` resolves through `SHGetKnownFolderPath` (ignores
	// env), so the temp home cannot be redirected and the allow-list roots never
	// match the fixture, making the success assertions fail. The delete/containment
	// logic is platform-agnostic and is covered on unix; gate these home-dependent
	// tests and their helpers to unix (Windows is a documented limitation).
	#[cfg(unix)]
	fn write_claude_skill(
		home: &std::path::Path,
		name: &str,
	) -> std::path::PathBuf {
		let dir = home.join(".claude/skills").join(name);
		std::fs::create_dir_all(&dir).unwrap();
		std::fs::write(
			dir.join("SKILL.md"),
			format!("---\nname: {name}\ndescription: d\n---\n"),
		)
		.unwrap();
		dir
	}

	#[cfg(unix)]
	fn by_path_req(
		source_path: &std::path::Path,
		confirm: Option<bool>,
	) -> DeleteSkillByPathRequest {
		DeleteSkillByPathRequest {
			source_path: source_path.join("SKILL.md").display().to_string(),
			agents: vec!["claude".to_string()],
			scope: "global".to_string(),
			project_root: None,
			all_agents: None,
			confirm,
		}
	}

	#[cfg(unix)]
	#[test]
	fn delete_by_path_dry_run_default_lists_paths_and_keeps_dir() {
		with_isolated_env(|home, _state| {
			let dir = write_claude_skill(home, "mytool");
			let resp = block_on(delete_skill_by_path(
				TrustedLocalOrigin,
				Json(by_path_req(&dir, None)),
			))
			.ok()
			.expect("handler returned ok")
			.into_inner();
			assert!(resp.success);
			assert!(resp.dry_run, "default must be dry-run");
			assert!(resp.paths.iter().any(|p| p.ends_with("mytool")));
			assert!(dir.exists(), "dry-run must not delete");
		});
	}

	#[cfg(unix)]
	#[test]
	fn delete_by_path_confirm_deletes_dir() {
		with_isolated_env(|home, _state| {
			let dir = write_claude_skill(home, "goner");
			let resp = block_on(delete_skill_by_path(
				TrustedLocalOrigin,
				Json(by_path_req(&dir, Some(true))),
			))
			.ok()
			.expect("handler returned ok")
			.into_inner();
			assert!(resp.success);
			assert!(!resp.dry_run);
			assert!(!dir.exists(), "confirm deletes the dir");
		});
	}

	#[cfg(unix)]
	#[test]
	fn delete_by_path_symlink_escaping_root_is_refused() {
		with_isolated_env(|home, _state| {
			// A symlink inside the agent skills dir whose target escapes every
			// allow-listed root must NOT be remove_dir_all'd.
			let outside = home.join("outside/evil");
			std::fs::create_dir_all(&outside).unwrap();
			std::fs::write(outside.join("SKILL.md"), "x").unwrap();
			let skills = home.join(".claude/skills");
			std::fs::create_dir_all(&skills).unwrap();
			let link = skills.join("evil");
			std::os::unix::fs::symlink(&outside, &link).unwrap();

			let resp = block_on(delete_skill_by_path(
				TrustedLocalOrigin,
				Json(by_path_req(&link, Some(true))),
			))
			.ok()
			.expect("handler returned ok")
			.into_inner();
			assert!(
				!resp.success,
				"out-of-tree symlink target must be refused"
			);
			assert!(outside.exists(), "out-of-tree dir must survive");
		});
	}

	#[cfg(unix)]
	#[test]
	fn delete_by_path_keeps_master_referenced_by_another_agent_symlink() {
		with_isolated_env(|home, _state| {
			// Project-scope universal master, read DIRECTLY (real dir) by cursor
			// and symlinked by another agent (claude). Deleting it by-path for
			// cursor must NOT remove the shared master (it would orphan claude's
			// live symlink + lose the skill for every other agent).
			let proj = home;
			let master = proj.join(".agents/skills/shared");
			std::fs::create_dir_all(&master).unwrap();
			std::fs::write(
				master.join("SKILL.md"),
				"---\nname: shared\ndescription: d\n---\n",
			)
			.unwrap();
			let claude = proj.join(".claude/skills");
			std::fs::create_dir_all(&claude).unwrap();
			std::os::unix::fs::symlink(&master, claude.join("shared")).unwrap();

			let req = DeleteSkillByPathRequest {
				source_path: master.join("SKILL.md").display().to_string(),
				agents: vec!["cursor".to_string()],
				scope: "project".to_string(),
				project_root: Some(proj.display().to_string()),
				all_agents: None,
				confirm: Some(true),
			};
			let resp =
				block_on(delete_skill_by_path(TrustedLocalOrigin, Json(req)))
					.ok()
					.expect("handler returned ok")
					.into_inner();

			assert!(
				master.join("SKILL.md").exists(),
				"shared master must survive a single-agent by-path delete"
			);
			assert!(
				claude.join("shared").join("SKILL.md").exists(),
				"the other agent's symlink must still resolve to the master"
			);
			assert!(
				resp.skipped.iter().any(|p| p.contains("shared")),
				"the kept master should be reported as skipped, got {:?}",
				resp.skipped
			);
		});
	}

	#[cfg(unix)]
	#[test]
	fn delete_by_path_absolutizes_relative_project_root() {
		with_isolated_env(|home, _state| {
			// Canonicalize the temp home so cwd-resolution (macOS /var ->
			// /private/var) matches the install paths and the absolutized
			// project root the handler computes from getcwd.
			let home = home.canonicalize().unwrap();
			let home = home.as_path();
			// A project with a .claude marker + a symlinked install.
			let proj = home.join("proj");
			let master = proj.join(".agents/skills/linked");
			std::fs::create_dir_all(&master).unwrap();
			std::fs::write(
				master.join("SKILL.md"),
				"---\nname: linked\ndescription: d\n---\n",
			)
			.unwrap();
			let skills = proj.join(".claude/skills");
			std::fs::create_dir_all(&skills).unwrap();
			let link = skills.join("linked");
			std::os::unix::fs::symlink(&master, &link).unwrap();

			// Drive delete with a RELATIVE project_root resolved against cwd.
			let prev = std::env::current_dir().unwrap();
			std::env::set_current_dir(home).unwrap();
			// Build the request inline (scope=project, project_root="proj"
			// relative, path = the link, confirm = true).
			let req = DeleteSkillByPathRequest {
				source_path: link.join("SKILL.md").display().to_string(),
				agents: vec!["claude".to_string()],
				scope: "project".to_string(),
				project_root: Some("proj".to_string()),
				all_agents: None,
				confirm: Some(true),
			};
			let resp =
				block_on(delete_skill_by_path(TrustedLocalOrigin, Json(req)))
					.ok()
					.expect("handler ok")
					.into_inner();
			std::env::set_current_dir(prev).unwrap();

			assert!(resp.success, "delete must resolve the relative root");
			assert!(!link.exists(), "referrer link removed");
			assert!(
				master.join("SKILL.md").exists(),
				"shared master must survive"
			);
		});
	}

	fn prune_req(
		scope: &str,
		project_root: Option<String>,
		confirm: Option<bool>,
	) -> PruneLockRequest {
		PruneLockRequest {
			scope: scope.to_string(),
			project_root,
			confirm,
		}
	}

	#[test]
	fn prune_lock_route_dry_run_reports_orphan_without_mutating() {
		with_isolated_env(|_home, state| {
			let lock_dir = state.join("skills");
			std::fs::create_dir_all(&lock_dir).unwrap();
			let lock_path = lock_dir.join(".skill-lock.json");
			std::fs::write(&lock_path, ORPHAN_LOCK_JSON).unwrap();
			let before = std::fs::read(&lock_path).unwrap();

			let resp = prune_lock_route(
				TrustedLocalOrigin,
				Json(prune_req("global", None, None)),
			)
			.ok()
			.expect("handler returned ok")
			.into_inner();

			assert!(resp.dry_run);
			assert!(resp.error.is_none());
			assert!(resp.pruned.iter().any(|n| n == "orphan"));
			assert_eq!(std::fs::read(&lock_path).unwrap(), before);
		});
	}

	#[test]
	fn prune_lock_route_confirm_removes_orphan_entry() {
		with_isolated_env(|_home, state| {
			let lock_dir = state.join("skills");
			std::fs::create_dir_all(&lock_dir).unwrap();
			let lock_path = lock_dir.join(".skill-lock.json");
			std::fs::write(&lock_path, ORPHAN_LOCK_JSON).unwrap();

			let resp = prune_lock_route(
				TrustedLocalOrigin,
				Json(prune_req("global", None, Some(true))),
			)
			.ok()
			.expect("handler returned ok")
			.into_inner();

			assert!(!resp.dry_run);
			assert!(resp.pruned.iter().any(|n| n == "orphan"));
			let raw = std::fs::read_to_string(&lock_path).unwrap();
			let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap();
			assert!(parsed["skills"].get("orphan").is_none());
			assert_eq!(parsed["version"], 3);
		});
	}

	#[test]
	fn prune_lock_route_project_requires_project_root() {
		with_isolated_env(|_home, _state| {
			let resp = prune_lock_route(
				TrustedLocalOrigin,
				Json(prune_req("project", None, Some(true))),
			)
			.ok()
			.expect("handler returned ok")
			.into_inner();
			assert!(resp.error.is_some(), "project prune needs a project root");
			assert!(resp.pruned.is_empty());
		});
	}

	// GAP-4: import_skill inherits the symlink-only model via
	// add_skill_from_path -- it must materialize a .agents Master + a link
	// (never an isolated copy) and still write the install lock from the
	// SOURCE folder (spec line 447).
	#[cfg(unix)]
	#[test]
	fn import_skill_links_master_and_writes_lock() {
		with_isolated_env(|home, _state| {
			// Create source skill outside the project
			let source_skill = home.join("source-skills/my-import-skill");
			std::fs::create_dir_all(&source_skill).unwrap();
			std::fs::write(
					source_skill.join("SKILL.md"),
					"---\nname: my-import-skill\ndescription: test\n---\n\n# My Import Skill\n",
				)
				.unwrap();

			// Build a project with a .claude marker
			let project = home.join("myproject");
			std::fs::create_dir_all(project.join(".claude/skills")).unwrap();

			let agent = AgentParam(AgentType::Claude);
			let scope = ScopeParams {
				scope: Some("project".to_string()),
				project_root: Some(project.display().to_string()),
			};
			let body = Json(crate::dto::skill::ImportSkillRequest {
				path: source_skill.join("SKILL.md").display().to_string(),
			});

			import_skill(TrustedLocalOrigin, agent, scope, body)
				.ok()
				.expect("import_skill returned ok");

			// 1. .agents Master exists
			assert!(
				project
					.join(".agents/skills/my-import-skill/SKILL.md")
					.exists(),
				".agents master must exist",
			);
			// 2. Claude link is a symlink (symlink-only model)
			assert!(
				aghub_core::skills::linker::Linker::is_link(
					&project.join(".claude/skills/my-import-skill"),
				),
				"claude skills entry must be a symlink",
			);
			// 3. Project lock contains the skill
			let lock = skill::lock::local::read_local_lock(Some(&project));
			assert!(
				lock.skills.contains_key("my-import-skill"),
				"project lock must contain the skill",
			);
		});
	}

	#[test]
	fn git_sync_ignores_root_source_path_and_preserves_siblings() {
		with_isolated_env(|_, _| {
			let temp = tempdir().unwrap();
			let project = temp.path().join("project");
			let skills_root = project.join(".claude/skills");
			let target = skills_root.join("sync-me");
			let sibling = skills_root.join("other");
			std::fs::create_dir_all(&target).unwrap();
			std::fs::create_dir_all(&sibling).unwrap();
			std::fs::write(
				target.join("SKILL.md"),
				"---\nname: sync-me\ndescription: old\n---\n\nold\n",
			)
			.unwrap();
			std::fs::write(
				sibling.join("SKILL.md"),
				"---\nname: other\ndescription: keep\n---\n\nkeep\n",
			)
			.unwrap();
			skill::add_skill_to_local_lock(
				"sync-me",
				skill::LocalSkillLockEntry {
					source_url: None,
					ref_commit: None,
					source: "owner/repo".to_string(),
					ref_name: Some("main".to_string()),
					source_type: "github".to_string(),
					computed_hash: "old".to_string(),
					skill_path: Some("sync-me/SKILL.md".to_string()),
				},
				Some(&project),
			)
			.unwrap();

			let fixture = tempdir().unwrap();
			let cloned_skill = fixture.path().join("sync-me");
			std::fs::create_dir_all(&cloned_skill).unwrap();
			std::fs::write(
				cloned_skill.join("SKILL.md"),
				"---\nname: sync-me\ndescription: new\n---\n\nnew\n",
			)
			.unwrap();

			let app_data = tempdir().unwrap();
			let client =
				rocket::local::blocking::Client::tracked(crate::build_rocket(
					rocket::Config::default(),
					app_data.path().to_path_buf(),
				))
				.expect("client");
			let sessions = client
				.rocket()
				.state::<PinnedSourceSessions>()
				.expect("git clone sessions");
			sessions.insert(
				"sync-session".to_string(),
				session_from_fixture(
					fixture.path(),
					"https://github.com/owner/repo.git",
					"main",
				),
			);

			let response = client
				.post("/api/v1/skills/git/sync")
				.json(&serde_json::json!({
					"session_id": "sync-session",
					"name": "sync-me",
					"scope": "project",
					"project_root": project.display().to_string(),
					"skill_path": "sync-me/SKILL.md",
					"source_paths": [skills_root.display().to_string()],
				}))
				.dispatch();

			assert_eq!(response.status(), rocket::http::Status::Ok);
			assert!(std::fs::read_to_string(target.join("SKILL.md"))
				.unwrap()
				.contains("new"));
			assert!(
				sibling.join("SKILL.md").exists(),
				"sync must not replace the entire skills root"
			);
			assert!(std::fs::read_to_string(sibling.join("SKILL.md"))
				.unwrap()
				.contains("keep"));
		});
	}

	// Ticket 02 (SkillPath): the desktop install route must reject a traversal /
	// absolute `skill_path` BEFORE any filesystem write. Today it raw-joins the
	// client string (`temp_path.join(skill_path)`), so an absolute path collapses
	// the join to the absolute path, escapes the clone root, and an out-of-tree
	// SKILL.md gets read and copied into the `.agents/skills` master. This asserts
	// the escape is refused and nothing is materialized from outside the clone.
	// FAILS on the raw-join (install succeeds); passes once the route validates
	// each path through `skill::SkillPath` before any join.
	#[cfg(unix)]
	#[test]
	fn git_install_rejects_out_of_tree_skill_path_before_write() {
		with_isolated_env(|home, _state| {
			// An out-of-tree skill the attacker points `skill_path` at.
			// Path validation runs before any fetch, so a dummy session is enough.
			let outside = tempdir().unwrap();
			let evil = outside.path().join("evil");
			std::fs::create_dir_all(&evil).unwrap();
			std::fs::write(
				evil.join("SKILL.md"),
				"---\nname: evil\ndescription: stolen\n---\n\nstolen\n",
			)
			.unwrap();

			let app_data = tempdir().unwrap();
			let client =
				rocket::local::blocking::Client::tracked(crate::build_rocket(
					rocket::Config::default(),
					app_data.path().to_path_buf(),
				))
				.expect("client");
			let sessions = client
				.rocket()
				.state::<PinnedSourceSessions>()
				.expect("git clone sessions");
			sessions.insert(
				"evil-session".to_string(),
				dummy_git_session("https://github.com/owner/repo.git", None),
			);

			// Absolute path: `temp_path.join(<abs>)` collapses to the absolute
			// path, escaping the clone entirely.
			let response = client
				.post("/api/v1/skills/git/install")
				.json(&serde_json::json!({
					"session_id": "evil-session",
					"skill_paths": [evil.display().to_string()],
					"agents": ["claude"],
					"scope": "global",
					"project_root": null,
				}))
				.dispatch();

			// The route must reject the traversal outright, not install it.
			assert_eq!(
				response.status(),
				rocket::http::Status::BadRequest,
				"an out-of-tree skill_path must be refused with 400",
			);

			// And nothing from outside the clone may reach the master.
			assert!(
				!home.join(".agents/skills/evil").exists(),
				"out-of-tree skill must not be materialized into the master",
			);
		});
	}

	#[test]
	fn git_sync_records_ref_commit_from_session_head() {
		with_isolated_env(|_, _| {
			let temp = tempdir().unwrap();
			let project = temp.path().join("project");
			let skills_root = project.join(".claude/skills");
			let target = skills_root.join("sync-me");
			std::fs::create_dir_all(&target).unwrap();
			std::fs::write(
				target.join("SKILL.md"),
				"---\nname: sync-me\ndescription: old\n---\n\nold\n",
			)
			.unwrap();
			skill::add_skill_to_local_lock(
				"sync-me",
				skill::LocalSkillLockEntry {
					source_url: None,
					ref_commit: None,
					source: "owner/repo".to_string(),
					ref_name: Some("main".to_string()),
					source_type: "github".to_string(),
					computed_hash: "old".to_string(),
					skill_path: Some("sync-me/SKILL.md".to_string()),
				},
				Some(&project),
			)
			.unwrap();

			// Session pins a scanned commit; refCommit comes from the snapshot
			// (not a re-read of a temp-repo HEAD).
			let fixture = tempdir().unwrap();
			let cloned_skill = fixture.path().join("sync-me");
			std::fs::create_dir_all(&cloned_skill).unwrap();
			std::fs::write(
				cloned_skill.join("SKILL.md"),
				"---\nname: sync-me\ndescription: new\n---\n\nnew\n",
			)
			.unwrap();

			let app_data = tempdir().unwrap();
			let client =
				rocket::local::blocking::Client::tracked(crate::build_rocket(
					rocket::Config::default(),
					app_data.path().to_path_buf(),
				))
				.expect("client");
			let sessions = client
				.rocket()
				.state::<PinnedSourceSessions>()
				.expect("git clone sessions");
			let session = session_from_fixture(
				fixture.path(),
				"https://github.com/owner/repo.git",
				"main",
			);
			let pinned_commit = session.commit_oid().to_string();
			sessions.insert("sync-session".to_string(), session);

			let response = client
				.post("/api/v1/skills/git/sync")
				.json(&serde_json::json!({
					"session_id": "sync-session",
					"name": "sync-me",
					"scope": "project",
					"project_root": project.display().to_string(),
					"skill_path": "sync-me/SKILL.md",
					"source_paths": [skills_root.display().to_string()],
				}))
				.dispatch();

			assert_eq!(response.status(), rocket::http::Status::Ok);
			let lock = skill::lock::local::read_local_lock(Some(&project));
			assert_eq!(
				lock.skills["sync-me"].ref_commit.as_deref(),
				Some(pinned_commit.as_str()),
				"git_sync must record the session snapshot commit as refCommit",
			);
		});
	}

	#[test]
	fn git_sync_locked_but_uninstalled_maps_to_skill_not_installed() {
		with_isolated_env(|_, _| {
			let temp = tempdir().unwrap();
			let project = temp.path().join("project");
			// Locked, but NO installed copy on disk: resync must report
			// NotInstalled, which git-sync maps to SKILL_NOT_INSTALLED (404).
			skill::add_skill_to_local_lock(
				"sync-me",
				skill::LocalSkillLockEntry {
					source_url: None,
					ref_commit: None,
					source: "owner/repo".to_string(),
					ref_name: Some("main".to_string()),
					source_type: "github".to_string(),
					computed_hash: "old".to_string(),
					skill_path: Some("sync-me/SKILL.md".to_string()),
				},
				Some(&project),
			)
			.unwrap();

			let fixture = tempdir().unwrap();
			let cloned_skill = fixture.path().join("sync-me");
			std::fs::create_dir_all(&cloned_skill).unwrap();
			std::fs::write(
				cloned_skill.join("SKILL.md"),
				"---\nname: sync-me\ndescription: new\n---\n\nnew\n",
			)
			.unwrap();

			let app_data = tempdir().unwrap();
			let client =
				rocket::local::blocking::Client::tracked(crate::build_rocket(
					rocket::Config::default(),
					app_data.path().to_path_buf(),
				))
				.expect("client");
			let sessions = client
				.rocket()
				.state::<PinnedSourceSessions>()
				.expect("git clone sessions");
			sessions.insert(
				"sync-session".to_string(),
				session_from_fixture(
					fixture.path(),
					"https://github.com/owner/repo.git",
					"main",
				),
			);

			let response = client
				.post("/api/v1/skills/git/sync")
				.json(&serde_json::json!({
					"session_id": "sync-session",
					"name": "sync-me",
					"scope": "project",
					"project_root": project.display().to_string(),
					"skill_path": "sync-me/SKILL.md",
					"source_paths": [project
						.join(".claude/skills")
						.display()
						.to_string()],
				}))
				.dispatch();

			assert_eq!(response.status(), rocket::http::Status::NotFound);
			let body: serde_json::Value =
				serde_json::from_str(&response.into_string().unwrap()).unwrap();
			assert_eq!(body["code"], "SKILL_NOT_INSTALLED");
			assert!(
				sessions.active("sync-session").is_some(),
				"a failed claimed session must be restored for retry",
			);
		});
	}

	#[test]
	fn git_sync_rejects_source_skill_name_mismatch() {
		with_isolated_env(|_, _| {
			let temp = tempdir().unwrap();
			let project = temp.path().join("project");
			let target = project.join(".claude/skills/sync-me");
			std::fs::create_dir_all(&target).unwrap();
			std::fs::write(
				target.join("SKILL.md"),
				"---\nname: sync-me\ndescription: old\n---\n\nold\n",
			)
			.unwrap();
			skill::add_skill_to_local_lock(
				"sync-me",
				skill::LocalSkillLockEntry {
					source_url: None,
					ref_commit: None,
					source: "owner/repo".to_string(),
					ref_name: Some("main".to_string()),
					source_type: "github".to_string(),
					computed_hash: "old".to_string(),
					skill_path: Some("other/SKILL.md".to_string()),
				},
				Some(&project),
			)
			.unwrap();

			let fixture = tempdir().unwrap();
			let cloned_skill = fixture.path().join("other");
			std::fs::create_dir_all(&cloned_skill).unwrap();
			std::fs::write(
				cloned_skill.join("SKILL.md"),
				"---\nname: other\ndescription: wrong\n---\n\nwrong\n",
			)
			.unwrap();

			let app_data = tempdir().unwrap();
			let client =
				rocket::local::blocking::Client::tracked(crate::build_rocket(
					rocket::Config::default(),
					app_data.path().to_path_buf(),
				))
				.expect("client");
			let sessions = client
				.rocket()
				.state::<PinnedSourceSessions>()
				.expect("git clone sessions");
			sessions.insert(
				"sync-session".to_string(),
				session_from_fixture(
					fixture.path(),
					"https://github.com/owner/repo.git",
					"main",
				),
			);

			let response = client
				.post("/api/v1/skills/git/sync")
				.json(&serde_json::json!({
					"session_id": "sync-session",
					"name": "sync-me",
					"scope": "project",
					"project_root": project.display().to_string(),
					"skill_path": "other/SKILL.md",
					"source_paths": [target.display().to_string()],
				}))
				.dispatch();

			assert_eq!(response.status(), rocket::http::Status::BadRequest);
			assert!(std::fs::read_to_string(target.join("SKILL.md"))
				.unwrap()
				.contains("old"));
		});
	}

	#[test]
	fn reconcile_skill_reuses_master_for_native_reader_opencode() {
		let _guard = crate::routes::test_env_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		let temp = tempdir().unwrap();
		let project_root = temp.path().join("project");
		std::fs::create_dir_all(&project_root).unwrap();

		let mut source_manager = aghub_core::ConfigManager::new(
			create_adapter(AgentType::Claude),
			false,
			Some(&project_root),
		);
		source_manager.load().unwrap();
		let mut skill = Skill::new("repo-helper");
		skill.description = Some("Copies files".to_string());
		source_manager.add_skill(skill).unwrap();
		let asset_dir = project_root.join(".claude/skills/repo-helper/assets");
		std::fs::create_dir_all(&asset_dir).unwrap();
		std::fs::write(asset_dir.join("notes.txt"), "hello").unwrap();

		let result = reconcile_skill(
			ResourceLocator {
				agent: AgentType::Claude,
				scope: InstallScope::Project,
				project_root: Some(project_root.clone()),
				name: "repo-helper".to_string(),
			},
			vec![AgentType::OpenCode],
			vec![],
		)
		.unwrap();

		assert_eq!(result.success_count(), 1);
		assert!(project_root
			.join(".agents/skills/repo-helper/assets/notes.txt")
			.exists());
		assert!(
			!project_root.join(".opencode/skills/repo-helper").exists(),
			"OpenCode is a NativeReader and must not get a private duplicate",
		);
	}

	#[test]
	fn detect_current_branch_uses_gix_not_subprocess() {
		// Scan no longer reads a local clone HEAD for the current branch.
		// Keep the invariant that this route file must not shell out to `git`.
		let source = include_str!("skills.rs");
		assert!(
			!source.contains("Command::new(\"git\")"),
			"branch detection must not shell out to the git binary"
		);
	}

	#[test]
	fn list_branches_for_scan_returns_cached_without_fetching() {
		let runtime = tokio::runtime::Runtime::new().unwrap();
		let branches = runtime
			.block_on(list_branches_for_scan(
				Some(vec!["main".to_string()]),
				|| panic!("fetcher should not be called"),
			))
			.unwrap_or_else(|e| panic!("{}", e.body.error));
		assert_eq!(branches, vec!["main".to_string()]);
	}

	#[test]
	fn list_branches_for_scan_propagates_fetch_errors() {
		let runtime = tokio::runtime::Runtime::new().unwrap();
		let error = runtime
			.block_on(list_branches_for_scan(None, || {
				Err(aghub_git::GitError::clone_failed("boom"))
			}))
			.unwrap_err();
		assert_eq!(error.status, Status::BadRequest);
		assert_eq!(error.body.code, "BRANCHES_ERROR");
		assert!(error.body.error.contains("Failed to list remote branches"));
	}

	// ---- M2: positive content/tree reads (over-strictness regression guard) -

	/// A legitimate global-scope skill under `~/.claude/skills` must serve its
	/// content (200), not be over-strictly refused. Uses `with_isolated_env` so
	/// HOME points at a temp dir and concurrent HOME-mutating tests cannot race
	/// the allow-list resolution.
	#[cfg(unix)]
	#[test]
	fn skill_content_serves_legit_global_skill() {
		with_isolated_env(|home, _| {
			let skill_dir = home.join(".claude/skills/legit");
			std::fs::create_dir_all(&skill_dir).unwrap();
			let skill_md = skill_dir.join("SKILL.md");
			std::fs::write(
				&skill_md,
				"---\nname: legit\ndescription: d\n---\n\n# Body\n",
			)
			.unwrap();

			let app_data = tempdir().unwrap();
			let client =
				rocket::local::blocking::Client::tracked(crate::build_rocket(
					rocket::Config::default(),
					app_data.path().to_path_buf(),
				))
				.expect("client");

			let mut q = url::form_urlencoded::Serializer::new(String::new());
			q.append_pair("path", &skill_md.to_string_lossy());
			q.append_pair("scope", "global");
			let uri = format!("/api/v1/skills/content?{}", q.finish());

			let response = client.get(&uri).dispatch();
			assert_eq!(
				response.status(),
				Status::Ok,
				"a legitimate global skill read must be served, not refused"
			);
			let body = response.into_string().expect("body");
			assert!(
				body.contains("# Body"),
				"served content should include the skill body, got: {body}"
			);
		});
	}

	/// A project-scope skill tree (scope=project + project_root) must return 200
	/// and list the skill's files — INCLUDING the universal-install case where a
	/// per-agent dir entry is a symlink at the `.agents/skills/<name>` master.
	/// The symlink target stays inside the allow-listed `.agents/skills` root,
	/// so it must be rendered, not 400'd (C3 regression guard).
	#[cfg(unix)]
	#[test]
	fn skill_tree_serves_project_universal_symlink() {
		use std::os::unix::fs::symlink;
		with_isolated_env(|_, _| {
			let project = tempdir().unwrap();
			// Universal master: <project>/.agents/skills/foo
			let master = project.path().join(".agents/skills/foo");
			std::fs::create_dir_all(&master).unwrap();
			std::fs::write(
				master.join("SKILL.md"),
				"---\nname: foo\ndescription: d\n---\n\n# Body\n",
			)
			.unwrap();
			std::fs::write(master.join("extra.md"), "extra").unwrap();
			// Per-agent dir that symlinks into the master (universal install).
			let agent_skills = project.path().join(".claude/skills");
			std::fs::create_dir_all(&agent_skills).unwrap();
			let link = agent_skills.join("foo");
			symlink(&master, &link).unwrap();

			let app_data = tempdir().unwrap();
			let client =
				rocket::local::blocking::Client::tracked(crate::build_rocket(
					rocket::Config::default(),
					app_data.path().to_path_buf(),
				))
				.expect("client");

			// Read the tree of the per-agent skills DIR so `foo` is encountered
			// as a symlink ENTRY during recursion. Under the old blanket
			// rejection this 400'd the whole tree; now the entry is included
			// (its target is the master inside `.agents/skills`) and recursed.
			let mut q = url::form_urlencoded::Serializer::new(String::new());
			q.append_pair("path", &agent_skills.to_string_lossy());
			q.append_pair("scope", "project");
			q.append_pair("project_root", &project.path().to_string_lossy());
			let uri = format!("/api/v1/skills/tree?{}", q.finish());

			let response = client.get(&uri).dispatch();
			assert_eq!(
				response.status(),
				Status::Ok,
				"a universal-install symlinked skill tree must be served, \
				 not 400"
			);
			let body = response.into_string().expect("body");
			assert!(
				body.contains("foo")
					&& body.contains("SKILL.md")
					&& body.contains("extra.md"),
				"tree should recurse the symlinked master's files, got: {body}"
			);
		});
	}

	/// A symlink ENTRY inside a skill dir whose target escapes the allow-listed
	/// roots must be silently skipped — the tree still returns 200 and simply
	/// omits the escaping entry (it does NOT 400 the whole tree, and does NOT
	/// leak the out-of-tree path).
	#[cfg(unix)]
	#[test]
	fn skill_tree_skips_escaping_symlink_entry() {
		use std::os::unix::fs::symlink;
		with_isolated_env(|_, _| {
			let project = tempdir().unwrap();
			let skill_dir = project.path().join(".claude/skills/foo");
			std::fs::create_dir_all(&skill_dir).unwrap();
			std::fs::write(
				skill_dir.join("SKILL.md"),
				"---\nname: foo\ndescription: d\n---\n\n# Body\n",
			)
			.unwrap();
			// An entry symlink pointing OUT of the skills roots entirely.
			let outside = tempdir().unwrap();
			std::fs::write(outside.path().join("secret.txt"), "top secret")
				.unwrap();
			symlink(
				outside.path().join("secret.txt"),
				skill_dir.join("escape.txt"),
			)
			.unwrap();

			let app_data = tempdir().unwrap();
			let client =
				rocket::local::blocking::Client::tracked(crate::build_rocket(
					rocket::Config::default(),
					app_data.path().to_path_buf(),
				))
				.expect("client");

			let mut q = url::form_urlencoded::Serializer::new(String::new());
			q.append_pair("path", &skill_dir.to_string_lossy());
			q.append_pair("scope", "project");
			q.append_pair("project_root", &project.path().to_string_lossy());
			let uri = format!("/api/v1/skills/tree?{}", q.finish());

			let response = client.get(&uri).dispatch();
			assert_eq!(
				response.status(),
				Status::Ok,
				"an escaping entry symlink must not 400 the whole tree"
			);
			let body = response.into_string().expect("body");
			assert!(
				body.contains("SKILL.md"),
				"tree should still list real files, got: {body}"
			);
			assert!(
				!body.contains("escape.txt")
					&& !body.contains(
						&outside.path().to_string_lossy().to_string()
					),
				"escaping symlink entry + its target path must be hidden, \
				 got: {body}"
			);
		});
	}

	#[test]
	fn github_credential_url_accepts_github_https() {
		assert!(require_github_credential_url(
			"https://github.com/owner/repo.git",
		)
		.is_ok());
	}

	#[test]
	fn github_credential_url_rejects_non_github_hosts() {
		let err = require_github_credential_url("https://evil.example/x.git")
			.unwrap_err();

		assert_eq!(err.status, Status::BadRequest);
		assert_eq!(err.body.code, "INVALID_GITHUB_CREDENTIAL_URL");
	}

	#[test]
	fn github_credential_url_rejects_github_lookalikes() {
		let err = require_github_credential_url(
			"https://github.com.attacker.example/x.git",
		)
		.unwrap_err();

		assert_eq!(err.status, Status::BadRequest);
		assert_eq!(err.body.code, "INVALID_GITHUB_CREDENTIAL_URL");
	}

	#[test]
	fn github_credential_url_rejects_non_https_github() {
		let err = require_github_credential_url("http://github.com/x.git")
			.unwrap_err();

		assert_eq!(err.status, Status::BadRequest);
		assert_eq!(err.body.code, "INVALID_GITHUB_CREDENTIAL_URL");
	}

	#[test]
	fn github_credential_url_rejects_non_default_port() {
		let err = require_github_credential_url(
			"https://github.com:8443/owner/repo.git",
		)
		.unwrap_err();

		assert_eq!(err.status, Status::BadRequest);
		assert_eq!(err.body.code, "INVALID_GITHUB_CREDENTIAL_URL");
	}

	#[test]
	fn github_credential_url_accepts_default_port() {
		assert!(require_github_credential_url(
			"https://github.com:443/owner/repo.git",
		)
		.is_ok());
	}

	#[test]
	fn same_origin_true_for_matching_origins() {
		assert!(same_origin(
			"https://gitlab.internal/a.git",
			"https://gitlab.internal/b.git",
		));
	}

	#[test]
	fn same_origin_false_for_different_hosts() {
		assert!(!same_origin("https://github.com/a", "https://evil.com/a"));
	}

	#[test]
	fn same_origin_false_on_parse_failure() {
		assert!(!same_origin("not a url", "https://github.com/a"));
	}

	#[test]
	fn same_origin_false_for_same_host_different_port() {
		// Session pinning now keys on the full origin: the same host on a
		// different explicit port is a DIFFERENT origin and must NOT match,
		// so a token bound to one port can't be reused against another.
		assert!(!same_origin(
			"https://git.internal:8080/a.git",
			"https://git.internal:9090/b.git",
		));
	}

	#[test]
	fn same_origin_true_for_default_port_forms() {
		// `https://h` and `https://h:443` are the same origin (default port
		// folds in), so this remains a match.
		assert!(same_origin(
			"https://git.internal/a.git",
			"https://git.internal:443/b.git",
		));
	}

	// ─── forwarded_token_for_url: host-scoped match + D8 origin pin ─────────

	fn forwarded(pairs: &[(&str, &str)]) -> ForwardedGitTokens {
		use crate::credentials::forwarding::ForwardedEntry;
		// Entries with NO explicit origin: `forwarded_token_for_url` then
		// re-resolves the forwarded source's clone-URL origin, exercising the
		// host-scoped + origin-pin path independently of the wire origin.
		ForwardedGitTokens(
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
				.collect(),
		)
	}

	#[test]
	fn forwarded_token_matches_same_github_source() {
		// Forwarded as the bare shorthand; the request uses the full URL. Both
		// resolve to the same github.com origin, so the token is attached.
		let map = forwarded(&[("owner/repo", "TOK")]);
		assert_eq!(
			forwarded_token_for_url(&map, "https://github.com/owner/repo.git"),
			Some("TOK".to_string())
		);
	}

	#[test]
	fn forwarded_token_not_attached_cross_host() {
		// A github.com forwarded token must not satisfy a gitlab.com request of
		// the same `owner/repo` shape (host is encoded in the key set).
		let map = forwarded(&[("owner/repo", "GHTOK")]);
		assert_eq!(
			forwarded_token_for_url(&map, "https://gitlab.com/owner/repo.git"),
			None
		);
	}

	#[test]
	fn forwarded_token_not_attached_same_host_different_port() {
		// D8: a token forwarded for a self-hosted forge on one port must NOT be
		// attached to a request for the SAME host on a different port.
		let map =
			forwarded(&[("https://git.internal:8443/owner/repo.git", "TOK")]);
		assert_eq!(
			forwarded_token_for_url(
				&map,
				"https://git.internal:9090/owner/repo.git"
			),
			None
		);
	}

	#[test]
	fn forwarded_token_attached_same_host_same_port() {
		// The positive counterpart to the D8 negative: a self-hosted forge on a
		// custom port DOES match when the request is for the SAME origin. This
		// proves the port-mismatch rejection above is the origin pin, not a
		// resolve failure on custom-port URLs.
		let map =
			forwarded(&[("https://git.internal:8443/owner/repo.git", "TOK")]);
		assert_eq!(
			forwarded_token_for_url(
				&map,
				"https://git.internal:8443/owner/repo.git"
			),
			Some("TOK".to_string())
		);
	}

	#[test]
	fn forwarded_token_none_for_empty_map() {
		let map = forwarded(&[]);
		assert_eq!(
			forwarded_token_for_url(&map, "https://github.com/owner/repo.git"),
			None
		);
	}

	/// Build a single-entry map carrying an explicit wire `origin`, exercising
	/// the new `{ token, origin }` shape on the scan path.
	fn forwarded_with_origin(
		source: &str,
		token: &str,
		scheme: &str,
		host: &str,
		port: Option<u16>,
	) -> ForwardedGitTokens {
		use crate::credentials::forwarding::{ForwardedEntry, ForwardedOrigin};
		let mut m = std::collections::BTreeMap::new();
		m.insert(
			source.to_string(),
			ForwardedEntry {
				token: token.to_string(),
				origin: Some(ForwardedOrigin {
					scheme: scheme.to_string(),
					host: host.to_string(),
					port,
				}),
			},
		);
		ForwardedGitTokens(m)
	}

	#[test]
	fn forwarded_token_uses_entry_origin_to_pin() {
		// The entry carries its own controller-resolved origin (matching the
		// request), so the token is attached using the wire origin.
		let map = forwarded_with_origin(
			"owner/repo",
			"TOK",
			"https",
			"github.com",
			Some(443),
		);
		assert_eq!(
			forwarded_token_for_url(&map, "https://github.com/owner/repo.git"),
			Some("TOK".to_string())
		);
	}

	#[test]
	fn forwarded_token_entry_origin_mismatch_rejected() {
		// The entry's wire origin pins a DIFFERENT port than the request: the
		// scan path must not attach the token even though the host-scoped key
		// would match.
		let map = forwarded_with_origin(
			"https://git.internal:8443/owner/repo.git",
			"TOK",
			"https",
			"git.internal",
			Some(8443),
		);
		assert_eq!(
			forwarded_token_for_url(
				&map,
				"https://git.internal:9090/owner/repo.git"
			),
			None
		);
	}

	// A session token bound to one host must never be reused against a
	// different host: the guard runs before any clone/spawn_blocking, so this
	// is exercised end-to-end through the handler without any network.
	#[test]
	fn git_scan_rejects_session_credential_for_different_host() {
		let app_data = tempdir().unwrap();
		let client =
			rocket::local::blocking::Client::tracked(crate::build_rocket(
				rocket::Config::default(),
				app_data.path().to_path_buf(),
			))
			.expect("client");

		let sessions = client
			.rocket()
			.state::<PinnedSourceSessions>()
			.expect("git clone sessions");
		sessions.insert(
			"test-session".to_string(),
			dummy_git_session(
				"https://gitlab.internal/repo.git",
				Some("secret-token".to_string()),
			),
		);

		let response = client
			.post("/api/v1/skills/git/scan")
			.json(&serde_json::json!({
				"url": "https://evil.example/repo.git",
				"session_id": "test-session",
			}))
			.dispatch();

		assert_eq!(response.status(), Status::BadRequest);
		let raw = response.into_string().expect("response body");
		let parsed: serde_json::Value =
			serde_json::from_str(&raw).expect("json body");
		assert_eq!(parsed["code"], "SESSION_CREDENTIAL_HOST_MISMATCH");
	}

	#[test]
	fn git_scan_does_not_reuse_credentials_from_an_expired_session() {
		let _env = crate::routes::test_env_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		let _unavailable =
			crate::credentials::test_hooks::ForceCredentialBackendUnavailable::new();
		let app_data = tempdir().unwrap();
		let client =
			rocket::local::blocking::Client::tracked(crate::build_rocket(
				rocket::Config::default(),
				app_data.path().to_path_buf(),
			))
			.expect("client");
		let sessions = client
			.rocket()
			.state::<PinnedSourceSessions>()
			.expect("git clone sessions");
		let mut expired = dummy_git_session(
			"https://stale.example/repo.git",
			Some("stale-token".to_string()),
		);
		expired.set_created_at(
			std::time::Instant::now()
				- std::time::Duration::from_secs(10 * 60 + 1),
		);
		sessions.insert("expired".to_string(), expired);

		let response = client
			.post("/api/v1/skills/git/scan")
			.json(&serde_json::json!({
				"url": "http://127.0.0.1:1/owner/repo.git",
				"session_id": "expired"
			}))
			.dispatch();

		assert_eq!(response.status(), Status::ServiceUnavailable);
		let body: serde_json::Value = serde_json::from_str(
			&response.into_string().expect("response body"),
		)
		.expect("json body");
		assert_eq!(body["code"], "KEYCHAIN_UNAVAILABLE");
	}

	/// Regression (GitHub #15 P2-3, Codex-found): git-scan's host-scoped
	/// keyring fallback (no explicit `credential_id`) used to read the
	/// keyring via `.ok()?` / `.unwrap_or_default()`, silently degrading ANY
	/// failure — including "the backend itself is unreachable" — to "no
	/// credential bound". That let a private-source request proceed as if
	/// public and fail with a confusing clone/network error instead of a
	/// stable, retryable 503.
	///
	/// Forces the backend-unavailable path via
	/// `crate::credentials::test_hooks::ForceCredentialBackendUnavailable`
	/// (deterministic, cross-platform) instead of the previous
	/// `DBUS_SESSION_BUS_ADDRESS` tampering: that env var only affects Linux
	/// secret-service, so a macOS/Windows CI runner would see a non-503
	/// result (GitHub #15 round-2 Codex finding). Omit `credential_id` so
	/// resolution falls through to the host-fallback branch. The target URL
	/// points at a closed local port (connection refused instantly) so if
	/// this regresses back to "swallow and attempt a clone", the test fails
	/// fast on a connection error rather than hanging on a real network
	/// timeout — it must never reach the network at all now that
	/// `load_or_unavailable` fails first.
	#[test]
	fn git_scan_host_fallback_fails_closed_when_keyring_backend_unreachable() {
		let _env = crate::routes::test_env_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		let _unavailable =
			crate::credentials::test_hooks::ForceCredentialBackendUnavailable::new();

		let app_data = tempdir().unwrap();
		let client =
			rocket::local::blocking::Client::tracked(crate::build_rocket(
				rocket::Config::default(),
				app_data.path().to_path_buf(),
			))
			.expect("client");

		let response = client
			.post("/api/v1/skills/git/scan")
			.json(&serde_json::json!({
				"url": "http://127.0.0.1:1/owner/repo.git",
			}))
			.dispatch();

		assert_eq!(
			response.status(),
			Status::ServiceUnavailable,
			"an unreachable keyring backend must fail closed with 503, not \
			 a confusing not-found/clone error"
		);
		let raw = response.into_string().expect("response body");
		let parsed: serde_json::Value =
			serde_json::from_str(&raw).expect("json body");
		assert_eq!(parsed["code"], "KEYCHAIN_UNAVAILABLE");
	}

	#[test]
	fn install_fails_closed_when_keyring_backend_unreachable() {
		let _env = crate::routes::test_env_lock()
			.lock()
			.unwrap_or_else(|error| error.into_inner());
		let _unavailable =
			crate::credentials::test_hooks::ForceCredentialBackendUnavailable::new();
		let app_data = tempdir().unwrap();
		let client =
			rocket::local::blocking::Client::tracked(crate::build_rocket(
				rocket::Config::default(),
				app_data.path().to_path_buf(),
			))
			.expect("client");

		let response = client
			.post("/api/v1/skills/install")
			.json(&serde_json::json!({
				"source": "http://127.0.0.1:1/owner/repo.git",
				"agents": ["claude"],
				"skills": ["example"],
				"scope": "global",
				"install_all": false,
			}))
			.dispatch();

		assert_eq!(response.status(), Status::ServiceUnavailable);
		let body: serde_json::Value = serde_json::from_str(
			&response.into_string().expect("response body"),
		)
		.expect("json body");
		assert_eq!(body["code"], "KEYCHAIN_UNAVAILABLE");
	}

	#[cfg(unix)]
	#[test]
	fn git_install_writes_npx_lock_symlink_only() {
		with_isolated_env(|home, state| {
			let app_data = tempdir().unwrap();
			let client =
				rocket::local::blocking::Client::tracked(crate::build_rocket(
					rocket::Config::default(),
					app_data.path().to_path_buf(),
				))
				.expect("client");
			let app_sessions = client
				.rocket()
				.state::<PinnedSourceSessions>()
				.expect("sessions state");
			// Fixture must outlive the install request (backend copies from it).
			let fixture = tempdir().unwrap();
			let dst = fixture.path().join("my-skill");
			std::fs::create_dir_all(&dst).unwrap();
			std::fs::write(
				dst.join("SKILL.md"),
				"---\nname: my-skill\ndescription: d\n---\n",
			)
			.unwrap();
			app_sessions.insert(
				"sess-1".to_string(),
				session_from_fixture(
					fixture.path(),
					"https://github.com/o/r",
					"main",
				),
			);
			let response = client
				.post("/api/v1/skills/git/install")
				.json(&serde_json::json!({
					"session_id": "sess-1",
					"skill_paths": ["my-skill"],
					"agents": ["claude"],
					"scope": "global",
					"project_root": null
				}))
				.dispatch();
			assert_eq!(
				response.status(),
				rocket::http::Status::Ok,
				"handler returned ok"
			);
			let body: serde_json::Value =
				serde_json::from_str(&response.into_string().expect("body"))
					.expect("json");
			let results = body["results"].as_array().expect("results array");
			assert!(
				results.iter().any(|r| r["agent"] == "claude"),
				"per-agent row present"
			);
			let master = home.join(".agents/skills/my-skill/SKILL.md");
			assert!(master.exists(), "universal master written: {master:?}");
			let lock = state.join("skills/.skill-lock.json");
			let lock_alt = home.join(".agents/.skill-lock.json");
			assert!(
				lock.exists() || lock_alt.exists(),
				"a global skill install lock was written"
			);
			assert!(
				app_sessions.active("sess-1").is_none(),
				"a successful install must consume its pinned session",
			);
		});
	}

	#[cfg(unix)]
	#[test]
	fn git_install_preflights_mixed_agents_before_writing() {
		with_isolated_env(|home, _state| {
			let project = home.join("project");
			std::fs::create_dir_all(&project).unwrap();
			let app_data = tempdir().unwrap();
			let client =
				rocket::local::blocking::Client::tracked(crate::build_rocket(
					rocket::Config::default(),
					app_data.path().to_path_buf(),
				))
				.expect("client");
			let sessions = client
				.rocket()
				.state::<PinnedSourceSessions>()
				.expect("sessions state");
			let fixture = tempdir().unwrap();
			let source = fixture.path().join("my-skill");
			std::fs::create_dir_all(&source).unwrap();
			std::fs::write(
				source.join("SKILL.md"),
				"---\nname: my-skill\ndescription: d\n---\n",
			)
			.unwrap();
			sessions.insert(
				"mixed-session".to_string(),
				session_from_fixture(
					fixture.path(),
					"https://github.com/o/r",
					"main",
				),
			);

			let response = client
				.post("/api/v1/skills/git/install")
				.json(&serde_json::json!({
					"session_id": "mixed-session",
					"skill_paths": ["my-skill"],
					"agents": ["claude", "augmentcode"],
					"scope": "project",
					"project_root": project.display().to_string()
				}))
				.dispatch();

			assert_eq!(response.status(), rocket::http::Status::Ok);
			let body: serde_json::Value = serde_json::from_str(
				&response.into_string().expect("response body"),
			)
			.expect("json response");
			assert_eq!(body["results"].as_array().unwrap().len(), 2);
			assert!(
				!project.join(".agents/skills/my-skill").exists(),
				"route preflight must happen before the shared Master write",
			);
			assert!(
				!project.join(".claude/skills/my-skill").exists(),
				"the valid target must not be partially installed",
			);
		});
	}

	#[cfg(unix)]
	#[test]
	fn install_skill_returns_per_agent_rows_symlink_only() {
		with_isolated_env(|home, _state| {
			// Mock keyring: without it the install route fail-closes 503 on
			// hosts with no reachable credential backend (CI runners).
			let _keyring =
				crate::credentials::test_hooks::MockKeyringBackend::new();
			let work = home.join("work");
			let skill_dir = work.join("my-skill");
			std::fs::create_dir_all(&skill_dir).unwrap();
			std::fs::write(
				skill_dir.join("SKILL.md"),
				"---\nname: my-skill\ndescription: d\n---\n",
			)
			.unwrap();
			// Build the git fixture with gix (no `git` subprocess).
			{
				use gix::objs::tree::{Entry, EntryKind};
				let repo = gix::init(&work).unwrap();
				let blob_id = repo
					.write_blob(b"---\nname: my-skill\ndescription: d\n---\n")
					.unwrap()
					.detach();
				// Subtree: my-skill/ containing SKILL.md
				let subtree_id = repo
					.write_object(&gix::objs::Tree {
						entries: vec![Entry {
							mode: EntryKind::Blob.into(),
							filename: "SKILL.md".into(),
							oid: blob_id,
						}],
					})
					.unwrap()
					.detach();
				// Root tree containing the my-skill/ subdirectory
				let tree_id = repo
					.write_object(&gix::objs::Tree {
						entries: vec![Entry {
							mode: EntryKind::Tree.into(),
							filename: "my-skill".into(),
							oid: subtree_id,
						}],
					})
					.unwrap()
					.detach();
				let sig = gix::actor::SignatureRef::from_bytes(
					b"t <t@t> 1000000000 +0000",
				)
				.unwrap();
				repo.commit_as(
					sig,
					sig,
					"HEAD",
					"init",
					tree_id,
					std::iter::empty::<gix::ObjectId>(),
				)
				.unwrap();
			}

			let req = InstallSkillRequest {
				source: format!("file://{}", work.display()),
				agents: vec!["claude".to_string()],
				skills: vec!["my-skill".to_string()],
				scope: "global".to_string(),
				project_path: None,
				install_all: Some(false),
			};
			let repo =
				std::sync::Arc::new(skill_update::SkillRepository::new());
			let resp = block_on(install_skill_route_with_repo(
				req,
				ForwardedGitTokens::default(),
				repo,
			))
			.ok()
			.expect("handler ok")
			.into_inner();
			assert!(resp.success, "install succeeded");
			assert!(
				resp.agents.iter().any(|a| a.agent == "claude"),
				"per-agent rows present"
			);
			assert!(
				home.join(".agents/skills/my-skill/SKILL.md").exists(),
				"master materialized (symlink-only)"
			);
		});
	}

	#[cfg(unix)]
	#[test]
	fn delete_by_path_symlinked_install_uses_canonical_layout() {
		with_isolated_env(|home, _state| {
			let master = home.join(".agents/skills/linked");
			std::fs::create_dir_all(&master).unwrap();
			std::fs::write(
				master.join("SKILL.md"),
				"---\nname: linked\ndescription: d\n---\n",
			)
			.unwrap();
			let skills = home.join(".claude/skills");
			std::fs::create_dir_all(&skills).unwrap();
			let link = skills.join("linked");
			std::os::unix::fs::symlink(&master, &link).unwrap();

			let resp = block_on(delete_skill_by_path(
				TrustedLocalOrigin,
				Json(by_path_req(&link, Some(true))),
			))
			.ok()
			.expect("handler ok")
			.into_inner();
			assert!(resp.success);
			assert!(!link.exists(), "referrer link removed");
			assert!(
				master.join("SKILL.md").exists(),
				"shared master must NOT be deleted"
			);
		});
	}

	// NOTE: there is intentionally NO windows by-path junction-delete test here.
	// The by-path delete tests redirect HOME via env overrides, which Windows
	// `dirs::home_dir()` ignores (it uses SHGetKnownFolderPath — see the cfg(unix)
	// gate on these helpers above), so they cannot run on windows-latest.
	// Junction-aware delete is covered at the core level (Linker::is_link reparse
	// detection + the removal.rs / manager::skill windows junction tests).

	// P1-E2: entry_allowed routes its link probe through Linker::is_link, so a
	// link (unix symlink / windows junction) is subjected to the containment
	// guard. A link that ESCAPES the allow-listed roots is excluded; a link
	// that stays inside is allowed; a plain real entry is always allowed.
	#[cfg(unix)]
	#[test]
	fn entry_allowed_excludes_escaping_link_keeps_contained() {
		let tmp = tempfile::tempdir().unwrap();
		let root = tmp.path().join("root");
		std::fs::create_dir_all(&root).unwrap();
		std::fs::write(root.join("real.txt"), "x").unwrap();
		// An escaping symlink: target outside the allow-listed root.
		let outside = tmp.path().join("outside");
		std::fs::create_dir_all(&outside).unwrap();
		let escaping = root.join("escape");
		std::os::unix::fs::symlink(&outside, &escaping).unwrap();
		let roots = vec![root.clone()];

		assert!(
			entry_allowed(&root.join("real.txt"), &roots),
			"a plain real entry is always allowed"
		);
		assert!(
			!entry_allowed(&escaping, &roots),
			"an escaping link must be excluded"
		);
	}

	/// Restores the process CWD on drop, so a mid-test panic can never leave
	/// the process standing in a soon-deleted temp dir — a leaked deleted CWD
	/// makes every later `std::env::current_dir()` caller in this binary
	/// (e.g. `gix::init`) fail with NotFound.
	#[cfg(unix)]
	struct CwdGuard(std::path::PathBuf);

	#[cfg(unix)]
	impl CwdGuard {
		fn change_to(dir: &std::path::Path) -> Self {
			let prev = std::env::current_dir().unwrap();
			std::env::set_current_dir(dir).unwrap();
			Self(prev)
		}
	}

	#[cfg(unix)]
	impl Drop for CwdGuard {
		fn drop(&mut self) {
			let _ = std::env::set_current_dir(&self.0);
		}
	}

	#[cfg(unix)]
	#[test]
	fn install_skill_relative_project_root_is_absolutized() {
		with_isolated_env(|home, _state| {
			// Mock keyring: without it the install route fail-closes 503 on
			// hosts with no reachable credential backend (CI runners).
			let _keyring =
				crate::credentials::test_hooks::MockKeyringBackend::new();
			let proj = home.join("proj");
			std::fs::create_dir_all(proj.join(".claude")).unwrap();
			let work = home.join("work");
			let skill_dir = work.join("my-skill");
			std::fs::create_dir_all(&skill_dir).unwrap();
			std::fs::write(
				skill_dir.join("SKILL.md"),
				"---\nname: my-skill\ndescription: d\n---\n",
			)
			.unwrap();
			// Build the git fixture with gix (no `git` subprocess).
			{
				use gix::objs::tree::{Entry, EntryKind};
				let repo = gix::init(&work).unwrap();
				let blob_id = repo
					.write_blob(b"---\nname: my-skill\ndescription: d\n---\n")
					.unwrap()
					.detach();
				// Subtree: my-skill/ containing SKILL.md
				let subtree_id = repo
					.write_object(&gix::objs::Tree {
						entries: vec![Entry {
							mode: EntryKind::Blob.into(),
							filename: "SKILL.md".into(),
							oid: blob_id,
						}],
					})
					.unwrap()
					.detach();
				// Root tree containing the my-skill/ subdirectory
				let tree_id = repo
					.write_object(&gix::objs::Tree {
						entries: vec![Entry {
							mode: EntryKind::Tree.into(),
							filename: "my-skill".into(),
							oid: subtree_id,
						}],
					})
					.unwrap()
					.detach();
				let sig = gix::actor::SignatureRef::from_bytes(
					b"t <t@t> 1000000000 +0000",
				)
				.unwrap();
				repo.commit_as(
					sig,
					sig,
					"HEAD",
					"init",
					tree_id,
					std::iter::empty::<gix::ObjectId>(),
				)
				.unwrap();
			}

			let resp = {
				let _cwd = CwdGuard::change_to(home);
				let req = InstallSkillRequest {
					source: format!("file://{}", work.display()),
					agents: vec!["claude".to_string()],
					skills: vec!["my-skill".to_string()],
					scope: "project".to_string(),
					project_path: Some("proj".to_string()),
					install_all: Some(false),
				};
				let repo =
					std::sync::Arc::new(skill_update::SkillRepository::new());
				block_on(install_skill_route_with_repo(
					req,
					ForwardedGitTokens::default(),
					repo,
				))
				.ok()
				.expect("handler ok")
				.into_inner()
			};

			assert!(
				resp.agents.iter().all(|a| a
					.error
					.as_deref()
					.map(|e| !e.contains("absolute"))
					.unwrap_or(true)),
				"no NonAbsoluteTarget error rows"
			);
			assert!(
				proj.join(".agents/skills/my-skill/SKILL.md").exists(),
				"master written at absolutized project root"
			);
		});
	}

	#[test]
	fn git_install_skills_agent_label_order_matches_request() {
		// Two agents with different target dirs: each result row must carry
		// the agent id whose install result it is reporting, not a positional
		// accident.
		let _guard = crate::routes::test_env_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());

		let temp = tempdir().unwrap();
		let project_root = temp.path().join("project");

		let clone_dir = temp.path().join("clone");
		let skill_src = clone_dir.join("hello-skill");
		std::fs::create_dir_all(&skill_src).unwrap();
		std::fs::write(
			skill_src.join("SKILL.md"),
			"---\nname: hello-skill\ndescription: test\n---\n\n# Hello\n",
		)
		.unwrap();

		let req_agents = vec!["claude".to_string(), "opencode".to_string()];
		let resource_scope = ResourceScope::ProjectOnly;

		let (valid, invalid) = partition_install_agents_in_request_order(
			&req_agents,
			resource_scope,
			Some(project_root.as_path()),
		);

		// also verify unknown agents go to invalid
		let (v2, inv2) = partition_install_agents_in_request_order(
			&["claude".to_string(), "not-a-real-agent".to_string()],
			resource_scope,
			Some(project_root.as_path()),
		);
		assert_eq!(v2.len(), 1, "only claude is valid");
		assert_eq!(inv2.len(), 1, "not-a-real-agent is invalid");
		assert!(
			inv2[0].1.contains("Unknown agent"),
			"wrong error: {}",
			inv2[0].1,
		);

		assert!(invalid.is_empty(), "unexpected invalids: {:?}", invalid);
		assert_eq!(valid.len(), 2);
		assert_eq!(valid[0].0, "claude");
		assert_eq!(valid[1].0, "opencode");

		let target_agents: Vec<AgentType> =
			valid.iter().map(|(_, a)| *a).collect();

		let lock_source = skill::InstallLockSource {
			source: "owner/repo".to_string(),
			source_type: "github".to_string(),
			source_url: "https://github.com/owner/repo".to_string(),
			ref_name: Some("main".to_string()),
		};
		let skill_file = skill_src.join("SKILL.md");
		let request =
			aghub_core::skills::install_fetched::FetchedSkillInstallRequest {
				skill_file: &skill_file,
				source: &lock_source,
				lock_skill_path: skill::lock_skill_file_path("hello-skill"),
				ref_commit: None,
				scope: resource_scope,
				project_root: Some(project_root.as_path()),
				target_agents: &target_agents,
				expected_name: None,
				target: aghub_core::skills::linker::LinkTarget::Relative,
			};

		let report =
			aghub_core::skills::install_fetched::install_fetched_skill_and_lock(
				request,
			)
			.expect("install should succeed");

		assert_eq!(
			report.agent_results.len(),
			valid.len(),
			"result count mismatch"
		);
		for ((agent_str, agent_type), agent_result) in
			valid.iter().zip(&report.agent_results)
		{
			assert_eq!(
				agent_result.agent, *agent_type,
				"agent result {:?} does not match label {}",
				agent_result.agent, agent_str
			);

			let expected_dir = resolve_git_install_target_dir(
				*agent_type,
				resource_scope,
				Some(&project_root),
			)
			.expect("dir must resolve");
			let installed_path = expected_dir.join("hello-skill/SKILL.md");
			let master_path =
				project_root.join(".agents/skills/hello-skill/SKILL.md");
			if agent_result.error.is_none() {
				assert!(
					installed_path.exists() || master_path.exists(),
					"skill not found at {} or {} for agent {}",
					installed_path.display(),
					master_path.display(),
					agent_str,
				);
			}

			let parsed: AgentType = agent_str.parse().unwrap();
			assert_eq!(
				parsed, *agent_type,
				"agent label '{}' does not match type {:?}",
				agent_str, agent_type
			);
		}
	}

	// ─── Ticket 08: desktop scan/install partial-fetch + session slimming ─────
	//
	// The desktop scan browses via `SkillRepository::list` (no whole-repo clone
	// on the github path) and installs via `fetch` (only the selected skill),
	// with the scan session pinning the resolved commit. These tests drive that
	// contract through a request-RECORDING GitHub REST transport (mirrors the
	// T06/T07 seam) so we can assert what was and was NOT downloaded — with no
	// network. Unix-gated because install materializes symlink-only masters.
	#[cfg(unix)]
	mod t08_desktop_partial_fetch {
		use std::collections::{BTreeSet, HashMap};
		use std::sync::atomic::{AtomicBool, Ordering};
		use std::sync::{Arc, Mutex};

		use aghub_git::{
			GitError, GithubRest, HttpRequest, HttpResponse, HttpTransport,
			RepoFetchBackend,
		};
		use base64::Engine as _;
		use rocket::http::{Header, Status};
		use rocket::local::blocking::Client;
		use rocket::Config;
		use skill_update::{SkillRepository, SourceRef};
		use tempfile::tempdir;

		use super::block_on;
		use super::with_isolated_env;
		use crate::dto::skill::InstallSkillRequest;
		use crate::routes::skills::{
			install_skill_with_repo, scan_repo_catalog,
		};
		use crate::source_sessions::{
			PinnedSourceSession, PinnedSourceSessions,
		};
		use crate::state::SkillRepositoryFactory;

		// ── Request-recording transport seam ──
		struct RecordingTransport<F> {
			responder: F,
			recorded: Arc<Mutex<Vec<HttpRequest>>>,
		}
		impl<F> HttpTransport for RecordingTransport<F>
		where
			F: Fn(&HttpRequest) -> Result<HttpResponse, GitError> + Send + Sync,
		{
			fn execute(
				&self,
				request: HttpRequest,
			) -> Result<HttpResponse, GitError> {
				self.recorded.lock().unwrap().push(request.clone());
				(self.responder)(&request)
			}
		}
		fn record_transport(
			responder: impl Fn(&HttpRequest) -> Result<HttpResponse, GitError>
				+ Send
				+ Sync
				+ 'static,
		) -> (Arc<dyn HttpTransport>, Arc<Mutex<Vec<HttpRequest>>>) {
			let recorded = Arc::new(Mutex::new(Vec::new()));
			let t: Arc<dyn HttpTransport> = Arc::new(RecordingTransport {
				responder,
				recorded: recorded.clone(),
			});
			(t, recorded)
		}

		fn json_ok(body: impl Into<Vec<u8>>) -> HttpResponse {
			HttpResponse {
				status: 200,
				headers: vec![(
					"content-type".into(),
					"application/json; charset=utf-8".into(),
				)],
				body: body.into(),
			}
		}
		fn raw_ok(bytes: impl Into<Vec<u8>>) -> HttpResponse {
			HttpResponse {
				status: 200,
				headers: vec![(
					"content-type".into(),
					"application/vnd.github.raw".into(),
				)],
				body: bytes.into(),
			}
		}
		fn resp_status(code: u16) -> HttpResponse {
			HttpResponse {
				status: code,
				headers: Vec::new(),
				body: Vec::new(),
			}
		}
		fn strip_query(u: &str) -> &str {
			u.split('?').next().unwrap_or(u)
		}
		fn is_commit_resolve(u: &str) -> bool {
			u.contains("/commits/")
		}
		fn is_tree(u: &str) -> bool {
			u.contains("/git/trees/")
		}
		fn blob_oid(u: &str) -> Option<String> {
			strip_query(u)
				.split("/git/blobs/")
				.nth(1)
				.map(|s| s.trim_end_matches('/').to_string())
		}

		fn github_source() -> SourceRef {
			SourceRef {
				source: "https://github.com/acme/skills.git".into(),
				ref_: Some("main".into()),
			}
		}

		// A gix-slot backend that must NEVER be consulted — every test here is
		// on the github REST path, so any call is a routing bug.
		struct NoGixBackend;
		impl RepoFetchBackend for NoGixBackend {
			fn resolve(
				&self,
				_s: &aghub_git::SourceRef,
				_a: Option<&aghub_git::Credentials>,
			) -> aghub_git::Result<aghub_git::RepoSnapshot> {
				unreachable!("gix slot must not run on the github REST path");
			}
			fn read_tree(
				&self,
				_s: &aghub_git::RepoSnapshot,
			) -> aghub_git::Result<aghub_git::RepoTree> {
				unreachable!("gix slot must not run on the github REST path");
			}
			fn read_blobs(
				&self,
				_s: &aghub_git::RepoSnapshot,
				_o: &[String],
			) -> aghub_git::Result<Vec<aghub_git::Blob>> {
				unreachable!("gix slot must not run on the github REST path");
			}
			fn materialize(
				&self,
				_s: &aghub_git::RepoSnapshot,
				_p: &[&str],
				_d: &std::path::Path,
			) -> aghub_git::Result<()> {
				unreachable!("gix slot must not run on the github REST path");
			}
		}

		// ── A canned repo: two skills + unrelated large / support blobs ──
		const COMMIT_OID: &str = "1111111111111111111111111111111111111111";
		const TREE_OID: &str = "2222222222222222222222222222222222222222";
		const OID_MUSIC_SKILL: &str =
			"3333333333333333333333333333333333333333";
		const OID_MUSIC_RUN: &str = "4444444444444444444444444444444444444444";
		const OID_OTHER_SKILL: &str =
			"6666666666666666666666666666666666666666";
		const OID_OTHER_BIG: &str = "7777777777777777777777777777777777777777";
		const OID_README: &str = "8888888888888888888888888888888888888888";

		const MUSIC_SKILL_BODY: &[u8] =
			b"---\nname: music\ndescription: a sub-folder skill\n---\n# body\n";
		const OTHER_SKILL_BODY: &[u8] =
			b"---\nname: other\ndescription: another skill\n---\n# other\n";
		const MUSIC_RUN_BODY: &[u8] = b"#!/bin/sh\necho hi\n";

		fn commit_json() -> String {
			format!(
				r#"{{"sha":"{COMMIT_OID}","commit":{{"tree":{{"sha":"{TREE_OID}"}},"committer":{{"date":"2026-07-17T00:00:00Z"}}}}}}"#
			)
		}
		fn tree_json() -> String {
			format!(
				r#"{{"sha":"{TREE_OID}","truncated":false,"tree":[
{{"path":"README.md","mode":"100644","type":"blob","sha":"{OID_README}","size":10}},
{{"path":"skills","mode":"040000","type":"tree","sha":"deadbeef00000000000000000000000000000001"}},
{{"path":"skills/music","mode":"040000","type":"tree","sha":"deadbeef00000000000000000000000000000002"}},
{{"path":"skills/music/SKILL.md","mode":"100644","type":"blob","sha":"{OID_MUSIC_SKILL}","size":56}},
{{"path":"skills/music/scripts","mode":"040000","type":"tree","sha":"deadbeef00000000000000000000000000000003"}},
{{"path":"skills/music/scripts/run.sh","mode":"100755","type":"blob","sha":"{OID_MUSIC_RUN}","size":18}},
{{"path":"skills/other","mode":"040000","type":"tree","sha":"deadbeef00000000000000000000000000000004"}},
{{"path":"skills/other/SKILL.md","mode":"100644","type":"blob","sha":"{OID_OTHER_SKILL}","size":56}},
{{"path":"skills/other/big.bin","mode":"100644","type":"blob","sha":"{OID_OTHER_BIG}","size":52428800}}
]}}"#
			)
		}
		fn blob_map() -> HashMap<String, Vec<u8>> {
			let mut m = HashMap::new();
			m.insert(OID_MUSIC_SKILL.to_string(), MUSIC_SKILL_BODY.to_vec());
			m.insert(OID_MUSIC_RUN.to_string(), MUSIC_RUN_BODY.to_vec());
			m.insert(OID_OTHER_SKILL.to_string(), OTHER_SKILL_BODY.to_vec());
			m.insert(OID_OTHER_BIG.to_string(), vec![b'x'; 1024]);
			m.insert(OID_README.to_string(), b"readme".to_vec());
			m
		}
		fn happy_responder(
		) -> impl Fn(&HttpRequest) -> Result<HttpResponse, GitError>
		       + Send
		       + Sync
		       + 'static {
			let commit = commit_json();
			let tree = tree_json();
			let blobs = blob_map();
			move |req: &HttpRequest| {
				let u = req.url.as_str();
				if let Some(oid) = blob_oid(u) {
					return match blobs.get(&oid) {
						Some(bytes) => Ok(raw_ok(bytes.clone())),
						None => Ok(resp_status(404)),
					};
				}
				if is_tree(u) {
					return Ok(json_ok(tree.clone().into_bytes()));
				}
				if is_commit_resolve(u) {
					return Ok(json_ok(commit.clone().into_bytes()));
				}
				Ok(resp_status(404))
			}
		}

		// ═══ Test 1: scan LISTS skills without downloading the whole repo ═════
		//
		// Drives the scan core `scan_repo_catalog`, which must resolve + `list`
		// through `SkillRepository`. It must download ONLY the catalog's
		// SKILL.md blobs — never the repo's other/large/support blobs. FAILS if
		// scan pulls the whole repo (the old full-clone behavior would touch
		// every blob).
		#[test]
		fn scan_lists_skills_without_whole_repo_download() {
			let (t, recorded) = record_transport(happy_responder());
			let rest: Arc<dyn RepoFetchBackend> = Arc::new(GithubRest::new(t));
			let repo = SkillRepository::with_backends(
				Some(rest),
				Arc::new(NoGixBackend),
			);

			let (snap, skills) =
				scan_repo_catalog(&repo, &github_source(), None)
					.expect("scan should list the catalog");

			// The scan pins the resolved COMMIT oid.
			assert_eq!(snap.commit_oid, COMMIT_OID, "scan pins the commit oid");

			// Both skills are listed, addressed by their repo-relative FOLDER.
			let paths: BTreeSet<String> =
				skills.iter().map(|s| s.path.clone()).collect();
			assert!(
				paths.contains("skills/music"),
				"music skill listed, got {paths:?}"
			);
			assert!(
				paths.contains("skills/other"),
				"other skill listed, got {paths:?}"
			);

			// No whole-repo download: only the catalog SKILL.md blobs were
			// fetched — never the unrelated / large / support blobs.
			let blobs: BTreeSet<String> = recorded
				.lock()
				.unwrap()
				.iter()
				.filter_map(|r| blob_oid(&r.url))
				.collect();
			assert!(
				blobs.contains(OID_MUSIC_SKILL),
				"the music SKILL.md is read for the catalog"
			);
			assert!(
				blobs.contains(OID_OTHER_SKILL),
				"the other SKILL.md is read for the catalog"
			);
			for unrelated in [OID_OTHER_BIG, OID_MUSIC_RUN, OID_README] {
				assert!(
					!blobs.contains(unrelated),
					"scan must NOT download the whole repo (blob {unrelated} \
					 was requested)"
				);
			}
		}

		fn install_body(session_id: &str) -> serde_json::Value {
			serde_json::json!({
				"session_id": session_id,
				"skill_paths": ["skills/music"],
				"agents": ["claude"],
				"scope": "global",
				"project_root": null,
			})
		}

		// ═══ Test 2: install FETCHES ONLY the selected skill ══════════════════
		//
		// After a scan, installing one selected skill through the real
		// `/skills/git/install` route must download ONLY that skill's blobs —
		// not the unselected skill, not the repo's large blob. FAILS if install
		// re-materializes the whole repo (the old cached-clone behavior).
		#[test]
		fn install_fetches_only_the_selected_skill() {
			with_isolated_env(|home, _state| {
				let (t, recorded) = record_transport(happy_responder());
				let rest: Arc<dyn RepoFetchBackend> =
					Arc::new(GithubRest::new(t));
				let repo = Arc::new(SkillRepository::with_backends(
					Some(rest),
					Arc::new(NoGixBackend),
				));
				let snap = repo
					.resolve(&github_source(), None)
					.expect("resolve pins the scanned commit");
				assert_eq!(snap.commit_oid, COMMIT_OID);

				let app_data = tempdir().unwrap();
				let client = Client::tracked(crate::build_rocket(
					Config::default(),
					app_data.path().to_path_buf(),
				))
				.expect("client");
				let sessions = client
					.rocket()
					.state::<PinnedSourceSessions>()
					.expect("git clone sessions");
				sessions.insert(
					"sess".to_string(),
					PinnedSourceSession::new(
						repo.clone(),
						snap.clone(),
						"https://github.com/acme/skills.git".to_string(),
						None,
						vec!["main".to_string()],
						"main".to_string(),
					),
				);
				// Only measure the traffic install itself issues.
				recorded.lock().unwrap().clear();

				let resp = client
					.post("/api/v1/skills/git/install")
					.json(&install_body("sess"))
					.dispatch();
				assert_eq!(resp.status(), Status::Ok);

				let blobs: BTreeSet<String> = recorded
					.lock()
					.unwrap()
					.iter()
					.filter_map(|r| blob_oid(&r.url))
					.collect();
				assert!(
					blobs.contains(OID_MUSIC_SKILL),
					"install fetched the selected skill's SKILL.md"
				);
				assert!(
					blobs.contains(OID_MUSIC_RUN),
					"install fetched the selected skill's support file"
				);
				assert!(
					!blobs.contains(OID_OTHER_SKILL),
					"install must NOT fetch the unselected skill"
				);
				assert!(
					!blobs.contains(OID_OTHER_BIG),
					"install must NOT fetch the unrelated large blob"
				);
				assert!(
					!blobs.contains(OID_README),
					"install must NOT fetch unrelated repo files"
				);

				assert!(
					home.join(".agents/skills/music/SKILL.md").exists(),
					"the selected skill materialized into the master"
				);
			});
		}

		// ── Advancing-branch fixture for the TOCTOU test ──
		const COMMIT_A: &str = "aaaa1111aaaa1111aaaa1111aaaa1111aaaa1111";
		const TREE_A: &str = "aaaa2222aaaa2222aaaa2222aaaa2222aaaa2222";
		const OID_MUSIC_A: &str = "aaaa3333aaaa3333aaaa3333aaaa3333aaaa3333";
		const COMMIT_B: &str = "bbbb1111bbbb1111bbbb1111bbbb1111bbbb1111";
		const TREE_B: &str = "bbbb2222bbbb2222bbbb2222bbbb2222bbbb2222";
		const OID_MUSIC_B: &str = "bbbb3333bbbb3333bbbb3333bbbb3333bbbb3333";

		fn advancing_responder(
			advanced: Arc<AtomicBool>,
		) -> impl Fn(&HttpRequest) -> Result<HttpResponse, GitError>
		       + Send
		       + Sync
		       + 'static {
			move |req: &HttpRequest| {
				let u = req.url.as_str();
				if let Some(oid) = blob_oid(u) {
					let body: &[u8] = if oid == OID_MUSIC_A {
						b"---\nname: music\ndescription: A version\n---\n# a\n"
					} else if oid == OID_MUSIC_B {
						b"---\nname: music\ndescription: B version\n---\n# b\n"
					} else {
						return Ok(resp_status(404));
					};
					return Ok(raw_ok(body.to_vec()));
				}
				if is_tree(u) {
					let (tree_oid, music) = if u.contains(TREE_B) {
						(TREE_B, OID_MUSIC_B)
					} else {
						(TREE_A, OID_MUSIC_A)
					};
					return Ok(json_ok(
						format!(
							r#"{{"sha":"{tree_oid}","truncated":false,"tree":[
{{"path":"skills/music/SKILL.md","mode":"100644","type":"blob","sha":"{music}","size":44}}
]}}"#
						)
						.into_bytes(),
					));
				}
				if is_commit_resolve(u) {
					let (commit, tree) = if advanced.load(Ordering::SeqCst) {
						(COMMIT_B, TREE_B)
					} else {
						(COMMIT_A, TREE_A)
					};
					return Ok(json_ok(
						format!(
							r#"{{"sha":"{commit}","commit":{{"tree":{{"sha":"{tree}"}},"committer":{{"date":"2026-07-17T00:00:00Z"}}}}}}"#
						)
						.into_bytes(),
					));
				}
				Ok(resp_status(404))
			}
		}

		// ═══ Test 3 (crux): install pins the SCANNED commit under TOCTOU ══════
		//
		// The branch tip advances between scan (pinned COMMIT_A) and install. The
		// install route must fetch and record the PINNED commit — never the moved
		// tip. FAILS if install re-resolves the branch (it would fetch COMMIT_B /
		// TREE_B and record COMMIT_B, or leave refCommit unset via a HEAD read).
		#[test]
		fn install_pins_scanned_commit_when_branch_advances() {
			with_isolated_env(|home, _state| {
				let advanced = Arc::new(AtomicBool::new(false));
				let (t, recorded) =
					record_transport(advancing_responder(advanced.clone()));
				let rest: Arc<dyn RepoFetchBackend> =
					Arc::new(GithubRest::new(t));
				let repo = Arc::new(SkillRepository::with_backends(
					Some(rest),
					Arc::new(NoGixBackend),
				));

				// Scan pins COMMIT_A / TREE_A.
				let snap = repo
					.resolve(&github_source(), None)
					.expect("resolve pins the scanned commit");
				assert_eq!(snap.commit_oid, COMMIT_A);
				assert_eq!(snap.tree_oid, TREE_A);

				// Branch advances AFTER the scan pinned COMMIT_A.
				advanced.store(true, Ordering::SeqCst);

				let app_data = tempdir().unwrap();
				let client = Client::tracked(crate::build_rocket(
					Config::default(),
					app_data.path().to_path_buf(),
				))
				.expect("client");
				let sessions = client
					.rocket()
					.state::<PinnedSourceSessions>()
					.expect("git clone sessions");
				sessions.insert(
					"sess".to_string(),
					PinnedSourceSession::new(
						repo.clone(),
						snap.clone(),
						"https://github.com/acme/skills.git".to_string(),
						None,
						vec!["main".to_string()],
						"main".to_string(),
					),
				);
				recorded.lock().unwrap().clear();

				let resp = client
					.post("/api/v1/skills/git/install")
					.json(&install_body("sess"))
					.dispatch();
				assert_eq!(resp.status(), Status::Ok);

				// The lock records the PINNED commit — not the moved tip.
				let entry = skill::lock::global::get_skill_from_lock("music")
					.expect("music must be locked after install");
				assert_eq!(
					entry.ref_commit.as_deref(),
					Some(COMMIT_A),
					"the lock must record the SCANNED commit, not the tip"
				);
				assert_ne!(
					entry.ref_commit.as_deref(),
					Some(COMMIT_B),
					"the moved tip must never reach the lock"
				);

				// Install must NOT re-resolve the moving ref, and must read the
				// pinned tree — never the moved tip's tree.
				let reqs = recorded.lock().unwrap();
				assert!(
					reqs.iter().all(|r| !is_commit_resolve(&r.url)),
					"install must not re-resolve the branch tip"
				);
				assert!(
					reqs.iter().any(|r| r.url.contains(TREE_A)),
					"install must read the pinned tree oid"
				);
				assert!(
					reqs.iter().all(|r| !r.url.contains(TREE_B)),
					"install must never read the moved tip's tree"
				);
				drop(reqs);

				// The installed content is the pinned commit's version.
				let body = std::fs::read_to_string(
					home.join(".agents/skills/music/SKILL.md"),
				)
				.expect("master SKILL.md present");
				assert!(
					body.contains("A version"),
					"install materialized the pinned commit's content, got: {body}"
				);
			});
		}

		// ═══ Test 4: POST /skills/install fetches ONLY the NAMED skill ════════
		//
		// `/skills/install` takes a source + skill NAMES (no session). The rewire
		// resolves ONE snapshot, `list`s to map names→SkillPaths, then fetches
		// ONLY the selected skill's folder — never a whole-repo clone. Driving the
		// extracted core with a request-recording REST transport, installing the
		// single named "music" skill must download that skill's content blobs plus
		// the catalog SKILL.md blobs (`list` reads every SKILL.md to resolve
		// names), but NEVER the unrelated large blob or unrelated repo files that
		// the old full-clone `install_skill` pulled down. FAILS if install
		// over-fetches (full clone / all-skill content).
		#[test]
		fn install_skill_core_fetches_only_the_named_skill() {
			with_isolated_env(|home, _state| {
				let (t, recorded) = record_transport(happy_responder());
				let rest: Arc<dyn RepoFetchBackend> =
					Arc::new(GithubRest::new(t));
				let repo = Arc::new(SkillRepository::with_backends(
					Some(rest),
					Arc::new(NoGixBackend),
				));

				let req = InstallSkillRequest {
					source: "https://github.com/acme/skills.git".to_string(),
					agents: vec!["claude".to_string()],
					skills: vec!["music".to_string()],
					scope: "global".to_string(),
					project_path: None,
					install_all: Some(false),
				};
				let resp = block_on(install_skill_with_repo(req, repo, None))
					.ok()
					.expect("install handler ok")
					.into_inner();
				assert!(
					resp.success,
					"install succeeded, rows: {:?}",
					resp.agents
				);

				let reqs = recorded.lock().unwrap();
				// Resolve one snapshot (commit) + read its tree via `list`.
				assert!(
					reqs.iter().any(|r| is_commit_resolve(&r.url)),
					"the commit is resolved once"
				);
				assert!(
					reqs.iter().any(|r| is_tree(&r.url)),
					"the tree is read to build the catalog"
				);
				let blobs: BTreeSet<String> =
					reqs.iter().filter_map(|r| blob_oid(&r.url)).collect();
				drop(reqs);

				// The selected skill's own blobs ARE fetched.
				assert!(
					blobs.contains(OID_MUSIC_SKILL),
					"install fetched the selected skill's SKILL.md"
				);
				assert!(
					blobs.contains(OID_MUSIC_RUN),
					"install fetched the selected skill's support file"
				);
				// The whole repo is NOT pulled: the unrelated large blob and
				// unrelated repo files are never requested (the old full-clone
				// install_skill would have pulled every blob).
				assert!(
					!blobs.contains(OID_OTHER_BIG),
					"install must NOT fetch the unrelated large blob"
				);
				assert!(
					!blobs.contains(OID_README),
					"install must NOT fetch unrelated repo files"
				);

				assert!(
					home.join(".agents/skills/music/SKILL.md").exists(),
					"the named skill materialized into the master"
				);
			});
		}

		#[test]
		fn install_skill_http_preflights_mixed_agents_before_writing() {
			with_isolated_env(|home, _state| {
				let project = home.join("project");
				std::fs::create_dir_all(&project).unwrap();
				let (transport, _recorded) =
					record_transport(happy_responder());
				let rest: Arc<dyn RepoFetchBackend> =
					Arc::new(GithubRest::new(transport));
				let repo = Arc::new(SkillRepository::with_backends(
					Some(rest),
					Arc::new(NoGixBackend),
				));
				let app_data = tempdir().unwrap();
				let rocket = crate::build_rocket_with_skill_repository_factory(
					Config::default(),
					app_data.path().to_path_buf(),
					SkillRepositoryFactory::fixed(repo),
				);
				let client = Client::tracked(rocket).unwrap();
				let forwarded = serde_json::json!({
					"https://github.com/acme/skills.git": {
						"token": "forwarded-token",
						"origin": null
					}
				});
				let encoded = base64::engine::general_purpose::STANDARD
					.encode(serde_json::to_vec(&forwarded).unwrap());

				let response = client
					.post("/api/v1/skills/install")
					.header(Header::new("X-Aghub-Git-Tokens", encoded))
					.json(&serde_json::json!({
						"source": "https://github.com/acme/skills.git",
						"agents": ["claude", "augmentcode"],
						"skills": ["music"],
						"scope": "project",
						"project_path": project.display().to_string(),
						"install_all": false
					}))
					.dispatch();

				assert_eq!(response.status(), Status::Ok);
				let body: serde_json::Value = serde_json::from_str(
					&response.into_string().expect("response body"),
				)
				.expect("json response");
				assert_eq!(body["success"], false);
				assert_eq!(body["agents"].as_array().unwrap().len(), 2);
				assert!(
					!project.join(".agents/skills/music").exists(),
					"capability preflight must happen before the Master write",
				);
				assert!(
					!project.join(".claude/skills/music").exists(),
					"the supported target must not receive a Referrer",
				);
			});
		}

		#[test]
		fn install_skill_uses_forwarded_token_on_first_rest_request() {
			with_isolated_env(|_home, _state| {
				let first = Arc::new(AtomicBool::new(true));
				let first_request = first.clone();
				let responder = happy_responder();
				let (t, _recorded) = record_transport(move |req| {
					if first_request.swap(false, Ordering::SeqCst) {
						assert!(
							req.headers.iter().any(|(name, value)| {
								name.eq_ignore_ascii_case("authorization")
									&& value == "Bearer forwarded-token"
							}),
							"the route's first REST request must carry the forwarded token"
						);
					}
					responder(req)
				});
				let rest: Arc<dyn RepoFetchBackend> =
					Arc::new(GithubRest::new(t));
				let repo = Arc::new(SkillRepository::with_backends(
					Some(rest),
					Arc::new(NoGixBackend),
				));
				let app_data = tempdir().unwrap();
				let rocket = crate::build_rocket_with_skill_repository_factory(
					Config::default(),
					app_data.path().to_path_buf(),
					SkillRepositoryFactory::fixed(repo),
				);
				let client = Client::tracked(rocket).unwrap();
				let forwarded = serde_json::json!({
					"https://github.com/acme/skills.git": {
						"token": "forwarded-token",
						"origin": null
					}
				});
				let encoded = base64::engine::general_purpose::STANDARD
					.encode(serde_json::to_vec(&forwarded).unwrap());
				let response = client
					.post("/api/v1/skills/install")
					.header(Header::new("X-Aghub-Git-Tokens", encoded))
					.json(&serde_json::json!({
						"source": "https://github.com/acme/skills.git",
						"agents": ["claude"],
						"skills": ["music"],
						"scope": "global",
						"install_all": false
					}))
					.dispatch();

				assert!(
					response.status() == Status::Ok,
					"token-authenticated install must succeed"
				);
				assert!(!first.load(Ordering::SeqCst), "REST was never called");
			});
		}

		// ══════════════════════════════════════════════════════════════════
		// Ticket 09 — cross-cutting integration (the ASSEMBLED feature holds).
		//
		// Reuse the T06/T07/T08 request-recording REST seam + fake backends and
		// assert what NO single earlier ticket covered: the two install surfaces
		// agree (cross-surface consistency), the REST path's lock hash + byte
		// shape equal a clone's (round-trip parity, incl. the symlink
		// npx-lstat-skip value), and a RestFallback install equals the
		// REST/clone install (fallback equivalence). All network-free; unix-gated
		// (this module already is: symlink staging + Master materialization).
		// ══════════════════════════════════════════════════════════════════

		/// Canned REST repo derived from a real on-disk folder: commit + tree
		/// JSON plus a blob map keyed by SYNTHETIC oids (opaque to `GithubRest`).
		struct RestFixture {
			commit: String,
			tree: String,
			blobs: HashMap<String, Vec<u8>>,
		}

		fn hex_bytes(bytes: &[u8]) -> String {
			use std::fmt::Write;
			bytes.iter().fold(String::new(), |mut s, b| {
				let _ = write!(s, "{b:02x}");
				s
			})
		}

		/// Recursively turn `root`'s files/symlinks into GitHub trees-API entries
		/// (full repo-relative paths, git modes) + a blob map. Directory entries
		/// are omitted — `read_tree` skips `type == "tree"`. A symlink's blob is
		/// its raw target (git semantics), so staging recreates it as a symlink.
		fn collect_fixture_entries(
			root: &std::path::Path,
			dir: &std::path::Path,
			entries: &mut Vec<String>,
			blobs: &mut HashMap<String, Vec<u8>>,
			counter: &mut u64,
		) {
			let mut kids: Vec<_> = std::fs::read_dir(dir)
				.unwrap()
				.map(|e| e.unwrap())
				.collect();
			kids.sort_by_key(|e| e.file_name());
			for e in kids {
				let p = e.path();
				let rel = p
					.strip_prefix(root)
					.unwrap()
					.to_string_lossy()
					.replace('\\', "/");
				let ft = std::fs::symlink_metadata(&p).unwrap().file_type();
				if ft.is_dir() {
					collect_fixture_entries(root, &p, entries, blobs, counter);
					continue;
				}
				*counter += 1;
				let oid = format!("{counter:040x}");
				if ft.is_symlink() {
					let target = std::fs::read_link(&p).unwrap();
					let bytes = target.to_string_lossy().as_bytes().to_vec();
					entries.push(format!(
						r#"{{"path":"{rel}","mode":"120000","type":"blob","sha":"{oid}","size":{}}}"#,
						bytes.len()
					));
					blobs.insert(oid, bytes);
				} else {
					let bytes = std::fs::read(&p).unwrap();
					let exec = {
						use std::os::unix::fs::PermissionsExt;
						std::fs::metadata(&p).unwrap().permissions().mode()
							& 0o111 != 0
					};
					let mode = if exec { "100755" } else { "100644" };
					entries.push(format!(
						r#"{{"path":"{rel}","mode":"{mode}","type":"blob","sha":"{oid}","size":{}}}"#,
						bytes.len()
					));
					blobs.insert(oid, bytes);
				}
			}
		}

		fn rest_fixture(
			root: &std::path::Path,
			commit_oid: &str,
			tree_oid: &str,
		) -> RestFixture {
			let mut entries = Vec::new();
			let mut blobs = HashMap::new();
			let mut counter = 0u64;
			collect_fixture_entries(
				root,
				root,
				&mut entries,
				&mut blobs,
				&mut counter,
			);
			let tree = format!(
				r#"{{"sha":"{tree_oid}","truncated":false,"tree":[{}]}}"#,
				entries.join(",")
			);
			let commit = format!(
				r#"{{"sha":"{commit_oid}","commit":{{"tree":{{"sha":"{tree_oid}"}},"committer":{{"date":"2026-07-17T00:00:00Z"}}}}}}"#
			);
			RestFixture {
				commit,
				tree,
				blobs,
			}
		}

		fn fixture_responder(
			fx: RestFixture,
		) -> impl Fn(&HttpRequest) -> Result<HttpResponse, GitError>
		       + Send
		       + Sync
		       + 'static {
			move |req: &HttpRequest| {
				let u = req.url.as_str();
				if let Some(oid) = blob_oid(u) {
					return match fx.blobs.get(&oid) {
						Some(b) => Ok(raw_ok(b.clone())),
						None => Ok(resp_status(404)),
					};
				}
				if is_tree(u) {
					return Ok(json_ok(fx.tree.clone().into_bytes()));
				}
				if is_commit_resolve(u) {
					return Ok(json_ok(fx.commit.clone().into_bytes()));
				}
				Ok(resp_status(404))
			}
		}

		/// A rest-slot backend that always signals `RestFallback` — the single
		/// error every transient REST condition (rate-limit / 401 / network)
		/// collapses to. Forces the `SkillRepository`'s single fallback owner to
		/// route to the gix slot. Mirrors T07's `AlwaysFallbackRest`.
		struct AlwaysFallbackRest;
		impl RepoFetchBackend for AlwaysFallbackRest {
			fn resolve(
				&self,
				_s: &aghub_git::SourceRef,
				_a: Option<&aghub_git::Credentials>,
			) -> aghub_git::Result<aghub_git::RepoSnapshot> {
				Err(GitError::rest_fallback("rate limited"))
			}
			fn read_tree(
				&self,
				_s: &aghub_git::RepoSnapshot,
			) -> aghub_git::Result<aghub_git::RepoTree> {
				Err(GitError::rest_fallback("rate limited"))
			}
			fn read_blobs(
				&self,
				_s: &aghub_git::RepoSnapshot,
				_o: &[String],
			) -> aghub_git::Result<Vec<aghub_git::Blob>> {
				Err(GitError::rest_fallback("rate limited"))
			}
			fn materialize(
				&self,
				_s: &aghub_git::RepoSnapshot,
				_p: &[&str],
				_d: &std::path::Path,
			) -> aghub_git::Result<()> {
				Err(GitError::rest_fallback("rate limited"))
			}
		}

		/// Byte snapshot of a materialized folder (rel-path -> content + exec bit,
		/// symlink target for links) for a "same Master bytes" comparison.
		fn dir_snapshot(
			root: &std::path::Path,
		) -> std::collections::BTreeMap<String, String> {
			fn walk(
				root: &std::path::Path,
				dir: &std::path::Path,
				out: &mut std::collections::BTreeMap<String, String>,
			) {
				for e in std::fs::read_dir(dir).unwrap() {
					let p = e.unwrap().path();
					let rel = p
						.strip_prefix(root)
						.unwrap()
						.to_string_lossy()
						.replace('\\', "/");
					let ft = std::fs::symlink_metadata(&p).unwrap().file_type();
					if ft.is_symlink() {
						let t = std::fs::read_link(&p).unwrap();
						out.insert(rel, format!("symlink:{}", t.display()));
					} else if ft.is_dir() {
						walk(root, &p, out);
					} else {
						use std::os::unix::fs::PermissionsExt;
						let bytes = std::fs::read(&p).unwrap();
						let exec =
							std::fs::metadata(&p).unwrap().permissions().mode()
								& 0o111 != 0;
						out.insert(
							rel,
							format!("file:exec={exec}:{}", hex_bytes(&bytes)),
						);
					}
				}
			}
			let mut out = std::collections::BTreeMap::new();
			walk(root, root, &mut out);
			out
		}

		// ═══ T09.1: cross-surface consistency ════════════════════════════════
		//
		// The SAME skill (skills/music) from the SAME canned snapshot installed
		// via the two production cores — POST /skills/install (by NAME) and the
		// desktop /skills/git/install (by PATH), both through an injected
		// recording GithubRest — MUST write an identical lock entry and Master.
		// FAILS if the surfaces drift (skillPath / hash / refCommit / source, or
		// a divergent Master).
		#[test]
		fn cross_surface_install_yields_identical_lock_and_master() {
			// Surface A: install_skill_with_repo, selecting by skill NAME.
			let (entry_a, master_a) = with_isolated_env(|home, _state| {
				let (t, _rec) = record_transport(happy_responder());
				let rest: Arc<dyn RepoFetchBackend> =
					Arc::new(GithubRest::new(t));
				let repo = Arc::new(SkillRepository::with_backends(
					Some(rest),
					Arc::new(NoGixBackend),
				));
				let project = home.join("proj-a");
				std::fs::create_dir_all(&project).unwrap();
				let req = InstallSkillRequest {
					source: "https://github.com/acme/skills.git".to_string(),
					agents: vec!["claude".to_string()],
					skills: vec!["music".to_string()],
					scope: "project".to_string(),
					project_path: Some(project.display().to_string()),
					install_all: Some(false),
				};
				let resp = block_on(install_skill_with_repo(req, repo, None))
					.ok()
					.expect("surface A handler ok")
					.into_inner();
				assert!(resp.success, "surface A install: {:?}", resp.agents);
				let lock = skill::lock::local::read_local_lock(Some(&project));
				let entry = lock.skills.get("music").expect("A: music").clone();
				let master =
					dir_snapshot(&project.join(".agents/skills/music"));
				(entry, master)
			});

			// Surface B: desktop /skills/git/install, selecting by skill PATH.
			// current_branch is empty so ref_name is None (== surface A), making
			// the whole lock entry directly comparable.
			let (entry_b, master_b) = with_isolated_env(|home, _state| {
				let (t, _rec) = record_transport(happy_responder());
				let rest: Arc<dyn RepoFetchBackend> =
					Arc::new(GithubRest::new(t));
				let repo = Arc::new(SkillRepository::with_backends(
					Some(rest),
					Arc::new(NoGixBackend),
				));
				let snap =
					repo.resolve(&github_source(), None).expect("resolve");
				assert_eq!(snap.commit_oid, COMMIT_OID);
				let project = home.join("proj-b");
				std::fs::create_dir_all(&project).unwrap();
				let app_data = tempdir().unwrap();
				let client = Client::tracked(crate::build_rocket(
					Config::default(),
					app_data.path().to_path_buf(),
				))
				.expect("client");
				let sessions = client
					.rocket()
					.state::<PinnedSourceSessions>()
					.expect("git clone sessions");
				sessions.insert(
					"sess".to_string(),
					PinnedSourceSession::new(
						repo.clone(),
						snap.clone(),
						"https://github.com/acme/skills.git".to_string(),
						None,
						vec![],
						String::new(),
					),
				);
				let resp = client
					.post("/api/v1/skills/git/install")
					.json(&serde_json::json!({
						"session_id": "sess",
						"skill_paths": ["skills/music"],
						"agents": ["claude"],
						"scope": "project",
						"project_root": project.display().to_string(),
					}))
					.dispatch();
				assert_eq!(resp.status(), Status::Ok);
				let lock = skill::lock::local::read_local_lock(Some(&project));
				let entry = lock.skills.get("music").expect("B: music").clone();
				let master =
					dir_snapshot(&project.join(".agents/skills/music"));
				(entry, master)
			});

			// Identical lock entry (project lock carries no timestamps): source,
			// sourceType, skillPath, computedHash, refCommit, ref all match.
			assert_eq!(
				serde_json::to_value(&entry_a).unwrap(),
				serde_json::to_value(&entry_b).unwrap(),
				"the two install surfaces must write an identical lock entry"
			);
			// And an identical Master (byte-for-byte, exec bits included).
			assert!(!master_a.is_empty(), "master A must be non-empty");
			assert_eq!(
				master_a, master_b,
				"the two surfaces must materialize an identical Master"
			);
		}

		// ═══ T09.2: round-trip lock parity (hash + byte shape, incl. symlink) ══
		//
		// A symlink-bearing skill fetched via the REST path (GithubRest fed
		// canned tree+blobs derived from a real on-disk folder) must write a lock
		// whose computedHash equals `compute_skill_folder_hash` of that folder —
		// the npx-parity anchor and the value a gix clone yields (T04 proves
		// stage==clone byte+hash INCL. the symlink). The in-folder symlink must
		// NOT change the hash (npx-lstat-skip), and the lock entry must be
		// byte-shaped exactly like the copy-era fixture (prior art:
		// install_lock_entry_byte_identical_to_copy_era_fixture).
		#[test]
		fn rest_install_lock_hash_and_shape_match_a_clone() {
			const RT_COMMIT: &str = "cccccccccccccccccccccccccccccccccccccccc";
			const RT_TREE: &str = "dddddddddddddddddddddddddddddddddddddddd";

			// Reference skill folder WITH an in-folder symlink.
			let content = tempdir().unwrap();
			let skill_dir = content.path().join("skills/roundtrip");
			std::fs::create_dir_all(&skill_dir).unwrap();
			let skill_md =
				"---\nname: roundtrip\ndescription: rt\n---\n# body\n";
			std::fs::write(skill_dir.join("SKILL.md"), skill_md).unwrap();
			std::fs::write(skill_dir.join("ref.md"), b"reference notes\n")
				.unwrap();
			std::os::unix::fs::symlink("SKILL.md", skill_dir.join("link.md"))
				.unwrap();

			// Clone / npx-parity anchor: hash the folder directly (symlink
			// skipped by the Source hash).
			let clone_hash =
				skill::compute_skill_folder_hash(&skill_dir).unwrap();
			assert_ne!(clone_hash, skill::hash::EMPTY_SKILLS_LOCK_DIGEST);

			// Guard: the in-folder symlink contributes NOTHING to the hash
			// (npx-lstat-skip). A folder without it hashes identically; the old
			// materialize_tree value (symlink dereferenced into content) would
			// NOT — so this pins the corrected value.
			let no_link = tempdir().unwrap();
			let nl = no_link.path().join("skills/roundtrip");
			std::fs::create_dir_all(&nl).unwrap();
			std::fs::write(nl.join("SKILL.md"), skill_md).unwrap();
			std::fs::write(nl.join("ref.md"), b"reference notes\n").unwrap();
			assert_eq!(
				skill::compute_skill_folder_hash(&nl).unwrap(),
				clone_hash,
				"the in-folder symlink must be skipped by the Source hash"
			);

			let fx = rest_fixture(content.path(), RT_COMMIT, RT_TREE);
			let (t, _rec) = record_transport(fixture_responder(fx));
			let rest: Arc<dyn RepoFetchBackend> = Arc::new(GithubRest::new(t));
			let repo = Arc::new(SkillRepository::with_backends(
				Some(rest),
				Arc::new(NoGixBackend),
			));

			let entry = with_isolated_env(|home, _state| {
				let project = home.join("proj");
				std::fs::create_dir_all(&project).unwrap();
				let req = InstallSkillRequest {
					source: "https://github.com/acme/roundtrip.git".to_string(),
					agents: vec!["claude".to_string()],
					skills: vec!["roundtrip".to_string()],
					scope: "project".to_string(),
					project_path: Some(project.display().to_string()),
					install_all: Some(false),
				};
				let resp = block_on(install_skill_with_repo(req, repo, None))
					.ok()
					.expect("install handler ok")
					.into_inner();
				assert!(resp.success, "REST install: {:?}", resp.agents);
				skill::lock::local::read_local_lock(Some(&project))
					.skills
					.get("roundtrip")
					.expect("roundtrip locked")
					.clone()
			});

			// REST-path lock hash equals the clone / npx-parity value.
			assert_eq!(
				entry.computed_hash, clone_hash,
				"REST-materialized skill must hash like a clone (lstat-skip)"
			);

			// Byte-shape parity with the copy-era fixture: exactly these fields.
			let expected_source = aghub_git::resolve_remote_source(
				"https://github.com/acme/roundtrip.git",
			)
			.unwrap()
			.lock_source();
			assert_eq!(
				serde_json::to_value(&entry).unwrap(),
				serde_json::json!({
					"source": expected_source,
					"sourceType": "github",
					"skillPath": "skills/roundtrip/SKILL.md",
					"computedHash": clone_hash,
					"refCommit": RT_COMMIT,
				}),
				"REST lock entry must be byte-shaped like the copy-era fixture"
			);
		}

		// ═══ T09.3: fallback equivalence ═════════════════════════════════════
		//
		// The SAME content installed via (a) the REST path and (b) a RestFallback
		// -> gix route (rate-limited/non-github, modeled by AlwaysFallbackRest
		// routing to the gix slot that serves the same content) must produce an
		// IDENTICAL lock entry and Master. FAILS if the fallback path installs
		// anything different from the REST/clone result.
		#[test]
		fn rest_and_gix_fallback_install_are_identical() {
			// The gix-slot fake resolves to this fixed snapshot; the REST fixture
			// agrees so refCommit matches across the two paths.
			const FB_COMMIT: &str = "9999999999999999999999999999999999999999";
			const FB_TREE: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

			// Reference content: a normal skill folder (no symlink, so the
			// plain-copy gix fake and the REST staging produce byte-identical
			// source folders — a symlink would only be present/hashed on one).
			let content = tempdir().unwrap();
			let foo = content.path().join("skills/foo");
			std::fs::create_dir_all(&foo).unwrap();
			std::fs::write(
				foo.join("SKILL.md"),
				"---\nname: foo\ndescription: f\n---\n# foo\n",
			)
			.unwrap();
			std::fs::write(foo.join("ref.md"), b"support\n").unwrap();

			let install_via = |repo: Arc<SkillRepository>| -> (
				skill::LocalSkillLockEntry,
				std::collections::BTreeMap<String, String>,
			) {
				with_isolated_env(|home, _state| {
					let snap =
						repo.resolve(&github_source(), None).expect("resolve");
					assert_eq!(snap.commit_oid, FB_COMMIT);
					let project = home.join("proj");
					std::fs::create_dir_all(&project).unwrap();
					let app_data = tempdir().unwrap();
					let client = Client::tracked(crate::build_rocket(
						Config::default(),
						app_data.path().to_path_buf(),
					))
					.expect("client");
					let sessions = client
						.rocket()
						.state::<PinnedSourceSessions>()
						.expect("git clone sessions");
					sessions.insert(
						"sess".to_string(),
						PinnedSourceSession::new(
							repo.clone(),
							snap.clone(),
							"https://github.com/acme/skills.git".to_string(),
							None,
							vec!["main".to_string()],
							"main".to_string(),
						),
					);
					let resp = client
						.post("/api/v1/skills/git/install")
						.json(&serde_json::json!({
							"session_id": "sess",
							"skill_paths": ["skills/foo"],
							"agents": ["claude"],
							"scope": "project",
							"project_root": project.display().to_string(),
						}))
						.dispatch();
					assert_eq!(resp.status(), Status::Ok);
					let entry =
						skill::lock::local::read_local_lock(Some(&project))
							.skills
							.get("foo")
							.expect("foo locked")
							.clone();
					let master =
						dir_snapshot(&project.join(".agents/skills/foo"));
					(entry, master)
				})
			};

			// (a) REST path.
			let fx = rest_fixture(content.path(), FB_COMMIT, FB_TREE);
			let (t, _rec) = record_transport(fixture_responder(fx));
			let rest: Arc<dyn RepoFetchBackend> = Arc::new(GithubRest::new(t));
			let repo_rest = Arc::new(SkillRepository::with_backends(
				Some(rest),
				Arc::new(NoGixBackend),
			));
			let (entry_rest, master_rest) = install_via(repo_rest);

			// (b) RestFallback -> gix: REST always falls back; the gix slot
			// (SessionLocalBackend, resolving to FB_COMMIT) serves the same
			// content by copy.
			let gix: Arc<dyn RepoFetchBackend> =
				Arc::new(super::SessionLocalBackend::new(content.path()));
			let repo_fb = Arc::new(SkillRepository::with_backends(
				Some(Arc::new(AlwaysFallbackRest) as Arc<dyn RepoFetchBackend>),
				gix,
			));
			let (entry_fb, master_fb) = install_via(repo_fb);

			assert_eq!(
				serde_json::to_value(&entry_rest).unwrap(),
				serde_json::to_value(&entry_fb).unwrap(),
				"a RestFallback install must write the SAME lock entry"
			);
			assert_eq!(
				master_rest, master_fb,
				"a RestFallback install must materialize the SAME Master"
			);
		}
	}
}
