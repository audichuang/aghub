//! `GET /skills/check-updates` — read-only update check for installed skills.
//!
//! Reads the global skill lock, projects each entry to the orchestrator's
//! [`EntryInput`], then delegates to the pure-ish F1.5 runner
//! ([`skill_update::check_updates`]).
//!
//! Network + credential resolution stay in this crate (never in `crates/core`).
//! The [`Fetcher`] materializes a worktree into a [`tempfile::TempDir`] (the
//! documented worst-case fallback — a checkout into a temp dir, never the `git`
//! binary), and the [`TokenResolver`] wraps the F1.4 keyring/keychain
//! resolution. Every gix error string is redacted of URL userinfo upstream so a
//! token can never leak into the response.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use aghub_core::models::ResourceScope;
use aghub_core::skills::removal::skill_root;
use chrono::Utc;
use rocket::http::Status;
use rocket::serde::json::Json;

use crate::credentials::forwarding::{ChainResolver, ForwardedGitTokens};
use crate::dto::skill::{
	AcceptRenameRequest, AcceptRenameResponse, ApplySkillUpdateRequest,
	ApplySkillUpdateResponse, SkillUpdateResponse, SkillUpdateStatusResponse,
};
use crate::error::{ApiError, ApiResult};
use crate::extractors::{ResolvedScope, ScopeParams, TrustedLocalOrigin};
use crate::skills::rename::{skill_renamed_message, SKILL_RENAMED_CODE};
use skill_update::{
	check_updates, keychain_host_for_source, CheckDeps, CheckOutput,
	EntryInput, FetchError, Fetcher, GitFetcherWithFallback, GitRefResolver,
	ResultCache, SourceRef, TokenResolver,
};

/// Default per-fetch timeout. Generous enough for a small skill repo clone but
/// bounded so a stuck remote cannot hang the request.
const PER_FETCH: Duration = Duration::from_secs(30);
const OVERALL_DEADLINE: Duration = Duration::from_secs(120);
/// Default bounded concurrency for upstream fetches.
const CONCURRENCY: usize = 4;
/// TTL for the per-request result cache. The cache is request-scoped here, so
/// this only dedups identical `(source, ref)` groups within one call.
const CACHE_TTL: Duration = Duration::from_secs(60);

/// Production [`TokenResolver`]: wraps the F1.4 keyring source→credential
/// binding + host keychain resolution. Loads the stored credentials and
/// bindings lazily per resolve (cheap; keyring reads are local).
struct KeyringResolver;

impl TokenResolver for KeyringResolver {
	fn resolve(&self, source: &str, host: Option<&str>) -> Option<String> {
		let creds =
			crate::routes::credentials::load_credentials().unwrap_or_default();
		let bindings = crate::credentials::resolve::load_source_bindings()
			.unwrap_or_default();
		crate::credentials::resolve::resolve_token_for_source(
			source, host, &bindings, &creds,
		)
	}
}

/// Query parameters for the update check. `offline` short-circuits every entry
/// to `Uncheckable { network }` without touching the network (useful for tests
/// and air-gapped environments).
#[derive(rocket::FromForm)]
pub struct CheckUpdatesParams {
	offline: Option<bool>,
	scope: Option<String>,
	project_root: Option<String>,
}

fn local_hashes_for_scope(
	resource_scope: ResourceScope,
	project_root: Option<&Path>,
) -> HashMap<String, String> {
	let mut hashes = HashMap::new();
	let mut ambiguous = HashSet::new();
	for agent in aghub_core::load_all_agents(resource_scope, project_root) {
		for skill in agent.skills {
			if ambiguous.contains(&skill.name) {
				continue;
			}
			let Some(root) = skill_root(&skill) else {
				continue;
			};
			let Ok(hash) = skill::compute_skill_folder_hash(&root) else {
				continue;
			};
			match hashes.get(&skill.name) {
				Some(existing) if existing != &hash => {
					hashes.remove(&skill.name);
					ambiguous.insert(skill.name);
				}
				Some(_) => {}
				None => {
					hashes.insert(skill.name, hash);
				}
			}
		}
	}
	hashes
}

/// Project the global skill lock into the orchestrator's per-entry inputs.
fn global_lock_entries(
	local_hashes: &HashMap<String, String>,
) -> Vec<EntryInput> {
	let lock = skill::lock::global::read_skill_lock();
	lock.skills
		.into_iter()
		.map(|(name, entry)| EntryInput {
			local_hash: local_hashes.get(&name).cloned(),
			name,
			scope: "global".to_string(),
			source_ref: SourceRef {
				source: entry.source_url,
				ref_: entry.ref_name,
			},
			source_type: entry.source_type,
			skill_path: entry.skill_path,
			stored_hash: entry.content_hash,
			ref_commit: entry.ref_commit,
		})
		.collect()
}

fn project_lock_entries(
	project_root: Option<&Path>,
	local_hashes: &HashMap<String, String>,
) -> Vec<EntryInput> {
	let lock = skill::lock::local::read_local_lock(project_root);
	lock.skills
		.into_iter()
		.map(|(name, entry)| EntryInput {
			local_hash: local_hashes.get(&name).cloned(),
			name,
			scope: "project".to_string(),
			source_ref: SourceRef {
				// Prefer the recorded clone URL so a non-github host (TFS/Azure
				// DevOps) is fetched correctly; github/legacy fall back to the
				// host-stripped owner/repo.
				source: entry.source_url.unwrap_or(entry.source),
				ref_: entry.ref_name,
			},
			source_type: entry.source_type,
			skill_path: entry.skill_path,
			stored_hash: Some(entry.computed_hash),
			ref_commit: entry.ref_commit,
		})
		.collect()
}

fn lock_entries_for_scope(
	scope: &ResolvedScope,
	offline: bool,
) -> Result<(Vec<EntryInput>, Option<PathBuf>), ApiError> {
	let mut entries = Vec::new();
	match scope {
		ResolvedScope::Global => {
			let local = if offline {
				HashMap::new()
			} else {
				local_hashes_for_scope(ResourceScope::GlobalOnly, None)
			};
			entries.extend(global_lock_entries(&local));
			Ok((entries, None))
		}
		ResolvedScope::Project { root } => {
			let local = if offline {
				HashMap::new()
			} else {
				local_hashes_for_scope(ResourceScope::ProjectOnly, Some(root))
			};
			entries.extend(project_lock_entries(Some(root), &local));
			Ok((entries, Some(root.clone())))
		}
		ResolvedScope::All { project_root } => {
			let global = if offline {
				HashMap::new()
			} else {
				local_hashes_for_scope(ResourceScope::GlobalOnly, None)
			};
			entries.extend(global_lock_entries(&global));
			if let Some(root) = project_root {
				let local = if offline {
					HashMap::new()
				} else {
					local_hashes_for_scope(
						ResourceScope::ProjectOnly,
						Some(root),
					)
				};
				entries.extend(project_lock_entries(Some(root), &local));
			}
			Ok((entries, project_root.clone()))
		}
	}
}

fn write_auto_healed_hashes(
	outputs: &[CheckOutput],
	project_root: Option<&Path>,
) -> Result<(), ApiError> {
	let mut global_heals = HashMap::new();
	let mut global_oid_heals = HashMap::new();
	let mut project_heals = HashMap::new();
	for output in outputs {
		if let Some(hash) = &output.heal_hash {
			match output.key.scope.as_str() {
				"global" => {
					global_heals.insert(output.key.name.clone(), hash.clone());
				}
				"project" => {
					project_heals.insert(output.key.name.clone(), hash.clone());
				}
				_ => {}
			}
		}
		// refCommit heal is GLOBAL-only (the project lock is VCS-tracked) and
		// independent of heal_hash (a known-stored entry has no heal_hash).
		if output.key.scope == "global" {
			if let Some(oid) = &output.heal_oid {
				global_oid_heals.insert(output.key.name.clone(), oid.clone());
			}
		}
	}

	if !global_heals.is_empty() || !global_oid_heals.is_empty() {
		skill::lock::global::modify_skill_lock_changed(|lock| {
			let now = Utc::now().to_rfc3339();
			let mut changed = false;
			for (name, hash) in &global_heals {
				if let Some(entry) = lock.skills.get_mut(name) {
					changed |= entry.apply_content_hash(hash, &now);
				}
			}
			for (name, oid) in &global_oid_heals {
				if let Some(entry) = lock.skills.get_mut(name) {
					if entry.ref_commit.as_deref() != Some(oid.as_str()) {
						entry.ref_commit = Some(oid.clone());
						entry.updated_at = now.clone();
						changed = true;
					}
				}
			}
			((), changed)
		})
		.map_err(|e| {
			ApiError::new(
				Status::InternalServerError,
				format!("Failed to auto-heal global skill lock: {e}"),
				"SKILL_LOCK_ERROR",
			)
		})?;
	}

	if !project_heals.is_empty() {
		let root = project_root.ok_or_else(|| {
			ApiError::new(
				Status::BadRequest,
				"project_root is required to auto-heal project skill lock",
				"MISSING_PARAM",
			)
		})?;
		skill::lock::local::modify_local_lock_changed(Some(root), |lock| {
			let mut changed = false;
			for (name, hash) in &project_heals {
				if let Some(entry) = lock.skills.get_mut(name) {
					changed |= entry.apply_computed_hash(hash);
				}
			}
			((), changed)
		})
		.map_err(|e| {
			ApiError::new(
				Status::InternalServerError,
				format!("Failed to auto-heal project skill lock: {e}"),
				"SKILL_LOCK_ERROR",
			)
		})?;
	}

	Ok(())
}

struct ApplySource {
	source: String,
	ref_name: Option<String>,
	skill_path: String,
}

