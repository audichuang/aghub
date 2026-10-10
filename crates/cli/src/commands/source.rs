//! `aghub-cli source <list|diff|sync>` — manage the git sources you've
//! installed skills from, scoped to the current project + global.
//!
//! `list`/`diff` are read-only. `sync` defaults to a dry-run and only writes
//! with `--yes`. The Sources domain (list + per-skill classification) and the
//! no-network install primitive live in shared crates (`skill_update::sources`
//! / `aghub_core::skills::install_fetched`); this module is the CLI surface:
//! scope resolution, an env-backed credential resolver, a debug-only fetch
//! hook for tests, dry-run/`--yes` gating, and output rendering.

use std::collections::HashMap;

use aghub_core::models::{AgentSelection, AgentType, ResourceScope};

use aghub_core::skills::lock::EntryIdentity;
use aghub_core::skills::update::UncheckableReason;
use aghub_core::WriteScope;
use anyhow::{bail, Result};
use serde::Serialize;
use skill_update::sources::{
	self, SourceScopeKind, SourceSkillDiff, SourceSummary,
};
use skill_update::{FetchError, FetchSelection, SourceRef};
use tabled::builder::Builder;
use tabled::settings::Style;

use crate::{Scope, SourceAction};

/// A source string safe to put in a message. `<SOURCE>` comes straight from
/// argv (or from a lock), and a user who typed `https://user:token@host/repo`
/// would otherwise see that token again in stderr, CI logs, or a captured
/// shell buffer. `aghub_git` already redacts what IT builds; this covers the
/// strings the CLI echoes itself.
///
/// Scheme-less scp-like sources are covered too:
/// [`aghub_git::redact_source_credentials`] owns that shape for every surface —
/// never re-copy it here. See docs/history/cli.md#source-command-consolidation
fn safe_source(source: &str) -> String {
	aghub_git::redact_source_credentials(source)
}

// ─────────────────────────── credential / fetch ────────────────────────────

/// Token resolver for CLI source auth. `GIT_PASSWORD` is explicit user
/// intent and applies to ANY host (self-hosted GitLab / TFS / local test
/// remotes must keep working). `GITHUB_TOKEN` is GitHub-specific by name,
/// so it is only offered when the source host is exactly github.com
/// — the fetch-then-retry-with-token flow would otherwise send the PAT to
/// an arbitrary host after the first failure. Empty/whitespace env values
/// count as unset. `GitFetcher` consumes the token as the `x-access-token`
/// password — there is no username/password basic-auth path. When neither env
/// var applies, the user's own git credential helpers (`gh auth git-credential`,
/// keychain, GCM…) are asked for an https source, so a private repo works with
/// whatever `git` already can read. Returns `NoToken` when nothing applies (one
/// anonymous attempt is made).
pub(crate) struct EnvTokenResolver;
impl skill_update::TokenResolver for EnvTokenResolver {
	fn resolve(&self, source: &str) -> skill_update::TokenResolution {
		let host = skill_update::keychain_host_for_source(source);
		match select_env_token(
			std::env::var("GIT_PASSWORD").ok(),
			std::env::var("GITHUB_TOKEN").ok(),
			host.as_deref(),
		) {
			Some(token) => skill_update::TokenResolution::Token(token),
			None => match git_helper_token(source) {
				Some(token) => skill_update::TokenResolution::Token(token),
				None => skill_update::TokenResolution::NoToken,
			},
		}
	}
}

/// Ask the system git credential helpers for the source's https clone URL.
/// The helper keys its answer on that host, so the token cannot cross hosts.
fn git_helper_token(source: &str) -> Option<String> {
	let clone_url = aghub_git::resolve_remote_source(source).ok()?.clone_url;
	let authority = clone_url.strip_prefix("https://")?.split('/').next()?;
	// An explicit userinfo in the source already is the caller's credential.
	if authority.contains('@') {
		return None;
	}
	aghub_git::credential_fill_password(&clone_url)
}

/// Pure token-selection policy behind [`EnvTokenResolver`] (extracted so it
/// can be unit-tested without touching process env).
fn select_env_token(
	git_password: Option<String>,
	github_token: Option<String>,
	host: Option<&str>,
) -> Option<String> {
	let non_empty = |t: Option<String>| t.filter(|t| !t.trim().is_empty());
	if let Some(token) = non_empty(git_password) {
		return Some(token);
	}
	if host.is_some_and(is_github_host) {
		return non_empty(github_token);
	}
	None
}

fn is_github_host(host: &str) -> bool {
	aghub_git::is_github_com_host(host)
}

/// Production fetch is `skill_update::GitFetcher`. Under debug builds ONLY, a
/// runtime env hook lets `assert_cmd` e2e tests point at a local dir (no
/// network). The hook is gated on `cfg(debug_assertions)` (NOT `cfg(test)`):
/// assert_cmd spawns the real binary, which is not built under `cfg(test)`.
#[derive(Default)]
pub(crate) struct CliFetcher {
	inner: skill_update::GitFetcher,
}

impl CliFetcher {
	pub fn new() -> Self {
		Self {
			inner: skill_update::GitFetcher::new(),
		}
	}

	pub fn ref_resolver(&self) -> skill_update::GitRefResolver {
		self.inner.ref_resolver()
	}
}

impl skill_update::Fetcher for CliFetcher {
	fn fetch(
		&self,
		sr: &SourceRef,
		token: Option<&str>,
		selection: FetchSelection<'_>,
	) -> Result<skill_update::FetchedRepo, FetchError> {
		#[cfg(debug_assertions)]
		if let Some(root) = std::env::var_os("AGHUB_TEST_SOURCE_FETCH_ROOT") {
			// e2e round-trip counter: one line per fetch the hook serves.
			if let Some(log) = std::env::var_os("AGHUB_TEST_SOURCE_FETCH_LOG") {
				use std::io::Write;
				let _ = std::fs::OpenOptions::new()
					.create(true)
					.append(true)
					.open(log)
					.and_then(|mut file| writeln!(file, "fetch"));
			}
			let root = std::path::PathBuf::from(root);
			return if root.is_dir() {
				Ok(skill_update::FetchedRepo {
					root,
					snapshot: aghub_git::RepoSnapshot {
						commit_oid: "test-fetch-root".into(),
						tree_oid: "test-fetch-tree".into(),
						commit_time: None,
					},
					_guard: None,
				})
			} else {
				Err(FetchError::network(format!(
					"AGHUB_TEST_SOURCE_FETCH_ROOT is not a directory \
					 (fetching '{}')",
					safe_source(&sr.source)
				)))
			};
		}
		self.inner.fetch(sr, token, selection)
	}

	fn fetch_pinned(
		&self,
		sr: &SourceRef,
		token: Option<&str>,
		selection: FetchSelection<'_>,
		pinned: &skill_update::PinnedSnapshot,
	) -> Result<skill_update::FetchedRepo, FetchError> {
		// The debug-only fetch-root hook stays authoritative for e2e tests.
		#[cfg(debug_assertions)]
		if std::env::var_os("AGHUB_TEST_SOURCE_FETCH_ROOT").is_some() {
			return self.fetch(sr, token, selection);
		}
		// Forward the claim so the fetch skips re-resolving the tip (one fewer
		// request, and exactly the tip the preflight decided about).
		self.inner.fetch_pinned(sr, token, selection, pinned)
	}

	fn default_branch(
		&self,
		sr: &SourceRef,
		token: Option<&str>,
	) -> Option<String> {
		// e2e tests must never reach the network for a default-branch lookup.
		#[cfg(debug_assertions)]
		if std::env::var_os("AGHUB_TEST_SOURCE_FETCH_ROOT").is_some() {
			return None;
		}
		self.inner.default_branch(sr, token)
	}
}

/// What makes two fetches the same fetch: the repository's IDENTITY and the ref
/// — never the coordinate string.
///
/// Each scope resolves its coordinate independently (typed string vs the
/// lock's recorded clone URL), so `owner/repo` and
/// `https://github.com/owner/repo(.git)` must key the same — the common
/// "installed globally, run from a project" case. `host` stays IN the key:
/// two forges serving one `owner/repo` are two repositories.
#[derive(PartialEq, Eq, Hash)]
struct FetchKey {
	host: Option<String>,
	repo: String,
	ref_: Option<String>,
}

fn fetch_key(sr: &SourceRef) -> FetchKey {
	match aghub_git::resolve_remote_source(&sr.source) {
		Ok(resolved) => FetchKey {
			host: resolved.host,
			repo: resolved.source,
			ref_: sr.ref_.clone(),
		},
		// Nothing parseable as a remote (a local directory, an unsupported
		// spelling): key on the raw string, which is exactly the un-normalized
		// behavior — it can only fail to dedup, never merge two repositories.
		Err(_) => FetchKey {
			host: None,
			repo: sr.source.clone(),
			ref_: sr.ref_.clone(),
		},
	}
}

