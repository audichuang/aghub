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

use aghub_core::models::{ResourceScope, Skill};
use chrono::Utc;
use rocket::http::Status;
use rocket::serde::json::Json;

use crate::dto::skill::{
	ApplySkillUpdateRequest, ApplySkillUpdateResponse, SkillUpdateResponse,
	SkillUpdateStatusResponse,
};
use crate::error::{ApiError, ApiResult};
use crate::extractors::{ResolvedScope, ScopeParams};
use crate::skills::rename::{
	detect_rename, skill_renamed_message, SKILL_RENAMED_CODE,
};
use skill_update::{
	check_updates, keychain_host_for_source, CheckDeps, CheckOutput,
	EntryInput, FetchError, Fetcher, GitFetcher, GitRefResolver, ResultCache,
	SourceRef, TokenResolver,
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

fn skill_root(skill: &Skill) -> Option<PathBuf> {
	let raw = skill
		.canonical_path
		.as_deref()
		.or(skill.source_path.as_deref())?;
	let path = if let Some(stripped) = raw.strip_prefix("~/") {
		dirs::home_dir().map(|home| home.join(stripped))?
	} else {
		PathBuf::from(raw)
	};
	let is_skill_file = path
		.file_name()
		.is_some_and(|name| name == std::ffi::OsStr::new("SKILL.md"));
	Some(if is_skill_file {
		path.parent().map(Path::to_path_buf).unwrap_or(path)
	} else {
		path
	})
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
				source: entry.source,
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

pub(crate) fn installed_skill_roots(
	name: &str,
	resource_scope: ResourceScope,
	project_root: Option<&Path>,
) -> Vec<PathBuf> {
	let mut roots = Vec::new();
	for agent in aghub_core::load_all_agents(resource_scope, project_root) {
		for skill in agent.skills {
			if skill.name != name {
				continue;
			}
			let Some(root) = skill_root(&skill) else {
				continue;
			};
			if !roots.contains(&root) {
				roots.push(root);
			}
		}
	}
	roots
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
				source: entry.source.clone(),
				ref_name: entry.ref_name.clone(),
				skill_path,
			})
		}
		_ => Err("scope must be global or project".to_string()),
	}
}

pub(crate) fn update_lock_hash(
	name: &str,
	scope: &str,
	project_root: Option<&Path>,
	hash: &str,
	ref_commit: Option<&str>,
) -> Result<(), String> {
	match scope {
		"global" => skill::lock::global::modify_skill_lock(|lock| {
			let Some(entry) = lock.skills.get_mut(name) else {
				return Err("Skill is not in global lock".to_string());
			};
			entry.apply_content_hash(hash, &Utc::now().to_rfc3339());
			if let Some(oid) = ref_commit {
				entry.ref_commit = Some(oid.to_string());
			}
			Ok(())
		})
		.map_err(|e| format!("Failed to update global lock: {e}"))?,
		"project" => {
			let Some(root) = project_root else {
				return Err("project_root is required when scope is project"
					.to_string());
			};
			skill::lock::local::modify_local_lock(Some(root), |lock| {
				let Some(entry) = lock.skills.get_mut(name) else {
					return Err("Skill is not in project lock".to_string());
				};
				entry.apply_computed_hash(hash);
				if let Some(oid) = ref_commit {
					entry.ref_commit = Some(oid.to_string());
				}
				Ok(())
			})
			.map_err(|e| format!("Failed to update project lock: {e}"))?
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
) -> ApiResult<Vec<SkillUpdateResponse>> {
	let resolved = ScopeParams {
		scope: query.scope.clone(),
		project_root: query.project_root.clone(),
	}
	.resolve()?;
	let offline = query.offline.unwrap_or(false);
	let (entries, project_root) = lock_entries_for_scope(&resolved, offline)?;

	let fetcher: Arc<dyn Fetcher> = Arc::new(GitFetcher);
	let resolver = KeyringResolver;
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
) -> ApiResult<ApplySkillUpdateResponse> {
	apply_skill_update_inner(body.into_inner(), &GitFetcher).await
}

/// Inner apply path that takes an injected [`Fetcher`] so the rename guard
/// (and the rest of the happy-path wiring) is unit-testable without a real
/// network. The route handler is a thin shim that supplies [`GitFetcher`].
pub(crate) async fn apply_skill_update_inner(
	req: ApplySkillUpdateRequest,
	fetcher: &dyn Fetcher,
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
	let targets = installed_skill_roots(
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

	let resolver = KeyringResolver;
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
	let parsed_skill = match skill::parse(&skill_file) {
		Ok(skill) => skill,
		Err(e) => {
			return Ok(Json(apply_error(
				&req.name,
				&req.scope,
				&format!("Failed to parse fetched skill: {e}"),
			)));
		}
	};
	if let Some(new_name) = detect_rename(&parsed_skill.name, &req.name) {
		return Ok(Json(apply_error_with_code(
			&req.name,
			&req.scope,
			&skill_renamed_message(&req.name, &new_name),
			Some(SKILL_RENAMED_CODE),
		)));
	}
	let updated_hash = match skill::compute_skill_folder_hash(source_dir) {
		Ok(hash) => hash,
		Err(e) => {
			return Ok(Json(apply_error(
				&req.name,
				&req.scope,
				&format!("Failed to hash fetched skill: {e}"),
			)));
		}
	};

	let agent_dirs = aghub_core::skills::removal::agent_skill_dirs_in_scope(
		resource_scope,
		project_root.as_deref(),
	);
	if let Err(error) = aghub_core::skills::removal::assert_targets_contained(
		&targets,
		&agent_dirs,
		project_root.as_deref(),
	) {
		return Ok(Json(apply_error(
			&req.name,
			&req.scope,
			&error.to_string(),
		)));
	}

	let mut paths = Vec::new();
	for target in &targets {
		if let Err(error) =
			aghub_core::skills::update::stage_and_swap_dir(source_dir, target)
		{
			return Ok(Json(apply_error(
				&req.name,
				&req.scope,
				&format!("Failed to replace installed skill: {error}"),
			)));
		}
		paths.push(target.display().to_string());
	}

	if let Err(response) = update_lock_hash(
		&req.name,
		&req.scope,
		project_root.as_deref(),
		&updated_hash,
		Some(&repo.oid),
	) {
		return Ok(Json(apply_error(&req.name, &req.scope, &response)));
	}

	Ok(Json(ApplySkillUpdateResponse {
		success: true,
		name: req.name,
		scope: req.scope,
		updated_hash: Some(updated_hash),
		paths,
		error: None,
		code: None,
	}))
}

#[cfg(test)]
mod tests {
	use super::*;
	use aghub_core::skills::update::SkillUpdateStatus;
	use skill_update::EntryKey;

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

			update_lock_hash("legacy", "global", None, "content-v2", None)
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
				"global",
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
	fn auto_heal_writes_project_computed_hash_only() {
		with_isolated_state(|| {
			let project = tempfile::tempdir().unwrap();
			let mut local = skill::LocalSkillLockFile::default();
			local.skills.insert(
				"legacy".into(),
				skill::LocalSkillLockEntry {
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

			let resp =
				match rocket::tokio::runtime::Builder::new_current_thread()
					.enable_all()
					.build()
					.unwrap()
					.block_on(apply_skill_update_inner(req, &fetcher))
				{
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
}