fn apply_source_from_lock(
	name: &str,
	scope: &str,
	project_root: Option<&Path>,
) -> Result<ApplySource, String> {
	match scope {
		"global" => {
			let lock = skill::lock::global::read_skill_lock();
			let Some(entry) = lock.skills.get(name) else {
				return Err("Skill is not in global lock".to_string());
			};
			let Some(skill_path) = entry.skill_path.clone() else {
				return Err("Locked skill has no skillPath".to_string());
			};
			Ok(ApplySource {
				source: entry.source_url.clone(),
				ref_name: entry.ref_name.clone(),
				skill_path,
			})
		}
		"project" => {
			let Some(root) = project_root else {
				return Err("project_root is required when scope is project"
					.to_string());
			};
			let lock = skill::lock::local::read_local_lock(Some(root));
			let Some(entry) = lock.skills.get(name) else {
				return Err("Skill is not in project lock".to_string());
			};
			let Some(skill_path) = entry.skill_path.clone() else {
				return Err("Locked skill has no skillPath".to_string());
			};
			Ok(ApplySource {
				// Fetch coordinate: prefer the recorded clone URL (non-github
				// host survives); github/legacy fall back to owner/repo.
				source: entry
					.source_url
					.clone()
					.unwrap_or_else(|| entry.source.clone()),
				ref_name: entry.ref_name.clone(),
				skill_path,
			})
		}
		_ => Err("scope must be global or project".to_string()),
	}
}

fn apply_error(
	name: &str,
	scope: &str,
	message: &str,
) -> ApplySkillUpdateResponse {
	apply_error_with_code(name, scope, message, None)
}

fn apply_error_with_code(
	name: &str,
	scope: &str,
	message: &str,
	code: Option<&'static str>,
) -> ApplySkillUpdateResponse {
	ApplySkillUpdateResponse {
		success: false,
		name: name.to_string(),
		scope: scope.to_string(),
		updated_hash: None,
		paths: Vec::new(),
		error: Some(message.to_string()),
		code: code.map(str::to_string),
	}
}

fn fetch_error_text(error: FetchError) -> &'static str {
	match error {
		FetchError::Auth => "Authentication failed while fetching source",
		FetchError::Network => "Failed to fetch source repository",
	}
}

/// `GET /skills/check-updates` — returns a per-skill update status list.
#[get("/skills/check-updates?<query..>")]
pub async fn check_skill_updates(
	query: CheckUpdatesParams,
	forwarded: ForwardedGitTokens,
	_origin: TrustedLocalOrigin,
) -> ApiResult<Vec<SkillUpdateResponse>> {
	let resolved = ScopeParams {
		scope: query.scope.clone(),
		project_root: query.project_root.clone(),
	}
	.resolve()?;
	let offline = query.offline.unwrap_or(false);
	let (entries, project_root) = lock_entries_for_scope(&resolved, offline)?;

	// System-git fallback for OS-credential-helper-only TFS/Azure DevOps repos
	// (forwarded/keyring token still wins — see GitFetcherWithFallback).
	let fetcher: Arc<dyn Fetcher> = Arc::new(GitFetcherWithFallback);
	// Forwarded tokens (header) take precedence over the local keyring; an
	// absent/empty header degrades to the keyring path (backward compatible).
	let keyring = KeyringResolver;
	let resolver = ChainResolver::new(forwarded.into_resolver(), &keyring);
	let mut cache = ResultCache::new(CACHE_TTL);
	let deps = CheckDeps {
		fetcher,
		ref_resolver: Some(Arc::new(GitRefResolver)),
		resolver: &resolver,
		cache: &mut cache,
		per_fetch: PER_FETCH,
		concurrency: CONCURRENCY,
		offline,
		overall_deadline: OVERALL_DEADLINE,
	};

	let outputs = check_updates(entries, deps).await;
	write_auto_healed_hashes(&outputs, project_root.as_deref())?;

	let mut out: Vec<SkillUpdateResponse> = outputs
		.into_iter()
		.map(|output| SkillUpdateResponse {
			name: output.key.name,
			scope: output.key.scope,
			status: SkillUpdateStatusResponse::from(output.status),
		})
		.collect();
	out.sort_by(|a, b| a.scope.cmp(&b.scope).then(a.name.cmp(&b.name)));

	Ok(Json(out))
}

/// `POST /skills/apply-update` — re-fetch a locked skill and replace installs.
#[post("/skills/apply-update", data = "<body>")]
pub async fn apply_skill_update(
	body: Json<ApplySkillUpdateRequest>,
	forwarded: ForwardedGitTokens,
	_origin: TrustedLocalOrigin,
) -> ApiResult<ApplySkillUpdateResponse> {
	// Forwarded tokens (header) take precedence over the local keyring; an
	// absent/empty header degrades to the keyring path (backward compatible).
	let keyring = KeyringResolver;
	let resolver = ChainResolver::new(forwarded.into_resolver(), &keyring);
	apply_skill_update_inner(
		body.into_inner(),
		&GitFetcherWithFallback,
		&resolver,
	)
	.await
}

/// Inner apply path that takes an injected [`Fetcher`] + [`TokenResolver`] so
/// the rename guard (and the rest of the happy-path wiring) is unit-testable
/// without a real network. The route handler is a thin shim that supplies
/// [`GitFetcher`] + the forwarded/keyring [`ChainResolver`].
pub(crate) async fn apply_skill_update_inner(
	req: ApplySkillUpdateRequest,
	fetcher: &dyn Fetcher,
	resolver: &dyn TokenResolver,
) -> ApiResult<ApplySkillUpdateResponse> {
	if !req.confirm.unwrap_or(false) {
		return Ok(Json(apply_error(
			&req.name,
			&req.scope,
			"confirm=true is required to overwrite installed skill files",
		)));
	}

	let project_root = req.project_root.as_deref().map(PathBuf::from);
	let resource_scope = match req.scope.as_str() {
		"global" => ResourceScope::GlobalOnly,
		"project" => ResourceScope::ProjectOnly,
		_ => {
			return Ok(Json(apply_error(
				&req.name,
				&req.scope,
				"scope must be global or project",
			)));
		}
	};
	if resource_scope == ResourceScope::ProjectOnly && project_root.is_none() {
		return Ok(Json(apply_error(
			&req.name,
			&req.scope,
			"project_root is required when scope is project",
		)));
	}

	let source = match apply_source_from_lock(
		&req.name,
		&req.scope,
		project_root.as_deref(),
	) {
		Ok(source) => source,
		Err(error) => {
			return Ok(Json(apply_error(&req.name, &req.scope, &error)));
		}
	};
	let targets = skill_update::installed_skill_roots(
		&req.name,
		resource_scope,
		project_root.as_deref(),
	);
	if targets.is_empty() {
		return Ok(Json(apply_error(
			&req.name,
			&req.scope,
			"Skill is locked but no installed copy was found",
		)));
	}

	let token = resolver.resolve(
		&source.source,
		keychain_host_for_source(&source.source).as_deref(),
	);
	let repo = match fetcher.fetch(
		&SourceRef {
			source: source.source.clone(),
			ref_: source.ref_name.clone(),
		},
		token.as_deref(),
	) {
		Ok(repo) => repo,
		Err(error) => {
			return Ok(Json(apply_error(
				&req.name,
				&req.scope,
				fetch_error_text(error),
			)));
		}
	};

	let Some(skill_file) = aghub_core::skills::update::sanitize_skill_path(
		&repo.root,
		&source.skill_path,
	) else {
		return Ok(Json(apply_error(
			&req.name,
			&req.scope,
			"Locked skillPath was not found in fetched source",
		)));
	};
	let source_dir = skill_file.parent().unwrap_or(&repo.root);

	use aghub_core::skills::resync::{
		resync_installed_skill, ResyncError, ResyncRequest,
	};
	match resync_installed_skill(ResyncRequest {
		source_dir,
		name: &req.name,
		scope: resource_scope,
		project_root: project_root.as_deref(),
		ref_commit: Some(&repo.oid),
	}) {
		Ok(report) => Ok(Json(ApplySkillUpdateResponse {
			success: true,
			name: req.name,
			scope: req.scope,
			updated_hash: Some(report.updated_hash),
			paths: report
				.swapped
				.iter()
				.map(|p| p.display().to_string())
				.collect(),
			error: None,
			code: None,
		})),
		Err(ResyncError::Renamed { new_name }) => {
			Ok(Json(apply_error_with_code(
				&req.name,
				&req.scope,
				&skill_renamed_message(&req.name, &new_name),
				Some(SKILL_RENAMED_CODE),
			)))
		}
		Err(e) => Ok(Json(apply_error(&req.name, &req.scope, &e.to_string()))),
	}
}

fn accept_rename_error(
	old_name: &str,
	new_name: &str,
	scope: &str,
	message: &str,
) -> AcceptRenameResponse {
	accept_rename_error_with_code(old_name, new_name, scope, message, None)
}

fn accept_rename_error_with_code(
	old_name: &str,
	new_name: &str,
	scope: &str,
	message: &str,
	code: Option<&'static str>,
) -> AcceptRenameResponse {
	AcceptRenameResponse {
		success: false,
		old_name: old_name.to_string(),
		new_name: new_name.to_string(),
		scope: scope.to_string(),
		installed_hash: None,
		paths: Vec::new(),
		error: Some(message.to_string()),
		code: code.map(str::to_string),
	}
}

/// Source coordinates for the OLD-name lock entry, including the fields needed
/// to re-install under the new name (`source_type` / `source_url` are not
/// surfaced by [`apply_source_from_lock`]).
struct RenameLockSource {
	source: String,
	source_type: String,
	source_url: String,
	ref_name: Option<String>,
	skill_path: String,
}