/// Fetch at most once per `(repository, ref)` for the whole command.
///
/// `source diff` calls [`sources::diff_source`] once per read scope; without
/// this a two-scope diff pays two identical round trips. `FetchedRepo`'s
/// temp-dir guard is an `Arc`, so a memo hit shares the same keep-alive.
/// Only `FetchSelection::CatalogSnapshot` fetches are shared.
struct MemoFetcher<'a> {
	inner: &'a dyn skill_update::Fetcher,
	seen: std::sync::Mutex<HashMap<FetchKey, skill_update::FetchedRepo>>,
	/// Round trips the inner fetcher actually performed. Counted, not timed —
	/// a warm HTTP cache and a memo hit look identical on a clock.
	fetches: std::sync::atomic::AtomicUsize,
}

impl<'a> MemoFetcher<'a> {
	fn new(inner: &'a dyn skill_update::Fetcher) -> Self {
		Self {
			inner,
			seen: std::sync::Mutex::new(HashMap::new()),
			fetches: std::sync::atomic::AtomicUsize::new(0),
		}
	}
}

fn clone_repo(repo: &skill_update::FetchedRepo) -> skill_update::FetchedRepo {
	skill_update::FetchedRepo {
		root: repo.root.clone(),
		snapshot: repo.snapshot.clone(),
		_guard: repo._guard.clone(),
	}
}

impl skill_update::Fetcher for MemoFetcher<'_> {
	fn fetch(
		&self,
		sr: &SourceRef,
		token: Option<&str>,
		selection: FetchSelection<'_>,
	) -> Result<skill_update::FetchedRepo, FetchError> {
		// Only a whole-catalog fetch is shareable: a `Skills` selection is a PARTIAL
		// tree, and serving it to another caller would hand that caller an
		// incomplete tree. Selective fetches pass through, never memoized.
		if !matches!(selection, FetchSelection::CatalogSnapshot) {
			return self.inner.fetch(sr, token, selection);
		}
		let key = fetch_key(sr);
		let mut seen = self.seen.lock().unwrap_or_else(|e| e.into_inner());
		if let Some(hit) = seen.get(&key) {
			return Ok(clone_repo(hit));
		}
		// Failures are NOT memoized: a refusal is the caller's to see per call,
		// and nothing here retries.
		let repo = self.inner.fetch(sr, token, selection)?;
		self.fetches
			.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
		let out = clone_repo(&repo);
		seen.insert(key, repo);
		Ok(out)
	}
}

// ──────────────────────────── scope resolution ─────────────────────────────

/// The `source`-flavoured view of an already-resolved [`Scope`].
///
/// TOTAL — it cannot fail: every rejection happened in `main`'s ONE resolver.
pub(crate) fn read_scopes(scope: &Scope) -> Vec<WriteScope> {
	match (scope.resource_scope(), scope.project_root()) {
		(ResourceScope::GlobalOnly, _) => vec![WriteScope::Global],
		(ResourceScope::ProjectOnly, Some(root)) => {
			vec![WriteScope::project(root)]
		}
		// `ProjectOnly` always carries a root — the resolver bails otherwise.
		(ResourceScope::ProjectOnly, None) => Vec::new(),
		(ResourceScope::Both, root) => {
			let mut scopes = vec![WriteScope::Global];
			if let Some(root) = root {
				scopes.push(WriteScope::project(root));
			}
			scopes
		}
	}
}

/// [`crate::commands::read_locks_checked`] for a resolved read-scope list.
///
/// `source list` / `source diff` / `doctor` all report the lock's contents as
/// their answer, so an unreadable lock must fail here instead of surfacing as
/// "no sources installed" / "untracked".
///
/// `doctor` no longer uses this: it reads its own scope's lock inside
/// `aghub_core::skills::health::report`. The `source` commands read through
/// `skill_update::sources`, which owns its lock access and has no snapshot
/// injection point, so they keep only the fail-closed CHECK and the narrow
/// re-read window. Honest partial, not an oversight.
pub(crate) fn read_scope_locks_checked(
	scopes: &[WriteScope],
) -> Result<crate::commands::LockSnapshot> {
	let want_global = scopes.iter().any(|s| matches!(s, WriteScope::Global));
	let project_root = scopes.iter().find_map(|s| match s {
		WriteScope::Project { root } => Some(root.as_path()),
		WriteScope::Global => None,
	});
	crate::commands::read_locks_checked(want_global, project_root)
}

fn scope_kind_str(kind: SourceScopeKind) -> &'static str {
	match kind {
		SourceScopeKind::Global => "global",
		SourceScopeKind::Project => "project",
	}
}

pub(crate) fn scope_label(scope: &WriteScope) -> &'static str {
	match scope {
		WriteScope::Global => "global",
		WriteScope::Project { .. } => "project",
	}
}

// ───────────────────────────── dispatch entry ──────────────────────────────

/// Dispatch a `source` subcommand action.
pub fn execute(
	action: &SourceAction,
	scope: &Scope,
	agent: &str,
	json: bool,
) -> Result<()> {
	match action {
		SourceAction::List => list(scope, json),
		SourceAction::Diff {
			source,
			git_ref,
			// Accepted and ignored — `diff` has no offline mode.
			online: _,
		} => diff(source, git_ref.as_deref(), scope, json),
		SourceAction::Sync {
			source,
			git_ref,
			update,
			install_missing,
			skills,
			universal,
			yes,
		} => sync(SyncArgs {
			source,
			git_ref: git_ref.as_deref(),
			update: *update,
			install_missing: *install_missing,
			skills,
			universal: *universal,
			yes: *yes,
			json,
			scope,
			agent,
		}),
		SourceAction::AcceptRename {
			old_name,
			new_name,
			git_ref,
			yes,
		} => accept_rename(AcceptRenameArgs {
			old_name,
			new_name,
			git_ref: git_ref.as_deref(),
			yes: *yes,
			json,
			scope,
		}),
	}
}

// ─────────────────────────────── source list ───────────────────────────────

#[derive(Serialize)]
struct SourceSummaryView {
	source: String,
	scope: &'static str,
	#[serde(rename = "skillCount")]
	skill_count: u32,
	#[serde(rename = "sourceUrl")]
	source_url: String,
	#[serde(rename = "sourceType")]
	source_type: String,
}

fn summary_to_view(s: &SourceSummary) -> SourceSummaryView {
	SourceSummaryView {
		source: s.source.clone(),
		scope: scope_kind_str(s.scope),
		skill_count: s.skill_count,
		source_url: s.source_url.clone(),
		source_type: s.source_type.clone(),
	}
}

fn list(scope: &Scope, json: bool) -> Result<()> {
	let scopes = read_scopes(scope);
	// The snapshot is discarded here — see `read_scope_locks_checked`. What is
	// kept is the fail-closed check: a corrupt lock must not read as "no
	// sources installed".
	read_scope_locks_checked(&scopes)?;
	let summaries = sources::list_sources(sources::SourceListInput { scopes });

	if json {
		let views: Vec<SourceSummaryView> =
			summaries.iter().map(summary_to_view).collect();
		println!("{}", serde_json::to_string_pretty(&views)?);
		return Ok(());
	}

	if summaries.is_empty() {
		println!("No installed sources.");
		return Ok(());
	}

	let mut builder = Builder::default();
	builder.push_record(["SOURCE", "SCOPE", "SKILLS", "URL"]);
	for s in &summaries {
		builder.push_record([
			s.source.clone(),
			scope_kind_str(s.scope).to_string(),
			s.skill_count.to_string(),
			s.source_url.clone(),
		]);
	}
	let mut table = builder.build();
	table.with(Style::sharp());
	println!("{table}");
	Ok(())
}

// ─────────────────────────────── source diff ───────────────────────────────

#[derive(Serialize)]
struct DiffSkillView {
	name: String,
	state: &'static str,
	#[serde(rename = "skillPath")]
	skill_path: String,
	#[serde(skip_serializing_if = "Option::is_none")]
	reason: Option<String>,
	#[serde(rename = "previousName", skip_serializing_if = "Option::is_none")]
	previous_name: Option<String>,
	/// RFC 3339 author-time of the upstream tip; populated only for
	/// `installedOutdated` rows (mirrors the API/domain contract).
	#[serde(
		rename = "upstreamCommitTime",
		skip_serializing_if = "Option::is_none"
	)]
	upstream_commit_time: Option<String>,
}

#[derive(Serialize)]
struct DiffScopeView {
	scope: &'static str,
	/// The repository THIS scope was judged against.
	///
	/// A host-blind `owner/repo` resolves per scope, from that scope's own
	/// lock, so two scopes can legitimately diff two forges; the rows must say
	/// which.
	origin: String,
	skills: Vec<DiffSkillView>,
}

