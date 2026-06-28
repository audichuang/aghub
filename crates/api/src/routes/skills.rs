use aghub_cc_plugins::claude::ClaudePluginManager;
use aghub_core::{
	create_adapter,
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
	dto::integrations::{
		CodeEditorType, EditSkillFolderRequest, OpenSkillFolderRequest,
	},
	dto::skill::{
		CreateSkillRequest, DeleteSkillByPathRequest,
		DeleteSkillByPathResponse, GitInstallRequest, GitInstallResponse,
		GitInstallResultEntry, GitScanRequest, GitScanResponse,
		GitScanSkillEntry, GitSyncRequest, GitSyncResponse,
		GlobalSkillLockResponse, InstallSkillRequest, InstallSkillResponse,
		LocalSkillLockEntryResponse, ProjectLockQuery,
		ProjectSkillLockResponse, PruneLockRequest, PruneLockResponse,
		SkillContentQuery, SkillLockEntryResponse, SkillResponse,
		SkillTreeNodeKind, SkillTreeNodeResponse, SkillTreeQuery,
		UpdateSkillRequest, ValidationError,
	},
	dto::transfer::{
		OperationBatchResponse, ReconcileRequest, TransferRequest,
	},
	error::{ApiCreated, ApiError, ApiResult},
	extractors::{AgentParam, ResolvedScope, ScopeParams},
	routes::{
		build_manager_from_resolved, require_writable_scope,
		resolved_to_resource_scope, skills_update::update_lock_hash,
	},
	skills::rename::{skill_renamed_message, SKILL_RENAMED_CODE},
	state::{GitCloneSession, GitCloneSessions},
};
use skill_update::{keychain_host_for_source, SourceCredentialStore};

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
			ApiError::new(
				Status::InternalServerError,
				format!("Branch listing task panicked: {e}"),
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
			return Ok(Json(crate::routes::removal_response(
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
		// Core-owned prune (same seam the manager's `remove_skill_planned`
		// uses). The copy branch only reaches here with a single writable
		// scope (`Both` is rejected upstream), so this is GlobalOnly or
		// ProjectOnly. The handler only RENDERS the returned PruneStatus.
		// DEFERRED: routing this whole by-path removal through a manager
		// planned-removal returning RemovalOutcome waits for the planned-
		// removal generalization (candidate #5 / Phase 3) — this route owns
		// API-layer path-containment that must not move into core.
		let prune = aghub_core::skills::prune::prune_lock_for_scope(
			resource_scope,
			project_root.as_deref(),
		);
		return Ok(Json(crate::routes::removal_response(
			aghub_core::skills::removal::RemovalOutcome {
				plan: executed_plan,
				executed: true,
				prune,
			},
		)));
	}

	match manager.remove_skill_planned(&skill_name, false, dry_run, confirm) {
		// remove_skill_planned prunes the per-scope lock itself on execute; the
		// caller must not prune again (unlike the non-link Copy branch above,
		// which bypasses the manager and therefore still prunes inline).
		Ok(outcome) => Ok(Json(crate::routes::removal_response(outcome))),
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

fn installed_skill_root(skill: &Skill) -> Option<PathBuf> {
	let raw = skill
		.canonical_path
		.as_deref()
		.or(skill.source_path.as_deref())?;
	Some(get_skill_root(expand_tilde_path(raw)))
}

fn installed_skill_roots(
	name: &str,
	resource_scope: ResourceScope,
	project_root: Option<&Path>,
) -> Vec<PathBuf> {
	let mut roots = Vec::new();
	for agent in load_all_agents(resource_scope, project_root) {
		for skill in agent.skills {
			if skill.name != name {
				continue;
			}
			let Some(root) = installed_skill_root(&skill) else {
				continue;
			};
			if !roots.contains(&root) {
				roots.push(root);
			}
		}
	}
	roots
}

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

#[post("/agents/<agent>/skills?<scope..>", data = "<body>")]
pub async fn create_skill(
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
	let mut response = SkillResponse::from(&skill);
	manager.add_skill(skill).map_err(ApiError::from)?;
	// Surface the advisory the CLI already shows: a NativeReader target gets the
	// `.agents` master only, with no per-agent link.
	response.native_reader = manager.skill_target_is_native_reader();
	Ok((Status::Created, Json(response)))
}

#[post("/agents/<agent>/skills/import?<scope..>", data = "<body>")]
pub fn import_skill(
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

	let mut response = SkillResponse::from(&imported);
	response.native_reader = manager.skill_target_is_native_reader();
	Ok(Json(response))
}

#[get("/agents/<agent>/skills/<name>?<scope..>")]
pub fn get_skill(
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
	agent: AgentParam,
	name: &str,
	params: DeleteSkillParams,
) -> ApiResult<DeleteSkillByPathResponse> {
	let resolved = params.resolve_scope()?;
	// project_root is unused: remove_skill_planned prunes the lock internally.
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
		// remove_skill_planned prunes the per-scope lock itself on execute, so
		// the handler must NOT prune again.
		Ok(outcome) => Ok(Json(crate::routes::removal_response(outcome))),
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

#[post("/skills/install", data = "<body>")]
pub async fn install_skill(
	body: Json<InstallSkillRequest>,
) -> ApiResult<InstallSkillResponse> {
	let req = body.into_inner();
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

	let (clone_url, lock_source, is_file_source) =
		match aghub_git::resolve_remote_source(&req.source) {
			Ok(source) => {
				let clone_url = source.clone_url.clone();
				let lock_source =
					install_lock_source_from_resolved(&source, None);
				(clone_url, lock_source, false)
			}
			Err(error) => {
				#[cfg(test)]
				{
					if let Some((clone_url, lock_source)) =
						file_install_source(&req.source)?
					{
						(clone_url, lock_source, true)
					} else {
						return Err(map_remote_source_error(error));
					}
				}
				#[cfg(not(test))]
				return Err(map_remote_source_error(error));
			}
		};

	let clone_url_for_task = clone_url.clone();
	let temp_dir = match timeout(
		Duration::from_secs(300),
		tokio::task::spawn_blocking(move || {
			clone_skill_source_to_temp(&clone_url_for_task, is_file_source)
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
			return Err(ApiError::new(
				Status::InternalServerError,
				format!("Clone task panicked: {e}"),
				"CLONE_ERROR",
			));
		}
		Err(_) => {
			return Err(ApiError::new(
				Status::RequestTimeout,
				"Skills installation timed out after 5 minutes".to_string(),
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

	// Resolve requested agents; unknown agents become per-agent failure rows.
	let mut invalid_rows: Vec<GitInstallResultEntry> = Vec::new();
	let mut target_agents: Vec<(String, AgentType)> = Vec::new();
	for agent_str in &req.agents {
		match agent_str.parse::<AgentType>() {
			Ok(a) => target_agents.push((agent_str.clone(), a)),
			Err(_) => invalid_rows.push(GitInstallResultEntry {
				name: String::new(),
				agent: agent_str.clone(),
				success: false,
				error: Some(format!("Unknown agent '{agent_str}'")),
			}),
		}
	}
	let agent_types: Vec<AgentType> =
		target_agents.iter().map(|(_, a)| *a).collect();

	let ref_commit = gix::open(temp_dir.path())
		.ok()
		.and_then(|repo| repo.head_id().ok().map(|id| id.detach()))
		.map(|oid| oid.to_string());

	let mut agent_rows: Vec<GitInstallResultEntry> = invalid_rows;
	let mut any_installed = false;
	for skill in &selected_skills {
		let request =
			aghub_core::skills::install_fetched::FetchedSkillInstallRequest {
				skill_file: &skill.full_path,
				source: &lock_source,
				lock_skill_path: skill::lock_skill_file_path(
					&skill.relative_dir,
				),
				ref_commit: ref_commit.clone(),
				scope: resource_scope,
				project_root: project_root.as_deref(),
				target_agents: &agent_types,
				expected_name: None,
				target: if matches!(resource_scope, ResourceScope::ProjectOnly)
				{
					aghub_core::skills::linker::LinkTarget::Relative
				} else {
					aghub_core::skills::linker::LinkTarget::Absolute
				},
			};
		match aghub_core::skills::install_fetched::install_fetched_skill_and_lock(
			request,
		) {
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
							skill.name.clone()
						},
						agent: agent_str.clone(),
						success,
						error: agent_result.error,
					});
				}
			}
			Err(e) => {
				let message = ApiError::from(e).body.error;
				for (agent_str, _) in &target_agents {
					agent_rows.push(GitInstallResultEntry {
						name: skill.name.clone(),
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
/// `removal::assert_targets_contained` distinguishes not-found targets.
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
pub fn get_skill_content(query: SkillContentQuery) -> ApiResult<String> {
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
pub fn get_global_skill_lock() -> ApiResult<GlobalSkillLockResponse> {
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

fn require_github_credential_url(url: &str) -> Result<(), ApiError> {
	let reject = || {
		ApiError::new(
			Status::BadRequest,
			"GitHub credentials can only be used with github.com HTTPS URLs",
			"INVALID_GITHUB_CREDENTIAL_URL",
		)
	};

	let parsed = url::Url::parse(url).map_err(|_| reject())?;

	let host = parsed.host_str().unwrap_or_default();
	if parsed.scheme() == "https"
		&& host.eq_ignore_ascii_case("github.com")
		&& parsed.port().is_none()
	{
		return Ok(());
	}

	Err(reject())
}

/// Returns true when both URLs parse and share the same (ASCII
/// case-insensitive) host. A parse failure or a missing host on either
/// side yields false.
///
/// Compares the HOST ONLY and is port-agnostic by design: session
/// credential pinning keys on host, not port, so the same host on a
/// different port is treated as the same host.
fn same_host(a: &str, b: &str) -> bool {
	let (Ok(a), Ok(b)) = (url::Url::parse(a), url::Url::parse(b)) else {
		return false;
	};
	match (a.host_str(), b.host_str()) {
		(Some(ha), Some(hb)) => ha.eq_ignore_ascii_case(hb),
		_ => false,
	}
}

#[post("/skills/git/scan", data = "<body>")]
pub async fn git_scan_skills(
	body: Json<GitScanRequest>,
	sessions: &rocket::State<GitCloneSessions>,
) -> ApiResult<GitScanResponse> {
	let req = body.into_inner();

	// Resolve credential token — either from session or from request.
	// The session reuse branch also captures the session's stored URL so the
	// guard below can pin a reused token to its own repository host.
	let mut session_url: Option<String> = None;
	let credential_token: Option<String> =
		if let Some(ref cred_id) = req.credential_id {
			let creds = SourceCredentialStore.list().map_err(|e| {
				ApiError::new(
					Status::InternalServerError,
					format!("Failed to read credentials: {e}"),
					"KEYCHAIN_ERROR",
				)
			})?;
			let cred =
				creds.iter().find(|c| c.id == *cred_id).ok_or_else(|| {
					ApiError::new(
						Status::NotFound,
						"Credential not found",
						"CREDENTIAL_NOT_FOUND",
					)
				})?;
			Some(cred.token.clone())
		} else if let Some(ref sid) = req.session_id {
			// Reuse credential from existing session
			let map = sessions.sessions.lock().unwrap();
			match map.get(sid) {
				Some(s) => {
					session_url = Some(s.url.clone());
					s.credential_token.clone()
				}
				None => None,
			}
		} else {
			None
		};

	// Guard the explicitly supplied credential to github.com, but pin a reused
	// session token to its own repository host instead (session tokens may be
	// host-scoped private credentials resolved lazily on the original scan).
	// The lazy/host-scoped path resolved inside the clone below is left
	// unguarded — it is already bound to the scanned host.
	if credential_token.is_some() {
		if req.credential_id.is_some() {
			require_github_credential_url(&req.url)?;
		} else if let Some(ref stored_url) = session_url {
			if !same_host(&req.url, stored_url) {
				return Err(ApiError::new(
					Status::BadRequest,
					"Session credential cannot be reused for a different host",
					"SESSION_CREDENTIAL_HOST_MISMATCH",
				));
			}
		}
	}

	// Retrieve cached branches from existing session if re-scanning
	let cached_branches: Option<Vec<String>> =
		if let Some(ref sid) = req.session_id {
			let map = sessions.sessions.lock().unwrap();
			map.get(sid).map(|s| s.branches.clone())
		} else {
			None
		};

	let url = req.url.clone();
	let branch_for_clone = req.branch.clone();
	let token_for_clone = credential_token.clone();

	// Clone repo in a blocking thread (gix is synchronous)
	let (temp_dir, credential_token) = tokio::task::spawn_blocking(move || {
		clone_for_git_scan_lazily_auth(
			&url,
			branch_for_clone.as_deref(),
			token_for_clone,
		)
	})
	.await
	.map_err(|e| {
		ApiError::new(
			Status::InternalServerError,
			format!("Clone task panicked: {e}"),
			"CLONE_ERROR",
		)
	})?
	.map_err(|e| {
		// Strip any URL userinfo (user:token@) from the surfaced gix error so
		// a token embedded in a clone URL never leaks into the API response/logs.
		let msg = aghub_git::redact_url_userinfo(&format!(
			"Failed to clone repository: {e}"
		));
		ApiError::new(Status::BadRequest, msg, "CLONE_FAILED")
	})?;

	// List remote branches (use cache from previous session if
	// available to avoid an extra network call on branch switch)
	let branch_url = req.url.clone();
	let credential_token_for_branches = credential_token.clone();
	let branches = list_branches_for_scan(cached_branches, move || {
		let options = match credential_token_for_branches {
			Some(token) => aghub_git::RemoteOptions::new(&branch_url)
				.with_credentials("x-access-token", token),
			None => aghub_git::RemoteOptions::new(&branch_url),
		};
		aghub_git::list_remote_branches(options)
	})
	.await?;

	// Determine current branch name from the checked-out HEAD
	let current_branch =
		detect_current_branch(temp_dir.path()).unwrap_or_else(|| {
			req.branch.clone().unwrap_or_else(|| {
				// Guess from the branches list — first one
				// alphabetically that looks like a default
				["main", "master"]
					.iter()
					.find(|b| branches.contains(&b.to_string()))
					.map(|b| b.to_string())
					.unwrap_or_default()
			})
		});

	// Scan the cloned repo for skills
	let scan_options = skill::scan::ScanOptions {
		max_depth: 10,
		full_depth: true,
		respect_gitignore: true,
	};
	let temp_path = temp_dir.path().to_path_buf();
	let skill_paths =
		skill::scan::scan_skills(&temp_path, scan_options, vec![]).map_err(
			|e| {
				ApiError::new(
					Status::InternalServerError,
					format!("Failed to scan repository for skills: {e:?}"),
					"SCAN_ERROR",
				)
			},
		)?;

	// Parse each skill to extract metadata
	let mut skills = Vec::new();
	for path in &skill_paths {
		match skill::parser::parse(path) {
			Ok(parsed) => {
				let relative = path
					.strip_prefix(&temp_path)
					.unwrap_or(path)
					.to_string_lossy()
					.to_string();
				skills.push(GitScanSkillEntry {
					name: parsed.name,
					description: parsed.description,
					author: parsed.author,
					version: parsed.version,
					path: relative,
				});
			}
			Err(_) => {
				// Skip unparseable skill directories
			}
		}
	}

	// Remove old session if re-scanning
	if let Some(ref old_sid) = req.session_id {
		let mut map = sessions.sessions.lock().unwrap();
		map.remove(old_sid);
	}

	// Store the temp dir in session map so it persists until install
	let session_id = uuid::Uuid::new_v4().to_string();
	{
		let mut map = sessions.sessions.lock().unwrap();
		// Purge sessions older than 30 minutes
		let cutoff = std::time::Duration::from_secs(30 * 60);
		map.retain(|_, s| s.created_at.elapsed() < cutoff);
		map.insert(
			session_id.clone(),
			GitCloneSession {
				temp_dir,
				created_at: std::time::Instant::now(),
				url: req.url,
				credential_token,
				branches: branches.clone(),
				current_branch: current_branch.clone(),
			},
		);
	}

	Ok(Json(GitScanResponse {
		session_id,
		skills,
		branches,
		current_branch,
	}))
}

fn clone_for_git_scan_lazily_auth(
	url: &str,
	branch: Option<&str>,
	credential_token: Option<String>,
) -> aghub_git::Result<(tempfile::TempDir, Option<String>)> {
	if let Some(token) = credential_token {
		return clone_for_git_scan(url, branch, Some(&token))
			.map(|temp_dir| (temp_dir, Some(token)));
	}

	match clone_for_git_scan(url, branch, None) {
		Ok(temp_dir) => Ok((temp_dir, None)),
		Err(first_error) => {
			let Some(token) = token_for_git_scan_source(url) else {
				return Err(first_error);
			};
			clone_for_git_scan(url, branch, Some(&token))
				.map(|temp_dir| (temp_dir, Some(token)))
		}
	}
}

fn clone_for_git_scan(
	url: &str,
	branch: Option<&str>,
	credential_token: Option<&str>,
) -> aghub_git::Result<tempfile::TempDir> {
	let mut options = aghub_git::CloneOptions::new(url);
	if let Some(token) = credential_token {
		options = options.with_credentials("x-access-token", token);
	}
	if let Some(branch) = branch {
		options = options.with_branch(branch);
	}
	aghub_git::clone_to_temp(options)
}

fn token_for_git_scan_source(source: &str) -> Option<String> {
	let host = keychain_host_for_source(source);
	SourceCredentialStore
		.resolve_token(source, host.as_deref())
		.ok()
		.flatten()
}

/// Try to detect the checked-out branch from the cloned repo via its gix `HEAD`
/// symref. Never shells out to the `git` binary.
fn detect_current_branch(repo_path: &std::path::Path) -> Option<String> {
	aghub_git::current_branch_at_path(repo_path)
}

/// Partition `agents` (raw strings from the request) into valid/invalid
/// entries in request order. Invalid entries carry the error message to
/// surface back to the caller.
///
/// Valid: the agent string parses AND `resolve_git_install_target_dir`
///        returns `Some` for `scope` + `project_root`.
/// Invalid: unknown agent string OR no skills dir for this scope.
#[allow(clippy::type_complexity)]
fn partition_install_agents_in_request_order(
	agents: &[String],
	scope: ResourceScope,
	project_root: Option<&std::path::Path>,
) -> (Vec<(String, AgentType)>, Vec<(String, String)>) {
	let project_root_buf: Option<std::path::PathBuf> =
		project_root.map(|p| p.to_path_buf());
	let mut valid: Vec<(String, AgentType)> = Vec::new();
	let mut invalid: Vec<(String, String)> = Vec::new();
	for agent_str in agents {
		match agent_str.parse::<AgentType>() {
			Ok(agent_type) => {
				if resolve_git_install_target_dir(
					agent_type,
					scope,
					project_root_buf.as_ref(),
				)
				.is_some()
				{
					valid.push((agent_str.clone(), agent_type));
				} else {
					invalid.push((
						agent_str.clone(),
						format!(
							"Agent '{}' does not support persistent \
							 skill creation in this scope",
							agent_str
						),
					));
				}
			}
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
	body: Json<GitInstallRequest>,
	sessions: &rocket::State<GitCloneSessions>,
) -> ApiResult<GitInstallResponse> {
	let req = body.into_inner();

	// Extract temp dir path and source metadata from session
	let (temp_path, source) = {
		let map = sessions.sessions.lock().unwrap();
		let session = map.get(&req.session_id).ok_or_else(|| {
			ApiError::new(
				Status::NotFound,
				"Session not found or expired",
				"SESSION_NOT_FOUND",
			)
		})?;
		let ref_name = if session.current_branch.is_empty() {
			None
		} else {
			Some(session.current_branch.clone())
		};
		let resolved = aghub_git::resolve_remote_source(&session.url)
			.map_err(map_remote_source_error)?;
		(
			session.temp_dir.path().to_path_buf(),
			install_lock_source_from_resolved(&resolved, ref_name),
		)
	};

	let resource_scope = parse_install_scope(&req.scope)?;

	let project_root: Option<std::path::PathBuf> = req
		.project_root
		.as_ref()
		.map(|r| crate::extractors::absolutize_root(r));

	let mut results = Vec::new();

	// Record the session clone's checked-out tip OID (repo-level, shared by all
	// installed skills) so the first `check` can preflight via ls-refs.
	// Best-effort: a read failure leaves `refCommit` unset.
	let ref_commit = gix::open(&temp_path)
		.ok()
		.and_then(|repo| repo.head_id().ok().map(|id| id.detach()))
		.map(|oid| oid.to_string());

	let (valid_agents, invalid_agents) =
		partition_install_agents_in_request_order(
			&req.agents,
			resource_scope,
			project_root.as_deref(),
		);

	for (agent_str, error) in &invalid_agents {
		for skill_path in &req.skill_paths {
			results.push(GitInstallResultEntry {
				name: skill_path.clone(),
				agent: agent_str.clone(),
				success: false,
				error: Some(error.clone()),
			});
		}
	}

	let target_agents: Vec<AgentType> =
		valid_agents.iter().map(|(_, agent)| *agent).collect();

	for skill_path in &req.skill_paths {
		let full_path = temp_path.join(skill_path);
		let relative_dir = skill_path.replace('\\', "/");

		let request =
			aghub_core::skills::install_fetched::FetchedSkillInstallRequest {
				skill_file: &full_path,
				source: &source,
				lock_skill_path: skill::lock_skill_file_path(&relative_dir),
				ref_commit: ref_commit.clone(),
				scope: resource_scope,
				project_root: project_root.as_deref(),
				target_agents: &target_agents,
				expected_name: None,
				target: if matches!(resource_scope, ResourceScope::ProjectOnly)
				{
					aghub_core::skills::linker::LinkTarget::Relative
				} else {
					aghub_core::skills::linker::LinkTarget::Absolute
				},
			};

		match aghub_core::skills::install_fetched::install_fetched_skill_and_lock(
			request,
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
			Err(e) => {
				let message = ApiError::from(e).body.error;
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

	// Remove session (drops TempDir, cleans up disk)
	{
		let mut map = sessions.sessions.lock().unwrap();
		map.remove(&req.session_id);
	}

	Ok(Json(GitInstallResponse { results }))
}

/// Replace existing skill installations in-place from a previously-scanned
/// git session. Targets are derived from the installed skill name on the server;
/// client-provided paths are accepted only for backward-compatible requests.
#[post("/skills/git/sync", data = "<body>")]
pub async fn git_sync_skill(
	body: Json<GitSyncRequest>,
	sessions: &rocket::State<GitCloneSessions>,
) -> ApiResult<GitSyncResponse> {
	let req = body.into_inner();

	// Retrieve temp dir from session (keep session alive until end)
	let temp_path = {
		let map = sessions.sessions.lock().unwrap();
		let session = map.get(&req.session_id).ok_or_else(|| {
			ApiError::new(
				Status::NotFound,
				"Session not found or expired",
				"SESSION_NOT_FOUND",
			)
		})?;
		session.temp_dir.path().to_path_buf()
	};

	// Full path of the SKILL.md (or skill dir) inside the clone
	let cloned_skill_path = aghub_core::skills::update::sanitize_skill_path(
		&temp_path,
		&req.skill_path,
	)
	.ok_or_else(|| {
		ApiError::new(
			Status::BadRequest,
			"skill_path must be a relative path inside the cloned repository",
			"SKILL_PATH_INVALID",
		)
	})?;
	let cloned_skill_dir = get_skill_root(cloned_skill_path.clone());

	if !cloned_skill_dir.exists() {
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

	let parsed_skill =
		skill::parser::parse(&cloned_skill_path).map_err(|e| {
			ApiError::new(
				Status::BadRequest,
				format!("Failed to parse synced skill: {e}"),
				"SKILL_PARSE_FAILED",
			)
		})?;
	if parsed_skill.name != req.name {
		return Err(ApiError::new(
			Status::BadRequest,
			skill_renamed_message(&req.name, &parsed_skill.name),
			SKILL_RENAMED_CODE,
		));
	}
	let updated_hash = skill::compute_skill_folder_hash(&cloned_skill_dir)
		.map_err(|e| {
			ApiError::new(
				Status::InternalServerError,
				format!("Failed to hash synced skill: {e}"),
				"SKILL_SYNC_ERROR",
			)
		})?;

	let target_dirs = installed_skill_roots(
		&req.name,
		resource_scope,
		project_root.as_deref(),
	);
	if target_dirs.is_empty() {
		return Err(ApiError::new(
			Status::NotFound,
			format!(
				"Skill '{}' is locked but no installed copy was found",
				req.name
			),
			"SKILL_NOT_INSTALLED",
		));
	}
	let agent_dirs = aghub_core::skills::removal::agent_skill_dirs_in_scope(
		resource_scope,
		project_root.as_deref(),
	);
	aghub_core::skills::removal::assert_targets_strictly_contained(
		&target_dirs,
		&agent_dirs,
		project_root.as_deref(),
	)
	.map_err(|e| {
		ApiError::new(
			Status::BadRequest,
			format!("Refusing to sync out-of-tree target: {e}"),
			"SKILL_TARGET_OUT_OF_TREE",
		)
	})?;

	// Replace each installation path
	for target_dir in &target_dirs {
		aghub_core::skills::update::stage_and_swap_dir(
			&cloned_skill_dir,
			target_dir,
		)
		.map_err(|e| ApiError::from(ConfigError::Io(e)))?;
	}

	// The session-based sync clones into a session temp dir and does not carry a
	// resolved tip OID, so it leaves `refCommit` untouched (None). install /
	// apply-update remain the explicit refCommit write points.
	update_lock_hash(
		&req.name,
		&req.scope,
		project_root.as_deref(),
		&updated_hash,
		None,
	)
	.map_err(|e| {
		ApiError::new(
			Status::InternalServerError,
			format!("Failed to update skill lock after sync: {e}"),
			"SKILL_LOCK_ERROR",
		)
	})?;

	// Remove session (drops TempDir, cleans up disk)
	{
		let mut map = sessions.sessions.lock().unwrap();
		map.remove(&req.session_id);
	}

	Ok(Json(GitSyncResponse {
		success: true,
		name: Some(parsed_skill.name),
		updated_hash: Some(updated_hash),
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
			let resp =
				block_on(delete_skill_by_path(Json(by_path_req(&dir, None))))
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
			let resp = block_on(delete_skill_by_path(Json(by_path_req(
				&dir,
				Some(true),
			))))
			.ok()
			.expect("handler returned ok")
			.into_inner();
			assert!(resp.success);
			assert!(!resp.dry_run);
			assert!(!dir.exists(), "confirm deletes the dir");
		});
	}

	// The delete_skill handler must NOT prune the lock itself — that moved into
	// remove_skill_planned. This proves the manager-side prune still fires
	// through the API path: an orphan global-lock entry (no dir on disk) is gone
	// after a confirmed delete. Would regress if both the handler prune AND the
	// manager prune were removed.
	#[cfg(unix)]
	#[test]
	fn delete_skill_executes_and_lock_pruned() {
		with_isolated_env(|home, state| {
			let dir = write_claude_skill(home, "goner");
			let lock_dir = state.join("skills");
			std::fs::create_dir_all(&lock_dir).unwrap();
			let lock_path = lock_dir.join(".skill-lock.json");
			std::fs::write(&lock_path, ORPHAN_LOCK_JSON).unwrap();

			let resp = block_on(delete_skill(
				AgentParam(AgentType::Claude),
				"goner",
				DeleteSkillParams {
					scope: Some("global".to_string()),
					project_root: None,
					confirm: Some(true),
					all_agents: None,
				},
			))
			.ok()
			.expect("handler returned ok")
			.into_inner();

			assert!(resp.success);
			assert!(resp.executed);
			assert!(!dir.exists(), "confirm deletes the dir");
			let raw = std::fs::read_to_string(&lock_path).unwrap();
			let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap();
			assert!(
				parsed["skills"].get("orphan").is_none(),
				"manager-side prune must drop the orphan lock entry"
			);
		});
	}

	// create_skill must surface the `native_reader` advisory the CLI shows: a
	// per-agent-linking agent (Claude) reports false (key omitted), while a
	// NativeReader (OpenCode at project scope, reads `.agents/skills` directly)
	// reports true. Both go through the REAL handler.
	#[cfg(unix)]
	#[test]
	fn create_skill_native_reader_false_for_claude() {
		with_isolated_env(|_home, _state| {
			let resp = block_on(create_skill(
				AgentParam(AgentType::Claude),
				ScopeParams {
					scope: Some("global".to_string()),
					project_root: None,
				},
				Json(CreateSkillRequest {
					name: "linked".to_string(),
					description: Some("d".to_string()),
					author: None,
					version: None,
					content: None,
					tools: None,
				}),
			))
			.ok()
			.expect("handler ok")
			.1
			.into_inner();

			assert!(
				!resp.native_reader,
				"Claude links per-agent → not a native reader"
			);
			let json = serde_json::to_value(&resp).unwrap();
			assert_eq!(
				json["native_reader"],
				serde_json::json!(false),
				"native_reader present (= false)"
			);
		});
	}

	#[cfg(unix)]
	#[test]
	fn create_skill_native_reader_true_for_opencode_project() {
		with_isolated_env(|home, _state| {
			let project = home.join("proj");
			std::fs::create_dir_all(project.join(".opencode")).unwrap();

			let resp = block_on(create_skill(
				AgentParam(AgentType::OpenCode),
				ScopeParams {
					scope: Some("project".to_string()),
					project_root: Some(project.display().to_string()),
				},
				Json(CreateSkillRequest {
					name: "native".to_string(),
					description: Some("d".to_string()),
					author: None,
					version: None,
					content: None,
					tools: None,
				}),
			))
			.ok()
			.expect("handler ok")
			.1
			.into_inner();

			assert!(
				resp.native_reader,
				"OpenCode reads .agents/skills directly → native reader"
			);
			let json = serde_json::to_value(&resp).unwrap();
			assert_eq!(json["native_reader"], serde_json::json!(true));
		});
	}

	// import_skill must surface the same `native_reader` advisory as create:
	// Claude (per-agent link) reports false / key omitted, OpenCode at project
	// scope (reads `.agents/skills` directly) reports true. Both go through the
	// REAL handler with a real on-disk source skill folder.
	#[cfg(unix)]
	fn write_source_skill(name: &str) -> tempfile::TempDir {
		let src = tempdir().unwrap();
		let dir = src.path().join(name);
		std::fs::create_dir_all(&dir).unwrap();
		std::fs::write(
			dir.join("SKILL.md"),
			format!("---\nname: {name}\ndescription: d\n---\n"),
		)
		.unwrap();
		src
	}

	#[cfg(unix)]
	#[test]
	fn import_skill_native_reader_false_for_claude() {
		with_isolated_env(|_home, _state| {
			let src = write_source_skill("imported");
			let resp = import_skill(
				AgentParam(AgentType::Claude),
				ScopeParams {
					scope: Some("global".to_string()),
					project_root: None,
				},
				Json(crate::dto::skill::ImportSkillRequest {
					path: src
						.path()
						.join("imported/SKILL.md")
						.display()
						.to_string(),
				}),
			)
			.ok()
			.expect("handler ok")
			.into_inner();

			assert!(
				!resp.native_reader,
				"Claude links per-agent → not a native reader"
			);
			let json = serde_json::to_value(&resp).unwrap();
			assert_eq!(
				json["native_reader"],
				serde_json::json!(false),
				"native_reader present (= false)"
			);
		});
	}

	#[cfg(unix)]
	#[test]
	fn import_skill_native_reader_true_for_opencode_project() {
		with_isolated_env(|home, _state| {
			let project = home.join("proj");
			std::fs::create_dir_all(project.join(".opencode")).unwrap();
			let src = write_source_skill("imported");

			let resp = import_skill(
				AgentParam(AgentType::OpenCode),
				ScopeParams {
					scope: Some("project".to_string()),
					project_root: Some(project.display().to_string()),
				},
				Json(crate::dto::skill::ImportSkillRequest {
					path: src
						.path()
						.join("imported/SKILL.md")
						.display()
						.to_string(),
				}),
			)
			.ok()
			.expect("handler ok")
			.into_inner();

			assert!(
				resp.native_reader,
				"OpenCode reads .agents/skills directly → native reader"
			);
			let json = serde_json::to_value(&resp).unwrap();
			assert_eq!(json["native_reader"], serde_json::json!(true));
		});
	}

	// Issue #2 + #3: the by-path copy branch must surface the post-delete lock
	// prune through the response — both the dropped keys (Pruned) and a prune
	// failure (Failed). It now routes through the core-owned
	// `prune::prune_lock_for_scope` seam; the handler only RENDERS the result.
	#[cfg(unix)]
	#[test]
	fn delete_by_path_surfaces_pruned_lock_entries() {
		with_isolated_env(|home, state| {
			let dir = write_claude_skill(home, "goner");
			let lock_dir = state.join("skills");
			std::fs::create_dir_all(&lock_dir).unwrap();
			let lock_path = lock_dir.join(".skill-lock.json");
			// An orphan with no dir on disk: a real prune drops it.
			std::fs::write(&lock_path, ORPHAN_LOCK_JSON).unwrap();

			let resp = block_on(delete_skill_by_path(Json(by_path_req(
				&dir,
				Some(true),
			))))
			.ok()
			.expect("handler returned ok")
			.into_inner();

			assert!(resp.success);
			assert!(resp.executed);
			assert!(!dir.exists(), "confirm deletes the dir");
			let pruned = resp
				.pruned_lock_entries
				.expect("a successful prune must report its dropped keys");
			assert!(
				pruned.contains(&"orphan".to_string()),
				"the dropped orphan must be surfaced, got {pruned:?}"
			);
			assert!(resp.prune_error.is_none(), "no error on a clean prune");
		});
	}

	#[cfg(unix)]
	#[test]
	fn delete_by_path_surfaces_prune_error() {
		use std::os::unix::fs::PermissionsExt;
		with_isolated_env(|home, state| {
			let dir = write_claude_skill(home, "goner");
			let lock_dir = state.join("skills");
			std::fs::create_dir_all(&lock_dir).unwrap();
			let lock_path = lock_dir.join(".skill-lock.json");
			std::fs::write(&lock_path, ORPHAN_LOCK_JSON).unwrap();

			// Make the lock dir read-only so the prune's atomic write fails.
			let probe = lock_dir.join(".perm-probe");
			std::fs::create_dir(&probe).unwrap();
			std::fs::set_permissions(
				&probe,
				std::fs::Permissions::from_mode(0o555),
			)
			.unwrap();
			let enforced = std::fs::write(probe.join("x"), b"x").is_err();
			std::fs::set_permissions(
				&probe,
				std::fs::Permissions::from_mode(0o755),
			)
			.unwrap();
			std::fs::remove_dir_all(&probe).ok();
			if !enforced {
				eprintln!("skip: perms not enforced (root)");
				return;
			}
			let orig = std::fs::metadata(&lock_dir).unwrap().permissions();
			std::fs::set_permissions(
				&lock_dir,
				std::fs::Permissions::from_mode(0o555),
			)
			.unwrap();

			let resp = block_on(delete_skill_by_path(Json(by_path_req(
				&dir,
				Some(true),
			))))
			.ok()
			.expect("handler returned ok")
			.into_inner();

			std::fs::set_permissions(&lock_dir, orig).unwrap();

			assert!(
				resp.success,
				"deletion still succeeds, prune is non-fatal"
			);
			assert!(resp.executed);
			assert!(!dir.exists(), "the skill is deleted before the prune");
			assert!(
				resp.prune_error.is_some(),
				"a failed prune must surface its error"
			);
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

			let resp = block_on(delete_skill_by_path(Json(by_path_req(
				&link,
				Some(true),
			))))
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
			let resp = block_on(delete_skill_by_path(Json(req)))
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
			let resp = block_on(delete_skill_by_path(Json(req)))
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

			let resp = prune_lock_route(Json(prune_req("global", None, None)))
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

			let resp =
				prune_lock_route(Json(prune_req("global", None, Some(true))))
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
			let resp =
				prune_lock_route(Json(prune_req("project", None, Some(true))))
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

			import_skill(agent, scope, body)
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

			let clone = tempdir().unwrap();
			let cloned_skill = clone.path().join("sync-me");
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
				.state::<GitCloneSessions>()
				.expect("git clone sessions");
			sessions.sessions.lock().unwrap().insert(
				"sync-session".to_string(),
				GitCloneSession {
					temp_dir: clone,
					created_at: std::time::Instant::now(),
					url: "https://github.com/owner/repo.git".to_string(),
					credential_token: None,
					branches: vec!["main".to_string()],
					current_branch: "main".to_string(),
				},
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

			let clone = tempdir().unwrap();
			let cloned_skill = clone.path().join("other");
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
				.state::<GitCloneSessions>()
				.expect("git clone sessions");
			sessions.sessions.lock().unwrap().insert(
				"sync-session".to_string(),
				GitCloneSession {
					temp_dir: clone,
					created_at: std::time::Instant::now(),
					url: "https://github.com/owner/repo.git".to_string(),
					credential_token: None,
					branches: vec!["main".to_string()],
					current_branch: "main".to_string(),
				},
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
	fn reconcile_skill_prefers_primary_path_for_opencode() {
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
			.join(".opencode/skills/repo-helper/assets/notes.txt")
			.exists());
		// Under the symlink-only model the source `add_skill` writes a
		// `.agents/skills` Master by construction (Task 25), so the Master
		// existing is expected. What this test pins is that reconcile copies
		// into OpenCode's own primary path (asserted above).
		assert!(project_root.join(".agents/skills/repo-helper").exists());
	}

	#[test]
	fn detect_current_branch_uses_gix_not_subprocess() {
		// Arrange a gix repo (no `git` binary), then assert the helper resolves
		// the branch from the on-disk HEAD symref.
		let temp = tempdir().unwrap();
		let repo = gix::init(temp.path()).unwrap();
		let head = repo.head_name().unwrap().unwrap();
		let full = head.as_bstr().to_string();
		let expected = full.strip_prefix("refs/heads/").unwrap_or(&full);

		let detected = detect_current_branch(temp.path()).unwrap();
		assert_eq!(detected, expected);
		assert!(!detected.starts_with("refs/"));

		// Guard: the source must not shell out to the `git` binary for branch
		// detection.
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
	fn same_host_true_for_matching_hosts() {
		assert!(same_host(
			"https://gitlab.internal/a.git",
			"https://gitlab.internal/b.git",
		));
	}

	#[test]
	fn same_host_false_for_different_hosts() {
		assert!(!same_host("https://github.com/a", "https://evil.com/a"));
	}

	#[test]
	fn same_host_false_on_parse_failure() {
		assert!(!same_host("not a url", "https://github.com/a"));
	}

	#[test]
	fn same_host_is_port_agnostic() {
		// Session pinning keys on host, not port; the same host on a
		// different port must be treated as the same host (see same_host
		// doc comment). This documents the intentional behavior.
		assert!(same_host(
			"https://git.internal:8080/a.git",
			"https://git.internal:9090/b.git",
		));
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
			.state::<GitCloneSessions>()
			.expect("git clone sessions");
		sessions.sessions.lock().unwrap().insert(
			"test-session".to_string(),
			GitCloneSession {
				temp_dir: tempdir().unwrap(),
				created_at: std::time::Instant::now(),
				url: "https://gitlab.internal/repo.git".to_string(),
				credential_token: Some("secret-token".to_string()),
				branches: vec![],
				current_branch: String::new(),
			},
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
				.state::<GitCloneSessions>()
				.expect("sessions state");
			{
				let mut map = app_sessions.sessions.lock().unwrap();
				let td = tempdir().unwrap();
				let dst = td.path().join("my-skill");
				std::fs::create_dir_all(&dst).unwrap();
				std::fs::write(
					dst.join("SKILL.md"),
					"---\nname: my-skill\ndescription: d\n---\n",
				)
				.unwrap();
				map.insert(
					"sess-1".to_string(),
					GitCloneSession {
						temp_dir: td,
						created_at: std::time::Instant::now(),
						url: "https://github.com/o/r".to_string(),
						credential_token: None,
						branches: vec![],
						current_branch: "main".to_string(),
					},
				);
			}
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
		});
	}

	#[cfg(unix)]
	#[test]
	fn install_skill_returns_per_agent_rows_symlink_only() {
		with_isolated_env(|home, _state| {
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
			let resp = block_on(install_skill(Json(req)))
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

			let resp = block_on(delete_skill_by_path(Json(by_path_req(
				&link,
				Some(true),
			))))
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

	#[cfg(unix)]
	#[test]
	fn install_skill_relative_project_root_is_absolutized() {
		with_isolated_env(|home, _state| {
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

			let prev = std::env::current_dir().unwrap();
			std::env::set_current_dir(home).unwrap();
			let req = InstallSkillRequest {
				source: format!("file://{}", work.display()),
				agents: vec!["claude".to_string()],
				skills: vec!["my-skill".to_string()],
				scope: "project".to_string(),
				project_path: Some("proj".to_string()),
				install_all: Some(false),
			};
			let resp = block_on(install_skill(Json(req)))
				.ok()
				.expect("handler ok")
				.into_inner();
			std::env::set_current_dir(prev).unwrap();

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
}
