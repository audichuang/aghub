//! `GET /skills/check-updates` — read-only update check for installed skills.
//!
//! Reads the global skill lock, projects each entry to the orchestrator's
//! [`EntryInput`], then delegates to [`skill_update::run_update_check`].
//!
//! Network + credential resolution stay in this crate (never in `crates/core`).
//! The [`Fetcher`] materializes a worktree into a [`tempfile::TempDir`] (the
//! documented worst-case fallback — a checkout into a temp dir, never the `git`
//! binary), and the [`TokenResolver`] wraps the keyring/keychain resolution.
//! Every gix error string is redacted of URL userinfo upstream so a token can
//! never leak into the response.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use aghub_core::models::ResourceScope;
use rocket::http::Status;
use rocket::serde::json::Json;

use crate::credentials::forwarding::ForwardedGitTokens;
use crate::credentials::source_auth::SourceAuth;
use crate::dto::skill::{
	AcceptRenameRequest, AcceptRenameResponse, ApplySkillUpdateRequest,
	ApplySkillUpdateResponse, ApplySkillUpdatesRequest,
	ApplySkillUpdatesResponse, CheckSkillUpdatesResponse,
	SkillHealErrorResponse, SkillUpdateResponse, SkillUpdateStatusResponse,
};
use crate::error::{ApiError, ApiResult};
use crate::extractors::{ResolvedScope, ScopeParams, TrustedLocalOrigin};
use crate::skills::rename::skill_renamed_message;
use crate::skills::resync::safe_resync_error;
use skill_update::mutation::{
	rename_locked_skill, resync_locked_skill, resync_locked_skills,
	LockedRenameRequest, LockedResyncError, LockedResyncRequest,
	LockedSkillsResyncRequest, RenameMutationError,
};
use skill_update::projection::{self, Identities};
// Only the `#[cfg(unix)]` StubBackendUnavailableResolver test uses this —
// match its gate exactly or Windows clippy flags an unused import.
#[cfg(all(test, unix))]
use skill_update::TokenResolution;
use skill_update::{
	run_update_check, EntryInput, FetchError, Fetcher, GitFetcher, RefResolver,
	TokenResolver,
};

/// Upper bound on one batch's `names`. Defined ONCE, in `dto::limits`, and
/// generated into the desktop's `generated/dto/limits.ts` — `source-detail.tsx`
/// chunks to it, and hand-copying the number here would let the two drift.
use crate::dto::limits::MAX_BATCH_NAMES;
pub use skill_update::mutation::SKILL_SOURCE_VIEW_STALE_CODE;

/// Query parameters for the update check. `offline` short-circuits every entry
/// to `Uncheckable { network }` without touching the network (useful for tests
/// and air-gapped environments).
#[derive(rocket::FromForm)]
pub struct CheckUpdatesParams {
	offline: Option<bool>,
	scope: Option<String>,
	project_root: Option<String>,
}

/// The global half of a check's pre-fetch read, bound to this surface's
/// policy: the lock is read FAIL-OPEN here (an unreadable global lock must not
/// take the route down), unlike the CLI which probes it fail-closed first.
/// Everything else — read order, the `wanted` filter, the offline skip — lives
/// in `skill_update::projection`.
fn global_lock_entries(offline: bool) -> (Vec<EntryInput>, Identities) {
	projection::global_lock_entries(
		offline,
		skill::lock::global::read_skill_lock,
	)
}

/// [`global_lock_entries`] for the project lock.
fn project_lock_entries(
	project_root: Option<&Path>,
	offline: bool,
) -> (Vec<EntryInput>, Identities) {
	projection::project_lock_entries(offline, project_root, || {
		skill::lock::local::read_local_lock(project_root)
	})
}

/// Everything one check read BEFORE fetching: the orchestrator inputs, the write
/// scope, and the pre-fetch identities its heal writer must compare against.
struct CheckInputs {
	entries: Vec<EntryInput>,
	project_root: Option<PathBuf>,
	global_identities: Identities,
	project_identities: Identities,
}

fn lock_entries_for_scope(
	scope: &ResolvedScope,
	offline: bool,
) -> Result<CheckInputs, ApiError> {
	let mut inputs = CheckInputs {
		entries: Vec::new(),
		project_root: None,
		global_identities: Identities::new(),
		project_identities: Identities::new(),
	};
	match scope {
		ResolvedScope::Global => {
			let (entries, identities) = global_lock_entries(offline);
			inputs.entries = entries;
			inputs.global_identities = identities;
		}
		ResolvedScope::Project { root } => {
			let (entries, identities) =
				project_lock_entries(Some(root), offline);
			inputs.entries = entries;
			inputs.project_identities = identities;
			inputs.project_root = Some(root.clone());
		}
		ResolvedScope::All { project_root } => {
			let (entries, identities) = global_lock_entries(offline);
			inputs.entries = entries;
			inputs.global_identities = identities;
			if let Some(root) = project_root {
				let (entries, identities) =
					project_lock_entries(Some(root), offline);
				inputs.entries.extend(entries);
				inputs.project_identities = identities;
			}
			inputs.project_root = project_root.clone();
		}
	}
	Ok(inputs)
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

fn fetch_error_text(error: &FetchError) -> &'static str {
	match error {
		FetchError::Auth => "Authentication failed while fetching source",
		// Deliberately drops `FetchError::Network`'s detail: it can name an
		// internal temp path, which API errors must never disclose.
		FetchError::Network(_) => "Failed to fetch source repository",
		FetchError::BackendUnavailable => "Credential backend unavailable",
	}
}

fn apply_locked_resync_error(
	name: &str,
	scope: &str,
	error: &LockedResyncError,
) -> Result<ApplySkillUpdateResponse, ApiError> {
	let message = match error {
		LockedResyncError::Fetch(FetchError::BackendUnavailable) => {
			return Err(crate::credentials::CredentialStoreError::Unavailable(
				"credential backend unreachable".to_string(),
			)
			.into());
		}
		LockedResyncError::LockEntryNotFound {
			scope: locked_scope,
		} => {
			if *locked_scope == ResourceScope::GlobalOnly {
				"Skill is not in global lock".to_string()
			} else {
				"Skill is not in project lock".to_string()
			}
		}
		LockedResyncError::MissingSkillPath => {
			"Locked skill has no skillPath".to_string()
		}
		LockedResyncError::NotInstalled => {
			"Skill is locked but no installed copy was found".to_string()
		}
		LockedResyncError::InvalidSkillPath => {
			"Locked skillPath is not a valid skill folder".to_string()
		}
		LockedResyncError::SourceSkillNotFound => {
			"Locked skillPath was not found in fetched source".to_string()
		}
		LockedResyncError::SourceGroupMismatch => {
			"Skill source changed; refresh Sources and retry".to_string()
		}
		LockedResyncError::Fetch(error) => fetch_error_text(error).to_string(),
		LockedResyncError::Resync(
			aghub_core::skills::resync::ResyncError::Renamed { new_name },
		) => skill_renamed_message(name, new_name),
		LockedResyncError::Resync(resync_err) => {
			safe_resync_error(resync_err).message.to_string()
		}
	};
	Ok(apply_error_with_code(
		name,
		scope,
		&message,
		Some(error.code()),
	))
}

fn apply_success(
	name: String,
	scope: &str,
	report: aghub_core::skills::resync::ResyncReport,
) -> ApplySkillUpdateResponse {
	ApplySkillUpdateResponse {
		success: true,
		name,
		scope: scope.to_string(),
		updated_hash: Some(report.updated_hash),
		paths: report
			.swapped
			.iter()
			.map(|path| path.display().to_string())
			.collect(),
		error: None,
		code: None,
	}
}