fn diff_skill_to_view(d: &SourceSkillDiff) -> DiffSkillView {
	DiffSkillView {
		name: d.name.clone(),
		state: d.state.as_wire(),
		skill_path: d.skill_path.clone(),
		reason: d.reason.clone(),
		previous_name: d.previous_name.clone(),
		upstream_commit_time: d.upstream_commit_time.clone(),
	}
}

fn diff(
	source: &str,
	git_ref: Option<&str>,
	scope: &Scope,
	json: bool,
) -> Result<()> {
	let scopes = read_scopes(scope);
	// The snapshot is discarded here — see `read_scope_locks_checked`. What is
	// kept is the fail-closed check: a corrupt lock must not read as "no
	// sources installed".
	read_scope_locks_checked(&scopes)?;
	diff_with(
		source,
		git_ref,
		&scopes,
		json,
		&CliFetcher::new(),
		&EnvTokenResolver,
	)
}

/// `diff` with its network seams injected.
///
/// The memo is built HERE so a test that counts round trips drives the
/// production wiring; dropping it costs one round trip per scope (GitHub REST
/// quota for a token-holding user).
fn diff_with(
	source: &str,
	git_ref: Option<&str>,
	scopes: &[WriteScope],
	json: bool,
	inner: &dyn skill_update::Fetcher,
	resolver: &dyn skill_update::TokenResolver,
) -> Result<()> {
	let source = source.trim().to_string();

	// ONE deep-entry-point call per read scope; it owns pre-fetch settlement
	// and the per-ref cohort split (mixed refs report row by row, as
	// `/sources/diff` does). The shared memo makes N scopes one round trip.
	let fetcher = MemoFetcher::new(inner);

	let mut per_scope: Vec<(&WriteScope, String, Vec<SourceSkillDiff>)> =
		Vec::new();
	for scope in scopes {
		let outcome = sources::diff_source(
			sources::SourceDiffInput {
				source: source.clone(),
				git_ref: git_ref.map(str::to_string),
				scopes: vec![scope.clone()],
			},
			sources::SourceDiffDeps {
				fetcher: &fetcher,
				resolver,
			},
		);
		let (origin, diffs) = diff_outcome_skills(&source, outcome)?;
		per_scope.push((scope, origin, diffs));
	}

	if json {
		let views: Vec<DiffScopeView> = per_scope
			.iter()
			.map(|(scope, origin, diffs)| DiffScopeView {
				scope: scope_label(scope),
				origin: origin.clone(),
				skills: diffs.iter().map(diff_skill_to_view).collect(),
			})
			.collect();
		println!("{}", serde_json::to_string_pretty(&views)?);
		return Ok(());
	}

	let mut builder = Builder::default();
	builder.push_record(["STATE", "NAME", "SKILL_PATH", "SCOPE"]);
	for (scope, _origin, diffs) in &per_scope {
		for d in diffs {
			builder.push_record([
				d.state.as_wire().to_string(),
				d.name.clone(),
				d.skill_path.clone(),
				scope_label(scope).to_string(),
			]);
		}
	}
	let mut table = builder.build();
	table.with(Style::sharp());
	println!("{table}");
	// The table's four columns are unchanged, so say this only when it is true:
	// the scopes resolved DIFFERENT repositories from the same argument. That
	// used to be an outright refusal ("matches 2 repositories"); each scope now
	// diffs its own recorded origin, which is a better answer only if the reader
	// is told the rows came from two places.
	let distinct: std::collections::BTreeSet<&str> = per_scope
		.iter()
		.map(|(_, origin, _)| origin.as_str())
		.collect();
	if distinct.len() > 1 {
		eprintln!(
			"note: '{}' resolves to a different repository per scope; each \
			 scope was diffed against its own:",
			safe_source(&source)
		);
		for (scope, origin, _) in &per_scope {
			eprintln!("  {}: {origin}", scope_label(scope));
		}
	}
	Ok(())
}

/// Project one scope's [`sources::SourceDiffOutcome`] into `(origin, rows)`, or
/// fail. The origin is the repository the rows were judged against — see
/// [`DiffScopeView::origin`].
fn diff_outcome_skills(
	source: &str,
	outcome: sources::SourceDiffOutcome,
) -> Result<(String, Vec<SourceSkillDiff>)> {
	match outcome {
		sources::SourceDiffOutcome::Ok {
			source: origin,
			skills,
			..
		} => Ok((origin, skills)),
		other => Err(refusal_error(source, other)),
	}
}

/// The wording for every pre-write refusal both deep entry points can return.
///
/// HERE, not in the domain: they name flags (`--ref`, `GIT_PASSWORD`) that
/// exist only on this surface. ONE copy, so `diff` and `sync` cannot word the
/// same refusal two ways.
fn refusal_error(
	source: &str,
	outcome: sources::SourceDiffOutcome,
) -> anyhow::Error {
	use sources::SourceDiffOutcome as O;
	match outcome {
		O::Ok { .. } => unreachable!("callers take the Ok arm themselves"),
		O::AmbiguousSource { origins } => anyhow::anyhow!(
			"source '{}' matches {} repositories:\n  {}\nRun it again \
			 with the SOURCE_URL of the one you mean.",
			safe_source(source),
			origins.len(),
			origins.join("\n  ")
		),
		// `precheck_source` never yields `Network`, so this reason can only come
		// from the credential backend being unreachable — a distinct, actionable
		// failure that must not read as "this source is not fetchable".
		O::UncheckableSource {
			reason: UncheckableReason::Network,
			..
		} => anyhow::anyhow!("Credential backend is unavailable; retry later."),
		O::UncheckableSource { reason, .. } => anyhow::anyhow!(
			"source '{}' cannot be fetched ({reason:?}); only HTTPS / \
			 owner/repo git sources are supported",
			safe_source(source)
		),
		O::NeedsCredential { .. } => anyhow::anyhow!(
			"Could not read this source. Either it needs a credential (log in \
			 with git — e.g. `gh auth login` — or set GIT_PASSWORD for any \
			 host, or GITHUB_TOKEN for github.com, in the environment) or the repo/ref does not exist or is \
			 not visible to the credential already in use."
		),
		O::FetchFailed { detail } => anyhow::anyhow!(
			"Failed to fetch source repository '{}': {}",
			safe_source(source),
			safe_source(&detail)
		),
	}
}

/// A sync refusal, as the diff refusal that words it.
///
/// Two enums because the SUCCESS payloads differ (a diff returns rows; a sync
/// returns the tree it fetched plus the identities it captured). The refusals
/// are the same set of pre-write facts, so they share one renderer instead of a
/// second copy of the same four sentences.
fn sync_refusal_as_diff(
	outcome: sources::SourceSyncOutcome,
) -> sources::SourceDiffOutcome {
	use sources::{SourceDiffOutcome as D, SourceSyncOutcome as S};
	match outcome {
		S::NeedsCredential { git_ref } => D::NeedsCredential { git_ref },
		S::FetchFailed { detail } => D::FetchFailed { detail },
		S::UncheckableSource { git_ref, reason } => {
			D::UncheckableSource { git_ref, reason }
		}
		S::AmbiguousSource { origins } => D::AmbiguousSource { origins },
		// Both carry a sync-only payload and are answered by the caller before
		// this is reached.
		S::Ok(_) | S::MultipleRefs { .. } => {
			unreachable!("answered at the call site")
		}
	}
}

// ─────────────────────────────── source sync ───────────────────────────────

struct SyncArgs<'a> {
	source: &'a str,
	git_ref: Option<&'a str>,
	update: bool,
	install_missing: bool,
	skills: &'a [String],
	universal: bool,
	yes: bool,
	json: bool,
	scope: &'a Scope,
	agent: &'a str,
}

/// Per-agent outcome of one install action. `installed:false` with no `error`
/// means the agent was ALREADY correctly linked (idempotent no-op = success);
/// `error:Some` is a real failure (link error or an occupied/foreign slot).
#[derive(Serialize, Clone)]
struct AgentResultView {
	agent: String,
	installed: bool,
	#[serde(skip_serializing_if = "Option::is_none")]
	error: Option<String>,
}

#[derive(Serialize)]
struct SyncActionView {
	action: &'static str, // "install" | "update"
	name: String,
	#[serde(rename = "skillPath")]
	skill_path: String,
	applied: bool,
	#[serde(skip_serializing_if = "Option::is_none")]
	error: Option<String>,
	/// Stable machine code for `error`, from the ONE classification in
	/// `aghub_core::skills::resync` — the same vocabulary the HTTP API sends.
	/// Additive: absent on success and on rows whose failure has no shared code
	/// yet, so a reader that never looked at it is unaffected.
	#[serde(rename = "errorCode", skip_serializing_if = "Option::is_none")]
	error_code: Option<&'static str>,
	/// Per-agent breakdown (install actions only; empty for update). Lets a
	/// multi-agent (`-a all`) sync show exactly which agents were linked vs
	/// already-present vs failed, instead of a single collapsed status.
	#[serde(skip_serializing_if = "Vec::is_empty")]
	agents: Vec<AgentResultView>,
}

