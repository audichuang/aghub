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
	EntryInput, FetchError, Fetcher, GitFetcher, GitRefResolver, ResultCache,
	SourceRef, TokenResolution, TokenResolver,
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
/// binding + host keychain resolution.
///
/// A per-request SNAPSHOT of the stored credentials + source bindings, loaded
/// ONCE (via [`KeyringResolver::load`]) and resolved against purely in
/// memory afterwards. `check_updates` calls `resolve()` once per unique
/// `(source, ref)` group — with the old "read the keyring on every call"
/// version that meant 2 secret-service D-Bus round trips PER GROUP (unlike
/// the old keyutils backend, a secret-service read is not "local/cheap" —
/// see GitHub #15 P1b). Snapshotting also means the one read that does
/// happen goes through `load()`'s `spawn_blocking`, never inline on
/// whichever thread is polling the route's async future (Rocket does not
/// spawn_blocking sync handlers on its own — see `crates/api/Cargo.toml`'s
/// keyring feature comment).
pub(crate) struct KeyringResolver {
	creds: Vec<crate::routes::credentials::StoredCredential>,
	bindings: crate::credentials::resolve::SourceBindings,
}

impl KeyringResolver {
	/// The one place any of the three snapshot-loading variants below reads
	/// the keyring: both sources, on Rocket's blocking-task pool, in a single
	/// `spawn_blocking`. Each inner `Result` carries its own
	/// [`crate::credentials::CredentialStoreError`]; the outer `Result` is
	/// `Err` only if the blocking task itself panicked or was cancelled.
	async fn load_parts() -> Result<
		(
			Result<
				Vec<crate::routes::credentials::StoredCredential>,
				crate::credentials::CredentialStoreError,
			>,
			Result<
				crate::credentials::resolve::SourceBindings,
				crate::credentials::CredentialStoreError,
			>,
		),
		tokio::task::JoinError,
	> {
		tokio::task::spawn_blocking(|| {
			(
				crate::routes::credentials::load_credentials(),
				crate::credentials::resolve::load_source_bindings(),
			)
		})
		.await
	}

	fn empty() -> Self {
		Self {
			creds: Vec::new(),
			bindings: Default::default(),
		}
	}

	/// Load the snapshot on Rocket's blocking-task pool, once per request.
	/// A failed read (backend unreachable, corrupt entry, ...) degrades to an
	/// empty snapshot — same as the resolver's previous per-call behavior —
	/// so a keyring outage means "no credential resolves", not a hard error
	/// for what is a best-effort convenience (skills stay `Uncheckable`
	/// rather than the whole request failing).
	///
	/// Only for the non-mutating check-updates path. Mutating routes
	/// (apply-update, accept-rename, git-scan's credential resolution) MUST
	/// use [`KeyringResolver::load_or_unavailable`] instead — silently
	/// degrading to "no credential" there used to turn a keyring outage into
	/// a confusing downstream clone/auth failure instead of a stable 503
	/// (GitHub #15 P2-3).
	pub(crate) async fn load() -> Self {
		match Self::load_parts().await {
			Ok((creds, bindings)) => Self {
				creds: creds.unwrap_or_default(),
				bindings: bindings.unwrap_or_default(),
			},
			Err(_) => Self::empty(),
		}
	}

	/// Load the snapshot for a MUTATING route. Unlike [`KeyringResolver::load`],
	/// a failed read is surfaced as a typed [`ApiError`] (503
	/// `KEYCHAIN_UNAVAILABLE` for an unreachable backend, via
	/// [`crate::credentials::CredentialStoreError`]'s `ApiError` mapping)
	/// instead of silently degrading to an empty snapshot — a mutating
	/// operation on a private source must fail closed, not proceed as if no
	/// credential were bound and then fail with a confusing clone/auth error
	/// (GitHub #15 P2-3). Still runs on the blocking pool, same reason as
	/// `load` (GitHub #15 P1b).
	pub(crate) async fn load_or_unavailable() -> Result<Self, ApiError> {
		match Self::load_parts().await {
			Ok((creds, bindings)) => Ok(Self {
				creds: creds.map_err(ApiError::from)?,
				bindings: bindings.map_err(ApiError::from)?,
			}),
			Err(e) => Err(ApiError::from_join_error(
				e,
				"Credential operation failed",
				"CREDENTIAL_TASK_ERROR",
			)),
		}
	}

