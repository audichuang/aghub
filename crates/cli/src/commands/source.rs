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
use std::path::{Path, PathBuf};

use aghub_core::models::{AgentSelection, AgentType, ResourceScope};

use aghub_core::skills::lock::EntryIdentity;
use aghub_core::skills::update::UncheckableReason;
use anyhow::{bail, Result};
use serde::Serialize;
use skill_update::sources::{
	self, SourceScope, SourceScopeKind, SourceSkillDiff, SourceSummary,
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
/// Scheme-less scp-like sources are covered too — see
/// [`aghub_git::redact_source_credentials`], which owns that shape for every
/// surface (this used to be a private copy here, and the copy is exactly why the
/// update-check log later grew the same hole).
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
/// password — there is no username/password basic-auth path. Returns
/// `NoToken` when nothing applies (one anonymous attempt is made).
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
			None => skill_update::TokenResolution::NoToken,
		}
	}
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
struct CliFetcher;
impl skill_update::Fetcher for CliFetcher {
	fn fetch(
		&self,
		sr: &SourceRef,
		token: Option<&str>,
		selection: FetchSelection<'_>,
	) -> Result<skill_update::FetchedRepo, FetchError> {
		#[cfg(debug_assertions)]
		if let Some(root) = std::env::var_os("AGHUB_TEST_SOURCE_FETCH_ROOT") {
			let root = PathBuf::from(root);
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
		skill_update::GitFetcher::new().fetch(sr, token, selection)
	}
}

/// What makes two fetches the same fetch: the repository's IDENTITY and the ref
/// — never the coordinate string.
///
/// The two calls being deduplicated resolve their coordinate independently, per
/// scope, and a scope with no matching lock entry falls back to the string the
/// user typed while a scope that HAS one resolves the entry's recorded clone
/// URL. `owner/repo` and `https://github.com/owner/repo(.git)` are then two
/// spellings of one repository, and a key of raw strings would fetch it twice —
/// the exact shape of "installed globally, run from a project", which is the
/// common case, not a corner.
///
/// `host` stays IN the key. Two forges serving the same `owner/repo` are two
/// repositories, and each scope diffing its own is the point.
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
/// The deep entry points own their fetches, and `source diff` calls
/// [`sources::diff_source`] once per read scope — so without this a two-scope
/// diff of one source pays two identical round trips. `FetchedRepo`'s temp-dir
/// guard is an `Arc`, so a memo hit hands back the same root with a clone of the
/// same keep-alive: the tree cannot be dropped while a later caller is reading
/// it.
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
/// TOTAL — it cannot fail. Every rejection (`-g` with `-p`, `--all` where it
/// is meaningless, `-p` with no project root) happened in `main`'s ONE
/// resolver, before this is reached. This file used to carry four private
/// resolvers of its own and three hand-copied versions of the same
/// "no project root found" sentence.
pub(crate) fn read_scopes(scope: &Scope) -> Vec<SourceScope> {
	match (scope.resource_scope(), scope.project_root()) {
		(ResourceScope::GlobalOnly, _) => vec![SourceScope::Global],
		(ResourceScope::ProjectOnly, Some(root)) => {
			vec![SourceScope::Project {
				root: root.to_path_buf(),
			}]
		}
		// `ProjectOnly` always carries a root — the resolver bails otherwise.
		(ResourceScope::ProjectOnly, None) => Vec::new(),
		(ResourceScope::Both, root) => {
			let mut scopes = vec![SourceScope::Global];
			if let Some(root) = root {
				scopes.push(SourceScope::Project {
					root: root.to_path_buf(),
				});
			}
			scopes
		}
	}
}

/// The single writing scope for `sync` / `accept-rename`.
///
/// `--all` and an unscoped invocation are refused by their scope policies in
/// `main`; [`Scope::write_target`] refuses anything else rather than falling
/// back to a silent GLOBAL write, which is what the `_ =>` arm this replaced
/// did.
fn write_scope(scope: &Scope) -> Result<SourceScope> {
	Ok(match scope.write_target()? {
		Some(root) => SourceScope::Project {
			root: root.to_path_buf(),
		},
		None => SourceScope::Global,
	})
}

/// [`crate::commands::read_locks_checked`] for a resolved read-scope list.
///
/// `source list` / `source diff` / `doctor` all report the lock's contents as
/// their answer, so an unreadable lock must fail here instead of surfacing as
/// "no sources installed" / "untracked".
///
/// `doctor` CONSUMES the returned snapshot; the two `source` commands route
/// their reads through `skill_update::sources`, which owns its own lock access,
/// so they discard it and keep the narrow re-read window. Honest partial, not
/// an oversight.
pub(crate) fn read_scope_locks_checked(
	scopes: &[SourceScope],
) -> Result<crate::commands::LockSnapshot> {
	let want_global = scopes.iter().any(|s| matches!(s, SourceScope::Global));
	let project_root = scopes.iter().find_map(|s| match s {
		SourceScope::Project { root } => Some(root.as_path()),
		SourceScope::Global => None,
	});
	// `source list` / `source diff` route their reads through
	// `skill_update::sources`, which owns its own lock access — there is no
	// injection point for a snapshot without reshaping that shared crate. So
	// they keep the fail-closed CHECK (a corrupt lock still fails loudly rather
	// than reading as "no sources installed") and keep the narrow re-read
	// window that `check` no longer has. Honest partial, not an oversight.
	crate::commands::read_locks_checked(want_global, project_root)
}

fn scope_kind_str(kind: SourceScopeKind) -> &'static str {
	match kind {
		SourceScopeKind::Global => "global",
		SourceScopeKind::Project => "project",
	}
}

