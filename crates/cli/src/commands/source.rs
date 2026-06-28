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
use anyhow::{anyhow, bail, Result};
use serde::Serialize;
use skill_update::sources::{
	self, ScopeError, ScopeSelector, SourceScope, SourceScopeKind,
	SourceSkillDiff, SourceSummary,
};
use skill_update::{BindError, FetchError, SourceCredentialStore, SourceRef};
use tabled::builder::Builder;
use tabled::settings::Style;

use crate::{CredentialAction, SourceAction};

// ─────────────────────────── credential / fetch ────────────────────────────

/// Token resolver for CLI source auth: the shared env-then-keyring resolver.
/// It tries `GIT_PASSWORD` / `GITHUB_TOKEN` from the environment first, then
/// falls back to a bound/host-matched credential in the OS keychain — the same
/// store the desktop app uses. `GitFetcher` consumes the token as the
/// `x-access-token` password; an unresolved token leaves the first fetch
/// unauthenticated.
fn cli_resolver() -> skill_update::EnvThenKeyringResolver {
	skill_update::EnvThenKeyringResolver::default()
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
/// project (when a project root is detected). Detects the project root once,
/// then delegates the policy to the shared `sources::read_scopes` mapper so the
/// CLI and API agree on what each scope selector means.
fn resolve_read_scopes(
	global: bool,
	project: bool,
) -> Result<Vec<SourceScope>> {
	let sel = if global {
		ScopeSelector::Global
	} else if project {
		ScopeSelector::Project
	} else {
		ScopeSelector::All
	};
	// Only Project/All read the project; resolve the root (a `current_dir`
	// syscall) ONLY then. Global-only must not touch the cwd — so `-g` works from
	// a broken/deleted cwd and never fails on project-root resolution.
	let project_root = match sel {
		ScopeSelector::Global => None,
		ScopeSelector::Project | ScopeSelector::All => current_project_root()?,
	};
	sources::read_scopes(sel, project_root).map_err(|e| anyhow!(e))
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
		// Credentials live in the OS keychain, not in any scope's config, so
		// the scope/agent flags don't apply here.
		SourceAction::Credential { action } => credential(action),
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

	// ponytail: `diff` is multi-scope (one fetch, classify against every scope),
	// so the single-scope `scan_for_sync` seam does not fit; the resolve/precheck/
	// fetch prologue stays inlined here. `scan_for_sync` targets the single-write-
	// scope `sync` duplication, the real win — extracting a multi-scope variant for
	// one caller would be more surface than it saves.
	//
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
		&cli_resolver(),
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
			let diffs =
				sources::classify_scope(repo.root.as_path(), scope, &source);
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
/// be chosen; `--all` and an unscoped invocation are rejected. The CLI keeps
/// the "both -g and -p" guard (the shared mapper has no two-flag case) and then
/// delegates to `sources::write_scope`, mapping its `SourceScopeKind` back to
/// the CLI's `ResourceScope` + label at this boundary.
fn resolve_write_scope(
	args: &SyncArgs,
) -> Result<(ResourceScope, Option<PathBuf>, SourceScope, &'static str)> {
	if args.global && args.project {
		bail!("choose either -g or -p, not both");
	}
	let sel = if args.all {
		ScopeSelector::All
	} else if args.global {
		ScopeSelector::Global
	} else if args.project {
		ScopeSelector::Project
	} else {
		return Err(anyhow!(ScopeError::ScopeRequired));
	};
	// Only Project resolves the project root (a `current_dir` syscall). Global
	// writes the global scope and `All` is rejected outright — neither needs the
	// cwd, so the `--all` rejection and Global sync work from a broken/deleted
	// cwd instead of dying on project-root resolution first.
	let project_root = match sel {
		ScopeSelector::Project => current_project_root()?,
		ScopeSelector::Global | ScopeSelector::All => None,
	};
	let (source_scope, kind) =
		sources::write_scope(sel, project_root).map_err(|e| anyhow!(e))?;
	let scope = match kind {
		SourceScopeKind::Global => ResourceScope::GlobalOnly,
		SourceScopeKind::Project => ResourceScope::ProjectOnly,
	};
	let project_root = match &source_scope {
		SourceScope::Project { root } => Some(root.clone()),
		SourceScope::Global => None,
	};
	Ok((scope, project_root, source_scope, scope_kind_str(kind)))
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

	// Resolve the fetch coordinate, precheck, fetch ONCE, and classify against
	// the single write scope — all behind the shared `scan_for_sync` seam (the
	// SAME resolve/precheck/fetch the API runs). The repo is reused for
	// classification AND every install/update; the scan runs BEFORE the flag
	// branch so the neither-flag informational path prints the plan without a
	// second fetch.
	let cli_resolver = cli_resolver();
	let scan = match sources::scan_for_sync(
		&source,
		args.git_ref,
		&source_scope,
		sources::SourceSyncDeps {
			fetcher: &CliFetcher,
			resolver: &cli_resolver,
		},
	) {
		Ok(scan) => scan,
		Err(sources::SyncScanError::Uncheckable(reason)) => bail!(
			"source '{source}' cannot be fetched ({reason:?}); only HTTPS / \
			 owner/repo git sources are supported"
		),
		Err(sources::SyncScanError::NeedsCredential) => bail!(
			"This source needs a credential. Set GIT_PASSWORD / GITHUB_TOKEN, \
			 or bind a credential in the desktop app."
		),
		Err(sources::SyncScanError::FetchFailed) => {
			bail!("Failed to fetch source repository '{source}'")
		}
	};
	let repo = scan.repo;
	let diffs = scan.diffs;
	let scan_ref = scan.git_ref;

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
		ref_name: scan_ref.clone(),
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
		// `source sync` gates its own dry-run before reaching here; this row is
		// only built on the apply path, so always perform the swap.
		false,
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

// ────────────────────────── source credential ──────────────────────────────

/// Env var carrying the token for `source credential add` when `--token` is
/// absent and stdin is a terminal — keeps the secret off argv (where `ps` /
/// shell history would expose it).
const SOURCE_TOKEN_ENV: &str = "AGHUB_SOURCE_TOKEN";

#[derive(Serialize)]
struct CredentialView {
	id: String,
	name: String,
}

#[derive(Serialize)]
struct BindingView {
	source: String,
	#[serde(rename = "credentialId")]
	credential_id: String,
}

/// Read the token for `add`: `--token`, else piped stdin, else
/// `$AGHUB_SOURCE_TOKEN`. The token is NEVER taken from a positional argv (it
/// would leak via `ps`/history). Errors clearly when none is available.
fn read_add_token(flag: Option<&str>) -> Result<String> {
	use std::io::{IsTerminal, Read};

	if let Some(t) = flag {
		return Ok(t.to_string());
	}
	// Piped stdin (not a TTY): read the token from it.
	if !std::io::stdin().is_terminal() {
		let mut buf = String::new();
		std::io::stdin().read_to_string(&mut buf)?;
		let token = buf.trim();
		if !token.is_empty() {
			return Ok(token.to_string());
		}
	}
	if let Ok(t) = std::env::var(SOURCE_TOKEN_ENV) {
		if !t.is_empty() {
			return Ok(t);
		}
	}
	bail!(
		"no token provided: pass --token, pipe it on stdin, or set \
		 ${SOURCE_TOKEN_ENV} (the token is never read from a positional \
		 argument so it can't leak via the process list)"
	)
}

/// Dispatch a `source credential` action over the shared keychain-backed
/// [`SourceCredentialStore`] — the SAME store the desktop app uses. Tokens are
/// write-only and never printed back. A keychain failure surfaces via `?`
/// (anyhow) as a clear error, never a silent swallow.
fn credential(action: &CredentialAction) -> Result<()> {
	let store = SourceCredentialStore;
	match action {
		CredentialAction::List { json } => {
			let creds = store.list()?;
			let views: Vec<CredentialView> = creds
				.into_iter()
				.map(|c| CredentialView {
					id: c.id,
					name: c.name,
				})
				.collect();
			if *json {
				println!("{}", serde_json::to_string_pretty(&views)?);
				return Ok(());
			}
			if views.is_empty() {
				println!("No stored credentials.");
				return Ok(());
			}
			let mut builder = Builder::default();
			builder.push_record(["ID", "NAME"]);
			for c in &views {
				builder.push_record([c.id.clone(), c.name.clone()]);
			}
			let mut table = builder.build();
			table.with(Style::sharp());
			println!("{table}");
			Ok(())
		}
		CredentialAction::Add { name, token } => {
			let token = read_add_token(token.as_deref())?;
			// Enforce the same unique-name policy as the API (finding #2): the
			// store's `create_unique` does the dup check + insert atomically.
			let created =
				store.create_unique(name, &token).map_err(|e| match e {
					skill_update::CreateError::Duplicate(name) => {
						anyhow::anyhow!(
							"a credential named '{name}' already exists"
						)
					}
					skill_update::CreateError::Store(inner) => {
						anyhow::anyhow!(inner)
					}
				})?;
			// Print the id only; the token is write-only and never echoed.
			println!("{}", created.id);
			Ok(())
		}
		CredentialAction::Remove { id } => {
			if store.delete(id)? {
				println!("removed {id}");
			} else {
				bail!("no credential with id '{id}'");
			}
			Ok(())
		}
		CredentialAction::Bind {
			source,
			credential_id,
		} => {
			store.bind(source, credential_id.as_deref()).map_err(
				|e| match e {
					BindError::EmptySource => {
						anyhow::anyhow!("source must not be empty")
					}
					BindError::CredentialNotFound(id) => {
						anyhow::anyhow!("credential not found: '{id}'")
					}
					BindError::Store(inner) => anyhow::anyhow!(inner),
				},
			)?;
			match credential_id {
				Some(id) => println!("bound '{source}' -> {id}"),
				None => println!("cleared binding for '{source}'"),
			}
			Ok(())
		}
		CredentialAction::ListBindings { json } => {
			let bindings = store.list_bindings()?;
			let views: Vec<BindingView> = bindings
				.0
				.into_iter()
				.map(|(source, credential_id)| BindingView {
					source,
					credential_id,
				})
				.collect();
			if *json {
				println!("{}", serde_json::to_string_pretty(&views)?);
				return Ok(());
			}
			if views.is_empty() {
				println!("No source bindings.");
				return Ok(());
			}
			let mut builder = Builder::default();
			builder.push_record(["SOURCE", "CREDENTIAL_ID"]);
			for b in &views {
				builder
					.push_record([b.source.clone(), b.credential_id.clone()]);
			}
			let mut table = builder.build();
			table.with(Style::sharp());
			println!("{table}");
			Ok(())
		}
	}
}
