use aghub_cc_plugins::claude::ClaudePluginManager;
#[cfg(test)]
use aghub_core::create_adapter;
use aghub_core::{
	errors::ConfigError,
	load_all_agents,
	manager::skill::SkillPatch,
	models::{AgentType, ResourceScope, Skill},
	registry, transfer, WriteScope,
};
use rocket::http::Status;
use rocket::serde::json::Json;
use std::{
	path::{Path, PathBuf},
	time::Duration,
};
use tokio::time::timeout;

use crate::{
	blocking::in_mutation_pool,
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
		SkillContentQuery, SkillHoldersResponse, SkillLockEntryResponse,
		SkillResponse, SkillTreeNodeKind, SkillTreeNodeResponse,
		SkillTreeQuery, SkillUsageResponse, UpdateSkillRequest,
	},
	dto::transfer::{
		OperationBatchResponse, ReconcileRequest, TransferRequest,
	},
	error::{ApiCreated, ApiError, ApiResult},
	extractors::{AgentParam, ScopeParams, TrustedLocalOrigin},
	routes::{
		build_manager_from_resolved, require_writable_scope,
		resolved_to_resource_scope,
	},
	skills::rename::skill_renamed_message,
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
	/// Comma list of every agent this one action deletes from; see
	/// `routes::requested_delete_agents`.
	agents: Option<String>,
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
	aghub_core::skills::removal::expand_tilde_path(std::path::Path::new(path))
}

async fn list_branches_for_scan<F>(
	cached_branches: Option<Vec<String>>,
	fetcher: F,
) -> Result<Vec<String>, ApiError>
where
	F: FnOnce() -> Result<Vec<String>, skill_update::SkillRepoError>
		+ Send
		+ 'static,
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
				format!(
					"Failed to list remote branches: {}",
					e.detail().unwrap_or(e.code())
				),
				"BRANCHES_ERROR",
			)
		})
}

#[post("/skills/transfer", data = "<body>")]
pub async fn transfer_skill_route(
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
	// Installs into every destination, so it takes the mutation lock per target.
	in_mutation_pool(move || {
		let result = transfer::transfer_skill(source, destinations)
			.map_err(ApiError::from)?;
		Ok(Json(result.into()))
	})
	.await
}

#[post("/skills/reconcile", data = "<body>")]
pub async fn reconcile_skill_route(
	_origin: TrustedLocalOrigin,
	body: Json<ReconcileRequest>,
) -> ApiResult<OperationBatchResponse> {
	let req = body.into_inner();
	// Read the gate BEFORE the vec fields below move out of `req`.
	let confirm = req.confirmed();
	let source = req.source.to_core()?;

	let added = crate::extractors::resolve_agent_strings(
		req.added.as_deref().unwrap_or(&[]),
	)?;
	let removed = crate::extractors::resolve_agent_strings(
		req.removed.as_deref().unwrap_or(&[]),
	)?;

	// Installs into `added` and removes from `removed`, both under the lock.
	in_mutation_pool(move || {
		let result = transfer::reconcile_skill(source, added, removed, confirm)
			.map_err(ApiError::from)?;
		Ok(Json(result.into()))
	})
	.await
}

#[delete("/skills/by-path", data = "<body>")]
pub async fn delete_skill_by_path(
	_origin: TrustedLocalOrigin,
	body: Json<DeleteSkillByPathRequest>,
) -> ApiResult<DeleteSkillByPathResponse> {
	let req = body.into_inner();

	let write_scope = crate::extractors::resolve_write_scope(
		&req.scope,
		req.project_root.as_deref(),
	)?;

	let requested_agents =
		crate::extractors::resolve_agent_strings(&req.agents)?;

	let raw_path = std::path::PathBuf::from(&req.source_path);
	let plugin_roots = ClaudePluginManager::owned_roots().await;

	let confirm = req.confirm.unwrap_or(false);
	let dry_run = !confirm;

	in_mutation_pool(move || {
		let request = aghub_core::skills::removal::SkillRemovalRequest {
			target: aghub_core::skills::removal::SkillRemovalTarget::ByPath(
				raw_path,
			),
			scope: write_scope,
			agents: requested_agents,
			dry_run,
			all_agents: false,
			prior_removed_paths: Vec::new(),
			keeps_master: false,
			plugin_roots,
		};
		let resp = aghub_core::skills::removal::remove_skill_batch(&request)
			.map_err(ApiError::from)?;

		let single = resp.to_single_view(dry_run).map_err(ApiError::from)?;

		let (pruned_lock_entries, would_prune_lock_entries, prune_error) =
			super::project_prune_status(single.prune);

		let (still_read_by, still_read_by_managed, still_read_by_unmanaged) =
			single.holders.to_options();

		let error = resp
			.rows
			.iter()
			.find(|r| {
				matches!(
					r.verdict,
					aghub_core::skills::removal::Verdict::Refused { .. }
				)
			})
			.and_then(|r| r.error.clone());

		Ok(Json(DeleteSkillByPathResponse {
			success: single.removal_view.success,
			dry_run: single.removal_view.dry_run,
			executed: single.removal_view.executed,
			needs_confirm: single.removal_view.needs_confirm,
			paths: single.removal_view.paths,
			skipped: single.removal_view.skipped,
			deleted_path: single.removal_view.deleted_path,
			pruned_lock_entries,
			would_prune_lock_entries,
			prune_error,
			outcome: single.removal_view.outcome.into(),
			error,
			validation_errors: None,
			still_read_by,
			still_read_by_managed,
			still_read_by_unmanaged,
			code: single.code.map(|s| s.to_string()),
		}))
	})
	.await
}

