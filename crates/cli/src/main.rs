use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use aghub_core::{
	adapters::create_adapter,
	load_all_agents,
	manager::ConfigManager,
	models::{AgentSelection, AgentType, ResourceScope},
	paths::find_project_root,
};

mod commands;

use commands::{
	add, check, delete, disable, enable, get, inference, plugin, prune,
	transfer, update,
};

/// Global verbose flag used by the eprintln_verbose macro
static VERBOSE: AtomicBool = AtomicBool::new(false);

/// Set the verbose flag
pub fn set_verbose(verbose: bool) {
	VERBOSE.store(verbose, Ordering::Relaxed);
}

/// Check if verbose mode is enabled
pub fn is_verbose() -> bool {
	VERBOSE.load(Ordering::Relaxed)
}

/// Print verbose message to stderr (prefixed with "# ")
#[macro_export]
macro_rules! eprintln_verbose {
    ($($arg:tt)*) => {
        if $crate::is_verbose() {
            eprintln!("# {}", format!($($arg)*));
        }
    };
}

/// CLI tool for managing Code Agent configurations (Claude Code, OpenCode)
#[derive(Parser)]
#[command(name = "aghub-cli")]
#[command(about = "Manage Code Agent configurations")]
#[command(version = env!("AGHUB_CLI_VERSION"))]
#[command(after_help = SCOPE_HELP)]
struct Cli {
	/// Target agent: one id, a comma-separated list, or "all".
	///
	/// A LIST fans the command out across those agents (get, add, update,
	/// delete, enable, disable, `source sync`, `doctor --verify-links`).
	/// "all" is accepted by `get`, `doctor --verify-links` and `source sync`
	/// (which fans install/relink out to every agent that can hold a skill in
	/// the scope); add/update/delete/enable/disable REJECT it — pass a
	/// comma-separated list there. Every other command is single-agent or
	/// agent-independent and ignores it.
	#[arg(short = 'a', long, default_value = "claude", global = true)]
	agent: String,

	/// Read AND write global config only (the default)
	#[arg(short, long, global = true)]
	global: bool,

	/// Read AND write the current project's config only
	#[arg(short, long, global = true)]
	project: bool,

	/// Read both project and global scopes (read-only commands only)
	#[arg(long, global = true)]
	all: bool,

	/// Emit machine-readable JSON instead of human-readable output
	#[arg(long, global = true)]
	json: bool,

	/// Enable verbose output (to stderr)
	#[arg(short, long, global = true)]
	verbose: bool,

	#[command(subcommand)]
	command: Commands,
}

/// Appended to `--help`. The scope flags are `global = true`, so clap prints
/// them on every subcommand but cannot explain the per-command policy there.
const SCOPE_HELP: &str = "\
Scope flags (-g / -p / --all) are mutually exclusive and may appear before or \
after the subcommand:
  -g/--global   global config only (default)
  -p/--project  the current project only; a mutation fails if no project root
  --all         both scopes. Accepted by the read-only commands and by
                prune-lock, which then writes BOTH locks. Rejected by
                add/update/delete/enable/disable, transfer, reconcile,
                coverage, apply-update and `source sync`.

`inference` and `plugin` manage a shared store, not per-scope agent config, so
they ignore the scope flags entirely.

Output:
  Human-readable by default; --json emits the machine-readable shape.
  The `plugin` mutations have no JSON form and reject --json rather than
  silently printing prose; `plugin list` and `plugin marketplace list` do.

Destructive commands (delete, apply-update, prune-lock, source sync, source
accept-rename, reconcile with --remove) preview by default and only write with
--yes.";

