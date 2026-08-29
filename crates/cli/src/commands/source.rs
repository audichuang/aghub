//! `aghub-cli source <list|diff|sync>` — manage the git sources you've
//! installed skills from, scoped to the current project + global.
//!
//! `list`/`diff` are read-only. `sync` defaults to a dry-run and only writes
//! with `--yes`. The Sources domain (list + per-skill classification) and the
//! no-network install primitive live in shared crates (`skill_update::sources`
//! / `aghub_core::skills::install_fetched`); this module is the CLI surface:
//! scope resolution, an env-backed credential resolver, a debug-only fetch
//! hook for tests, dry-run/`--yes` gating, and output rendering.

use std::path::{Path, PathBuf};

use aghub_core::models::{AgentSelection, AgentType, ResourceScope};
use aghub_core::paths::find_project_root;
use aghub_core::skills::lock::EntryIdentity;
use anyhow::{bail, Result};
use serde::Serialize;
use skill_update::sources::{
	self, SourceScope, SourceScopeKind, SourceSkillDiff, SourceSummary,
};
use skill_update::{FetchError, FetchSelection, SourceRef};
use tabled::builder::Builder;
use tabled::settings::Style;

use crate::SourceAction;

/// `source diff` / `source sync` fetch the repository ONCE and reuse that tree
/// for every matched entry. A Sources row is one repository but its entries need
/// not share a branch, so that is only sound when they do: judged against
/// `main`, a `v1`-pinned skill reads as outdated forever (its own update fetches
/// `v1`, so the hash never converges) or as removed when its folder is absent
/// there — and `--update` would overwrite it with `main`'s content.
///
/// An explicit `--ref` IS the caller picking one tree, so it passes.
///
/// Takes ALL the scopes the command will classify, not one at a time: the fetch
/// is shared across them, so a global entry on `main` and a project entry on
/// `v1` is exactly the hazard even though each scope alone looks uniform.
///
/// The API `/sources/diff` does not need this: it owns its fetches and splits
/// into one cohort per ref (`skill_update::sources::baseline_by_ref`). Teaching
/// this command to do the same means holding one fetched tree per ref through
/// the install/update flow — see `.scratch/source-grouping/spec.md`.
fn assert_one_tree_can_serve(
	source: &str,
	scopes: &[SourceScope],
	git_ref: Option<&str>,
) -> Result<()> {
	if git_ref.is_some() {
		return Ok(());
	}
	let refs = scopes
		.iter()
		.flat_map(|scope| sources::scope_ref_cohorts(scope, source))
		.collect::<std::collections::BTreeSet<_>>();
	if refs.len() < 2 {
		return Ok(());
	}
	bail!(
		"source '{}' has skills pinned to {} different refs:\n  {}\nRun \
		 it again with --ref to work on one of them.",
		safe_source(source),
		refs.len(),
		refs.iter()
			.map(|name| name.as_deref().unwrap_or("(default branch)"))
			.collect::<Vec<_>>()
			.join("\n  ")
	)
}

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

// ──────────────────────────── scope resolution ─────────────────────────────