impl SyncActionView {
	/// The failed-update row: message AND machine code both come from
	/// [`resync_row_error`], so a row can never carry one without the other.
	fn update_failed(
		d: &SourceSkillDiff,
		error: skill_update::mutation::ResyncMutationError,
	) -> Self {
		let (message, code) = resync_row_error(&d.name, error);
		Self {
			action: "update",
			name: d.name.clone(),
			skill_path: d.skill_path.clone(),
			applied: false,
			error: Some(message),
			error_code: Some(code),
			agents: Vec::new(),
		}
	}

	/// A hard failure = at least one target agent reported an error (link
	/// failure or occupied slot), OR the action itself failed before reaching
	/// any agent. "Already present" (installed:false, error:None) is NOT a
	/// failure.
	fn had_error(&self) -> bool {
		self.error.is_some() || self.agents.iter().any(|a| a.error.is_some())
	}
}

#[derive(Serialize)]
struct SyncOutcomeView {
	source: String,
	scope: &'static str,
	#[serde(rename = "dryRun")]
	dry_run: bool,
	/// The agents an install would link (safety-critical for `-a all`:
	/// the fan-out must be visible in the dry-run BEFORE `--yes`). Empty —
	/// and omitted — when the plan has no install action.
	#[serde(rename = "targetAgents", skip_serializing_if = "Vec::is_empty")]
	target_agents: Vec<&'static str>,
	actions: Vec<SyncActionView>,
}

