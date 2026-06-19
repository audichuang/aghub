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
use anyhow::{bail, Result};
use serde::Serialize;
use skill_update::sources::{
	self, SourceScope, SourceScopeKind, SourceSkillDiff, SourceSummary,
};
use skill_update::{FetchError, SourceRef};
use tabled::builder::Builder;
use tabled::settings::Style;

use crate::SourceAction;

// ─────────────────────────── credential / fetch ────────────────────────────

/// Token resolver for CLI source auth: a token in `GIT_PASSWORD`
/// (or `GITHUB_TOKEN`). `GitFetcher` consumes it as the `x-access-token`
/// password — there is no username/password basic-auth path. Returns `None`
/// when neither is set (the first unauthenticated fetch attempt stands).
struct EnvTokenResolver;
impl skill_update::TokenResolver for EnvTokenResolver {
	fn resolve(&self, _source: &str, _host: Option<&str>) -> Option<String> {
		std::env::var("GIT_PASSWORD")
			.or_else(|_| std::env::var("GITHUB_TOKEN"))
			.ok()
	}
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
/// project (when a project root is detected).
fn resolve_read_scopes(
	global: bool,
	project: bool,
) -> Result<Vec<SourceScope>> {
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

	// Skip sources we cannot fetch (local/ssh/unsupported scheme) up front,
	// before paying for a fetch. The CLI treats every non-`local` source as
	// `github`-ish; `precheck_source` only rejects on the source string here.
	if let Some(reason) =
		aghub_core::skills::update::precheck_source("github", &source)
	{
		bail!(
			"source '{source}' cannot be fetched ({reason:?}); only HTTPS / \
			 owner/repo git sources are supported"
		);
	}

	let repo = match sources::fetch_source_with_resolver(
		&SourceRef {
			source: source.clone(),
			ref_: git_ref.map(str::to_string),
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
	let source = args.source.trim().to_string();

	if let Some(reason) =
		aghub_core::skills::update::precheck_source("github", &source)
	{
		bail!(
			"source '{source}' cannot be fetched ({reason:?}); only HTTPS / \
			 owner/repo git sources are supported"
		);
	}

	let (scope, project_root, source_scope, scope_label) =
		resolve_write_scope(&args)?;

	// Fetch ONCE; reuse the repo for classification AND every install/update.
	// Fetch + classify happen BEFORE the flag branch so the neither-flag
	// informational path can print the same plan without a second fetch.
	let repo = match sources::fetch_source_with_resolver(
		&SourceRef {
			source: source.clone(),
			ref_: args.git_ref.map(str::to_string),
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

	let diffs =
		sources::classify_scope(repo.root.as_path(), &source_scope, &source);

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
	let lock_source = skill::InstallLockSource {
		source: resolved.lock_source(),
		source_type: resolved.source_type.as_str().to_string(),
		source_url: resolved.source_url.clone(),
		ref_name: args.git_ref.map(str::to_string),
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
	universal: bool,
	lock_source: &skill::InstallLockSource,
) -> SyncActionView {
	use aghub_core::skills::install_fetched::{
		install_fetched_skill_and_lock, FetchedSkillInstallRequest,
		SkillInstallLayout,
	};

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

	let layout = if universal {
		SkillInstallLayout::Universal
	} else {
		SkillInstallLayout::IsolatedCopy
	};

	let req = FetchedSkillInstallRequest {
		skill_file: &skill_file,
		source: lock_source,
		lock_skill_path: d.skill_path.clone(),
		ref_commit: Some(repo.oid.clone()),
		scope,
		project_root,
		target_agents,
		layout,
		expected_name: Some(&d.name),
		use_relative_links: matches!(scope, ResourceScope::ProjectOnly),
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