#[derive(Subcommand, Clone)]
enum Commands {
	/// List resources (skills, mcps)
	Get {
		#[arg(value_enum)]
		resource: ResourceType,
	},
	/// Add a resource
	Add {
		#[arg(value_enum)]
		resource: ResourceType,

		/// Resource name (required for manual creation, optional when using --from)
		#[arg(short, long)]
		name: Option<String>,

		/// For skill: Import from file/directory/.skill package path
		#[arg(long, value_name = "PATH")]
		from: Option<PathBuf>,

		/// For MCP: command to run (e.g., "npx -y @modelcontextprotocol/server-filesystem /path")
		#[arg(short, long, group = "mcp_config")]
		command: Option<String>,

		/// For MCP: URL for HTTP/SSE transport (e.g., "http://localhost:3000")
		#[arg(short, long, group = "mcp_config")]
		url: Option<String>,

		/// For MCP with URL: Transport type (streamable-http, sse)
		#[arg(
			short,
			long,
			value_name = "TYPE",
			default_value = aghub_core::models::DEFAULT_REMOTE_TRANSPORT
		)]
		transport: String,

		/// For MCP with URL: HTTP headers (e.g., "Authorization:Bearer token")
		#[arg(long = "header", value_name = "KEY:VALUE")]
		headers: Vec<String>,

		/// For MCP with command: Environment variables (e.g., "KEY=value")
		#[arg(short = 'e', long = "env", value_name = "KEY=VALUE")]
		env_vars: Vec<String>,

		/// For MCP: request timeout in seconds
		#[arg(long, value_name = "SECONDS")]
		timeout: Option<u64>,

		/// For skill: Description
		#[arg(short, long)]
		description: Option<String>,

		/// For skill: Author name
		#[arg(long)]
		author: Option<String>,

		/// For skill: Version string written into the skill's frontmatter
		/// (no `-v` short — that is the global --verbose)
		#[arg(long)]
		version: Option<String>,

		/// For skill: Comma-separated list of tool names
		#[arg(long, value_delimiter = ',')]
		tools: Vec<String>,

		/// DEPRECATED — no-op. Installs are always symlink-only now: a single
		/// `.agents/skills/<name>` master plus a per-agent link (npx-style). The
		/// flag is accepted (so existing scripts don't error) but ignored; there
		/// is no copy install path.
		#[arg(long, hide = true)]
		universal: bool,
	},
	/// Update an existing resource
	Update {
		#[arg(value_enum)]
		resource: ResourceType,
		name: String,

		/// For MCP: command to run
		#[arg(short, long, group = "mcp_config")]
		command: Option<String>,

		/// For MCP: URL for HTTP/SSE transport
		#[arg(short, long, group = "mcp_config")]
		url: Option<String>,

		/// For MCP with URL: Transport type (streamable-http, sse)
		#[arg(
			short,
			long,
			value_name = "TYPE",
			default_value = aghub_core::models::DEFAULT_REMOTE_TRANSPORT
		)]
		transport: String,

		/// For MCP with URL: HTTP headers
		#[arg(long = "header", value_name = "KEY:VALUE")]
		headers: Vec<String>,

		/// For MCP with command: Environment variables
		#[arg(short = 'e', long = "env", value_name = "KEY=VALUE")]
		env_vars: Vec<String>,

		/// For MCP: request timeout in seconds
		#[arg(long, value_name = "SECONDS")]
		timeout: Option<u64>,

		/// For skill: Description
		#[arg(short, long)]
		description: Option<String>,

		/// For skill: Author name
		#[arg(long)]
		author: Option<String>,

		/// For skill: Version string written into the skill's frontmatter
		/// (no `-v` short — that is the global --verbose)
		#[arg(long)]
		version: Option<String>,

		/// For skill: Comma-separated list of tool names
		#[arg(long, value_delimiter = ',')]
		tools: Vec<String>,
	},
	/// Delete a resource permanently
	Delete {
		#[arg(value_enum)]
		resource: ResourceType,
		name: String,

		/// For skills: remove it from EVERY agent, not just --agent
		/// (destructive; still needs --yes)
		#[arg(long = "all-agents")]
		all_agents: bool,

		/// Force a preview even when --yes is also passed. Delete already
		/// previews without --yes, so this only matters alongside it.
		#[arg(long = "dry-run")]
		dry_run: bool,

		/// Actually perform the removal. Without it delete only previews
		/// (applies to both skills and MCP servers).
		#[arg(short = 'y', long = "yes")]
		yes: bool,
	},
	/// Disable a resource (keeps in config)
	Disable {
		#[arg(value_enum)]
		resource: ResourceType,
		name: String,
	},
	/// Enable a previously disabled resource
	Enable {
		#[arg(value_enum)]
		resource: ResourceType,
		name: String,
	},
	/// Show detailed info about a resource
	Describe {
		#[arg(value_enum)]
		resource: ResourceType,
		name: String,
	},
	/// Check installed skills for available updates (read-only)
	Check {
		#[arg(value_enum)]
		resource: ResourceType,

		/// Check upstream over the network: cheap ls-refs preflight, then a
		/// treeless fetch only when the tip moved. Default is offline (remote
		/// sources are reported `uncheckable`). Read-only on both locks.
		#[arg(long, visible_alias = "check-remote")]
		online: bool,
	},
	/// Apply an available skill update from the lock's source/ref/skillPath.
	ApplyUpdate {
		#[arg(value_enum)]
		resource: ResourceType,
		name: String,

		/// Actually overwrite installed skill files. Without it apply-update
		/// refuses outright — it has no preview mode.
		#[arg(short = 'y', long = "yes")]
		yes: bool,
	},
	/// Prune skill lock entries whose skill is no longer on disk (skills only).
	///
	/// Disk-driven and lock-only: never deletes skill files or edits agent config.
	/// Defaults to a dry-run; pass --yes to write. Scope follows -g/-p/--all.
	PruneLock {
		/// Force a preview even when --yes is also passed. prune-lock already
		/// previews without --yes, so this only matters alongside it.
		#[arg(long = "dry-run")]
		dry_run: bool,

		/// Actually write the pruned lock. Without it prune-lock only previews.
		#[arg(short = 'y', long = "yes")]
		yes: bool,
	},
	/// Manage Claude Code plugins
	Plugin {
		#[command(subcommand)]
		action: plugin::PluginAction,
	},
	/// Install skills from a git repo, and manage the repos already installed
	/// from. `source sync` (alias `install`) is the install entry point.
	Source {
		#[command(subcommand)]
		action: SourceAction,
	},
	/// Manage the inference provider inventory (LLM endpoints + keys)
	Inference {
		#[command(subcommand)]
		action: inference::InferenceAction,
	},
	/// Copy a resource from one agent into one or more target agents
	Transfer {
		#[command(subcommand)]
		action: transfer::TransferAction,
	},
	/// Add/remove a resource across agents to match a desired set
	Reconcile {
		#[command(subcommand)]
		action: transfer::ReconcileAction,
	},
	/// Show per-agent skill coverage of the .agents/skills master (read-only)
	Coverage,
	/// Diagnose installed skills: source, on-disk master, and lock health
	/// (read-only). Scope -g/-p/--all; default spans global + project.
	Doctor {
		/// Verify each selected agent's skill referrer against the Master.
		/// Supports one id, a comma-separated roster, or `-a all`.
		#[arg(long)]
		verify_links: bool,
	},
	/// Show Claude skill usage counts, least-used first (read-only).
	///
	/// Reads Claude Code's own `skillUsage` counter from `~/.claude.json`;
	/// installed skills never dispatched show 0 uses. Claude-only — no other
	/// agent keeps such a counter.
	SkillUsage,
}