fn rename_source_from_lock(
	name: &str,
	scope: &str,
	project_root: Option<&Path>,
) -> Result<RenameLockSource, String> {
	match scope {
		"global" => {
			let lock = skill::lock::global::read_skill_lock();
			let Some(entry) = lock.skills.get(name) else {
				return Err("Skill is not in global lock".to_string());
			};
			let Some(skill_path) = entry.skill_path.clone() else {
				return Err("Locked skill has no skillPath".to_string());
			};
			Ok(RenameLockSource {
				source: entry.source.clone(),
				source_type: entry.source_type.clone(),
				source_url: entry.source_url.clone(),
				ref_name: entry.ref_name.clone(),
				skill_path,
			})
		}
		"project" => {
			let Some(root) = project_root else {
				return Err("project_root is required when scope is project"
					.to_string());
			};
			let lock = skill::lock::local::read_local_lock(Some(root));
			let Some(entry) = lock.skills.get(name) else {
				return Err("Skill is not in project lock".to_string());
			};
			let Some(skill_path) = entry.skill_path.clone() else {
				return Err("Locked skill has no skillPath".to_string());
			};
			Ok(RenameLockSource {
				source: entry.source.clone(),
				source_type: entry.source_type.clone(),
				// Prefer the recorded clone URL so a non-github host is fetched
				// correctly; fall back to `source` for github/legacy locks.
				source_url: entry
					.source_url
					.clone()
					.unwrap_or_else(|| entry.source.clone()),
				ref_name: entry.ref_name.clone(),
				skill_path,
			})
		}
		_ => Err("scope must be global or project".to_string()),
	}
}

/// Machine code for a rename target that already exists (lock entry or on-disk
/// dir) in the scope, or a degenerate rename (sanitizes to the same name).
pub(crate) const RENAME_TARGET_EXISTS_CODE: &str = "RENAME_TARGET_EXISTS";

/// Whether `new_name` already has a lock entry OR an on-disk skill dir in the
/// target scope's agent dirs / universal master. Used to refuse clobbering a
/// pre-existing skill, which would make the "remove all new_name paths"
/// rollback delete data that this transaction did not create.
fn new_name_exists_in_scope(
	new_name: &str,
	scope: ResourceScope,
	project_root: Option<&Path>,
	agent_dirs: &[PathBuf],
) -> bool {
	// Lock entry under the new name.
	let in_lock = match scope {
		ResourceScope::GlobalOnly => {
			skill::lock::global::get_skill_from_lock(new_name).is_some()
		}
		ResourceScope::ProjectOnly => project_root.is_some_and(|root| {
			skill::lock::local::read_local_lock(Some(root))
				.skills
				.contains_key(new_name)
		}),
		ResourceScope::Both => false,
	};
	if in_lock {
		return true;
	}
	// On-disk dir/link in any in-scope agent dir or the universal master.
	let safe = skill::sanitize::sanitize_name(new_name);
	let mut targets: Vec<PathBuf> =
		agent_dirs.iter().map(|d| d.join(&safe)).collect();
	let canonical_root = if matches!(scope, ResourceScope::ProjectOnly) {
		project_root
	} else {
		None
	};
	if let Some(master) =
		aghub_core::skills::linker::universal_canonical_dir(canonical_root)
	{
		targets.push(master.join(&safe));
	}
	targets.iter().any(|p| std::fs::symlink_metadata(p).is_ok())
}

/// Remove a skill entry from the appropriate scope's lock. The closure is
/// NON-fallible (it returns `()`); a `modify_*` no-op (entry already absent)
/// does not rewrite the file, so this only errors on a real I/O failure.
fn remove_lock_entry(
	name: &str,
	scope: &str,
	project_root: Option<&Path>,
) -> Result<(), String> {
	match scope {
		"global" => skill::lock::global::modify_skill_lock(|lock| {
			lock.skills.remove(name);
		})
		.map_err(|e| format!("global lock write failed: {e}")),
		"project" => {
			let root = project_root.ok_or_else(|| {
				"project_root required for project scope".to_string()
			})?;
			skill::lock::local::modify_local_lock(Some(root), |lock| {
				lock.skills.remove(name);
			})
			.map_err(|e| format!("project lock write failed: {e}"))
		}
		_ => Err("scope must be global or project".to_string()),
	}
}

/// Restore a previously-removed lock entry (rollback). Re-inserts the cloned
/// entry under `name` in the appropriate scope's lock.
fn restore_lock_entry(
	name: &str,
	scope: &str,
	project_root: Option<&Path>,
	global_entry: Option<&skill::SkillLockEntry>,
	local_entry: Option<&skill::LocalSkillLockEntry>,
) -> Result<(), String> {
	match scope {
		"global" => {
			let Some(entry) = global_entry else {
				return Ok(());
			};
			let entry = entry.clone();
			let name = name.to_string();
			skill::lock::global::modify_skill_lock(move |lock| {
				lock.skills.insert(name, entry);
			})
			.map_err(|e| format!("global lock restore failed: {e}"))
		}
		"project" => {
			let root = project_root.ok_or_else(|| {
				"project_root required for project scope".to_string()
			})?;
			let Some(entry) = local_entry else {
				return Ok(());
			};
			let entry = entry.clone();
			let name = name.to_string();
			skill::lock::local::modify_local_lock(Some(root), move |lock| {
				lock.skills.insert(name, entry);
			})
			.map_err(|e| format!("project lock restore failed: {e}"))
		}
		_ => Err("scope must be global or project".to_string()),
	}
}

/// A filesystem snapshot of one skill name across the in-scope agent dirs +
/// the universal master, recursively copied into a temp backup so a failed
/// rename transaction can be rolled back to its pre-mutation state.
struct SkillSnapshot {
	/// `_tmp` owns the backup tree; dropping it deletes the backup.
	_tmp: tempfile::TempDir,
	/// `(live_path, backup_path)` pairs for every captured location.
	entries: Vec<(PathBuf, PathBuf)>,
}

/// Capture the old-name skill across the in-scope agent dirs + the universal
/// master into a temp backup. Symlinks are preserved as symlinks; real dirs are
/// deep-copied.
///
/// This MUST run BEFORE any mutation. A failure to create the backup tempdir, or
/// to copy/readlink an EXISTING old target, aborts the whole operation (returns
/// `Err`) so a backup failure can never become permanent old-skill loss when a
/// later step fails. Paths that genuinely do not exist are skipped (there is
/// nothing to back up).
fn snapshot_old_skill(
	name: &str,
	scope: ResourceScope,
	project_root: Option<&Path>,
	agent_dirs: &[PathBuf],
) -> Result<SkillSnapshot, String> {
	let safe = skill::sanitize::sanitize_name(name);
	let tmp = tempfile::tempdir()
		.map_err(|e| format!("Failed to create snapshot backup dir: {e}"))?;
	let mut entries: Vec<(PathBuf, PathBuf)> = Vec::new();
	let mut captured = std::collections::HashSet::new();

	let mut targets: Vec<PathBuf> =
		agent_dirs.iter().map(|d| d.join(&safe)).collect();
	let canonical_root = if matches!(scope, ResourceScope::ProjectOnly) {
		project_root
	} else {
		None
	};
	if let Some(master) =
		aghub_core::skills::linker::universal_canonical_dir(canonical_root)
	{
		targets.push(master.join(&safe));
	}

	for (idx, live) in targets.into_iter().enumerate() {
		if !captured.insert(live.clone()) {
			continue;
		}
		// A genuinely-absent path has nothing to back up; only an EXISTING
		// target that fails to copy/readlink aborts the transaction.
		let Ok(meta) = std::fs::symlink_metadata(&live) else {
			continue;
		};
		let backup = tmp.path().join(format!("snap-{idx}"));
		// A reparse point (Unix symlink OR Windows symlink/junction) is captured
		// by recording its target and re-creating it as a link — NEVER
		// deep-copied as a real directory. `Linker::is_link` covers junctions
		// (FILE_ATTRIBUTE_REPARSE_POINT), which bare `is_symlink()` may miss.
		let result = if aghub_core::skills::linker::Linker::is_link(&live) {
			std::fs::read_link(&live).and_then(|target| {
				aghub_core::skills::linker::Linker::symlink(&target, &backup)
			})
		} else if meta.is_dir() {
			aghub_core::skills::linker::Linker::copy_preserving_links(
				&live, &backup,
			)
		} else {
			std::fs::copy(&live, &backup).map(|_| ())
		};
		result.map_err(|e| {
			format!("Failed to snapshot old skill before rename: {e}")
		})?;
		entries.push((live, backup));
	}

	Ok(SkillSnapshot { _tmp: tmp, entries })
}

/// Restore every captured location from a snapshot (best-effort rollback).
fn restore_snapshot(snapshot: &SkillSnapshot) {
	use aghub_core::skills::linker::Linker;
	for (live, backup) in &snapshot.entries {
		// Clear whatever (partial) state is at `live` before restoring. A
		// reparse point (Unix symlink OR Windows symlink/junction) is unlinked
		// with `Linker::unlink` (Windows `remove_dir`, junction-safe), NEVER
		// `remove_dir_all` — recursing into a junction would delete the Master.
		if Linker::is_link(live) {
			let _ = Linker::unlink(live);
		} else if let Ok(meta) = std::fs::symlink_metadata(live) {
			if meta.is_file() {
				let _ = std::fs::remove_file(live);
			} else if meta.is_dir() {
				let _ = std::fs::remove_dir_all(live);
			}
		}
		let Ok(meta) = std::fs::symlink_metadata(backup) else {
			continue;
		};
		let _ = if Linker::is_link(backup) {
			std::fs::read_link(backup)
				.and_then(|target| Linker::symlink(&target, live))
		} else if meta.is_dir() {
			Linker::copy_preserving_links(backup, live)
		} else {
			std::fs::copy(backup, live).map(|_| ())
		};
	}
}