	/// Find a stored credential by id (git-scan's explicit `credential_id`
	/// request field).
	pub(crate) fn find_credential(
		&self,
		id: &str,
	) -> Option<&crate::routes::credentials::StoredCredential> {
		self.creds.iter().find(|c| c.id == id)
	}

	/// Load the snapshot for a MUTATING route's forwarded-first fallback, on
	/// the blocking pool, WITHOUT hard-erroring on an unreachable backend.
	/// Returns the (possibly empty) snapshot plus a flag: `true` iff the
	/// credential backend itself was unreachable.
	///
	/// Unlike [`KeyringResolver::load_or_unavailable`] (which 503s eagerly on
	/// an unreachable backend), the caller ([`LazyKeyringFallback`]) only
	/// escalates that flag to a 503 when the forwarded map ALSO misses the
	/// requested source — so a forwarded hit is never blocked by an
	/// unreachable LOCAL keyring (GitHub #15 round-2 forwarded-token
	/// regression). Doing the read here — once, up front, via `spawn_blocking`
	/// — keeps it OFF the route's async worker: the previous
	/// lazy-inside-`resolve()` version ran `load_credentials` /
	/// `load_source_bindings` synchronously on the async task, re-introducing
	/// the very blocking-I/O hazard P1b fixed (GitHub #15 round-3 P1).
	pub(crate) async fn load_soft() -> (Self, bool) {
		use crate::credentials::CredentialStoreError::Unavailable;
		match Self::load_parts().await {
			Ok((creds, bindings)) => {
				let unavailable = matches!(creds, Err(Unavailable(_)))
					|| matches!(bindings, Err(Unavailable(_)));
				(
					Self {
						creds: creds.unwrap_or_default(),
						bindings: bindings.unwrap_or_default(),
					},
					unavailable,
				)
			}
			// The blocking task panicked or was cancelled (JoinError): the
			// keyring state is INDETERMINATE, so treat it as unavailable
			// (fail-closed) rather than "reachable, no credential"
			// (fail-open). A forwarded MISS then 503s instead of silently
			// attempting an anonymous fetch on what may be a private source;
			// a forwarded HIT still short-circuits before this flag is ever
			// consulted, so it succeeds regardless (GitHub #15 round-4).
			Err(_join) => (Self::empty(), true),
		}
	}
}

impl TokenResolver for KeyringResolver {
	fn resolve(&self, source: &str, host: Option<&str>) -> Option<String> {
		crate::credentials::resolve::resolve_token_for_source(
			source,
			host,
			&self.bindings,
			&self.creds,
		)
	}
}