pub(crate) fn scope_label(scope: &SourceScope) -> &'static str {
	match scope {
		SourceScope::Global => "global",
		SourceScope::Project { .. } => "project",
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
	/// A host-blind `owner/repo` resolves per scope, from that scope's own lock,
	/// so two scopes can legitimately diff two different forges. This used to be
	/// refused outright as an ambiguous source; judging each scope against its
	/// own recorded origin is the better answer, but only if the rows say which
	/// origin that was.
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
		&CliFetcher,
		&EnvTokenResolver,
	)
}

/// `diff` with its network seams injected.
///
/// The memo is built HERE rather than in `diff`, so a test that counts round
/// trips drives the same wiring production does. Constructing a `MemoFetcher`
/// beside the call under test instead only proves the type compiles: dropping
/// the wrap from `diff` was invisible to the suite, and the cost of dropping it
/// is one full round trip per scope (a token-holding user's `source diff` spends
/// GitHub REST quota, 60/hr).
fn diff_with(
	source: &str,
	git_ref: Option<&str>,
	scopes: &[SourceScope],
	json: bool,
	inner: &dyn skill_update::Fetcher,
	resolver: &dyn skill_update::TokenResolver,
) -> Result<()> {
	let source = source.trim().to_string();

	// ONE call into the deep entry point per read scope. It owns the whole
	// pre-fetch settlement (coordinate resolution, the ambiguous-source refusal,
	// the precheck) AND the per-ref cohort split, so a source whose entries are
	// pinned to different refs is reported row by row instead of refused —
	// the answer `/sources/diff` has always given.
	//
	// The fetcher is shared and memoizing, so N scopes over one ref still cost
	// one round trip.
	let fetcher = MemoFetcher::new(inner);

	let mut per_scope: Vec<(&SourceScope, String, Vec<SourceSkillDiff>)> =
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
/// It lives HERE, not in the domain: these are the strings the CLI has always
/// printed, and they name flags (`--ref`, `GIT_PASSWORD`) that exist only on
/// this surface. ONE copy, so `diff` and `sync` cannot word the same refusal
/// two ways — which is exactly what they did while each owned its own
/// three-branch `FetchError` match.
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
			"Could not read this source. Either it needs a credential (set \
			 GIT_PASSWORD for any host, or GITHUB_TOKEN for github.com, in \
			 the environment and retry) or the repo/ref does not exist or is \
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
			 (.agents/skills master + per-agent link)"
		);
	}
	let source = args.source.trim().to_string();

	// Scope was resolved and validated ONCE, in `main` — `--all`, an unscoped
	// run and `-p` with no project root were all refused there.
	let scope = args.scope.resource_scope();
	let project_root = args.scope.project_root().map(Path::to_path_buf);
	let source_scope = write_scope(args.scope)?;
	let scope_label = args.scope.label();

	// Parse the agent selection BEFORE any network work, so an invalid
	// --agent fails here (offline runs included) instead of surfacing a
	// misleading network/auth error first.
	let selection = AgentSelection::parse(args.agent)
		.map_err(|e| anyhow::anyhow!("invalid --agent: {e}"))?;

	// Same reason, one line earlier in the flow: `--yes` with NO action flag is
	// a caller who believes they asked for a write. `source sync <repo> --yes`
	// is the most natural spelling of "install this repo's skills", and it used
	// to fall through to the no-action overview — exit 0, no `dryRun` key at
	// all, and a THIRD payload shape, so a consumer keyed on `dryRun == false`
	// (missing → falsy) concluded the install had been applied. Refused here,
	// before the fetch, so it costs no network round-trip and reports the real
	// problem instead of a credential/network error.
	if args.yes && !args.update && !args.install_missing {
		bail!(
			"--yes needs an action: pass --install-missing (install missing \
			 skills) and/or --update (refresh outdated ones). Without either, \
			 `source sync` only prints an overview."
		);
	}

	// ONE call into the deep entry point. It owns the identity snapshot, the
	// coordinate resolution, the single-tree assertion, the ambiguous-source and
	// precheck refusals, the single fetch, and the classification — the sequence
	// this file used to re-assemble here and again in `diff`, with the branches
	// copied word for word.
	let inner = CliFetcher;
	let sync_plan = match sources::plan_source_sync(
		sources::SourceSyncInput {
			source: source.clone(),
			git_ref: args.git_ref.map(str::to_string),
			scope: source_scope.clone(),
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
			// Iterate the registry in its stable order (claude first) and
			// keep agents that can hold a skill here. `agent_link_need` is
			// the probe-free classifier — no per-agent availability
			// subprocess, since we only need the link decision, not whether
			// the CLI is installed. (`classify_all` would run that probe for
			// every agent.)
			aghub_core::registry::ALL_AGENTS
				.iter()
				.copied()
				.filter(|d| {
					!matches!(
						agent_link_need(d, scope, project_root.as_deref()),
						LinkNeed::Unsupported
					)
				})
				.filter_map(|d| d.id.parse::<AgentType>().ok())
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
	use skill_update::sources::SourceSkillState as St;
	let ensure_named = !args.skills.is_empty();
	let mut plan: Vec<(&'static str, &SourceSkillDiff)> = Vec::new();
	if args.install_missing {
		for d in diffs.iter().filter(|d| {
			d.state == St::NotInstalled
				|| (ensure_named && d.state == St::InstalledCurrent)
		}) {
			plan.push(("install", d));
		}
	}
	if args.update {
		for d in diffs.iter().filter(|d| d.state == St::InstalledOutdated) {
			plan.push(("update", d));
		}
	}

	if !args.yes {
		// Dry-run (default): print the plan, write nothing.
		return print_dry_run(
			&source,
			scope_label,
			&plan,
			&target_agents,
			args.json,
		);
	}

	// Resolve the normalized lock source ONCE from the RECOVERED fetch
	// coordinate (recorded `sourceUrl` for a non-github host, else the arg) —
	// NOT the raw shorthand, or a TFS `Collection/_git/repo` would fail
	// github-shorthand parsing and a 2-segment non-github source would
	// normalize to the wrong github lock source. Normalization lives in
	// `aghub_git`; we never re-implement it.
	let fetch_source = sync_plan.fetch_source.clone();
	let resolved =
		aghub_git::resolve_remote_source(&fetch_source).map_err(|e| {
			anyhow::anyhow!(
				"invalid source '{}': {e}",
				safe_source(&fetch_source)
			)
		})?;
	// Record the RESOLVED ref (explicit `--ref` OR the source's recorded lock
	// ref), not just the explicit flag — so re-installing a source pinned to a
	// tag/branch persists that pin, matching what the API records.
	//
	// Residual API divergence: when neither an explicit `--ref` nor a recorded
	// ref exists, the CLI records `None` here, whereas the API records the scan
	// session's resolved default-branch name (`session.current_branch`). The
	// CLI's `FetchedRepo` exposes only the tip OID (`repo.oid`), not the
	// branch/ref name, so the default-branch name is not cheaply available
	// without a second ls-refs round-trip; we record `None` rather than invent a
	// branch name. Both still fetch the same tree (the default branch), and the
	// recorded-ref fallback simply has nothing to fall back to in this case.
	let lock_source = skill::InstallLockSource {
		source: resolved.lock_source(),
		source_type: resolved.source_type.as_str().to_string(),
		source_url: resolved.source_url.clone(),
		ref_name: sync_plan.git_ref.clone(),
	};
	let fetched = skill_update::mutation::FetchedSource::from_repo(repo);

	let plan_targets = plan_target_agents(&plan, &target_agents);
	let action_report = aghub_core::batch::run_multi_target_mutation(
		&plan,
		|(kind, _)| {
			if *kind != "install" {
				return Ok(());
			}
			aghub_core::batch::skill_batch_preflight(&target_agents, scope)
				.map_err(|error| error.to_string())
		},
		|(kind, d)| {
			Ok::<SyncActionView, String>(match *kind {
				"install" => apply_install(
					&fetched,
					d,
					scope,
					project_root.as_deref(),
					&target_agents,
					&lock_source,
				),
				"update" => apply_update_row(
					&fetched,
					d,
					scope,
					project_root.as_deref(),
					&pre_fetch_identities,
				),
				_ => unreachable!(),
			})
		},
	)
	.map_err(|error| {
		let mut reasons = error
			.failures
			.into_iter()
			.map(|failure| failure.reason)
			.collect::<Vec<_>>();
		reasons.dedup();
		anyhow::anyhow!(reasons.join("; "))
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

fn print_dry_run(
	source: &str,
	scope_label: &'static str,
	plan: &[(&'static str, &SourceSkillDiff)],
	target_agents: &[AgentType],
	json: bool,
) -> Result<()> {
	let plan_targets = plan_target_agents(plan, target_agents);

	if json {
		let actions: Vec<SyncActionView> = plan
			.iter()
			.map(|(kind, d)| SyncActionView {
				action: kind,
				name: d.name.clone(),
				skill_path: d.skill_path.clone(),
				applied: false,
				error: None,
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
		return Ok(());
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
	for (kind, d) in plan {
		println!("  would {}: {} ({})", kind, d.name, d.skill_path);
	}
	Ok(())
}

fn apply_install(
	fetched: &skill_update::mutation::FetchedSource,
	d: &SourceSkillDiff,
	scope: ResourceScope,
	project_root: Option<&Path>,
	target_agents: &[AgentType],
	lock_source: &skill::InstallLockSource,
) -> SyncActionView {
	use skill_update::mutation::{
		install_fetched_source, FetchedInstallRequest, InstallMutationError,
	};

	let req = FetchedInstallRequest {
		source: lock_source,
		lock_skill_path: &d.skill_path,
		expected_name: Some(&d.name),
		scope,
		project_root,
		target_agents,
	};

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
			error: Some(match error {
				InstallMutationError::InvalidSkillPath => {
					"skillPath was not found in the source".to_string()
				}
				InstallMutationError::Install(error) => error.to_string(),
			}),
			error_code: None,
			agents: Vec::new(),
		},
	}
}

fn apply_update_row(
	fetched: &skill_update::mutation::FetchedSource,
	d: &SourceSkillDiff,
	scope: ResourceScope,
	project_root: Option<&Path>,
	pre_fetch: &std::collections::BTreeMap<String, EntryIdentity>,
) -> SyncActionView {
	use skill_update::mutation::{resync_fetched_source, FetchedResyncRequest};

	// No pre-fetch identity means this name was NOT in the lock when the fetch
	// started: another process installed it in between, and this sync has no
	// mandate to overwrite it.
	let Some(expected) = pre_fetch.get(&d.name).cloned() else {
		return SyncActionView {
			action: "update",
			name: d.name.clone(),
			skill_path: d.skill_path.clone(),
			applied: false,
			error: Some(
				"skill appeared in the lock while this sync was fetching; \
				 nothing was written"
					.to_string(),
			),
			// The API answers this exact condition with 409 +
			// SOURCE_CHANGED_DURING_FETCH. It is the same refusal for the same
			// reason, so it carries the same code.
			error_code: Some(
				aghub_core::skills::lock::SOURCE_CHANGED_DURING_FETCH_CODE,
			),
			agents: Vec::new(),
		};
	};

	match resync_fetched_source(
		fetched,
		FetchedResyncRequest {
			skill_path: &d.skill_path,
			name: &d.name,
			scope,
			project_root,
			expected,
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
/// Only the wording is this surface's: a CLI row can name the skill and quote
/// the underlying detail, where the HTTP API owes a path-free sentence. The
/// classification is `aghub_core::skills::resync::resync_error_code`, so a
/// `StaleFetch` — the source moving mid-fetch — now reaches a script here as
/// `SKILL_SOURCE_CHANGED_DURING_FETCH` instead of untyped prose.
fn resync_row_error(
	name: &str,
	error: skill_update::mutation::ResyncMutationError,
) -> (String, &'static str) {
	use aghub_core::skills::resync::{resync_error_code, ResyncError};
	use skill_update::mutation::ResyncMutationError;

	match error {
		// Not a `ResyncError` at all: the fetched tree simply has no such path.
		// Same code the API's git-sync route answers with.
		ResyncMutationError::InvalidSkillPath => (
			"locked skillPath was not found in source".to_string(),
			"SKILL_PATH_NOT_FOUND",
		),
		ResyncMutationError::Resync(error) => {
			let code = resync_error_code(&error);
			let message = match error {
				ResyncError::NotInstalled => format!(
					"skill '{name}' is locked but no installed copy was found"
				),
				ResyncError::Renamed { new_name } => {
					aghub_core::skills::update::skill_renamed_message(
						name, &new_name,
					)
				}
				other => other.to_string(),
			};
			(message, code)
		}
	}
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

/// `source accept-rename <old> <new>` — thin adapter over the core rename
/// transaction. Resolves scope + dry-run (CLI concerns), reads the lock source
/// and fetches (the fetch cannot live in core — `skill-update` depends on
/// core), then hands the fetched tree to `rename::accept_rename`.
fn accept_rename(args: AcceptRenameArgs) -> Result<()> {
	use aghub_core::skills::rename::{self, RenameRequest, RenameScope};
	use skill_update::mutation::{
		accept_fetched_rename, fetch_for_rename, FetchRenameError,
		FetchedRenameRequest,
	};

	// Scope was resolved and validated ONCE, in `main`; `write_target` refuses
	// anything that is not a single write target instead of defaulting to
	// global.
	let scope = match args.scope.write_target()? {
		Some(root) => RenameScope::Project {
			root: root.to_path_buf(),
		},
		None => RenameScope::Global,
	};
	let scope_label = args.scope.label();

	// P0-2 guard (a): refuse a degenerate rename before any lock read / fetch.
	//
	// Both this guard and the lock read below used to sit AFTER the dry-run
	// return, so the preview green-lit a rename of a name that is not in the
	// lock at all (and a degenerate `a → a`); the caller only hit the wall on
	// the `--yes` run. They validate, they do not write, so they belong in
	// front of the preview.
	rename::ensure_distinct_names(args.old_name, args.new_name)
		.map_err(|e| anyhow::anyhow!("{}", e.message()))?;

	// Step 1: read the OLD-name lock entry for the fetch coordinates.
	let mut source = rename::rename_source_from_lock(args.old_name, &scope)
		.map_err(|e| anyhow::anyhow!("{}", e.message()))?;

	if !args.yes {
		// The preview MUST honour --json. This branch used to `println!` prose
		// unconditionally and return Ok(()) — a destructive command's DEFAULT
		// path emitting unparseable text on exit 0, which a strict parser reads
		// as a crash on the success path and a lenient one reads as "the rename
		// was committed". Every sibling preview (delete, prune-lock, source
		// sync, reconcile) already emits JSON here. Keys match the --yes
		// payload below, plus `applied` so the two are machine-distinguishable.
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

	// Apply any --ref override: the effective ref is fetched AND written to the
	// new lock entry.
	let effective_ref =
		args.git_ref.map(str::to_string).or(source.ref_name.clone());
	source.ref_name = effective_ref.clone();

	// Step 3: fetch a catalog snapshot and resolve the new frontmatter name to
	// its current path. The directory may have moved as part of the rename.
	let prepared = fetch_for_rename(
		FetchedRenameRequest {
			source: &source,
			new_name: args.new_name,
		},
		&CliFetcher,
		&EnvTokenResolver,
	)
	.map_err(|error| match error {
		FetchRenameError::CredentialBackendUnavailable => {
			anyhow::anyhow!("Credential backend is unavailable; retry later.")
		}
		FetchRenameError::Fetch(FetchError::Auth) => anyhow::anyhow!(
			"This source needs a credential. Set GIT_PASSWORD (any host) \
			 or GITHUB_TOKEN (github.com) in the environment and retry."
		),
		FetchRenameError::Fetch(FetchError::Network(detail)) => {
			anyhow::anyhow!(
				"Failed to fetch source repository '{}': {}",
				safe_source(&source.source_url),
				safe_source(&detail)
			)
		}
		FetchRenameError::Fetch(FetchError::BackendUnavailable) => {
			anyhow::anyhow!("Credential backend is unavailable; retry later.")
		}
		FetchRenameError::CatalogScan => {
			anyhow::anyhow!(
				"Fetched source catalog could not be scanned safely"
			)
		}
		FetchRenameError::SkillNotFound => anyhow::anyhow!(
			"new skill '{}' was not found in the fetched source",
			args.new_name
		),
	})?;

	// Steps 2/4/5/6/7/8/9 + P0 guards + rollback all live in core.
	let outcome = accept_fetched_rename(
		&prepared.fetched,
		RenameRequest {
			old_name: args.old_name,
			new_name: args.new_name,
			scope,
		},
		&prepared.source,
	)
	.map_err(|e| anyhow::anyhow!("{}", e.message()))?;

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
		diff_with, narrow_by_name, plan_target_agents, resync_row_error,
		select_env_token, FetchError, PathBuf, SyncActionView,
	};
	use aghub_core::models::AgentType;
	use skill_update::sources::{
		SourceScope, SourceSkillDiff, SourceSkillState,
	};

	fn s(v: &str) -> Option<String> {
		Some(v.to_string())
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
		use skill_update::{FetchSelection, Fetcher, SourceRef};

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
			SourceScope::Project {
				root: locked.path().to_path_buf(),
			},
			SourceScope::Project {
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
}