/// Disk-reconciled, lock-only prune (renamed to avoid colliding with
/// `transfer::reconcile_skill` / `POST /skills/reconcile`). Defaults to a
/// dry-run; `confirm: true` writes. Any disk-scan error aborts the prune and is
/// reported in `error` with the lock left untouched.
#[post("/skills/prune-lock", data = "<body>")]
pub async fn prune_lock_route(
	_origin: TrustedLocalOrigin,
	body: Json<PruneLockRequest>,
) -> ApiResult<PruneLockResponse> {
	use aghub_core::skills::prune::{preview_prune, prune_lock_scanning};
	let req = body.into_inner();

	let write_scope = match crate::extractors::resolve_write_scope(
		&req.scope,
		req.project_root.as_deref(),
	) {
		Ok(scope) => scope,
		Err(err) => {
			return Ok(Json(PruneLockResponse {
				pruned: vec![],
				dry_run: true,
				error: Some(err.body.error),
			}));
		}
	};
	let dry_run = !req.confirm.unwrap_or(false);

	// A commit takes the mutation lock across scan + rewrite; the dry-run preview
	// does not, but it still scans disk, so both belong off the async worker.
	in_mutation_pool(move || {
		let result = if dry_run {
			preview_prune(&write_scope)
		} else {
			prune_lock_scanning(&write_scope)
		};

		match result {
			Ok(pruned) => Ok(Json(PruneLockResponse {
				pruned,
				dry_run,
				error: None,
			})),
			// This route reports failures in its `error` field rather than as an
			// HTTP status — including contention, whose message already says
			// nothing was scanned or written.
			Err(e) => Ok(Json(PruneLockResponse {
				pruned: vec![],
				dry_run,
				error: Some(e.to_string()),
			})),
		}
	})
	.await
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

#[cfg(test)]
fn resolve_git_install_target_dir(
	agent_type: AgentType,
	resource_scope: ResourceScope,
	project_root: Option<&std::path::PathBuf>,
) -> Option<std::path::PathBuf> {
	create_adapter(agent_type)
		.target_skills_dir(project_root.map(|p| p.as_path()), resource_scope)
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
/// Symlinks are NOT blanket-rejected: a Referrer is a symlink at the Master, so
/// the Master must show up. A symlink is followed only when its canonical
/// target stays inside the allow-listed `roots`; one escaping them is skipped
/// silently. The top-level `path` is already asserted contained by the caller.
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
			// Skip escaping symlinks instead of erroring the whole tree.
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

/// A tree entry is renderable if it is not a link, OR a link whose canonical
/// target stays inside `roots`. Escaping links are excluded silently.
fn entry_allowed(path: &std::path::Path, roots: &[PathBuf]) -> bool {
	// Recognize a Windows junction too (`is_symlink()` is false for one), or it
	// would skip the containment guard.
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
		return Err(ApiError::from(ConfigError::unsupported_op(format!(
			"Agent '{}' does not support skills in {:?} scope",
			descriptor.id, scope
		))));
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

/// Masters in one scope's store that NO agent reads (every agent unticked).
///
/// Every per-agent listing misses them by construction, so this is the one
/// place the desktop learns they exist — and can offer to re-grant or delete
/// them. Empty for the `all` scope: a store belongs to exactly one scope.
#[get("/skills/withheld?<scope..>")]
pub fn list_withheld_skills(
	_origin: TrustedLocalOrigin,
	scope: ScopeParams,
) -> ApiResult<Vec<SkillResponse>> {
	let resolved = scope.resolve()?;
	let (resource_scope, project_root) = resolved_to_resource_scope(&resolved);
	let masters = aghub_core::skills::discovery::withheld_masters(
		resource_scope,
		project_root.as_deref(),
	)
	.map_err(|e| ApiError::from(ConfigError::Io(e)))?;
	Ok(Json(masters.iter().map(SkillResponse::from).collect()))
}

/// Holders of a skill split into managed and unmanaged agents.
#[get("/skills/<name>/holders?<scope..>")]
pub fn get_skill_holders(
	_origin: TrustedLocalOrigin,
	name: &str,
	scope: ScopeParams,
) -> ApiResult<SkillHoldersResponse> {
	let resolved = scope.resolve()?;
	require_writable_scope(&resolved)?;
	let (resource_scope, project_root) = resolved_to_resource_scope(&resolved);
	let view = aghub_core::skills::removal::batch::get_skill_holders(
		name,
		resource_scope,
		project_root.as_deref(),
	);
	Ok(Json(SkillHoldersResponse::from(view)))
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
	// `add_skill` takes the mutation lock (Master write + link).
	in_mutation_pool(move || {
		manager.add_skill(skill).map_err(ApiError::from)?;
		Ok((Status::Created, Json(response)))
	})
	.await
}

#[post("/agents/<agent>/skills/import?<scope..>", data = "<body>")]
pub async fn import_skill(
	_origin: TrustedLocalOrigin,
	agent: AgentParam,
	scope: ScopeParams,
	body: Json<crate::dto::skill::ImportSkillRequest>,
) -> ApiResult<SkillResponse> {
	let resolved = scope.resolve()?;
	let (resource_scope, _) = resolved_to_resource_scope(&resolved);
	check_skills_mutable(&agent, resource_scope)?;
	let write_scope = resolved.to_write_scope()?;
	let request = body.into_inner();
	let agent_type = agent.0;

	in_mutation_pool(move || {
		let path = std::path::Path::new(&request.path);
		let install_req =
			aghub_core::skills::install_local::LocalSkillInstallRequest {
				source_path: path,
				scope: write_scope,
				target_agents: &[agent_type],
				install_name: request.name.as_deref(),
			};
		let report =
			aghub_core::skills::install_local::install_local_skill(install_req)
				.map_err(ApiError::from)?;

		Ok(Json(SkillResponse::from(
			&aghub_core::dto::SkillView::from(&report.skill)
				.with_already_installed(report.already_installed),
		)))
	})
	.await
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
	let write_scope = resolved.to_write_scope()?;
	let mut manager = super::build_manager_from_resolved(
		&agent,
		&write_scope.clone().into(),
	)?;
	manager.load().map_err(ApiError::from)?;
	let existing = manager
		.get_skill(name)
		.ok_or_else(|| {
			ApiError::from(ConfigError::resource_not_found("skill", name))
		})?
		.clone();
	let updated = SkillPatch::from(body.into_inner()).apply_to(existing);
	let response = SkillResponse::from(&updated);
	let name = name.to_string();
	let plugin_roots = ClaudePluginManager::owned_roots().await;
	// `update_skill` takes the mutation lock (a rename is a Master move plus a
	// relink of every Referrer).
	in_mutation_pool(move || {
		manager
			.update_skill(&name, updated, &plugin_roots)
			.map_err(ApiError::from)?;
		Ok(Json(response))
	})
	.await
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
	let write_scope = resolved.to_write_scope()?;
	let mut manager = super::build_manager_from_resolved(
		&agent,
		&write_scope.clone().into(),
	)?;
	// No `ConfigError::NotFound` arm: nothing constructs that variant; a missing
	// config surfaces as `Io(NotFound)` and takes the normal error path.
	manager.load().map_err(ApiError::from)?;
	let requested =
		super::requested_delete_agents(agent.0, params.agents.as_deref())?;
	let confirm = params.confirm.unwrap_or(false);
	let dry_run = !confirm;
	let name = name.to_string();
	let all_agents = params.all_agents.unwrap_or(false);
	let plugin_roots = ClaudePluginManager::owned_roots().await;

	in_mutation_pool(move || {
		// Shared batch removal entry; projects outcome and managed/unmanaged holders.
		// See docs/history/api.md#by-name-delete-and-reconcile-removal-rows-wire-fields.
		let request = aghub_core::skills::removal::SkillRemovalRequest {
			target: aghub_core::skills::removal::SkillRemovalTarget::ByName(
				name.clone(),
			),
			scope: write_scope,
			agents: requested,
			dry_run,
			all_agents,
			prior_removed_paths: Vec::new(),
			keeps_master: false,
			plugin_roots,
		};
		let resp = aghub_core::skills::removal::remove_skill_batch(&request)
			.map_err(ApiError::from)?;

		let single = resp.to_single_view(dry_run).map_err(ApiError::from)?;

		let (pruned_lock_entries, would_prune_lock_entries, prune_error) =
			super::project_prune_status(single.prune);

		let (still_read_by, still_read_by_managed, still_read_by_unmanaged) =
			single.holders.to_options();

		Ok(Json(DeleteSkillByPathResponse {
			success: single.removal_view.success,
			dry_run: single.removal_view.dry_run,
			executed: single.removal_view.executed,
			needs_confirm: single.removal_view.needs_confirm,
			paths: single.removal_view.paths,
			skipped: single.removal_view.skipped,
			deleted_path: single.removal_view.deleted_path,
			pruned_lock_entries,
			would_prune_lock_entries,
			prune_error,
			outcome: single.removal_view.outcome.into(),
			error: None,
			validation_errors: None,
			still_read_by,
			still_read_by_managed,
			still_read_by_unmanaged,
			code: single.code.map(|s| s.to_string()),
		}))
	})
	.await
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
	let write_scope = resolved.to_write_scope()?;
	let mut manager = super::build_manager_from_resolved(
		&agent,
		&write_scope.clone().into(),
	)?;
	manager.load().map_err(ApiError::from)?;
	let plugin_roots = ClaudePluginManager::owned_roots().await;
	manager
		.enable_skill(name, &plugin_roots)
		.map_err(ApiError::from)?;
	// `enable_skill` always refuses (nothing persists the flag); the refusal
	// order (not-found, plugin-managed, unsupported) lives in core.
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
	let write_scope = resolved.to_write_scope()?;
	let mut manager = super::build_manager_from_resolved(
		&agent,
		&write_scope.clone().into(),
	)?;
	manager.load().map_err(ApiError::from)?;
	let plugin_roots = ClaudePluginManager::owned_roots().await;
	manager
		.disable_skill(name, &plugin_roots)
		.map_err(ApiError::from)?;
	// Unreachable today — the refusal order (not-found, plugin-managed,
	// unsupported) lives in core; see `enable_skill` above.
	let skill = manager
		.get_skill(name)
		.expect("skill present after disable");
	Ok(Json(SkillResponse::from(skill)))
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

// HTTP 200 means the batch was handled, not that its targets were installed.
// Log the same safe messages the client receives, never tokens/source URLs.
fn log_install_results(
	operation: &str,
	scope: ResourceScope,
	results: &[GitInstallResultEntry],
) {
	for row in results.iter().filter(|row| !row.success) {
		log::warn!(
			"skill install failed: operation={operation} scope={scope:?} skill={:?} agent={:?} reason={:?}",
			row.name,
			row.agent,
			row.error.as_deref().filter(|e| !e.trim().is_empty()).unwrap_or("No failure reason was returned"),
		);
	}
}

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
	scope: WriteScope,
	target_agents: &[AgentType],
) -> Result<
	aghub_core::skills::install_fetched::FetchedSkillInstallReport,
	String,
> {
	let skill_file =
		aghub_core::skills::update::sanitize_skill_path(root, lock_skill_path)
			.ok_or_else(|| INVALID_FETCHED_SKILL_PATH.to_string())?;
	let target = match scope {
		WriteScope::Project { .. } => {
			aghub_core::skills::linker::LinkTarget::Relative
		}
		WriteScope::Global => aghub_core::skills::linker::LinkTarget::Absolute,
	};
	aghub_core::skills::install_fetched::install_fetched_skill_and_lock(
		aghub_core::skills::install_fetched::FetchedSkillInstallRequest {
			skill_file: &skill_file,
			source,
			lock_skill_path: lock_skill_path.to_string(),
			ref_commit: ref_commit.map(str::to_string),
			scope,
			target_agents,
			expected_name: None,
			target,
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
	let write_scope = crate::extractors::resolve_write_scope(
		&req.scope,
		req.project_path.as_deref(),
	)?;
	let resource_scope = write_scope.resource_scope();

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
		let agents: Vec<GitInstallResultEntry> = parsed_agents
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
		log_install_results("skills/install", resource_scope, &agents);
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
			let install_all = req.install_all.unwrap_or(false);
			let requested = req.skills.clone();
			let repo_for_task = repo.clone();
			let token_for_task = token;
			let scope_for_task = write_scope.clone();
			let source_for_task = req.source.clone();

			let (selected, fetched, ref_name) = match timeout(
				Duration::from_secs(300),
				tokio::task::spawn_blocking(move || {
					// Fetch the ref the lock will record: an existing cohort's ref (a recorded
					// `None` = the default branch), else the default branch (`sources::import_ref`).
					let recorded = skill_update::sources::recorded_refs(
						&scope_for_task,
						&source_for_task,
					);
					// Name the ref FIRST, then resolve exactly that ref: resolving
					// HEAD and asking its name in a second call races a default
					// branch switch (bytes from one branch, lock naming another).
					let fetch_ref = match recorded.first() {
						Some(cohort) => cohort.clone(),
						None => repo_for_task.default_branch(
							&skill_update::SourceRef {
								source: source_for_task.clone(),
								ref_: None,
							},
							token_for_task.as_deref(),
						),
					};
					let claim = repo_for_task
						.resolve_pinned(
							&skill_update::SourceRef {
								source: source_for_task.clone(),
								ref_: fetch_ref.clone(),
							},
							token_for_task.as_deref(),
						)
						.map_err(InstallFetchError::Repo)?;
					let ref_name = skill_update::sources::import_ref(
						None,
						&recorded,
						|| fetch_ref.clone(),
					);
					let catalog = repo_for_task
						.list_pinned(&claim)
						.map_err(InstallFetchError::Repo)?;
					let selected = select_catalog_paths(
						&catalog.skills,
						&requested,
						install_all,
					)?;
					let paths: Vec<skill::SkillPath> =
						selected.iter().map(|(_, p)| p.clone()).collect();
					let fetched = repo_for_task
						.fetch_pinned(
							&claim,
							skill_update::FetchSelection::Skills(&paths),
						)
						.map_err(InstallFetchError::Repo)?;
					Ok::<_, InstallFetchError>((selected, fetched, ref_name))
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

			let lock_source =
				install_lock_source_from_resolved(&resolved, ref_name);
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

	// The install loop below is fully synchronous and takes the mutation lock per
	// skill, so it runs on the blocking pool. Everything above — scope parsing,
	// agent preflight, the fetch — is unchanged and still decides errors first.
	in_mutation_pool(move || {
		let mut agent_rows: Vec<GitInstallResultEntry> = Vec::new();
		for (name, lock_skill_path) in &items {
			let installed = match &materialization {
				InstallMaterialization::Fetched(fetched) => {
					skill_update::mutation::install_fetched_source(
						fetched,
						skill_update::mutation::FetchedInstallRequest {
							source: &lock_source,
							lock_skill_path,
							expected_name: None,
							scope: write_scope.clone(),
							target_agents: &agent_types,
							expected_ref: lock_source.ref_name.as_deref(),
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
					write_scope.clone(),
					&agent_types,
				),
			};
			match installed {
				Ok(report) => {
					for ((agent_str, _), agent_result) in
						target_agents.iter().zip(report.agent_results)
					{
						let success = agent_result.error.is_none();
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

		// Aggregate over OUTCOMES: `installed` is false for an idempotent
		// re-install, which is still a success.
		let success =
			!agent_rows.is_empty() && agent_rows.iter().all(|r| r.success);
		log_install_results("skills/install", resource_scope, &agent_rows);
		Ok(Json(InstallSkillResponse {
			success,
			agents: agent_rows,
		}))
	})
	.await
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

/// Assert `path` canonicalizes inside the scope's allow-listed skills roots —
/// the same containment as `delete_skill_by_path`, so content/tree reads cannot
/// escape via `..` or an out-of-tree symlink. A path that does NOT exist is 404
/// (`not_found_code`); only an existing path outside the roots is 403.
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
	// Thread the roots down so a Referrer entry (`<agent>/skills/foo ->
	// .aghub/foo`) is included when it stays inside them, skipped otherwise.
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
	// The same `SkillRepository` instance is retained on the session, together
	// with the claim it minted, so a later install/sync `fetch` stays pinned to
	// this commit.
	let repo = std::sync::Arc::new(skill_update::SkillRepository::new());
	let source_ref = skill_update::SourceRef {
		source: req.url.clone(),
		ref_: req.branch.clone(),
	};
	let token_for_scan = credential_token.clone();
	let repo_for_scan = repo.clone();
	let (claim, skills, import_ref) = tokio::task::spawn_blocking(move || {
		let (claim, skills) = scan_repo_catalog(
			&repo_for_scan,
			&source_ref,
			token_for_scan.as_deref(),
		)?;
		let import_ref = repo_for_scan.import_ref(&claim);
		Ok::<_, skill_update::SkillRepoError>((claim, skills, import_ref))
	})
	.await
	.map_err(|e| {
		ApiError::from_join_error(e, "Scan task failed", "SCAN_ERROR")
	})?
	.map_err(map_skill_repo_error)?;

	// List remote branches (use cache from previous session if
	// available to avoid an extra network call on branch switch)
	let repo_for_branches = repo.clone();
	let branch_source = skill_update::SourceRef {
		source: req.url.clone(),
		ref_: None,
	};
	let credential_token_for_branches = credential_token.clone();
	let branches = list_branches_for_scan(cached_branches, move || {
		repo_for_branches.list_branches(
			&branch_source,
			credential_token_for_branches.as_deref(),
		)
	})
	.await?;

	// Display + session: the asked branch, else the remote's real default branch, else "". The install decides what the lock records (`sources::import_ref`).
	let current_branch = import_ref.unwrap_or_default();

	// Store the commit-pinned repository handle until install/sync.
	let session_id = uuid::Uuid::new_v4().to_string();
	// The session module owns the 10-minute lifetime and eviction policy. The
	// browse-then-install window stays tight so credentials and any internal gix
	// shallow-clone cache are not retained longer than needed.
	let session = PinnedSourceSession::new(
		repo,
		claim,
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
	(skill_update::PinnedSnapshot, Vec<GitScanSkillEntry>),
	skill_update::SkillRepoError,
> {
	let claim = repo.resolve_pinned(source_ref, token)?;
	let catalog = repo.list_pinned(&claim)?;
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
	Ok((claim, skills))
}

fn map_skill_repo_error(e: skill_update::SkillRepoError) -> ApiError {
	use skill_update::SkillRepoError;
	match e {
		SkillRepoError::Auth => ApiError::new(
			Status::BadRequest,
			"Failed to access repository: authentication required",
			"CLONE_FAILED",
		),
		// Detail dropped on purpose — see skills_update.rs.
		SkillRepoError::Network(_) => ApiError::new(
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

/// `(valid (raw, parsed), invalid (raw, message))`, both in request order.
type InstallAgentPartition = (Vec<(String, AgentType)>, Vec<(String, String)>);

/// Partition `agents` (raw strings from the request) into valid/invalid
/// entries in request order. Invalid entries carry the error message to
/// surface back to the caller.
///
/// Valid means the id parses. Scope support is intentionally NOT filtered
/// here: the deep install seam must see every known requested target so its
/// all-target preflight can reject a mixed list before writing the Master.
/// Invalid means an unknown raw agent id.
fn partition_install_agents_in_request_order(
	agents: &[String],
) -> InstallAgentPartition {
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
	let resolved = aghub_git::resolve_remote_source(session.url())
		.map_err(map_remote_source_error)?;

	let write_scope = crate::extractors::resolve_write_scope(
		&req.scope,
		req.project_root.as_deref(),
	)?;
	let resource_scope = write_scope.resource_scope();
	let ref_name = skill_update::sources::import_ref(
		session.requested_branch(),
		&skill_update::sources::recorded_refs(&write_scope, session.url()),
		// The scan already paid for the default branch's name.
		|| Some(session.current_branch().to_string()).filter(|b| !b.is_empty()),
	);
	// The session holds the scanned branch's commit; recording another ref (the
	// scope's cohort) would pin these bytes to a branch they did not come from.
	if let Some(cohort) = ref_name
		.as_deref()
		.filter(|r| *r != session.current_branch())
	{
		return Err(ApiError::new(
			Status::BadRequest,
			format!(
				"This source is already installed in this scope from '{cohort}'; nothing was written. Re-scan with branch '{cohort}' to install from it"
			),
			skill_update::mutation::SKILL_SOURCE_MISMATCH_CODE,
		));
	}
	let source = install_lock_source_from_resolved(&resolved, ref_name);
	// Only a ref taken from the scope's cohort is re-checked under the lock.
	let expected_ref = source
		.ref_name
		.clone()
		.filter(|_| session.requested_branch().is_none());

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
		partition_install_agents_in_request_order(&req.agents);

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
		log_install_results("skills/git/install", resource_scope, &results);
		return Ok(Json(GitInstallResponse { results }));
	}

	let target_agents: Vec<AgentType> =
		valid_agents.iter().map(|(_, agent)| *agent).collect();

	// The install loop takes the mutation lock per skill and is synchronous, so it
	// runs on the blocking pool; the fetch above stays on the async worker.
	let skill_paths = req.skill_paths.clone();
	let results = in_mutation_pool(move || {
		for (skill_path, validated) in skill_paths.iter().zip(&validated_paths)
		{
			let lock_skill_path =
				skill::lock_skill_file_path(validated.as_str());
			match skill_update::mutation::install_fetched_source(
				&fetched,
				skill_update::mutation::FetchedInstallRequest {
					source: &source,
					lock_skill_path: &lock_skill_path,
					expected_name: None,
					scope: write_scope.clone(),
					target_agents: &target_agents,
					expected_ref: expected_ref.as_deref(),
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
		Ok(results)
	})
	.await?;

	// Successful request permanently consumes the exclusive session claim.
	session.consume();

	log_install_results("skills/git/install", resource_scope, &results);
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

	// Snapshot the entry's identity BEFORE the fetch. The Resync seam refuses an
	// entry that appeared meanwhile or coordinates it does not name.
	let write_scope = crate::extractors::resolve_write_scope(
		&req.scope,
		req.project_root.as_deref(),
	)?;
	let pre_fetch_identity = aghub_core::skills::lock::EntryIdentity::capture(
		&req.name,
		write_scope.resource_scope(),
		write_scope.project_root(),
	);
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
	// reported before request lock validation. The mutation seam repeats
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
			skill_update::mutation::SKILL_PATH_NOT_FOUND_CODE,
		));
	}

	let locked = match &write_scope {
		WriteScope::Global => {
			skill::lock::global::get_skill_from_lock(&req.name).is_some()
		}
		WriteScope::Project { root } => {
			skill::lock::local::read_local_lock(Some(root))
				.skills
				.contains_key(&req.name)
		}
	};
	if !locked {
		return Err(ApiError::new(
			Status::NotFound,
			format!("Skill '{}' is not present in the lock", req.name),
			skill_update::mutation::SKILL_LOCK_ENTRY_NOT_FOUND_CODE,
		));
	}

	// The post-session transaction (rename guard → containment → swap → lock) is
	// the shared core resync; the route owns only the session lifecycle.
	use crate::skills::resync::safe_resync_error;
	use aghub_core::skills::resync::ResyncError;
	use skill_update::mutation::{
		resync_fetched_source, FetchedResyncRequest, ResyncMutationError,
	};
	// The transaction (rename guard → containment → swap → lock re-stamp) takes the
	// mutation lock and is synchronous, so it runs on the blocking pool. The fetch
	// above stays on the async worker — it must never hold the lock anyway.
	let name = req.name.clone();
	let skill_path = req.skill_path.clone();
	let source_url = session.url().to_string();
	let report = in_mutation_pool(move || {
		resync_fetched_source(
			&fetched,
			FetchedResyncRequest {
				skill_path: &skill_path,
				name: &name,
				scope: write_scope,
				source: &source_url,
				expected: pre_fetch_identity,
			},
		)
		.map_err(|e| {
			let code = e.code();
			match e {
				ResyncMutationError::InvalidSkillPath => ApiError::new(
					Status::NotFound,
					format!(
						"Skill path '{skill_path}' not found in cloned repository"
					),
					code,
				),
				ResyncMutationError::SourceChangedDuringFetch => ApiError::new(
					Status::Conflict,
					format!(
						"Skill '{name}' appeared in the lock while this sync was fetching; nothing was written. Re-run to sync the current entry"
					),
					code,
				),
				ResyncMutationError::SourceMismatch => ApiError::new(
					Status::BadRequest,
					format!(
						"The scanned source or skill path does not match what '{name}' is locked to; nothing was written. Re-scan the skill's own source"
					),
					code,
				),
				ResyncMutationError::Resync(ResyncError::NotInstalled) => {
					ApiError::new(
						Status::NotFound,
						format!(
							"Skill '{name}' is locked but no installed copy was found"
						),
						code,
					)
				}
				ResyncMutationError::Resync(ResyncError::Renamed {
					new_name,
				}) => ApiError::new(
					Status::BadRequest,
					skill_renamed_message(&name, &new_name),
					code,
				),
				ResyncMutationError::Resync(error) => {
					let mapped = safe_resync_error(&error);
					ApiError::new(mapped.status, mapped.message, code)
				}
			}
		})
	})
	.await?;

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
	#[cfg(unix)]
	use crate::routes::skills_test_git::{test_git, test_has_git};
	use aghub_core::transfer::{reconcile_skill, ResourceLocator};
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

		// Overriding HOME is NOT enough. `dirs::config_dir()` prefers
		// `XDG_CONFIG_HOME`, and several descriptors honour their own agent
		// variable ahead of both — so a developer's real `~/.config` leaked
		// straight into these tests. It was observed: a live
		// `~/.config/orca/opencode-hooks/shared/skills` turned up in a test's
		// allow-listed roots, which makes the outcome depend on what happens to
		// be installed on the machine. Same list as the CLI harness's
		// `clear_agent_home_overrides`, for the same reason.
		const OVERRIDES: &[&str] = aghub_core::PATH_OVERRIDE_VARS;
		let saved: Vec<(&str, Option<String>)> = OVERRIDES
			.iter()
			.map(|k| (*k, std::env::var(k).ok()))
			.collect();
		for (key, _) in &saved {
			std::env::remove_var(key);
		}
		let old_home = std::env::var("HOME").ok();
		let old_state = std::env::var("XDG_STATE_HOME").ok();
		std::env::set_var("HOME", home.path());
		std::env::set_var("XDG_STATE_HOME", state.path());

		let result = f(home.path(), state.path());

		match old_home {
			Some(v) => std::env::set_var("HOME", v),
			None => std::env::remove_var("HOME"),
		}
		match old_state {
			Some(v) => std::env::set_var("XDG_STATE_HOME", v),
			None => std::env::remove_var("XDG_STATE_HOME"),
		}
		for (key, value) in saved {
			match value {
				Some(v) => std::env::set_var(key, v),
				None => std::env::remove_var(key),
			}
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
	fn session_from_fixture(
		fixture_root: &std::path::Path,
		url: &str,
		current_branch: &str,
	) -> PinnedSourceSession {
		session_with_backend(
			std::sync::Arc::new(SessionLocalBackend::new(fixture_root)),
			url,
			current_branch,
		)
	}

	/// Calls `resolve_pinned` so the session holds a claim it can fetch from.
	fn session_with_backend(
		backend: std::sync::Arc<dyn aghub_git::RepoFetchBackend>,
		url: &str,
		current_branch: &str,
	) -> PinnedSourceSession {
		let repo = std::sync::Arc::new(
			skill_update::SkillRepository::with_backends(None, backend),
		);
		let ref_ = if current_branch.is_empty() {
			None
		} else {
			Some(current_branch.to_string())
		};
		let snapshot = repo
			.resolve_pinned(
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
		let repo =
			std::sync::Arc::new(skill_update::SkillRepository::with_backends(
				None,
				std::sync::Arc::new(SessionLocalBackend::new(
					std::path::PathBuf::new(),
				)),
			));
		let claim = repo
			.resolve_pinned(
				&skill_update::SourceRef {
					source: url.to_string(),
					ref_: None,
				},
				None,
			)
			.expect("resolve dummy session");
		PinnedSourceSession::new(
			repo,
			claim,
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

	/// Drive an async handler directly from a sync test. NOT `#[cfg(unix)]`: it
	/// started out serving only the unix-gated by-path delete tests, but the
	/// prune-lock and import handlers became `async` (they run their transaction
	/// through `blocking::in_mutation_pool`) and their tests are cross-platform,
	/// so gating this to unix broke the Windows build — which only CI sees.
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

	/// A by-path request the entry refuses: since A6 that is an HTTP error
	/// (status + wire code), never `200` with `success: false`.
	#[cfg(unix)]
	fn by_path_refused(req: DeleteSkillByPathRequest) -> ApiError {
		match block_on(delete_skill_by_path(TrustedLocalOrigin, Json(req))) {
			Ok(resp) => {
				panic!("by-path must be refused, got {:?}", resp.into_inner())
			}
			Err(error) => error,
		}
	}

	#[test]
	fn delete_by_path_rejects_missing_project_root_when_scope_is_project() {
		let req = DeleteSkillByPathRequest {
			source_path: "/some/path/SKILL.md".to_string(),
			agents: vec!["claude".to_string()],
			scope: "project".to_string(),
			project_root: None,
			all_agents: None,
			confirm: Some(false),
		};
		let err = block_on(delete_skill_by_path(TrustedLocalOrigin, Json(req)))
			.unwrap_err();
		assert_eq!(err.status, Status::BadRequest);
		assert_eq!(err.body.code, "PROJECT_ROOT_REQUIRED");
		assert_eq!(err.body.error, "project scope requires a project root");
	}

	#[test]
	fn delete_by_path_validates_scope_before_agents() {
		let req = DeleteSkillByPathRequest {
			source_path: "/some/path/SKILL.md".to_string(),
			agents: vec!["unknown-agent".to_string()],
			scope: "invalid-scope".to_string(),
			project_root: None,
			all_agents: None,
			confirm: Some(false),
		};
		let err = block_on(delete_skill_by_path(TrustedLocalOrigin, Json(req)))
			.unwrap_err();
		assert_eq!(err.status, Status::BadRequest);
		assert_eq!(err.body.code, skill_update::mutation::INVALID_SCOPE_CODE);
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

			let refusal = by_path_refused(by_path_req(&link, Some(true)));
			assert_eq!(refusal.status, rocket::http::Status::BadRequest);
			assert_eq!(refusal.body.code, "INVALID_CONFIG");
			assert!(outside.exists(), "out-of-tree dir must survive");
		});
	}

	#[cfg(unix)]
	#[test]
	fn delete_by_path_keeps_shared_slot_referenced_by_another_agent_symlink() {
		with_isolated_env(|home, _state| {
			// An un-migrated REAL directory in the shared `.agents/skills`
			// slot: read directly by cursor and nine other project agents, and
			// symlinked on top by claude. Deleting it by-path for cursor must
			// NOT remove it — that orphans claude's live symlink and loses the
			// skill for every other slot reader.
			//
			// The store Master (`.aghub/<name>`) is deliberately NOT the target
			// here: no agent reads the store, so a by-path delete can never
			// name it — the route's own per-agent path validation rejects it
			// before any guard runs.
			let proj = home;
			let slot = proj.join(".agents/skills/shared");
			std::fs::create_dir_all(&slot).unwrap();
			std::fs::write(
				slot.join("SKILL.md"),
				"---\nname: shared\ndescription: d\n---\n",
			)
			.unwrap();
			let claude = proj.join(".claude/skills");
			std::fs::create_dir_all(&claude).unwrap();
			std::os::unix::fs::symlink(&slot, claude.join("shared")).unwrap();

			let req = DeleteSkillByPathRequest {
				source_path: slot.join("SKILL.md").display().to_string(),
				agents: vec!["cursor".to_string()],
				scope: "project".to_string(),
				project_root: Some(proj.display().to_string()),
				all_agents: None,
				confirm: Some(true),
			};
			let refusal = by_path_refused(req);
			assert_eq!(
				refusal.status,
				rocket::http::Status::UnprocessableEntity
			);
			assert_eq!(refusal.body.code, "UNSUPPORTED_OPERATION");

			assert!(
				slot.join("SKILL.md").exists(),
				"the shared slot must survive a single-agent by-path delete"
			);
			assert!(
				claude.join("shared").join("SKILL.md").exists(),
				"the other agent's symlink must still resolve into the slot"
			);
		});
	}

	/// The same shared slot, with NO symlink anywhere — the shape the referrer
	/// sweep is blind to.
	///
	/// This branch builds its own plan instead of coming through
	/// `remove_skill_planned`, and its only guard was
	/// `dir_has_external_referrer`. Ten project agents reach a real directory
	/// in `.agents/skills` by SCANNING that directory, leaving no link behind,
	/// so the sweep answered "nobody references it" and the route
	/// `remove_dir_all`'d the directory out from under all of them and reported
	/// `removed` — while `DELETE /agents/<a>/skills/<n>` and `aghub delete`
	/// refused the identical request. This is the delete surface that never
	/// converged on core's answer.
	#[cfg(unix)]
	#[test]
	fn delete_by_path_keeps_shared_slot_read_by_other_agents() {
		with_isolated_env(|home, _state| {
			let proj = home;
			let slot = proj.join(".agents/skills/shared");
			std::fs::create_dir_all(&slot).unwrap();
			std::fs::write(
				slot.join("SKILL.md"),
				"---\nname: shared\ndescription: d\n---\n",
			)
			.unwrap();

			let req = DeleteSkillByPathRequest {
				source_path: slot.join("SKILL.md").display().to_string(),
				agents: vec!["cursor".to_string()],
				scope: "project".to_string(),
				project_root: Some(proj.display().to_string()),
				all_agents: None,
				confirm: Some(true),
			};
			let refusal = by_path_refused(req);

			let by_name = match block_on(delete_skill(
				TrustedLocalOrigin,
				AgentParam(AgentType::Cursor),
				"shared",
				DeleteSkillParams {
					scope: Some("project".to_string()),
					project_root: Some(proj.display().to_string()),
					confirm: Some(true),
					all_agents: None,
					agents: Some("cursor".to_string()),
				},
			)) {
				Ok(resp) => panic!(
					"by-name must refuse the same slot, got {:?}",
					resp.into_inner()
				),
				Err(error) => error,
			};
			assert_eq!(
				refusal.status,
				rocket::http::Status::UnprocessableEntity
			);
			assert_eq!(refusal.body.code, "UNSUPPORTED_OPERATION");
			assert_eq!(
				(refusal.status, refusal.body.code),
				(by_name.status, by_name.body.code),
				"by-path and by-name must answer one slot with one status and code"
			);
			let targets = refusal
				.body
				.rejected_targets
				.as_deref()
				.expect("rejected_targets on by-path");
			assert_eq!(targets.len(), 1);
			assert_eq!(targets[0].kind.as_deref(), Some("shared"));
			let by_name_targets = by_name
				.body
				.rejected_targets
				.as_deref()
				.expect("rejected_targets on by-name");
			assert_eq!(by_name_targets.len(), 1);
			assert_eq!(by_name_targets[0].kind.as_deref(), Some("shared"));

			assert!(
				slot.join("SKILL.md").exists(),
				"a single-agent by-path delete may not take the shared slot \
				 — nine other project agents read it from there"
			);

			// A preview of the same request is not an error: the entity is
			// still there, so the answer is `kept`, not `removed` (the desktop
			// closes its dialog on `removed`).
			let preview = block_on(delete_skill_by_path(
				TrustedLocalOrigin,
				Json(DeleteSkillByPathRequest {
					source_path: slot.join("SKILL.md").display().to_string(),
					agents: vec!["cursor".to_string()],
					scope: "project".to_string(),
					project_root: Some(proj.display().to_string()),
					all_agents: None,
					confirm: None,
				}),
			))
			.ok()
			.expect("a preview returns ok")
			.into_inner();
			assert_eq!(
				preview.outcome,
				crate::dto::skill::RemovalOutcomeKind::Kept
			);
			assert_eq!(preview.code.as_deref(), Some("UNSUPPORTED_OPERATION"));
			// The reader that would have lost it, asked directly.
			let mut other = aghub_core::manager::ConfigManager::new(
				aghub_core::create_adapter(
					aghub_core::models::AgentType::OpenCode,
				),
				false,
				Some(proj),
			);
			other.load().unwrap();
			assert!(
				other.get_skill("shared").is_some(),
				"opencode must not lose a skill because cursor asked to drop \
				 that location"
			);
		});
	}

	#[cfg(unix)]
	#[test]
	fn delete_by_name_and_by_path_preview_parity() {
		with_isolated_env(|home, _state| {
			let proj = home;
			let slot = proj.join(".agents/skills/shared");
			std::fs::create_dir_all(&slot).unwrap();
			std::fs::write(
				slot.join("SKILL.md"),
				"---\nname: shared\ndescription: d\n---\n",
			)
			.unwrap();

			// 1. By-name idempotent-absent delete answers outcome Absent and code None.
			let absent = block_on(delete_skill(
				TrustedLocalOrigin,
				AgentParam(AgentType::Cursor),
				"does-not-exist",
				DeleteSkillParams {
					scope: Some("project".to_string()),
					project_root: Some(proj.display().to_string()),
					confirm: Some(true),
					all_agents: None,
					agents: Some("cursor".to_string()),
				},
			))
			.ok()
			.expect("by-name absent delete must succeed idempotently")
			.into_inner();
			assert_eq!(
				absent.outcome,
				crate::dto::skill::RemovalOutcomeKind::Absent
			);
			assert_eq!(absent.code, None);

			// 2. Parity on refused shared-slot preview: both by-path and by-name
			// answer outcome Kept and code UNSUPPORTED_OPERATION, and report
			// identical managed/unmanaged holders when an unmanaged holder exists.
			with_pinned_data_dir(|data| {
				let mut disabled = std::collections::BTreeSet::new();
				disabled.insert("opencode".to_string());
				aghub_core::agent_settings::write_disabled_agents_in(
					data, &disabled,
				)
				.unwrap();

				let by_path_preview = block_on(delete_skill_by_path(
					TrustedLocalOrigin,
					Json(DeleteSkillByPathRequest {
						source_path: slot
							.join("SKILL.md")
							.display()
							.to_string(),
						agents: vec!["cursor".to_string()],
						scope: "project".to_string(),
						project_root: Some(proj.display().to_string()),
						all_agents: None,
						confirm: None,
					}),
				))
				.ok()
				.expect("by-path preview returns ok")
				.into_inner();

				let by_name_preview = block_on(delete_skill(
					TrustedLocalOrigin,
					AgentParam(AgentType::Cursor),
					"shared",
					DeleteSkillParams {
						scope: Some("project".to_string()),
						project_root: Some(proj.display().to_string()),
						confirm: None,
						all_agents: None,
						agents: Some("cursor".to_string()),
					},
				))
				.ok()
				.expect("by-name preview returns ok")
				.into_inner();

				assert_eq!(
					by_name_preview.outcome,
					crate::dto::skill::RemovalOutcomeKind::Kept
				);
				assert_eq!(by_name_preview.outcome, by_path_preview.outcome);
				assert_eq!(
					by_name_preview.code.as_deref(),
					Some("UNSUPPORTED_OPERATION")
				);
				assert_eq!(by_name_preview.code, by_path_preview.code);
				assert_eq!(
					by_name_preview.still_read_by_unmanaged,
					Some(vec!["opencode".to_string()])
				);
				assert_eq!(
					by_name_preview.still_read_by_unmanaged,
					by_path_preview.still_read_by_unmanaged
				);
				assert_eq!(
					by_name_preview.still_read_by_managed,
					by_path_preview.still_read_by_managed
				);
			});
		});
	}

	/// Error precedence is public contract: an unsupported agent answers
	/// UNSUPPORTED_OPERATION even when the scope is read-only (`all`) —
	/// capability is checked BEFORE the writable-scope gate.
	#[test]
	fn delete_skill_capability_beats_readonly_scope() {
		let err = block_on(delete_skill(
			TrustedLocalOrigin,
			AgentParam(AgentType::JetBrainsAi),
			"any-skill",
			DeleteSkillParams {
				scope: Some("all".to_string()),
				project_root: None,
				confirm: None,
				all_agents: None,
				agents: None,
			},
		))
		.expect_err(
			"agent without skill support must fail capability check first",
		);

		assert_eq!(err.status, Status::UnprocessableEntity);
		assert_eq!(err.body.code, "UNSUPPORTED_OPERATION");
	}

	#[cfg(unix)]
	#[test]
	fn test_by_path_plugin_refusal_is_managed_resource() {
		with_isolated_env(|home, _state| {
			use std::os::unix::fs::PermissionsExt;
			let bin_dir = home.join(".local/bin");
			std::fs::create_dir_all(&bin_dir).unwrap();
			let mock_claude = bin_dir.join("claude");

			let skill_dir = home.join(".claude/skills/my-plugin-skill");
			std::fs::create_dir_all(&skill_dir).unwrap();
			std::fs::write(
				skill_dir.join("SKILL.md"),
				"---\nname: my-plugin-skill\ndescription: plugin skill\n---\n",
			)
			.unwrap();

			let claude_dir = home.join(".claude");
			std::fs::create_dir_all(&claude_dir).unwrap();
			std::fs::write(claude_dir.join("settings.json"), "{}").unwrap();

			let script = format!(
				"#!/bin/sh\n\
				if [ \"$1\" = \"plugin\" ] && [ \"$2\" = \"list\" ]; then\n\
					echo '[{{\"id\":\"test-plugin@official\",\"version\":\"1.0.0\",\"scope\":\"user\",\"enabled\":true,\"installPath\":\"{}\",\"installedAt\":\"2026-01-01\",\"lastUpdated\":\"2026-01-01\"}}]'\n\
					exit 0\n\
				fi\n\
				exit 0\n",
				skill_dir.display()
			);
			std::fs::write(&mock_claude, script).unwrap();
			let mut perms =
				std::fs::metadata(&mock_claude).unwrap().permissions();
			perms.set_mode(0o755);
			std::fs::set_permissions(&mock_claude, perms).unwrap();

			// Prepend bin_dir to PATH for ClaudeCli discovery
			let old_path = std::env::var_os("PATH");
			let new_path = match &old_path {
				Some(p) => {
					let mut v = bin_dir.clone().into_os_string();
					v.push(":");
					v.push(p);
					v
				}
				None => bin_dir.into_os_string(),
			};
			std::env::set_var("PATH", &new_path);
			struct PathRestore(Option<std::ffi::OsString>);
			impl Drop for PathRestore {
				fn drop(&mut self) {
					match self.0.take() {
						Some(p) => std::env::set_var("PATH", p),
						None => std::env::remove_var("PATH"),
					}
				}
			}
			let _restore = PathRestore(old_path);

			// By-path delete on plugin-owned skill refuses with BadRequest + MANAGED_RESOURCE,
			// matching the by-name delete refusal (core `refuse_plugin_owned`).
			let by_path_err = by_path_refused(DeleteSkillByPathRequest {
				source_path: skill_dir.join("SKILL.md").display().to_string(),
				agents: vec!["claude".to_string()],
				scope: "global".to_string(),
				project_root: None,
				all_agents: None,
				confirm: Some(true),
			});

			assert_eq!(by_path_err.status, Status::BadRequest);
			assert_eq!(by_path_err.body.code, "MANAGED_RESOURCE");
		});
	}

	/// Caller must hold `test_env_lock`; this helper does not reacquire the
	/// binary's non-reentrant environment mutex.
	fn with_pinned_data_dir<T>(f: impl FnOnce(&std::path::Path) -> T) -> T {
		let data = tempdir().unwrap();
		let old = std::env::var_os("AGHUB_DATA_DIR");
		std::env::set_var("AGHUB_DATA_DIR", data.path());
		struct Restore(Option<std::ffi::OsString>);
		impl Drop for Restore {
			fn drop(&mut self) {
				match &self.0 {
					Some(val) => std::env::set_var("AGHUB_DATA_DIR", val),
					None => std::env::remove_var("AGHUB_DATA_DIR"),
				}
			}
		}
		let _restore = Restore(old);
		f(data.path())
	}

	#[cfg(unix)]
	#[test]
	fn dotfiles_shared_private_dir_is_kept_by_name_and_by_path_for_either_reader(
	) {
		with_isolated_env(|home, _state| {
			with_pinned_data_dir(|data| {
				let disabled: std::collections::BTreeSet<String> =
					aghub_core::models::AgentType::ALL
						.iter()
						.filter(|agent| {
							!matches!(
								**agent,
								AgentType::Claude
									| AgentType::Cursor | AgentType::OpenCode
							)
						})
						.map(|agent| agent.as_str().to_string())
						.collect();
				aghub_core::agent_settings::write_disabled_agents_in(
					data, &disabled,
				)
				.unwrap();

				let name = "dotfiles-shared";
				let real_dir = home.join(".claude/skills").join(name);
				std::fs::create_dir_all(&real_dir).unwrap();
				std::fs::write(
					real_dir.join("SKILL.md"),
					format!("---\nname: {name}\ndescription: test\n---\n"),
				)
				.unwrap();
				std::fs::create_dir_all(home.join(".cursor")).unwrap();
				let cursor_skills = home.join(".cursor/skills");
				std::os::unix::fs::symlink(
					home.join(".claude/skills"),
					&cursor_skills,
				)
				.unwrap();
				std::fs::create_dir_all(home.join(".opencode")).unwrap();

				for (agent, id, other, source) in [
					(
						AgentType::Cursor,
						"cursor",
						"claude",
						cursor_skills.join(name).join("SKILL.md"),
					),
					(
						AgentType::Claude,
						"claude",
						"cursor",
						real_dir.join("SKILL.md"),
					),
				] {
					let params = |confirm| DeleteSkillParams {
						scope: Some("project".to_string()),
						project_root: Some(home.display().to_string()),
						confirm,
						all_agents: None,
						agents: Some(id.to_string()),
					};
					let preview = block_on(delete_skill(
						TrustedLocalOrigin,
						AgentParam(agent),
						name,
						params(None),
					))
					.ok()
					.expect("by-name preview must return kept")
					.into_inner();
					assert_eq!(
						preview.outcome,
						crate::dto::skill::RemovalOutcomeKind::Kept
					);
					assert!(!preview.executed);
					assert!(preview.paths.is_empty());

					let error = block_on(delete_skill(
						TrustedLocalOrigin,
						AgentParam(agent),
						name,
						params(Some(true)),
					))
					.expect_err(
						"confirmed by-name delete must refuse the keep",
					);
					assert!(
						error.body.error.contains(other),
						"refusal for {id} must name {other}: {}",
						error.body.error
					);

					let by_path_request = |confirm| DeleteSkillByPathRequest {
						source_path: source.display().to_string(),
						agents: vec![id.to_string()],
						scope: "project".to_string(),
						project_root: Some(home.display().to_string()),
						all_agents: None,
						confirm,
					};
					let response = block_on(delete_skill_by_path(
						TrustedLocalOrigin,
						Json(by_path_request(None)),
					))
					.ok()
					.expect("by-path preview must report kept")
					.into_inner();
					assert_eq!(
						response.outcome,
						crate::dto::skill::RemovalOutcomeKind::Kept
					);
					assert!(!response.executed);
					assert!(response.paths.is_empty());

					let refusal = by_path_refused(by_path_request(Some(true)));
					assert_eq!(
						(refusal.status, refusal.body.code),
						(error.status, error.body.code),
						"confirmed by-path must refuse like by-name"
					);

					assert!(real_dir.join("SKILL.md").is_file());
					assert!(std::fs::symlink_metadata(&cursor_skills)
						.unwrap()
						.file_type()
						.is_symlink());
				}
			});
		});
	}

	/// A real directory in a shared slot goes when every other reader is
	/// disabled. The verdict is core's `single_agent_keep_reason`, the same one
	/// the CLI and by-name delete use; see docs/history/core-removal.md
	/// ("Disabled agent blocked a single-agent delete").
	#[cfg(unix)]
	#[test]
	fn delete_by_path_removes_real_shared_dir_when_every_other_reader_is_disabled(
	) {
		with_isolated_env(|home, _state| {
			with_pinned_data_dir(|dir| {
				let disabled: std::collections::BTreeSet<String> =
					aghub_core::models::AgentType::ALL
						.iter()
						.filter(|a| a.as_str() != "cursor")
						.map(|a| a.as_str().to_string())
						.collect();
				aghub_core::agent_settings::write_disabled_agents_in(
					dir, &disabled,
				)
				.unwrap();

				let proj = home;
				let slot = proj.join(".agents/skills/shared");
				std::fs::create_dir_all(&slot).unwrap();
				std::fs::write(
					slot.join("SKILL.md"),
					"---\nname: shared\ndescription: d\n---\n",
				)
				.unwrap();

				let req = DeleteSkillByPathRequest {
					source_path: slot.join("SKILL.md").display().to_string(),
					agents: vec!["cursor".to_string()],
					scope: "project".to_string(),
					project_root: Some(proj.display().to_string()),
					all_agents: None,
					confirm: Some(true),
				};
				let resp = block_on(delete_skill_by_path(
					TrustedLocalOrigin,
					Json(req),
				))
				.ok()
				.expect("handler returned ok")
				.into_inner();

				assert!(
					!slot.join("SKILL.md").exists(),
					"a single-agent by-path delete removes the real shared dir \
					 when every other reader is disabled"
				);
				assert_eq!(
					resp.outcome,
					crate::dto::skill::RemovalOutcomeKind::Removed,
				);
			});
		});
	}

	/// Counterpart of the test above: with an ENABLED reader outside the
	/// request the real directory is kept (same core verdict as the CLI).
	#[cfg(unix)]
	#[test]
	fn delete_by_path_keeps_real_shared_dir_when_another_reader_is_enabled() {
		with_isolated_env(|home, _state| {
			with_pinned_data_dir(|dir| {
				let disabled: std::collections::BTreeSet<String> =
					aghub_core::models::AgentType::ALL
						.iter()
						.filter(|a| {
							a.as_str() != "cursor" && a.as_str() != "opencode"
						})
						.map(|a| a.as_str().to_string())
						.collect();
				aghub_core::agent_settings::write_disabled_agents_in(
					dir, &disabled,
				)
				.unwrap();

				let proj = home;
				let slot = proj.join(".agents/skills/shared");
				std::fs::create_dir_all(&slot).unwrap();
				std::fs::write(
					slot.join("SKILL.md"),
					"---\nname: shared\ndescription: d\n---\n",
				)
				.unwrap();

				let req = DeleteSkillByPathRequest {
					source_path: slot.join("SKILL.md").display().to_string(),
					agents: vec!["cursor".to_string()],
					scope: "project".to_string(),
					project_root: Some(proj.display().to_string()),
					all_agents: None,
					confirm: Some(true),
				};
				let refusal = by_path_refused(req);

				assert!(
					slot.join("SKILL.md").exists(),
					"a single-agent by-path delete keeps the real shared dir \
					 when another reader is enabled"
				);
				assert_eq!(
					refusal.status,
					rocket::http::Status::UnprocessableEntity
				);
				assert_eq!(refusal.body.code, "UNSUPPORTED_OPERATION");
			});
		});
	}

	#[cfg(unix)]
	#[test]
	fn delete_by_path_git_tracked_is_kept_untracked_is_deleted() {
		if !test_has_git() {
			eprintln!("skipping test: git binary unavailable");
			return;
		}

		with_isolated_env(|home, _state| {
			with_pinned_data_dir(|dir| {
				let disabled: std::collections::BTreeSet<String> =
					aghub_core::models::AgentType::ALL
						.iter()
						.filter(|a| a.as_str() != "cursor")
						.map(|a| a.as_str().to_string())
						.collect();
				aghub_core::agent_settings::write_disabled_agents_in(
					dir, &disabled,
				)
				.unwrap();

				let proj = home;
				test_git(proj, &["init", "-q"]);

				// 1. Tracked skill: response outcome is kept, dir remains
				let tracked_slot = proj.join(".agents/skills/tracked");
				std::fs::create_dir_all(&tracked_slot).unwrap();
				std::fs::write(
					tracked_slot.join("SKILL.md"),
					"---\nname: tracked\ndescription: d\n---\n",
				)
				.unwrap();

				test_git(
					proj,
					&["add", "--", ".agents/skills/tracked/SKILL.md"],
				);

				let req = DeleteSkillByPathRequest {
					source_path: tracked_slot
						.join("SKILL.md")
						.display()
						.to_string(),
					agents: vec!["cursor".to_string()],
					scope: "project".to_string(),
					project_root: Some(proj.display().to_string()),
					all_agents: None,
					confirm: Some(true),
				};
				let refusal = by_path_refused(req);

				assert!(
					tracked_slot.join("SKILL.md").exists(),
					"git tracked real dir must remain after delete by path"
				);
				assert_eq!(
					refusal.status,
					rocket::http::Status::UnprocessableEntity
				);
				assert_eq!(refusal.body.code, "UNSUPPORTED_OPERATION");
				let error = refusal.body.error.as_str();
				assert!(
					error.contains("tracked by git")
						&& error.contains("git rm -r --cached")
						&& error.contains(&tracked_slot.display().to_string()),
					"error must include the tracked path and escape command: {error}"
				);
				let targets =
					refusal.body.rejected_targets.as_deref().expect(
						"rejected_targets must be present on git refusal",
					);
				assert_eq!(targets.len(), 1);
				assert_eq!(targets[0].kind.as_deref(), Some("git"));
				assert_eq!(
					targets[0].path.as_deref(),
					Some(tracked_slot.display().to_string().as_str())
				);

				// 2. Untracked skill: deleted
				let untracked_slot = proj.join(".agents/skills/untracked");
				std::fs::create_dir_all(&untracked_slot).unwrap();
				std::fs::write(
					untracked_slot.join("SKILL.md"),
					"---\nname: untracked\ndescription: d\n---\n",
				)
				.unwrap();

				let req_untracked = DeleteSkillByPathRequest {
					source_path: untracked_slot
						.join("SKILL.md")
						.display()
						.to_string(),
					agents: vec!["cursor".to_string()],
					scope: "project".to_string(),
					project_root: Some(proj.display().to_string()),
					all_agents: None,
					confirm: Some(true),
				};
				let resp_untracked = block_on(delete_skill_by_path(
					TrustedLocalOrigin,
					Json(req_untracked),
				))
				.ok()
				.expect("handler returned ok")
				.into_inner();

				assert!(
					!untracked_slot.join("SKILL.md").exists(),
					"untracked real dir must be deleted"
				);
				assert_eq!(
					resp_untracked.outcome,
					crate::dto::skill::RemovalOutcomeKind::Removed,
					"untracked real dir outcome must be removed"
				);
			});
		});
	}

	/// The OTHER direction, and the one the keep-guard must not swallow: the
	/// desktop's location dialog sends EVERY agent installed at that exact
	/// `source_path`, so nobody is left to lose the skill and the location has
	/// to go.
	///
	/// Refusing on "is this shared storage?" alone answers yes for every entry
	/// in the `.agents/skills` slot by construction, which turned that dialog
	/// into a button that can never succeed — the `kept` reply raises "another
	/// agent still reads it" while naming no such agent, because there is
	/// none.
	///
	/// The agent list is COMPUTED, not hardcoded: which agents read a project
	/// `.agents/skills` is a roster fact that moves whenever a descriptor gains
	/// `universal: true`, and a stale literal list would silently degrade this
	/// into the partial-request case above (which the test would then still
	/// pass, for the wrong reason). Asking with `requested: []` yields exactly
	/// the readers, and the route's own per-agent path validation is what pins
	/// the opposite error — an over-broad list is rejected before the guard.
	#[cfg(unix)]
	#[test]
	fn delete_by_path_removes_shared_slot_when_every_reader_is_in_the_request()
	{
		with_isolated_env(|home, _state| {
			let proj = home;
			let slot = proj.join(".agents/skills/shared");
			std::fs::create_dir_all(&slot).unwrap();
			std::fs::write(
				slot.join("SKILL.md"),
				"---\nname: shared\ndescription: d\n---\n",
			)
			.unwrap();

			let readers =
				aghub_core::skills::removal::skill_dir_readers_outside(
					&slot,
					aghub_core::models::ResourceScope::ProjectOnly,
					Some(proj),
					&[],
				);
			assert!(
				readers.len() > 1,
				"the shape under test needs SEVERAL readers of the project \
				 slot, else this is just the single-agent case again: {readers:?}"
			);

			let req = DeleteSkillByPathRequest {
				source_path: slot.join("SKILL.md").display().to_string(),
				agents: readers.iter().map(|id| id.to_string()).collect(),
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

			assert_eq!(
				resp.outcome,
				crate::dto::skill::RemovalOutcomeKind::Removed,
				"every reader of this location asked for it to go, so it goes \
				 — a `kept` here is a dialog the user can never make succeed"
			);
			assert!(
				!slot.exists(),
				"`removed` must mean removed: the desktop closes its dialog \
				 and drops the row on this outcome"
			);
		});
	}

	#[cfg(unix)]
	#[test]
	fn delete_by_path_release_also_unlinks_requested_agents_private_link() {
		with_isolated_env(|home, _state| {
			with_pinned_data_dir(|dir| {
				let disabled: std::collections::BTreeSet<String> =
					aghub_core::models::AgentType::ALL
						.iter()
						.filter(|a| {
							a.as_str() != "cursor" && a.as_str() != "opencode"
						})
						.map(|a| a.as_str().to_string())
						.collect();
				aghub_core::agent_settings::write_disabled_agents_in(
					dir, &disabled,
				)
				.unwrap();

				let proj = home;
				let real_dir = proj.join(".agents/skills/x");
				std::fs::create_dir_all(&real_dir).unwrap();
				std::fs::write(
					real_dir.join("SKILL.md"),
					"---\nname: x\ndescription: d\n---\n",
				)
				.unwrap();

				let cursor_skills = proj.join(".cursor/skills");
				std::fs::create_dir_all(&cursor_skills).unwrap();
				let cursor_link = cursor_skills.join("x");
				std::os::unix::fs::symlink(&real_dir, &cursor_link).unwrap();

				let readers =
					aghub_core::skills::removal::skill_dir_readers_outside(
						&real_dir,
						aghub_core::models::ResourceScope::ProjectOnly,
						Some(proj),
						&[],
					);
				assert_eq!(
					readers,
					vec!["opencode", "cursor"],
					"only opencode and cursor are enabled readers"
				);

				let dry_req = DeleteSkillByPathRequest {
					source_path: real_dir
						.join("SKILL.md")
						.display()
						.to_string(),
					agents: readers.iter().map(|id| id.to_string()).collect(),
					scope: "project".to_string(),
					project_root: Some(proj.display().to_string()),
					all_agents: None,
					confirm: None,
				};
				let dry_resp = block_on(delete_skill_by_path(
					TrustedLocalOrigin,
					Json(dry_req),
				))
				.ok()
				.expect("dry run handler returned ok")
				.into_inner();

				assert!(
					real_dir.join("SKILL.md").exists(),
					"real dir must not be deleted by dry run"
				);
				assert!(
					std::fs::symlink_metadata(&cursor_link).is_ok(),
					"link must not be unlinked by dry run"
				);

				let req = DeleteSkillByPathRequest {
					source_path: real_dir
						.join("SKILL.md")
						.display()
						.to_string(),
					agents: readers.iter().map(|id| id.to_string()).collect(),
					scope: "project".to_string(),
					project_root: Some(proj.display().to_string()),
					all_agents: None,
					confirm: Some(true),
				};
				let resp = block_on(delete_skill_by_path(
					TrustedLocalOrigin,
					Json(req),
				))
				.ok()
				.expect("handler returned ok")
				.into_inner();

				assert_eq!(
					resp.outcome,
					crate::dto::skill::RemovalOutcomeKind::Removed
				);
				assert!(!real_dir.exists(), "real dir must be gone");
				assert!(
					std::fs::symlink_metadata(&cursor_link).is_err(),
					"<root>/.cursor/skills/x no longer exists even as a dangling link"
				);

				assert!(
					dry_resp.paths.contains(&cursor_link.display().to_string()),
					"dry run paths must contain cursor link, got: {:?}",
					dry_resp.paths
				);
				assert!(
					dry_resp.paths.contains(&real_dir.display().to_string()),
					"dry run paths must contain real dir, got: {:?}",
					dry_resp.paths
				);
			});
		});
	}

	#[cfg(unix)]
	#[test]
	fn delete_by_path_removes_shared_referrer_when_every_reader_is_requested() {
		with_isolated_env(|home, _state| {
			let master = home.join(".aghub/full-group-link");
			std::fs::create_dir_all(&master).unwrap();
			std::fs::write(
				master.join("SKILL.md"),
				"---\nname: full-group-link\ndescription: d\n---\n",
			)
			.unwrap();
			let shared = home.join(".agents/skills/full-group-link");
			std::fs::create_dir_all(shared.parent().unwrap()).unwrap();
			std::os::unix::fs::symlink(&master, &shared).unwrap();
			let readers =
				aghub_core::skills::removal::skill_dir_readers_outside(
					shared.parent().unwrap(),
					ResourceScope::GlobalOnly,
					None,
					&[],
				);
			assert!(readers.len() > 1, "fixture requires a shared reader set");

			let response = block_on(delete_skill_by_path(
				TrustedLocalOrigin,
				Json(DeleteSkillByPathRequest {
					source_path: shared.join("SKILL.md").display().to_string(),
					agents: readers.iter().map(|id| id.to_string()).collect(),
					scope: "global".to_string(),
					project_root: None,
					all_agents: None,
					confirm: Some(true),
				}),
			))
			.ok()
			.expect("handler returned ok")
			.into_inner();
			assert_eq!(
				response.outcome,
				crate::dto::skill::RemovalOutcomeKind::Removed,
				"{response:?}"
			);
			assert!(std::fs::symlink_metadata(&shared).is_err());
			assert!(!master.exists());
		});
	}

	/// The by-name route desktop's bulk delete uses: one agent alone may not
	/// take a shared Referrer from the rest of its readers, and `agents`
	/// naming every reader lets the same request remove it.
	#[cfg(unix)]
	#[test]
	fn delete_by_name_removes_shared_referrer_only_with_every_reader() {
		with_isolated_env(|home, _state| {
			let master = home.join(".aghub/by-name-group");
			std::fs::create_dir_all(&master).unwrap();
			std::fs::write(
				master.join("SKILL.md"),
				"---\nname: by-name-group\ndescription: d\n---\n",
			)
			.unwrap();
			let shared = home.join(".agents/skills/by-name-group");
			std::fs::create_dir_all(shared.parent().unwrap()).unwrap();
			std::os::unix::fs::symlink(&master, &shared).unwrap();
			let readers =
				aghub_core::skills::removal::skill_dir_readers_outside(
					shared.parent().unwrap(),
					ResourceScope::GlobalOnly,
					None,
					&[],
				);
			assert!(readers.len() > 1, "fixture requires a shared reader set");
			let params = |agents: Option<String>| DeleteSkillParams {
				scope: Some("global".to_string()),
				project_root: None,
				confirm: Some(true),
				all_agents: None,
				agents,
			};

			let alone = block_on(delete_skill(
				TrustedLocalOrigin,
				AgentParam(AgentType::Cline),
				"by-name-group",
				params(None),
			));
			assert!(
				alone.as_ref().map_or(true, |r| r.outcome
					!= crate::dto::skill::RemovalOutcomeKind::Removed),
				"one agent removed a shared grant"
			);
			assert!(std::fs::symlink_metadata(&shared).is_ok());
			assert!(master.exists());

			let all = readers
				.iter()
				.map(|id| id.to_string())
				.collect::<Vec<_>>()
				.join(",");
			let response = block_on(delete_skill(
				TrustedLocalOrigin,
				AgentParam(AgentType::Cline),
				"by-name-group",
				params(Some(all)),
			))
			.ok()
			.expect("handler returned ok")
			.into_inner();
			assert_eq!(
				response.outcome,
				crate::dto::skill::RemovalOutcomeKind::Removed,
				"{response:?}"
			);
			assert!(std::fs::symlink_metadata(&shared).is_err());
			assert!(!master.exists());
		});
	}

	#[cfg(unix)]
	#[test]
	fn delete_by_name_removes_real_shared_dir_when_request_names_every_enabled_reader(
	) {
		with_isolated_env(|home, _state| {
			with_pinned_data_dir(|dir| {
				let disabled: std::collections::BTreeSet<String> =
					aghub_core::models::AgentType::ALL
						.iter()
						.filter(|a| {
							a.as_str() != "cursor" && a.as_str() != "opencode"
						})
						.map(|a| a.as_str().to_string())
						.collect();
				aghub_core::agent_settings::write_disabled_agents_in(
					dir, &disabled,
				)
				.unwrap();

				// A real project, not HOME: at HOME the project slot IS the
				// global shared slot, which a project delete must not take.
				// See docs/history/core-removal.md#project-delete-reached-the-global-store
				let proj = home.join("proj");
				let slot = proj.join(".agents/skills/shared");
				std::fs::create_dir_all(&slot).unwrap();
				std::fs::write(
					slot.join("SKILL.md"),
					"---\nname: shared\ndescription: d\n---\n",
				)
				.unwrap();

				let req = DeleteSkillParams {
					scope: Some("project".to_string()),
					project_root: Some(proj.display().to_string()),
					confirm: Some(true),
					all_agents: None,
					agents: Some("cursor,opencode".to_string()),
				};
				let resp = block_on(delete_skill(
					TrustedLocalOrigin,
					AgentParam(AgentType::Cursor),
					"shared",
					req,
				))
				.ok()
				.expect("handler returned ok")
				.into_inner();

				assert!(
					!slot.exists(),
					"naming every enabled reader must remove the real shared dir"
				);
				assert_eq!(
					resp.outcome,
					crate::dto::skill::RemovalOutcomeKind::Removed,
				);
			});
		});
	}

	/// `all_agents` must not be a way around the git-tracked refusal that the
	/// single-agent and by-path deletes honour.
	#[cfg(unix)]
	#[test]
	fn delete_by_name_all_agents_refuses_git_tracked_real_dir() {
		if !test_has_git() {
			eprintln!("skipping test: git binary unavailable");
			return;
		}
		with_isolated_env(|home, _state| {
			with_pinned_data_dir(|dir| {
				let disabled: std::collections::BTreeSet<String> =
					aghub_core::models::AgentType::ALL
						.iter()
						.filter(|a| {
							a.as_str() != "cursor" && a.as_str() != "opencode"
						})
						.map(|a| a.as_str().to_string())
						.collect();
				aghub_core::agent_settings::write_disabled_agents_in(
					dir, &disabled,
				)
				.unwrap();

				let proj = home;
				test_git(proj, &["init", "-q"]);
				let slot = proj.join(".agents/skills/tracked-all");
				std::fs::create_dir_all(&slot).unwrap();
				std::fs::write(
					slot.join("SKILL.md"),
					"---\nname: tracked-all\ndescription: d\n---\n",
				)
				.unwrap();
				test_git(
					proj,
					&["add", "--", ".agents/skills/tracked-all/SKILL.md"],
				);

				let req = DeleteSkillParams {
					scope: Some("project".to_string()),
					project_root: Some(proj.display().to_string()),
					confirm: Some(true),
					all_agents: Some(true),
					agents: None,
				};
				let result = block_on(delete_skill(
					TrustedLocalOrigin,
					AgentParam(AgentType::Cursor),
					"tracked-all",
					req,
				));
				let message = match result {
					Ok(resp) => format!("{:?}", resp.into_inner().error),
					Err(error) => error.body.error,
				};
				assert!(
					slot.join("SKILL.md").exists(),
					"all_agents must not delete a git-tracked directory: {message}"
				);
				assert!(
					message.contains("git rm -r --cached"),
					"the refusal must carry the escape command: {message}"
				);
			});
		});
	}

	#[cfg(unix)]
	#[test]
	fn delete_by_name_keeps_real_shared_dir_when_enabled_reader_not_in_request()
	{
		with_isolated_env(|home, _state| {
			with_pinned_data_dir(|dir| {
				let disabled: std::collections::BTreeSet<String> =
					aghub_core::models::AgentType::ALL
						.iter()
						.filter(|a| {
							a.as_str() != "cursor" && a.as_str() != "opencode"
						})
						.map(|a| a.as_str().to_string())
						.collect();
				aghub_core::agent_settings::write_disabled_agents_in(
					dir, &disabled,
				)
				.unwrap();

				let proj = home;
				let slot = proj.join(".agents/skills/shared");
				std::fs::create_dir_all(&slot).unwrap();
				std::fs::write(
					slot.join("SKILL.md"),
					"---\nname: shared\ndescription: d\n---\n",
				)
				.unwrap();

				let req = DeleteSkillParams {
					scope: Some("project".to_string()),
					project_root: Some(proj.display().to_string()),
					confirm: None,
					all_agents: None,
					agents: Some("cursor".to_string()),
				};
				let resp = block_on(delete_skill(
					TrustedLocalOrigin,
					AgentParam(AgentType::Cursor),
					"shared",
					req,
				))
				.ok()
				.expect("handler returned ok")
				.into_inner();

				assert!(
					slot.join("SKILL.md").exists(),
					"the real shared dir must survive when an enabled reader is not in request"
				);
				assert_eq!(
					resp.outcome,
					crate::dto::skill::RemovalOutcomeKind::Kept,
				);
			});
		});
	}

	#[cfg(unix)]
	#[test]
	fn delete_by_path_never_removes_a_different_same_name_master() {
		with_isolated_env(|home, _state| {
			let name = "same-frontmatter-name";
			let shared_dir = home.join(".agents/skills");
			std::fs::create_dir_all(&shared_dir).unwrap();
			let mut entries = Vec::new();
			// Both entries are folders named after the skill (a by-path target
			// must be), one grouped under a category so they can coexist.
			for (folder, master_name) in [
				(name.to_string(), "first-master"),
				(format!("team/{name}"), "second-master"),
			] {
				let master = home.join(".aghub").join(master_name);
				std::fs::create_dir_all(&master).unwrap();
				std::fs::write(
					master.join("SKILL.md"),
					format!("---\nname: {name}\ndescription: d\n---\n"),
				)
				.unwrap();
				let entry = shared_dir.join(folder);
				std::fs::create_dir_all(entry.parent().unwrap()).unwrap();
				std::os::unix::fs::symlink(&master, &entry).unwrap();
				entries.push((entry, master));
			}
			let mut manager = aghub_core::manager::ConfigManager::new(
				aghub_core::create_adapter(AgentType::Cline),
				true,
				None,
			);
			manager.load().unwrap();
			let selected = manager
				.get_skill(name)
				.unwrap()
				.source_path
				.as_ref()
				.unwrap();
			let target = if selected.ends_with(&format!("team/{name}/SKILL.md"))
			{
				&entries[0].0
			} else {
				assert!(
					selected.ends_with(&format!("skills/{name}/SKILL.md")),
					"{selected}"
				);
				&entries[1].0
			};
			let readers =
				aghub_core::skills::removal::skill_dir_readers_outside(
					&shared_dir,
					ResourceScope::GlobalOnly,
					None,
					&[],
				);
			assert!(readers.contains(&"cline"));
			let refusal = by_path_refused(DeleteSkillByPathRequest {
				source_path: target.join("SKILL.md").display().to_string(),
				agents: readers.iter().map(|id| id.to_string()).collect(),
				scope: "global".to_string(),
				project_root: None,
				all_agents: None,
				confirm: Some(true),
			});
			assert!(
				refusal.body.error.contains("requested skill location"),
				"request must fail at exact-location identity check: {}",
				refusal.body.error
			);
			for (entry, master) in entries {
				assert!(
					entry.symlink_metadata().is_ok(),
					"Referrer was deleted"
				);
				assert!(master.join("SKILL.md").exists(), "Master was deleted");
			}
		});
	}

	#[cfg(unix)]
	#[test]
	fn delete_by_path_full_group_keeps_shared_slot_with_legacy_named_referrer()
	{
		with_isolated_env(|home, _state| {
			let proj = home;
			let slot = proj.join(".agents/skills/realname");
			std::fs::create_dir_all(&slot).unwrap();
			std::fs::write(
				slot.join("SKILL.md"),
				"---\nname: realname\ndescription: d\n---\n",
			)
			.unwrap();
			let claude_referrer = proj.join(".claude/skills/dirname");
			std::fs::create_dir_all(claude_referrer.parent().unwrap()).unwrap();
			std::os::unix::fs::symlink(&slot, &claude_referrer).unwrap();

			let readers =
				aghub_core::skills::removal::skill_dir_readers_outside(
					&slot,
					aghub_core::models::ResourceScope::ProjectOnly,
					Some(proj),
					&[],
				);
			let req = DeleteSkillByPathRequest {
				source_path: slot.join("SKILL.md").display().to_string(),
				agents: readers.iter().map(|id| id.to_string()).collect(),
				scope: "project".to_string(),
				project_root: Some(proj.display().to_string()),
				all_agents: None,
				confirm: Some(true),
			};
			let refusal = by_path_refused(req);

			assert!(
				slot.join("SKILL.md").exists(),
				"Claude's differently-named Referrer must keep the slot alive"
			);
			assert!(
				std::fs::canonicalize(&claude_referrer).is_ok(),
				"the Referrer must not be left dangling"
			);
			assert_eq!(
				refusal.status,
				rocket::http::Status::UnprocessableEntity,
				"a real reader outside the request makes this a kept location"
			);
			assert_eq!(refusal.body.code, "UNSUPPORTED_OPERATION");
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
			let master = proj.join(".aghub/linked");
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
			// A SECOND grant, in the shared `.agents/skills` slot the other ten
			// project agents read. Without it nothing but Claude refers to the
			// store Master and dropping Claude's grant rightly takes the Master
			// with it — the survival assertion below would then pass for a
			// reason that has nothing to do with sharing.
			let shared = proj.join(".agents/skills");
			std::fs::create_dir_all(&shared).unwrap();
			std::os::unix::fs::symlink(&master, shared.join("linked")).unwrap();

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
				"a Master the shared slot still refers to must survive"
			);
		});
	}

	/// Every agent that reads `slot`, as request ids. A by-path request naming
	/// exactly the slot's readers passes per-agent validation and the shared-slot
	/// guard, so only the path checks under test stand between it and deletion.
	#[cfg(unix)]
	fn slot_readers(
		slot: &std::path::Path,
		scope: aghub_core::models::ResourceScope,
		project_root: Option<&std::path::Path>,
	) -> Vec<String> {
		let readers = aghub_core::skills::removal::skill_dir_readers_outside(
			slot,
			scope,
			project_root,
			&[],
		)
		.into_iter()
		.map(|id| id.to_string())
		.collect::<Vec<_>>();
		assert!(!readers.is_empty(), "slot must have readers");
		readers
	}

	#[cfg(unix)]
	#[test]
	fn delete_by_path_rejects_trailing_dotdot_project_agents_slot() {
		with_isolated_env(|home, _state| {
			let proj = home.join("proj");
			let slot = proj.join(".agents/skills");
			let y = slot.join("y");
			let z = slot.join("z");
			std::fs::create_dir_all(&y).unwrap();
			std::fs::write(
				y.join("SKILL.md"),
				"---\nname: y\ndescription: y\n---\n",
			)
			.unwrap();
			std::fs::create_dir_all(&z).unwrap();
			std::fs::write(
				z.join("SKILL.md"),
				"---\nname: z\ndescription: z\n---\n",
			)
			.unwrap();

			let req = DeleteSkillByPathRequest {
				source_path: format!("{}/..", y.display()),
				agents: slot_readers(
					&slot,
					aghub_core::models::ResourceScope::ProjectOnly,
					Some(&proj),
				),
				scope: "project".to_string(),
				project_root: Some(proj.display().to_string()),
				all_agents: None,
				confirm: Some(true),
			};
			let refusal = by_path_refused(req);

			assert!(slot.exists(), "slot dir must survive");
			assert!(y.join("SKILL.md").exists(), "skill y must survive");
			assert!(z.join("SKILL.md").exists(), "skill z must survive");
			assert_eq!(refusal.status, rocket::http::Status::BadRequest);
			assert_eq!(refusal.body.code, "INVALID_CONFIG");
			let err = refusal.body.error.as_str();
			assert!(err.contains("'..'"), "error must mention '..': {err}");
		});
	}

	#[cfg(unix)]
	#[test]
	fn delete_by_path_rejects_trailing_dotdot_project_cursor_slot() {
		with_isolated_env(|home, _state| {
			let proj = home.join("proj");
			let slot = proj.join(".cursor/skills");
			let y = slot.join("y");
			let z = slot.join("z");
			std::fs::create_dir_all(&y).unwrap();
			std::fs::write(
				y.join("SKILL.md"),
				"---\nname: y\ndescription: y\n---\n",
			)
			.unwrap();
			std::fs::create_dir_all(&z).unwrap();
			std::fs::write(
				z.join("SKILL.md"),
				"---\nname: z\ndescription: z\n---\n",
			)
			.unwrap();

			let req = DeleteSkillByPathRequest {
				source_path: format!("{}/..", y.display()),
				agents: slot_readers(
					&slot,
					aghub_core::models::ResourceScope::ProjectOnly,
					Some(&proj),
				),
				scope: "project".to_string(),
				project_root: Some(proj.display().to_string()),
				all_agents: None,
				confirm: Some(true),
			};
			let refusal = by_path_refused(req);

			assert!(slot.exists(), "cursor slot dir must survive");
			assert!(y.join("SKILL.md").exists(), "skill y must survive");
			assert!(z.join("SKILL.md").exists(), "skill z must survive");
			assert_eq!(refusal.status, rocket::http::Status::BadRequest);
			assert_eq!(refusal.body.code, "INVALID_CONFIG");
			let err = refusal.body.error.as_str();
			assert!(err.contains("'..'"), "error must mention '..': {err}");
		});
	}

	#[cfg(unix)]
	#[test]
	fn delete_by_path_rejects_trailing_dotdot_global_agents_slot() {
		with_isolated_env(|home, _state| {
			let slot = home.join(".agents/skills");
			let y = slot.join("y");
			let z = slot.join("z");
			std::fs::create_dir_all(&y).unwrap();
			std::fs::write(
				y.join("SKILL.md"),
				"---\nname: y\ndescription: y\n---\n",
			)
			.unwrap();
			std::fs::create_dir_all(&z).unwrap();
			std::fs::write(
				z.join("SKILL.md"),
				"---\nname: z\ndescription: z\n---\n",
			)
			.unwrap();

			let req = DeleteSkillByPathRequest {
				source_path: format!("{}/..", y.display()),
				agents: slot_readers(
					&slot,
					aghub_core::models::ResourceScope::GlobalOnly,
					None,
				),
				scope: "global".to_string(),
				project_root: None,
				all_agents: None,
				confirm: Some(true),
			};
			let refusal = by_path_refused(req);

			assert!(slot.exists(), "global slot dir must survive");
			assert!(y.join("SKILL.md").exists(), "skill y must survive");
			assert!(z.join("SKILL.md").exists(), "skill z must survive");
			assert_eq!(refusal.status, rocket::http::Status::BadRequest);
			assert_eq!(refusal.body.code, "INVALID_CONFIG");
			let err = refusal.body.error.as_str();
			assert!(err.contains("'..'"), "error must mention '..': {err}");
		});
	}

	#[cfg(unix)]
	#[test]
	fn delete_by_path_rejects_dotdot_in_middle_of_path() {
		with_isolated_env(|home, _state| {
			let proj = home.join("proj");
			let slot = proj.join(".agents/skills");
			let y = slot.join("y");
			let z = slot.join("z");
			std::fs::create_dir_all(&y).unwrap();
			std::fs::write(
				y.join("SKILL.md"),
				"---\nname: y\ndescription: y\n---\n",
			)
			.unwrap();
			std::fs::create_dir_all(&z).unwrap();
			std::fs::write(
				z.join("SKILL.md"),
				"---\nname: z\ndescription: z\n---\n",
			)
			.unwrap();

			let req = DeleteSkillByPathRequest {
				source_path: format!("{}/../z", y.display()),
				agents: slot_readers(
					&slot,
					aghub_core::models::ResourceScope::ProjectOnly,
					Some(&proj),
				),
				scope: "project".to_string(),
				project_root: Some(proj.display().to_string()),
				all_agents: None,
				confirm: Some(true),
			};
			let refusal = by_path_refused(req);

			assert!(slot.exists(), "slot dir must survive");
			assert!(y.join("SKILL.md").exists(), "skill y must survive");
			assert!(z.join("SKILL.md").exists(), "skill z must survive");
			assert_eq!(refusal.status, rocket::http::Status::BadRequest);
			assert_eq!(refusal.body.code, "INVALID_CONFIG");
			let err = refusal.body.error.as_str();
			assert!(err.contains("'..'"), "error must mention '..': {err}");
		});
	}

	#[cfg(unix)]
	#[test]
	fn delete_by_path_rejects_dotdot_before_skills_root_with_no_strippable_prefix(
	) {
		with_isolated_env(|home, _state| {
			let proj = home.join("proj");
			let slot = proj.join(".agents/skills");
			let y = slot.join("y");
			let z = slot.join("z");
			std::fs::create_dir_all(&y).unwrap();
			std::fs::write(
				y.join("SKILL.md"),
				"---\nname: y\ndescription: y\n---\n",
			)
			.unwrap();
			std::fs::create_dir_all(&z).unwrap();
			std::fs::write(
				z.join("SKILL.md"),
				"---\nname: z\ndescription: z\n---\n",
			)
			.unwrap();
			std::fs::create_dir_all(proj.join("foo")).unwrap();

			assert!(slot.exists(), "slot dir must exist before");
			assert!(y.join("SKILL.md").exists(), "skill y must exist before");
			assert!(z.join("SKILL.md").exists(), "skill z must exist before");

			let req = DeleteSkillByPathRequest {
				source_path: format!(
					"{}/foo/../.agents/skills/y/..",
					proj.display()
				),
				agents: slot_readers(
					&slot,
					aghub_core::models::ResourceScope::ProjectOnly,
					Some(&proj),
				),
				scope: "project".to_string(),
				project_root: Some(proj.display().to_string()),
				all_agents: None,
				confirm: Some(true),
			};
			let refusal = by_path_refused(req);

			assert!(slot.exists(), "slot dir must survive");
			assert!(y.join("SKILL.md").exists(), "skill y must survive");
			assert!(z.join("SKILL.md").exists(), "skill z must survive");
			assert_eq!(refusal.status, rocket::http::Status::BadRequest);
			assert_eq!(refusal.body.code, "INVALID_CONFIG");
			let err = refusal.body.error.as_str();
			assert!(err.contains("'..'"), "error must mention '..': {err}");
			assert!(
				!err.contains(&proj.display().to_string()),
				"error must not contain filesystem path: {err}"
			);
		});
	}

	#[cfg(unix)]
	#[test]
	fn delete_by_path_rejects_dotdot_before_skills_root_without_trailing_dotdot(
	) {
		with_isolated_env(|home, _state| {
			let proj = home.join("proj");
			let slot = proj.join(".agents/skills");
			let y = slot.join("y");
			let z = slot.join("z");
			std::fs::create_dir_all(&y).unwrap();
			std::fs::write(
				y.join("SKILL.md"),
				"---\nname: y\ndescription: y\n---\n",
			)
			.unwrap();
			std::fs::create_dir_all(&z).unwrap();
			std::fs::write(
				z.join("SKILL.md"),
				"---\nname: z\ndescription: z\n---\n",
			)
			.unwrap();
			std::fs::create_dir_all(proj.join("foo")).unwrap();

			assert!(slot.exists(), "slot dir must exist before");
			assert!(y.join("SKILL.md").exists(), "skill y must exist before");
			assert!(z.join("SKILL.md").exists(), "skill z must exist before");

			let req = DeleteSkillByPathRequest {
				source_path: format!(
					"{}/foo/../.agents/skills/y",
					proj.display()
				),
				agents: slot_readers(
					&slot,
					aghub_core::models::ResourceScope::ProjectOnly,
					Some(&proj),
				),
				scope: "project".to_string(),
				project_root: Some(proj.display().to_string()),
				all_agents: None,
				confirm: Some(true),
			};
			let refusal = by_path_refused(req);

			assert!(slot.exists(), "slot dir must survive");
			assert!(y.join("SKILL.md").exists(), "skill y must survive");
			assert!(z.join("SKILL.md").exists(), "skill z must survive");
			assert_eq!(refusal.status, rocket::http::Status::BadRequest);
			assert_eq!(refusal.body.code, "INVALID_CONFIG");
			let err = refusal.body.error.as_str();
			assert!(err.contains("'..'"), "error must mention '..': {err}");
			assert!(
				!err.contains(&proj.display().to_string()),
				"error must not contain filesystem path: {err}"
			);
		});
	}

	#[cfg(unix)]
	#[test]
	fn delete_by_path_accepts_dotdot_in_project_root() {
		with_isolated_env(|home, _state| {
			let x = home.join("x");
			let proj = home.join("proj");
			std::fs::create_dir_all(&x).unwrap();
			let slot = proj.join(".agents/skills");
			let y = slot.join("y");
			let z = slot.join("z");
			std::fs::create_dir_all(&y).unwrap();
			std::fs::write(
				y.join("SKILL.md"),
				"---\nname: y\ndescription: y\n---\n",
			)
			.unwrap();
			std::fs::create_dir_all(&z).unwrap();
			std::fs::write(
				z.join("SKILL.md"),
				"---\nname: z\ndescription: z\n---\n",
			)
			.unwrap();

			let project_root = format!("{}/x/../proj", home.display());
			let unnormalized_proj = std::path::PathBuf::from(&project_root);
			let source_path = format!(
				"{}/x/../proj/.agents/skills/y/SKILL.md",
				home.display()
			);

			let req = DeleteSkillByPathRequest {
				source_path,
				agents: slot_readers(
					&slot,
					aghub_core::models::ResourceScope::ProjectOnly,
					Some(&unnormalized_proj),
				),
				scope: "project".to_string(),
				project_root: Some(project_root),
				all_agents: None,
				confirm: Some(true),
			};
			let resp =
				block_on(delete_skill_by_path(TrustedLocalOrigin, Json(req)))
					.ok()
					.expect("handler returned ok")
					.into_inner();

			assert!(resp.success, "delete must succeed: {:?}", resp.error);
			assert!(!y.exists(), "skill y dir must be gone");
			assert!(
				z.join("SKILL.md").exists(),
				"sibling skill z must survive"
			);
			assert!(slot.exists(), "slot dir must survive");
		});
	}

	#[cfg(unix)]
	#[test]
	fn delete_by_path_rejects_cross_slot_dotdot() {
		with_isolated_env(|home, _state| {
			let proj = home.join("proj");
			let cursor_slot = proj.join(".cursor/skills");
			let agents_slot = proj.join(".agents/skills");
			std::fs::create_dir_all(&cursor_slot).unwrap();
			let y = agents_slot.join("y");
			let z = agents_slot.join("z");
			std::fs::create_dir_all(&y).unwrap();
			std::fs::write(
				y.join("SKILL.md"),
				"---\nname: y\ndescription: y\n---\n",
			)
			.unwrap();
			std::fs::create_dir_all(&z).unwrap();
			std::fs::write(
				z.join("SKILL.md"),
				"---\nname: z\ndescription: z\n---\n",
			)
			.unwrap();

			let mut agents = slot_readers(
				&cursor_slot,
				aghub_core::models::ResourceScope::ProjectOnly,
				Some(&proj),
			);
			agents.extend(slot_readers(
				&agents_slot,
				aghub_core::models::ResourceScope::ProjectOnly,
				Some(&proj),
			));
			agents.sort();
			agents.dedup();

			let req = DeleteSkillByPathRequest {
				source_path: format!(
					"{}/.cursor/skills/../../.agents/skills/y",
					proj.display()
				),
				agents,
				scope: "project".to_string(),
				project_root: Some(proj.display().to_string()),
				all_agents: None,
				confirm: Some(true),
			};
			let refusal = by_path_refused(req);

			assert_eq!(refusal.status, rocket::http::Status::BadRequest);
			assert_eq!(refusal.body.code, "INVALID_CONFIG");
			let err = refusal.body.error.as_str();
			assert!(err.contains("'..'"), "error must mention '..': {err}");
			assert!(cursor_slot.exists(), "cursor slot dir must survive");
			assert!(agents_slot.exists(), "agents slot dir must survive");
			assert!(y.join("SKILL.md").exists(), "skill y must survive");
			assert!(z.join("SKILL.md").exists(), "skill z must survive");
		});
	}

	#[cfg(unix)]
	#[test]
	fn delete_by_path_rejects_skills_root_itself() {
		with_isolated_env(|home, _state| {
			let proj = home.join("proj");
			let slot = proj.join(".agents/skills");
			let sibling = slot.join("sibling");
			std::fs::create_dir_all(&sibling).unwrap();
			std::fs::write(
				sibling.join("SKILL.md"),
				"---\nname: sibling\ndescription: s\n---\n",
			)
			.unwrap();

			let readers: Vec<String> =
				aghub_core::skills::removal::skill_dir_readers_outside(
					&slot,
					aghub_core::models::ResourceScope::ProjectOnly,
					Some(&proj),
					&[],
				)
				.into_iter()
				.map(|id| id.to_string())
				.collect();
			assert!(!readers.is_empty(), "readers must be non-empty");

			// Case 1: source_path = <proj>/.agents/skills
			let req = DeleteSkillByPathRequest {
				source_path: slot.display().to_string(),
				agents: readers.clone(),
				scope: "project".to_string(),
				project_root: Some(proj.display().to_string()),
				all_agents: None,
				confirm: Some(true),
			};
			let refusal = by_path_refused(req);

			assert!(slot.exists(), "slot dir must survive");
			assert!(
				sibling.join("SKILL.md").exists(),
				"sibling skill must survive"
			);
			assert_eq!(refusal.status, rocket::http::Status::BadRequest);
			assert_eq!(refusal.body.code, "INVALID_CONFIG");
			let err = refusal.body.error.as_str();
			assert!(
				err.contains("strictly"),
				"error must mention strictly: {err}"
			);

			// Case 2: source_path = <proj>/.agents/skills/SKILL.md (SKILL.md need not exist)
			let req = DeleteSkillByPathRequest {
				source_path: slot.join("SKILL.md").display().to_string(),
				agents: readers.clone(),
				scope: "project".to_string(),
				project_root: Some(proj.display().to_string()),
				all_agents: None,
				confirm: Some(true),
			};
			let refusal = by_path_refused(req);

			assert!(slot.exists(), "slot dir must survive");
			assert!(
				sibling.join("SKILL.md").exists(),
				"sibling skill must survive"
			);
			assert_eq!(refusal.status, rocket::http::Status::BadRequest);
			assert_eq!(refusal.body.code, "INVALID_CONFIG");
			let err = refusal.body.error.as_str();
			assert!(
				err.contains("strictly"),
				"error must mention strictly: {err}"
			);

			// Case 3: global <home>/.agents/skills (scope global)
			let global_slot = home.join(".agents/skills");
			let global_sibling = global_slot.join("sibling");
			std::fs::create_dir_all(&global_sibling).unwrap();
			std::fs::write(
				global_sibling.join("SKILL.md"),
				"---\nname: sibling\ndescription: s\n---\n",
			)
			.unwrap();

			let global_readers: Vec<String> =
				aghub_core::skills::removal::skill_dir_readers_outside(
					&global_slot,
					aghub_core::models::ResourceScope::GlobalOnly,
					None,
					&[],
				)
				.into_iter()
				.map(|id| id.to_string())
				.collect();
			assert!(
				!global_readers.is_empty(),
				"global readers must be non-empty"
			);

			let req = DeleteSkillByPathRequest {
				source_path: global_slot.display().to_string(),
				agents: global_readers,
				scope: "global".to_string(),
				project_root: None,
				all_agents: None,
				confirm: Some(true),
			};
			let refusal = by_path_refused(req);

			assert!(global_slot.exists(), "global slot dir must survive");
			assert!(
				global_sibling.join("SKILL.md").exists(),
				"global sibling skill must survive"
			);
			assert_eq!(refusal.status, rocket::http::Status::BadRequest);
			assert_eq!(refusal.body.code, "INVALID_CONFIG");
			let err = refusal.body.error.as_str();
			assert!(
				err.contains("strictly"),
				"error must mention strictly: {err}"
			);
		});
	}

	/// A `SKILL.md` placed directly in the skills root (not in a subdirectory)
	/// must not delete the root or the sibling — the dir parent is the skills
	/// root itself, so the strictly-contained guard rejects it.
	#[cfg(unix)]
	#[test]
	fn delete_by_path_rejects_skill_md_directly_under_the_skills_root() {
		with_isolated_env(|home, _state| {
			let proj = home.join("proj");
			let slot = proj.join(".agents/skills");
			let sibling = slot.join("sibling");
			std::fs::create_dir_all(&sibling).unwrap();
			std::fs::write(
				sibling.join("SKILL.md"),
				"---\nname: sibling\ndescription: s\n---\n",
			)
			.unwrap();
			// A bare SKILL.md in the root — not inside a skill subdirectory.
			std::fs::write(
				slot.join("SKILL.md"),
				"---\nname: root-skill\ndescription: r\n---\n",
			)
			.unwrap();

			let readers = slot_readers(
				&slot,
				aghub_core::models::ResourceScope::ProjectOnly,
				Some(&proj),
			);
			let req = DeleteSkillByPathRequest {
				source_path: slot.join("SKILL.md").display().to_string(),
				agents: readers,
				scope: "project".to_string(),
				project_root: Some(proj.display().to_string()),
				all_agents: None,
				confirm: Some(true),
			};
			let refusal = by_path_refused(req);

			assert!(slot.exists(), "slot dir must survive");
			assert!(
				sibling.join("SKILL.md").exists(),
				"sibling skill must survive"
			);
			assert_eq!(refusal.status, rocket::http::Status::BadRequest);
			assert_eq!(refusal.body.code, "INVALID_CONFIG");
		});
	}

	/// `<proj>/.agents/skills/y/..` with sibling skills y and z: both must
	/// survive because the path contains `..`.
	#[cfg(unix)]
	#[test]
	fn delete_by_path_rejects_child_then_dotdot() {
		with_isolated_env(|home, _state| {
			let proj = home.join("proj");
			let slot = proj.join(".agents/skills");
			let y = slot.join("y");
			let z = slot.join("z");
			std::fs::create_dir_all(&y).unwrap();
			std::fs::write(
				y.join("SKILL.md"),
				"---\nname: y\ndescription: y\n---\n",
			)
			.unwrap();
			std::fs::create_dir_all(&z).unwrap();
			std::fs::write(
				z.join("SKILL.md"),
				"---\nname: z\ndescription: z\n---\n",
			)
			.unwrap();

			let readers = slot_readers(
				&slot,
				aghub_core::models::ResourceScope::ProjectOnly,
				Some(&proj),
			);
			let req = DeleteSkillByPathRequest {
				source_path: format!("{}/{}", y.display(), ".."),
				agents: readers,
				scope: "project".to_string(),
				project_root: Some(proj.display().to_string()),
				all_agents: None,
				confirm: Some(true),
			};
			let refusal = by_path_refused(req);

			assert!(y.join("SKILL.md").exists(), "skill y must survive");
			assert!(z.join("SKILL.md").exists(), "skill z must survive");
			assert_eq!(refusal.status, rocket::http::Status::BadRequest);
			assert_eq!(refusal.body.code, "INVALID_CONFIG");
			let err = refusal.body.error.as_str();
			assert!(err.contains("'..'"), "error must mention '..': {err}");
		});
	}

	#[cfg(unix)]
	fn claude_by_path(
		source_path: &std::path::Path,
		confirm: bool,
	) -> DeleteSkillByPathRequest {
		DeleteSkillByPathRequest {
			source_path: source_path.display().to_string(),
			agents: vec!["claude".to_string()],
			scope: "global".to_string(),
			project_root: None,
			all_agents: None,
			confirm: Some(confirm),
		}
	}

	/// A category folder (`<slot>/team`, skills only underneath) is not a skill:
	/// naming it must refuse and leave every skill beneath it alone, dry-run or
	/// confirmed. See docs/history/api.md#delete-by-path-target-must-be-a-skill-root
	#[cfg(unix)]
	#[test]
	fn delete_by_path_refuses_category_folder_and_keeps_every_skill_under_it() {
		with_isolated_env(|home, _state| {
			let team = home.join(".claude/skills/team");
			for name in ["a", "b"] {
				let dir = team.join(name);
				std::fs::create_dir_all(&dir).unwrap();
				std::fs::write(
					dir.join("SKILL.md"),
					format!("---\nname: {name}\ndescription: d\n---\n"),
				)
				.unwrap();
			}

			for confirm in [false, true] {
				let refusal = by_path_refused(claude_by_path(&team, confirm));
				assert_eq!(refusal.status, rocket::http::Status::BadRequest);
				assert_eq!(refusal.body.code, "INVALID_CONFIG");
				let err = refusal.body.error.as_str();
				assert!(
					err.contains("SKILL.md"),
					"the refusal must say what a target needs: {err}"
				);
				assert!(
					!err.contains(&home.display().to_string()),
					"the refusal must not echo filesystem paths: {err}"
				);
				assert!(team.join("a/SKILL.md").exists());
				assert!(team.join("b/SKILL.md").exists());
			}
		});
	}

	/// A directory INSIDE a skill, or a stray file inside it, must not resolve to
	/// the enclosing skill either: `<slot>/z/scripts`, `<slot>/z/scripts/run.sh`
	/// and `<slot>/z/gone.txt` (whose `parent()` is the skill root) all refuse.
	#[cfg(unix)]
	#[test]
	fn delete_by_path_refuses_skill_subdirectory_and_stray_file() {
		with_isolated_env(|home, _state| {
			let z = home.join(".claude/skills/z");
			std::fs::create_dir_all(z.join("scripts")).unwrap();
			std::fs::write(
				z.join("SKILL.md"),
				"---\nname: z\ndescription: d\n---\n",
			)
			.unwrap();
			std::fs::write(z.join("scripts/run.sh"), "#!/bin/sh\n").unwrap();

			for target in [
				z.join("scripts"),
				z.join("scripts/run.sh"),
				z.join("gone.txt"),
			] {
				let refusal = by_path_refused(claude_by_path(&target, true));
				assert_eq!(refusal.status, rocket::http::Status::BadRequest);
				assert_eq!(refusal.body.code, "INVALID_CONFIG");
				assert!(z.join("SKILL.md").exists(), "the skill must survive");
				assert!(z.join("scripts/run.sh").exists());
			}
		});
	}

	/// The by-path preview reports the SAME `needs_confirm` core's by-name plan
	/// does: releasing a real directory from a shared Referrer root needs it, a
	/// private copy does not. The route used to hand-build `false` for both.
	#[cfg(unix)]
	#[test]
	fn delete_by_path_needs_confirm_matches_core_plan_for_shared_slot() {
		with_isolated_env(|home, _state| {
			with_pinned_data_dir(|dir| {
				let disabled: std::collections::BTreeSet<String> =
					aghub_core::models::AgentType::ALL
						.iter()
						.filter(|a| a.as_str() != "cursor")
						.map(|a| a.as_str().to_string())
						.collect();
				aghub_core::agent_settings::write_disabled_agents_in(
					dir, &disabled,
				)
				.unwrap();

				let proj = home;
				let shared = proj.join(".agents/skills/shared");
				std::fs::create_dir_all(&shared).unwrap();
				std::fs::write(
					shared.join("SKILL.md"),
					"---\nname: shared\ndescription: d\n---\n",
				)
				.unwrap();
				let private = proj.join(".cursor/skills/private");
				std::fs::create_dir_all(&private).unwrap();
				std::fs::write(
					private.join("SKILL.md"),
					"---\nname: private\ndescription: d\n---\n",
				)
				.unwrap();

				let ask = |dir: &std::path::Path| {
					let req = DeleteSkillByPathRequest {
						source_path: dir.join("SKILL.md").display().to_string(),
						agents: vec!["cursor".to_string()],
						scope: "project".to_string(),
						project_root: Some(proj.display().to_string()),
						all_agents: None,
						confirm: None,
					};
					block_on(delete_skill_by_path(
						TrustedLocalOrigin,
						Json(req),
					))
					.ok()
					.expect("handler returned ok")
					.into_inner()
				};
				let shared_resp = ask(&shared);
				assert!(shared_resp.success, "{:?}", shared_resp.error);
				assert!(
					shared_resp.needs_confirm,
					"a shared-slot release needs confirm, as in the by-name plan"
				);
				let private_resp = ask(&private);
				assert!(private_resp.success, "{:?}", private_resp.error);
				assert!(!private_resp.needs_confirm);

				let symlink_master = proj.join(".aghub/symlink-skill");
				std::fs::create_dir_all(&symlink_master).unwrap();
				std::fs::write(
					symlink_master.join("SKILL.md"),
					"---\nname: symlink-skill\ndescription: d\n---\n",
				)
				.unwrap();
				let symlink_slot = proj.join(".cursor/skills/symlink-skill");
				std::fs::create_dir_all(symlink_slot.parent().unwrap())
					.unwrap();
				std::os::unix::fs::symlink(&symlink_master, &symlink_slot)
					.unwrap();

				let ask_by_name = |name: &str| {
					let params = DeleteSkillParams {
						scope: Some("project".to_string()),
						project_root: Some(proj.display().to_string()),
						confirm: None,
						all_agents: None,
						agents: Some("cursor".to_string()),
					};
					block_on(delete_skill(
						TrustedLocalOrigin,
						AgentParam(aghub_core::models::AgentType::Cursor),
						name,
						params,
					))
					.ok()
					.expect("by-name preview returned ok")
					.into_inner()
				};

				let shared_by_name = ask_by_name("shared");
				assert_eq!(
					shared_by_name.needs_confirm, shared_resp.needs_confirm,
					"shared-slot real dir reports the same needs_confirm by-name and by-path"
				);
				assert!(
					shared_by_name.needs_confirm,
					"shared-slot by-name preview must report needs_confirm: true"
				);

				let symlink_by_name = ask_by_name("symlink-skill");
				assert!(
					symlink_by_name.needs_confirm,
					"single-agent symlink-layout by-name preview reports needs_confirm: true"
				);

				assert!(
					shared.exists()
						&& private.exists()
						&& symlink_slot.exists(),
					"dry-run only"
				);
			});
		});
	}

	/// A symlink loop as the target must refuse without echoing a filesystem
	/// path, and must not read as "already gone".
	#[cfg(unix)]
	#[test]
	fn delete_by_path_symlink_loop_refuses_without_leaking_paths() {
		with_isolated_env(|home, _state| {
			let skills = home.join(".claude/skills");
			std::fs::create_dir_all(&skills).unwrap();
			let looped = skills.join("loop");
			std::os::unix::fs::symlink("loop", &looped).unwrap();

			for target in [looped.clone(), looped.join("SKILL.md")] {
				let refusal = by_path_refused(claude_by_path(&target, true));
				assert_eq!(refusal.status, rocket::http::Status::BadRequest);
				assert_eq!(refusal.body.code, "INVALID_CONFIG");
				let json = serde_json::to_string(&refusal.body).unwrap();
				eprintln!("ELOOP {} => {json}", target.display());
				assert!(
					!json.contains(&home.display().to_string()),
					"no internal path may reach the response: {json}"
				);
				assert!(
					std::fs::symlink_metadata(&looped).is_ok(),
					"the link itself must be untouched"
				);
			}
		});
	}

	/// A REAL directory inside `.aghub/<name>` (the Master store) must survive a
	/// by-path request from a single enabled agent.
	#[cfg(unix)]
	#[test]
	fn delete_by_path_rejects_aghub_store_path_before_the_keep_rule() {
		with_isolated_env(|home, _state| {
			with_pinned_data_dir(|dir| {
				let disabled: std::collections::BTreeSet<String> =
					aghub_core::models::AgentType::ALL
						.iter()
						.filter(|a| a.as_str() != "cursor")
						.map(|a| a.as_str().to_string())
						.collect();
				aghub_core::agent_settings::write_disabled_agents_in(
					dir, &disabled,
				)
				.unwrap();

				let proj = home;
				let name = "store-real";
				let master = proj.join(".aghub").join(name);
				std::fs::create_dir_all(&master).unwrap();
				std::fs::write(
					master.join("SKILL.md"),
					format!("---\nname: {name}\ndescription: test\n---\n"),
				)
				.unwrap();
				// Agent marker so project root is recognized.
				std::fs::create_dir_all(proj.join(".cursor")).unwrap();

				let req = DeleteSkillByPathRequest {
					source_path: master.join("SKILL.md").display().to_string(),
					agents: vec!["cursor".to_string()],
					scope: "project".to_string(),
					project_root: Some(proj.display().to_string()),
					all_agents: None,
					confirm: Some(true),
				};
				let refusal = by_path_refused(req);

				assert!(
					master.join("SKILL.md").exists(),
					".aghub store real dir must survive"
				);
				// No agent reads `.aghub`, so the route's location validation
				// refuses before the keep rule is reached; the keep rule itself
				// is pinned in core (`single_agent_keep_reason_aghub_store_*`).
				assert_eq!(refusal.status, rocket::http::Status::BadRequest);
				assert_eq!(refusal.body.code, "INVALID_CONFIG");
			});
		});
	}

	/// A source_path spelling `..` into `.aghub` (e.g.
	/// `<proj>/.agents/skills/../../.aghub/<name>/SKILL.md`) must be
	/// rejected because the `..` components escape the agent skills root.
	#[cfg(unix)]
	#[test]
	fn delete_by_path_rejects_dotdot_into_aghub_store() {
		with_isolated_env(|home, _state| {
			with_pinned_data_dir(|dir| {
				let disabled: std::collections::BTreeSet<String> =
					aghub_core::models::AgentType::ALL
						.iter()
						.filter(|a| a.as_str() != "cursor")
						.map(|a| a.as_str().to_string())
						.collect();
				aghub_core::agent_settings::write_disabled_agents_in(
					dir, &disabled,
				)
				.unwrap();

				let proj = home;
				let name = "store-dot";
				let master = proj.join(".aghub").join(name);
				std::fs::create_dir_all(&master).unwrap();
				std::fs::write(
					master.join("SKILL.md"),
					format!("---\nname: {name}\ndescription: test\n---\n"),
				)
				.unwrap();
				std::fs::create_dir_all(proj.join(".cursor")).unwrap();

				// Spell the path through .. into .aghub.
				let dotdot_path = format!(
					"{}/.agents/skills/../../.aghub/{}/SKILL.md",
					proj.display(),
					name
				);
				let req = DeleteSkillByPathRequest {
					source_path: dotdot_path,
					agents: vec!["cursor".to_string()],
					scope: "project".to_string(),
					project_root: Some(proj.display().to_string()),
					all_agents: None,
					confirm: Some(true),
				};
				let refusal = by_path_refused(req);

				assert!(
					master.join("SKILL.md").exists(),
					".aghub store dir must survive the dotdot attempt"
				);
				// The handler rejects the `..` before reaching the store
				// guard, so this is a validation failure, not a kept outcome.
				assert_eq!(refusal.status, rocket::http::Status::BadRequest);
				assert_eq!(refusal.body.code, "INVALID_CONFIG");
				let err = refusal.body.error.as_str();
				assert!(err.contains("'..'"), "error must mention '..': {err}");
			});
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

			let resp = block_on(prune_lock_route(
				TrustedLocalOrigin,
				Json(prune_req("global", None, None)),
			))
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

			let resp = block_on(prune_lock_route(
				TrustedLocalOrigin,
				Json(prune_req("global", None, Some(true))),
			))
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
			let resp = block_on(prune_lock_route(
				TrustedLocalOrigin,
				Json(prune_req("project", None, Some(true))),
			))
			.ok()
			.expect("handler returned ok")
			.into_inner();
			assert!(resp.error.is_some(), "project prune needs a project root");
			assert!(resp.pruned.is_empty());
		});
	}

	#[cfg(unix)]
	#[test]
	fn import_skill_smoke() {
		with_isolated_env(|home, _state| {
			let source_skill = home.join("source-skills/my-skill");
			std::fs::create_dir_all(&source_skill).unwrap();
			std::fs::write(
				source_skill.join("SKILL.md"),
				"---\nname: my-skill\ndescription: test\n---\n\nbody\n",
			)
			.unwrap();

			let project = home.join("myproject");
			std::fs::create_dir_all(project.join(".claude/skills")).unwrap();

			let resp = match block_on(import_skill(
				TrustedLocalOrigin,
				AgentParam(AgentType::Claude),
				ScopeParams {
					scope: Some("project".to_string()),
					project_root: Some(project.display().to_string()),
				},
				Json(crate::dto::skill::ImportSkillRequest {
					path: source_skill.join("SKILL.md").display().to_string(),
					name: None,
				}),
			)) {
				Ok(val) => val.into_inner(),
				Err(err) => panic!(
					"import_skill must succeed, got Err(status={:?}, code={:?}, error={:?})",
					err.status, err.body.code, err.body.error
				),
			};

			assert_eq!(resp.name, "my-skill");
			assert!(project.join(".aghub/my-skill/SKILL.md").exists());
			assert!(project.join(".claude/skills/my-skill").exists());
			let lock = skill::lock::local::read_local_lock(Some(&project));
			assert!(lock.skills.contains_key("my-skill"));
		});
	}

	#[cfg(unix)]
	#[test]
	fn import_skill_refuses_a_malformed_agent_config_like_the_cli() {
		with_isolated_env(|home, _state| {
			let source_skill = home.join("source-skills/my-skill");
			std::fs::create_dir_all(&source_skill).unwrap();
			std::fs::write(
				source_skill.join("SKILL.md"),
				"---\nname: my-skill\ndescription: test\n---\n\nbody\n",
			)
			.unwrap();

			let project = home.join("myproject");
			std::fs::create_dir_all(project.join(".claude/skills")).unwrap();
			let mcp =
				r#"{ "mcpServers": { "keepme": { "command": "echo" } } } OOPS"#;
			std::fs::write(project.join(".mcp.json"), mcp).unwrap();

			let err = match block_on(import_skill(
				TrustedLocalOrigin,
				AgentParam(AgentType::Claude),
				ScopeParams {
					scope: Some("project".to_string()),
					project_root: Some(project.display().to_string()),
				},
				Json(crate::dto::skill::ImportSkillRequest {
					path: source_skill.join("SKILL.md").display().to_string(),
					name: None,
				}),
			)) {
				Ok(_) => {
					panic!("a malformed agent config must refuse the import")
				}
				Err(err) => err,
			};

			assert_eq!(err.body.code, "INVALID_CONFIG");
			assert!(!err.status.class().is_success(), "status must not be 2xx");
			assert!(!project.join(".aghub/my-skill").exists());
			assert!(
				std::fs::symlink_metadata(
					project.join(".claude/skills/my-skill")
				)
				.is_err(),
				"no referrer, not even a dangling link"
			);
			let lock = skill::lock::local::read_local_lock(Some(&project));
			assert!(!lock.skills.contains_key("my-skill"));
			assert_eq!(
				std::fs::read_to_string(project.join(".mcp.json")).unwrap(),
				mcp
			);
		});
	}

	#[cfg(unix)]
	#[test]
	fn import_skill_with_name_installs_under_requested_name() {
		with_isolated_env(|home, _state| {
			let source_skill = home.join("source-skills/my-skill");
			std::fs::create_dir_all(&source_skill).unwrap();
			std::fs::write(
				source_skill.join("SKILL.md"),
				"---\nname: my-skill\ndescription: test\n---\n\nbody\n",
			)
			.unwrap();

			let project = home.join("myproject");
			std::fs::create_dir_all(project.join(".claude/skills")).unwrap();

			let resp = match block_on(import_skill(
				TrustedLocalOrigin,
				AgentParam(AgentType::Claude),
				ScopeParams {
					scope: Some("project".to_string()),
					project_root: Some(project.display().to_string()),
				},
				Json(crate::dto::skill::ImportSkillRequest {
					path: source_skill.join("SKILL.md").display().to_string(),
					name: Some("imported-alias".to_string()),
				}),
			)) {
				Ok(val) => val.into_inner(),
				Err(err) => panic!(
					"import_skill must succeed, got Err(status={:?}, code={:?}, error={:?})",
					err.status, err.body.code, err.body.error
				),
			};

			assert_eq!(resp.name, "imported-alias");
			assert!(project.join(".aghub/imported-alias/SKILL.md").exists());
			assert!(project.join(".claude/skills/imported-alias").exists());
			let lock = skill::lock::local::read_local_lock(Some(&project));
			assert!(lock.skills.contains_key("imported-alias"));
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
				!home.join(".aghub/evil").exists(),
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

	/// The session (a repo) and the skill name are SEPARATE request fields, so a
	/// caller can pair one repo's scan with a skill locked to another. Nothing
	/// else in the route catches it: the lock entry is present and unchanged, so
	/// `ensure_unchanged` is satisfied, and the resync would then install the
	/// scanned repo's bytes under this entry's source/path/ref with only the hash
	/// re-stamped. No race involved.
	///
	/// The surviving content is the assertion with teeth — drop the `describes`
	/// check and the swap goes through, so this fails on the file contents (and on
	/// the status, which becomes 200).
	#[test]
	fn git_sync_refuses_a_session_for_a_different_repo() {
		with_isolated_env(|_, _| {
			let temp = tempdir().unwrap();
			let project = temp.path().join("project");
			let installed = project.join(".claude/skills/sync-me");
			std::fs::create_dir_all(&installed).unwrap();
			std::fs::write(
				installed.join("SKILL.md"),
				"---\nname: sync-me\ndescription: mine\n---\n\nmine\n",
			)
			.unwrap();
			// Locked to `owner/repo`.
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
			let lock_before =
				skill::lock::local::read_local_lock(Some(&project));

			// A scan of a DIFFERENT repo that happens to contain the same path.
			let fixture = tempdir().unwrap();
			let elsewhere = fixture.path().join("sync-me");
			std::fs::create_dir_all(&elsewhere).unwrap();
			std::fs::write(
				elsewhere.join("SKILL.md"),
				"---\nname: sync-me\ndescription: theirs\n---\n\ntheirs\n",
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
				"other-repo".to_string(),
				session_from_fixture(
					fixture.path(),
					"https://github.com/someone-else/repo.git",
					"main",
				),
			);

			let response = client
				.post("/api/v1/skills/git/sync")
				.json(&serde_json::json!({
					"session_id": "other-repo",
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

			assert_eq!(response.status(), rocket::http::Status::BadRequest);
			let body: serde_json::Value =
				serde_json::from_str(&response.into_string().unwrap()).unwrap();
			assert_eq!(body["code"], "SKILL_SOURCE_MISMATCH");
			assert!(
				std::fs::read_to_string(installed.join("SKILL.md"))
					.unwrap()
					.contains("mine"),
				"the locked skill's content must survive a mismatched session"
			);
			assert_eq!(
				skill::lock::local::read_local_lock(Some(&project)).skills,
				lock_before.skills,
				"a refused sync must not stamp a hash"
			);
		});
	}

	/// Another process installs the skill while this request is fetching: the
	/// lock entry is written from inside the fetch.
	struct AppearingEntryBackend {
		inner: SessionLocalBackend,
		project: std::path::PathBuf,
	}

	impl aghub_git::RepoFetchBackend for AppearingEntryBackend {
		fn resolve(
			&self,
			source: &aghub_git::SourceRef,
			auth: Option<&aghub_git::Credentials>,
		) -> aghub_git::Result<aghub_git::RepoSnapshot> {
			aghub_git::RepoFetchBackend::resolve(&self.inner, source, auth)
		}
		fn read_tree(
			&self,
			s: &aghub_git::RepoSnapshot,
		) -> aghub_git::Result<aghub_git::RepoTree> {
			aghub_git::RepoFetchBackend::read_tree(&self.inner, s)
		}
		fn read_blobs(
			&self,
			s: &aghub_git::RepoSnapshot,
			o: &[String],
		) -> aghub_git::Result<Vec<aghub_git::Blob>> {
			aghub_git::RepoFetchBackend::read_blobs(&self.inner, s, o)
		}
		fn materialize(
			&self,
			s: &aghub_git::RepoSnapshot,
			paths: &[&str],
			dest: &std::path::Path,
		) -> aghub_git::Result<()> {
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
				Some(&self.project),
			)
			.unwrap();
			aghub_git::RepoFetchBackend::materialize(
				&self.inner,
				s,
				paths,
				dest,
			)
		}
	}

	#[test]
	fn git_sync_refuses_an_entry_that_appeared_during_the_fetch() {
		with_isolated_env(|_, _| {
			let temp = tempdir().unwrap();
			let project = temp.path().join("project");
			let installed = project.join(".claude/skills/sync-me");
			std::fs::create_dir_all(&installed).unwrap();
			std::fs::write(
				installed.join("SKILL.md"),
				"---\nname: sync-me\ndescription: mine\n---\n\nmine\n",
			)
			.unwrap();

			// Same fixture as the different-repo test: a scan of a repo holding the
			// same path. No lock entry exists before the request.
			let fixture = tempdir().unwrap();
			let elsewhere = fixture.path().join("sync-me");
			std::fs::create_dir_all(&elsewhere).unwrap();
			std::fs::write(
				elsewhere.join("SKILL.md"),
				"---\nname: sync-me\ndescription: theirs\n---\n\ntheirs\n",
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
				"appearing-entry".to_string(),
				session_with_backend(
					std::sync::Arc::new(AppearingEntryBackend {
						inner: SessionLocalBackend::new(fixture.path()),
						project: project.clone(),
					}),
					"https://github.com/owner/repo.git",
					"main",
				),
			);

			let response = client
				.post("/api/v1/skills/git/sync")
				.json(&serde_json::json!({
					"session_id": "appearing-entry",
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

			assert_eq!(response.status(), rocket::http::Status::Conflict);
			let body: serde_json::Value =
				serde_json::from_str(&response.into_string().unwrap()).unwrap();
			assert_eq!(body["code"], "SKILL_SOURCE_CHANGED_DURING_FETCH");
			assert!(
				std::fs::read_to_string(installed.join("SKILL.md"))
					.unwrap()
					.contains("mine"),
				"the installed skill must survive an appeared entry"
			);
			assert_eq!(
				skill::lock::local::read_local_lock(Some(&project)).skills
					["sync-me"]
					.computed_hash,
				"old",
				"a refused sync must not stamp a hash"
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
	fn git_sync_unknown_scope_rejected_before_fetch_identity_capture() {
		with_isolated_env(|_, _| {
			with_pinned_data_dir(|_| {
				let fixture = tempdir().unwrap();
				let cloned_skill = fixture.path().join("sync-me");
				std::fs::create_dir_all(&cloned_skill).unwrap();
				std::fs::write(
					cloned_skill.join("SKILL.md"),
					"---\nname: sync-me\ndescription: new\n---\n\nnew\n",
				)
				.unwrap();

				let app_data = tempdir().unwrap();
				let client = rocket::local::blocking::Client::tracked(
					crate::build_rocket(
						rocket::Config::default(),
						app_data.path().to_path_buf(),
					),
				)
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

				// Note: skill_path does NOT exist in fixture ("missing/SKILL.md").
				// If catch-all were active, the route would proceed to fetch and return
				// 404 SKILL_PATH_NOT_FOUND instead of 400 INVALID_SCOPE.
				let response = client
					.post("/api/v1/skills/git/sync")
					.json(&serde_json::json!({
						"session_id": "sync-session",
						"name": "sync-me",
						"scope": "bogus-scope",
						"project_root": null,
						"skill_path": "missing/SKILL.md",
						"source_paths": [],
					}))
					.dispatch();

				assert_eq!(response.status(), rocket::http::Status::BadRequest);
				let body: serde_json::Value =
					serde_json::from_str(&response.into_string().unwrap())
						.unwrap();
				assert_eq!(body["code"], "INVALID_SCOPE");
			});
		});
	}

	/// A reconcile into OpenCode must REUSE the store Master and hand OpenCode
	/// a Referrer link into it — never a second physical copy of the skill.
	///
	/// This used to assert the opposite (`.opencode/skills/<n>` must NOT
	/// exist), because OpenCode reached the Master by scanning
	/// `.agents/skills` and needed no link of its own. Now that the Master
	/// lives in a store nobody reads, "no entry in OpenCode's dir" means
	/// OpenCode does not have the skill at all — so the regression being
	/// guarded moved from "no entry" to "an entry that is a LINK": a private
	/// duplicate is still the failure, and it now shows up as a real directory
	/// where the link belongs.
	#[test]
	fn reconcile_skill_links_opencode_referrer_to_the_master() {
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
				scope: WriteScope::project(project_root.clone()),
				name: "repo-helper".to_string(),
			},
			vec![AgentType::OpenCode],
			vec![],
			false, // confirm: add-only, nothing to confirm
		)
		.unwrap();

		assert_eq!(result.success_count(), 1);
		let master = project_root.join(".aghub/repo-helper");
		assert!(
			master.join("assets/notes.txt").exists(),
			"the copy must have landed in the store Master",
		);
		let referrer = project_root.join(".opencode/skills/repo-helper");
		// `Linker::is_link`, not `is_symlink`: this test is not unix-gated and
		// the Referrer is a junction on Windows, which `is_symlink` calls false.
		assert!(
			aghub_core::skills::linker::Linker::is_link(&referrer),
			"OpenCode's grant must be a Referrer link, not a private duplicate",
		);
		assert_eq!(
			std::fs::canonicalize(&referrer).unwrap(),
			std::fs::canonicalize(&master).unwrap(),
			"the Referrer must resolve to the one store Master",
		);
		assert!(
			referrer.join("assets/notes.txt").exists(),
			"and the whole skill must be reachable through it",
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
				Err(skill_update::SkillRepoError::Network("boom".to_string()))
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
			// Universal master: <project>/.aghub/foo
			let master = project.path().join(".aghub/foo");
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

	/// git-scan's host-scoped keyring fallback (no `credential_id`) must answer
	/// a retryable 503 on a keyring outage, not proceed as "no credential"
	/// (GitHub #15). See docs/history/api.md#apply-update-keyring-fail-closed
	///
	/// Uses `ForceCredentialBackendUnavailable` (cross-platform). The URL is a
	/// closed local port, so a regression that swallows the error and clones
	/// fails fast instead of hanging on the network.
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
			let master = home.join(".aghub/my-skill/SKILL.md");
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
	fn git_install_keeps_the_scopes_none_cohort_over_the_default_branch() {
		with_isolated_env(|home, _state| {
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
			let fixture = tempdir().unwrap();
			let dst = fixture.path().join("my-skill");
			std::fs::create_dir_all(&dst).unwrap();
			std::fs::write(
				dst.join("SKILL.md"),
				"---\nname: my-skill\ndescription: d\n---\n",
			)
			.unwrap();

			// The project already records `other` with NO ref: its cohort is `None`.
			let project = home.join("proj");
			std::fs::create_dir_all(&project).unwrap();
			let mut lock = skill::LocalSkillLockFile::new();
			lock.skills.insert(
				"other".to_string(),
				skill::LocalSkillLockEntry {
					source: "o/r".to_string(),
					source_url: Some("https://github.com/o/r".to_string()),
					source_type: "github".to_string(),
					ref_name: None,
					skill_path: Some("other/SKILL.md".to_string()),
					computed_hash: "h".to_string(),
					ref_commit: None,
				},
			);
			skill::write_local_lock(&lock, Some(&project)).unwrap();

			// The scan found `develop` as the default branch; nothing was asked for.
			let repo = std::sync::Arc::new(
				skill_update::SkillRepository::with_backends(
					None,
					std::sync::Arc::new(SessionLocalBackend::new(
						fixture.path(),
					)),
				),
			);
			let claim = repo
				.resolve_pinned(
					&skill_update::SourceRef {
						source: "https://github.com/o/r".to_string(),
						ref_: None,
					},
					None,
				)
				.expect("resolve fixture session");
			app_sessions.insert(
				"sess-1".to_string(),
				PinnedSourceSession::new(
					repo,
					claim,
					"https://github.com/o/r".to_string(),
					None,
					vec!["develop".to_string()],
					"develop".to_string(),
				),
			);
			let response = client
				.post("/api/v1/skills/git/install")
				.json(&serde_json::json!({
					"session_id": "sess-1",
					"skill_paths": ["my-skill"],
					"agents": ["claude"],
					"scope": "project",
					"project_root": project.to_str().unwrap()
				}))
				.dispatch();
			assert_eq!(
				response.status(),
				rocket::http::Status::Ok,
				"handler returned ok"
			);
			// A scope whose entries record no ref must not gain `develop`:
			// that would be a mixed cohort the next `source sync` refuses.
			let recorded = skill::read_local_lock(Some(&project));
			assert_eq!(recorded.skills["my-skill"].ref_name, None);
		});
	}

	#[cfg(unix)]
	#[test]
	fn git_install_refuses_a_cohort_ref_the_scan_did_not_fetch() {
		with_isolated_env(|home, _state| {
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
			let fixture = tempdir().unwrap();
			let dst = fixture.path().join("my-skill");
			std::fs::create_dir_all(&dst).unwrap();
			std::fs::write(
				dst.join("SKILL.md"),
				"---\nname: my-skill\ndescription: d\n---\n",
			)
			.unwrap();

			// The project already records `other` pinned to `main`: the cohort is `main`.
			let project = home.join("proj");
			std::fs::create_dir_all(&project).unwrap();
			let mut lock = skill::LocalSkillLockFile::new();
			lock.skills.insert(
				"other".to_string(),
				skill::LocalSkillLockEntry {
					source: "o/r".to_string(),
					source_url: Some("https://github.com/o/r".to_string()),
					source_type: "github".to_string(),
					ref_name: Some("main".to_string()),
					skill_path: Some("other/SKILL.md".to_string()),
					computed_hash: "h".to_string(),
					ref_commit: None,
				},
			);
			skill::write_local_lock(&lock, Some(&project)).unwrap();

			// The scan found `develop` as the default branch; nothing was asked for.
			let repo = std::sync::Arc::new(
				skill_update::SkillRepository::with_backends(
					None,
					std::sync::Arc::new(SessionLocalBackend::new(
						fixture.path(),
					)),
				),
			);
			let claim = repo
				.resolve_pinned(
					&skill_update::SourceRef {
						source: "https://github.com/o/r".to_string(),
						ref_: None,
					},
					None,
				)
				.expect("resolve fixture session");
			app_sessions.insert(
				"sess-cohort".to_string(),
				PinnedSourceSession::new(
					repo,
					claim,
					"https://github.com/o/r".to_string(),
					None,
					vec!["develop".to_string()],
					"develop".to_string(),
				),
			);
			let response = client
				.post("/api/v1/skills/git/install")
				.json(&serde_json::json!({
					"session_id": "sess-cohort",
					"skill_paths": ["my-skill"],
					"agents": ["claude"],
					"scope": "project",
					"project_root": project.to_str().unwrap()
				}))
				.dispatch();
			assert_eq!(
				response.status(),
				rocket::http::Status::BadRequest,
				"a cohort ref the scan did not fetch is refused"
			);
			let body: serde_json::Value =
				serde_json::from_str(&response.into_string().unwrap()).unwrap();
			assert_eq!(body["code"], "SKILL_SOURCE_MISMATCH");
			// Nothing was written: no lock entry, no Master copy.
			let recorded = skill::read_local_lock(Some(&project));
			assert!(!recorded.skills.contains_key("my-skill"));
			assert!(
				!project.join(".aghub/my-skill").exists(),
				"nothing may be materialized"
			);
			// The cohort's own entry is untouched.
			assert_eq!(
				recorded.skills["other"].ref_name.as_deref(),
				Some("main")
			);
		});
	}

	#[cfg(unix)]
	const COHORT_DEVELOP: &str = "dddddddddddddddddddddddddddddddddddddddd";
	#[cfg(unix)]
	const COHORT_MAIN: &str = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";

	/// Two branches: `develop` (the default) and `main`, each with its own `alpha`.
	///
	/// `head_is_main`: the remote's HEAD still serves `main` while its
	/// advertisement already names `develop` — the default switched between the
	/// two calls.
	#[cfg(unix)]
	struct TwoBranchCatalog {
		head_is_main: bool,
		/// `repin_root`: during the fetch, rewrite every entry in this project's lock
		/// to `stable` — a concurrent `source sync --ref stable`.
		repin_root: Option<std::path::PathBuf>,
	}

	#[cfg(unix)]
	fn cohort_skill_md(commit: &str) -> String {
		let body = if commit == COHORT_MAIN {
			"main body"
		} else {
			"develop body"
		};
		format!("---\nname: alpha\ndescription: d\n---\n{body}\n")
	}

	#[cfg(unix)]
	impl aghub_git::RepoFetchBackend for TwoBranchCatalog {
		fn resolve(
			&self,
			src: &aghub_git::SourceRef,
			_a: Option<&aghub_git::Credentials>,
		) -> aghub_git::Result<aghub_git::RepoSnapshot> {
			let oid = match src.ref_.as_deref() {
				Some("main") => COHORT_MAIN,
				None if self.head_is_main => COHORT_MAIN,
				_ => COHORT_DEVELOP,
			};
			Ok(aghub_git::RepoSnapshot {
				commit_oid: oid.into(),
				tree_oid: oid.into(),
				commit_time: None,
			})
		}
		fn read_tree(
			&self,
			s: &aghub_git::RepoSnapshot,
		) -> aghub_git::Result<aghub_git::RepoTree> {
			Ok(aghub_git::RepoTree {
				entries: vec![aghub_git::TreeEntry {
					path: "alpha/SKILL.md".into(),
					mode: aghub_git::StagedEntryMode::Regular,
					oid: s.commit_oid.clone(),
					size: Some(cohort_skill_md(&s.commit_oid).len() as u64),
				}],
			})
		}
		fn read_blobs(
			&self,
			s: &aghub_git::RepoSnapshot,
			_o: &[String],
		) -> aghub_git::Result<Vec<aghub_git::Blob>> {
			Ok(vec![aghub_git::Blob {
				oid: s.commit_oid.clone(),
				bytes: cohort_skill_md(&s.commit_oid).into_bytes(),
			}])
		}
		fn materialize(
			&self,
			s: &aghub_git::RepoSnapshot,
			paths: &[&str],
			dest: &std::path::Path,
		) -> aghub_git::Result<()> {
			if let Some(root) = &self.repin_root {
				let mut lock = skill::read_local_lock(Some(root.as_path()));
				for entry in lock.skills.values_mut() {
					entry.ref_name = Some("stable".to_string());
				}
				skill::write_local_lock(&lock, Some(root.as_path())).unwrap();
			}
			for p in paths {
				std::fs::create_dir_all(dest.join(p)).unwrap();
				std::fs::write(
					dest.join(p).join("SKILL.md"),
					cohort_skill_md(&s.commit_oid),
				)
				.unwrap();
			}
			Ok(())
		}
		fn default_branch(
			&self,
			_s: &aghub_git::SourceRef,
			_a: Option<&aghub_git::Credentials>,
		) -> aghub_git::Result<Option<String>> {
			Ok(Some("develop".into()))
		}
	}

	#[cfg(unix)]
	#[test]
	fn install_skill_fetches_the_scopes_cohort_ref_not_the_default_branch() {
		with_isolated_env(|home, _state| {
			// The project records `alpha`'s cohort as `main` (a different ref than
			// the default branch `develop`).
			let project = home.join("proj");
			std::fs::create_dir_all(&project).unwrap();
			let mut lock = skill::LocalSkillLockFile::new();
			lock.skills.insert(
				"other".to_string(),
				skill::LocalSkillLockEntry {
					source: "o/r".to_string(),
					source_url: Some("https://github.com/o/r".to_string()),
					source_type: "github".to_string(),
					ref_name: Some("main".to_string()),
					skill_path: Some("other/SKILL.md".to_string()),
					computed_hash: "h".to_string(),
					ref_commit: None,
				},
			);
			skill::write_local_lock(&lock, Some(&project)).unwrap();

			let repo = std::sync::Arc::new(
				skill_update::SkillRepository::with_backends(
					None,
					std::sync::Arc::new(TwoBranchCatalog {
						head_is_main: false,
						repin_root: None,
					}),
				),
			);
			let req = crate::dto::skill::InstallSkillRequest {
				source: "https://github.com/o/r".to_string(),
				agents: vec!["claude".to_string()],
				skills: vec!["alpha".to_string()],
				scope: "project".to_string(),
				project_path: Some(project.display().to_string()),
				install_all: Some(false),
			};
			let resp =
				block_on(super::install_skill_with_repo(req, repo, None))
					.ok()
					.expect("install ok")
					.into_inner();
			assert!(resp.success, "{:?}", resp.agents);

			// The fetched bytes, the recorded ref and its commit must all be the
			// cohort's `main`, not the default branch `develop`.
			let lock = skill::read_local_lock(Some(&project));
			assert_eq!(lock.skills["alpha"].ref_name.as_deref(), Some("main"));
			assert_eq!(
				lock.skills["alpha"].ref_commit.as_deref(),
				Some(COHORT_MAIN)
			);
			assert!(std::fs::read_to_string(
				project.join(".aghub/alpha/SKILL.md")
			)
			.unwrap()
			.contains("main body"));
		});
	}

	#[cfg(unix)]
	#[test]
	fn install_skill_records_the_branch_it_fetched_when_the_default_switches() {
		with_isolated_env(|home, _state| {
			// A new source for this scope: no lock, so the default-branch lookup
			// runs. The remote's HEAD still serves `main` while it names `develop`.
			let project = home.join("proj");
			std::fs::create_dir_all(&project).unwrap();

			let repo = std::sync::Arc::new(
				skill_update::SkillRepository::with_backends(
					None,
					std::sync::Arc::new(TwoBranchCatalog {
						head_is_main: true,
						repin_root: None,
					}),
				),
			);
			let req = crate::dto::skill::InstallSkillRequest {
				source: "https://github.com/o/r".to_string(),
				agents: vec!["claude".to_string()],
				skills: vec!["alpha".to_string()],
				scope: "project".to_string(),
				project_path: Some(project.display().to_string()),
				install_all: Some(false),
			};
			let resp =
				block_on(super::install_skill_with_repo(req, repo, None))
					.ok()
					.expect("install ok")
					.into_inner();
			assert!(resp.success, "{:?}", resp.agents);

			// The recorded branch and its commit must match the bytes fetched:
			// all of `develop`, never `main` bytes under a `develop` name.
			let lock = skill::read_local_lock(Some(&project));
			assert_eq!(
				lock.skills["alpha"].ref_name.as_deref(),
				Some("develop")
			);
			assert_eq!(
				lock.skills["alpha"].ref_commit.as_deref(),
				Some(COHORT_DEVELOP)
			);
			assert!(std::fs::read_to_string(
				project.join(".aghub/alpha/SKILL.md")
			)
			.unwrap()
			.contains("develop body"));
		});
	}

	#[cfg(unix)]
	#[test]
	fn install_skill_refuses_when_the_entry_is_repinned_during_the_fetch() {
		with_isolated_env(|home, _state| {
			let project = home.join("proj");
			std::fs::create_dir_all(&project).unwrap();
			let mut lock = skill::LocalSkillLockFile::new();
			lock.skills.insert(
				"other".to_string(),
				skill::LocalSkillLockEntry {
					source: "o/r".to_string(),
					source_url: Some("https://github.com/o/r".to_string()),
					source_type: "github".to_string(),
					ref_name: Some("main".to_string()),
					skill_path: Some("other/SKILL.md".to_string()),
					computed_hash: "h".to_string(),
					ref_commit: None,
				},
			);
			skill::write_local_lock(&lock, Some(&project)).unwrap();

			let req = || crate::dto::skill::InstallSkillRequest {
				source: "https://github.com/o/r".to_string(),
				agents: vec!["claude".to_string()],
				skills: vec!["alpha".to_string()],
				scope: "project".to_string(),
				project_path: Some(project.display().to_string()),
				install_all: Some(false),
			};

			// Setup sanity: the first install records the cohort's `main`.
			let repo = std::sync::Arc::new(
				skill_update::SkillRepository::with_backends(
					None,
					std::sync::Arc::new(TwoBranchCatalog {
						head_is_main: false,
						repin_root: None,
					}),
				),
			);
			let resp =
				block_on(super::install_skill_with_repo(req(), repo, None))
					.ok()
					.expect("install ok")
					.into_inner();
			assert!(resp.success, "{:?}", resp.agents);
			assert_eq!(
				skill::read_local_lock(Some(&project)).skills["alpha"]
					.ref_name
					.as_deref(),
				Some("main")
			);

			// Second install: the pre-fetch read sees `main`, but the fetch repins
			// the scope to `stable`. The install must refuse, not heal back to `main`.
			let repo = std::sync::Arc::new(
				skill_update::SkillRepository::with_backends(
					None,
					std::sync::Arc::new(TwoBranchCatalog {
						head_is_main: false,
						repin_root: Some(project.clone()),
					}),
				),
			);
			let resp =
				block_on(super::install_skill_with_repo(req(), repo, None))
					.ok()
					.expect("route handled the install")
					.into_inner();
			assert!(!resp.success);
			assert!(
				resp.agents.iter().any(|row| row
					.error
					.as_deref()
					.is_some_and(|e| e.contains("now pinned to 'stable'"))),
				"{:?}",
				resp.agents
			);
			assert_eq!(
				skill::read_local_lock(Some(&project)).skills["alpha"]
					.ref_name
					.as_deref(),
				Some("stable"),
				"the repin must survive; a heal back to main is the bug"
			);
			assert!(std::fs::read_to_string(
				project.join(".aghub/alpha/SKILL.md")
			)
			.unwrap()
			.contains("main body"));
		});
	}

	#[cfg(unix)]
	#[test]
	fn git_install_failure_identifies_every_target_without_source_contents() {
		with_isolated_env(|home, _state| {
			let app_data = tempdir().unwrap();
			let client =
				rocket::local::blocking::Client::tracked(crate::build_rocket(
					rocket::Config::default(),
					app_data.path().to_path_buf(),
				))
				.expect("client");
			let sessions =
				client.rocket().state::<PinnedSourceSessions>().unwrap();
			let fixture = tempdir().unwrap();
			let source = fixture.path().join("broken-fixture");
			std::fs::create_dir_all(&source).unwrap();
			// Unparseable: no frontmatter at all, so the install fails
			// per-skill inside the mutation pool rather than 4xx-ing the
			// request. The trigger is incidental — what this pins is that ONE
			// per-skill failure still answers for EVERY requested target, and
			// that the row's error names neither the source contents nor the
			// server-side fetch path.
			std::fs::write(source.join("SKILL.md"), "not a skill at all\n")
				.unwrap();
			std::fs::write(
				source.join("payload.js"),
				"// PRIVATE_SOURCE_SENTINEL\n",
			)
			.unwrap();
			sessions.insert(
				"broken-session".into(),
				session_from_fixture(
					fixture.path(),
					"https://github.com/o/r",
					"main",
				),
			);
			let response = client
				.post("/api/v1/skills/git/install")
				.json(&serde_json::json!({
					"session_id": "broken-session", "skill_paths": ["broken-fixture"],
					"agents": ["codex", "claude"], "scope": "global"
				}))
				.dispatch();
			assert_eq!(response.status(), Status::Ok);
			let body: serde_json::Value = response.into_json().unwrap();
			let rows = body["results"].as_array().unwrap();
			assert_eq!(rows.len(), 2);
			for (row, agent) in rows.iter().zip(["codex", "claude"]) {
				assert_eq!(row["agent"], agent);
				assert_eq!(row["name"], "broken-fixture");
				assert_eq!(row["success"], false);
				let error = row["error"].as_str().unwrap();
				assert!(!error.is_empty(), "a failed row must say why");
				assert!(!error.contains("PRIVATE_SOURCE_SENTINEL"), "{error}");
				assert!(
					!error.contains(fixture.path().to_str().unwrap()),
					"{error}"
				);
			}
			assert!(!home.join(".aghub/broken-fixture").exists());
			assert!(!home.join(".codex/skills/broken-fixture").exists());
			assert!(!home.join(".claude/skills/broken-fixture").exists());
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
					// jetbrains-ai declares no skills scopes: the stable
					// unsupported sentinel (augmentcode was one until it
					// gained `.augment/skills`).
					"agents": ["claude", "jetbrains-ai"],
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
				!project.join(".aghub/my-skill").exists(),
				"route preflight must happen before the shared Master write",
			);
			assert!(
				!project.join(".claude/skills/my-skill").exists(),
				"the valid target must not be partially installed",
			);
		});
	}

	/// A one-skill git repo at `work`, built with gix (no `git` subprocess).
	#[cfg(unix)]
	fn write_single_skill_git_fixture(work: &std::path::Path) {
		use gix::objs::tree::{Entry, EntryKind};

		const SKILL_MD: &[u8] = b"---\nname: my-skill\ndescription: d\n---\n";
		let skill_dir = work.join("my-skill");
		std::fs::create_dir_all(&skill_dir).unwrap();
		std::fs::write(skill_dir.join("SKILL.md"), SKILL_MD).unwrap();

		let repo = gix::init(work).unwrap();
		let blob_id = repo.write_blob(SKILL_MD).unwrap().detach();
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
		let sig =
			gix::actor::SignatureRef::from_bytes(b"t <t@t> 1000000000 +0000")
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

	#[cfg(unix)]
	#[test]
	fn install_skill_returns_per_agent_rows_symlink_only() {
		with_isolated_env(|home, _state| {
			// Mock keyring: without it the install route fail-closes 503 on
			// hosts with no reachable credential backend (CI runners).
			let _keyring =
				crate::credentials::test_hooks::MockKeyringBackend::new();
			let work = home.join("work");
			write_single_skill_git_fixture(&work);

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
				home.join(".aghub/my-skill/SKILL.md").exists(),
				"master materialized (symlink-only)"
			);
		});
	}

	/// The confirmation gate must hold at the HTTP boundary, not just in core.
	///
	/// The original defect was API-only: `/skills/reconcile` removed without any
	/// gate while the CLI required `--yes`. A core-level test cannot catch a
	/// regression that hardcodes `confirm: true` (or flips `unwrap_or`) in this
	/// adapter, which would restore the exact bug with the suite still green.
	#[cfg(unix)]
	#[test]
	fn reconcile_route_refuses_removal_without_confirm() {
		with_isolated_env(|home, _state| {
			let project = home.join("proj");
			std::fs::create_dir_all(project.join(".claude/skills")).unwrap();
			let skill_dir = project.join(".claude/skills/verbs");
			std::fs::create_dir_all(&skill_dir).unwrap();
			std::fs::write(
				skill_dir.join("SKILL.md"),
				"---\nname: verbs\ndescription: d\n---\n",
			)
			.unwrap();

			let request = |confirm: Option<bool>| ReconcileRequest {
				source: crate::dto::transfer::ResourceLocatorDto {
					agent: "claude".to_string(),
					scope: crate::dto::transfer::InstallScopeDto::Project,
					project_root: Some(project.display().to_string()),
					name: "verbs".to_string(),
				},
				added: None,
				removed: Some(vec!["claude".to_string()]),
				confirm,
			};

			for omitted in [None, Some(false)] {
				let err = block_on(reconcile_skill_route(
					crate::extractors::TrustedLocalOrigin,
					rocket::serde::json::Json(request(omitted)),
				))
				.expect_err("a removal without confirmation must be rejected");
				assert_eq!(
					err.status,
					rocket::http::Status::BadRequest,
					"confirm={omitted:?} must be a 400"
				);
				assert!(
					skill_dir.join("SKILL.md").exists(),
					"confirm={omitted:?} must not delete anything"
				);
			}

			block_on(reconcile_skill_route(
				crate::extractors::TrustedLocalOrigin,
				rocket::serde::json::Json(request(Some(true))),
			))
			.ok()
			.expect("confirm: true must execute");
			assert!(
				!skill_dir.exists(),
				"the confirmed removal must actually run — otherwise the two \
				 assertions above would pass on a route that never removes"
			);
		});
	}

	// The aggregate used to fold in `installed`, which means "this call wrote
	// bytes" — false for an already-correctly-linked agent. Re-installing an
	// unchanged skill therefore reported `success: false` with every per-agent
	// row `success: true` and no error, and the desktop had to route around it.
	// The per-row assertion is the control: without it, a genuinely broken
	// second install would also satisfy "success is false".
	#[cfg(unix)]
	#[test]
	fn install_skill_reports_success_on_idempotent_reinstall() {
		with_isolated_env(|home, _state| {
			let _keyring =
				crate::credentials::test_hooks::MockKeyringBackend::new();
			let work = home.join("work");
			write_single_skill_git_fixture(&work);

			let install = || {
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
				block_on(install_skill_route_with_repo(
					req,
					ForwardedGitTokens::default(),
					repo,
				))
				.ok()
				.expect("handler ok")
				.into_inner()
			};

			assert!(install().success, "first install succeeds");

			let again = install();
			assert!(
				again.agents.iter().all(|a| a.success && a.error.is_none()),
				"every agent row is a success: {:?}",
				again.agents
			);
			assert!(
				again.success,
				"a no-op re-install is a success, not a failure"
			);
		});
	}

	#[cfg(unix)]
	#[test]
	fn delete_by_path_symlinked_install_uses_canonical_layout() {
		with_isolated_env(|home, _state| {
			let master = home.join(".aghub/linked");
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

			// This was the LAST Referrer, so the Master is collected too — core
			// pins that (`plan_removal_symlink_gc_canonical_when_last_referrer_
			// removed`). Both endings leave the same disk, so disk state cannot
			// tell "unlink the Referrer, then collect the Master" from
			// "`remove_dir_all` straight through the Referrer".
			//
			// The plan can. Canonical layout lists BOTH paths, Referrer FIRST:
			// a Referrer must never outlive the Master it points at, so
			// unlinking leads. Recursing through the link would name one path.
			// Compare NORMALIZED paths, never raw strings. The response
			// carries resolved paths, and on macOS the tempdir root is
			// `/var/...` while its resolved form is `/private/var/...` — so a
			// string compare passed on Linux and failed on macOS only. Root
			// `AGENTS.md`: never hand-roll path normalization; use
			// `resolve_existing`, which resolves the longest EXISTING prefix
			// and so still works for the paths this delete just removed.
			let norm = |p: &std::path::Path| {
				skill::lock::resolve_existing(p).display().to_string()
			};
			let paths: Vec<String> = resp
				.paths
				.iter()
				.map(|p| norm(std::path::Path::new(p)))
				.collect();
			let referrer_at = paths.iter().position(|p| p == &norm(&link));
			let master_at = paths.iter().position(|p| p == &norm(&master));
			assert!(
				referrer_at.is_some() && master_at.is_some(),
				"canonical layout removes the Referrer and the Master as two \
				 named steps, not one recursive delete: {paths:?}"
			);
			assert!(
				referrer_at < master_at,
				"the Referrer must be unlinked BEFORE the Master it points at, \
				 or a crash between the two leaves a dangling link that breaks \
				 npx's folder hash: {paths:?}"
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
	use crate::routes::CwdGuard;

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
				proj.join(".aghub/my-skill/SKILL.md").exists(),
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

		let (valid, invalid) =
			partition_install_agents_in_request_order(&req_agents);

		// also verify unknown agents go to invalid
		let (v2, inv2) = partition_install_agents_in_request_order(&[
			"claude".to_string(),
			"not-a-real-agent".to_string(),
		]);
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
				scope: WriteScope::project(&project_root),
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
			let master_path = project_root.join(".aghub/hello-skill/SKILL.md");
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
	// The desktop scan browses via `SkillRepository::list_pinned` (no whole-repo clone
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
			assert_eq!(
				snap.commit_oid(),
				COMMIT_OID,
				"scan pins the commit oid"
			);

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
					.resolve_pinned(&github_source(), None)
					.expect("resolve pins the scanned commit");
				assert_eq!(snap.commit_oid(), COMMIT_OID);

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
					home.join(".aghub/music/SKILL.md").exists(),
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
					.resolve_pinned(&github_source(), None)
					.expect("resolve pins the scanned commit");
				assert_eq!(snap.commit_oid(), COMMIT_A);
				assert_eq!(snap.snapshot().tree_oid, TREE_A);

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
				let body =
					std::fs::read_to_string(home.join(".aghub/music/SKILL.md"))
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
					home.join(".aghub/music/SKILL.md").exists(),
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
						// jetbrains-ai = the skills-unsupported sentinel.
						"agents": ["claude", "jetbrains-ai"],
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
					!project.join(".aghub/music").exists(),
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
				let master = dir_snapshot(&project.join(".aghub/music"));
				(entry, master)
			});

			// Surface B: desktop /skills/git/install, selecting by skill PATH.
			// The scan asked for no branch and the REST backend advertises no
			// default, so ref is None (== surface A), making the whole lock
			// entry directly comparable.
			let (entry_b, master_b) = with_isolated_env(|home, _state| {
				let (t, _rec) = record_transport(happy_responder());
				let rest: Arc<dyn RepoFetchBackend> =
					Arc::new(GithubRest::new(t));
				let repo = Arc::new(SkillRepository::with_backends(
					Some(rest),
					Arc::new(NoGixBackend),
				));
				let snap = repo
					.resolve_pinned(
						&SourceRef {
							source: "https://github.com/acme/skills.git".into(),
							ref_: None,
						},
						None,
					)
					.expect("resolve");
				assert_eq!(snap.commit_oid(), COMMIT_OID);
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
				let master = dir_snapshot(&project.join(".aghub/music"));
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
					// The session asked for no branch (`ref_: None`), so it records
					// none, as surface A does; asking for "main" would record "main".
					let snap = repo
						.resolve_pinned(
							&SourceRef {
								source: github_source().source,
								ref_: None,
							},
							None,
						)
						.expect("resolve");
					assert_eq!(snap.commit_oid(), FB_COMMIT);
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
					let master = dir_snapshot(&project.join(".aghub/foo"));
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

	/// `POST /skills/repair` had ZERO tests until this block — the desktop's
	/// one-click migration button, unexercised, on a route whose loop had
	/// already drifted from the CLI verb it claims to mirror.
	mod repair_route {
		use super::*;
		use std::path::{Path, PathBuf};

		fn client() -> rocket::local::blocking::Client {
			let app_data = tempdir().unwrap();
			rocket::local::blocking::Client::tracked(crate::build_rocket(
				rocket::Config::default(),
				app_data.path().to_path_buf(),
			))
			.expect("client")
		}

		/// The un-migrated layout: real directories in the shared slot, named
		/// by the PROJECT lock (version 1, `computedHash` — the global lock's
		/// shape reads as corrupt here and the fail-closed read would make
		/// every assertion pass without the route running).
		fn legacy_project(names: &[&str]) -> (tempfile::TempDir, PathBuf) {
			let temp = tempdir().unwrap();
			let root = temp.path().join("project");
			std::fs::create_dir_all(root.join(".claude")).unwrap();
			for n in names {
				let slot = root.join(".agents").join("skills").join(n);
				std::fs::create_dir_all(&slot).unwrap();
				std::fs::write(
					slot.join("SKILL.md"),
					format!("---\nname: {n}\ndescription: legacy\n---\n"),
				)
				.unwrap();
			}
			let entries: Vec<String> = names
				.iter()
				.map(|n| {
					format!(
						r#""{n}":{{"source":"o/r","sourceType":"github","computedHash":"deadbeef"}}"#
					)
				})
				.collect();
			std::fs::write(
				root.join("skills-lock.json"),
				format!(
					r#"{{"version":1,"skills":{{{}}}}}"#,
					entries.join(",")
				),
			)
			.unwrap();
			(temp, root)
		}

		fn post(
			client: &rocket::local::blocking::Client,
			root: &Path,
			dry_run: bool,
		) -> (Status, serde_json::Value) {
			with_pinned_data_dir(|_| {
				let response = client
					.post("/api/v1/skills/repair")
					.json(&serde_json::json!({
						"scope": "project",
						"project_root": root.to_str().unwrap(),
						"dry_run": dry_run,
					}))
					.dispatch();
				let status = response.status();
				let body: serde_json::Value = serde_json::from_str(
					&response.into_string().expect("response body"),
				)
				.expect("json body");
				(status, body)
			})
		}

		/// `#[cfg(unix)]` on the HELPER too, not just on its caller: Windows
		/// clippy runs the whole test module and a `std::os::unix` import left
		/// ungated goes red there — a gap that only surfaces on push to main.
		#[cfg(unix)]
		fn perms_enforced(path: &Path) -> bool {
			use std::os::unix::fs::PermissionsExt;
			let probe = path.join(".perm-probe");
			std::fs::create_dir_all(&probe).unwrap();
			let orig = std::fs::metadata(&probe).unwrap().permissions();
			std::fs::set_permissions(
				&probe,
				std::fs::Permissions::from_mode(0o000),
			)
			.unwrap();
			let denied = std::fs::read_dir(&probe).is_err();
			std::fs::set_permissions(&probe, orig).unwrap();
			std::fs::remove_dir_all(&probe).unwrap();
			denied
		}

		/// The button, working. Asserts DISK, not just the response — a route
		/// that answers `migrated` and writes nothing is the worse bug.
		#[test]
		fn a_bulk_repair_migrates_every_skill_the_lock_names() {
			let _guard = crate::routes::test_env_lock()
				.lock()
				.unwrap_or_else(|e| e.into_inner());
			let c = client();
			let (_temp, root) = legacy_project(&["alpha", "beta", "gamma"]);

			let (status, body) = post(&c, &root, false);

			assert_eq!(status, Status::Ok, "body: {body}");
			assert_eq!(body["skills"].as_array().unwrap().len(), 3);
			assert_eq!(body["refused"], false);
			for n in ["alpha", "beta", "gamma"] {
				assert!(
					root.join(".aghub").join(n).join("SKILL.md").is_file(),
					"{n} must have a real master on disk"
				);
				assert!(
					aghub_core::skills::linker::Linker::is_link(
						&root.join(".agents").join("skills").join(n)
					),
					"{n}'s shared slot must have become a referrer"
				);
			}
		}

		/// The preview must decide everything and write nothing — the desktop
		/// banner runs this on every render.
		#[test]
		fn a_dry_run_previews_without_touching_the_disk() {
			let _guard = crate::routes::test_env_lock()
				.lock()
				.unwrap_or_else(|e| e.into_inner());
			let c = client();
			let (_temp, root) = legacy_project(&["alpha", "beta"]);

			let (status, body) = post(&c, &root, true);

			assert_eq!(status, Status::Ok, "body: {body}");
			assert_eq!(body["dry_run"], true);
			assert_eq!(body["skills"].as_array().unwrap().len(), 2);
			assert!(
				!root.join(".aghub").exists(),
				"a preview must not create the store"
			);
			assert!(
				root.join(".agents/skills/alpha/SKILL.md").is_file(),
				"and the shared slot must still be the real directory"
			);
		}

		/// THE regression. One unreadable skill used to abort the whole loop
		/// with `?`, so the route answered HTTP 500 with no mention of the
		/// skills it had ALREADY migrated — observed live at 29 of 50.
		///
		/// Revert `repair_all`'s `Err` arm back to `?` and this goes red on the
		/// status code.
		#[test]
		#[cfg(unix)]
		fn one_unreadable_skill_still_returns_a_full_receipt() {
			use std::os::unix::fs::PermissionsExt;
			let _guard = crate::routes::test_env_lock()
				.lock()
				.unwrap_or_else(|e| e.into_inner());
			let c = client();
			let (_temp, root) = legacy_project(&["alpha", "beta", "gamma"]);
			if !perms_enforced(&root) {
				eprintln!("skip: perms not enforced (root)");
				return;
			}
			let beta = root.join(".agents").join("skills").join("beta");
			std::fs::set_permissions(
				&beta,
				std::fs::Permissions::from_mode(0o000),
			)
			.unwrap();

			let (status, body) = post(&c, &root, false);

			std::fs::set_permissions(
				&beta,
				std::fs::Permissions::from_mode(0o755),
			)
			.unwrap();

			assert_eq!(
				status,
				Status::Ok,
				"a single failing skill must not 500 the whole batch: {body}"
			);
			let rows = body["skills"].as_array().unwrap();
			assert_eq!(rows.len(), 3, "every skill must be accounted for");
			let outcome = |n: &str| {
				rows.iter().find(|r| r["name"] == n).unwrap()["outcome"]
					.as_str()
					.unwrap()
					.to_string()
			};
			assert_eq!(outcome("beta"), "failed");
			assert_eq!(outcome("alpha"), "migrated");
			assert_eq!(outcome("gamma"), "migrated");
			assert_eq!(
				body["refused"], true,
				"`refused` gates the desktop keeping its dialog open, and a \
				 failed row is exactly a row the user must still act on"
			);
			// The two that worked really landed.
			for n in ["alpha", "gamma"] {
				assert!(
					root.join(".aghub").join(n).join("SKILL.md").is_file(),
					"{n} migrated before the failure and must be on disk"
				);
			}
			// And nothing is unreadable: the whole safety claim.
			assert!(
				beta.join("SKILL.md").is_file(),
				"the failed skill must still be served by its old directory"
			);
		}

		/// The `name` path — the one the desktop's per-skill selection drives,
		/// one request per checked row.
		///
		/// Two things it must get right, neither of which the bulk tests touch:
		/// a NAMED repair reports the skill even when there is nothing to do
		/// (the caller asked about it), and the lock still gates ADOPTION, so a
		/// skill the lock does not name must not have its shared directory
		/// promoted to a master just because someone named it.
		#[test]
		fn a_named_repair_will_not_adopt_a_skill_the_lock_does_not_name() {
			let _guard = crate::routes::test_env_lock()
				.lock()
				.unwrap_or_else(|e| e.into_inner());
			let c = client();
			// The lock names `alpha` only; `orphan` is on disk but unlocked.
			let (_temp, root) = legacy_project(&["alpha"]);
			let orphan = root.join(".agents").join("skills").join("orphan");
			std::fs::create_dir_all(&orphan).unwrap();
			std::fs::write(
				orphan.join("SKILL.md"),
				"---\nname: orphan\ndescription: unlocked\n---\n",
			)
			.unwrap();

			let response = c
				.post("/api/v1/skills/repair")
				.json(&serde_json::json!({
					"scope": "project",
					"project_root": root.to_str().unwrap(),
					"name": "orphan",
					"dry_run": false,
				}))
				.dispatch();
			assert_eq!(response.status(), Status::Ok);
			let body: serde_json::Value =
				serde_json::from_str(&response.into_string().unwrap()).unwrap();

			let rows = body["skills"].as_array().unwrap();
			assert_eq!(
				rows.len(),
				1,
				"a NAMED repair always reports its skill, even with nothing \
				 to do: {body}"
			);
			assert_eq!(rows[0]["name"], "orphan");
			// `conformant`, not `refused`: with no lock entry there is nothing
			// repair MAY do, so it reports that it did nothing. Arguably
			// generous wording for a skill still in the legacy layout, but it
			// is the shipped answer and the load-bearing part is below — the
			// store must not gain an entry.
			assert_eq!(rows[0]["outcome"], "conformant", "{body}");
			assert!(
				!root.join(".aghub").join("orphan").exists(),
				"naming a skill must not promote it into the store — the lock \
				 is what decides adoption"
			);
			assert!(
				orphan.join("SKILL.md").is_file(),
				"and its directory must be left exactly as it was"
			);
		}

		/// A NAMED repair of a name nothing holds is refused, like the CLI.
		#[test]
		fn a_named_repair_of_an_unknown_name_is_refused() {
			let _guard = crate::routes::test_env_lock()
				.lock()
				.unwrap_or_else(|e| e.into_inner());
			let c = client();
			let (_temp, root) = legacy_project(&["alpha"]);
			let response = c
				.post("/api/v1/skills/repair")
				.json(&serde_json::json!({
					"scope": "project",
					"project_root": root.to_str().unwrap(),
					"name": "no-such-skill",
					"dry_run": true,
				}))
				.dispatch();
			assert_eq!(response.status(), Status::Ok);
			let body: serde_json::Value =
				serde_json::from_str(&response.into_string().unwrap()).unwrap();
			assert_eq!(body["refused"], true, "{body}");
			assert_eq!(body["skills"][0]["outcome"], "refused", "{body}");
		}

		/// A scope the route cannot resolve to one store is a 400, not a
		/// silent global write.
		#[test]
		fn project_scope_without_a_root_is_refused() {
			let c = client();
			let response = c
				.post("/api/v1/skills/repair")
				.json(&serde_json::json!({
					"scope": "project",
					"dry_run": true,
				}))
				.dispatch();
			assert_eq!(response.status(), Status::BadRequest);
			let body: serde_json::Value =
				serde_json::from_str(&response.into_string().unwrap()).unwrap();
			assert_eq!(body["code"], "PROJECT_ROOT_REQUIRED");
		}

		#[cfg(unix)]
		#[test]
		fn repair_relative_project_root_is_absolutized() {
			let _guard = crate::routes::test_env_lock()
				.lock()
				.unwrap_or_else(|e| e.into_inner());
			let (temp, root) = legacy_project(&["alpha"]);
			let temp_path = temp.path().canonicalize().unwrap();
			let root_canon = root.canonicalize().unwrap();
			let rel_root = root_canon.strip_prefix(&temp_path).unwrap();
			let _cwd = crate::routes::CwdGuard::change_to(&temp_path);
			let c = client();
			with_pinned_data_dir(|_| {
				let response = c
					.post("/api/v1/skills/repair")
					.json(&serde_json::json!({
						"scope": "project",
						"project_root": rel_root.to_str().unwrap(),
						"dry_run": false,
					}))
					.dispatch();
				assert_eq!(response.status(), Status::Ok);
				let body: serde_json::Value =
					serde_json::from_str(&response.into_string().unwrap())
						.unwrap();
				assert_eq!(body["refused"], false, "{body}");
				assert_eq!(body["skills"][0]["outcome"], "migrated", "{body}");

				let master_str = body["skills"][0]["master"]
					.as_str()
					.expect("master path present");
				let master = Path::new(master_str);
				assert!(
					master.is_absolute(),
					"master path in receipt must be absolute: {master_str}"
				);
				assert!(
					master.starts_with(&root_canon),
					"master path in receipt must start with canonical project root: {master_str}"
				);

				let referrers = body["skills"][0]["referrers"]
					.as_array()
					.expect("referrers list present");
				assert!(
					!referrers.is_empty(),
					"referrers list must not be empty"
				);
				for r in referrers {
					let ref_str = r.as_str().expect("referrer path string");
					let ref_path = Path::new(ref_str);
					assert!(
						ref_path.is_absolute(),
						"referrer path in receipt must be absolute: {ref_str}"
					);
					assert!(
						ref_path.starts_with(&root_canon),
						"referrer path in receipt must start with canonical project root: {ref_str}"
					);
				}

				assert!(
					root_canon.join(".aghub/alpha/SKILL.md").exists(),
					"Master must be materialized at absolutized project root"
				);
				assert!(
					aghub_core::skills::linker::Linker::is_link(
						&root_canon
							.join(".agents")
							.join("skills")
							.join("alpha")
					),
					"alpha's shared slot must have become a referrer"
				);
			});
		}
	}

	#[cfg(unix)]
	#[test]
	fn reconcile_skill_removal_row_reports_outcome_and_managed_holders() {
		with_isolated_env(|home, _state| {
			with_pinned_data_dir(|data_dir| {
				let client = rocket::local::blocking::Client::tracked(
					crate::build_rocket(
						rocket::Config::default(),
						data_dir.to_path_buf(),
					),
				)
				.expect("client");

				let disabled: std::collections::BTreeSet<String> =
					["opencode".to_string()].into_iter().collect();
				aghub_core::agent_settings::write_disabled_agents_in(
					data_dir, &disabled,
				)
				.unwrap();

				let project = home.join("proj");
				let master = project.join(".aghub/shared-skill");
				std::fs::create_dir_all(&master).unwrap();
				std::fs::write(
					master.join("SKILL.md"),
					"---\nname: shared-skill\ndescription: shared\n---\n",
				)
				.unwrap();

				let shared_slot = project.join(".agents/skills/shared-skill");
				std::fs::create_dir_all(shared_slot.parent().unwrap()).unwrap();
				std::os::unix::fs::symlink(&master, &shared_slot).unwrap();

				let claude_slot = project.join(".claude/skills/shared-skill");
				std::fs::create_dir_all(claude_slot.parent().unwrap()).unwrap();
				std::os::unix::fs::symlink(&master, &claude_slot).unwrap();

				let response = client
					.post("/api/v1/skills/reconcile")
					.json(&serde_json::json!({
						"source": {
							"agent": "claude",
							"scope": "project",
							"project_root": project.display().to_string(),
							"name": "shared-skill"
						},
						"removed": ["claude"],
						"confirm": true
					}))
					.dispatch();

				assert_eq!(response.status(), rocket::http::Status::Ok);
				let body: serde_json::Value = serde_json::from_str(
					&response.into_string().expect("response body"),
				)
				.expect("json body");

				let results =
					body["results"].as_array().expect("results array");
				assert_eq!(results.len(), 1);
				let row = &results[0];

				assert_eq!(row["agent"], "claude");
				assert_eq!(row["action"], "delete");
				assert_eq!(row["outcome"], "removed");

				let managed = row["still_read_by_managed"]
					.as_array()
					.expect("still_read_by_managed array");
				let unmanaged = row["still_read_by_unmanaged"]
					.as_array()
					.expect("still_read_by_unmanaged array");
				let all = row["still_read_by"]
					.as_array()
					.expect("still_read_by array");

				assert!(
					unmanaged.contains(&serde_json::json!("opencode")),
					"unmanaged list must contain disabled opencode: {unmanaged:?}"
				);
				assert!(
					!managed.contains(&serde_json::json!("opencode")),
					"managed list must not contain disabled opencode: {managed:?}"
				);
				assert!(
					all.contains(&serde_json::json!("opencode")),
					"all holders list must contain opencode: {all:?}"
				);
				assert!(
					!managed.is_empty(),
					"managed list must contain remaining enabled holders: {managed:?}"
				);
			});
		});
	}

	#[cfg(unix)]
	#[test]
	fn reconcile_skill_removal_row_with_added_keeps_master_reports_holders() {
		with_isolated_env(|home, _state| {
			with_pinned_data_dir(|data_dir| {
				let client = rocket::local::blocking::Client::tracked(
					crate::build_rocket(
						rocket::Config::default(),
						data_dir.to_path_buf(),
					),
				)
				.expect("client");

				let project = home.join("proj");
				let master = project.join(".aghub/shared-skill");
				std::fs::create_dir_all(&master).unwrap();
				std::fs::write(
					master.join("SKILL.md"),
					"---\nname: shared-skill\ndescription: shared\n---\n",
				)
				.unwrap();

				let shared_slot = project.join(".agents/skills/shared-skill");
				std::fs::create_dir_all(shared_slot.parent().unwrap()).unwrap();
				std::os::unix::fs::symlink(&master, &shared_slot).unwrap();

				let claude_slot = project.join(".claude/skills/shared-skill");
				std::fs::create_dir_all(claude_slot.parent().unwrap()).unwrap();
				std::os::unix::fs::symlink(&master, &claude_slot).unwrap();

				let response = client
					.post("/api/v1/skills/reconcile")
					.json(&serde_json::json!({
						"source": {
							"agent": "claude",
							"scope": "project",
							"project_root": project.display().to_string(),
							"name": "shared-skill"
						},
						"added": ["windsurf"],
						"removed": ["claude"],
						"confirm": true
					}))
					.dispatch();

				assert_eq!(response.status(), rocket::http::Status::Ok);
				let body: serde_json::Value = serde_json::from_str(
					&response.into_string().expect("response body"),
				)
				.expect("json body");

				let results =
					body["results"].as_array().expect("results array");
				let delete_row = results
					.iter()
					.find(|r| r["action"] == "delete")
					.expect("delete row");

				let managed = delete_row["still_read_by_managed"]
					.as_array()
					.expect("still_read_by_managed array");
				assert!(
					!managed.is_empty(),
					"delete row must carry surviving holders when added is non-empty (keeps_master): {managed:?}"
				);
			});
		});
	}

	#[cfg(unix)]
	#[test]
	fn delete_by_name_preview_and_commit_carry_outcome_and_managed_holders() {
		with_isolated_env(|home, _state| {
			with_pinned_data_dir(|data_dir| {
				let client = rocket::local::blocking::Client::tracked(
					crate::build_rocket(
						rocket::Config::default(),
						data_dir.to_path_buf(),
					),
				)
				.expect("client");

				let disabled: std::collections::BTreeSet<String> =
					["opencode".to_string()].into_iter().collect();
				aghub_core::agent_settings::write_disabled_agents_in(
					data_dir, &disabled,
				)
				.unwrap();

				let project = home.join("proj");
				let master = project.join(".aghub/by-name-skill");
				std::fs::create_dir_all(&master).unwrap();
				std::fs::write(
					master.join("SKILL.md"),
					"---\nname: by-name-skill\ndescription: test\n---\n",
				)
				.unwrap();

				let shared_slot = project.join(".agents/skills/by-name-skill");
				std::fs::create_dir_all(shared_slot.parent().unwrap()).unwrap();
				std::os::unix::fs::symlink(&master, &shared_slot).unwrap();

				let claude_slot = project.join(".claude/skills/by-name-skill");
				std::fs::create_dir_all(claude_slot.parent().unwrap()).unwrap();
				std::os::unix::fs::symlink(&master, &claude_slot).unwrap();

				// 1. Dry-run preview (confirm omitted)
				let preview_resp = client
					.delete(format!(
						"/api/v1/agents/claude/skills/by-name-skill?scope=project&project_root={}",
						project.display()
					))
					.dispatch();

				assert_eq!(preview_resp.status(), rocket::http::Status::Ok);
				let preview_body: serde_json::Value = serde_json::from_str(
					&preview_resp.into_string().expect("response body"),
				)
				.expect("json body");

				assert_eq!(preview_body["dry_run"], true);
				assert_eq!(
					preview_body["outcome"], "preview",
					"preview must carry preview outcome: {preview_body}"
				);
				let preview_unmanaged = preview_body["still_read_by_unmanaged"]
					.as_array()
					.expect("still_read_by_unmanaged");
				let preview_managed = preview_body["still_read_by_managed"]
					.as_array()
					.expect("still_read_by_managed");
				let preview_all = preview_body["still_read_by"]
					.as_array()
					.expect("still_read_by");

				assert!(
					preview_unmanaged.contains(&serde_json::json!("opencode"))
				);
				assert!(
					!preview_managed.contains(&serde_json::json!("opencode"))
				);
				assert!(preview_all.contains(&serde_json::json!("opencode")));
				assert!(!preview_managed.is_empty());
				assert!(
					claude_slot.exists(),
					"preview must not delete disk files"
				);

				// 2. Commit (confirm=true)
				let commit_resp = client
					.delete(format!(
						"/api/v1/agents/claude/skills/by-name-skill?scope=project&project_root={}&confirm=true",
						project.display()
					))
					.dispatch();

				assert_eq!(commit_resp.status(), rocket::http::Status::Ok);
				let commit_body: serde_json::Value = serde_json::from_str(
					&commit_resp.into_string().expect("response body"),
				)
				.expect("json body");

				assert_eq!(commit_body["dry_run"], false);
				assert_eq!(commit_body["outcome"], "removed");
				let commit_unmanaged = commit_body["still_read_by_unmanaged"]
					.as_array()
					.expect("still_read_by_unmanaged");
				let commit_managed = commit_body["still_read_by_managed"]
					.as_array()
					.expect("still_read_by_managed");
				let commit_all = commit_body["still_read_by"]
					.as_array()
					.expect("still_read_by");

				assert!(
					commit_unmanaged.contains(&serde_json::json!("opencode"))
				);
				assert!(
					!commit_managed.contains(&serde_json::json!("opencode"))
				);
				assert!(commit_all.contains(&serde_json::json!("opencode")));
				assert!(!commit_managed.is_empty());
				assert!(
					!claude_slot.exists(),
					"commit must remove requested slot"
				);
				assert!(
					master.exists(),
					"master must be kept since shared slot survives"
				);

				// 3. Preview with all_agents=true while disabled opencode holds skill in private slot
				std::os::unix::fs::symlink(&master, &claude_slot).unwrap();
				let opencode_slot =
					project.join(".opencode/skills/by-name-skill");
				std::fs::create_dir_all(opencode_slot.parent().unwrap())
					.unwrap();
				std::os::unix::fs::symlink(&master, &opencode_slot).unwrap();

				let all_preview_resp = client
					.delete(format!(
						"/api/v1/agents/claude/skills/by-name-skill?scope=project&project_root={}&all_agents=true",
						project.display()
					))
					.dispatch();

				assert_eq!(all_preview_resp.status(), rocket::http::Status::Ok);
				let all_preview_body: serde_json::Value = serde_json::from_str(
					&all_preview_resp.into_string().expect("response body"),
				)
				.expect("json body");

				let preview_unmanaged = all_preview_body
					["still_read_by_unmanaged"]
					.as_array()
					.expect("still_read_by_unmanaged array");
				assert!(
					preview_unmanaged.contains(&serde_json::json!("opencode")),
					"disabled agent with private slot must appear in still_read_by_unmanaged: {all_preview_body}"
				);

				// Remove disabled agent's private slot so all_agents commit can clean up the shared slot
				std::fs::remove_file(&opencode_slot).unwrap();

				// 4. Commit with all_agents=true: remaining holders removed, keepers empty
				let all_resp = client
					.delete(format!(
						"/api/v1/agents/claude/skills/by-name-skill?scope=project&project_root={}&confirm=true&all_agents=true",
						project.display()
					))
					.dispatch();

				assert_eq!(all_resp.status(), rocket::http::Status::Ok);
				let all_body: serde_json::Value = serde_json::from_str(
					&all_resp.into_string().expect("response body"),
				)
				.expect("json body");

				assert_eq!(all_body["outcome"], "removed");
				assert!(
					all_body.get("still_read_by").is_none_or(|v| v.is_null()
						|| v.as_array().is_some_and(|a| a.is_empty())),
					"still_read_by must be absent or empty: {all_body}"
				);
				assert!(
					all_body
						.get("still_read_by_managed")
						.is_none_or(|v| v.is_null()
							|| v.as_array().is_some_and(|a| a.is_empty())),
					"still_read_by_managed must be absent or empty: {all_body}"
				);
				assert!(
					all_body.get("still_read_by_unmanaged").is_none_or(|v| v.is_null() || v.as_array().is_some_and(|a| a.is_empty())),
					"still_read_by_unmanaged must be absent or empty: {all_body}"
				);
				assert!(!claude_slot.exists(), "target slot must be removed");
				assert!(
					!shared_slot.exists(),
					"all_agents delete must remove other holder slot"
				);
				assert!(
					!master.exists(),
					"master must be reclaimed when all holders are gone"
				);
			});
		});
	}

	#[cfg(unix)]
	#[test]
	fn delete_by_name_preview_refuses_when_shape_needs_repair() {
		with_isolated_env(|home, _state| {
			with_pinned_data_dir(|data_dir| {
				let client = rocket::local::blocking::Client::tracked(
					crate::build_rocket(
						rocket::Config::default(),
						data_dir.to_path_buf(),
					),
				)
				.expect("client");

				let project = home.join("proj");
				let name = "forked-skill";
				let master = project.join(".aghub").join(name);
				std::fs::create_dir_all(&master).unwrap();
				std::fs::write(
					master.join("SKILL.md"),
					format!("---\nname: {name}\ndescription: master\n---\n"),
				)
				.unwrap();

				// npx clobber: real directory in .agents/skills instead of symlink
				let shared = project.join(".agents/skills").join(name);
				std::fs::create_dir_all(&shared).unwrap();
				std::fs::write(
					shared.join("SKILL.md"),
					format!("---\nname: {name}\ndescription: clobber\n---\n"),
				)
				.unwrap();

				// Cursor reads .agents/skills
				let resp = client
					.delete(format!(
						"/api/v1/agents/cursor/skills/{name}?scope=project&project_root={}",
						project.display()
					))
					.dispatch();

				assert_eq!(
					resp.status(),
					rocket::http::Status::UnprocessableEntity
				);
				let body: serde_json::Value = serde_json::from_str(
					&resp.into_string().expect("response body"),
				)
				.expect("json body");
				assert_eq!(body["code"], "UNSUPPORTED_OPERATION");
				assert!(body["error"].as_str().unwrap().contains("repair"));
			});
		});
	}

	#[cfg(unix)]
	#[test]
	fn delete_by_name_preflight_rejection_carries_structured_rejected_targets()
	{
		with_isolated_env(|home, _state| {
			with_pinned_data_dir(|data_dir| {
				let client = rocket::local::blocking::Client::tracked(
					crate::build_rocket(
						rocket::Config::default(),
						data_dir.to_path_buf(),
					),
				)
				.expect("client");

				let project = home.join("proj");
				let master = project.join(".aghub/notebooklm");
				std::fs::create_dir_all(&master).unwrap();
				std::fs::write(
					master.join("SKILL.md"),
					"---\nname: notebooklm\ndescription: test\n---\n",
				)
				.unwrap();

				let claude_slot = project.join(".claude/skills/notebooklm");
				std::fs::create_dir_all(claude_slot.parent().unwrap()).unwrap();
				std::os::unix::fs::symlink(&master, &claude_slot).unwrap();

				let shared_slot = project.join(".agents/skills/notebooklm");
				std::fs::create_dir_all(shared_slot.parent().unwrap()).unwrap();
				std::os::unix::fs::symlink(&master, &shared_slot).unwrap();

				let cursor_slot = project.join(".cursor/skills/notebooklm");
				std::fs::create_dir_all(cursor_slot.parent().unwrap()).unwrap();
				std::os::unix::fs::symlink(&master, &cursor_slot).unwrap();

				let opencode_slot = project.join(".opencode/skills/notebooklm");
				std::fs::create_dir_all(opencode_slot.parent().unwrap())
					.unwrap();
				std::os::unix::fs::symlink(&master, &opencode_slot).unwrap();

				let response = client
					.delete(format!(
						"/api/v1/agents/opencode/skills/notebooklm?scope=project&project_root={}&confirm=true",
						project.display()
					))
					.dispatch();

				assert_eq!(
					response.status(),
					rocket::http::Status::UnprocessableEntity
				);
				let body: serde_json::Value = serde_json::from_str(
					&response.into_string().expect("response body"),
				)
				.expect("json body");

				assert_eq!(body["code"], "UNSUPPORTED_OPERATION");
				let rejected = body["rejected_targets"]
					.as_array()
					.expect("rejected_targets array");
				assert_eq!(rejected.len(), 1);
				assert_eq!(rejected[0]["agent"], "opencode");
				let reason =
					rejected[0]["reason"].as_str().expect("reason string");
				assert!(
					reason.contains("location shared with other agents"),
					"reason must contain refusal detail, got: {reason}"
				);
			});
		});
	}

	#[cfg(unix)]
	#[test]
	fn reconcile_route_preflight_rejection_carries_structured_rejected_targets()
	{
		with_isolated_env(|home, _state| {
			with_pinned_data_dir(|data_dir| {
				let client = rocket::local::blocking::Client::tracked(
					crate::build_rocket(
						rocket::Config::default(),
						data_dir.to_path_buf(),
					),
				)
				.expect("client");

				let project = home.join("proj");
				let master = project.join(".aghub/reconcile-reject");
				std::fs::create_dir_all(&master).unwrap();
				std::fs::write(
					master.join("SKILL.md"),
					"---\nname: reconcile-reject\ndescription: test\n---\n",
				)
				.unwrap();

				let codex_slot = project.join(".codex/skills/reconcile-reject");
				std::fs::create_dir_all(codex_slot.parent().unwrap()).unwrap();
				std::os::unix::fs::symlink(&master, &codex_slot).unwrap();

				// Zed does not support project skill scope -> preflight refusal
				let response = client
					.post("/api/v1/skills/reconcile")
					.header(rocket::http::ContentType::JSON)
					.body(
						serde_json::to_string(&serde_json::json!({
							"source": {
								"agent": "codex",
								"scope": "project",
								"project_root": project.display().to_string(),
								"name": "reconcile-reject"
							},
							"removed": ["zed"],
							"confirm": true
						}))
						.unwrap(),
					)
					.dispatch();

				assert_eq!(
					response.status(),
					rocket::http::Status::UnprocessableEntity
				);
				let body: serde_json::Value = serde_json::from_str(
					&response.into_string().expect("response body"),
				)
				.expect("json body");

				assert_eq!(body["code"], "UNSUPPORTED_OPERATION");
				let rejected = body["rejected_targets"]
					.as_array()
					.expect("rejected_targets array");
				assert_eq!(rejected.len(), 1);
				assert_eq!(rejected[0]["agent"], "zed");
				let reason =
					rejected[0]["reason"].as_str().expect("reason string");
				assert!(
					reason.contains("no project skill config"),
					"reason must contain refusal detail, got: {reason}"
				);
			});
		});
	}

	#[cfg(unix)]
	#[test]
	fn reconcile_route_shared_slot_refusal_carries_kind_and_readers() {
		with_isolated_env(|home, _state| {
			with_pinned_data_dir(|data_dir| {
				let client = rocket::local::blocking::Client::tracked(
					crate::build_rocket(
						rocket::Config::default(),
						data_dir.to_path_buf(),
					),
				)
				.expect("client");

				let project = home.join("proj");
				let shared_slot = project.join(".agents/skills/notebooklm");
				std::fs::create_dir_all(&shared_slot).unwrap();
				std::fs::write(
					shared_slot.join("SKILL.md"),
					"---\nname: notebooklm\ndescription: test\n---\n",
				)
				.unwrap();

				let cursor_slot = project.join(".cursor/skills/notebooklm");
				std::fs::create_dir_all(cursor_slot.parent().unwrap()).unwrap();
				std::os::unix::fs::symlink(&shared_slot, &cursor_slot).unwrap();

				let opencode_slot = project.join(".opencode/skills/notebooklm");
				std::fs::create_dir_all(opencode_slot.parent().unwrap())
					.unwrap();
				std::os::unix::fs::symlink(&shared_slot, &opencode_slot)
					.unwrap();

				let response = client
					.post("/api/v1/skills/reconcile")
					.header(rocket::http::ContentType::JSON)
					.body(
						serde_json::to_string(&serde_json::json!({
							"source": {
								"agent": "opencode",
								"scope": "project",
								"project_root": project.display().to_string(),
								"name": "notebooklm"
							},
							"removed": ["opencode"],
							"confirm": true
						}))
						.unwrap(),
					)
					.dispatch();

				assert_eq!(
					response.status(),
					rocket::http::Status::UnprocessableEntity
				);
				let body: serde_json::Value = serde_json::from_str(
					&response.into_string().expect("response body"),
				)
				.expect("json body");

				assert_eq!(body["code"], "UNSUPPORTED_OPERATION");
				let rejected = body["rejected_targets"]
					.as_array()
					.expect("rejected_targets array");
				assert_eq!(rejected.len(), 1);
				assert_eq!(rejected[0]["agent"], "opencode");
				assert_eq!(rejected[0]["kind"], "shared");
				let readers = rejected[0]["readers"]
					.as_array()
					.expect("readers must be present and an array");
				assert!(!readers.is_empty(), "readers must be non-empty");
				assert!(
					readers.iter().any(|r| r["agent"] == "cursor"),
					"readers must contain cursor: {readers:?}"
				);
			});
		});
	}

	#[cfg(unix)]
	#[test]
	fn delete_by_name_preflight_rejection_on_planner_error_carries_rejected_targets(
	) {
		with_isolated_env(|home, _state| {
			with_pinned_data_dir(|data_dir| {
				let client = rocket::local::blocking::Client::tracked(
					crate::build_rocket(
						rocket::Config::default(),
						data_dir.to_path_buf(),
					),
				)
				.expect("client");

				let project = home.join("proj");
				let master = project.join(".aghub/dup-skill");
				std::fs::create_dir_all(&master).unwrap();
				std::fs::write(
					master.join("SKILL.md"),
					"---\nname: dup-skill\ndescription: test\n---\n",
				)
				.unwrap();

				// Second store folder with duplicate skill name -> planner error
				let dup_master = project.join(".aghub/dup-skill-second");
				std::fs::create_dir_all(&dup_master).unwrap();
				std::fs::write(
					dup_master.join("SKILL.md"),
					"---\nname: dup-skill\ndescription: dup\n---\n",
				)
				.unwrap();

				let claude_slot = project.join(".claude/skills/dup-skill");
				std::fs::create_dir_all(claude_slot.parent().unwrap()).unwrap();
				std::os::unix::fs::symlink(&master, &claude_slot).unwrap();

				let response = client
					.delete(format!(
						"/api/v1/agents/claude/skills/dup-skill?scope=project&project_root={}&confirm=true",
						project.display()
					))
					.dispatch();

				assert_eq!(response.status(), rocket::http::Status::BadRequest);
				let body: serde_json::Value = serde_json::from_str(
					&response.into_string().expect("response body"),
				)
				.expect("json body");

				assert_eq!(body["code"], "INVALID_CONFIG");
				let rejected = body["rejected_targets"]
					.as_array()
					.expect("rejected_targets array");
				assert_eq!(rejected.len(), 1);
				assert_eq!(rejected[0]["agent"], "claude");
				assert!(
					rejected[0]["reason"]
						.as_str()
						.unwrap()
						.contains("two Masters"),
					"reason must contain planner error detail: {}",
					rejected[0]["reason"]
				);
			});
		});
	}

	#[cfg(unix)]
	#[test]
	fn delete_by_name_preview_with_orphan_lock_discloses_prune_and_preserves_lock(
	) {
		with_isolated_env(|home, _state| {
			with_pinned_data_dir(|data_dir| {
				let client = rocket::local::blocking::Client::tracked(
					crate::build_rocket(
						rocket::Config::default(),
						data_dir.to_path_buf(),
					),
				)
				.expect("client");

				let project = home.join("proj");
				std::fs::create_dir_all(project.join(".claude")).unwrap();

				let lock_path = project.join("skills-lock.json");
				let initial_raw = r#"{"version":1,"skills":{"orphan-skill":{"source":"o/r","sourceType":"github","computedHash":"deadbeef"}}}"#;
				std::fs::write(&lock_path, initial_raw).unwrap();

				let response = client
					.delete(format!(
						"/api/v1/agents/claude/skills/orphan-skill?scope=project&project_root={}",
						project.display()
					))
					.dispatch();

				assert_eq!(response.status(), rocket::http::Status::Ok);
				let body: serde_json::Value = serde_json::from_str(
					&response.into_string().expect("response body"),
				)
				.expect("json body");

				let would_prune = body["would_prune_lock_entries"]
					.as_array()
					.expect("would_prune_lock_entries array");
				assert!(
					would_prune.contains(&serde_json::json!("orphan-skill")),
					"must disclose orphan lock entry to prune: {body}"
				);

				let after_raw = std::fs::read_to_string(&lock_path).unwrap();
				assert_eq!(
					initial_raw, after_raw,
					"preview must not modify the lock file"
				);
			});
		});
	}

	#[cfg(unix)]
	#[test]
	fn get_skill_holders_returns_managed_unmanaged_split() {
		with_isolated_env(|home, _state| {
			with_pinned_data_dir(|data_dir| {
				let client = rocket::local::blocking::Client::tracked(
					crate::build_rocket(
						rocket::Config::default(),
						data_dir.to_path_buf(),
					),
				)
				.expect("client");

				let disabled: std::collections::BTreeSet<String> =
					["opencode".to_string()].into_iter().collect();
				aghub_core::agent_settings::write_disabled_agents_in(
					data_dir, &disabled,
				)
				.unwrap();

				let project = home.join("proj");
				let master = project.join(".aghub/holders-skill");
				std::fs::create_dir_all(&master).unwrap();
				std::fs::write(
					master.join("SKILL.md"),
					"---\nname: holders-skill\ndescription: test\n---\n",
				)
				.unwrap();

				let opencode_slot =
					project.join(".agents/skills/holders-skill");
				std::fs::create_dir_all(opencode_slot.parent().unwrap())
					.unwrap();
				std::os::unix::fs::symlink(&master, &opencode_slot).unwrap();

				let claude_slot = project.join(".claude/skills/holders-skill");
				std::fs::create_dir_all(claude_slot.parent().unwrap()).unwrap();
				std::os::unix::fs::symlink(&master, &claude_slot).unwrap();

				let resp = client
					.get(format!(
						"/api/v1/skills/holders-skill/holders?scope=project&project_root={}",
						project.display()
					))
					.dispatch();

				assert_eq!(resp.status(), rocket::http::Status::Ok);
				let body: serde_json::Value = serde_json::from_str(
					&resp.into_string().expect("response body"),
				)
				.expect("json body");

				let managed = body["managed"]
					.as_array()
					.expect("managed array")
					.iter()
					.map(|v| v.as_str().unwrap().to_string())
					.collect::<Vec<_>>();
				let unmanaged = body["unmanaged"]
					.as_array()
					.expect("unmanaged array")
					.iter()
					.map(|v| v.as_str().unwrap().to_string())
					.collect::<Vec<_>>();

				assert!(managed.contains(&"claude".to_string()));
				assert!(!managed.contains(&"opencode".to_string()));
				assert!(unmanaged.contains(&"opencode".to_string()));
				assert!(!unmanaged.contains(&"claude".to_string()));
			})
		});
	}
}

/// `POST /skills/repair` — the desktop's one-click migration.
///
/// Same core seam as the CLI verb (`repair_skill`), so the two surfaces cannot
/// answer differently: the mutation lock, the "who reads this today" question
/// and the plan all live in core, and this route is a thin adapter.
///
/// Runs in `in_mutation_pool` because acquiring the guard BLOCKS the thread —
/// on a Rocket worker, enough contended requests would park every worker and
/// the server would stop answering even unlocked reads.
#[derive(serde::Deserialize)]
pub struct RepairRequest {
	/// `"global"` or `"project"`. There is no `both`: repair moves directories
	/// and must resolve exactly ONE store.
	pub scope: String,
	pub project_root: Option<String>,
	/// Omitted = every skill the lock names at this scope (the bulk migration).
	pub name: Option<String>,
	/// Defaults to a PREVIEW. A client that forgets the field gets the safe
	/// answer, matching the CLI's dry-run-unless-`--yes` house rule.
	#[serde(default = "default_true")]
	pub dry_run: bool,
}

fn default_true() -> bool {
	true
}

#[post("/skills/repair", format = "json", data = "<body>")]
pub async fn repair_skills_route(
	_origin: TrustedLocalOrigin,
	body: Json<RepairRequest>,
) -> ApiResult<crate::dto::repair::RepairResponse> {
	let req = body.into_inner();
	let write_scope = crate::extractors::resolve_write_scope(
		&req.scope,
		req.project_root.as_deref(),
	)?;
	let name = req.name.clone();
	let dry_run = req.dry_run;

	in_mutation_pool(move || {
		// The whole batch — worklist, fail-closed lock read, the ONE outer
		// mutation guard, and turning a single skill's I/O error into a row
		// rather than aborting — lives in core, so this route and the CLI verb
		// cannot answer differently. They used to: this loop took no bulk guard,
		// making a fifty-skill desktop migration fifty independently racing
		// mutations, and it aborted on the first error with no report of the
		// skills already migrated.
		let reports = aghub_core::skills::repair::repair_all(
			&write_scope,
			name.as_deref(),
			dry_run,
		)
		.map_err(ApiError::from)?;

		let skills: Vec<crate::dto::repair::RepairReportDto> = reports
			.iter()
			.map(crate::dto::repair::RepairReportDto::from)
			.collect();

		Ok(Json(crate::dto::repair::RepairResponse {
			dry_run,
			scope: req.scope.clone(),
			// `failed` counts too: the desktop reads this flag to keep the
			// dialog open, and a row the user still has to act on is exactly
			// what that is for.
			refused: skills
				.iter()
				.any(|s| s.outcome == "refused" || s.outcome == "failed"),
			skills,
		}))
	})
	.await
}