/// Actions for the `source` subcommand group.
#[derive(clap::Subcommand, Clone)]
pub enum SourceAction {
	/// List the git sources the installed skills came from
	List,
	/// Show how a source's current content differs from your installed skills
	Diff {
		/// Repo the skills came from: `owner/repo`, an https git URL, or a
		/// source id from `source list`
		source: String,
		/// Branch/tag/commit to compare against (defaults to each skill's
		/// locked ref)
		#[arg(long = "ref", alias = "git-ref")]
		git_ref: Option<String>,
	},
	/// Install skills from a git repo, and refresh ones already installed.
	///
	/// This is also the INSTALL entry point — there is no separate `source
	/// add`. A repo that is not installed yet is fetched the same way:
	///
	///   aghub-cli -p source sync <owner/repo> --install-missing --yes
	///   aghub-cli -p source sync <owner/repo> --skill <name> --install-missing --yes
	///
	/// Neither --install-missing nor --update means "report only". Both
	/// preview unless --yes is passed. Private repos read GIT_PASSWORD (any
	/// host) or GITHUB_TOKEN (github.com only) from the environment.
	#[command(visible_alias = "install")]
	Sync {
		/// Repo to sync from: `owner/repo`, an https git URL, or a source id
		/// from `source list`
		source: String,
		/// Branch/tag/commit to fetch (defaults to each skill's locked ref)
		#[arg(long = "ref", alias = "git-ref")]
		git_ref: Option<String>,
		/// Refresh outdated installed skills in the selected scope. Updates replace
		/// the scoped Master and resync existing referrers; `-a/--agent` does not
		/// narrow update targets (it applies to install/relink actions only).
		#[arg(long)]
		update: bool,
		/// Install missing skills, or idempotently repair explicitly named skills,
		/// for the roster selected by `-a/--agent`.
		#[arg(long)]
		install_missing: bool,
		/// Only act on these skills (comma-separated names, as shown in the NAME
		/// column of `source diff`). Narrows the overview and both
		/// --install-missing and --update; unknown names are reported. With
		/// --install-missing it ENSURES each named skill is linked for the target
		/// agent(s) even when already installed (idempotent repair) — combine
		/// with `-a all` to (re)link a named skill across every supported agent.
		/// Without it, every matching skill in the source is targeted.
		#[arg(long = "skill", value_delimiter = ',', value_name = "NAME")]
		skills: Vec<String>,
		/// DEPRECATED — no-op. `source sync` is always symlink-only now (a single
		/// `.agents/skills/<name>` master plus per-agent links). Accepted so
		/// existing scripts don't error, but ignored; there is no copy install.
		#[arg(long, hide = true)]
		universal: bool,
		/// Actually install/update. Without it sync only previews.
		#[arg(long)]
		yes: bool,
	},
	/// Accept an upstream rename: install the new name and remove the old one
	/// as a single transaction (rolls back on any failure).
	AcceptRename {
		/// Locked name of the skill that was renamed upstream — the
		/// `previousName` of a `renamed` row in `source diff --json`.
		old_name: String,
		/// New upstream name — the `name` of that same `renamed` row
		/// (`check --online` calls the same value `newName`).
		new_name: String,
		/// Branch/tag/commit to read the new name from (defaults to the
		/// locked ref)
		#[arg(long = "ref", alias = "git-ref")]
		git_ref: Option<String>,
		/// Actually commit the rename. Without it accept-rename only previews.
		#[arg(long)]
		yes: bool,
	},
}

#[derive(ValueEnum, Clone, Copy, Debug)]
enum ResourceType {
	#[value(alias = "skill")]
	Skills,
	#[value(alias = "mcp")]
	Mcps,
}