/// The agent ids an install plan fans out to: the resolved targets when the
/// plan contains at least one install action, empty otherwise (updates touch
/// only the master, not per-agent links). ONE helper for the text and JSON
/// outputs so they cannot disagree.
fn plan_target_agents(
	plan: &[(&'static str, &SourceSkillDiff)],
	target_agents: &[AgentType],
) -> Vec<&'static str> {
	if plan.iter().any(|(kind, _)| *kind == "install") {
		target_agents.iter().map(|a| a.as_str()).collect()
	} else {
		Vec::new()
	}
}

fn sync(args: SyncArgs) -> Result<()> {
	if args.universal {
		eprintln!(
			"warning: --universal is deprecated and ignored; \
			 skill installs are always symlink-only \
			 (.aghub master + per-agent link)"
		);
	}
	let source = args.source.trim().to_string();

	// Scope was resolved and validated ONCE, in `main` — `--all`, an unscoped
	// run and `-p` with no project root were all refused there.
	let write_scope = args.scope.write_scope()?;
	let scope_label = args.scope.label();

	// Parse the agent selection BEFORE any network work, so an invalid
	// --agent fails here (offline runs included) instead of surfacing a
	// misleading network/auth error first.
	let selection = AgentSelection::parse(args.agent)
		.map_err(|e| anyhow::anyhow!("invalid --agent: {e}"))?;

	// `--yes` with NO action flag is a caller who believes they asked for a
	// write; refuse it before the fetch.
	// See docs/history/cli.md#source-sync-yes-without-action
	if args.yes && !args.update && !args.install_missing {
		bail!(
			"--yes needs an action: pass --install-missing (install missing \
			 skills) and/or --update (refresh outdated ones). Without either, \
			 `source sync` only prints an overview."
		);
	}

	// ONE call into the deep entry point: identity snapshot, coordinate
	// resolution, single-tree assertion, refusals, the single fetch and the
	// classification — never re-assembled here.
	let inner = CliFetcher::new();
	let sync_plan = match sources::plan_source_sync(
		sources::SourceSyncInput {
			source: source.clone(),
			git_ref: args.git_ref.map(str::to_string),
			scope: write_scope.clone(),
		},
		sources::SourceDiffDeps {
			fetcher: &inner,
			resolver: &EnvTokenResolver,
		},
	) {
		sources::SourceSyncOutcome::Ok(plan) => *plan,
		sources::SourceSyncOutcome::MultipleRefs { refs } => bail!(
			"source '{}' has skills pinned to {} different refs:\n  {}\nRun \
			 it again with --ref to work on one of them.",
			safe_source(&source),
			refs.len(),
			refs.iter()
				.map(|name| name.as_deref().unwrap_or("(default branch)"))
				.collect::<Vec<_>>()
				.join("\n  ")
		),
		// Every remaining refusal is shaped exactly like `diff`'s, so it renders
		// through the same words.
		other => {
			return Err(refusal_error(&source, sync_refusal_as_diff(other)))
		}
	};
	let pre_fetch_identities = sync_plan.pre_fetch_identities;
	let repo = sync_plan.repo;
	let diffs = sync_plan.diffs;

	// `--skill a,b` narrows every downstream path (overview, --install-missing,
	// --update) to the named skills. Unknown names are reported (not silently
	// dropped) so a typo doesn't masquerade as a no-op.
	let diffs = if args.skills.is_empty() {
		diffs
	} else {
		let available: Vec<String> =
			diffs.iter().map(|d| d.name.clone()).collect();
		let (kept, unknown) =
			narrow_by_name(diffs, args.skills, |d| d.name.as_str());
		if !unknown.is_empty() {
			eprintln!(
				"warning: source '{}' has no skill named: {} (available: {})",
				safe_source(&source),
				unknown.join(", "),
				available.join(", ")
			);
		}
		kept
	};

	// Neither flag: print the plan (per-state overview) and ask the user to
	// choose an action. Read-only/informational — write NOTHING.
	if !args.update && !args.install_missing {
		return print_no_action_plan(&source, scope_label, &diffs, args.json);
	}

	// Resolve the target agent(s). `-a all` fans the install across every agent
	// that can ACTUALLY receive this skill in this scope — the multi-agent
	// extract-and-replace case. Native readers are covered by the shared master;
	// other supported agents each get their own symlink. Agents that are
	// Unsupported here (no skill dir / project-only) are dropped up front, NOT
	// reported as failures — otherwise `-a all` would always exit non-zero.
	// An explicit `-a <agent>` or comma list (`-a claude,grok`) is taken
	// verbatim (an unsupported one is a real error the user asked for).
	// Default is one agent (claude).
	let target_agents: Vec<AgentType> = match &selection {
		AgentSelection::All => {
			use aghub_core::skills::linker::{agent_link_need, LinkNeed};
			// Registry order; keep agents that can hold a skill here.
			// `agent_link_need` is probe-free (`classify_all` would spawn an
			// availability subprocess per agent). An agent the user disabled
			// is not aghub's to write to (`aghub_core::agent_settings`).
			let disabled = aghub_core::agent_settings::disabled_agents();
			aghub_core::registry::ALL_AGENTS
				.iter()
				.copied()
				.filter(|d| !disabled.contains(d.id))
				.filter(|d| {
					!matches!(
						agent_link_need(
							d,
							write_scope.resource_scope(),
							write_scope.project_root()
						),
						LinkNeed::Unsupported
					)
				})
				.map(|d| d.agent_type)
				.collect()
		}
		AgentSelection::List(agents) => agents.clone(),
	};

	// No agent in this scope can hold a skill (e.g. `-a all` where every agent
	// is Unsupported here). Bail rather than let `materialize_universal_master`
	// vacuously report success on an empty target set.
	if target_agents.is_empty() {
		bail!(
			"no agent in the current scope can receive skills — nothing to \
			 install"
		);
	}

	// Build the plan.
	// - Without `--skill`: `--install-missing` targets only `NotInstalled` rows
	//   (excludes Deprecated/Renamed/Removed); `--update` targets
	//   `InstalledOutdated` rows.
	// - With an explicit `--skill`: `--install-missing` ALSO re-materializes
	//   `InstalledCurrent` rows. The install is idempotent (already-correct
	//   links are no-ops), so this ENSURES each named skill is linked for every
	//   target agent even when the scope lock already says "installed" — the
	//   repair path a bare, lock-gated `--install-missing` cannot reach (e.g.
	//   adding a new agent's link, or `-a all` after a single-agent install).
	//   It also takes `Uncheckable { local }` rows: a lock entry with nothing on
	//   disk (or disagreeing copies) has no local evidence; the install's
	//   adoption guard refuses a Master whose bytes differ from the fetched ones,
	//   and an occupied agent slot is reported as a conflict, not overwritten.
	use skill_update::sources::SourceSkillState as St;
	let ensure_named = !args.skills.is_empty();
	let mut plan: Vec<(&'static str, &SourceSkillDiff)> = Vec::new();
	if args.install_missing {
		for d in diffs.iter().filter(|d| {
			d.state == St::NotInstalled
				|| (ensure_named
					&& (d.state == St::InstalledCurrent
						|| (d.state == St::Uncheckable
							&& d.reason.as_deref() == Some("local"))))
		}) {
			plan.push(("install", d));
		}
	}
	if args.update {
		for d in diffs.iter().filter(|d| d.state == St::InstalledOutdated) {
			plan.push(("update", d));
		}
	}

	// Resolve the lock source ONCE from the RECOVERED fetch coordinate
	// (recorded `sourceUrl` for a non-github host, else the arg), NOT the raw
	// shorthand — a TFS `Collection/_git/repo` would misparse as github.
	// Normalization lives in `aghub_git`.
	let fetch_source = sync_plan.fetch_source.clone();
	let resolved =
		aghub_git::resolve_remote_source(&fetch_source).map_err(|e| {
			anyhow::anyhow!(
				"invalid source '{}': {e}",
				safe_source(&fetch_source)
			)
		})?;
	// Record the ref the plan fetched: the shared decision (`sources::import_ref`).
	let lock_source = skill::InstallLockSource {
		source: resolved.lock_source(),
		source_type: resolved.source_type.as_str().to_string(),
		source_url: resolved.source_url.clone(),
		ref_name: sync_plan.git_ref.clone(),
	};
	let fetched = skill_update::mutation::FetchedSource::from_repo(repo);

	if !args.yes {
		// Dry-run (default): print the plan, write nothing. The fetch already
		// succeeded and the request construction is pure.
		return print_dry_run(
			&source,
			scope_label,
			&plan,
			&target_agents,
			args.json,
			&PreviewContext {
				fetched: &fetched,
				lock_source: &lock_source,
				scope: &write_scope,
			},
		);
	}

	let plan_targets = plan_target_agents(&plan, &target_agents);
	let action_report = aghub_core::batch::run_multi_target_mutation(
		&plan,
		|(kind, _)| {
			if *kind != "install" {
				return Ok(());
			}
			aghub_core::batch::skill_batch_preflight(
				&target_agents,
				write_scope.resource_scope(),
			)
		},
		|(kind, d)| {
			Ok::<SyncActionView, aghub_core::ConfigError>(match *kind {
				"install" => apply_install(
					&fetched,
					d,
					&write_scope,
					&target_agents,
					&lock_source,
				),
				"update" => apply_update_row(
					&fetched,
					d,
					&write_scope,
					&fetch_source,
					&pre_fetch_identities,
				),
				_ => unreachable!(),
			})
		},
	)
	.map_err(|error| {
		// Every rejected row carries the same skill_batch_preflight error; keep
		// it typed so report_failure emits its wire code.
		anyhow::Error::from(
			error
				.failures
				.into_iter()
				.next()
				.expect("a preflight rejection names at least one target")
				.reason,
		)
	})?;
	let actions: Vec<SyncActionView> = action_report
		.results
		.into_iter()
		.map(|row| {
			row.result
				.expect("action execution is infallible after preflight")
		})
		.collect();

	// A hard failure on ANY action (an agent link error / occupied slot, or an
	// action that failed outright) must surface as a non-zero exit — a conflict
	// or partial multi-agent failure was previously swallowed as success.
	let had_error = actions.iter().any(|a| a.had_error());

	if args.json {
		let view = SyncOutcomeView {
			source,
			scope: scope_label,
			dry_run: false,
			target_agents: plan_targets,
			actions,
		};
		println!("{}", serde_json::to_string_pretty(&view)?);
	} else {
		for a in &actions {
			if a.agents.len() > 1 {
				// Multi-agent (`-a all`): a summary plus a per-agent breakdown so
				// a partial relink is visible, never silently reported as done.
				let linked = a.agents.iter().filter(|x| x.installed).count();
				let already = a
					.agents
					.iter()
					.filter(|x| !x.installed && x.error.is_none())
					.count();
				let failed =
					a.agents.iter().filter(|x| x.error.is_some()).count();
				println!(
					"{}: {} ({}) — {linked} installed, {already} already \
					 present, {failed} failed",
					a.action, a.name, a.skill_path
				);
				for ag in &a.agents {
					// "installed" covers a fresh symlink AND a native reader
					// (which reads the master with no link of its own).
					let status = match &ag.error {
						Some(e) => format!("failed: {e}"),
						None if ag.installed => "installed".to_string(),
						None => "already present".to_string(),
					};
					println!("    - {}: {status}", ag.agent);
				}
			} else {
				match &a.error {
					None if a.applied => {
						println!("{}: {} ({})", a.action, a.name, a.skill_path)
					}
					None => println!(
						"{}: {} ({}) — skipped (already present)",
						a.action, a.name, a.skill_path
					),
					Some(err) => {
						println!("{}: {} — failed: {err}", a.action, a.name)
					}
				}
			}
		}
		if actions.is_empty() {
			println!("Nothing to do.");
		}
	}

	if had_error {
		// The view above already carries every per-action verdict, so a second
		// error document after it would leave stdout holding TWO concatenated
		// JSON documents and every parse of it failing. This path was missed
		// when the other three were marked.
		crate::note_answer_on_stdout();
		bail!("one or more sync actions failed (see the results above)");
	}
	Ok(())
}

/// Per-state counts of a scope's classified skills, for the no-action plan.
#[derive(Serialize, Default)]
struct PlanCounts {
	#[serde(rename = "notInstalled")]
	not_installed: u32,
	#[serde(rename = "installedOutdated")]
	installed_outdated: u32,
	#[serde(rename = "installedCurrent")]
	installed_current: u32,
	deprecated: u32,
	other: u32,
}

#[derive(Serialize)]
struct NoActionPlanView {
	source: String,
	scope: &'static str,
	#[serde(rename = "actionSelected")]
	action_selected: bool,
	counts: PlanCounts,
	skills: Vec<DiffSkillView>,
}

fn count_states(diffs: &[SourceSkillDiff]) -> PlanCounts {
	use skill_update::sources::SourceSkillState as St;
	let mut c = PlanCounts::default();
	for d in diffs {
		match d.state {
			St::NotInstalled => c.not_installed += 1,
			St::InstalledOutdated => c.installed_outdated += 1,
			St::InstalledCurrent => c.installed_current += 1,
			St::Deprecated => c.deprecated += 1,
			_ => c.other += 1,
		}
	}
	c
}

/// `sync` with neither `--update` nor `--install-missing`: print the plan (the
/// per-skill state overview, same rows `diff` prints) and the per-state counts,
/// then ask the user to choose an action. Writes NOTHING.
fn print_no_action_plan(
	source: &str,
	scope_label: &'static str,
	diffs: &[SourceSkillDiff],
	json: bool,
) -> Result<()> {
	let counts = count_states(diffs);

	if json {
		let view = NoActionPlanView {
			source: source.to_string(),
			scope: scope_label,
			action_selected: false,
			counts,
			skills: diffs.iter().map(diff_skill_to_view).collect(),
		};
		println!("{}", serde_json::to_string_pretty(&view)?);
		return Ok(());
	}

	let mut builder = Builder::default();
	builder.push_record(["STATE", "NAME", "SKILL_PATH"]);
	for d in diffs {
		builder.push_record([
			d.state.as_wire().to_string(),
			d.name.clone(),
			d.skill_path.clone(),
		]);
	}
	let mut table = builder.build();
	table.with(Style::sharp());
	println!("{table}");

	println!(
		"No action selected. Pass --install-missing to install the {} \
		 not-installed skill(s) and/or --update to update the {} outdated \
		 skill(s).",
		counts.not_installed, counts.installed_outdated
	);
	Ok(())
}

/// What the preview needs to run the install's own refusal guard per row.
struct PreviewContext<'a> {
	fetched: &'a skill_update::mutation::FetchedSource,
	lock_source: &'a skill::InstallLockSource,
	scope: &'a WriteScope,
}

fn print_dry_run(
	source: &str,
	scope_label: &'static str,
	plan: &[(&'static str, &SourceSkillDiff)],
	target_agents: &[AgentType],
	json: bool,
	ctx: &PreviewContext<'_>,
) -> Result<()> {
	let plan_targets = plan_target_agents(plan, target_agents);
	// An install row predicted to be refused by the SAME guard `--yes` runs
	// (advisory: unlocked, and the install re-checks under the lock).
	let predicted: Vec<Option<String>> = plan
		.iter()
		.map(|(kind, d)| {
			(*kind == "install")
				.then(|| {
					skill_update::mutation::preflight_fetched_source(
						ctx.fetched,
						install_request(
							d,
							ctx.lock_source,
							ctx.scope,
							target_agents,
						),
					)
					.err()
					.map(install_error_message)
				})
				.flatten()
		})
		.collect();
	let would_fail = predicted.iter().any(Option::is_some);

	if json {
		let actions: Vec<SyncActionView> = plan
			.iter()
			.zip(&predicted)
			.map(|((kind, d), error)| SyncActionView {
				action: kind,
				name: d.name.clone(),
				skill_path: d.skill_path.clone(),
				applied: false,
				error: error.clone(),
				error_code: None,
				agents: Vec::new(),
			})
			.collect();
		let view = SyncOutcomeView {
			source: source.to_string(),
			scope: scope_label,
			dry_run: true,
			target_agents: plan_targets,
			actions,
		};
		println!("{}", serde_json::to_string_pretty(&view)?);
		return preview_verdict(would_fail);
	}

	if plan.is_empty() {
		println!("Nothing to do (everything is already in sync).");
		return Ok(());
	}
	println!("Dry-run (pass --yes to apply):");
	if plan.iter().any(|(kind, _)| *kind == "update") {
		println!(
			"  update target: scoped Master + existing referrers (`-a/--agent` \
			 applies only to install/relink actions)"
		);
	}
	// Make the fan-out visible BEFORE --yes: installs touch every agent
	// listed here (`-a all` can be the whole registry).
	if !plan_targets.is_empty() {
		println!(
			"  target agents ({}): {}",
			plan_targets.len(),
			plan_targets.join(", ")
		);
	}
	for ((kind, d), error) in plan.iter().zip(&predicted) {
		match error {
			None => println!("  would {}: {} ({})", kind, d.name, d.skill_path),
			Some(err) => {
				println!("  would {} — refused: {} — {err}", kind, d.name)
			}
		}
	}
	preview_verdict(would_fail)
}

/// A predicted refusal exits 1 like the `--yes` run would. The view is already
/// on stdout, so mark the answer there before bailing (one JSON document).
fn preview_verdict(would_fail: bool) -> Result<()> {
	if would_fail {
		crate::note_answer_on_stdout();
		bail!("one or more sync actions would fail (see the preview above)");
	}
	Ok(())
}

fn install_request<'a>(
	d: &'a SourceSkillDiff,
	lock_source: &'a skill::InstallLockSource,
	scope: &WriteScope,
	target_agents: &'a [AgentType],
) -> skill_update::mutation::FetchedInstallRequest<'a> {
	skill_update::mutation::FetchedInstallRequest {
		source: lock_source,
		lock_skill_path: &d.skill_path,
		expected_name: Some(&d.name),
		scope: scope.clone(),
		target_agents,
		expected_ref: None,
	}
}

fn install_error_message(
	error: skill_update::mutation::InstallMutationError,
) -> String {
	use skill_update::mutation::InstallMutationError;
	match error {
		InstallMutationError::InvalidSkillPath => {
			"skillPath was not found in the source".to_string()
		}
		InstallMutationError::Install(error) => error.to_string(),
	}
}

fn apply_install(
	fetched: &skill_update::mutation::FetchedSource,
	d: &SourceSkillDiff,
	scope: &WriteScope,
	target_agents: &[AgentType],
	lock_source: &skill::InstallLockSource,
) -> SyncActionView {
	use skill_update::mutation::install_fetched_source;

	let req = install_request(d, lock_source, scope, target_agents);

	match install_fetched_source(fetched, req) {
		Ok(report) => {
			let applied = report.agent_results.iter().any(|r| r.installed);
			// First HARD per-agent error (link failure / occupied slot). An
			// already-linked agent has installed:false + error:None and must NOT
			// count as an error — so surface the real error even when another
			// agent installed fine (partial multi-agent failure stays visible).
			let error =
				report.agent_results.iter().find_map(|r| r.error.clone());
			let agents = report
				.agent_results
				.iter()
				.map(|r| AgentResultView {
					agent: r.agent.as_str().to_string(),
					installed: r.installed,
					error: r.error.clone(),
				})
				.collect();
			SyncActionView {
				action: "install",
				name: d.name.clone(),
				skill_path: d.skill_path.clone(),
				applied,
				error,
				error_code: None,
				agents,
			}
		}
		Err(error) => SyncActionView {
			action: "install",
			name: d.name.clone(),
			skill_path: d.skill_path.clone(),
			applied: false,
			error: Some(install_error_message(error)),
			error_code: None,
			agents: Vec::new(),
		},
	}
}

fn apply_update_row(
	fetched: &skill_update::mutation::FetchedSource,
	d: &SourceSkillDiff,
	scope: &WriteScope,
	fetch_source: &str,
	pre_fetch: &std::collections::BTreeMap<String, EntryIdentity>,
) -> SyncActionView {
	use skill_update::mutation::{resync_fetched_source, FetchedResyncRequest};

	match resync_fetched_source(
		fetched,
		FetchedResyncRequest {
			skill_path: &d.skill_path,
			name: &d.name,
			scope: scope.clone(),
			source: fetch_source,
			expected: pre_fetch.get(&d.name).cloned(),
		},
	) {
		Ok(report) => SyncActionView {
			action: "update",
			name: d.name.clone(),
			skill_path: d.skill_path.clone(),
			applied: !report.swapped.is_empty(),
			error: None,
			error_code: None,
			agents: Vec::new(),
		},
		Err(error) => SyncActionView::update_failed(d, error),
	}
}

/// Map a sync update-row resync failure to its row MESSAGE plus the shared
/// machine CODE.
///
/// Only the wording is this surface's (a CLI row may name the skill and quote
/// detail; the API owes a path-free sentence). The code is
/// `error.code()` from skill-update.
fn resync_row_error(
	name: &str,
	error: skill_update::mutation::ResyncMutationError,
) -> (String, &'static str) {
	use aghub_core::skills::resync::ResyncError;
	use skill_update::mutation::ResyncMutationError;

	let code = error.code();
	let message = match error {
		ResyncMutationError::InvalidSkillPath => {
			"locked skillPath was not found in source".to_string()
		}
		ResyncMutationError::SourceChangedDuringFetch => {
			"skill appeared in the lock while this sync was fetching; nothing was written"
				.to_string()
		}
		ResyncMutationError::SourceMismatch => format!(
			"the fetched source or skill path does not match what '{name}' is locked to; nothing was written"
		),
		ResyncMutationError::Resync(ResyncError::NotInstalled) => {
			format!("skill '{name}' is locked but no installed copy was found")
		}
		ResyncMutationError::Resync(ResyncError::Renamed { new_name }) => {
			aghub_core::skills::update::skill_renamed_message(name, &new_name)
		}
		ResyncMutationError::Resync(other) => other.to_string(),
	};
	(message, code)
}

// ──────────────────────────── source accept-rename ─────────────────────────

struct AcceptRenameArgs<'a> {
	old_name: &'a str,
	new_name: &'a str,
	git_ref: Option<&'a str>,
	yes: bool,
	json: bool,
	scope: &'a Scope,
}