fn apply_locked_resync_outcome(
	name: String,
	scope: &str,
	outcome: Result<
		aghub_core::skills::resync::ResyncReport,
		LockedResyncError,
	>,
) -> Result<ApplySkillUpdateResponse, ApiError> {
	match outcome {
		Ok(report) => Ok(apply_success(name, scope, report)),
		Err(error) => apply_locked_resync_error(&name, scope, &error),
	}
}

/// A batch row NEVER escalates to a top-level API error: one row hitting the
/// keyring must not erase every other row's attribution.
fn apply_locked_resync_batch_outcome(
	name: String,
	scope: &str,
	outcome: Result<
		aghub_core::skills::resync::ResyncReport,
		LockedResyncError,
	>,
) -> ApplySkillUpdateResponse {
	match outcome {
		Ok(report) => apply_success(name, scope, report),
		Err(error) => apply_locked_resync_batch_error(&name, scope, &error),
	}
}

fn apply_locked_resync_batch_error(
	name: &str,
	scope: &str,
	error: &LockedResyncError,
) -> ApplySkillUpdateResponse {
	if matches!(
		error,
		LockedResyncError::Fetch(FetchError::BackendUnavailable)
	) {
		return apply_error_with_code(
			name,
			scope,
			"Credential backend unavailable",
			Some(error.code()),
		);
	}
	match apply_locked_resync_error(name, scope, error) {
		Ok(response) => response,
		// Only the credential-backend arm returns `Err` (handled above). Any
		// future one is a wiring mistake: loud in debug, an attributed row in
		// release — never a 500 that erases the batch.
		Err(_) => {
			debug_assert!(
				false,
				"a new LockedResyncError arm projects to a top-level API \
				 error; give it a batch row projection"
			);
			apply_error(name, scope, "Skill update failed")
		}
	}
}

/// `GET /skills/check-updates` — returns the per-skill update status list, plus
/// `healError` when the post-check lock auto-heal did not land.
#[get("/skills/check-updates?<query..>")]
pub async fn check_skill_updates(
	query: CheckUpdatesParams,
	forwarded: ForwardedGitTokens,
	_origin: TrustedLocalOrigin,
) -> ApiResult<CheckSkillUpdatesResponse> {
	let resolved = ScopeParams {
		scope: query.scope.clone(),
		project_root: query.project_root.clone(),
	}
	.resolve()?;
	let offline = query.offline.unwrap_or(false);
	// One repository behind both: the preflight's tip resolution and the fetch
	// that may follow it share the composite, its snapshot memo, and its token
	// context.
	let git_fetcher = GitFetcher::new();
	let ref_resolver: Arc<dyn RefResolver> =
		Arc::new(git_fetcher.ref_resolver());
	let fetcher: Arc<dyn Fetcher> = Arc::new(git_fetcher);
	let auth_started = std::time::Instant::now();
	let resolver = SourceAuth::load(forwarded).await;
	log::info!(
		"check-updates: credential resolve took={:?}",
		auth_started.elapsed()
	);
	check_skill_updates_inner(
		resolved,
		offline,
		fetcher,
		ref_resolver,
		&resolver,
	)
	.await
}

/// The check itself, with the token resolver injected so a test can drive it
/// without the request guards.
pub(crate) async fn check_skill_updates_inner(
	resolved: ResolvedScope,
	offline: bool,
	fetcher: Arc<dyn Fetcher>,
	ref_resolver: Arc<dyn RefResolver>,
	resolver: &dyn TokenResolver,
) -> ApiResult<CheckSkillUpdatesResponse> {
	// Log the resolved `offline`: the two modes differ by orders of magnitude
	// and the query string alone does not say which one ran.
	let route_started = std::time::Instant::now();
	let inputs_started = std::time::Instant::now();
	let CheckInputs {
		entries,
		project_root,
		global_identities,
		project_identities,
	} = lock_entries_for_scope(&resolved, offline)?;
	log::info!(
		"check-updates: offline={offline} entries={} inputs took={:?}",
		entries.len(),
		inputs_started.elapsed()
	);

	let check_started = std::time::Instant::now();
	let outputs =
		run_update_check(entries, fetcher, ref_resolver, resolver, offline)
			.await;
	log::info!(
		"check-updates: fetch+compare results={} took={:?}",
		outputs.len(),
		check_started.elapsed()
	);

	// The route only decides THAT it heals; the write and its precondition live
	// in skill_update::projection. Writes the lock under the mutation lock, so
	// off the async worker (`crates/api/AGENTS.md`). A failed heal must not
	// discard the computed results, so its error is reported beside them.
	let heal_error = crate::blocking::in_mutation_pool(|| {
		Ok(projection::write_auto_healed_hashes(
			&outputs,
			project_root.as_deref(),
			&global_identities,
			&project_identities,
		)
		.err())
	})
	.await?
	.map(|e| {
		log::warn!("check-updates: auto-heal failed: {e}");
		SkillHealErrorResponse {
			code: aghub_core::error_codes::wire_code(&e).to_string(),
			retryable: aghub_core::error_codes::retryable(&e),
		}
	});

	let mut results: Vec<SkillUpdateResponse> = outputs
		.into_iter()
		.map(|output| SkillUpdateResponse {
			name: output.key.name,
			scope: output.key.scope,
			status: SkillUpdateStatusResponse::from(output.status),
		})
		.collect();
	results.sort_by(|a, b| a.scope.cmp(&b.scope).then(a.name.cmp(&b.name)));

	log::info!(
		"check-updates: done offline={offline} results={} total={:?}",
		results.len(),
		route_started.elapsed()
	);
	Ok(Json(CheckSkillUpdatesResponse {
		results,
		heal_error,
	}))
}

/// `POST /skills/apply-update` — re-fetch a locked skill and replace installs.
#[post("/skills/apply-update", data = "<body>")]
pub async fn apply_skill_update(
	body: Json<ApplySkillUpdateRequest>,
	forwarded: ForwardedGitTokens,
	_origin: TrustedLocalOrigin,
) -> ApiResult<ApplySkillUpdateResponse> {
	let resolver = SourceAuth::load(forwarded).await;
	apply_skill_update_inner(body.into_inner(), &GitFetcher::new(), &resolver)
		.await
}

/// Inner apply path that takes an injected [`Fetcher`] + [`TokenResolver`] so
/// the rename guard (and the rest of the happy-path wiring) is unit-testable
/// without a real network. The route handler is a thin shim that supplies
/// [`GitFetcher`] + the request-scoped [`SourceAuth`].
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

	let write_scope = match crate::extractors::resolve_write_scope(
		&req.scope,
		req.project_root.as_deref(),
	) {
		Ok(scope) => scope,
		Err(err) => {
			return Ok(Json(apply_error_with_code(
				&req.name,
				&req.scope,
				&err.body.error,
				Some(err.body.code),
			)));
		}
	};

	// `resync_locked_skill` is synchronous but does BOTH the network fetch and the
	// lock-holding transaction, so it must not run on an async worker.
	let name = req.name;
	let scope = req.scope;
	crate::blocking::in_mutation_pool(|| {
		let outcome = resync_locked_skill(
			LockedResyncRequest {
				name: &name,
				scope: write_scope,
			},
			fetcher,
			resolver,
		);
		apply_locked_resync_outcome(name, &scope, outcome)
			.inspect(log_failed_row)
			.map(Json)
	})
	.await
}

/// A failed row is still HTTP 200, so without this the log cannot tell a
/// write from a refusal. The message is the redacted client text, safe to log.
fn log_failed_row(row: &ApplySkillUpdateResponse) {
	if !row.success {
		log::warn!(
			"apply-update failed: skill={} code={:?} error={:?}",
			row.name,
			row.code,
			row.error
		);
	}
}