impl ResourceType {
	/// Noun for human-readable output ("added skill 'x'").
	fn singular(self) -> &'static str {
		match self {
			Self::Skills => "skill",
			Self::Mcps => "mcp",
		}
	}
}

fn main() -> Result<()> {
	let cli = Cli::parse();

	// Set global verbose flag
	set_verbose(cli.verbose);

	// The scope flags are `global = true` so they can be written before OR
	// after the subcommand. clap does NOT propagate an ArgGroup to
	// subcommands, so the old `ArgGroup::new("scope")` would have silently
	// stopped enforcing exclusivity the moment the args went global — the
	// check has to be manual, and it has to run for EVERY command, including
	// the ones dispatched early below.
	let picked: Vec<&str> = [
		(cli.global, "-g/--global"),
		(cli.project, "-p/--project"),
		(cli.all, "--all"),
	]
	.into_iter()
	.filter_map(|(set, name)| set.then_some(name))
	.collect();
	if picked.len() > 1 {
		anyhow::bail!(
			"scope flags are mutually exclusive; got {}",
			picked.join(" and ")
		);
	}

	// A comma --agent list is consumed only by the fan-out commands (get /
	// add / update / delete / enable / disable / source sync); every other
	// command ignores the agent flag or is single-agent by nature, so a list
	// there would be silently dropped — reject it up front, BEFORE the early
	// dispatches below. A scalar -a stays ignored on those commands for
	// backcompat.
	if cli.agent.contains(',') && !takes_agent_list(&cli.command) {
		anyhow::bail!(
			"this command does not take an --agent list; pass a single agent \
			 or omit -a (lists work with: get, add, update, delete, enable, \
			 disable, `source sync`, `doctor --verify-links`)"
		);
	}

	// `source` operates on installed skills / git sources, not on a single
	// agent's config. Dispatch it BEFORE the `-a all` special-case and the
	// adapter/ConfigManager setup so it never fails on a missing agent config.
	if let Commands::Source { action } = &cli.command {
		return commands::source::execute(
			action,
			cli.global,
			cli.project,
			cli.all,
			&cli.agent,
			cli.json,
		);
	}

	// `apply-update` is driven by the skill lock + Master, not an agent's
	// config. Dispatch it before adapter/ConfigManager setup so an unrelated
	// missing or malformed agent config cannot block the Resync.
	if let Commands::ApplyUpdate {
		resource,
		name,
		yes,
	} = &cli.command
	{
		let (scope, project_root) =
			resolve_scope_and_root(&cli, ScopePolicy::AllowBoth)?;
		return commands::apply_update::execute(
			*resource,
			name.clone(),
			scope,
			project_root.as_deref(),
			*yes,
			cli.json,
		);
	}

	// Inference inventory is not agent-scoped (it's the shared provider store +
	// keyring). Dispatch it before the adapter/ConfigManager setup too.
	if let Commands::Inference { action } = &cli.command {
		return commands::inference::execute(action, cli.json);
	}

	// `transfer` / `reconcile` span MULTIPLE agents (source + targets), so they
	// resolve their own per-target scope and are dispatched before the
	// single-agent adapter/ConfigManager setup. They take a single writing
	// scope (-g/-p); the top-level `--all` has no meaning here, so reject it
	// rather than silently ignoring it (mirrors `coverage`).
	if matches!(
		cli.command,
		Commands::Transfer { .. } | Commands::Reconcile { .. }
	) && cli.all
	{
		anyhow::bail!(
			"transfer/reconcile support only 'global' or 'project' scope, not \
			 'all'; pass -g/--global or -p/--project"
		);
	}
	if let Commands::Transfer { action } = &cli.command {
		return commands::transfer::execute_transfer(
			action,
			cli.global,
			cli.project,
			cli.json,
		);
	}
	if let Commands::Reconcile { action } = &cli.command {
		return commands::transfer::execute_reconcile(
			action,
			cli.global,
			cli.project,
			cli.json,
		);
	}

	// `coverage` classifies EVERY registered agent against the per-scope master,
	// so it is not single-agent scoped; dispatch it before the adapter setup.
	if let Commands::Coverage = &cli.command {
		return commands::coverage::execute(
			cli.global,
			cli.project,
			cli.all,
			cli.json,
		);
	}

	// `doctor` reconciles the skill lock against the on-disk master across
	// scopes; not single-agent scoped, so dispatch before adapter setup.
	if let Commands::Doctor { verify_links } = &cli.command {
		return commands::doctor::execute_with_options(
			cli.global,
			cli.project,
			cli.json,
			*verify_links,
			&cli.agent,
		);
	}

	// `skill-usage` reads Claude's global `skillUsage` counter — it is
	// Claude-global, not single-agent scoped, so dispatch before adapter setup.
	if let Commands::SkillUsage = &cli.command {
		return commands::skill_usage::execute(cli.project, cli.all, cli.json);
	}

	// Parse the agent flag — "all" (case-insensitive), a single id, or a
	// comma-separated list — through the ONE shared parser.
	let agents = match AgentSelection::parse(&cli.agent)
		.map_err(|e| anyhow::anyhow!("invalid --agent: {e}"))?
	{
		AgentSelection::All => return handle_all_agents(&cli),
		AgentSelection::List(agents) => agents,
	};
	// A SYNTACTIC list keeps the list output contract even when dedup (or an
	// alias) collapses it to one agent — `-a claude,claude` must produce the
	// same top-level shape as `-a claude,opencode`, or scripts break on
	// equivalent inputs.
	if cli.agent.contains(',') || agents.len() > 1 {
		return handle_agent_list(&cli, &agents);
	}
	let agent_type = agents[0];
	eprintln_verbose!("Agent type: {}", cli.agent);
	match run_for_agent(&cli, agent_type)? {
		Some(payload) => {
			if cli.json {
				println!("{}", serde_json::to_string_pretty(&payload)?);
			} else {
				print!("{}", render_mutation(&cli.command, &payload));
			}
			Ok(())
		}
		None => Ok(()),
	}
}