/// `source accept-rename <old> <new>` — thin adapter over
/// `skill_update::mutation::rename_locked_skill`. Resolves scope and dry-run
/// (CLI concerns); the preview runs the commit's own fetch-free plan
/// (`plan_locked_rename`), so it refuses exactly what `--yes` refuses.
fn accept_rename(args: AcceptRenameArgs) -> Result<()> {
	use skill_update::mutation::{
		plan_locked_rename, rename_locked_skill, LockedRenameRequest,
	};

	// Scope was resolved and validated ONCE, in `main`; `write_scope` refuses
	// anything that is not a single write target instead of defaulting to
	// global.
	let scope = args.scope.write_scope()?;
	let scope_label = args.scope.label();

	let request = LockedRenameRequest {
		old_name: args.old_name,
		new_name: args.new_name,
		scope,
		git_ref: args.git_ref,
	};
	// The fetch-free refusals (degenerate names, not in the lock, target already
	// present) BEFORE the preview, so the preview never green-lights what `--yes`
	// refuses. See docs/history/cli.md#accept-rename-preview
	plan_locked_rename(&request).map_err(|e| rename_error(args.new_name, e))?;

	if !args.yes {
		// The preview MUST honour --json like every sibling preview. Keys
		// match the --yes payload, plus `applied` to tell the two apart.
		if args.json {
			println!(
				"{}",
				serde_json::to_string_pretty(&serde_json::json!({
					"success": true,
					"dryRun": true,
					"applied": false,
					"oldName": args.old_name,
					"newName": args.new_name,
					"scope": scope_label,
				}))?
			);
		} else {
			println!(
				"Dry-run: would install '{}' and remove '{}' \
				 ({scope_label}). Pass --yes to execute.",
				args.new_name, args.old_name
			);
		}
		return Ok(());
	}

	// Fetch, then the lock-holding transaction — both inside the shared entry.
	let outcome =
		rename_locked_skill(request, &CliFetcher::new(), &EnvTokenResolver)
			.map_err(|e| rename_error(args.new_name, e))?;

	if args.json {
		println!(
			"{}",
			serde_json::to_string_pretty(&serde_json::json!({
				"success": true,
				// Mirrors the preview branch above so ONE parser handles both
				// and can tell them apart without inspecting other keys.
				"dryRun": false,
				"applied": true,
				"oldName": args.old_name,
				"newName": args.new_name,
				"scope": scope_label,
				"installedHash": outcome.installed_hash,
				"paths": outcome.paths,
			}))?
		);
	} else {
		println!(
			"Renamed '{}' → '{}': installed to {} path(s), removed old skill.",
			args.old_name,
			args.new_name,
			outcome.paths.len()
		);
	}
	Ok(())
}