/// Best-effort rollback of the just-installed new-name dirs (and the universal
/// master if it was freshly created), re-asserting containment before each
/// `remove_dir_all` (TOCTOU guard).
fn rollback_rename_install(
	new_name: &str,
	scope: ResourceScope,
	project_root: Option<&Path>,
	agent_dirs: &[PathBuf],
) {
	let safe = skill::sanitize::sanitize_name(new_name);
	let roots = aghub_core::skills::removal::allowed_skill_roots(
		agent_dirs,
		project_root,
	);
	for dir in agent_dirs {
		let target = dir.join(&safe);
		// A reparse point (Unix symlink OR Windows symlink/junction) is unlinked
		// directly with `Linker::unlink` (Windows `remove_dir`, junction-safe) —
		// NEVER `remove_dir_all`, which would recurse into a junction's Master. A
		// real dir is removed only if contained.
		if aghub_core::skills::linker::Linker::is_link(&target) {
			let _ = aghub_core::skills::linker::Linker::unlink(&target);
		} else if let Ok(meta) = std::fs::symlink_metadata(&target) {
			if meta.is_dir()
				&& aghub_core::skills::removal::assert_contained(
					&target, &roots,
				)
				.is_some()
			{
				let _ = std::fs::remove_dir_all(&target);
			} else if meta.is_file() {
				let _ = std::fs::remove_file(&target);
			}
		}
	}
	let canonical_root = if matches!(scope, ResourceScope::ProjectOnly) {
		project_root
	} else {
		None
	};
	if let Some(canonical_dir) =
		aghub_core::skills::linker::universal_canonical_dir(canonical_root)
	{
		let canonical = canonical_dir.join(&safe);
		if canonical.exists()
			&& aghub_core::skills::removal::assert_contained(&canonical, &roots)
				.is_some()
		{
			let _ = std::fs::remove_dir_all(&canonical);
		}
	}
}

/// `POST /skills/accept-rename` — atomic rename: install the new name, delete
/// the old name, update both lock entries. A single transaction: any failure
/// after the new-name install rolls the install back and restores the old name
/// (dirs + lock) to its pre-transaction state.
#[post("/skills/accept-rename", data = "<body>")]
pub async fn accept_skill_rename(
	body: Json<AcceptRenameRequest>,
	forwarded: ForwardedGitTokens,
	_origin: TrustedLocalOrigin,
) -> ApiResult<AcceptRenameResponse> {
	// Same credential path as apply-update: forwarded tokens (header) take
	// precedence over the local keyring; an absent header degrades to keyring.
	let keyring = KeyringResolver;
	let resolver = ChainResolver::new(forwarded.into_resolver(), &keyring);
	accept_rename_inner(body.into_inner(), &GitFetcherWithFallback, &resolver)
		.await
}

pub(crate) async fn accept_rename_inner(
	req: AcceptRenameRequest,
	fetcher: &dyn Fetcher,
	resolver: &dyn TokenResolver,
) -> ApiResult<AcceptRenameResponse> {
	if !req.confirm.unwrap_or(false) {
		return Ok(Json(accept_rename_error(
			&req.old_name,
			&req.new_name,
			&req.scope,
			"confirm=true is required to accept a skill rename",
		)));
	}
	let project_root = req.project_root.as_deref().map(PathBuf::from);
	let resource_scope = match req.scope.as_str() {
		"global" => ResourceScope::GlobalOnly,
		"project" => ResourceScope::ProjectOnly,
		_ => {
			return Ok(Json(accept_rename_error(
				&req.old_name,
				&req.new_name,
				&req.scope,
				"scope must be global or project",
			)));
		}
	};
	if resource_scope == ResourceScope::ProjectOnly && project_root.is_none() {
		return Ok(Json(accept_rename_error(
			&req.old_name,
			&req.new_name,
			&req.scope,
			"project_root is required when scope is project",
		)));
	}

	// P0-2 guard (a): a degenerate rename whose names sanitize to the same
	// on-disk dir would have the install write the very dir the removal then
	// deletes. Refuse before any fetch/mutation.
	if skill::sanitize::sanitize_name(&req.old_name)
		== skill::sanitize::sanitize_name(&req.new_name)
	{
		return Ok(Json(accept_rename_error_with_code(
			&req.old_name,
			&req.new_name,
			&req.scope,
			"old_name and new_name resolve to the same on-disk skill \
			 directory; choose a distinct rename target",
			Some(RENAME_TARGET_EXISTS_CODE),
		)));
	}

	// 1. Read the OLD-name lock entry for source coordinates.
	let source = match rename_source_from_lock(
		&req.old_name,
		&req.scope,
		project_root.as_deref(),
	) {
		Ok(s) => s,
		Err(e) => {
			return Ok(Json(accept_rename_error(
				&req.old_name,
				&req.new_name,
				&req.scope,
				&e,
			)));
		}
	};

	// 2. Target agents = those that ACTUALLY have the old name installed (never
	//    `AgentType::ALL`, which would spread the skill to agents that never
	//    had it). Mirrors apply-update only touching installed roots.
	let target_agents: Vec<aghub_core::models::AgentType> =
		aghub_core::load_all_agents(resource_scope, project_root.as_deref())
			.into_iter()
			.filter(|r| r.skills.iter().any(|s| s.name == req.old_name))
			.filter_map(|r| r.agent_id.parse().ok())
			.collect();
	if target_agents.is_empty() {
		return Ok(Json(accept_rename_error(
			&req.old_name,
			&req.new_name,
			&req.scope,
			"Skill is locked but no installed copy was found",
		)));
	}

	// 3. Fetch upstream (same credential path as apply-update). Resolve the
	// token against the fetch coordinate (`source_url`) so a non-github host
	// (TFS/Azure DevOps) binds to the right keychain host — resolving against
	// the host-stripped `source` would yield host `None` and silently miss a
	// credential bound to the real host.
	let token = resolver.resolve(
		&source.source_url,
		keychain_host_for_source(&source.source_url).as_deref(),
	);
	let repo = match fetcher.fetch(
		&SourceRef {
			source: source.source_url.clone(),
			ref_: source.ref_name.clone(),
		},
		token.as_deref(),
	) {
		Ok(r) => r,
		Err(e) => {
			return Ok(Json(accept_rename_error(
				&req.old_name,
				&req.new_name,
				&req.scope,
				fetch_error_text(e),
			)));
		}
	};

	// 4. Locate the skill file in the fetched tree (containment check).
	let Some(skill_file) = aghub_core::skills::update::sanitize_skill_path(
		&repo.root,
		&source.skill_path,
	) else {
		return Ok(Json(accept_rename_error(
			&req.old_name,
			&req.new_name,
			&req.scope,
			"Locked skillPath was not found in fetched source",
		)));
	};

	// 5. Verify the fetched name matches new_name (confirms this rename).
	let parsed_skill = match skill::parse(&skill_file) {
		Ok(s) => s,
		Err(e) => {
			return Ok(Json(accept_rename_error(
				&req.old_name,
				&req.new_name,
				&req.scope,
				&format!("Failed to parse fetched skill: {e}"),
			)));
		}
	};
	if parsed_skill.name != req.new_name {
		return Ok(Json(accept_rename_error(
			&req.old_name,
			&req.new_name,
			&req.scope,
			&format!(
				"Fetched SKILL.md declares name '{}', expected '{}'. \
				 Verify the new_name matches the upstream source.",
				parsed_skill.name, req.new_name,
			),
		)));
	}

	let agent_dirs = aghub_core::skills::removal::agent_skill_dirs_in_scope(
		resource_scope,
		project_root.as_deref(),
	);

	// P0-2 guard (b): refuse if the new name ALREADY exists (lock entry or
	// on-disk dir) in this scope. The rollback/cleanup deletes EVERY new_name
	// path; if new_name pre-existed, that would destroy data this transaction
	// did not create. Requiring new_name to be absent makes the cleanup safe.
	if new_name_exists_in_scope(
		&req.new_name,
		resource_scope,
		project_root.as_deref(),
		&agent_dirs,
	) {
		return Ok(Json(accept_rename_error_with_code(
			&req.old_name,
			&req.new_name,
			&req.scope,
			&format!(
				"A skill named '{}' already exists in this scope (lock entry \
				 or on-disk directory); pick a rename target that does not \
				 already exist",
				req.new_name
			),
			Some(RENAME_TARGET_EXISTS_CODE),
		)));
	}

	// 6. SNAPSHOT the old-name dirs + clone the old lock entry BEFORE mutating.
	//    A snapshot failure (P0-3) aborts BEFORE install — nothing mutated.
	let snapshot = match snapshot_old_skill(
		&req.old_name,
		resource_scope,
		project_root.as_deref(),
		&agent_dirs,
	) {
		Ok(s) => s,
		Err(e) => {
			return Ok(Json(accept_rename_error(
				&req.old_name,
				&req.new_name,
				&req.scope,
				&e,
			)));
		}
	};
	let old_global_entry: Option<skill::SkillLockEntry> =
		if req.scope == "global" {
			skill::lock::global::read_skill_lock()
				.skills
				.get(&req.old_name)
				.cloned()
		} else {
			None
		};
	let old_local_entry: Option<skill::LocalSkillLockEntry> =
		if req.scope == "project" {
			project_root.as_deref().and_then(|root| {
				skill::lock::local::read_local_lock(Some(root))
					.skills
					.get(&req.old_name)
					.cloned()
			})
		} else {
			None
		};

	// Helper: roll the WHOLE transaction back to its pre-mutation state. Defined
	// BEFORE install so every post-snapshot failure path (P0-1: including the
	// install Err / no-agent arms) runs the SAME rollback — remove the freshly
	// created new_name dirs/master + new lock entry, restore old dirs from the
	// snapshot, restore the old lock entry. The new_name-clobber guard above
	// guarantees new_name did not pre-exist, so removing all new_name paths is
	// safe.
	let rollback_all = || {
		rollback_rename_install(
			&req.new_name,
			resource_scope,
			project_root.as_deref(),
			&agent_dirs,
		);
		let _ = remove_lock_entry(
			&req.new_name,
			&req.scope,
			project_root.as_deref(),
		);
		restore_snapshot(&snapshot);
		let _ = restore_lock_entry(
			&req.old_name,
			&req.scope,
			project_root.as_deref(),
			old_global_entry.as_ref(),
			old_local_entry.as_ref(),
		);
	};

	// 7. Install the new-named skill. A failure AFTER this point rolls back via
	//    `rollback_all` (the install itself may have written the master/link
	//    before the lock-write step failed — P0-1).
	let install_source = skill::InstallLockSource {
		source: source.source.clone(),
		source_type: source.source_type.clone(),
		source_url: source.source_url.clone(),
		ref_name: source.ref_name.clone(),
	};
	let install_req =
		aghub_core::skills::install_fetched::FetchedSkillInstallRequest {
			skill_file: &skill_file,
			source: &install_source,
			lock_skill_path: source.skill_path.clone(),
			ref_commit: Some(repo.oid.clone()),
			scope: resource_scope,
			project_root: project_root.as_deref(),
			target_agents: &target_agents,
			expected_name: Some(&req.new_name),
			target: if matches!(resource_scope, ResourceScope::ProjectOnly) {
				aghub_core::skills::linker::LinkTarget::Relative
			} else {
				aghub_core::skills::linker::LinkTarget::Absolute
			},
		};
	let install_report =
		match aghub_core::skills::install_fetched::install_fetched_skill_and_lock(
			install_req,
		) {
			Ok(r) => r,
			Err(e) => {
				// P0-1: install_fetched writes the master/link BEFORE the lock,
				// so an Err here may have left a half-installed new_name. Run the
				// full rollback (cleanup new_name + restore old) before bailing.
				rollback_all();
				return Ok(Json(accept_rename_error(
					&req.old_name,
					&req.new_name,
					&req.scope,
					&format!("Failed to install renamed skill: {e}"),
				)));
			}
		};
	// If no agent actually received the skill, treat as a failed install: run
	// the full rollback and bail (old skill restored to its pre-txn state).
	if !install_report.agent_results.iter().any(|r| r.installed) {
		let detail = install_report
			.agent_results
			.iter()
			.find_map(|r| r.error.clone())
			.unwrap_or_else(|| "no agent received the skill".to_string());
		rollback_all();
		return Ok(Json(accept_rename_error(
			&req.old_name,
			&req.new_name,
			&req.scope,
			&format!("Failed to install renamed skill: {detail}"),
		)));
	}

	let installed_paths: Vec<String> = install_report
		.agent_results
		.iter()
		.filter(|r| r.installed)
		.filter_map(|r| {
			aghub_core::create_adapter(r.agent)
				.get_skills_paths(project_root.as_deref(), resource_scope)
				.first()
				.map(|p| p.join(&req.new_name).display().to_string())
		})
		.collect();

	// 8. Remove the old-name dirs. A removal failure rolls back the whole txn.
	let mut old_skill = aghub_core::models::Skill::new(&req.old_name);
	if let Some(dir) = agent_dirs.first() {
		old_skill.source_path = Some(
			dir.join(&req.old_name)
				.join("SKILL.md")
				.display()
				.to_string(),
		);
	}
	let removal_plan = aghub_core::skills::removal::plan_removal(
		&old_skill,
		None,
		&agent_dirs,
		project_root.as_deref(),
		true,
	);
	let removal_roots = aghub_core::skills::removal::allowed_skill_roots(
		&agent_dirs,
		project_root.as_deref(),
	);

	let removal_report = match aghub_core::skills::removal::execute_removal(
		&removal_plan,
		&removal_roots,
	) {
		Ok(r) => r,
		Err(e) => {
			rollback_all();
			return Ok(Json(accept_rename_error(
				&req.old_name,
				&req.new_name,
				&req.scope,
				&format!("Failed to remove old skill '{}': {e}", req.old_name),
			)));
		}
	};
	if !removal_report.failed.is_empty() {
		let failed_msgs: Vec<String> = removal_report
			.failed
			.iter()
			.map(|(p, e)| format!("{}: {e}", p.display()))
			.collect();
		rollback_all();
		return Ok(Json(accept_rename_error(
			&req.old_name,
			&req.new_name,
			&req.scope,
			&format!(
				"Partial removal failure for old skill: {}",
				failed_msgs.join("; ")
			),
		)));
	}

	// 9. Remove the old-name lock entry. NOT log-and-continue: a failure here
	//    means the transaction did not fully commit -> roll everything back.
	if let Err(e) =
		remove_lock_entry(&req.old_name, &req.scope, project_root.as_deref())
	{
		rollback_all();
		return Ok(Json(accept_rename_error(
			&req.old_name,
			&req.new_name,
			&req.scope,
			&format!("Failed to remove old lock entry '{}': {e}", req.old_name),
		)));
	}

	Ok(Json(AcceptRenameResponse {
		success: true,
		old_name: req.old_name,
		new_name: req.new_name,
		scope: req.scope,
		installed_hash: Some(install_report.installed_hash),
		paths: installed_paths,
		error: None,
		code: None,
	}))
}