/// Render one mutating command's JSON payload as human-readable text.
///
/// Dispatches on the COMMAND, not on the payload's keys: `add`/`update` emit a
/// bare `SkillView`/`McpServer` with no discriminator, so sniffing fields would
/// guess wrong the moment either shape gains a key. Every arm falls back to the
/// name so an unexpected payload still says what it acted on.
fn render_mutation(command: &Commands, payload: &serde_json::Value) -> String {
	let name = payload
		.get("name")
		.and_then(|v| v.as_str())
		.unwrap_or("(unnamed)");
	match command {
		Commands::Add { resource, .. } => {
			// An idempotent re-add writes nothing; saying "added" there
			// contradicts the note `add` prints and reads as an overwrite.
			if payload.get("already_installed").and_then(|v| v.as_bool())
				== Some(true)
			{
				format!(
					"{} '{name}' is already installed\n",
					resource.singular()
				)
			} else {
				format!("added {} '{name}'\n", resource.singular())
			}
		}
		Commands::Update { resource, .. } => {
			format!("updated {} '{name}'\n", resource.singular())
		}
		Commands::Enable { resource, .. } => {
			format!("enabled {} '{name}'\n", resource.singular())
		}
		Commands::Disable { resource, .. } => {
			format!("disabled {} '{name}'\n", resource.singular())
		}
		Commands::Delete {
			resource,
			yes,
			dry_run,
			..
		} => render_removal(*resource, name, payload, !yes || *dry_run),
		// `run_for_agent` only returns a payload for the six arms above.
		_ => format!("{name}\n"),
	}
}

/// Render a `RemovalView` payload. A preview MUST say how to commit it and a
/// commit MUST disclose the Master left behind — the JSON carried both facts in
/// `needs_confirm` / `skipped`, where a human running `delete` never saw them
/// and could read `"success": true` as "it was removed".
///
/// It must NOT claim the resource is absent. `RemovalView` cannot express that:
/// an MCP that exists and one that does not serialize IDENTICALLY (MCP removal
/// rewrites shared config and deletes no disk path, so `paths` is deliberately
/// always empty — root AGENTS.md "MCP removal contract"), and a skill's noop
/// looks the same as a skill whose files are already gone. So the wording stays
/// inside what the payload proves: which paths, if any, are involved.
fn render_removal(
	resource: ResourceType,
	name: &str,
	payload: &serde_json::Value,
	is_preview: bool,
) -> String {
	let kind = resource.singular();
	let flag = |key: &str| payload.get(key).and_then(|v| v.as_bool());
	let list = |key: &str| -> Vec<&str> {
		payload
			.get(key)
			.and_then(|v| v.as_array())
			.map(|a| a.iter().filter_map(|p| p.as_str()).collect())
			.unwrap_or_default()
	};
	let paths = list("paths");
	let skipped = list("skipped");
	let mut out = String::new();

	// What removal actually touches, per resource. Naming it beats printing an
	// empty path list under "would remove:" and leaving the user to guess.
	let target = match resource {
		ResourceType::Mcps => "the agent's MCP config entry",
		ResourceType::Skills => {
			"no installed files (nothing on disk to remove)"
		}
	};

	if flag("executed") != Some(true) && is_preview {
		if paths.is_empty() {
			out.push_str(&format!("would remove {kind} '{name}': {target}\n"));
		} else {
			out.push_str(&format!("would remove {kind} '{name}':\n"));
			for p in &paths {
				out.push_str(&format!("  {p}\n"));
			}
		}
		out.push_str("re-run with --yes to remove\n");
	} else if flag("executed") != Some(true) {
		// `--yes` was given and nothing ran: the resource was already gone
		// (`RemovalOutcome::noop`). Telling the caller to "re-run with --yes"
		// there is a loop that never terminates — a script retrying on that
		// hint would spin forever. Delete stays idempotent (exit 0).
		out.push_str(&format!("{kind} '{name}': nothing to remove\n"));
	} else if paths.is_empty() {
		out.push_str(&format!("removed {kind} '{name}': {target}\n"));
	} else {
		out.push_str(&format!("removed {kind} '{name}':\n"));
		for p in &paths {
			out.push_str(&format!("  {p}\n"));
		}
	}

	if !skipped.is_empty() {
		out.push_str("kept (shared with other agents):\n");
		for p in &skipped {
			out.push_str(&format!("  {p}\n"));
		}
		out.push_str(
			"note: the .agents/skills Master above is NOT removed. `source \
			 sync` refuses to overwrite an existing Master, so delete it by \
			 hand before reinstalling this skill from git.\n",
		);
	}
	if let Some(err) = payload.get("prune_error").and_then(|v| v.as_str()) {
		out.push_str(&format!("lock prune failed: {err}\n"));
	}
	out
}