/// Resolve the read scopes for `list`/`diff` from the global flags:
/// `-g` → global only; `-p` → project only; otherwise global plus the current
/// project (when a project root is detected).
pub(crate) fn resolve_read_scopes(
	global: bool,
	project: bool,
) -> Result<Vec<SourceScope>> {
	if global && project {
		bail!("choose either -g or -p, not both");
	}
	if global {
		return Ok(vec![SourceScope::Global]);
	}
	let project_root = current_project_root()?;
	if project {
		return match project_root {
			Some(root) => Ok(vec![SourceScope::Project { root }]),
			None => bail!(
				"no project root found (need an agent marker like .claude/, \
				 .opencode/, .mcp.json, …)"
			),
		};
	}
	let mut scopes = vec![SourceScope::Global];
	if let Some(root) = project_root {
		scopes.push(SourceScope::Project { root });
	}
	Ok(scopes)
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

fn current_project_root() -> Result<Option<PathBuf>> {
	let cwd = std::env::current_dir()?;
	Ok(find_project_root(&cwd))
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
	global: bool,
	project: bool,
	all: bool,
	agent: &str,
	json: bool,
) -> Result<()> {
	match action {
		SourceAction::List => list(global, project, json),
		SourceAction::Diff {
			source,
			git_ref,
			// Accepted and ignored — `diff` has no offline mode.
			online: _,
		} => diff(source, git_ref.as_deref(), global, project, json),
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
			global,
			project,
			all,
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
			global,
			project,
			all,
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

fn list(global: bool, project: bool, json: bool) -> Result<()> {
	let scopes = resolve_read_scopes(global, project)?;
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
	global: bool,
	project: bool,
	json: bool,
) -> Result<()> {
	let scopes = resolve_read_scopes(global, project)?;
	// The snapshot is discarded here — see `read_scope_locks_checked`. What is
	// kept is the fail-closed check: a corrupt lock must not read as "no
	// sources installed".
	read_scope_locks_checked(&scopes)?;
	let source = source.trim().to_string();

	// Resolve `(source_type, effective_ref)` from the lock entries via the SHARED
	// helper — the SAME resolution the API `diff_source` runs — so the CLI checks
	// the recorded ref (not the default branch) and prechecks with the recorded
	// source_type (not a hard-coded "github"). No fetch happens here.
	let meta = sources::resolve_source_meta(&source, &scopes, git_ref);

	assert_one_tree_can_serve(&source, &scopes, git_ref)?;

	// Skip sources we cannot fetch (local/ssh/unsupported scheme) up front,
	// before paying for a fetch — honoring the precheck the API path honors.
	// One fetched tree cannot judge two forges. `source list` prints the lock's
	// host-blind SOURCE, and pasting that back selects every forge serving the
	// same path — so refuse rather than fetch one of them and apply it to all.
	if let Some(origins) = meta.ambiguous_origins() {
		bail!(
			"source '{}' matches {} repositories:\n  {}\nRun it again \
			 with the SOURCE_URL of the one you mean.",
			safe_source(&source),
			origins.len(),
			origins.join("\n  ")
		);
	}

	if let Some(reason) = aghub_core::skills::update::precheck_source(
		&meta.source_type,
		meta.effective_source.as_deref().unwrap_or(&source),
	) {
		bail!(
			"source '{}' cannot be fetched ({reason:?}); only HTTPS / \
			 owner/repo git sources are supported",
			safe_source(&source)
		);
	}

	let repo = match sources::fetch_source_with_resolver(
		&SourceRef {
			source: meta
				.effective_source
				.clone()
				.unwrap_or_else(|| source.clone()),
			ref_: meta.effective_ref.clone(),
		},
		&CliFetcher,
		&EnvTokenResolver,
		FetchSelection::CatalogSnapshot,
	) {
		Ok(repo) => repo,
		Err(FetchError::BackendUnavailable) => {
			bail!("Credential backend is unavailable; retry later.")
		}
		Err(FetchError::Auth) => bail!(
			"Could not read this source. Either it needs a credential (set \
			 GIT_PASSWORD for any host, or GITHUB_TOKEN for github.com, in \
			 the environment and retry) or the repo/ref does not exist or is \
			 not visible to the credential already in use."
		),
		Err(FetchError::Network(detail)) => {
			bail!(
				"Failed to fetch source repository '{}': {}",
				safe_source(&source),
				safe_source(&detail)
			)
		}
	};

	let per_scope: Vec<(&SourceScope, Vec<SourceSkillDiff>)> = scopes
		.iter()
		.map(|scope| {
			let diffs = sources::classify_scope(
				repo.root.as_path(),
				scope,
				&source,
				repo.upstream_commit_time(),
			);
			(scope, diffs)
		})
		.collect();

	if json {
		let views: Vec<DiffScopeView> = per_scope
			.iter()
			.map(|(scope, diffs)| DiffScopeView {
				scope: scope_label(scope),
				skills: diffs.iter().map(diff_skill_to_view).collect(),
			})
			.collect();
		println!("{}", serde_json::to_string_pretty(&views)?);
		return Ok(());
	}

	let mut builder = Builder::default();
	builder.push_record(["STATE", "NAME", "SKILL_PATH", "SCOPE"]);
	for (scope, diffs) in &per_scope {
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
	Ok(())
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
	global: bool,
	project: bool,
	all: bool,
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
	/// Per-agent breakdown (install actions only; empty for update). Lets a
	/// multi-agent (`-a all`) sync show exactly which agents were linked vs
	/// already-present vs failed, instead of a single collapsed status.
	#[serde(skip_serializing_if = "Vec::is_empty")]
	agents: Vec<AgentResultView>,
}

impl SyncActionView {
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

/// Resolve the single writing scope for `sync`. Exactly one of `-g`/`-p` must
/// be chosen; `--all` and an unscoped invocation are rejected.
fn resolve_write_scope(
	args: &SyncArgs,
) -> Result<(ResourceScope, Option<PathBuf>, SourceScope, &'static str)> {
	if args.all {
		bail!("`source sync` needs exactly one scope; --all is not allowed");
	}
	if args.global && args.project {
		bail!("choose either -g or -p, not both");
	}
	if args.global {
		return Ok((
			ResourceScope::GlobalOnly,
			None,
			SourceScope::Global,
			"global",
		));
	}
	if args.project {
		let root = current_project_root()?.ok_or_else(|| {
			anyhow::anyhow!(
				"no project root found (need an agent marker like .claude/, \
				 .opencode/, .mcp.json, …)"
			)
		})?;
		return Ok((
			ResourceScope::ProjectOnly,
			Some(root.clone()),
			SourceScope::Project { root },
			"project",
		));
	}
	bail!("`source sync` needs a scope: pass -g (global) or -p (project)")
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

	let (scope, project_root, source_scope, scope_label) =
		resolve_write_scope(&args)?;

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

	// Snapshot every in-scope lock entry's identity FIRST, before the resolution
	// below reads those same entries to decide what to fetch. Captured (never
	// reconstructed) because a project entry's `sourceUrl` is optional — an
	// npx-written one has none, and its effective source is the `owner/repo`
	// shorthand, so comparing a rebuilt HTTPS URL would falsely reject it.
	//
	// Ordered before the resolution on purpose: if another process repoints an
	// entry between the two reads, the identity is the OLDER observation, so the
	// compare-after-fetch refuses rather than approving a fetch that used the
	// newer coordinates. The reverse order approves exactly that mismatch.
	let pre_fetch_identities =
		capture_scope_identities(scope, project_root.as_deref());

	// Resolve `(source_type, effective_ref)` from the lock entries via the SHARED
	// helper (the SAME resolution the API runs) BEFORE the single fetch, so sync
	// fetches/installs from the recorded ref — not the default branch — and
	// prechecks with the recorded source_type rather than a hard-coded "github".
	let meta = sources::resolve_source_meta(
		&source,
		std::slice::from_ref(&source_scope),
		args.git_ref,
	);

	assert_one_tree_can_serve(
		&source,
		std::slice::from_ref(&source_scope),
		args.git_ref,
	)?;

	// One fetched tree cannot judge two forges. `source list` prints the lock's
	// host-blind SOURCE, and pasting that back selects every forge serving the
	// same path — so refuse rather than fetch one of them and apply it to all.
	if let Some(origins) = meta.ambiguous_origins() {
		bail!(
			"source '{}' matches {} repositories:\n  {}\nRun it again \
			 with the SOURCE_URL of the one you mean.",
			safe_source(&source),
			origins.len(),
			origins.join("\n  ")
		);
	}

	if let Some(reason) = aghub_core::skills::update::precheck_source(
		&meta.source_type,
		meta.effective_source.as_deref().unwrap_or(&source),
	) {
		bail!(
			"source '{}' cannot be fetched ({reason:?}); only HTTPS / \
			 owner/repo git sources are supported",
			safe_source(&source)
		);
	}

	// Fetch ONCE; reuse the repo for classification AND every install/update.
	// Fetch + classify happen BEFORE the flag branch so the neither-flag
	// informational path can print the same plan without a second fetch.
	// CatalogSnapshot: classify needs the whole tree (renames/removals).
	let repo = match sources::fetch_source_with_resolver(
		&SourceRef {
			source: meta
				.effective_source
				.clone()
				.unwrap_or_else(|| source.clone()),
			ref_: meta.effective_ref.clone(),
		},
		&CliFetcher,
		&EnvTokenResolver,
		FetchSelection::CatalogSnapshot,
	) {
		Ok(repo) => repo,
		Err(FetchError::BackendUnavailable) => {
			bail!("Credential backend is unavailable; retry later.")
		}
		Err(FetchError::Auth) => bail!(
			"Could not read this source. Either it needs a credential (set \
			 GIT_PASSWORD for any host, or GITHUB_TOKEN for github.com, in \
			 the environment and retry) or the repo/ref does not exist or is \
			 not visible to the credential already in use."
		),
		Err(FetchError::Network(detail)) => {
			bail!(
				"Failed to fetch source repository '{}': {}",
				safe_source(&source),
				safe_source(&detail)
			)
		}
	};

	let diffs = sources::classify_scope(
		repo.root.as_path(),
		&source_scope,
		&source,
		repo.upstream_commit_time(),
	);

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
			use aghub_core::skills::linker::{
				agent_link_need, universal_canonical_dir, LinkNeed,
			};
			let master = universal_canonical_dir(project_root.as_deref())
				.ok_or_else(|| {
					anyhow::anyhow!(
						"could not resolve the universal master skills \
						 directory"
					)
				})?;
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
						agent_link_need(
							d,
							scope,
							project_root.as_deref(),
							&master,
						),
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
	let fetch_source = meta
		.effective_source
		.clone()
		.unwrap_or_else(|| source.clone());
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
		ref_name: meta.effective_ref.clone(),
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
			agents: Vec::new(),
		},
	}
}

/// Every in-scope lock entry's identity, keyed by skill name. Taken before a
/// fetch; see the call site.
fn capture_scope_identities(
	scope: ResourceScope,
	project_root: Option<&Path>,
) -> std::collections::BTreeMap<String, EntryIdentity> {
	let names: Vec<String> = match scope {
		ResourceScope::ProjectOnly => project_root
			.map(|root| {
				skill::lock::local::read_local_lock(Some(root))
					.skills
					.keys()
					.cloned()
					.collect()
			})
			.unwrap_or_default(),
		_ => skill::get_all_locked_skills().keys().cloned().collect(),
	};
	names
		.into_iter()
		.filter_map(|name| {
			EntryIdentity::capture(&name, scope, project_root)
				.map(|id| (name, id))
		})
		.collect()
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
			agents: Vec::new(),
		},
		Err(error) => SyncActionView {
			action: "update",
			name: d.name.clone(),
			skill_path: d.skill_path.clone(),
			applied: false,
			error: Some(resync_row_error(&d.name, error)),
			agents: Vec::new(),
		},
	}
}

/// Map a sync update-row resync failure to its user-facing row message.
fn resync_row_error(
	name: &str,
	error: skill_update::mutation::ResyncMutationError,
) -> String {
	use aghub_core::skills::resync::ResyncError;
	use skill_update::mutation::ResyncMutationError;

	match error {
		ResyncMutationError::InvalidSkillPath => {
			"locked skillPath was not found in source".to_string()
		}
		ResyncMutationError::Resync(ResyncError::NotInstalled) => {
			format!("skill '{name}' is locked but no installed copy was found")
		}
		ResyncMutationError::Resync(ResyncError::Renamed { new_name }) => {
			aghub_core::skills::update::skill_renamed_message(name, &new_name)
		}
		ResyncMutationError::Resync(other) => other.to_string(),
	}
}

// ──────────────────────────── source accept-rename ─────────────────────────

struct AcceptRenameArgs<'a> {
	old_name: &'a str,
	new_name: &'a str,
	git_ref: Option<&'a str>,
	yes: bool,
	json: bool,
	global: bool,
	project: bool,
	all: bool,
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

	if args.all {
		bail!(
			"`source accept-rename` needs exactly one scope; --all is not allowed"
		);
	}
	if args.global && args.project {
		bail!("choose either -g or -p, not both");
	}
	let scope = if args.project {
		let root = current_project_root()?.ok_or_else(|| {
			anyhow::anyhow!(
				"no project root found (need an agent marker like .claude/, \
				 .opencode/, .mcp.json, …)"
			)
		})?;
		RenameScope::Project { root }
	} else {
		RenameScope::Global
	};
	let scope_label = if args.project { "project" } else { "global" };

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
		assert_one_tree_can_serve, narrow_by_name, plan_target_agents,
		resync_row_error, select_env_token,
	};
	use aghub_core::models::AgentType;
	use skill_update::sources::{
		SourceScope, SourceSkillDiff, SourceSkillState,
	};

	fn s(v: &str) -> Option<String> {
		Some(v.to_string())
	}

	fn lock_entry(ref_name: &str) -> skill::LocalSkillLockEntry {
		skill::LocalSkillLockEntry {
			source: "owner/repo".to_string(),
			ref_name: Some(ref_name.to_string()),
			source_type: "github".to_string(),
			skill_path: None,
			computed_hash: "h".to_string(),
			ref_commit: None,
			source_url: None,
		}
	}

	/// `source diff`/`sync` fetch ONE tree and reuse it for every entry, so they
	/// must refuse a scope whose entries are pinned to different refs — judged
	/// against the wrong ref a skill reads as outdated forever, or as removed,
	/// and `--update` overwrites it with the other ref's content. An explicit
	/// `--ref` is the caller picking one tree, so it passes.
	#[test]
	fn a_scope_spanning_two_refs_is_refused_without_an_explicit_ref() {
		let project = tempfile::tempdir().unwrap();
		let mut lock = skill::LocalSkillLockFile::new();
		for (name, ref_name) in [("alpha", "main"), ("zeta", "v1")] {
			let mut entry = lock_entry(ref_name);
			entry.skill_path = Some(format!("{name}/SKILL.md"));
			lock.skills.insert(name.to_string(), entry);
		}
		skill::write_local_lock(&lock, Some(project.path())).unwrap();
		let scope = SourceScope::Project {
			root: project.path().to_path_buf(),
		};

		let refused = assert_one_tree_can_serve(
			"owner/repo",
			std::slice::from_ref(&scope),
			None,
		)
		.expect_err("two refs cannot share one fetched tree");
		let message = refused.to_string();
		assert!(message.contains("main"), "{message}");
		assert!(message.contains("v1"), "{message}");

		assert!(
			assert_one_tree_can_serve("owner/repo", &[scope], Some("main"))
				.is_ok(),
			"an explicit --ref picks one tree"
		);
	}

	/// The refs must be unioned ACROSS the scopes the command will classify, not
	/// checked one scope at a time: `diff` fetches once and classifies every
	/// scope against that tree, so one scope on `main` and another on `v1` is the
	/// hazard even though each looks uniform on its own.
	#[test]
	fn refs_are_unioned_across_scopes_not_checked_per_scope() {
		let uniform_scope = |ref_name: &str| {
			let dir = tempfile::tempdir().unwrap();
			let mut lock = skill::LocalSkillLockFile::new();
			let mut entry = lock_entry(ref_name);
			entry.skill_path = Some("alpha/SKILL.md".to_string());
			lock.skills.insert("alpha".to_string(), entry);
			skill::write_local_lock(&lock, Some(dir.path())).unwrap();
			let scope = SourceScope::Project {
				root: dir.path().to_path_buf(),
			};
			(dir, scope)
		};
		let (_a, on_main) = uniform_scope("main");
		let (_b, on_v1) = uniform_scope("v1");

		// Each scope alone is uniform, so a per-scope check passes both.
		for scope in [&on_main, &on_v1] {
			assert!(assert_one_tree_can_serve(
				"owner/repo",
				std::slice::from_ref(scope),
				None,
			)
			.is_ok());
		}
		let refused =
			assert_one_tree_can_serve("owner/repo", &[on_main, on_v1], None)
				.expect_err(
					"one fetched tree serves both scopes, so both count",
				);
		let message = refused.to_string();
		assert!(message.contains("main"), "{message}");
		assert!(message.contains("v1"), "{message}");
	}

	/// The guard must not fire on the ordinary single-ref source — it would make
	/// every `source diff` bail.
	#[test]
	fn one_ref_passes() {
		let project = tempfile::tempdir().unwrap();
		let mut lock = skill::LocalSkillLockFile::new();
		for name in ["alpha", "zeta"] {
			let mut entry = lock_entry("main");
			entry.skill_path = Some(format!("{name}/SKILL.md"));
			lock.skills.insert(name.to_string(), entry);
		}
		skill::write_local_lock(&lock, Some(project.path())).unwrap();

		assert!(assert_one_tree_can_serve(
			"owner/repo",
			&[SourceScope::Project {
				root: project.path().to_path_buf(),
			}],
			None,
		)
		.is_ok());
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
			resync_row_error("keep", ResyncMutationError::InvalidSkillPath),
			"locked skillPath was not found in source"
		);
		assert_eq!(
			resync_row_error(
				"keep",
				ResyncMutationError::Resync(ResyncError::NotInstalled)
			),
			"skill 'keep' is locked but no installed copy was found"
		);
		let renamed = resync_row_error(
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