#[cfg(test)]
mod tests {
	use super::*;
	use aghub_core::skills::lock::update_lock_hash;
	use aghub_core::skills::update::SkillUpdateStatus;
	// GitFetcher (no fallback) is used only by the network E2E tests here; the
	// production paths use GitFetcherWithFallback, so import it test-locally to
	// avoid an unused-import warning in non-test builds.
	use skill_update::{EntryKey, GitFetcher};

	fn with_isolated_state<T>(f: impl FnOnce() -> T) -> T {
		let _guard = crate::routes::test_env_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		let state = tempfile::tempdir().unwrap();
		let old_xdg = std::env::var("XDG_STATE_HOME").ok();
		std::env::set_var("XDG_STATE_HOME", state.path());
		let result = f();
		match old_xdg {
			Some(value) => std::env::set_var("XDG_STATE_HOME", value),
			None => std::env::remove_var("XDG_STATE_HOME"),
		}
		result
	}

	fn global_entry() -> skill::SkillLockEntry {
		skill::SkillLockEntry {
			source: "owner/repo".to_string(),
			source_type: "github".to_string(),
			source_url: "https://github.com/owner/repo".to_string(),
			ref_name: Some("main".to_string()),
			skill_path: Some("SKILL.md".to_string()),
			skill_folder_hash: String::new(),
			content_hash: None,
			ref_commit: None,
			installed_at: "t".to_string(),
			updated_at: "t".to_string(),
			plugin_name: None,
		}
	}

	/// Run `accept_rename_inner` on a current-thread runtime and unwrap the
	/// JSON body, panicking on the (never-returned) `ApiError` path since
	/// `ApiError` does not implement `Debug`.
	#[cfg(unix)]
	fn run_accept_rename(
		req: crate::dto::skill::AcceptRenameRequest,
		fetcher: &dyn Fetcher,
	) -> crate::dto::skill::AcceptRenameResponse {
		let resolver = KeyringResolver;
		match rocket::tokio::runtime::Builder::new_current_thread()
			.enable_all()
			.build()
			.unwrap()
			.block_on(accept_rename_inner(req, fetcher, &resolver))
		{
			Ok(json) => json.into_inner(),
			Err(error) => {
				panic!("accept_rename should return Ok: {}", error.body.error)
			}
		}
	}

	fn healed_output(name: &str, scope: &str, hash: &str) -> CheckOutput {
		CheckOutput {
			key: EntryKey {
				name: name.to_string(),
				scope: scope.to_string(),
			},
			status: SkillUpdateStatus::UpToDate,
			heal_hash: Some(hash.to_string()),
			heal_oid: None,
		}
	}

	/// Offline short-circuits every entry without touching the network. With an
	/// empty lock the result is simply an empty list.
	#[tokio::test]
	async fn offline_check_returns_without_network() {
		let entries = vec![EntryInput {
			name: "skill-a".to_string(),
			scope: "global".to_string(),
			source_ref: SourceRef {
				source: "https://github.com/owner/repo".to_string(),
				ref_: None,
			},
			source_type: "github".to_string(),
			skill_path: Some("SKILL.md".to_string()),
			stored_hash: None,
			local_hash: None,
			ref_commit: None,
		}];
		let fetcher: Arc<dyn Fetcher> = Arc::new(GitFetcher);
		let resolver = KeyringResolver;
		let mut cache = ResultCache::new(CACHE_TTL);
		let deps = CheckDeps {
			ref_resolver: None,
			fetcher,
			resolver: &resolver,
			cache: &mut cache,
			per_fetch: PER_FETCH,
			concurrency: CONCURRENCY,
			offline: true,
			overall_deadline: OVERALL_DEADLINE,
		};
		let out = check_updates(entries, deps).await;
		assert_eq!(out.len(), 1);
		assert!(matches!(
			out[0].status,
			aghub_core::skills::update::SkillUpdateStatus::Uncheckable { .. }
		));
	}

	#[test]
	fn auto_heal_writes_global_content_hash() {
		with_isolated_state(|| {
			let mut lock = skill::SkillLockFile::default();
			let mut entry = global_entry();
			entry.skill_folder_hash = "tree-v1".to_string();
			lock.skills.insert("legacy".into(), entry);
			skill::lock::global::write_skill_lock(&lock).unwrap();

			assert!(write_auto_healed_hashes(
				&[healed_output("legacy", "global", "abc123")],
				None,
			)
			.is_ok());

			let lock = skill::lock::global::read_skill_lock();
			assert_eq!(
				lock.skills["legacy"].content_hash.as_deref(),
				Some("abc123")
			);
			assert_eq!(lock.skills["legacy"].skill_folder_hash, "");
		});
	}

	#[test]
	fn auto_heal_writes_global_ref_commit() {
		with_isolated_state(|| {
			let mut lock = skill::SkillLockFile::default();
			lock.skills.insert("legacy".into(), global_entry());
			skill::lock::global::write_skill_lock(&lock).unwrap();

			// A freshly-fetched global member carries heal_oid (and no heal_hash);
			// write_auto_healed_hashes must still persist refCommit.
			let mut output = healed_output("legacy", "global", "ignored");
			output.heal_hash = None;
			output.heal_oid = Some("deadbeefcafef00d".to_string());

			assert!(write_auto_healed_hashes(&[output], None).is_ok());

			let lock = skill::lock::global::read_skill_lock();
			assert_eq!(
				lock.skills["legacy"].ref_commit.as_deref(),
				Some("deadbeefcafef00d")
			);
		});
	}