/// True for the commands that fan out across an --agent list. Everything
/// else either ignores the agent flag or is single-agent by nature.
fn takes_agent_list(command: &Commands) -> bool {
	matches!(
		command,
		Commands::Get { .. }
			| Commands::Add { .. }
			| Commands::Update { .. }
			| Commands::Delete { .. }
			| Commands::Enable { .. }
			| Commands::Disable { .. }
			| Commands::Source {
				action: SourceAction::Sync { .. }
			} | Commands::Doctor {
			verify_links: true,
			..
		}
	)
}

#[derive(Clone, Copy)]
enum ScopePolicy {
	AllowBoth,
	SingleWrite,
}

fn scope_policy(command: &Commands) -> ScopePolicy {
	match command {
		Commands::Add { .. }
		| Commands::Update { .. }
		| Commands::Delete { .. }
		| Commands::Enable { .. }
		| Commands::Disable { .. } => ScopePolicy::SingleWrite,
		// Any new MUTATING subcommand must be added to the SingleWrite arm
		// above, or it silently bypasses the project-root guard.
		_ => ScopePolicy::AllowBoth,
	}
}

/// Resolve top-level scope flags once for every generic command path.
///
/// Read-only commands may span both scopes. Generic CRUD mutations must pick
/// one write target so a read scope can never disagree with the config that is
/// mutated.
fn resolve_scope_and_root(
	cli: &Cli,
	policy: ScopePolicy,
) -> Result<(ResourceScope, Option<PathBuf>)> {
	if cli.all && matches!(policy, ScopePolicy::SingleWrite) {
		anyhow::bail!(
			"generic mutation does not support --all; pass -g/--global or \
			 -p/--project"
		);
	}

	let scope = if cli.all {
		ResourceScope::Both
	} else if cli.project {
		ResourceScope::ProjectOnly
	} else {
		ResourceScope::GlobalOnly
	};
	let project_root = if scope == ResourceScope::GlobalOnly {
		None
	} else {
		let current_dir = std::env::current_dir()?;
		find_project_root(&current_dir)
	};

	// A single-write mutation targeting -p must fail here, before any config
	// is touched, rather than silently falling back to the global write.
	if matches!(policy, ScopePolicy::SingleWrite)
		&& scope == ResourceScope::ProjectOnly
		&& project_root.is_none()
	{
		anyhow::bail!(
			"no project root found from the current directory; run this \
			 inside a project (a directory with an agent config marker, \
			 e.g. .claude/, .mcp.json, or skills-lock.json) or pass \
			 -g/--global"
		);
	}

	Ok((scope, project_root))
}

