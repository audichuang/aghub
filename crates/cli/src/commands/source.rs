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

use aghub_core::models::{AgentType, ResourceScope};
use aghub_core::paths::find_project_root;
use anyhow::{bail, Context, Result};
use serde::Serialize;
use skill_update::sources::{
	self, SourceScope, SourceScopeKind, SourceSkillDiff, SourceSummary,
};
use skill_update::{FetchError, SourceRef};
use tabled::builder::Builder;
use tabled::settings::Style;

use crate::SourceAction;

// ─────────────────────────── credential / fetch ────────────────────────────

/// Token resolver for CLI source auth. `GIT_PASSWORD` is explicit user
/// intent and applies to ANY host (self-hosted GitLab / TFS / local test
/// remotes must keep working). `GITHUB_TOKEN` is GitHub-specific by name,
/// so it is only offered when the source host is github.com or a subdomain
/// — the fetch-then-retry-with-token flow would otherwise send the PAT to
/// an arbitrary host after the first failure. Empty/whitespace env values
/// count as unset. `GitFetcher` consumes the token as the `x-access-token`
/// password — there is no username/password basic-auth path. Returns
/// `None` when nothing applies (the unauthenticated attempt stands).
struct EnvTokenResolver;
impl skill_update::TokenResolver for EnvTokenResolver {
	fn resolve(&self, _source: &str, host: Option<&str>) -> Option<String> {
		select_env_token(
			std::env::var("GIT_PASSWORD").ok(),
			std::env::var("GITHUB_TOKEN").ok(),
			host,
		)
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
	let host = host.to_ascii_lowercase();
	host == "github.com" || host.ends_with(".github.com")
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
	) -> Result<skill_update::FetchedRepo, FetchError> {
		#[cfg(debug_assertions)]
		if let Some(root) = std::env::var_os("AGHUB_TEST_SOURCE_FETCH_ROOT") {
			let root = PathBuf::from(root);
			return if root.is_dir() {
				Ok(skill_update::FetchedRepo {
					root,
					oid: "test-fetch-root".into(),
					upstream_commit_time: None,
					_guard: None,
				})
			} else {
				Err(FetchError::Network)
			};
		}
		skill_update::GitFetcher.fetch(sr, token)
	}
}

// ──────────────────────────── scope resolution ─────────────────────────────