/// CLI wording for a rename failure. The code and retryability are the shared
/// ones from skill-update, so `--json` reports the same code as the API.
fn rename_error(
	new_name: &str,
	error: skill_update::mutation::RenameMutationError,
) -> anyhow::Error {
	use skill_update::mutation::RenameMutationError;

	let message = match &error {
		RenameMutationError::Fetch(FetchError::Auth) => {
			"This source needs a credential. Log in with git (e.g. `gh auth \
			 login`), or set GIT_PASSWORD (any host) or GITHUB_TOKEN \
			 (github.com) in the environment, and retry."
				.to_string()
		}
		RenameMutationError::Fetch(FetchError::Network(detail)) => {
			format!(
				"Failed to fetch source repository: {}",
				safe_source(detail)
			)
		}
		RenameMutationError::Fetch(FetchError::BackendUnavailable) => {
			"Credential backend is unavailable; retry later.".to_string()
		}
		RenameMutationError::CatalogScan => {
			"Fetched source catalog could not be scanned safely".to_string()
		}
		RenameMutationError::SkillNotFound => {
			format!(
				"new skill '{new_name}' was not found in the fetched source"
			)
		}
		RenameMutationError::Rename(e) => e.message(),
	};
	match error.code() {
		Some(code) => anyhow::Error::new(crate::CodedError {
			message,
			code,
			retryable: error.retryable(),
		}),
		None => anyhow::anyhow!("{message}"),
	}
}

/// Split `items` into those whose name is in `requested` (source order kept)
/// and the requested names that matched nothing. Generic over the item so the
/// `--skill` filter unit-tests without constructing a full `SourceSkillDiff`.
fn narrow_by_name<T>(
	items: Vec<T>,
	requested: &[String],
	name_of: impl Fn(&T) -> &str,
) -> (Vec<T>, Vec<String>) {
	use std::collections::HashSet;
	let present: HashSet<&str> = items.iter().map(&name_of).collect();
	let unknown: Vec<String> = requested
		.iter()
		.filter(|r| !present.contains(r.as_str()))
		.cloned()
		.collect();
	let want: HashSet<&str> = requested.iter().map(String::as_str).collect();
	let kept = items
		.into_iter()
		.filter(|it| want.contains(name_of(it)))
		.collect();
	(kept, unknown)
}

#[cfg(test)]
mod tests {
	use super::{
		apply_update_row, diff_with, narrow_by_name, plan_target_agents,
		resync_row_error, select_env_token, FetchError, MemoFetcher,
		SyncActionView,
	};
	use aghub_core::models::AgentType;
	use aghub_core::WriteScope;
	use skill_update::sources::{SourceSkillDiff, SourceSkillState};
	use std::path::PathBuf;

	fn s(v: &str) -> Option<String> {
		Some(v.to_string())
	}

	use skill_update::{FetchSelection, Fetcher, SourceRef};

	/// Counts the round trips the inner fetcher actually performs.
	struct CountingFetcher {
		root: PathBuf,
		calls: std::sync::atomic::AtomicUsize,
	}
	impl Fetcher for CountingFetcher {
		fn fetch(
			&self,
			_sr: &SourceRef,
			_token: Option<&str>,
			_selection: FetchSelection<'_>,
		) -> Result<skill_update::FetchedRepo, FetchError> {
			self.calls
				.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
			Ok(skill_update::FetchedRepo {
				root: self.root.clone(),
				snapshot: aghub_git::RepoSnapshot::default(),
				_guard: None,
			})
		}
	}

