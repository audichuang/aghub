use aghub_cc_plugins::claude::ClaudePluginManager;
use aghub_core::{
	convert_skill, create_adapter,
	errors::ConfigError,
	load_all_agents,
	models::{AgentType, ResourceScope, Skill},
	registry, transfer,
};
use rocket::http::Status;
use rocket::serde::json::Json;
use skill::sanitize::sanitize_name;
use std::{
	collections::HashMap,
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
	state::{GitCloneSession, GitCloneSessions},
};

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

	let project_root = req.project_root.as_ref().map(std::path::PathBuf::from);

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
	let path_is_symlink = std::fs::symlink_metadata(&skill_dir)
		.map(|meta| meta.file_type().is_symlink())
		.unwrap_or(false);
	let canonical_layout = manager
		.get_skill(&skill_name)
		.and_then(|skill| skill.canonical_path.as_ref())
		.is_some()
		|| path_is_symlink;

	if !canonical_layout {
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
		prune_scope_lock(resource_scope, project_root.as_deref());
		return Ok(Json(delete_response_from_outcome(
			aghub_core::skills::removal::RemovalOutcome {
				plan: executed_plan,
				executed: true,
			},
		)));
	}

	match manager.remove_skill_planned(&skill_name, false, dry_run, confirm) {
		Ok(outcome) => {
			if outcome.executed {
				prune_scope_lock(resource_scope, project_root.as_deref());
			}
			Ok(Json(delete_response_from_outcome(outcome)))
		}
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

/// Best-effort disk-reconciled lock prune for a scope after a deletion. A scan
/// error (or missing project root) is swallowed — the orphan lock entry simply
/// survives; deletion correctness does not depend on the prune succeeding.
fn prune_scope_lock(
	resource_scope: aghub_core::models::ResourceScope,
	project_root: Option<&std::path::Path>,
) {
	use aghub_core::models::ResourceScope;
	use aghub_core::skills::prune::{prune_lock_scanning, PruneScope};
	if matches!(
		resource_scope,
		ResourceScope::GlobalOnly | ResourceScope::Both
	) {
		let _ = prune_lock_scanning(PruneScope::Global, None);
	}
	if matches!(
		resource_scope,
		ResourceScope::ProjectOnly | ResourceScope::Both
	) {
		if let Some(root) = project_root {
			let _ = prune_lock_scanning(PruneScope::Project, Some(root));
		}
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
		error: None,
		validation_errors: None,
	}
}

fn copy_dir_recursive(
	from: &std::path::Path,
	to: &std::path::Path,
) -> Result<(), ApiError> {
	std::fs::create_dir_all(to)
		.map_err(|e| ApiError::from(ConfigError::Io(e)))?;
	for entry in std::fs::read_dir(from)
		.map_err(|e| ApiError::from(ConfigError::Io(e)))?
	{
		let entry = entry.map_err(|e| ApiError::from(ConfigError::Io(e)))?;
		let from_path = entry.path();
		let to_path = to.join(entry.file_name());
		let file_type = entry
			.file_type()
			.map_err(|e| ApiError::from(ConfigError::Io(e)))?;
		if file_type.is_dir() {
			copy_dir_recursive(&from_path, &to_path)?;
		} else {
			std::fs::copy(&from_path, &to_path)
				.map_err(|e| ApiError::from(ConfigError::Io(e)))?;
		}
	}
	Ok(())
}

fn resolve_git_install_target_dir(
	agent_type: AgentType,
	resource_scope: ResourceScope,
	project_root: Option<&std::path::PathBuf>,
) -> Option<std::path::PathBuf> {
	create_adapter(agent_type)
		.target_skills_dir(project_root.map(|p| p.as_path()), resource_scope)
}

fn install_git_skill_to_dir(
	full_path: &std::path::Path,
	target_dir: &std::path::Path,
) -> Result<(String, bool), ApiError> {
	let parsed = skill::parser::parse(full_path).map_err(|e| {
		ApiError::new(
			Status::BadRequest,
			format!("Failed to parse skill: {e}"),
			"SKILL_PARSE_FAILED",
		)
	})?;
	let skill = convert_skill(parsed);
	let safe_name = sanitize_name(&skill.name);
	let dest_root = target_dir.join(&safe_name);

	let copied = if !dest_root.exists() {
		let source_root = get_skill_root(full_path.to_path_buf());
		copy_dir_recursive(&source_root, &dest_root)?;
		true
	} else {
		false
	};

	Ok((skill.name, copied))
}

/// Universal-layout install: write the skill master once into
/// `<canonical_skills_dir>/<name>` and symlink each selected agent's skills dir
/// to it. Agents whose write dir *is* the canonical dir are skipped (the master
/// already lives there); agents that merely read `.agents` still get a redundant
/// but harmless link — [`aghub_core::skills::install_layout`] is idempotent and
/// never clobbers an existing real directory. Returns `(name, wrote_master)`.
fn install_git_skill_universal(
	full_path: &std::path::Path,
	agent_target_dirs: &[std::path::PathBuf],
	canonical_skills_dir: &std::path::Path,
	use_relative_links: bool,
) -> Result<(String, bool), ApiError> {
	let parsed = skill::parser::parse(full_path).map_err(|e| {
		ApiError::new(
			Status::BadRequest,
			format!("Failed to parse skill: {e}"),
			"SKILL_PARSE_FAILED",
		)
	})?;
	let skill = convert_skill(parsed);
	let safe_name = sanitize_name(&skill.name);
	let canonical = canonical_skills_dir.join(&safe_name);
	let wrote_master = !canonical.exists();
	let source_root = get_skill_root(full_path.to_path_buf());

	let symlink_dirs: Vec<std::path::PathBuf> = agent_target_dirs
		.iter()
		.filter(|dir| dir.as_path() != canonical_skills_dir)
		.cloned()
		.collect();

	aghub_core::skills::install_layout::install_universal(
		&source_root,
		&canonical,
		&symlink_dirs,
		use_relative_links,
	)
	.map_err(|e| ApiError::from(ConfigError::Io(e)))?;

	Ok((skill.name, wrote_master))
}

type GitInstallAgentGroup = Vec<(String, AgentType)>;
type GitInstallGroups = HashMap<std::path::PathBuf, GitInstallAgentGroup>;
type GitInstallInvalidAgent = (String, Option<AgentType>, String);

fn build_git_install_groups(
	agents: &[String],
	resource_scope: ResourceScope,
	project_root: Option<&std::path::PathBuf>,
) -> (GitInstallGroups, Vec<GitInstallInvalidAgent>) {
	let mut groups = HashMap::new();
	let mut invalid = Vec::new();

	for agent_str in agents {
		let agent_type: AgentType = match agent_str.parse() {
			Ok(agent) => agent,
			Err(_) => {
				invalid.push((
					agent_str.clone(),
					None,
					format!("Unknown agent '{agent_str}'"),
				));
				continue;
			}
		};

		let Some(target_dir) = resolve_git_install_target_dir(
			agent_type,
			resource_scope,
			project_root,
		) else {
			invalid.push((
				agent_str.clone(),
				Some(agent_type),
				format!(
					"Agent '{}' does not support persistent skill creation \
					 in this scope",
					agent_str
				),
			));
			continue;
		};

		groups
			.entry(target_dir)
			.or_insert_with(Vec::new)
			.push((agent_str.clone(), agent_type));
	}

	(groups, invalid)
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
) -> Result<(), ApiError> {
	match resource_scope {
		ResourceScope::GlobalOnly => {
			skill::write_global_install_lock(
				skill_name,
				source,
				lock_skill_path,
				source_dir,
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

fn skill_lock_contains(
	skill_name: &str,
	resource_scope: ResourceScope,
	project_root: Option<&Path>,
) -> bool {
	match resource_scope {
		ResourceScope::GlobalOnly => {
			skill::lock::global::get_skill_from_lock(skill_name).is_some()
		}
		ResourceScope::ProjectOnly => project_root.is_some_and(|root| {
			skill::lock::local::read_local_lock(Some(root))
				.skills
				.contains_key(skill_name)
		}),
		ResourceScope::Both => false,
	}
}

fn should_write_install_lock(
	skill_name: &str,
	copied_any: bool,
	resource_scope: ResourceScope,
	project_root: Option<&Path>,
) -> bool {
	copied_any || !skill_lock_contains(skill_name, resource_scope, project_root)
}

fn detect_available_editor() -> Option<CodeEditorType> {
	crate::editor_detection::detect_any_installed_editor()
}

fn build_skill_tree_node(
	path: &std::path::Path,
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
			.map(|entry| build_skill_tree_node(&entry.path()))
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
	let response = SkillResponse::from(&skill);
	manager.add_skill(skill).map_err(ApiError::from)?;
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
	)?;

	Ok(Json(SkillResponse::from(&imported)))
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
	let (resource_scope, project_root) = resolved_to_resource_scope(&resolved);
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
		Ok(outcome) => {
			if outcome.executed {
				prune_scope_lock(resource_scope, project_root.as_deref());
			}
			Ok(Json(delete_response_from_outcome(outcome)))
		}
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

	let project_root = req.project_path.as_ref().map(std::path::PathBuf::from);
	if resource_scope == ResourceScope::ProjectOnly && project_root.is_none() {
		return Err(ApiError::new(
			Status::BadRequest,
			"project_path is required for project skill installs",
			"INVALID_PARAM",
		));
	}

	let source = aghub_git::resolve_remote_source(&req.source)
		.map_err(map_remote_source_error)?;
	let clone_url = source.clone_url.clone();
	let lock_source = install_lock_source_from_resolved(&source, None);

	let clone_url_for_task = clone_url.clone();
	let temp_dir = match timeout(
		Duration::from_secs(300),
		tokio::task::spawn_blocking(move || {
			aghub_git::clone_to_temp(aghub_git::CloneOptions::new(
				&clone_url_for_task,
			))
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
	let (dir_groups, invalid_agents) = build_git_install_groups(
		&req.agents,
		resource_scope,
		project_root.as_ref(),
	);

	let mut has_errors = !invalid_agents.is_empty();
	let mut installed_skill_names = std::collections::HashSet::new();
	let mut copied_skill_names = std::collections::HashSet::new();

	for skill in &selected_skills {
		for (target_dir, agents) in &dir_groups {
			match install_git_skill_to_dir(&skill.full_path, target_dir) {
				Ok((skill_name, copied)) => {
					installed_skill_names.insert(skill_name.clone());
					if copied {
						copied_skill_names.insert(skill_name);
					}
					let _ = agents;
				}
				Err(_) => has_errors = true,
			}
		}
	}

	for skill in &selected_skills {
		let copied = copied_skill_names.contains(&skill.name);
		let should_write = installed_skill_names.contains(&skill.name)
			&& should_write_install_lock(
				&skill.name,
				copied,
				resource_scope,
				project_root.as_deref(),
			);
		if !should_write {
			continue;
		}

		// Hash the SOURCE repo subfolder in the temp clone, not the
		// post-copy installed dir.
		let source_dir = get_skill_root(skill.full_path.clone());
		write_skill_install_lock(
			&skill.name,
			resource_scope,
			project_root.as_deref(),
			&lock_source,
			Some(skill::lock_skill_file_path(&skill.relative_dir)),
			&source_dir,
		)?;
	}

	let success = !has_errors && !installed_skill_names.is_empty();

	Ok(Json(InstallSkillResponse { success }))
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
			match std::process::Command::new(editor.cli_command())
				.arg(&folder)
				.spawn()
			{
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

#[get("/skills/content?<query..>")]
pub fn get_skill_content(query: SkillContentQuery) -> ApiResult<String> {
	let path = expand_tilde_path(&query.path);
	let content = std::fs::read_to_string(&path).map_err(|e| {
		ApiError::new(
			Status::NotFound,
			format!("Failed to read skill file: {e}"),
			"SKILL_FILE_NOT_FOUND",
		)
	})?;

	// Use the proper skill parser to extract the body content
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
	let path = expand_tilde_path(&query.path);
	let root = get_skill_root(path);
	let tree = build_skill_tree_node(&root)?;
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

#[post("/skills/git/scan", data = "<body>")]
pub async fn git_scan_skills(
	body: Json<GitScanRequest>,
	sessions: &rocket::State<GitCloneSessions>,
) -> ApiResult<GitScanResponse> {
	let req = body.into_inner();

	// Resolve credential token — either from session or from request
	let credential_token: Option<String> =
		if let Some(ref cred_id) = req.credential_id {
			let creds = crate::routes::credentials::load_credentials()
				.map_err(|e| {
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
			map.get(sid).and_then(|s| s.credential_token.clone())
		} else {
			None
		};

	// Retrieve cached branches from existing session if re-scanning
	let cached_branches: Option<Vec<String>> =
		if let Some(ref sid) = req.session_id {
			let map = sessions.sessions.lock().unwrap();
			map.get(sid).map(|s| s.branches.clone())
		} else {
			None
		};

	let url = req.url.clone();
	let branch = req.branch.clone();
	let token_for_clone = credential_token.clone();

	// Clone repo in a blocking thread (gix is synchronous)
	let temp_dir = tokio::task::spawn_blocking(move || {
		let mut options = aghub_git::CloneOptions::new(&url);
		if let Some(token) = token_for_clone {
			options = options.with_credentials("x-access-token", token);
		}
		if let Some(ref branch) = branch {
			options = options.with_branch(branch);
		}
		aghub_git::clone_to_temp(options)
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
		// Strip any URL userinfo (user:token@) from the surfaced gix error so a
		// token embedded in a clone URL never leaks into the API response/logs.
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

/// Try to detect the checked-out branch from the cloned repo via its gix `HEAD`
/// symref. Never shells out to the `git` binary.
fn detect_current_branch(repo_path: &std::path::Path) -> Option<String> {
	aghub_git::current_branch_at_path(repo_path)
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

	let project_root: Option<std::path::PathBuf> =
		req.project_root.as_ref().map(std::path::PathBuf::from);

	let mut results = Vec::new();

	let (dir_groups, invalid_agents) = build_git_install_groups(
		&req.agents,
		resource_scope,
		project_root.as_ref(),
	);

	for (agent_str, _, error) in invalid_agents {
		for skill_path in &req.skill_paths {
			results.push(GitInstallResultEntry {
				name: skill_path.clone(),
				agent: agent_str.clone(),
				success: false,
				error: Some(error.clone()),
			});
		}
	}

	for skill_path in &req.skill_paths {
		let full_path = temp_path.join(skill_path);
		let mut installed = false;
		let mut copied_any = false;

		if req.universal.unwrap_or(false) {
			// Universal layout must follow the RESOLVED scope, not the mere
			// presence of project_root: global → ~/.agents/skills, project →
			// <root>/.agents/skills (matches the per-agent target dirs, which are
			// resolved by `resource_scope`).
			let canonical_project_root = match resource_scope {
				ResourceScope::ProjectOnly => project_root.as_deref(),
				_ => None,
			};
			let Some(canonical_skills_dir) =
				aghub_core::skills::install_layout::universal_canonical_dir(
					canonical_project_root,
				)
			else {
				for agents in dir_groups.values() {
					for (agent_str, _) in agents {
						results.push(GitInstallResultEntry {
							name: skill_path.clone(),
							agent: agent_str.clone(),
							success: false,
							error: Some(
								"Cannot resolve .agents canonical directory"
									.to_string(),
							),
						});
					}
				}
				continue;
			};
			let target_dirs: Vec<std::path::PathBuf> =
				dir_groups.keys().cloned().collect();
			match install_git_skill_universal(
				&full_path,
				&target_dirs,
				&canonical_skills_dir,
				matches!(resource_scope, ResourceScope::ProjectOnly),
			) {
				Ok((skill_name, wrote_master)) => {
					installed = true;
					copied_any |= wrote_master;
					for agents in dir_groups.values() {
						for (agent_str, _) in agents {
							results.push(GitInstallResultEntry {
								name: skill_name.clone(),
								agent: agent_str.clone(),
								success: true,
								error: None,
							});
						}
					}
				}
				Err(e) => {
					for agents in dir_groups.values() {
						for (agent_str, _) in agents {
							results.push(GitInstallResultEntry {
								name: skill_path.clone(),
								agent: agent_str.clone(),
								success: false,
								error: Some(e.body.error.clone()),
							});
						}
					}
				}
			}
		} else {
			for (target_dir, agents) in &dir_groups {
				match install_git_skill_to_dir(&full_path, target_dir) {
					Ok((skill_name, copied)) => {
						installed = true;
						copied_any |= copied;
						for (agent_str, _) in agents {
							results.push(GitInstallResultEntry {
								name: skill_name.clone(),
								agent: agent_str.clone(),
								success: true,
								error: None,
							});
						}
					}
					Err(e) => {
						for (agent_str, _) in agents {
							results.push(GitInstallResultEntry {
								name: skill_path.clone(),
								agent: agent_str.clone(),
								success: false,
								error: Some(e.body.error.clone()),
							});
						}
					}
				}
			}
		}

		if installed {
			let relative_dir = skill_path.replace('\\', "/");
			let parsed_name = skill::parser::parse(&full_path)
				.ok()
				.map(|skill| skill.name);
			if let Some(skill_name) = parsed_name {
				if !should_write_install_lock(
					&skill_name,
					copied_any,
					resource_scope,
					project_root.as_deref(),
				) {
					continue;
				}
				// Hash the SOURCE repo subfolder in the temp clone.
				let source_dir = get_skill_root(full_path.clone());
				write_skill_install_lock(
					&skill_name,
					resource_scope,
					project_root.as_deref(),
					&source,
					Some(skill::lock_skill_file_path(&relative_dir)),
					&source_dir,
				)?;
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
			format!(
				"Synced skill name '{}' does not match installed skill '{}'",
				parsed_skill.name, req.name
			),
			"SKILL_NAME_MISMATCH",
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

	update_lock_hash(
		&req.name,
		&req.scope,
		project_root.as_deref(),
		&updated_hash,
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

	#[test]
	fn git_install_groups_agents_by_primary_target_dir() {
		let project_root = std::path::PathBuf::from("/tmp/demo");
		let (groups, invalid) = build_git_install_groups(
			&["claude".into(), "opencode".into(), "codex".into()],
			ResourceScope::ProjectOnly,
			Some(&project_root),
		);

		assert!(invalid.is_empty());
		assert_eq!(groups.len(), 3);
		assert!(groups.contains_key(&project_root.join(".claude/skills")));
		assert!(groups.contains_key(&project_root.join(".opencode/skills")));
		assert!(groups.contains_key(&project_root.join(".agents/skills")));
	}

	#[test]
	fn git_install_marks_same_primary_dir_agents_success() {
		let _guard = crate::routes::test_env_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		let temp = tempdir().unwrap();
		let target_dir = temp.path().join("shared");
		let source_dir = temp.path().join("source/hello-skill");
		std::fs::create_dir_all(&source_dir).unwrap();
		std::fs::write(
			source_dir.join("SKILL.md"),
			"---\nname: hello-skill\ndescription: hi\n---\n\n# Hello\n",
		)
		.unwrap();

		let result =
			install_git_skill_to_dir(&source_dir.join("SKILL.md"), &target_dir)
				.unwrap_or_else(|e| panic!("{}", e.body.error));
		assert_eq!(result, ("hello-skill".to_string(), true));
		assert!(target_dir.join("hello-skill/SKILL.md").exists());

		let second =
			install_git_skill_to_dir(&source_dir.join("SKILL.md"), &target_dir)
				.unwrap_or_else(|e| panic!("{}", e.body.error));
		assert_eq!(second, ("hello-skill".to_string(), false));
		assert!(target_dir.join("hello-skill/SKILL.md").exists());
	}

	#[test]
	fn git_install_existing_folder_without_lock_writes_lock() {
		let _guard = crate::routes::test_env_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		let temp = tempdir().unwrap();
		let project = temp.path().join("project");
		let target_dir = project.join(".claude/skills");
		let source_dir = temp.path().join("source/hello-skill");
		std::fs::create_dir_all(&source_dir).unwrap();
		std::fs::write(
			source_dir.join("SKILL.md"),
			"---\nname: hello-skill\ndescription: hi\n---\n\n# Hello\n",
		)
		.unwrap();

		install_git_skill_to_dir(&source_dir.join("SKILL.md"), &target_dir)
			.unwrap_or_else(|e| panic!("{}", e.body.error));
		let (skill_name, copied) =
			install_git_skill_to_dir(&source_dir.join("SKILL.md"), &target_dir)
				.unwrap_or_else(|e| panic!("{}", e.body.error));

		assert!(!copied);
		assert!(should_write_install_lock(
			&skill_name,
			copied,
			ResourceScope::ProjectOnly,
			Some(&project),
		));

		write_skill_install_lock(
			&skill_name,
			ResourceScope::ProjectOnly,
			Some(&project),
			&skill::InstallLockSource {
				source: "owner/repo".to_string(),
				source_type: "github".to_string(),
				source_url: "https://github.com/owner/repo".to_string(),
				ref_name: Some("main".to_string()),
			},
			Some(skill::lock_skill_file_path("hello-skill")),
			&source_dir,
		)
		.unwrap_or_else(|e| panic!("{}", e.body.error));

		let lock = skill::lock::local::read_local_lock(Some(&project));
		assert!(lock.skills.contains_key("hello-skill"));
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
		assert!(!project_root.join(".agents/skills/repo-helper").exists());
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
}