/// Resolve the read scopes for `list`/`diff` from the global flags:
/// `-g` → global only; `-p` → project only; otherwise global plus the current
/// project (when a project root is detected).
fn resolve_read_scopes(
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

fn scope_label(scope: &SourceScope) -> &'static str {
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
) -> Result<()> {
	match action {
		SourceAction::List { json } => list(global, project, *json),
		SourceAction::Diff {
			source,
			git_ref,
			json,
		} => diff(source, git_ref.as_deref(), global, project, *json),
		SourceAction::Sync {
			source,
			git_ref,
			update,
			install_missing,
			universal,
			yes,
			json,
		} => sync(SyncArgs {
			source,
			git_ref: git_ref.as_deref(),
			update: *update,
			install_missing: *install_missing,
			universal: *universal,
			yes: *yes,
			json: *json,
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
			json,
		} => accept_rename(AcceptRenameArgs {
			old_name,
			new_name,
			git_ref: git_ref.as_deref(),
			yes: *yes,
			json: *json,
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
	let source = source.trim().to_string();

	// Resolve `(source_type, effective_ref)` from the lock entries via the SHARED
	// helper — the SAME resolution the API `diff_source` runs — so the CLI checks
	// the recorded ref (not the default branch) and prechecks with the recorded
	// source_type (not a hard-coded "github"). No fetch happens here.
	let meta = sources::resolve_source_meta(&source, &scopes, git_ref);

	// Skip sources we cannot fetch (local/ssh/unsupported scheme) up front,
	// before paying for a fetch — honoring the precheck the API path honors.
	if let Some(reason) =
		aghub_core::skills::update::precheck_source(&meta.source_type, &source)
	{
		bail!(
			"source '{source}' cannot be fetched ({reason:?}); only HTTPS / \
			 owner/repo git sources are supported"
		);
	}

	let repo = match sources::fetch_source_with_resolver(
		&SourceRef {
			source: source.clone(),
			ref_: meta.effective_ref.clone(),
		},
		&CliFetcher,
		&EnvTokenResolver,
	) {
		Ok(repo) => repo,
		Err(FetchError::Auth) => bail!(
			"This source needs a credential. Set GIT_PASSWORD / GITHUB_TOKEN, \
			 or bind a credential in the desktop app."
		),
		Err(FetchError::Network) => {
			bail!("Failed to fetch source repository '{source}'")
		}
	};

	let per_scope: Vec<(&SourceScope, Vec<SourceSkillDiff>)> = scopes
		.iter()
		.map(|scope| {
			let diffs = sources::classify_scope(
				repo.root.as_path(),
				scope,
				&source,
				repo.upstream_commit_time.clone(),
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
	universal: bool,
	yes: bool,
	json: bool,
	global: bool,
	project: bool,
	all: bool,
	agent: &'a str,
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
}

#[derive(Serialize)]
struct SyncOutcomeView {
	source: String,
	scope: &'static str,
	#[serde(rename = "dryRun")]
	dry_run: bool,
	actions: Vec<SyncActionView>,
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

	// Resolve `(source_type, effective_ref)` from the lock entries via the SHARED
	// helper (the SAME resolution the API runs) BEFORE the single fetch, so sync
	// fetches/installs from the recorded ref — not the default branch — and
	// prechecks with the recorded source_type rather than a hard-coded "github".
	let meta = sources::resolve_source_meta(
		&source,
		std::slice::from_ref(&source_scope),
		args.git_ref,
	);

	if let Some(reason) =
		aghub_core::skills::update::precheck_source(&meta.source_type, &source)
	{
		bail!(
			"source '{source}' cannot be fetched ({reason:?}); only HTTPS / \
			 owner/repo git sources are supported"
		);
	}

	// Fetch ONCE; reuse the repo for classification AND every install/update.
	// Fetch + classify happen BEFORE the flag branch so the neither-flag
	// informational path can print the same plan without a second fetch.
	let repo = match sources::fetch_source_with_resolver(
		&SourceRef {
			source: source.clone(),
			ref_: meta.effective_ref.clone(),
		},
		&CliFetcher,
		&EnvTokenResolver,
	) {
		Ok(repo) => repo,
		Err(FetchError::Auth) => bail!(
			"This source needs a credential. Set GIT_PASSWORD / GITHUB_TOKEN, \
			 or bind a credential in the desktop app."
		),
		Err(FetchError::Network) => {
			bail!("Failed to fetch source repository '{source}'")
		}
	};

	let diffs = sources::classify_scope(
		repo.root.as_path(),
		&source_scope,
		&source,
		repo.upstream_commit_time.clone(),
	);

	// Neither flag: print the plan (per-state overview) and ask the user to
	// choose an action. Read-only/informational — write NOTHING.
	if !args.update && !args.install_missing {
		return print_no_action_plan(&source, scope_label, &diffs, args.json);
	}

	// Parse the target agent the same way the top-level `-a` does.
	let target = args
		.agent
		.parse::<AgentType>()
		.map_err(|e| anyhow::anyhow!("Unknown agent type: {e}"))?;
	let target_agents = [target];

	// Build the plan. `--install-missing` targets only `NotInstalled` rows
	// (excludes Deprecated/Renamed/Removed); `--update` targets
	// `InstalledOutdated` rows.
	use skill_update::sources::SourceSkillState as St;
	let mut plan: Vec<(&'static str, &SourceSkillDiff)> = Vec::new();
	if args.install_missing {
		for d in diffs.iter().filter(|d| d.state == St::NotInstalled) {
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
		return print_dry_run(&source, scope_label, &plan, args.json);
	}

	// Resolve the normalized lock source ONCE. Source normalization lives in
	// `aghub_git`; we never re-implement it.
	let resolved = aghub_git::resolve_remote_source(&source)
		.map_err(|e| anyhow::anyhow!("invalid source '{source}': {e}"))?;
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

	let mut actions: Vec<SyncActionView> = Vec::new();
	for (kind, d) in &plan {
		match *kind {
			"install" => actions.push(apply_install(
				&repo,
				d,
				scope,
				project_root.as_deref(),
				&target_agents,
				args.universal,
				&lock_source,
			)),
			"update" => actions.push(apply_update_row(
				&repo,
				d,
				scope,
				project_root.as_deref(),
			)),
			_ => unreachable!(),
		}
	}

	if args.json {
		let view = SyncOutcomeView {
			source,
			scope: scope_label,
			dry_run: false,
			actions,
		};
		println!("{}", serde_json::to_string_pretty(&view)?);
	} else {
		for a in &actions {
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
		if actions.is_empty() {
			println!("Nothing to do.");
		}
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
	json: bool,
) -> Result<()> {
	if json {
		let actions: Vec<SyncActionView> = plan
			.iter()
			.map(|(kind, d)| SyncActionView {
				action: kind,
				name: d.name.clone(),
				skill_path: d.skill_path.clone(),
				applied: false,
				error: None,
			})
			.collect();
		let view = SyncOutcomeView {
			source: source.to_string(),
			scope: scope_label,
			dry_run: true,
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
	for (kind, d) in plan {
		println!("  would {}: {} ({})", kind, d.name, d.skill_path);
	}
	Ok(())
}

fn apply_install(
	repo: &skill_update::FetchedRepo,
	d: &SourceSkillDiff,
	scope: ResourceScope,
	project_root: Option<&Path>,
	target_agents: &[AgentType],
	_universal: bool,
	lock_source: &skill::InstallLockSource,
) -> SyncActionView {
	use aghub_core::skills::install_fetched::{
		install_fetched_skill_and_lock, FetchedSkillInstallRequest,
	};
	use aghub_core::skills::linker::LinkTarget;

	let Some(skill_file) = aghub_core::skills::update::sanitize_skill_path(
		repo.root.as_path(),
		&d.skill_path,
	) else {
		return SyncActionView {
			action: "install",
			name: d.name.clone(),
			skill_path: d.skill_path.clone(),
			applied: false,
			error: Some("skillPath was not found in the source".to_string()),
		};
	};

	let req = FetchedSkillInstallRequest {
		skill_file: &skill_file,
		source: lock_source,
		lock_skill_path: d.skill_path.clone(),
		ref_commit: Some(repo.oid.clone()),
		scope,
		project_root,
		target_agents,
		expected_name: Some(&d.name),
		target: if matches!(scope, ResourceScope::ProjectOnly) {
			LinkTarget::Relative
		} else {
			LinkTarget::Absolute
		},
	};

	match install_fetched_skill_and_lock(req) {
		Ok(report) => {
			let applied = report.agent_results.iter().any(|r| r.installed);
			let error =
				report.agent_results.iter().find_map(|r| r.error.clone());
			SyncActionView {
				action: "install",
				name: d.name.clone(),
				skill_path: d.skill_path.clone(),
				applied,
				error: if applied { None } else { error },
			}
		}
		Err(e) => SyncActionView {
			action: "install",
			name: d.name.clone(),
			skill_path: d.skill_path.clone(),
			applied: false,
			error: Some(e.to_string()),
		},
	}
}

fn apply_update_row(
	repo: &skill_update::FetchedRepo,
	d: &SourceSkillDiff,
	scope: ResourceScope,
	project_root: Option<&Path>,
) -> SyncActionView {
	match crate::commands::apply_update::apply_skill_update_from_fetched(
		repo.root.as_path(),
		&d.skill_path,
		&d.name,
		scope,
		project_root,
		Some(&repo.oid),
	) {
		Ok(paths) => SyncActionView {
			action: "update",
			name: d.name.clone(),
			skill_path: d.skill_path.clone(),
			applied: !paths.is_empty(),
			error: None,
		},
		Err(e) => SyncActionView {
			action: "update",
			name: d.name.clone(),
			skill_path: d.skill_path.clone(),
			applied: false,
			error: Some(e.to_string()),
		},
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

/// Source coordinates of the OLD-name lock entry, plus the fields needed to
/// re-install under the new name. Mirrors the API's `RenameLockSource`.
struct RenameLockSource {
	source: String,
	source_type: String,
	source_url: String,
	ref_name: Option<String>,
	skill_path: String,
}

/// Read the OLD-name lock entry from the chosen scope's lock.
fn rename_source_from_lock(
	name: &str,
	scope: ResourceScope,
	project_root: Option<&Path>,
) -> Result<RenameLockSource> {
	match scope {
		ResourceScope::GlobalOnly => {
			let lock = skill::read_skill_lock();
			let entry = lock.skills.get(name).ok_or_else(|| {
				anyhow::anyhow!("'{name}' is not in the global lock")
			})?;
			let skill_path = entry.skill_path.clone().ok_or_else(|| {
				anyhow::anyhow!("locked entry has no skillPath")
			})?;
			Ok(RenameLockSource {
				source: entry.source.clone(),
				source_type: entry.source_type.clone(),
				source_url: entry.source_url.clone(),
				ref_name: entry.ref_name.clone(),
				skill_path,
			})
		}
		ResourceScope::ProjectOnly => {
			let root = project_root
				.ok_or_else(|| anyhow::anyhow!("project_root is required"))?;
			let lock = skill::read_local_lock(Some(root));
			let entry = lock.skills.get(name).ok_or_else(|| {
				anyhow::anyhow!("'{name}' is not in the project lock")
			})?;
			let skill_path = entry.skill_path.clone().ok_or_else(|| {
				anyhow::anyhow!("locked entry has no skillPath")
			})?;
			// Project entries store only `source`; reuse it as the URL.
			Ok(RenameLockSource {
				source: entry.source.clone(),
				source_type: entry.source_type.clone(),
				source_url: entry.source.clone(),
				ref_name: entry.ref_name.clone(),
				skill_path,
			})
		}
		_ => bail!("only -g (global) or -p (project) is supported"),
	}
}

/// Remove `name` from the scope's lock. The closure is NON-fallible (returns
/// `()`); a no-op modify (entry already absent) does not rewrite the file.
fn remove_lock_entry(
	name: &str,
	scope: ResourceScope,
	project_root: Option<&Path>,
) -> Result<()> {
	match scope {
		ResourceScope::GlobalOnly => {
			skill::lock::global::modify_skill_lock(|lock| {
				lock.skills.remove(name);
			})
			.map_err(|e| anyhow::anyhow!("global lock write failed: {e}"))
		}
		ResourceScope::ProjectOnly => {
			let root = project_root
				.ok_or_else(|| anyhow::anyhow!("project_root is required"))?;
			skill::lock::local::modify_local_lock(Some(root), |lock| {
				lock.skills.remove(name);
			})
			.map_err(|e| anyhow::anyhow!("project lock write failed: {e}"))
		}
		_ => bail!("only -g (global) or -p (project) is supported"),
	}
}

/// Re-insert a previously-removed lock entry under `name` (rollback).
fn restore_lock_entry(
	name: &str,
	scope: ResourceScope,
	project_root: Option<&Path>,
	global_entry: Option<&skill::SkillLockEntry>,
	local_entry: Option<&skill::LocalSkillLockEntry>,
) {
	match scope {
		ResourceScope::GlobalOnly => {
			if let Some(entry) = global_entry {
				let entry = entry.clone();
				let name = name.to_string();
				let _ = skill::lock::global::modify_skill_lock(move |lock| {
					lock.skills.insert(name, entry);
				});
			}
		}
		ResourceScope::ProjectOnly => {
			if let (Some(root), Some(entry)) = (project_root, local_entry) {
				let entry = entry.clone();
				let name = name.to_string();
				let _ = skill::lock::local::modify_local_lock(
					Some(root),
					move |lock| {
						lock.skills.insert(name, entry);
					},
				);
			}
		}
		_ => {}
	}
}

/// A filesystem snapshot of one skill name across the in-scope agent dirs + the
/// universal master, deep-copied into a temp backup so a failed rename txn can
/// be rolled back. Mirrors the API's `SkillSnapshot`.
struct SkillSnapshot {
	/// `_tmp` owns the backup tree; dropping it deletes the backup.
	_tmp: tempfile::TempDir,
	/// `(live_path, backup_path)` pairs for every captured location.
	entries: Vec<(PathBuf, PathBuf)>,
}

/// Cross-platform symlink: create a link at `link` pointing at `target`
/// (possibly relative). On Unix one syscall handles both file and dir targets;
/// on Windows the kind must be chosen, so resolve `target` relative to `link`'s
/// parent and pick `symlink_dir`/`symlink_file` by the resolved metadata
/// (defaulting to a file link when the target cannot be stat'd). For a directory
/// target on Windows we mirror the project linker's create-fallback: native
/// `symlink_dir` first (needs Dev Mode/admin), else a directory junction via
/// `mklink /J` using the ABSOLUTE resolved target — so a junction Referrer
/// round-trips through snapshot/restore even without admin. Mirrors the API
/// helper; keeps snapshot/restore compiling on the Windows release build.
fn xplat_symlink(target: &Path, link: &Path) -> std::io::Result<()> {
	#[cfg(unix)]
	{
		std::os::unix::fs::symlink(target, link)
	}
	#[cfg(windows)]
	{
		let resolved = if target.is_absolute() {
			target.to_path_buf()
		} else {
			link.parent().unwrap_or_else(|| Path::new(".")).join(target)
		};
		if std::fs::metadata(&resolved)
			.map(|m| m.is_dir())
			.unwrap_or(false)
		{
			if std::os::windows::fs::symlink_dir(target, link).is_ok() {
				return Ok(());
			}
			// Fallback: directory junction (no admin). A junction cannot store a
			// relative target, so use the absolute resolved path.
			create_junction(&resolved, link)
		} else {
			std::os::windows::fs::symlink_file(target, link)
		}
	}
}

/// Create a directory junction at `link` pointing at the ABSOLUTE `abs_target`
/// via `cmd /C mklink /J`. Mirrors the project linker's `create_junction`
/// (which is crate-private to `aghub-core`): the junction fallback the linker
/// uses when native `symlink_dir` is unavailable. Create-only.
#[cfg(windows)]
fn create_junction(abs_target: &Path, link: &Path) -> std::io::Result<()> {
	use std::os::windows::process::CommandExt;
	use std::process::Command;

	let out = Command::new("cmd")
		.args(["/C", "mklink", "/J"])
		.arg(link)
		.arg(abs_target)
		.creation_flags(0x08000000) // CREATE_NO_WINDOW
		.output()?;
	if out.status.success() {
		Ok(())
	} else {
		Err(std::io::Error::other(format!(
			"mklink /J {} {} failed: {} {}",
			link.display(),
			abs_target.display(),
			String::from_utf8_lossy(&out.stderr).trim(),
			String::from_utf8_lossy(&out.stdout).trim()
		)))
	}
}

/// Recursively copy `src` (a real directory) into `dst`. A reparse point
/// (Unix symlink OR Windows symlink/junction — detected via the project linker's
/// [`Linker::is_link`], not bare `is_symlink()`) is re-created as a link, never
/// deep-copied as a real directory.
fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
	use aghub_core::skills::linker::Linker;
	std::fs::create_dir_all(dst)?;
	for entry in std::fs::read_dir(src)? {
		let entry = entry?;
		let file_type = entry.file_type()?;
		let from = entry.path();
		let to = dst.join(entry.file_name());
		if Linker::is_link(&from) {
			let target = std::fs::read_link(&from)?;
			xplat_symlink(&target, &to)?;
		} else if file_type.is_dir() {
			copy_dir_recursive(&from, &to)?;
		} else {
			std::fs::copy(&from, &to)?;
		}
	}
	Ok(())
}

/// Capture the old-name skill across the in-scope agent dirs + the universal
/// master into a temp backup. Symlinks are preserved; real dirs are deep-copied.
///
/// MUST run BEFORE any mutation. A failure to create the backup tempdir, or to
/// copy/readlink an EXISTING old target, aborts the operation (returns `Err`) so
/// a backup failure can never become permanent old-skill loss when a later step
/// fails. Genuinely-absent paths are skipped (nothing to back up).
fn snapshot_old_skill(
	name: &str,
	scope: ResourceScope,
	project_root: Option<&Path>,
	agent_dirs: &[PathBuf],
) -> Result<SkillSnapshot> {
	let safe = skill::sanitize_name(name);
	let tmp =
		tempfile::tempdir().context("failed to create snapshot backup dir")?;
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
		let Ok(meta) = std::fs::symlink_metadata(&live) else {
			continue;
		};
		let backup = tmp.path().join(format!("snap-{idx}"));
		// A reparse point (Unix symlink OR Windows symlink/junction) is captured
		// by recording its target and re-creating it as a link — NEVER
		// deep-copied as a real directory. `Linker::is_link` covers junctions
		// (FILE_ATTRIBUTE_REPARSE_POINT), which bare `is_symlink()` may miss.
		let result = if aghub_core::skills::linker::Linker::is_link(&live) {
			std::fs::read_link(&live)
				.and_then(|target| xplat_symlink(&target, &backup))
		} else if meta.is_dir() {
			copy_dir_recursive(&live, &backup)
		} else {
			std::fs::copy(&live, &backup).map(|_| ())
		};
		result.context("failed to snapshot old skill before rename")?;
		entries.push((live, backup));
	}

	Ok(SkillSnapshot { _tmp: tmp, entries })
}

/// Whether `new_name` already has a lock entry OR an on-disk skill dir in the
/// target scope. Used to refuse clobbering pre-existing data (P0-2), which would
/// make the "remove all new_name paths" rollback delete data this transaction
/// did not create. Mirrors the API's `new_name_exists_in_scope`.
fn new_name_exists_in_scope(
	new_name: &str,
	scope: ResourceScope,
	project_root: Option<&Path>,
	agent_dirs: &[PathBuf],
) -> bool {
	let in_lock = match scope {
		ResourceScope::GlobalOnly => {
			skill::lock::global::get_skill_from_lock(new_name).is_some()
		}
		ResourceScope::ProjectOnly => project_root.is_some_and(|root| {
			skill::lock::local::read_local_lock(Some(root))
				.skills
				.contains_key(new_name)
		}),
		_ => false,
	};
	if in_lock {
		return true;
	}
	let safe = skill::sanitize_name(new_name);
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
				.and_then(|target| xplat_symlink(&target, live))
		} else if meta.is_dir() {
			copy_dir_recursive(backup, live)
		} else {
			std::fs::copy(backup, live).map(|_| ())
		};
	}
}

/// Best-effort rollback of the just-installed new-name dirs (and the universal
/// master if freshly created), re-asserting containment before each delete.
fn rollback_rename_install(
	new_name: &str,
	scope: ResourceScope,
	project_root: Option<&Path>,
	agent_dirs: &[PathBuf],
) {
	let safe = skill::sanitize_name(new_name);
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

/// `source accept-rename <old> <new>` — atomic rename: install the new name,
/// remove the old name, transition both lock entries. A single transaction:
/// any failure after the new-name install rolls the install back and restores
/// the old name (dirs + lock) to its pre-transaction state.
fn accept_rename(args: AcceptRenameArgs) -> Result<()> {
	use aghub_core::skills::install_fetched::{
		install_fetched_skill_and_lock, FetchedSkillInstallRequest,
	};
	use aghub_core::skills::linker::LinkTarget;

	if args.all {
		bail!("`source accept-rename` needs exactly one scope; --all is not allowed");
	}
	if args.global && args.project {
		bail!("choose either -g or -p, not both");
	}
	let scope = if args.project {
		ResourceScope::ProjectOnly
	} else {
		// Default (and explicit -g) is global.
		ResourceScope::GlobalOnly
	};
	let project_root = if matches!(scope, ResourceScope::ProjectOnly) {
		Some(current_project_root()?.ok_or_else(|| {
			anyhow::anyhow!(
				"no project root found (need an agent marker like .claude/, \
				 .opencode/, .mcp.json, …)"
			)
		})?)
	} else {
		None
	};
	let scope_label = if matches!(scope, ResourceScope::ProjectOnly) {
		"project"
	} else {
		"global"
	};

	if !args.yes {
		println!(
			"Dry-run: would install '{}' and remove '{}' ({scope_label}). \
			 Pass --yes to execute.",
			args.new_name, args.old_name
		);
		return Ok(());
	}

	// P0-2 guard (a): a degenerate rename whose names sanitize to the same
	// on-disk dir would have the install write the very dir the removal then
	// deletes. Refuse before any fetch/mutation.
	if skill::sanitize_name(args.old_name)
		== skill::sanitize_name(args.new_name)
	{
		bail!(
			"old_name and new_name resolve to the same on-disk skill \
			 directory; choose a distinct rename target"
		);
	}

	// 1. Read the OLD-name lock entry for source coordinates.
	let source =
		rename_source_from_lock(args.old_name, scope, project_root.as_deref())?;

	// 2. Target agents = those that ACTUALLY have the old name installed (never
	//    every agent). Mirrors apply-update only touching installed roots.
	let target_agents: Vec<AgentType> =
		aghub_core::load_all_agents(scope, project_root.as_deref())
			.into_iter()
			.filter(|r| r.skills.iter().any(|s| s.name == args.old_name))
			.filter_map(|r| r.agent_id.parse().ok())
			.collect();
	if target_agents.is_empty() {
		bail!(
			"'{}' is locked but no installed copy was found",
			args.old_name
		);
	}

	// 3. Fetch upstream (test hook honored by `CliFetcher`).
	let effective_ref =
		args.git_ref.map(str::to_string).or(source.ref_name.clone());
	let token = <EnvTokenResolver as skill_update::TokenResolver>::resolve(
		&EnvTokenResolver,
		&source.source_url,
		None,
	);
	let repo = <CliFetcher as skill_update::Fetcher>::fetch(
		&CliFetcher,
		&SourceRef {
			source: source.source_url.clone(),
			ref_: effective_ref.clone(),
		},
		token.as_deref(),
	)
	.map_err(|e| match e {
		FetchError::Auth => anyhow::anyhow!(
			"This source needs a credential. Set GIT_PASSWORD / \
				 GITHUB_TOKEN, or bind a credential in the desktop app."
		),
		FetchError::Network => anyhow::anyhow!(
			"Failed to fetch source repository '{}'",
			source.source_url
		),
	})?;

	// 4. Locate the skill file in the fetched tree (containment check).
	let skill_file = aghub_core::skills::update::sanitize_skill_path(
		&repo.root,
		&source.skill_path,
	)
	.ok_or_else(|| {
		anyhow::anyhow!("locked skillPath was not found in fetched source")
	})?;

	// 5. Verify the fetched name matches new_name (confirms this rename).
	let parsed_skill = skill::parse(&skill_file)
		.context("failed to parse fetched SKILL.md")?;
	if parsed_skill.name != args.new_name {
		bail!(
			"Fetched SKILL.md declares name '{}', expected '{}'. \
			 Verify the new_name matches the upstream source.",
			parsed_skill.name,
			args.new_name,
		);
	}

	let agent_dirs = aghub_core::skills::removal::agent_skill_dirs_in_scope(
		scope,
		project_root.as_deref(),
	);

	// P0-2 guard (b): refuse if the new name ALREADY exists (lock entry or
	// on-disk dir) in this scope. The rollback/cleanup deletes EVERY new_name
	// path; if new_name pre-existed, that would destroy data this transaction
	// did not create. Requiring new_name to be absent makes the cleanup safe.
	if new_name_exists_in_scope(
		args.new_name,
		scope,
		project_root.as_deref(),
		&agent_dirs,
	) {
		bail!(
			"A skill named '{}' already exists in this scope (lock entry or \
			 on-disk directory); pick a rename target that does not already \
			 exist",
			args.new_name
		);
	}

	// 6. SNAPSHOT the old-name dirs + clone the old lock entry BEFORE mutating.
	//    A snapshot failure (P0-3) aborts BEFORE install — nothing mutated.
	let snapshot = snapshot_old_skill(
		args.old_name,
		scope,
		project_root.as_deref(),
		&agent_dirs,
	)?;
	let old_global_entry: Option<skill::SkillLockEntry> =
		if matches!(scope, ResourceScope::GlobalOnly) {
			skill::read_skill_lock().skills.get(args.old_name).cloned()
		} else {
			None
		};
	let old_local_entry: Option<skill::LocalSkillLockEntry> =
		if matches!(scope, ResourceScope::ProjectOnly) {
			project_root.as_deref().and_then(|root| {
				skill::read_local_lock(Some(root))
					.skills
					.get(args.old_name)
					.cloned()
			})
		} else {
			None
		};

	// Helper: roll the WHOLE transaction back to its pre-mutation state. Defined
	// BEFORE install so every post-snapshot failure path (P0-1: including the
	// install Err / no-agent arms) runs the SAME rollback.
	let rollback_all = || {
		rollback_rename_install(
			args.new_name,
			scope,
			project_root.as_deref(),
			&agent_dirs,
		);
		let _ =
			remove_lock_entry(args.new_name, scope, project_root.as_deref());
		restore_snapshot(&snapshot);
		restore_lock_entry(
			args.old_name,
			scope,
			project_root.as_deref(),
			old_global_entry.as_ref(),
			old_local_entry.as_ref(),
		);
	};

	// 7. Install the new-named skill. A failure AFTER this point rolls back via
	//    `rollback_all` (install writes the master/link before the lock, so an
	//    Err may have left a half-installed new_name — P0-1).
	let install_source = skill::InstallLockSource {
		source: source.source.clone(),
		source_type: source.source_type.clone(),
		source_url: source.source_url.clone(),
		ref_name: effective_ref.clone(),
	};
	let install_req = FetchedSkillInstallRequest {
		skill_file: &skill_file,
		source: &install_source,
		lock_skill_path: source.skill_path.clone(),
		ref_commit: Some(repo.oid.clone()),
		scope,
		project_root: project_root.as_deref(),
		target_agents: &target_agents,
		expected_name: Some(args.new_name),
		target: if matches!(scope, ResourceScope::ProjectOnly) {
			LinkTarget::Relative
		} else {
			LinkTarget::Absolute
		},
	};
	let install_report = match install_fetched_skill_and_lock(install_req) {
		Ok(r) => r,
		Err(e) => {
			// P0-1: install_fetched writes the master/link BEFORE the lock, so
			// an Err here may have left a half-installed new_name. Run the full
			// rollback (cleanup new_name + restore old) before bailing.
			rollback_all();
			bail!("failed to install renamed skill: {e}");
		}
	};
	if !install_report.agent_results.iter().any(|r| r.installed) {
		let detail = install_report
			.agent_results
			.iter()
			.find_map(|r| r.error.clone())
			.unwrap_or_else(|| "no agent received the skill".to_string());
		rollback_all();
		bail!("failed to install renamed skill: {detail}");
	}

	let installed_paths: Vec<String> = install_report
		.agent_results
		.iter()
		.filter(|r| r.installed)
		.filter_map(|r| {
			aghub_core::create_adapter(r.agent)
				.get_skills_paths(project_root.as_deref(), scope)
				.first()
				.map(|p| p.join(args.new_name).display().to_string())
		})
		.collect();

	// 8. Remove the old-name dirs. A removal failure rolls back the whole txn.
	let mut old_skill = aghub_core::models::Skill::new(args.old_name);
	if let Some(dir) = agent_dirs.first() {
		old_skill.source_path = Some(
			dir.join(args.old_name)
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
			bail!("failed to remove old skill '{}': {e}", args.old_name);
		}
	};
	if !removal_report.failed.is_empty() {
		let failed_msgs: Vec<String> = removal_report
			.failed
			.iter()
			.map(|(p, e)| format!("{}: {e}", p.display()))
			.collect();
		rollback_all();
		bail!(
			"partial removal failure for old skill: {}",
			failed_msgs.join("; ")
		);
	}

	// 9. Remove the old-name lock entry. NOT log-and-continue: a failure here
	//    means the transaction did not fully commit -> roll everything back.
	if let Err(e) =
		remove_lock_entry(args.old_name, scope, project_root.as_deref())
	{
		rollback_all();
		bail!("failed to remove old lock entry '{}': {e}", args.old_name);
	}

	if args.json {
		println!(
			"{}",
			serde_json::to_string_pretty(&serde_json::json!({
				"success": true,
				"oldName": args.old_name,
				"newName": args.new_name,
				"scope": scope_label,
				"installedHash": install_report.installed_hash,
				"paths": installed_paths,
			}))?
		);
	} else {
		println!(
			"Renamed '{}' → '{}': installed to {} path(s), removed old skill.",
			args.old_name,
			args.new_name,
			installed_paths.len()
		);
	}
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::select_env_token;

	fn s(v: &str) -> Option<String> {
		Some(v.to_string())
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
	fn github_token_is_bound_to_github_hosts() {
		assert_eq!(
			select_env_token(None, s("gh"), Some("github.com")),
			s("gh")
		);
		assert_eq!(
			select_env_token(None, s("gh"), Some("API.GitHub.com")),
			s("gh")
		);
		for host in [Some("gitlab.com"), Some("evil-github.com"), None] {
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
}