/// Production [`TokenResolver`] for the two MUTATING routes
/// (`apply_skill_update`, `accept_skill_rename`): tries the forwarded map
/// first and, on a miss, consults a keyring snapshot pre-loaded once up front
/// on the blocking pool (see below) — `resolve()` never touches the OS
/// keyring directly.
///
/// Regression fix (GitHub #15 round-2 Codex finding): both routes used to
/// build their resolver as `KeyringResolver::load_or_unavailable().await?`
/// (an eager, fail-closed keyring snapshot load) BEFORE constructing the
/// `ChainResolver` that tries forwarded first — so an unreachable keyring
/// backend 503'd the request UNCONDITIONALLY, even when the forwarded
/// header already covered the requested source. That defeats the entire
/// point of forwarding: a headless remote (no keyring of its own, relying on
/// the desktop client forwarding a per-source token over
/// `X-Aghub-Git-Tokens`) would 503 on every apply-update/accept-rename
/// whenever ITS OWN (irrelevant) keyring happened to be unreachable.
///
/// This type keeps forwarded-first precedence while never 503-ing on a
/// forwarded hit: the keyring snapshot is pre-loaded ONCE, up front, on the
/// blocking pool ([`KeyringResolver::load_soft`]), which — unlike
/// `load_or_unavailable` — does NOT hard-error on an unreachable backend but
/// returns an empty snapshot plus an `unavailable` flag. `resolve()` then
/// tries the forwarded map first (a hit short-circuits and can never 503),
/// and only on a miss consults the in-memory snapshot; if the backend was
/// unavailable at load time AND forwarding missed this source, it records
/// that so the handler can turn it into a 503. `resolve()` itself does NO
/// keyring I/O, so it never blocks the async worker (an earlier version read
/// the keyring synchronously inside `resolve()` — GitHub #15 round-3 P1).
///
/// Plumbed through `apply_skill_update_inner`/`accept_rename_inner`'s
/// existing `resolver: &dyn TokenResolver` parameter, same as every other
/// caller — those fns call [`TokenResolver::resolve_checked`] and fail closed
/// (503) BEFORE attempting the fetch on a `BackendUnavailable` outcome,
/// rather than proceeding with a `None` token that might really mean
/// "couldn't tell" (see that method's doc comment). Every OTHER resolver in
/// this module (`KeyringResolver`, `ChainResolver`, plain stubs) keeps the
/// trait's default `resolve_checked`, so no existing test is affected by this
/// addition.
struct LazyKeyringFallback {
	forwarded: crate::credentials::forwarding::ForwardedTokenResolver,
	/// Keyring snapshot pre-loaded ONCE, up front, on the blocking pool (via
	/// [`KeyringResolver::load_soft`]) — `resolve()` only ever reads this
	/// in-memory copy, never the OS keyring directly, so it never blocks the
	/// async worker (GitHub #15 round-3 P1).
	snapshot: KeyringResolver,
	/// Whether the backend was unreachable when `snapshot` was loaded.
	keyring_unavailable: bool,
}

impl LazyKeyringFallback {
	fn new(
		forwarded: crate::credentials::forwarding::ForwardedTokenResolver,
		snapshot: KeyringResolver,
		keyring_unavailable: bool,
	) -> Self {
		Self {
			forwarded,
			snapshot,
			keyring_unavailable,
		}
	}
}

impl TokenResolver for LazyKeyringFallback {
	fn resolve(&self, source: &str, host: Option<&str>) -> Option<String> {
		// Delegate to `resolve_checked` — never re-derive the priority order
		// here, or the two could drift.
		match self.resolve_checked(source, host) {
			TokenResolution::Token(token) => Some(token),
			TokenResolution::NoToken | TokenResolution::BackendUnavailable => {
				None
			}
		}
	}