	/// `source diff` calls the deep entry point ONCE PER SCOPE, and each call
	/// owns its own fetch — so without the memo a two-scope diff of one source
	/// pays two identical round trips.
	///
	/// Counted, not timed: both git backends cache, so a clock cannot tell a
	/// second round trip from a warm one. This drives `diff_with` — the
	/// PRODUCTION function, memo and all — so removing the wrap goes red;
	/// building a `MemoFetcher` beside a bare `diff_source` call would only
	/// prove the type compiles.
	///
	/// The two scopes resolve the coordinate DIFFERENTLY on purpose: one has a
	/// lock entry recording the full clone URL, the other has no lock at all and
	/// falls back to the `owner/repo` the caller typed. That is "installed
	/// globally, run from a project", and keying the memo on the raw strings
	/// fetched the one repository twice.
	#[test]
	fn two_scopes_over_one_ref_cost_one_fetch() {
		struct NoToken;
		impl skill_update::TokenResolver for NoToken {
			fn resolve(&self, _source: &str) -> skill_update::TokenResolution {
				skill_update::TokenResolution::NoToken
			}
		}

		let upstream = tempfile::tempdir().unwrap();
		let skill_dir = upstream.path().join("alpha");
		std::fs::create_dir_all(&skill_dir).unwrap();
		std::fs::write(
			skill_dir.join("SKILL.md"),
			"---\nname: alpha\ndescription: d\n---\nbody\n",
		)
		.unwrap();

		let inner = CountingFetcher {
			root: upstream.path().to_path_buf(),
			calls: std::sync::atomic::AtomicUsize::new(0),
		};

		// Scope A knows the source: its lock records the full clone URL, so the
		// deep entry point fetches THAT. No recorded ref — a different ref is a
		// different tree, and would be two fetches by design.
		let locked = tempfile::tempdir().unwrap();
		std::fs::write(
			locked.path().join("skills-lock.json"),
			r#"{"version":1,"skills":{"alpha":{"source":"owner/repo",
			  "sourceType":"github","sourceUrl":"https://github.com/owner/repo.git",
			  "skillPath":"alpha/SKILL.md","computedHash":"stale"}}}"#,
		)
		.unwrap();
		// Scope B has no lock, so it falls back to the raw `owner/repo` argument.
		let bare = tempfile::tempdir().unwrap();
		let scopes = [
			WriteScope::Project {
				root: locked.path().to_path_buf(),
			},
			WriteScope::Project {
				root: bare.path().to_path_buf(),
			},
		];

		diff_with("owner/repo", None, &scopes, true, &inner, &NoToken).unwrap();

		assert_eq!(
			inner.calls.load(std::sync::atomic::Ordering::Relaxed),
			1,
			"one repository at one ref, one round trip — the second scope must \
			 be served from the memo even though it resolved a different \
			 SPELLING of the same repo"
		);
	}

	/// Only a whole-catalog fetch is shared. A `Skills` selection is a partial
	/// tree, so memoizing it would hand a later caller an incomplete catalog.
	#[test]
	fn memo_shares_only_a_whole_catalog_fetch() {
		let upstream = tempfile::tempdir().unwrap();
		let inner = CountingFetcher {
			root: upstream.path().to_path_buf(),
			calls: std::sync::atomic::AtomicUsize::new(0),
		};
		let memo = MemoFetcher::new(&inner);
		let sr = SourceRef {
			source: "owner/repo".into(),
			ref_: None,
		};
		let a = [skill::SkillPath::parse("alpha").unwrap()];
		let b = [skill::SkillPath::parse("beta").unwrap()];

		memo.fetch(&sr, None, FetchSelection::Skills(&a)).unwrap();
		memo.fetch(&sr, None, FetchSelection::Skills(&b)).unwrap();
		assert_eq!(
			inner.calls.load(std::sync::atomic::Ordering::Relaxed),
			2,
			"different selections must not share one partial tree"
		);

		memo.fetch(&sr, None, FetchSelection::CatalogSnapshot)
			.unwrap();
		memo.fetch(&sr, None, FetchSelection::CatalogSnapshot)
			.unwrap();
		assert_eq!(
			inner.calls.load(std::sync::atomic::Ordering::Relaxed),
			3,
			"one repository, one catalog, one round trip"
		);
	}

	fn diff(name: &str, state: SourceSkillState) -> SourceSkillDiff {
		SourceSkillDiff {
			name: name.to_string(),
			skill_path: format!("{name}/SKILL.md"),
			description: None,
			version: None,
			author: None,
			state,
			previous_name: None,
			reason: None,
			installed_paths: Vec::new(),
			upstream_commit_time: None,
		}
	}

	// Pin the PRODUCTION update-row variant→message mapping (previously
	// inlined in `apply_update_row` with no coverage — a swapped arm was
	// invisible to the suite).
	#[test]
	fn resync_row_error_maps_variants_to_row_messages() {
		use aghub_core::skills::resync::ResyncError;
		use skill_update::mutation::ResyncMutationError;

		assert_eq!(
			resync_row_error("keep", ResyncMutationError::InvalidSkillPath).0,
			"locked skillPath was not found in source"
		);
		assert_eq!(
			resync_row_error(
				"keep",
				ResyncMutationError::Resync(ResyncError::NotInstalled)
			)
			.0,
			"skill 'keep' is locked but no installed copy was found"
		);
		let (renamed, _) = resync_row_error(
			"keep",
			ResyncMutationError::Resync(ResyncError::Renamed {
				new_name: "keep-v2".to_string(),
			}),
		);
		assert!(
			renamed.contains("keep") && renamed.contains("keep-v2"),
			"rename mapping must carry both names, got: {renamed}"
		);
	}

	/// A `source sync` row that failed because the source moved mid-fetch must
	/// carry the SAME machine code the HTTP API answers with, and it must reach
	/// the JSON as `errorCode`.
	///
	/// This is what the API's own comment already claimed ("the same answer the
	/// CLI's sync gives") while the CLI in fact emitted untyped prose: a script
	/// could not tell a re-fetchable race from a genuine sync failure.
	#[test]
	fn an_entry_that_appeared_mid_fetch_carries_the_shared_code() {
		let temp = tempfile::tempdir().unwrap();
		let fetched = skill_update::mutation::FetchedSource::from_repo(
			skill_update::FetchedRepo {
				root: temp.path().to_path_buf(),
				snapshot: aghub_git::RepoSnapshot {
					commit_oid: "c".into(),
					tree_oid: "t".into(),
					commit_time: None,
				},
				_guard: None,
			},
		);
		let row = apply_update_row(
			&fetched,
			&diff("keep", SourceSkillState::InstalledOutdated),
			&WriteScope::project(temp.path()),
			"owner/repo",
			&std::collections::BTreeMap::new(),
		);
		let json = serde_json::to_value(&row).unwrap();
		assert_eq!(json["errorCode"], "SKILL_SOURCE_CHANGED_DURING_FETCH");
		assert_eq!(json["applied"], false);
	}

	#[test]
	fn a_source_that_moved_mid_fetch_carries_the_shared_code() {
		use aghub_core::skills::resync::ResyncError;
		use skill_update::mutation::ResyncMutationError;

		let (_message, code) = resync_row_error(
			"keep",
			ResyncMutationError::Resync(ResyncError::StaleFetch(
				"entry moved".to_string(),
			)),
		);
		assert_eq!(
			code,
			aghub_core::skills::lock::SOURCE_CHANGED_DURING_FETCH_CODE
		);

		// Through the PRODUCTION constructor, not a hand-built row: the failed
		// branch of `apply_update_row` builds it this way, so a row that
		// dropped the code on the way to the wire fails here.
		let row = SyncActionView::update_failed(
			&diff("keep", SourceSkillState::InstalledOutdated),
			ResyncMutationError::Resync(ResyncError::StaleFetch(
				"entry moved".to_string(),
			)),
		);
		let json = serde_json::to_value(&row).unwrap();
		assert_eq!(json["errorCode"], "SKILL_SOURCE_CHANGED_DURING_FETCH");
		assert_eq!(json["applied"], false);

		// Additive: a row with nothing to report omits the field entirely, so a
		// reader that never looked at it sees the shape it always saw.
		let ok = SyncActionView {
			action: "update",
			name: "keep".to_string(),
			skill_path: "keep/SKILL.md".to_string(),
			applied: true,
			error: None,
			error_code: None,
			agents: Vec::new(),
		};
		assert!(serde_json::to_value(&ok)
			.unwrap()
			.get("errorCode")
			.is_none());
		let _ = code;
	}

	#[test]
	fn plan_target_agents_lists_agents_only_for_installs() {
		let agents = [AgentType::Claude, AgentType::Grok];
		let install = diff("a", SourceSkillState::NotInstalled);
		let update = diff("b", SourceSkillState::InstalledOutdated);

		// An install action exposes the full fan-out (the safety-critical
		// pre-`--yes` visibility for `-a all`).
		let plan = [("install", &install), ("update", &update)];
		assert_eq!(plan_target_agents(&plan, &agents), vec!["claude", "grok"]);

		// Update-only plans touch the master, not per-agent links: empty
		// (and the JSON field is omitted).
		let plan = [("update", &update)];
		assert!(plan_target_agents(&plan, &agents).is_empty());

		// Empty plan → empty.
		assert!(plan_target_agents(&[], &agents).is_empty());
	}

	#[test]
	fn git_password_applies_to_any_host() {
		for host in [Some("github.com"), Some("tfs.corp.local"), None] {
			assert_eq!(
				select_env_token(s("pw"), s("gh"), host),
				s("pw"),
				"GIT_PASSWORD must win on host {host:?}"
			);
		}
	}

	#[test]
	fn github_token_is_bound_to_exact_github_host() {
		assert_eq!(
			select_env_token(None, s("gh"), Some("github.com")),
			s("gh")
		);
		assert_eq!(
			select_env_token(None, s("gh"), Some("API.GitHub.com")),
			None
		);
		// FQDN trailing root dot is the same host.
		assert_eq!(
			select_env_token(None, s("gh"), Some("github.com.")),
			s("gh")
		);
		assert_eq!(
			select_env_token(None, s("gh"), Some("api.github.com.")),
			None
		);
		for host in [
			Some("gitlab.com"),
			Some("evil-github.com"),
			Some("github.com.evil.com"),
			Some("github.com.evil.com."),
			None,
		] {
			assert_eq!(
				select_env_token(None, s("gh"), host),
				None,
				"GITHUB_TOKEN must not leak to host {host:?}"
			);
		}
	}

	#[test]
	fn empty_or_whitespace_tokens_count_as_unset() {
		assert_eq!(
			select_env_token(s(""), s("gh"), Some("github.com")),
			s("gh"),
			"empty GIT_PASSWORD falls through to GITHUB_TOKEN"
		);
		assert_eq!(select_env_token(s(" "), s("\t"), Some("github.com")), None);
	}

	#[test]
	fn narrow_by_name_keeps_requested_in_source_order_and_reports_unknown() {
		let items = vec!["a".to_string(), "b".to_string(), "c".to_string()];
		let requested = vec!["c".to_string(), "a".to_string(), "x".to_string()];
		let (kept, unknown) = narrow_by_name(items, &requested, |s| s.as_str());
		// Kept follows the source order (a, c), not the request order.
		assert_eq!(kept, vec!["a".to_string(), "c".to_string()]);
		// A typo'd name surfaces instead of vanishing into a silent no-op.
		assert_eq!(unknown, vec!["x".to_string()]);
	}

	#[test]
	fn narrow_by_name_all_unknown_keeps_nothing() {
		let items = vec!["a".to_string()];
		let (kept, unknown) =
			narrow_by_name(items, &["zzz".to_string()], |s| s.as_str());
		assert!(kept.is_empty());
		assert_eq!(unknown, vec!["zzz".to_string()]);
	}

	#[test]
	fn cli_fetcher_forwards_pinned_claim_without_resolving_again() {
		use skill_update::{FetchSelection, Fetcher, RefResolver, SourceRef};
		use std::sync::atomic::{AtomicUsize, Ordering};
		use std::sync::Arc;

		struct CountingBackend {
			resolves: AtomicUsize,
		}
		impl aghub_git::RepoFetchBackend for CountingBackend {
			fn resolve(
				&self,
				_s: &aghub_git::SourceRef,
				_a: Option<&aghub_git::Credentials>,
			) -> aghub_git::Result<aghub_git::RepoSnapshot> {
				self.resolves.fetch_add(1, Ordering::SeqCst);
				Ok(aghub_git::RepoSnapshot {
					commit_oid: "1111111111111111111111111111111111111111"
						.into(),
					tree_oid: "tree".into(),
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
				_d: &std::path::Path,
			) -> aghub_git::Result<()> {
				Ok(())
			}
		}

		let rest = Arc::new(CountingBackend {
			resolves: AtomicUsize::new(0),
		});
		let gix = Arc::new(CountingBackend {
			resolves: AtomicUsize::new(0),
		});
		let fetcher = super::CliFetcher {
			inner: skill_update::GitFetcher::with_repository(
				skill_update::SkillRepository::with_backends(
					Some(rest.clone() as Arc<dyn aghub_git::RepoFetchBackend>),
					gix.clone() as Arc<dyn aghub_git::RepoFetchBackend>,
				),
			),
		};
		// github.com host => the REST slot answers the preflight and yields a claim.
		let sr = SourceRef {
			source: "https://github.com/owner/repo".into(),
			ref_: None,
		};
		let tip = fetcher.ref_resolver().resolve(&sr, None).expect("tip");
		let pinned = tip.pinned.expect("REST preflight returns a pinned claim");

		fetcher
			.fetch_pinned(&sr, None, FetchSelection::Skills(&[]), &pinned)
			.expect("pinned fetch");

		assert_eq!(
			rest.resolves.load(Ordering::SeqCst),
			1,
			"pinned fetch must not resolve the tip a second time"
		);
		assert_eq!(gix.resolves.load(Ordering::SeqCst), 0);
	}
}
