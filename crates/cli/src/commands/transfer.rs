//! `aghub-cli transfer` + `reconcile` — thin adapters over
//! `aghub_core::transfer`.
//!
//! Both subcommands span MULTIPLE agents, so they are dispatched in `main.rs`
//! before the single-agent adapter/ConfigManager setup. They build the same
//! `ResourceLocator` / `InstallTarget` inputs the HTTP API builds and call the
//! same `transfer_*` / `reconcile_*` core fns, then render the returned
//! `OperationBatchResult`. No copying logic lives here — the CLI is a pure
//! passthrough, so the npx symlink-Master layout the core fns preserve is
//! unaffected.
//
// ponytail: render the OperationBatchResult directly via tabled + serde_json;
// do NOT drag the api transfer DTO crate into the CLI.

use aghub_core::models::AgentType;
use aghub_core::paths::find_project_root;
use aghub_core::transfer::{
	reconcile_mcp, reconcile_skill, reconcile_sub_agent, transfer_mcp,
	transfer_skill, transfer_sub_agent, InstallScope, InstallTarget,
	OperationBatchResult, OperationBatchView, ResourceLocator,
};
use anyhow::{bail, Result};
use clap::Subcommand;
use tabled::builder::Builder;
use tabled::settings::Style;

/// The three resource kinds transfer/reconcile operate on. Each carries the same
/// flags; the kind only selects which core fn pair runs.
#[derive(Subcommand, Clone)]
pub enum TransferAction {
	/// Copy a skill from one agent into one or more target agents.
	Skill(TransferArgs),
	/// Copy an MCP server from one agent into one or more target agents.
	Mcp(TransferArgs),
	/// Copy a sub-agent from one agent into one or more target agents.
	SubAgent(TransferArgs),
}

/// Shared flags for every `transfer` kind.
#[derive(clap::Args, Clone)]
pub struct TransferArgs {
	/// Source agent the resource is read from.
	#[arg(long = "from-agent", value_parser = parse_agent)]
	from_agent: AgentType,
	/// Name of the resource to copy.
	#[arg(long)]
	name: String,
	/// Target agent (repeatable) to copy the resource into. At least one is
	/// required — a transfer to no destinations is meaningless (the core guard
	/// rejects it too; this just fails fast at parse with a usage error).
	#[arg(long = "to", value_parser = parse_agent, required = true, num_args = 1..)]
	to: Vec<AgentType>,
	#[arg(long)]
	json: bool,
}

#[derive(Subcommand, Clone)]
pub enum ReconcileAction {
	/// Add/remove a skill across agents to match the given set.
	Skill(ReconcileArgs),
	/// Add/remove an MCP server across agents to match the given set.
	Mcp(ReconcileArgs),
	/// Add/remove a sub-agent across agents to match the given set.
	SubAgent(ReconcileArgs),
}

/// Shared flags for every `reconcile` kind.
#[derive(clap::Args, Clone)]
pub struct ReconcileArgs {
	/// Source agent the resource is read from.
	#[arg(long = "from-agent", value_parser = parse_agent)]
	from_agent: AgentType,
	/// Name of the resource to reconcile.
	#[arg(long)]
	name: String,
	/// Agent (repeatable) to copy the resource into.
	#[arg(long = "add", value_parser = parse_agent)]
	add: Vec<AgentType>,
	/// Agent (repeatable) to remove the resource from.
	#[arg(long = "remove", value_parser = parse_agent)]
	remove: Vec<AgentType>,
	/// Only list what would change (this is the default when --remove is
	/// given).
	#[arg(long = "dry-run")]
	dry_run: bool,
	/// Actually perform removals (without it, a reconcile that removes is a
	/// dry-run).
	#[arg(short = 'y', long = "yes")]
	yes: bool,
	#[arg(long)]
	json: bool,
}

/// A core `transfer_*` fn (copy a resource into a set of destinations).
type TransferFn = fn(
	ResourceLocator,
	Vec<InstallTarget>,
) -> aghub_core::Result<OperationBatchResult>;

/// A core `reconcile_*` fn (add/remove a resource across agents).
type ReconcileFn = fn(
	ResourceLocator,
	Vec<AgentType>,
	Vec<AgentType>,
) -> aghub_core::Result<OperationBatchResult>;

/// clap value parser for an agent id, reusing the canonical FromStr so the CLI
/// accepts the same spellings/aliases as the API and store.
fn parse_agent(value: &str) -> Result<AgentType, String> {
	value.parse()
}

/// Resolve the top-level `-g`/`-p` flags into the transfer-local
/// [`InstallScope`] plus a project root.
///
/// `-p`/Project resolves the project root from the current dir; the actual
/// "project root is required" check is left to the core `validate_target` so its
/// message is the single source of truth. Default (neither flag) is Global, the
/// same default `main.rs` uses for single-agent ops.
fn resolve_scope(
	global: bool,
	project: bool,
) -> Result<(InstallScope, Option<std::path::PathBuf>)> {
	if global && project {
		bail!("pass at most one of -g/--global or -p/--project");
	}
	if project {
		let cwd = std::env::current_dir()?;
		return Ok((InstallScope::Project, find_project_root(&cwd)));
	}
	Ok((InstallScope::Global, None))
}