	#[test]
	fn global_apply_update_hash_clears_npx_folder_hash() {
		with_isolated_state(|| {
			let mut lock = skill::SkillLockFile::default();
			let mut entry = global_entry();
			entry.skill_folder_hash = "tree-v1".to_string();
			lock.skills.insert("legacy".into(), entry);
			skill::lock::global::write_skill_lock(&lock).unwrap();

			update_lock_hash(
				"legacy",
				ResourceScope::GlobalOnly,
				None,
				"content-v2",
				None,
			)
			.unwrap();

			let lock = skill::lock::global::read_skill_lock();
			let entry = &lock.skills["legacy"];
			assert_eq!(entry.content_hash.as_deref(), Some("content-v2"));
			assert_eq!(entry.skill_folder_hash, "");
		});
	}

	#[test]
	fn renamed_message_tells_user_to_delete_and_install() {
		let message = skill_renamed_message("old-skill", "new-skill");

		assert!(message.contains("old-skill"));
		assert!(message.contains("new-skill"));
		assert!(message.contains("Delete the old skill"));
		assert!(message.contains("install 'new-skill'"));
	}

	#[test]
	fn project_lock_entries_reads_ref_commit() {
		let project = tempfile::tempdir().unwrap();
		let mut local = skill::LocalSkillLockFile::default();
		local.skills.insert(
			"s".into(),
			skill::LocalSkillLockEntry {
				source_url: None,
				source: "owner/repo".to_string(),
				ref_name: Some("main".to_string()),
				source_type: "github".to_string(),
				computed_hash: "h".to_string(),
				skill_path: Some("SKILL.md".to_string()),
				ref_commit: Some("deadbeefcafef00d".to_string()),
			},
		);
		skill::lock::local::write_local_lock(&local, Some(project.path()))
			.unwrap();

		let entries =
			project_lock_entries(Some(project.path()), &HashMap::new());
		assert_eq!(entries.len(), 1);
		assert_eq!(entries[0].ref_commit.as_deref(), Some("deadbeefcafef00d"));
	}

	#[test]
	fn apply_update_writes_global_ref_commit() {
		with_isolated_state(|| {
			let mut lock = skill::SkillLockFile::default();
			lock.skills.insert("legacy".into(), global_entry());
			skill::lock::global::write_skill_lock(&lock).unwrap();

			update_lock_hash(
				"legacy",
				ResourceScope::GlobalOnly,
				None,
				"content-v2",
				Some("deadbeefcafef00d"),
			)
			.unwrap();

			let lock = skill::lock::global::read_skill_lock();
			let entry = &lock.skills["legacy"];
			assert_eq!(entry.content_hash.as_deref(), Some("content-v2"));
			assert_eq!(entry.ref_commit.as_deref(), Some("deadbeefcafef00d"));
		});
	}

	#[test]
	fn update_lock_hash_none_clears_stale_ref_commit() {
		with_isolated_state(|| {
			let mut lock = skill::SkillLockFile::default();
			let mut entry = global_entry();
			entry.ref_commit = Some("staleoldoid".to_string());
			lock.skills.insert("legacy".into(), entry);
			skill::lock::global::write_skill_lock(&lock).unwrap();

			// A content rewrite with no resolvable OID must CLEAR the recorded
			// refCommit: preserving the old tip next to freshly-swapped content
			// would let a later ls-refs preflight falsely skip the fetch.
			update_lock_hash(
				"legacy",
				ResourceScope::GlobalOnly,
				None,
				"content-v2",
				None,
			)
			.unwrap();

			let lock = skill::lock::global::read_skill_lock();
			let entry = &lock.skills["legacy"];
			assert_eq!(entry.content_hash.as_deref(), Some("content-v2"));
			assert_eq!(entry.ref_commit, None);
		});
	}

	#[test]
	fn auto_heal_writes_project_computed_hash_only() {
		with_isolated_state(|| {
			let project = tempfile::tempdir().unwrap();
			let mut local = skill::LocalSkillLockFile::default();
			local.skills.insert(
				"legacy".into(),
				skill::LocalSkillLockEntry {
					source_url: None,
					ref_commit: None,
					source: "owner/repo".to_string(),
					ref_name: Some("main".to_string()),
					source_type: "github".to_string(),
					computed_hash: skill::EMPTY_SKILLS_LOCK_DIGEST.to_string(),
					skill_path: Some("SKILL.md".to_string()),
				},
			);
			skill::lock::local::write_local_lock(&local, Some(project.path()))
				.unwrap();

			assert!(write_auto_healed_hashes(
				&[healed_output("legacy", "project", "def456")],
				Some(project.path()),
			)
			.is_ok());

			let local =
				skill::lock::local::read_local_lock(Some(project.path()));
			assert_eq!(local.skills["legacy"].computed_hash, "def456");
			assert!(
				skill::lock::global::read_skill_lock().skills.is_empty(),
				"project auto-heal must not touch the global lock"
			);
		});
	}

	/// A public repo with no stored hash recomputes locally and never panics;
	/// the result is `UpToDate` or `UpdateAvailable` (never `Uncheckable`).
	#[ignore = "network"]
	#[tokio::test]
	async fn e2e_check_public_repo_no_crash() {
		let entries = vec![EntryInput {
			name: "public".to_string(),
			scope: "global".to_string(),
			source_ref: SourceRef {
				source: "https://github.com/anthropics/anthropic-sdk-python"
					.to_string(),
				ref_: None,
			},
			source_type: "github".to_string(),
			skill_path: Some("SKILL.md".to_string()),
			stored_hash: None,
			local_hash: None,
			ref_commit: None,
		}];
		let fetcher: Arc<dyn Fetcher> = Arc::new(GitFetcher);
		let resolver = KeyringResolver;
		let mut cache = ResultCache::new(CACHE_TTL);
		let deps = CheckDeps {
			ref_resolver: None,
			fetcher,
			resolver: &resolver,
			cache: &mut cache,
			per_fetch: PER_FETCH,
			concurrency: CONCURRENCY,
			offline: false,
			overall_deadline: OVERALL_DEADLINE,
		};
		let out = check_updates(entries, deps).await;
		// No panic; some status was produced for the entry.
		assert!(out.iter().any(|entry| entry.key.name == "public"));
	}

	/// A private repo with no resolvable token surfaces `Uncheckable { auth }`
	/// (or a redacted network error) and never panics or leaks a token.
	#[ignore = "network"]
	#[tokio::test]
	async fn e2e_check_private_repo_no_token_uncheckable() {
		use aghub_core::skills::update::SkillUpdateStatus;
		let entries = vec![EntryInput {
			name: "private".to_string(),
			scope: "global".to_string(),
			source_ref: SourceRef {
				source: "https://github.com/owner/definitely-private-repo"
					.to_string(),
				ref_: None,
			},
			source_type: "github".to_string(),
			skill_path: Some("SKILL.md".to_string()),
			stored_hash: None,
			local_hash: None,
			ref_commit: None,
		}];
		let fetcher: Arc<dyn Fetcher> = Arc::new(GitFetcher);
		let resolver = KeyringResolver;
		let mut cache = ResultCache::new(CACHE_TTL);
		let deps = CheckDeps {
			ref_resolver: None,
			fetcher,
			resolver: &resolver,
			cache: &mut cache,
			per_fetch: PER_FETCH,
			concurrency: CONCURRENCY,
			offline: false,
			overall_deadline: OVERALL_DEADLINE,
		};
		let out = check_updates(entries, deps).await;
		assert_eq!(out.len(), 1);
		assert!(matches!(
			out[0].status,
			SkillUpdateStatus::Uncheckable { .. }
		));
	}

	/// Fetcher stub that returns a pre-built local directory as if it were
	/// the upstream checkout. Used by the rename-guard integration test to
	/// exercise the apply path without a real network call.
	#[cfg(unix)]
	struct LocalRepoFetcher {
		root: PathBuf,
	}
	#[cfg(unix)]
	impl Fetcher for LocalRepoFetcher {
		fn fetch(
			&self,
			_source_ref: &SourceRef,
			_token: Option<&str>,
		) -> Result<skill_update::FetchedRepo, FetchError> {
			Ok(skill_update::FetchedRepo {
				root: self.root.clone(),
				oid: String::new(),
				upstream_commit_time: None,
				_guard: None,
			})
		}
	}