	fn resolve_checked(
		&self,
		source: &str,
		host: Option<&str>,
	) -> TokenResolution {
		if let Some(token) = self.forwarded.resolve(source, host) {
			return TokenResolution::Token(token);
		}
		// Forwarding missed this source -- consult the PRE-LOADED keyring
		// snapshot (pure in-memory lookup, no I/O on the async worker).
		if self.keyring_unavailable {
			// The backend was unreachable at load time AND forwarding did not
			// cover this source -> fail closed; the caller turns this into a
			// 503.
			return TokenResolution::BackendUnavailable;
		}
		// Backend was reachable: `None` here is a genuine "no credential
		// bound", so a public source proceeds anonymously (unchanged
		// behavior). Any non-`Unavailable` load failure (corrupt JSON, ...)
		// already degraded to an empty snapshot in `load_soft`.
		match self.snapshot.resolve(source, host) {
			Some(token) => TokenResolution::Token(token),
			None => TokenResolution::NoToken,
		}
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

	let fetcher: Arc<dyn Fetcher> = Arc::new(GitFetcher);
	// Forwarded tokens (header) take precedence over the local keyring; an
	// absent/empty header degrades to the keyring path (backward compatible).
	let keyring = KeyringResolver::load().await;
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
	// Forwarded tokens (header) take precedence over the local keyring. The
	// keyring snapshot is pre-loaded ONCE here on the blocking pool
	// (`load_soft`), so no keyring I/O ever runs on the async worker; the
	// resolver then tries forwarded first and consults that in-memory
	// snapshot only on a miss (see `LazyKeyringFallback`). This is a MUTATING
	// route, so a forwarded miss WITH an unreachable backend fails the whole
	// request closed with 503 — checked inside `apply_skill_update_inner`
	// right after resolving the token and before any fetch is attempted.
	let (snapshot, keyring_unavailable) = KeyringResolver::load_soft().await;
	let resolver = LazyKeyringFallback::new(
		forwarded.into_resolver(),
		snapshot,
		keyring_unavailable,
	);
	apply_skill_update_inner(body.into_inner(), &GitFetcher, &resolver).await
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

	// Fail closed BEFORE attempting any fetch if the resolver reports its
	// backend unreachable (see `TokenResolver::resolve_checked` and
	// `LazyKeyringFallback`) — a `None` token reached that way might really
	// mean "couldn't tell", not "no credential needed" (GitHub #15 round-2
	// forwarded-token regression fix: this check must run before the fetch,
	// not after, so an unreachable backend never triggers a wasted real
	// network fetch attempt first).
	let token = match resolver.resolve_checked(
		&source.source,
		keychain_host_for_source(&source.source).as_deref(),
	) {
		TokenResolution::Token(token) => Some(token),
		TokenResolution::NoToken => None,
		TokenResolution::BackendUnavailable => {
			return Err(crate::credentials::CredentialStoreError::Unavailable(
				"credential backend unreachable".to_string(),
			)
			.into());
		}
	};
	let folder =
		match skill_update::skill_folder_from_lock_path(&source.skill_path) {
			Some(f) => f,
			None => {
				return Ok(Json(apply_error(
					&req.name,
					&req.scope,
					"Locked skillPath is not a valid skill folder",
				)));
			}
		};
	let repo = match fetcher.fetch(
		&SourceRef {
			source: source.source.clone(),
			ref_: source.ref_name.clone(),
		},
		token.as_deref(),
		skill_update::FetchSelection::Skills(std::slice::from_ref(&folder)),
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
		ref_commit: Some(repo.oid()),
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

/// `POST /skills/accept-rename` — atomic rename: install the new name, delete
/// the old name, transition both lock entries. The transaction is owned by
/// `aghub_core::skills::rename`; this route just wires credentials + fetch.
#[post("/skills/accept-rename", data = "<body>")]
pub async fn accept_skill_rename(
	body: Json<AcceptRenameRequest>,
	forwarded: ForwardedGitTokens,
	_origin: TrustedLocalOrigin,
) -> ApiResult<AcceptRenameResponse> {
	// Same credential path as apply-update: forwarded tokens (header) take
	// precedence; the keyring snapshot is pre-loaded once on the blocking
	// pool (`load_soft`, never on the async worker) and consulted in-memory
	// only on a forwarded miss (see `LazyKeyringFallback`). Also mutating, so
	// a forwarded miss WITH an unreachable backend fails closed (503) —
	// checked inside `accept_rename_inner`.
	let (snapshot, keyring_unavailable) = KeyringResolver::load_soft().await;
	let resolver = LazyKeyringFallback::new(
		forwarded.into_resolver(),
		snapshot,
		keyring_unavailable,
	);
	accept_rename_inner(body.into_inner(), &GitFetcher, &resolver).await
}

/// Thin adapter over the core rename transaction: validate the request, fetch
/// the source (the fetch cannot live in core — `skill-update` depends on core),
/// then hand the fetched tree to `rename::accept_rename` and map the outcome to
/// the response DTO.
pub(crate) async fn accept_rename_inner(
	req: AcceptRenameRequest,
	fetcher: &dyn Fetcher,
	resolver: &dyn TokenResolver,
) -> ApiResult<AcceptRenameResponse> {
	use aghub_core::skills::rename::{
		self, FetchedRename, RenameRequest, RenameScope,
	};

	// Adapter concern: confirmation gate.
	if !req.confirm.unwrap_or(false) {
		return Ok(Json(accept_rename_error(
			&req.old_name,
			&req.new_name,
			&req.scope,
			"confirm=true is required to accept a skill rename",
		)));
	}

	// Adapter concern: scope string -> RenameScope (illegal states rejected).
	let scope = match req.scope.as_str() {
		"global" => RenameScope::Global,
		"project" => match req.project_root.as_deref() {
			Some(root) => RenameScope::Project {
				root: PathBuf::from(root),
			},
			None => {
				return Ok(Json(accept_rename_error(
					&req.old_name,
					&req.new_name,
					&req.scope,
					"project_root is required when scope is project",
				)));
			}
		},
		_ => {
			return Ok(Json(accept_rename_error(
				&req.old_name,
				&req.new_name,
				&req.scope,
				"scope must be global or project",
			)));
		}
	};

	// P0-2 guard (a): refuse a degenerate rename before any lock read / fetch.
	if let Err(e) = rename::ensure_distinct_names(&req.old_name, &req.new_name)
	{
		return Ok(Json(accept_rename_error_with_code(
			&req.old_name,
			&req.new_name,
			&req.scope,
			&e.message(),
			e.code(),
		)));
	}

	// Step 1: read the OLD-name lock entry for the fetch coordinates.
	let source = match rename::rename_source_from_lock(&req.old_name, &scope) {
		Ok(s) => s,
		Err(e) => {
			return Ok(Json(accept_rename_error_with_code(
				&req.old_name,
				&req.new_name,
				&req.scope,
				&e.message(),
				e.code(),
			)));
		}
	};

	// Step 3: fetch (same credential path as apply-update). Resolve the token
	// against the fetch coordinate (`source_url`) so a non-github host binds to
	// the right keychain host. See the matching check in
	// `apply_skill_update_inner` — fail closed BEFORE the fetch if the
	// resolver reports its backend unreachable.
	let token = match resolver.resolve_checked(
		&source.source_url,
		keychain_host_for_source(&source.source_url).as_deref(),
	) {
		TokenResolution::Token(token) => Some(token),
		TokenResolution::NoToken => None,
		TokenResolution::BackendUnavailable => {
			return Err(crate::credentials::CredentialStoreError::Unavailable(
				"credential backend unreachable".to_string(),
			)
			.into());
		}
	};
	let folder =
		match skill_update::skill_folder_from_lock_path(&source.skill_path) {
			Some(f) => f,
			None => {
				return Ok(Json(accept_rename_error(
					&req.old_name,
					&req.new_name,
					&req.scope,
					"Locked skillPath is not a valid skill folder",
				)));
			}
		};
	let repo = match fetcher.fetch(
		&SourceRef {
			source: source.source_url.clone(),
			ref_: source.ref_name.clone(),
		},
		token.as_deref(),
		skill_update::FetchSelection::Skills(std::slice::from_ref(&folder)),
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

	// Steps 2/4/5/6/7/8/9 + P0 guards + rollback all live in core.
	match rename::accept_rename(
		RenameRequest {
			old_name: &req.old_name,
			new_name: &req.new_name,
			scope,
		},
		FetchedRename {
			repo_root: &repo.root,
			oid: repo.oid(),
			source: &source,
		},
	) {
		Ok(ok) => Ok(Json(AcceptRenameResponse {
			success: true,
			old_name: req.old_name,
			new_name: req.new_name,
			scope: req.scope,
			installed_hash: Some(ok.installed_hash),
			paths: ok.paths,
			error: None,
			code: None,
		})),
		Err(e) => Ok(Json(accept_rename_error_with_code(
			&req.old_name,
			&req.new_name,
			&req.scope,
			&e.message(),
			e.code(),
		))),
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use aghub_core::skills::lock::update_lock_hash;
	use aghub_core::skills::update::SkillUpdateStatus;
	use skill_update::EntryKey;

	/// Empty snapshot for tests that don't seed the keyring — they only need
	/// a [`TokenResolver`] to satisfy the type; `resolve()` always returns
	/// `None`. Bypasses `KeyringResolver::load()` (which needs an async
	/// context to `spawn_blocking`) since these are plain sync test helpers.
	fn empty_keyring_resolver() -> KeyringResolver {
		KeyringResolver {
			creds: Vec::new(),
			bindings: Default::default(),
		}
	}

	/// Stub resolver whose `resolve_checked` always reports
	/// `BackendUnavailable`, with no real keyring involved -- used to assert
	/// the fail-closed 503 path directly against `resolve_checked`'s
	/// dedicated enum variant (as opposed to forcing the real backend
	/// unreachable via the OS-level test hook, as the route-level regression
	/// test below does).
	// Only constructed by
	// `apply_skill_update_inner_fails_closed_on_backend_unavailable` below,
	// which is `#[cfg(unix)]` -- match that exactly so Windows clippy doesn't
	// see this as dead code under -D warnings.
	#[cfg(unix)]
	struct StubBackendUnavailableResolver;
	#[cfg(unix)]
	impl TokenResolver for StubBackendUnavailableResolver {
		fn resolve(
			&self,
			_source: &str,
			_host: Option<&str>,
		) -> Option<String> {
			None
		}

		fn resolve_checked(
			&self,
			_source: &str,
			_host: Option<&str>,
		) -> TokenResolution {
			TokenResolution::BackendUnavailable
		}
	}

	/// Fetcher that panics if invoked -- proves the fail-closed 503 check
	/// runs BEFORE any fetch is attempted.
	#[cfg(unix)]
	struct PanicOnFetch;
	#[cfg(unix)]
	impl Fetcher for PanicOnFetch {
		fn fetch(
			&self,
			_source_ref: &SourceRef,
			_token: Option<&str>,
			_selection: skill_update::FetchSelection<'_>,
		) -> Result<skill_update::FetchedRepo, FetchError> {
			panic!(
				"fetch must not be attempted when resolve_checked reports \
				 BackendUnavailable"
			);
		}
	}

	/// Regression coverage for F2: `apply_skill_update_inner` must fail
	/// closed (503 `KEYCHAIN_UNAVAILABLE`) BEFORE any fetch is attempted when
	/// the injected resolver's `resolve_checked` reports
	/// `BackendUnavailable` -- exercised directly against a stub, so this
	/// fails on a regression without needing a real (im)possible keyring
	/// state.
	#[cfg(unix)]
	#[test]
	fn apply_skill_update_inner_fails_closed_on_backend_unavailable() {
		with_isolated_state(|| {
			let home = tempfile::tempdir().unwrap();
			let installed_dir = home.path().join(".claude/skills/some-skill");
			std::fs::create_dir_all(&installed_dir).unwrap();
			std::fs::write(
				installed_dir.join("SKILL.md"),
				"---\nname: some-skill\ndescription: original\n---\nbody\n",
			)
			.unwrap();
			let old_home = std::env::var("HOME").ok();
			std::env::set_var("HOME", home.path());

			let mut lock = skill::SkillLockFile::default();
			let mut entry = global_entry();
			entry.skill_path = Some("SKILL.md".to_string());
			lock.skills.insert("some-skill".into(), entry);
			skill::lock::global::write_skill_lock(&lock).unwrap();

			let req = ApplySkillUpdateRequest {
				name: "some-skill".to_string(),
				scope: "global".to_string(),
				project_root: None,
				confirm: Some(true),
			};

			let resolver = StubBackendUnavailableResolver;
			let result = rocket::tokio::runtime::Builder::new_current_thread()
				.enable_all()
				.build()
				.unwrap()
				.block_on(apply_skill_update_inner(
					req,
					&PanicOnFetch,
					&resolver,
				));

			match old_home {
				Some(value) => std::env::set_var("HOME", value),
				None => std::env::remove_var("HOME"),
			}

			let Err(error) = result else {
				panic!(
					"expected a 503 when resolve_checked reports \
					 BackendUnavailable"
				);
			};
			assert_eq!(error.status, rocket::http::Status::ServiceUnavailable);
			assert_eq!(error.body.code, "KEYCHAIN_UNAVAILABLE");
		});
	}

	/// Regression (GitHub #15 P2-3, Codex-found): `apply_skill_update` used
	/// to load its keyring snapshot via `KeyringResolver::load`, which
	/// degrades ANY read failure -- including "the backend itself is
	/// unreachable" -- to an empty snapshot. For this MUTATING route that
	/// meant a keyring outage silently resolved "no credential" and the
	/// request went on to fail later with a confusing error instead of a
	/// stable, retryable 503.
	///
	/// Forces the backend-unavailable path via
	/// `crate::credentials::test_hooks::ForceCredentialBackendUnavailable`
	/// (deterministic, cross-platform -- see its doc comment) instead of the
	/// previous `DBUS_SESSION_BUS_ADDRESS` tampering: that env var only
	/// affects Linux secret-service, so on a macOS/Windows CI runner it did
	/// nothing and this test would silently observe a non-503 result
	/// (GitHub #15 round-2 Codex finding, P1-1-adjacent).
	///
	/// A real lock entry + installed copy for `some-skill` is required: the
	/// keyring fallback (`LazyKeyringFallback`, see its doc comment) is only
	/// ever consulted once `apply_skill_update_inner` actually reaches its
	/// `resolver.resolve(...)` call — which requires a real, locked,
	/// installed skill to get past the earlier "not installed"/"no lock
	/// entry" short-circuits. (The keyring READ is eager/off-worker via
	/// `load_soft`; only the in-memory `resolve()` lookup is gated here.) Dispatches a real HTTP
	/// request through the mounted route (not `apply_skill_update_inner`
	/// directly, which bypasses this exact code path). No forwarded-token
	/// header is sent, so the forwarded resolver misses and the keyring
	/// fallback IS consulted, and must reject before any fetch is attempted
	/// (see
	/// `apply_update_forwarded_token_succeeds_even_when_keyring_backend_unreachable`
	/// for the complementary case where forwarding covers the source and the
	/// keyring must never even be touched).
	#[cfg(unix)]
	#[test]
	fn apply_skill_update_route_fails_closed_when_keyring_backend_unreachable()
	{
		with_isolated_state(|| {
			let _unavailable = crate::credentials::test_hooks::
				ForceCredentialBackendUnavailable::new();

			let home = tempfile::tempdir().unwrap();
			let installed_dir = home.path().join(".claude/skills/some-skill");
			std::fs::create_dir_all(&installed_dir).unwrap();
			std::fs::write(
				installed_dir.join("SKILL.md"),
				"---\nname: some-skill\ndescription: original\n---\nbody\n",
			)
			.unwrap();
			let old_home = std::env::var("HOME").ok();
			std::env::set_var("HOME", home.path());

			// Locked source resolves to https://github.com/owner/repo -- no
			// forwarded header will be sent for it, so resolution falls
			// through to the (forced-unreachable) keyring.
			let mut lock = skill::SkillLockFile::default();
			let mut entry = global_entry();
			entry.skill_path = Some("SKILL.md".to_string());
			lock.skills.insert("some-skill".into(), entry);
			skill::lock::global::write_skill_lock(&lock).unwrap();

			let app_data = tempfile::tempdir().unwrap();
			let client =
				rocket::local::blocking::Client::tracked(crate::build_rocket(
					rocket::Config::default(),
					app_data.path().to_path_buf(),
				))
				.expect("client");

			let response = client
				.post("/api/v1/skills/apply-update")
				.json(&serde_json::json!({
					"name": "some-skill",
					"scope": "global",
					"confirm": true,
				}))
				.dispatch();

			match old_home {
				Some(value) => std::env::set_var("HOME", value),
				None => std::env::remove_var("HOME"),
			}

			assert_eq!(
				response.status(),
				rocket::http::Status::ServiceUnavailable,
				"an unreachable keyring backend must fail closed with 503"
			);
			let raw = response.into_string().expect("response body");
			let parsed: serde_json::Value =
				serde_json::from_str(&raw).expect("json body");
			assert_eq!(parsed["code"], "KEYCHAIN_UNAVAILABLE");
		});
	}

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
		let resolver = empty_keyring_resolver();
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
		let resolver = empty_keyring_resolver();
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
		let resolver = empty_keyring_resolver();
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
		let resolver = empty_keyring_resolver();
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
			_selection: skill_update::FetchSelection<'_>,
		) -> Result<skill_update::FetchedRepo, FetchError> {
			Ok(skill_update::FetchedRepo {
				root: self.root.clone(),
				snapshot: aghub_git::RepoSnapshot {
					commit_oid: String::new(),
					tree_oid: "test-tree-oid".to_string(),
					commit_time: None,
				},
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

			let resolver = empty_keyring_resolver();
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
			_selection: skill_update::FetchSelection<'_>,
		) -> Result<skill_update::FetchedRepo, FetchError> {
			*self.seen_token.lock().unwrap() = Some(token.map(str::to_string));
			Ok(skill_update::FetchedRepo {
				root: self.root.clone(),
				snapshot: aghub_git::RepoSnapshot {
					commit_oid: String::new(),
					tree_oid: "test-tree-oid".to_string(),
					commit_time: None,
				},
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
			let keyring = empty_keyring_resolver();
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

	/// Regression (GitHub #15 round-2 Codex finding): both mutating routes
	/// used to build their resolver as `KeyringResolver::load_or_unavailable()
	/// .await?` — an eager, fail-closed keyring load — BEFORE ever
	/// constructing the `ChainResolver` that tries the forwarded map first.
	/// That meant an unreachable keyring 503'd the request UNCONDITIONALLY,
	/// even when the forwarded header already covered the requested source
	/// — defeating the entire purpose of forwarding for a headless remote
	/// (no keyring of its own). Uses `LazyKeyringFallback` directly — the
	/// SAME resolver type the production route handler constructs — with the
	/// credential backend forced unreachable via the cross-platform
	/// injection hook (never DBUS). Must succeed using the forwarded token;
	/// before the fix this 503s instead.
	#[cfg(unix)]
	#[test]
	fn apply_update_forwarded_token_succeeds_even_when_keyring_backend_unreachable(
	) {
		use crate::credentials::forwarding::{ForwardedEntry, ForwardedOrigin};
		with_isolated_state(|| {
			// Process-global (crosses any blocking-pool thread boundary);
			// safe here because `with_isolated_state` already holds
			// `test_env_lock` for this whole closure.
			let _unavailable = crate::credentials::test_hooks::
				ForceCredentialBackendUnavailable::new();

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

			// Fetched repo keeps the SAME name so the rename guard passes.
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

			// Forwarded header carries a github.com-pinned token that covers
			// the locked source.
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
			// Simulate an UNREACHABLE local keyring: empty snapshot +
			// `keyring_unavailable = true`. The forwarded hit must still
			// succeed and never 503 (GitHub #15 round-2 regression); round-3
			// keeps the keyring read off the async worker via `load_soft`, so
			// this constructs the already-loaded state directly.
			let resolver = LazyKeyringFallback::new(
				forwarded.into_resolver(),
				KeyringResolver {
					creds: Vec::new(),
					bindings: Default::default(),
				},
				true,
			);

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
				Err(error) => panic!(
					"apply must succeed using the forwarded token, not \
					 503 just because the (irrelevant, since forwarding \
					 covers the source) keyring backend is unreachable: {}",
					error.body.error
				),
			};
			assert!(resp.success, "apply should succeed: {:?}", resp.error);
			let seen = fetcher.seen_token.lock().unwrap().clone();
			assert_eq!(
				seen,
				Some(Some("FWD-TOKEN".to_string())),
				"the forwarded token must reach the apply fetch even though \
				 the keyring backend is unreachable"
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
			let resolver = empty_keyring_resolver();
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
	/// with the machine code. Adapter-level: the route calls
	/// `rename::ensure_distinct_names` before it reads the lock or fetches.
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
			assert_eq!(
				resp.code.as_deref(),
				Some(aghub_core::skills::rename::RENAME_TARGET_EXISTS_CODE)
			);
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
			assert_eq!(
				resp.code.as_deref(),
				Some(aghub_core::skills::rename::RENAME_TARGET_EXISTS_CODE)
			);
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