/// Run one command against ONE agent's config. The multi-agent entry points
/// (`handle_all_agents`, `handle_agent_list`) fan out to this.
///
/// Mutating commands return `Some(payload)` — the caller prints it
/// (single-agent) or wraps it in the batch envelope (multi-agent). Commands
/// that manage their own output return `None`.
fn run_for_agent(
	cli: &Cli,
	agent_type: AgentType,
) -> Result<Option<serde_json::Value>> {
	let (scope, project_root) =
		resolve_scope_and_root(cli, scope_policy(&cli.command))?;

	// Determine which config file to use for writes (primary scope)
	let use_global_config = if cli.global {
		true
	} else if cli.project {
		false
	} else if cli.all {
		// For --all, use project config as primary if available
		project_root.is_some()
	} else {
		true // default to global
	};

	eprintln_verbose!("Resource scope: {:?}", scope);
	if let Some(ref root) = project_root {
		eprintln_verbose!("Project root: {}", root.display());
	}

	// Create adapter and manager with scope
	let adapter = create_adapter(agent_type);
	let mut manager = ConfigManager::with_scope(
		adapter,
		use_global_config,
		project_root.as_deref(),
		scope,
	);
	eprintln_verbose!("Config manager created");
	if let Some(config_path) = manager.config_path() {
		eprintln_verbose!("Config file: {}", config_path.display());
	}

	// Load existing config (or fail if not found)
	eprintln_verbose!("Loading configuration...");
	match manager.load() {
		Ok(_) => {
			eprintln_verbose!("Configuration loaded successfully");
		}
		Err(e) => {
			// If config not found and we're adding, that's okay - we'll create it.
			// `check` is read-only and reads the lock file, not the agent config,
			// so a missing config is also fine.
			let tolerate_missing = matches!(
				cli.command,
				Commands::Add { .. }
					| Commands::Check { .. }
					| Commands::PruneLock { .. }
					| Commands::Delete { .. }
			);
			if tolerate_missing {
				eprintln_verbose!(
					"No existing config found, will create new configuration"
				);
			} else {
				return Err(anyhow::anyhow!("Failed to load config: {}", e));
			}
		}
	}

	// Execute command (cloned so multi-agent entry points can re-run it).
	// Mutating commands return their JSON payload for the caller to print
	// or collect; the rest print for themselves and yield None.
	match cli.command.clone() {
		Commands::Get { resource } => {
			get::execute(&manager, resource, cli.json).map(|()| None)
		}
		Commands::Add {
			resource,
			name,
			from,
			command,
			url,
			transport,
			headers,
			env_vars,
			timeout,
			description,
			author,
			version,
			tools,
			universal,
		} => add::execute(
			&mut manager,
			resource,
			name,
			from,
			command,
			url,
			transport,
			headers,
			env_vars,
			timeout,
			description,
			author,
			version,
			tools,
			universal,
		)
		.map(Some),
		Commands::Update {
			resource,
			name,
			command,
			url,
			transport,
			headers,
			env_vars,
			timeout,
			description,
			author,
			version,
			tools,
		} => update::execute(
			&mut manager,
			resource,
			name,
			command,
			url,
			transport,
			headers,
			env_vars,
			timeout,
			description,
			author,
			version,
			tools,
		)
		.map(Some),
		Commands::Delete {
			resource,
			name,
			all_agents,
			dry_run,
			yes,
		} => delete::execute(
			&mut manager,
			resource,
			name,
			delete::DeleteOptions {
				all_agents,
				dry_run,
				yes,
			},
		)
		.map(Some),
		Commands::Disable { resource, name } => {
			disable::execute(&mut manager, resource, name).map(Some)
		}
		Commands::Enable { resource, name } => {
			enable::execute(&mut manager, resource, name).map(Some)
		}
		Commands::Describe { resource, name } => {
			describe::execute(&manager, resource, name, cli.json).map(|()| None)
		}
		Commands::Check { resource, online } => check::execute(
			resource,
			scope,
			project_root.as_deref(),
			online,
			cli.json,
		)
		.map(|()| None),
		Commands::ApplyUpdate { .. } => {
			unreachable!(
				"`apply-update` is dispatched before agent-config setup"
			)
		}
		Commands::PruneLock { dry_run, yes } => prune::execute(
			scope,
			project_root.as_deref(),
			dry_run || !yes,
			cli.json,
		)
		.map(|()| None),
		Commands::Plugin { action } => {
			// Plugin management is Claude-specific
			if agent_type != AgentType::Claude {
				return Err(anyhow::anyhow!(
					"Plugin management is only supported for Claude Code. Use -a claude"
				));
			}
			plugin::execute(action, cli.json).map(|()| None)
		}
		// Dispatched earlier in `main`, before adapter/manager setup.
		Commands::Source { .. } => {
			unreachable!("`source` is dispatched before agent-config setup")
		}
		Commands::Inference { .. } => {
			unreachable!("`inference` is dispatched before agent-config setup")
		}
		Commands::Transfer { .. } => {
			unreachable!("`transfer` is dispatched before agent-config setup")
		}
		Commands::Reconcile { .. } => {
			unreachable!("`reconcile` is dispatched before agent-config setup")
		}
		Commands::Coverage => {
			unreachable!("`coverage` is dispatched before agent-config setup")
		}
		Commands::Doctor { .. } => {
			unreachable!("`doctor` is dispatched before agent-config setup")
		}
		Commands::SkillUsage => {
			unreachable!(
				"`skill-usage` is dispatched before agent-config setup"
			)
		}
	}
}

// Handle --agent all: list resources for every registered agent
fn handle_all_agents(cli: &Cli) -> Result<()> {
	let resource = match &cli.command {
		Commands::Get { resource } => *resource,
		_ => {
			return Err(anyhow::anyhow!(
				"--agent all supports only 'get'; to fan a command across \
				 specific agents pass a comma-separated list (-a claude,grok)"
			))
		}
	};

	let (scope, project_root) =
		resolve_scope_and_root(cli, ScopePolicy::AllowBoth)?;
	eprintln_verbose!("Loading resources for all agents (scope: {:?})", scope);
	let resources = load_all_agents(scope, project_root.as_deref());
	get::execute_all(resources, resource, cli.json)
}