	/// The rename guard in `apply_skill_update` must reject the request
	/// (success=false, with the shared `SKILL_RENAMED_CODE`) when the fetched
	/// `SKILL.md` declares a name that differs from the lock entry. It must
	/// also leave the installed target untouched.
	#[cfg(unix)]
	#[test]
	fn apply_skill_update_renamed_guard_rejects_without_mutating() {
		with_isolated_state(|| {
			// We need an installed target so the apply path proceeds past
			// the `targets.is_empty()` short-circuit. The lock then points
			// at a real-feeling source; the fetch is intercepted by
			// `LocalRepoFetcher` to return a SKILL.md with a different name.
			let home = tempfile::tempdir().unwrap();
			let installed_dir = home.path().join(".claude/skills/some-skill");
			std::fs::create_dir_all(&installed_dir).unwrap();
			let pre_existing =
				"---\nname: some-skill\ndescription: original\n---\n\
				pre-existing body that must remain untouched\n"
					.to_string();
			std::fs::write(installed_dir.join("SKILL.md"), &pre_existing)
				.unwrap();
			let old_home = std::env::var("HOME").ok();
			std::env::set_var("HOME", home.path());

			// Global lock: `some-skill` is the locked name.
			let mut lock = skill::SkillLockFile::default();
			let mut entry = global_entry();
			entry.skill_path = Some("SKILL.md".to_string());
			lock.skills.insert("some-skill".into(), entry);
			skill::lock::global::write_skill_lock(&lock).unwrap();

			// Fetched repo declares a DIFFERENT name in SKILL.md frontmatter.
			let fetched = tempfile::tempdir().unwrap();
			std::fs::write(
				fetched.path().join("SKILL.md"),
				"---\nname: different-skill\ndescription: renamed upstream\n---\nnew body\n",
			)
			.unwrap();

			let fetcher = LocalRepoFetcher {
				root: fetched.path().to_path_buf(),
			};
			let req = ApplySkillUpdateRequest {
				name: "some-skill".to_string(),
				scope: "global".to_string(),
				project_root: None,
				confirm: Some(true),
			};

			let resolver = KeyringResolver;
			let resp =
				match rocket::tokio::runtime::Builder::new_current_thread()
					.enable_all()
					.build()
					.unwrap()
					.block_on(apply_skill_update_inner(
						req, &fetcher, &resolver,
					)) {
					Ok(json) => json.into_inner(),
					Err(error) => {
						panic!("apply should return Ok: {}", error.body.error)
					}
				};

			// Restore HOME before asserting, so other tests aren't disturbed.
			match old_home {
				Some(value) => std::env::set_var("HOME", value),
				None => std::env::remove_var("HOME"),
			}

			assert!(!resp.success, "rename must be rejected");
			assert_eq!(resp.code.as_deref(), Some(SKILL_RENAMED_CODE));
			let err = resp.error.expect("error message required");
			assert!(err.contains("some-skill"), "error: {err}");
			assert!(err.contains("different-skill"), "error: {err}");
			assert!(
				err.contains("Delete") && err.contains("install"),
				"advice missing: {err}"
			);

			// Lock hash must not have been written; the installed target
			// must be byte-for-byte unchanged.
			let lock = skill::lock::global::read_skill_lock();
			let entry = &lock.skills["some-skill"];
			assert!(entry.content_hash.is_none());
			let still_there =
				std::fs::read_to_string(installed_dir.join("SKILL.md"))
					.unwrap();
			assert_eq!(still_there, pre_existing);
		});
	}

	/// Recording fetcher: captures the token the apply path resolved + passed
	/// to the fetch, then returns the locked skill unchanged so the apply
	/// succeeds. Proves which credential reached the fetch.
	#[cfg(unix)]
	struct RecordingFetcher {
		root: PathBuf,
		seen_token: std::sync::Mutex<Option<Option<String>>>,
	}
	#[cfg(unix)]
	impl Fetcher for RecordingFetcher {
		fn fetch(
			&self,
			_source_ref: &SourceRef,
			token: Option<&str>,
		) -> Result<skill_update::FetchedRepo, FetchError> {
			*self.seen_token.lock().unwrap() = Some(token.map(str::to_string));
			Ok(skill_update::FetchedRepo {
				root: self.root.clone(),
				oid: String::new(),
				upstream_commit_time: None,
				_guard: None,
			})
		}
	}

	/// P1-b: a forwarded `X-Aghub-Git-Tokens` entry (the new `{token,origin}`
	/// shape) must reach the apply-update fetch via the [`ChainResolver`], with
	/// the controller-resolved origin matching the locked source.
	#[cfg(unix)]
	#[test]
	fn apply_update_uses_forwarded_token_for_fetch() {
		use crate::credentials::forwarding::{ForwardedEntry, ForwardedOrigin};
		with_isolated_state(|| {
			let home = tempfile::tempdir().unwrap();
			let installed_dir = home.path().join(".claude/skills/some-skill");
			std::fs::create_dir_all(&installed_dir).unwrap();
			std::fs::write(
				installed_dir.join("SKILL.md"),
				"---\nname: some-skill\ndescription: original\n---\nold body\n",
			)
			.unwrap();
			let old_home = std::env::var("HOME").ok();
			std::env::set_var("HOME", home.path());

			// Locked source resolves to https://github.com/owner/repo.
			let mut lock = skill::SkillLockFile::default();
			let mut entry = global_entry();
			entry.skill_path = Some("SKILL.md".to_string());
			lock.skills.insert("some-skill".into(), entry);
			skill::lock::global::write_skill_lock(&lock).unwrap();

			// Fetched repo keeps the SAME name so the rename guard passes and the
			// fetch is actually consulted.
			let fetched = tempfile::tempdir().unwrap();
			std::fs::write(
				fetched.path().join("SKILL.md"),
				"---\nname: some-skill\ndescription: updated\n---\nnew body\n",
			)
			.unwrap();

			let fetcher = RecordingFetcher {
				root: fetched.path().to_path_buf(),
				seen_token: std::sync::Mutex::new(None),
			};

			// Forwarded header carries a github.com-pinned token for the source.
			let mut map = std::collections::BTreeMap::new();
			map.insert(
				"owner/repo".to_string(),
				ForwardedEntry {
					token: "FWD-TOKEN".to_string(),
					origin: Some(ForwardedOrigin {
						scheme: "https".to_string(),
						host: "github.com".to_string(),
						port: Some(443),
					}),
				},
			);
			let forwarded = ForwardedGitTokens(map);
			let keyring = KeyringResolver;
			let resolver =
				ChainResolver::new(forwarded.into_resolver(), &keyring);

			let req = ApplySkillUpdateRequest {
				name: "some-skill".to_string(),
				scope: "global".to_string(),
				project_root: None,
				confirm: Some(true),
			};

			let resp =
				match rocket::tokio::runtime::Builder::new_current_thread()
					.enable_all()
					.build()
					.unwrap()
					.block_on(apply_skill_update_inner(
						req, &fetcher, &resolver,
					)) {
					Ok(json) => json.into_inner(),
					Err(error) => {
						panic!("apply should return Ok: {}", error.body.error)
					}
				};

			match old_home {
				Some(value) => std::env::set_var("HOME", value),
				None => std::env::remove_var("HOME"),
			}

			assert!(resp.success, "apply should succeed: {:?}", resp.error);
			let seen = fetcher.seen_token.lock().unwrap().clone();
			assert_eq!(
				seen,
				Some(Some("FWD-TOKEN".to_string())),
				"the forwarded token must reach the apply fetch"
			);
		});
	}

	/// Global-scope happy path through the apply route: a matching-name source
	/// must swap the installed copy AND advance the global lock hash — i.e.
	/// resync's GlobalOnly swap+lock branch, asserting the on-disk and lock
	/// effects (not just `success`).
	#[cfg(unix)]
	#[test]
	fn apply_update_global_swaps_content_and_advances_lock() {
		with_isolated_state(|| {
			let home = tempfile::tempdir().unwrap();
			let installed_dir = home.path().join(".claude/skills/some-skill");
			std::fs::create_dir_all(&installed_dir).unwrap();
			std::fs::write(
				installed_dir.join("SKILL.md"),
				"---\nname: some-skill\ndescription: original\n---\nold body\n",
			)
			.unwrap();
			let old_home = std::env::var("HOME").ok();
			std::env::set_var("HOME", home.path());

			let mut lock = skill::SkillLockFile::default();
			let mut entry = global_entry();
			entry.skill_path = Some("SKILL.md".to_string());
			lock.skills.insert("some-skill".into(), entry);
			skill::lock::global::write_skill_lock(&lock).unwrap();

			let fetched = tempfile::tempdir().unwrap();
			std::fs::write(
				fetched.path().join("SKILL.md"),
				"---\nname: some-skill\ndescription: updated\n---\nnew body\n",
			)
			.unwrap();
			let fetcher = LocalRepoFetcher {
				root: fetched.path().to_path_buf(),
			};
			let resolver = KeyringResolver;
			let req = ApplySkillUpdateRequest {
				name: "some-skill".to_string(),
				scope: "global".to_string(),
				project_root: None,
				confirm: Some(true),
			};
			let result = rocket::tokio::runtime::Builder::new_current_thread()
				.enable_all()
				.build()
				.unwrap()
				.block_on(apply_skill_update_inner(req, &fetcher, &resolver));

			match old_home {
				Some(value) => std::env::set_var("HOME", value),
				None => std::env::remove_var("HOME"),
			}

			let resp = match result {
				Ok(json) => json.into_inner(),
				Err(error) => {
					panic!("apply should return Ok: {}", error.body.error)
				}
			};
			assert!(resp.success, "apply should succeed: {:?}", resp.error);
			assert!(std::fs::read_to_string(installed_dir.join("SKILL.md"))
				.unwrap()
				.contains("new body"));
			let lock = skill::lock::global::read_skill_lock();
			assert!(
				lock.skills["some-skill"].content_hash.is_some(),
				"global lock hash must advance after a successful apply"
			);
		});
	}

	#[cfg(unix)]
	#[test]
	fn accept_rename_inner_rejects_without_confirm() {
		use crate::dto::skill::AcceptRenameRequest;
		let req = AcceptRenameRequest {
			old_name: "old".to_string(),
			new_name: "new".to_string(),
			scope: "global".to_string(),
			project_root: None,
			confirm: Some(false),
		};
		let fetcher = LocalRepoFetcher {
			root: std::path::PathBuf::from("/tmp"),
		};
		let resp = run_accept_rename(req, &fetcher);
		assert!(!resp.success);
		assert!(resp.error.as_deref().unwrap_or("").contains("confirm"));
	}