/// `POST /skills/apply-updates` — update several locked skills from Sources.
#[post("/skills/apply-updates", data = "<body>")]
pub async fn apply_skill_updates(
	_origin: TrustedLocalOrigin,
	body: Json<ApplySkillUpdatesRequest>,
	forwarded: ForwardedGitTokens,
) -> ApiResult<ApplySkillUpdatesResponse> {
	let resolver = SourceAuth::load(forwarded).await;
	apply_skill_updates_inner(body.into_inner(), &GitFetcher::new(), &resolver)
		.await
}

pub(crate) async fn apply_skill_updates_inner(
	req: ApplySkillUpdatesRequest,
	fetcher: &dyn Fetcher,
	resolver: &dyn TokenResolver,
) -> ApiResult<ApplySkillUpdatesResponse> {
	if !req.confirm.unwrap_or(false) {
		return Err(ApiError::new(
			Status::BadRequest,
			"confirm=true is required to overwrite installed skill files",
			"INVALID_PARAM",
		));
	}
	// An empty list is answered by the seam's own `EmptyRequest` (projected
	// below) — one written contract, not two. The CAP is the route's own job:
	// the seam cannot know how long a caller may occupy a mutation worker.
	if req.names.len() > MAX_BATCH_NAMES {
		return Err(ApiError::new(
			Status::BadRequest,
			format!("names must not exceed {MAX_BATCH_NAMES} per batch"),
			"INVALID_PARAM",
		));
	}

	let write_scope = crate::extractors::resolve_write_scope(
		&req.scope,
		req.project_root.as_deref(),
	)?;

	let names = req.names;
	let scope = req.scope;
	crate::blocking::in_mutation_pool(|| {
		// Every per-skill failure — unresolvable entry, repointed Source, its
		// group's fetch — comes back as its own ordered row, so one bad skill
		// never costs the others their update. Only a request that cannot
		// produce rows at all is an API-level error.
		let outcomes = resync_locked_skills(
			LockedSkillsResyncRequest {
				source_group: Some(&req.source),
				names: &names,
				scope: write_scope,
			},
			fetcher,
			resolver,
		)
		// Unreachable in practice (empty names and bad scopes are answered
		// earlier); ONE generic arm, not a second copy of the extractor's
		// messages.
		.map_err(|_| {
			ApiError::new(
				Status::BadRequest,
				"scope must be global or project, with a non-empty names list",
				"INVALID_PARAM",
			)
		})?;
		let results = outcomes
			.into_iter()
			.map(|item| {
				apply_locked_resync_batch_outcome(
					item.name,
					&scope,
					item.outcome,
				)
			})
			.inspect(log_failed_row)
			.collect();
		Ok(Json(ApplySkillUpdatesResponse { results }))
	})
	.await
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
/// the old name, transition both lock entries. The plan, fetch and transaction
/// are owned by `skill_update::mutation::rename_locked_skill`; this route just
/// wires credentials.
#[post("/skills/accept-rename", data = "<body>")]
pub async fn accept_skill_rename(
	body: Json<AcceptRenameRequest>,
	forwarded: ForwardedGitTokens,
	_origin: TrustedLocalOrigin,
) -> ApiResult<AcceptRenameResponse> {
	let resolver = SourceAuth::load(forwarded).await;
	accept_rename_inner(body.into_inner(), &GitFetcher::new(), &resolver).await
}

/// Thin adapter over `skill_update::mutation::rename_locked_skill`: validate
/// the request, run the shared entry on the mutation pool, and map its outcome
/// to the response DTO. The plan, fetch and transaction live in that entry.
pub(crate) async fn accept_rename_inner(
	req: AcceptRenameRequest,
	fetcher: &dyn Fetcher,
	resolver: &dyn TokenResolver,
) -> ApiResult<AcceptRenameResponse> {
	// Adapter concern: confirmation gate.
	if !req.confirm.unwrap_or(false) {
		return Ok(Json(accept_rename_error(
			&req.old_name,
			&req.new_name,
			&req.scope,
			"confirm=true is required to accept a skill rename",
		)));
	}

	// Adapter concern: scope string -> WriteScope (illegal states rejected).
	let scope = match crate::extractors::resolve_write_scope(
		&req.scope,
		req.project_root.as_deref(),
	) {
		Ok(scope) => scope,
		Err(err) => {
			return Ok(Json(accept_rename_error(
				&req.old_name,
				&req.new_name,
				&req.scope,
				&err.body.error,
			)));
		}
	};

	// The entry fetches (blocking network) AND runs the lock-holding transaction,
	// so the WHOLE call belongs on the mutation pool, never on the async worker (#19).
	crate::blocking::in_mutation_pool(|| {
		match rename_locked_skill(
			LockedRenameRequest {
				old_name: &req.old_name,
				new_name: &req.new_name,
				scope,
				git_ref: None,
			},
			fetcher,
			resolver,
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
			Err(RenameMutationError::Fetch(FetchError::BackendUnavailable)) => {
				Err(crate::credentials::CredentialStoreError::Unavailable(
					"credential backend unreachable".to_string(),
				)
				.into())
			}
			Err(error) => {
				let message = match &error {
					RenameMutationError::Rename(e) => e.message(),
					RenameMutationError::Fetch(e) => {
						fetch_error_text(e).to_string()
					}
					RenameMutationError::CatalogScan => {
						"Fetched source catalog could not be scanned safely"
							.to_string()
					}
					RenameMutationError::SkillNotFound => {
						"New skill name was not found in the fetched source"
							.to_string()
					}
				};
				Ok(Json(accept_rename_error_with_code(
					&req.old_name,
					&req.new_name,
					&req.scope,
					&message,
					error.code(),
				)))
			}
		}
	})
	.await
}

#[cfg(test)]
mod tests {
	use super::*;
	use aghub_core::skills::lock::update_lock_hash;
	use skill_update::SourceRef;

	/// Empty source-auth snapshot for synchronous route-core tests.
	fn empty_keyring_resolver() -> SourceAuth {
		SourceAuth::for_test(ForwardedGitTokens::default(), false)
	}

	/// Stub resolver whose `resolve` always reports
	/// `BackendUnavailable`, with no real keyring involved -- used to assert
	/// the fail-closed 503 path directly against `resolve`'s
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
		fn resolve(&self, _source: &str) -> TokenResolution {
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
			// Shared by several tests (backend-unavailable fail-closed, the
			// confirm gate, request validation), so keep the message about
			// the stub's contract rather than one caller's scenario.
			panic!("fetch must not be attempted");
		}
	}

	/// Restores `HOME` on drop, including during a panic. A test that restores
	/// it manually AFTER its assertions leaks a deleted tempdir HOME into the
	/// rest of the binary the moment it actually catches a regression — which
	/// buries the signal under unrelated failures.
	#[cfg(unix)]
	struct HomeGuard(Option<String>);

	#[cfg(unix)]
	impl HomeGuard {
		fn set(home: &Path) -> Self {
			let previous = std::env::var("HOME").ok();
			std::env::set_var("HOME", home);
			Self(previous)
		}
	}

	#[cfg(unix)]
	impl Drop for HomeGuard {
		fn drop(&mut self) {
			match self.0.take() {
				Some(value) => std::env::set_var("HOME", value),
				None => std::env::remove_var("HOME"),
			}
		}
	}

	#[cfg(unix)]
	struct CountingFetcher {
		root: PathBuf,
		calls: std::sync::atomic::AtomicUsize,
	}

	#[cfg(unix)]
	impl Fetcher for CountingFetcher {
		fn fetch(
			&self,
			_source_ref: &SourceRef,
			_token: Option<&str>,
			_selection: skill_update::FetchSelection<'_>,
		) -> Result<skill_update::FetchedRepo, FetchError> {
			self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
			Ok(skill_update::FetchedRepo {
				root: self.root.clone(),
				snapshot: aghub_git::RepoSnapshot {
					commit_oid: "batch-commit".to_string(),
					tree_oid: "batch-tree".to_string(),
					commit_time: None,
				},
				_guard: None,
			})
		}
	}

	#[cfg(unix)]
	fn prepare_global_batch(home: &Path) {
		prepare_global_batch_names(home, ["alpha", "beta"]);
	}

	#[cfg(unix)]
	fn prepare_global_batch_names(
		home: &Path,
		names: impl IntoIterator<Item = impl AsRef<str>>,
	) {
		let mut lock = skill::SkillLockFile::default();
		for name in names {
			let name = name.as_ref();
			let installed = home.join(format!(".claude/skills/{name}"));
			std::fs::create_dir_all(&installed).unwrap();
			std::fs::write(
				installed.join("SKILL.md"),
				format!("---\nname: {name}\ndescription: old\n---\nold\n"),
			)
			.unwrap();
			let mut entry = global_entry();
			entry.skill_path = Some(format!("skills/{name}/SKILL.md"));
			lock.skills.insert(name.to_string(), entry);
		}
		skill::lock::global::write_skill_lock(&lock).unwrap();
	}

	/// Regression coverage for F2: `apply_skill_update_inner` must fail
	/// closed (503 `KEYCHAIN_UNAVAILABLE`) BEFORE any fetch is attempted when
	/// the injected resolver's `resolve` reports
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
					"expected a 503 when resolve reports \
					 BackendUnavailable"
				);
			};
			assert_eq!(error.status, rocket::http::Status::ServiceUnavailable);
			assert_eq!(error.body.code, "KEYCHAIN_UNAVAILABLE");
		});
	}

	/// "Locked but not installed" is reachable from two arms — the batch's
	/// advisory pre-fetch check and the transaction's authoritative one — and the
	/// two say the same thing in prose. Only the machine code tells a client they
	/// are the same condition, so a client that branches on it must not depend on
	/// which arm happened to fire. Folding one arm into the other silently
	/// dropped the code once: the message was identical, so nothing looked wrong.
	#[test]
	fn both_not_installed_arms_carry_one_wire_code() {
		let expected_message = crate::skills::resync::safe_resync_error(
			&aghub_core::skills::resync::ResyncError::NotInstalled,
		)
		.message;
		let expected_code =
			aghub_core::skills::resync::ResyncError::NotInstalled.code();
		for error in [
			LockedResyncError::NotInstalled,
			LockedResyncError::Resync(
				aghub_core::skills::resync::ResyncError::NotInstalled,
			),
		] {
			let Ok(response) =
				apply_locked_resync_error("alpha", "global", &error)
			else {
				panic!("a not-installed row is a row, not a request failure");
			};
			assert!(!response.success);
			assert_eq!(response.error.as_deref(), Some(expected_message));
			assert_eq!(
				response.code.as_deref(),
				Some(expected_code),
				"{error:?} must be machine-distinguishable"
			);
		}
	}

	#[cfg(unix)]
	#[test]
	fn apply_skill_updates_rejects_257_installed_names_before_fetching() {
		with_isolated_state(|| {
			let home = tempfile::tempdir().unwrap();
			let _home = HomeGuard::set(home.path());
			let names = (0..257)
				.map(|index| format!("skill-{index}"))
				.collect::<Vec<_>>();
			prepare_global_batch_names(home.path(), &names);
			let runtime = rocket::tokio::runtime::Builder::new_current_thread()
				.enable_all()
				.build()
				.unwrap();

			let error = runtime
				.block_on(apply_skill_updates_inner(
					ApplySkillUpdatesRequest {
						source: "https://github.com/owner/repo".to_string(),
						names,
						scope: "global".to_string(),
						project_root: None,
						confirm: Some(true),
					},
					&PanicOnFetch,
					&empty_keyring_resolver(),
				))
				.expect_err(
					"257 installed skills must exceed the safe batch cap",
				);

			assert_eq!(error.status, Status::BadRequest);
			assert_eq!(error.body.code, "INVALID_PARAM");
			assert_eq!(error.body.error, "names must not exceed 256 per batch");
		});
	}

	#[cfg(unix)]
	#[test]
	fn apply_skill_updates_fetches_once_and_preserves_request_order() {
		with_isolated_state(|| {
			let home = tempfile::tempdir().unwrap();
			let old_home = std::env::var("HOME").ok();
			std::env::set_var("HOME", home.path());
			prepare_global_batch(home.path());

			let fetched = tempfile::tempdir().unwrap();
			for name in ["alpha", "beta"] {
				let directory = fetched.path().join(format!("skills/{name}"));
				std::fs::create_dir_all(&directory).unwrap();
				std::fs::write(
					directory.join("SKILL.md"),
					format!("---\nname: {name}\ndescription: new\n---\nnew\n"),
				)
				.unwrap();
			}
			let fetcher = CountingFetcher {
				root: fetched.path().to_path_buf(),
				calls: std::sync::atomic::AtomicUsize::new(0),
			};
			let resolver = empty_keyring_resolver();
			let result = rocket::tokio::runtime::Builder::new_current_thread()
				.enable_all()
				.build()
				.unwrap()
				.block_on(apply_skill_updates_inner(
					ApplySkillUpdatesRequest {
						source: "https://github.com/owner/repo".to_string(),
						names: vec!["beta".to_string(), "alpha".to_string()],
						scope: "global".to_string(),
						project_root: None,
						confirm: Some(true),
					},
					&fetcher,
					&resolver,
				));

			match old_home {
				Some(value) => std::env::set_var("HOME", value),
				None => std::env::remove_var("HOME"),
			}

			let response = match result {
				Ok(json) => json.into_inner(),
				Err(error) => {
					panic!("batch apply should return Ok: {}", error.body.error)
				}
			};
			assert_eq!(
				fetcher.calls.load(std::sync::atomic::Ordering::SeqCst),
				1,
				"skills sharing a source and ref must share one fetch"
			);
			assert_eq!(
				response
					.results
					.iter()
					.map(|row| row.name.as_str())
					.collect::<Vec<_>>(),
				["beta", "alpha"]
			);
			assert!(response.results.iter().all(|row| row.success));
			// The success projection is shared with the single-update route, so
			// assert its whole payload here: scope echoed, the hash the resync
			// actually computed, and the swapped path attributed.
			for row in &response.results {
				assert_eq!(row.scope, "global");
				assert!(
					row.updated_hash.as_ref().is_some_and(|h| !h.is_empty()),
					"{} must report the hash it stamped",
					row.name
				);
				assert!(
					row.paths.iter().any(|path| path.contains(&row.name)),
					"{} must attribute its swapped path, got {:?}",
					row.name,
					row.paths
				);
				assert!(row.error.is_none() && row.code.is_none());
			}
			let lock = skill::lock::global::read_skill_lock();
			for name in ["alpha", "beta"] {
				let installed = std::fs::read_to_string(
					home.path().join(format!(".claude/skills/{name}/SKILL.md")),
				)
				.unwrap();
				assert!(installed.contains("new"), "{name} was not updated");
				assert_eq!(
					lock.skills[name].ref_commit.as_deref(),
					Some("batch-commit"),
					"{name} lock must record the fetched commit"
				);
			}
		});
	}

	/// The REST slot of the real fetch composite: resolves, then refuses the
	/// download the way blob admission does when a skill has more files than
	/// the remaining (anonymous) budget.
	#[cfg(unix)]
	struct BudgetRefusingRest;

	#[cfg(unix)]
	const BUDGET_COMMIT: &str = "1111111111111111111111111111111111111111";

	#[cfg(unix)]
	impl aghub_git::RepoFetchBackend for BudgetRefusingRest {
		fn resolve(
			&self,
			_source: &aghub_git::SourceRef,
			_auth: Option<&aghub_git::Credentials>,
		) -> aghub_git::Result<aghub_git::RepoSnapshot> {
			Ok(aghub_git::RepoSnapshot {
				commit_oid: BUDGET_COMMIT.to_string(),
				tree_oid: "budget-tree".to_string(),
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
			_p: &[&str],
			_d: &Path,
		) -> aghub_git::Result<()> {
			Err(aghub_git::GitError::rest_fallback(
				"blob admission needs 135 requests and 17266008 bytes, but \
				 only 28 requests remain",
			))
		}
	}

	/// The gix slot: serves the same commit from a prebuilt tree.
	#[cfg(unix)]
	struct TreeServingGix {
		root: PathBuf,
	}

	#[cfg(unix)]
	impl aghub_git::RepoFetchBackend for TreeServingGix {
		fn resolve(
			&self,
			_source: &aghub_git::SourceRef,
			_auth: Option<&aghub_git::Credentials>,
		) -> aghub_git::Result<aghub_git::RepoSnapshot> {
			Ok(aghub_git::RepoSnapshot {
				commit_oid: BUDGET_COMMIT.to_string(),
				tree_oid: "budget-tree".to_string(),
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
			dest: &Path,
		) -> aghub_git::Result<()> {
			for path in paths {
				let target = dest.join(path);
				std::fs::create_dir_all(&target).unwrap();
				std::fs::copy(
					self.root.join(path).join("SKILL.md"),
					target.join("SKILL.md"),
				)
				.unwrap();
			}
			Ok(())
		}
	}

	/// The incident: an anonymous REST budget cannot pay for a large skill, so
	/// every "update all" returned 200 with this row failed and nothing
	/// written. The same commit is reachable over git, so it must update.
	#[cfg(unix)]
	#[test]
	fn apply_skill_updates_writes_a_skill_the_rest_budget_refused() {
		with_isolated_state(|| {
			let home = tempfile::tempdir().unwrap();
			let old_home = std::env::var("HOME").ok();
			std::env::set_var("HOME", home.path());
			prepare_global_batch_names(home.path(), ["alpha"]);

			let upstream = tempfile::tempdir().unwrap();
			let directory = upstream.path().join("skills/alpha");
			std::fs::create_dir_all(&directory).unwrap();
			std::fs::write(
				directory.join("SKILL.md"),
				"---\nname: alpha\ndescription: new\n---\nnew\n",
			)
			.unwrap();
			let fetcher = skill_update::GitFetcher::with_repository(
				skill_update::SkillRepository::with_backends(
					Some(Arc::new(BudgetRefusingRest)),
					Arc::new(TreeServingGix {
						root: upstream.path().to_path_buf(),
					}),
				),
			);
			let resolver = empty_keyring_resolver();
			let result = rocket::tokio::runtime::Builder::new_current_thread()
				.enable_all()
				.build()
				.unwrap()
				.block_on(apply_skill_updates_inner(
					ApplySkillUpdatesRequest {
						source: "https://github.com/owner/repo".to_string(),
						names: vec!["alpha".to_string()],
						scope: "global".to_string(),
						project_root: None,
						confirm: Some(true),
					},
					&fetcher,
					&resolver,
				));

			match old_home {
				Some(value) => std::env::set_var("HOME", value),
				None => std::env::remove_var("HOME"),
			}

			let response = match result {
				Ok(json) => json.into_inner(),
				Err(error) => {
					panic!("batch apply should return Ok: {}", error.body.error)
				}
			};
			let row = &response.results[0];
			assert!(row.success, "row failed: {:?} {:?}", row.error, row.code);
			let installed = std::fs::read_to_string(
				home.path().join(".claude/skills/alpha/SKILL.md"),
			)
			.unwrap();
			assert!(installed.contains("new"), "alpha was not updated");
			assert_eq!(
				skill::lock::global::read_skill_lock().skills["alpha"]
					.ref_commit
					.as_deref(),
				Some(BUDGET_COMMIT)
			);
		});
	}

	/// `confirm=true` is the destructive-default gate on a route that overwrites
	/// every installed file of a whole Source. Nothing else in the repo notices
	/// if it is removed, so assert BOTH the rejection and that no fetch and no
	/// write happened.
	#[cfg(unix)]
	#[test]
	fn apply_skill_updates_requires_confirm_and_writes_nothing_without_it() {
		with_isolated_state(|| {
			let home = tempfile::tempdir().unwrap();
			let _home = HomeGuard::set(home.path());
			prepare_global_batch(home.path());
			let runtime = rocket::tokio::runtime::Builder::new_current_thread()
				.enable_all()
				.build()
				.unwrap();

			let mut statuses = Vec::new();
			for confirm in [None, Some(false)] {
				let result = runtime.block_on(apply_skill_updates_inner(
					ApplySkillUpdatesRequest {
						source: "https://github.com/owner/repo".to_string(),
						names: vec!["alpha".to_string()],
						scope: "global".to_string(),
						project_root: None,
						confirm,
					},
					&PanicOnFetch,
					&empty_keyring_resolver(),
				));
				statuses.push(match result {
					Ok(_) => panic!("confirm={confirm:?} must be rejected"),
					Err(error) => (error.status, error.body.code),
				});
			}

			let installed = std::fs::read_to_string(
				home.path().join(".claude/skills/alpha/SKILL.md"),
			)
			.unwrap();

			for (status, code) in statuses {
				assert_eq!(status, Status::BadRequest);
				assert_eq!(code, "INVALID_PARAM");
			}
			assert!(
				installed.contains("old"),
				"an unconfirmed batch must not touch installed content"
			);
		});
	}

	#[cfg(unix)]
	#[test]
	fn apply_skill_updates_rejects_empty_oversized_and_all_scope() {
		with_isolated_state(|| {
			let runtime = rocket::tokio::runtime::Builder::new_current_thread()
				.enable_all()
				.build()
				.unwrap();
			let request =
				|names: Vec<String>, scope: &str| ApplySkillUpdatesRequest {
					source: "https://github.com/owner/repo".to_string(),
					names,
					scope: scope.to_string(),
					project_root: None,
					confirm: Some(true),
				};
			let cases = [
				request(Vec::new(), "global"),
				request(
					(0..=MAX_BATCH_NAMES)
						.map(|index| format!("skill-{index}"))
						.collect(),
					"global",
				),
				request(vec!["alpha".to_string()], "all"),
			];
			let at_cap = request(
				(0..MAX_BATCH_NAMES)
					.map(|index| format!("skill-{index}"))
					.collect(),
				"global",
			);

			for case in cases {
				let names = case.names.len();
				let scope = case.scope.clone();
				match runtime.block_on(apply_skill_updates_inner(
					case,
					&PanicOnFetch,
					&empty_keyring_resolver(),
				)) {
					Ok(_) => {
						panic!("names={names} scope={scope} must be rejected")
					}
					Err(error) => {
						assert_eq!(error.status, Status::BadRequest);
					}
				}
			}

			// Exactly at the cap is LEGAL: a `>=` typo would reject a real
			// batch with nothing else going red. The rows themselves fail
			// (no such lock entries) — only the request-level verdict matters.
			let home = tempfile::tempdir().unwrap();
			let _home = HomeGuard::set(home.path());
			let response = match runtime.block_on(apply_skill_updates_inner(
				at_cap,
				&PanicOnFetch,
				&empty_keyring_resolver(),
			)) {
				Ok(json) => json.into_inner(),
				Err(error) => panic!(
					"a batch exactly at the cap must be accepted: {}",
					error.body.error
				),
			};
			assert_eq!(response.results.len(), MAX_BATCH_NAMES);
			assert!(response.results.iter().all(|row| !row.success));
		});
	}

	#[cfg(unix)]
	#[test]
	fn apply_skill_updates_reports_backend_failure_for_every_ordered_row() {
		with_isolated_state(|| {
			let home = tempfile::tempdir().unwrap();
			let old_home = std::env::var("HOME").ok();
			std::env::set_var("HOME", home.path());
			prepare_global_batch(home.path());
			let result = rocket::tokio::runtime::Builder::new_current_thread()
				.enable_all()
				.build()
				.unwrap()
				.block_on(apply_skill_updates_inner(
					ApplySkillUpdatesRequest {
						source: "https://github.com/owner/repo".to_string(),
						names: vec!["beta".to_string(), "alpha".to_string()],
						scope: "global".to_string(),
						project_root: None,
						confirm: Some(true),
					},
					&PanicOnFetch,
					&StubBackendUnavailableResolver,
				));

			match old_home {
				Some(value) => std::env::set_var("HOME", value),
				None => std::env::remove_var("HOME"),
			}

			let response = match result {
				Ok(json) => json.into_inner(),
				Err(error) => panic!(
					"batch failures belong in ordered rows: {}",
					error.body.error
				),
			};
			assert_eq!(response.results.len(), 2);
			assert_eq!(response.results[0].name, "beta");
			assert_eq!(response.results[1].name, "alpha");
			assert!(response.results.iter().all(|row| !row.success));
			assert!(response.results.iter().all(|row| {
				row.code.as_deref() == Some("KEYCHAIN_UNAVAILABLE")
			}));
		});
	}

	/// The bulk route's PROJECT wiring: the desktop uses this scope for a
	/// project Source row, and a mis-wire aims the batch at the wrong lock and
	/// the wrong installed tree. Also pins the repointed row's wire code, the
	/// only row state worth retrying after a refresh.
	#[cfg(unix)]
	#[test]
	fn apply_skill_updates_project_scope_writes_only_the_project() {
		with_isolated_state(|| {
			let home = tempfile::tempdir().unwrap();
			let _home = HomeGuard::set(home.path());
			prepare_global_batch(home.path());

			let project = tempfile::tempdir().unwrap();
			let fetched = tempfile::tempdir().unwrap();
			for name in ["alpha", "gamma"] {
				let installed =
					project.path().join(format!(".claude/skills/{name}"));
				std::fs::create_dir_all(&installed).unwrap();
				std::fs::write(
					installed.join("SKILL.md"),
					format!("---\nname: {name}\ndescription: old\n---\nold\n"),
				)
				.unwrap();
				let directory = fetched.path().join(format!("skills/{name}"));
				std::fs::create_dir_all(&directory).unwrap();
				std::fs::write(
					directory.join("SKILL.md"),
					format!("---\nname: {name}\ndescription: new\n---\nnew\n"),
				)
				.unwrap();
			}
			// gamma belongs to a DIFFERENT repository than the caller names.
			for (name, source) in
				[("alpha", "owner/repo"), ("gamma", "other/repo")]
			{
				skill::add_skill_to_local_lock(
					name,
					skill::LocalSkillLockEntry {
						source_url: None,
						source: source.to_string(),
						ref_name: Some("main".to_string()),
						source_type: "github".to_string(),
						computed_hash: "old".to_string(),
						skill_path: Some(format!("skills/{name}/SKILL.md")),
						ref_commit: None,
					},
					Some(project.path()),
				)
				.unwrap();
			}

			let fetcher = CountingFetcher {
				root: fetched.path().to_path_buf(),
				calls: std::sync::atomic::AtomicUsize::new(0),
			};
			let response =
				match rocket::tokio::runtime::Builder::new_current_thread()
					.enable_all()
					.build()
					.unwrap()
					.block_on(apply_skill_updates_inner(
						ApplySkillUpdatesRequest {
							source: "owner/repo".to_string(),
							names: vec![
								"alpha".to_string(),
								"gamma".to_string(),
							],
							scope: "project".to_string(),
							project_root: Some(
								project.path().to_string_lossy().to_string(),
							),
							confirm: Some(true),
						},
						&fetcher,
						&empty_keyring_resolver(),
					)) {
					Ok(json) => json.into_inner(),
					Err(error) => {
						panic!(
							"project batch should return Ok: {}",
							error.body.error
						)
					}
				};

			assert_eq!(response.results[0].name, "alpha");
			assert!(
				response.results[0].success,
				"{:?}",
				response.results[0].error
			);
			assert_eq!(response.results[0].scope, "project");
			assert!(!response.results[1].success, "gamma was repointed");
			assert_eq!(
				response.results[1].code.as_deref(),
				Some(SKILL_SOURCE_VIEW_STALE_CODE),
				"a stale Source view must be machine-distinguishable from \
				 terminal row states"
			);

			assert!(std::fs::read_to_string(
				project.path().join(".claude/skills/alpha/SKILL.md")
			)
			.unwrap()
			.contains("new"));
			assert!(std::fs::read_to_string(
				project.path().join(".claude/skills/gamma/SKILL.md")
			)
			.unwrap()
			.contains("old"));
			// The identically-named global entry must be untouched: a batch
			// scoped to a project may not write the global lock or its tree.
			assert!(std::fs::read_to_string(
				home.path().join(".claude/skills/alpha/SKILL.md")
			)
			.unwrap()
			.contains("old"));
			assert!(skill::lock::global::read_skill_lock().skills["alpha"]
				.ref_commit
				.is_none());
			let project_lock =
				skill::lock::local::read_local_lock(Some(project.path()));
			assert_eq!(
				project_lock.skills["alpha"].ref_commit.as_deref(),
				Some("batch-commit")
			);
			assert!(project_lock.skills["gamma"].ref_commit.is_none());
		});
	}

	/// A keyring outage on this MUTATING route must answer a retryable 503,
	/// not degrade to "no credential" and fail later (GitHub #15).
	/// See docs/history/api.md#apply-update-keyring-fail-closed
	///
	/// Uses `ForceCredentialBackendUnavailable` (cross-platform), not
	/// `DBUS_SESSION_BUS_ADDRESS`. Needs a real lock entry + installed copy so
	/// the request reaches `resolver.resolve(...)`, and goes through the
	/// mounted route with no forwarded header so the keyring IS consulted.
	/// Complement:
	/// `apply_update_forwarded_token_succeeds_even_when_keyring_backend_unreachable`.
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

	/// The route's `offline` reaches the DISK SWEEP, not just the orchestrator.
	///
	/// Nothing downstream can catch a mis-wire here: the orchestrator's offline
	/// gate answers `Uncheckable{network}` without ever reading `local_hash`, so
	/// every response is byte-identical whether or not the sweep ran — only the
	/// wasted folder hashing differs. `local_hash` on the projected entry is the
	/// one place it is visible. Project scope on purpose: no `HOME` to isolate,
	/// so this needs no env lock.
	#[test]
	fn the_route_passes_offline_to_the_disk_sweep_too() {
		let project = tempfile::tempdir().unwrap();
		let installed = project.path().join(".claude/skills/locked");
		std::fs::create_dir_all(&installed).unwrap();
		std::fs::write(
			installed.join("SKILL.md"),
			"---\nname: locked\ndescription: d\n---\nbody\n",
		)
		.unwrap();
		let mut lock = skill::lock::local::LocalSkillLockFile::new();
		lock.skills.insert(
			"locked".to_string(),
			skill::LocalSkillLockEntry {
				source: "owner/repo".to_string(),
				source_url: None,
				source_type: "github".to_string(),
				ref_name: Some("main".to_string()),
				skill_path: Some("locked/SKILL.md".to_string()),
				computed_hash: "stale".to_string(),
				ref_commit: None,
			},
		);
		skill::write_local_lock(&lock, Some(project.path())).unwrap();
		let scope = ResolvedScope::Project {
			root: project.path().to_path_buf(),
		};

		let Ok(online) = lock_entries_for_scope(&scope, false) else {
			panic!("the project lock projects without error");
		};
		assert_eq!(
			online.entries[0].local_hash,
			skill::compute_skill_folder_hash(&installed).ok(),
			"an online check must carry the installed copy's real hash, or a \
			 locally-modified skill reads as up to date"
		);

		let Ok(offline) = lock_entries_for_scope(&scope, true) else {
			panic!("the project lock projects without error");
		};
		assert_eq!(
			offline.entries[0].local_hash, None,
			"an offline check must not hash a single folder"
		);
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
			local_comparison_hash: None,
			local_ambiguous: false,
			ref_commit: None,
		}];
		let git_fetcher = GitFetcher::new();
		let ref_resolver: Arc<dyn RefResolver> =
			Arc::new(git_fetcher.ref_resolver());
		let fetcher: Arc<dyn Fetcher> = Arc::new(git_fetcher);
		let resolver = empty_keyring_resolver();
		let out =
			run_update_check(entries, fetcher, ref_resolver, &resolver, true)
				.await;
		assert_eq!(out.len(), 1);
		assert!(matches!(
			out[0].status,
			aghub_core::skills::update::SkillUpdateStatus::Uncheckable { .. }
		));
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
				&aghub_core::WriteScope::Global,
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

		let (entries, _identities) =
			project_lock_entries(Some(project.path()), true);
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
				&aghub_core::WriteScope::Global,
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
				&aghub_core::WriteScope::Global,
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

	/// A busy mutation lock during the post-check heal must not turn the check
	/// into a 500: the results come back and the busy heal is reported beside
	/// them as retryable. Unix-only because it reuses `LocalRepoFetcher`.
	#[cfg(unix)]
	#[test]
	fn check_updates_reports_retryable_busy_heal_and_keeps_results() {
		/// Restores one env var on drop, including during a panic.
		struct Restore(&'static str, Option<std::ffi::OsString>);
		impl Drop for Restore {
			fn drop(&mut self) {
				match self.1.take() {
					Some(value) => std::env::set_var(self.0, value),
					None => std::env::remove_var(self.0),
				}
			}
		}

		/// Stub resolver. Never reached: an unknown stored hash disables the
		/// ls-refs preflight, so the check goes straight to the fetch.
		struct NoTip;
		impl RefResolver for NoTip {
			fn resolve(
				&self,
				_source_ref: &SourceRef,
				_token: Option<&str>,
			) -> Result<skill_update::TipObservation, FetchError> {
				Err(FetchError::network("no tip"))
			}
		}

		let _env = crate::routes::test_env_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		let data = tempfile::tempdir().unwrap();
		let _data_env =
			Restore("AGHUB_DATA_DIR", std::env::var_os("AGHUB_DATA_DIR"));
		std::env::set_var("AGHUB_DATA_DIR", data.path());
		let _timeout_env = Restore(
			"AGHUB_TEST_MUTATION_LOCK_TIMEOUT_MS",
			std::env::var_os("AGHUB_TEST_MUTATION_LOCK_TIMEOUT_MS"),
		);
		std::env::set_var("AGHUB_TEST_MUTATION_LOCK_TIMEOUT_MS", "100");

		// Installed project copy, with a placeholder computed hash so the
		// orchestrator emits a heal.
		let project = tempfile::tempdir().unwrap();
		let body = "---\nname: locked\ndescription: d\n---\nbody\n";
		let installed = project.path().join(".claude/skills/locked");
		std::fs::create_dir_all(&installed).unwrap();
		std::fs::write(installed.join("SKILL.md"), body).unwrap();
		let mut local = skill::LocalSkillLockFile::default();
		local.skills.insert(
			"locked".into(),
			skill::LocalSkillLockEntry {
				source_url: None,
				ref_commit: None,
				source: "owner/repo".to_string(),
				ref_name: Some("main".to_string()),
				source_type: "github".to_string(),
				computed_hash: skill::EMPTY_SKILLS_LOCK_DIGEST.to_string(),
				skill_path: Some("locked/SKILL.md".to_string()),
			},
		);
		skill::write_local_lock(&local, Some(project.path())).unwrap();

		// Upstream copy the local fetcher hands back.
		let upstream = tempfile::tempdir().unwrap();
		std::fs::create_dir_all(upstream.path().join("locked")).unwrap();
		std::fs::write(upstream.path().join("locked/SKILL.md"), body).unwrap();

		// Another aghub process holds the project mutation lock.
		let lock_path =
			skill::lock::MutationScope::Project(project.path().to_path_buf())
				.lock_path();
		std::fs::create_dir_all(lock_path.parent().unwrap()).unwrap();
		let held = std::fs::File::options()
			.read(true)
			.write(true)
			.create(true)
			.truncate(false)
			.open(&lock_path)
			.unwrap();
		held.try_lock().expect("must acquire external file lock");

		let outcome = rocket::tokio::runtime::Builder::new_current_thread()
			.enable_all()
			.build()
			.unwrap()
			.block_on(check_skill_updates_inner(
				ResolvedScope::Project {
					root: project.path().to_path_buf(),
				},
				false,
				Arc::new(LocalRepoFetcher {
					root: upstream.path().to_path_buf(),
				}),
				Arc::new(NoTip),
				&empty_keyring_resolver(),
			));
		let json = match outcome {
			Ok(json) => json.into_inner(),
			Err(error) => {
				panic!("check should return Ok: {}", error.body.error)
			}
		};

		let body = serde_json::to_value(json).unwrap();
		assert_eq!(body["results"].as_array().unwrap().len(), 1);
		assert_eq!(body["results"][0]["name"], "locked");
		assert_eq!(body["healError"]["code"], "SKILL_MUTATION_LOCK_BUSY");
		assert_eq!(body["healError"]["retryable"], true);
		assert_eq!(
			skill::lock::local::read_local_lock(Some(project.path())).skills
				["locked"]
				.computed_hash,
			skill::EMPTY_SKILLS_LOCK_DIGEST,
			"a busy heal must write nothing"
		);
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
			local_comparison_hash: None,
			local_ambiguous: false,
			ref_commit: None,
		}];
		let git_fetcher = GitFetcher::new();
		let ref_resolver: Arc<dyn RefResolver> =
			Arc::new(git_fetcher.ref_resolver());
		let fetcher: Arc<dyn Fetcher> = Arc::new(git_fetcher);
		let resolver = empty_keyring_resolver();
		let out =
			run_update_check(entries, fetcher, ref_resolver, &resolver, false)
				.await;
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
			local_comparison_hash: None,
			local_ambiguous: false,
			ref_commit: None,
		}];
		let git_fetcher = GitFetcher::new();
		let ref_resolver: Arc<dyn RefResolver> =
			Arc::new(git_fetcher.ref_resolver());
		let fetcher: Arc<dyn Fetcher> = Arc::new(git_fetcher);
		let resolver = empty_keyring_resolver();
		let out =
			run_update_check(entries, fetcher, ref_resolver, &resolver, false)
				.await;
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
			assert_eq!(
				resp.code.as_deref(),
				Some(aghub_core::skills::update::SKILL_RENAMED_CODE)
			);
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
	/// shape) must reach the apply-update fetch via [`SourceAuth`], with
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
			let resolver = SourceAuth::for_test(forwarded, false);

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
	/// once loaded the fail-closed keyring snapshot BEFORE trying the forwarded
	/// map.
	/// That meant an unreachable keyring 503'd the request UNCONDITIONALLY,
	/// even when the forwarded header already covered the requested source
	/// — defeating the entire purpose of forwarding for a headless remote
	/// (no keyring of its own). Uses `SourceAuth` directly — the
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
			let resolver = SourceAuth::for_test(forwarded, true);

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
	fn apply_skill_update_relative_project_root_is_absolutized() {
		with_isolated_state(|| {
			let home = tempfile::tempdir().unwrap();
			let home_path = home.path().canonicalize().unwrap();
			let _home = HomeGuard::set(&home_path);
			prepare_global_batch(&home_path);

			let project = home_path.join("proj");
			std::fs::create_dir_all(&project).unwrap();
			let installed = project.join(".claude/skills/alpha");
			std::fs::create_dir_all(&installed).unwrap();
			std::fs::write(
				installed.join("SKILL.md"),
				"---\nname: alpha\ndescription: old\n---\nold\n",
			)
			.unwrap();

			let mut lock = skill::LocalSkillLockFile::default();
			lock.skills.insert(
				"alpha".to_string(),
				skill::LocalSkillLockEntry {
					source_url: None,
					source: "owner/repo".to_string(),
					ref_name: Some("main".to_string()),
					source_type: "github".to_string(),
					computed_hash: "old".to_string(),
					skill_path: Some("skills/alpha/SKILL.md".to_string()),
					ref_commit: None,
				},
			);
			skill::lock::local::write_local_lock(&lock, Some(&project))
				.unwrap();

			let fetched = tempfile::tempdir().unwrap();
			let directory = fetched.path().join("skills/alpha");
			std::fs::create_dir_all(&directory).unwrap();
			std::fs::write(
				directory.join("SKILL.md"),
				"---\nname: alpha\ndescription: new\n---\nnew\n",
			)
			.unwrap();

			let fetcher = LocalRepoFetcher {
				root: fetched.path().to_path_buf(),
			};
			let resolver = empty_keyring_resolver();

			let _guard = crate::routes::CwdGuard::change_to(&home_path);

			let req = ApplySkillUpdateRequest {
				name: "alpha".to_string(),
				scope: "project".to_string(),
				project_root: Some("proj".to_string()),
				confirm: Some(true),
			};
			let result = rocket::tokio::runtime::Builder::new_current_thread()
				.enable_all()
				.build()
				.unwrap()
				.block_on(apply_skill_update_inner(req, &fetcher, &resolver));

			let resp = match result {
				Ok(json) => json.into_inner(),
				Err(error) => {
					panic!("apply should return Ok: {}", error.body.error)
				}
			};
			assert!(resp.success, "apply should succeed: {:?}", resp.error);
			assert!(!resp.paths.is_empty(), "swapped paths must not be empty");
			for p in &resp.paths {
				let path = std::path::Path::new(p);
				assert!(
					path.is_absolute(),
					"swapped path must be absolute: {p}"
				);
				assert!(
					path.starts_with(&project),
					"swapped path must start with canonical project root: {p}"
				);
			}
			assert!(std::fs::read_to_string(installed.join("SKILL.md"))
				.unwrap()
				.contains("new"));
			let lock = skill::lock::local::read_local_lock(Some(&project));
			assert_ne!(
				lock.skills["alpha"].computed_hash, "old",
				"project lock hash must advance after a successful apply"
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
	fn accept_rename_inner_resolves_moved_path_and_rewrites_lock() {
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
			entry.skill_path = Some("old/location/SKILL.md".to_string());
			lock.skills.insert("old-skill".into(), entry);
			skill::lock::global::write_skill_lock(&lock).unwrap();

			// Fetched repo has SKILL.md with new name
			let fetched = tempfile::tempdir().unwrap();
			let new_skill_dir = fetched.path().join("new/location");
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
			assert_eq!(
				lock.skills["new-skill"].skill_path.as_deref(),
				Some("new/location/SKILL.md"),
				"the new lock must carry the discovered moved path"
			);
		});
	}

	/// #19: the rename fetch is blocking network I/O, so it must run on the
	/// mutation pool. Proof: the fetch parks until the test's release task runs
	/// on the SAME single-worker runtime. Inline on the worker the release can
	/// never run, the bounded wait times out, and `released` stays false.
	#[cfg(unix)]
	#[test]
	fn accept_rename_fetch_runs_in_the_mutation_pool_not_on_the_async_worker() {
		use std::sync::atomic::{AtomicBool, Ordering};
		use std::sync::{mpsc, Arc, Mutex};
		use std::time::Duration;

		struct GatedFetcher {
			started: Mutex<Option<rocket::tokio::sync::oneshot::Sender<()>>>,
			go: Mutex<mpsc::Receiver<()>>,
			released: Arc<AtomicBool>,
			root: std::path::PathBuf,
		}
		impl Fetcher for GatedFetcher {
			fn fetch(
				&self,
				_source_ref: &SourceRef,
				_token: Option<&str>,
				_selection: skill_update::FetchSelection<'_>,
			) -> Result<skill_update::FetchedRepo, FetchError> {
				if let Some(tx) = self.started.lock().unwrap().take() {
					let _ = tx.send(());
				}
				// Bounded: a regression must fail, not hang the test.
				let ok = self
					.go
					.lock()
					.unwrap()
					.recv_timeout(Duration::from_secs(5))
					.is_ok();
				self.released.store(ok, Ordering::SeqCst);
				Ok(skill_update::FetchedRepo {
					root: self.root.clone(),
					snapshot: aghub_git::RepoSnapshot {
						commit_oid: "c".to_string(),
						tree_oid: "t".to_string(),
						commit_time: None,
					},
					_guard: None,
				})
			}
		}

		with_isolated_state(|| {
			let home = tempfile::tempdir().unwrap();
			let old_home = std::env::var("HOME").ok();
			std::env::set_var("HOME", home.path());

			let mut lock = skill::SkillLockFile::default();
			lock.skills.insert("old-skill".into(), global_entry());
			skill::lock::global::write_skill_lock(&lock).unwrap();

			// Empty fetched tree: the fetch succeeds, the catalog has no
			// `new-skill`, so the route answers SkillNotFound after the fetch.
			let root = tempfile::tempdir().unwrap();
			let (started_tx, started_rx) =
				rocket::tokio::sync::oneshot::channel();
			let (go_tx, go_rx) = mpsc::channel();
			let released = Arc::new(AtomicBool::new(false));
			let fetcher = GatedFetcher {
				started: Mutex::new(Some(started_tx)),
				go: Mutex::new(go_rx),
				released: Arc::clone(&released),
				root: root.path().to_path_buf(),
			};
			let req = crate::dto::skill::AcceptRenameRequest {
				old_name: "old-skill".to_string(),
				new_name: "new-skill".to_string(),
				scope: "global".to_string(),
				project_root: None,
				confirm: Some(true),
			};

			let runtime = rocket::tokio::runtime::Builder::new_multi_thread()
				.worker_threads(1)
				.enable_all()
				.build()
				.unwrap();
			let resp = runtime.block_on(async move {
				let resolver = empty_keyring_resolver();
				let route = rocket::tokio::spawn(async move {
					match accept_rename_inner(req, &fetcher, &resolver).await {
						Ok(json) => json.into_inner(),
						Err(error) => panic!(
							"accept_rename should return Ok: {}",
							error.body.error
						),
					}
				});
				let release = rocket::tokio::spawn(async move {
					let _ = started_rx.await;
					let _ = go_tx.send(());
				});
				release.await.unwrap();
				route.await.unwrap()
			});

			match old_home {
				Some(v) => std::env::set_var("HOME", v),
				None => std::env::remove_var("HOME"),
			}

			assert!(
				released.load(Ordering::SeqCst),
				"the fetch parked the only async worker: it must run on the mutation pool"
			);
			assert!(!resp.success);
			assert_eq!(
				resp.code.as_deref(),
				Some(skill_update::mutation::SKILL_PATH_NOT_FOUND_CODE),
			);
		});
	}

	/// P0-2 guard (a): a degenerate rename whose old/new names sanitize to the
	/// same on-disk dir must be rejected up front (before any fetch/mutation)
	/// with the machine code. Adapter-level: the shared plan
	/// (`plan_locked_rename`) refuses before any lock read / fetch.
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