// Handle a comma-separated --agent list: fan the command across the named
// agents. `get` aggregates (same JSON shape as `--agent all`); mutating
// commands map onto the SHARED core batch policy (`aghub_core::batch`):
// preflight before any write, attempt every agent, one JSON envelope on
// stdout, non-zero exit if any failed. (`source sync` is dispatched earlier
// and resolves the list itself; the top-of-main guard rejects lists on
// every other command.)
fn handle_agent_list(cli: &Cli, agents: &[AgentType]) -> Result<()> {
	match &cli.command {
		Commands::Get { resource } => {
			let resource = *resource;
			let (scope, project_root) =
				resolve_scope_and_root(cli, ScopePolicy::AllowBoth)?;
			let mut resources = load_all_agents(scope, project_root.as_deref());
			resources
				.retain(|r| agents.iter().any(|a| a.as_str() == r.agent_id));
			get::execute_all(resources, resource, cli.json)
		}
		Commands::Add { resource, .. }
		| Commands::Update { resource, .. }
		| Commands::Delete { resource, .. }
		| Commands::Enable { resource, .. }
		| Commands::Disable { resource, .. } => {
			// Preflight judges the same write scope `run_for_agent` resolves;
			// the policy itself (which capabilities, all-before-any-write)
			// lives in core; MCPs share it with the API's /mcps/batch.
			let view = if matches!(resource, ResourceType::Mcps) {
				let (write_scope, _) =
					resolve_scope_and_root(cli, ScopePolicy::SingleWrite)?;
				let is_toggle = matches!(
					cli.command,
					Commands::Enable { .. } | Commands::Disable { .. }
				);
				aghub_core::batch::run_mcp_agent_mutation(
					agents,
					write_scope,
					is_toggle,
					|agent| {
						eprintln_verbose!(
							"Running for agent: {}",
							agent.as_str()
						);
						run_for_agent(cli, agent)
							.map(|o| o.unwrap_or(serde_json::Value::Null))
							.map_err(|e| format!("{e:#}"))
					},
				)
				.map_err(|e| anyhow::anyhow!("{e}"))?
			} else {
				let (write_scope, _) =
					resolve_scope_and_root(cli, ScopePolicy::SingleWrite)?;
				aghub_core::batch::run_skill_agent_mutation(
					agents,
					write_scope,
					|agent| {
						eprintln_verbose!(
							"Running for agent: {}",
							agent.as_str()
						);
						run_for_agent(cli, agent)
							// Mutating commands always yield a payload; Null keeps
							// the row well-formed if that invariant ever slips.
							.map(|o| o.unwrap_or(serde_json::Value::Null))
							.map_err(|e| format!("{e:#}"))
					},
				)
				.map_err(|e| anyhow::anyhow!("{e}"))?
			};
			if cli.json {
				println!("{}", serde_json::to_string_pretty(&view)?);
			} else {
				for row in &view.results {
					if row.ok {
						let payload = row
							.output
							.clone()
							.unwrap_or(serde_json::Value::Null);
						print!(
							"{}: {}",
							row.agent,
							render_mutation(&cli.command, &payload)
						);
					} else {
						println!(
							"{}: FAILED — {}",
							row.agent,
							row.error.as_deref().unwrap_or("unknown error")
						);
					}
				}
				println!(
					"{} ok, {} failed",
					view.success_count, view.failed_count
				);
			}
			if view.failed_count > 0 {
				anyhow::bail!(
					"{} of {} agent(s) failed",
					view.failed_count,
					view.results.len()
				);
			}
			Ok(())
		}
		_ => Err(anyhow::anyhow!(
			"an --agent list supports get/add/update/delete/enable/disable; \
			 run this command with a single agent"
		)),
	}
}

// Describe command - a key/value block, or the raw view under --json
mod describe {
	use super::*;
	use crate::commands::print_value;

	pub fn execute(
		manager: &ConfigManager,
		resource: ResourceType,
		name: String,
		json: bool,
	) -> Result<()> {
		let config = manager.config().context("No configuration loaded")?;

		let resource_type_str = match resource {
			ResourceType::Skills => "skill",
			ResourceType::Mcps => "mcp",
		};
		eprintln_verbose!("Describing {}: {}", resource_type_str, name);

		match resource {
			ResourceType::Skills => {
				let skill = config
					.skills
					.iter()
					.find(|s| s.name == name)
					.with_context(|| format!("Skill '{}' not found", name))?;
				eprintln_verbose!("Found skill: {}", skill.name);
				// Same SkillView shape as `add`/API. describe does no install
				// prep, so native_reader stays false.
				let view = aghub_core::dto::SkillView::from(skill);
				let mut value = serde_json::to_value(&view)?;
				// `native_reader` / `already_installed` are INSTALL advisories.
				// describe does no install, so both are always false here, and
				// "already_installed: false" on an installed skill reads as a
				// contradiction. --json keeps them (one wire shape).
				if !json {
					if let Some(map) = value.as_object_mut() {
						map.remove("native_reader");
						map.remove("already_installed");
					}
				}
				print_value(&value, json)?;
			}
			ResourceType::Mcps => {
				let mcp =
					config.mcps.iter().find(|m| m.name == name).with_context(
						|| format!("MCP server '{}' not found", name),
					)?;
				eprintln_verbose!("Found MCP server: {}", mcp.name);
				print_value(&serde_json::to_value(mcp)?, json)?;
			}
		}

		Ok(())
	}
}