	#[cfg(unix)]
	#[test]
	fn accept_rename_inner_installs_new_and_removes_old() {
		with_isolated_state(|| {
			let home = tempfile::tempdir().unwrap();
			// Install old skill
			let old_dir = home.path().join(".claude/skills/old-skill");
			std::fs::create_dir_all(&old_dir).unwrap();
			std::fs::write(
				old_dir.join("SKILL.md"),
				"---\nname: old-skill\ndescription: original\n---\n",
			)
			.unwrap();
			let old_home = std::env::var("HOME").ok();
			std::env::set_var("HOME", home.path());

			// Lock entry for old-skill
			let mut lock = skill::SkillLockFile::default();
			let mut entry = global_entry();
			entry.skill_path = Some("new-skill/SKILL.md".to_string());
			lock.skills.insert("old-skill".into(), entry);
			skill::lock::global::write_skill_lock(&lock).unwrap();

			// Fetched repo has SKILL.md with new name
			let fetched = tempfile::tempdir().unwrap();
			let new_skill_dir = fetched.path().join("new-skill");
			std::fs::create_dir_all(&new_skill_dir).unwrap();
			std::fs::write(
				new_skill_dir.join("SKILL.md"),
				"---\nname: new-skill\ndescription: renamed\n---\nbody\n",
			)
			.unwrap();
			let fetcher = LocalRepoFetcher {
				root: fetched.path().to_path_buf(),
			};

			let req = crate::dto::skill::AcceptRenameRequest {
				old_name: "old-skill".to_string(),
				new_name: "new-skill".to_string(),
				scope: "global".to_string(),
				project_root: None,
				confirm: Some(true),
			};
			let resp = run_accept_rename(req, &fetcher);

			match old_home {
				Some(v) => std::env::set_var("HOME", v),
				None => std::env::remove_var("HOME"),
			}

			assert!(resp.success, "error: {:?}", resp.error);
			assert_eq!(resp.old_name, "old-skill");
			assert_eq!(resp.new_name, "new-skill");

			// New skill dir should exist
			assert!(
				home.path().join(".claude/skills/new-skill").exists(),
				"new skill dir must be installed"
			);
			// Old skill dir should be removed
			assert!(
				!home.path().join(".claude/skills/old-skill").exists(),
				"old skill dir must be removed"
			);

			// Lock: new-skill present, old-skill absent
			let lock = skill::lock::global::read_skill_lock();
			assert!(lock.skills.contains_key("new-skill"), "new-skill in lock");
			assert!(
				!lock.skills.contains_key("old-skill"),
				"old-skill removed from lock"
			);
		});
	}

	/// P0-2 guard (a): a degenerate rename whose old/new names sanitize to the
	/// same on-disk dir must be rejected up front (before any fetch/mutation)
	/// with the `RENAME_TARGET_EXISTS_CODE` machine code.
	#[cfg(unix)]
	#[test]
	fn accept_rename_rejects_degenerate_sanitized_collision() {
		with_isolated_state(|| {
			// "old skill" and "old-skill" both sanitize to "old-skill".
			assert_eq!(
				skill::sanitize::sanitize_name("old skill"),
				skill::sanitize::sanitize_name("old-skill"),
			);
			let fetcher = LocalRepoFetcher {
				root: std::path::PathBuf::from("/tmp"),
			};
			let req = crate::dto::skill::AcceptRenameRequest {
				old_name: "old skill".to_string(),
				new_name: "old-skill".to_string(),
				scope: "global".to_string(),
				project_root: None,
				confirm: Some(true),
			};
			let resp = run_accept_rename(req, &fetcher);
			assert!(!resp.success, "degenerate rename must be rejected");
			assert_eq!(resp.code.as_deref(), Some(RENAME_TARGET_EXISTS_CODE));
		});
	}

	/// P0-2 guard (b): when the new name is ALREADY installed (on-disk dir),
	/// accept-rename must refuse BEFORE mutating — so the rollback's
	/// "remove all new_name paths" can never delete pre-existing data. The
	/// pre-existing new-skill dir must remain byte-for-byte intact.
	#[cfg(unix)]
	#[test]
	fn accept_rename_rejects_when_new_name_already_installed() {
		with_isolated_state(|| {
			let home = tempfile::tempdir().unwrap();
			// Old skill installed + locked.
			let old_dir = home.path().join(".claude/skills/old-skill");
			std::fs::create_dir_all(&old_dir).unwrap();
			std::fs::write(
				old_dir.join("SKILL.md"),
				"---\nname: old-skill\ndescription: original\n---\n",
			)
			.unwrap();
			// New skill ALREADY present on disk with sentinel content.
			let new_dir = home.path().join(".claude/skills/new-skill");
			std::fs::create_dir_all(&new_dir).unwrap();
			let pre_existing =
				"---\nname: new-skill\ndescription: PRE-EXISTING\n---\n\
				 do not clobber\n"
					.to_string();
			std::fs::write(new_dir.join("SKILL.md"), &pre_existing).unwrap();
			let old_home = std::env::var("HOME").ok();
			std::env::set_var("HOME", home.path());

			let mut lock = skill::SkillLockFile::default();
			let mut entry = global_entry();
			entry.skill_path = Some("new-dir/SKILL.md".to_string());
			lock.skills.insert("old-skill".into(), entry);
			skill::lock::global::write_skill_lock(&lock).unwrap();

			// Fetched repo declares the new name.
			let fetched = tempfile::tempdir().unwrap();
			let new_skill_src = fetched.path().join("new-dir");
			std::fs::create_dir_all(&new_skill_src).unwrap();
			std::fs::write(
				new_skill_src.join("SKILL.md"),
				"---\nname: new-skill\ndescription: renamed\n---\nbody\n",
			)
			.unwrap();
			let fetcher = LocalRepoFetcher {
				root: fetched.path().to_path_buf(),
			};
			let req = crate::dto::skill::AcceptRenameRequest {
				old_name: "old-skill".to_string(),
				new_name: "new-skill".to_string(),
				scope: "global".to_string(),
				project_root: None,
				confirm: Some(true),
			};
			let resp = run_accept_rename(req, &fetcher);

			match old_home {
				Some(v) => std::env::set_var("HOME", v),
				None => std::env::remove_var("HOME"),
			}

			assert!(!resp.success, "must refuse to clobber existing new-skill");
			assert_eq!(resp.code.as_deref(), Some(RENAME_TARGET_EXISTS_CODE));
			// Pre-existing new-skill dir must be untouched.
			let still =
				std::fs::read_to_string(new_dir.join("SKILL.md")).unwrap();
			assert_eq!(still, pre_existing, "new-skill must not be clobbered");
			// Old skill + its lock entry must remain (nothing mutated).
			assert!(old_dir.exists(), "old skill dir must remain");
			let lock = skill::lock::global::read_skill_lock();
			assert!(lock.skills.contains_key("old-skill"));
			assert!(!lock.skills.contains_key("new-skill"));
		});
	}

	#[cfg(unix)]
	#[test]
	fn accept_rename_inner_rollback_on_removal_failure() {
		// Make the old-skill agent dir read-only so the transaction fails
		// (either at install or at removal). Either way the end state must be
		// the pre-transaction state: old-skill in the lock, new-skill absent,
		// and the old-skill dir still on disk. Skipped under root, where mode
		// 0o500 is ignored and writes still succeed.
		use std::os::unix::fs::PermissionsExt;
		with_isolated_state(|| {
			let home = tempfile::tempdir().unwrap();
			let old_dir = home.path().join(".claude/skills/old-skill");
			std::fs::create_dir_all(&old_dir).unwrap();
			std::fs::write(
				old_dir.join("SKILL.md"),
				"---\nname: old-skill\ndescription: original\n---\n",
			)
			.unwrap();
			let old_home = std::env::var("HOME").ok();
			std::env::set_var("HOME", home.path());

			let mut lock = skill::SkillLockFile::default();
			let mut entry = global_entry();
			entry.skill_path = Some("new-skill/SKILL.md".to_string());
			lock.skills.insert("old-skill".into(), entry);
			skill::lock::global::write_skill_lock(&lock).unwrap();

			let fetched = tempfile::tempdir().unwrap();
			let new_skill_dir = fetched.path().join("new-skill");
			std::fs::create_dir_all(&new_skill_dir).unwrap();
			std::fs::write(
				new_skill_dir.join("SKILL.md"),
				"---\nname: new-skill\ndescription: renamed\n---\nbody\n",
			)
			.unwrap();

			// Root probe: a process running as root ignores 0o500, so the
			// failure we rely on never happens — skip rather than false-pass.
			let skills_dir = home.path().join(".claude/skills");
			let original_perms =
				std::fs::metadata(&skills_dir).unwrap().permissions();
			std::fs::set_permissions(
				&skills_dir,
				std::fs::Permissions::from_mode(0o500),
			)
			.unwrap();
			let probe = skills_dir.join(".rename-root-probe");
			let is_root = std::fs::write(&probe, b"x").is_ok();
			if is_root {
				let _ = std::fs::remove_file(&probe);
				std::fs::set_permissions(&skills_dir, original_perms).unwrap();
				match old_home {
					Some(v) => std::env::set_var("HOME", v),
					None => std::env::remove_var("HOME"),
				}
				eprintln!("skipping under root: 0o500 is not enforced");
				return;
			}

			let fetcher = LocalRepoFetcher {
				root: fetched.path().to_path_buf(),
			};
			let req = crate::dto::skill::AcceptRenameRequest {
				old_name: "old-skill".to_string(),
				new_name: "new-skill".to_string(),
				scope: "global".to_string(),
				project_root: None,
				confirm: Some(true),
			};
			let resp = run_accept_rename(req, &fetcher);

			// Restore permissions before asserting so other tests aren't
			// disturbed and the tempdir can be cleaned up.
			std::fs::set_permissions(&skills_dir, original_perms).unwrap();
			match old_home {
				Some(v) => std::env::set_var("HOME", v),
				None => std::env::remove_var("HOME"),
			}

			// The op must fail (install or removal under the locked dir).
			assert!(
				!resp.success,
				"must fail when the old-skill dir cannot be mutated"
			);
			// The old-skill dir must still be present (restored / never lost).
			assert!(
				old_dir.exists(),
				"old skill dir must remain after a failed transaction"
			);
			// The lock must remain with only old-skill (no partial state).
			let lock = skill::lock::global::read_skill_lock();
			assert!(
				lock.skills.contains_key("old-skill"),
				"lock must be restored to old-skill only"
			);
			assert!(
				!lock.skills.contains_key("new-skill"),
				"new-skill must not be in lock after rollback"
			);
		});
	}
}