/// Dispatch a `transfer` subcommand action.
pub fn execute_transfer(
	action: &TransferAction,
	global: bool,
	project: bool,
) -> Result<()> {
	let (scope, project_root) = resolve_scope(global, project)?;
	let (args, run): (&TransferArgs, TransferFn) = match action {
		TransferAction::Skill(a) => (a, transfer_skill),
		TransferAction::Mcp(a) => (a, transfer_mcp),
		TransferAction::SubAgent(a) => (a, transfer_sub_agent),
	};

	let source = ResourceLocator {
		agent: args.from_agent,
		scope,
		project_root: project_root.clone(),
		name: args.name.clone(),
	};
	let destinations = args
		.to
		.iter()
		.map(|agent| InstallTarget {
			agent: *agent,
			scope,
			project_root: project_root.clone(),
		})
		.collect();

	let result = run(source, destinations)?;
	render(&result, args.json)
}

/// Dispatch a `reconcile` subcommand action.
pub fn execute_reconcile(
	action: &ReconcileAction,
	global: bool,
	project: bool,
) -> Result<()> {
	let (scope, project_root) = resolve_scope(global, project)?;
	let (args, run): (&ReconcileArgs, ReconcileFn) = match action {
		ReconcileAction::Skill(a) => (a, reconcile_skill),
		ReconcileAction::Mcp(a) => (a, reconcile_mcp),
		ReconcileAction::SubAgent(a) => (a, reconcile_sub_agent),
	};

	let source = ResourceLocator {
		agent: args.from_agent,
		scope,
		project_root,
		name: args.name.clone(),
	};

	// Delete-consistent gate: a reconcile that REMOVES is destructive, so it
	// defaults to a dry-run and only executes with an explicit --yes. Adds
	// alone are non-destructive and run immediately (like `transfer`), unless
	// --dry-run is asked for explicitly.
	if args.dry_run || (!args.remove.is_empty() && !args.yes) {
		return render_dry_run(args);
	}

	let result = run(source, args.add.clone(), args.remove.clone())?;
	render(&result, args.json)
}

/// Report what a reconcile WOULD do without touching anything, mirroring the
/// `delete` contract (dry-run by default, `--yes` to apply).
fn render_dry_run(args: &ReconcileArgs) -> Result<()> {
	let names = |agents: &[AgentType]| -> Vec<String> {
		agents.iter().map(|a| a.as_str().to_string()).collect()
	};
	if args.json {
		println!(
			"{}",
			serde_json::to_string_pretty(&serde_json::json!({
				"dry_run": true,
				"name": args.name,
				"add": names(&args.add),
				"remove": names(&args.remove),
			}))?
		);
	} else {
		if !args.add.is_empty() {
			println!(
				"dry-run: would add '{}' to: {}",
				args.name,
				names(&args.add).join(", ")
			);
		}
		if !args.remove.is_empty() {
			println!(
				"dry-run: would remove '{}' from: {}",
				args.name,
				names(&args.remove).join(", ")
			);
		}
		eprintln!("pass --yes to apply");
	}
	Ok(())
}

// Scope note: transfer carries its own `InstallScope` (Global/Project), NOT the
// `ResourceScope` (GlobalOnly/ProjectOnly/Both) the single-agent path uses —
// `resolve_scope` maps the top-level -g/-p flags into `InstallScope` directly so
// the two enums never get mixed.

/// Render an [`OperationBatchResult`] as a table (default) or JSON (`--json`),
/// then fail with a non-zero exit when any target failed.
///
/// `--json` serializes the SHARED `OperationBatchView` (core), the exact type
/// the API's `OperationBatchResponse` mirrors — so both surfaces emit one wire
/// shape (`{success_count, failed_count, results:[…]}`, scope lowercase,
/// `project_root`/`error` omitted when absent) defined in one place.
fn render(result: &OperationBatchResult, json: bool) -> Result<()> {
	if json {
		let view = OperationBatchView::from(result);
		println!("{}", serde_json::to_string_pretty(&view)?);
	} else {
		let mut builder = Builder::default();
		builder.push_record(["AGENT", "ACTION", "OK", "ERROR"]);
		for r in &result.results {
			builder.push_record([
				r.target.agent.as_str().to_string(),
				r.action.to_string(),
				if r.success { "yes" } else { "no" }.to_string(),
				r.error.clone().unwrap_or_default(),
			]);
		}
		let mut table = builder.build();
		table.with(Style::sharp());
		println!("{table}");
		eprintln!(
			"{} succeeded, {} failed",
			result.success_count(),
			result.failed_count()
		);
	}

	if result.failed_count() > 0 {
		bail!(
			"{} of {} operations failed",
			result.failed_count(),
			result.results.len()
		);
	}
	Ok(())
}
