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
#[command(group(
	clap::ArgGroup::new("scope")
		.args(["global", "project", "all"])
		.multiple(false)
))]
struct Cli {
	/// Target agent id, a comma-separated list, or "all"
	/// (e.g. -a claude / -a claude,grok / -a all)
	#[arg(short = 'a', long, default_value = "claude")]
	agent: String,

	/// Use global config (forces global-only scope)
	#[arg(short, long)]
	global: bool,

	/// Show only project resources (project-only scope)
	#[arg(short, long)]
	project: bool,

	/// Show both project and global resources
	#[arg(long)]
	all: bool,

	/// Enable verbose output (to stderr)
	#[arg(short, long)]
	verbose: bool,

	#[command(subcommand)]
	command: Commands,
}

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

		/// For skill: Version
		#[arg(short, long)]
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

		/// For skill: Version
		#[arg(short, long)]
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

		/// For skills: remove the skill from EVERY agent (destructive; needs --yes)
		#[arg(long = "all-agents")]
		all_agents: bool,

		/// For skills: only list what would be removed (this is the default)
		#[arg(long = "dry-run")]
		dry_run: bool,

		/// For skills: actually perform the removal (without it, delete is a dry-run)
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

		/// Emit machine-readable JSON (default output is also JSON today)
		#[arg(long)]
		json: bool,
	},
	/// Apply an available skill update from the lock's source/ref/skillPath.
	ApplyUpdate {
		#[arg(value_enum)]
		resource: ResourceType,
		name: String,

		/// Actually overwrite installed skill files.
		#[arg(short = 'y', long = "yes")]
		yes: bool,

		/// Emit machine-readable JSON (default output is also JSON today)
		#[arg(long)]
		json: bool,
	},
	/// Prune skill lock entries whose skill is no longer on disk (skills only).
	///
	/// Disk-driven and lock-only: never deletes skill files or edits agent config.
	/// Defaults to a dry-run; pass --yes to write. Scope follows -g/-p/--all.
	PruneLock {
		/// Only report what would be pruned (this is the default)
		#[arg(long = "dry-run")]
		dry_run: bool,

		/// Actually write the pruned lock (without it, prune-lock is a dry-run)
		#[arg(short = 'y', long = "yes")]
		yes: bool,

		/// Emit machine-readable JSON (default output is also JSON today)
		#[arg(long)]
		json: bool,
	},
	/// Manage Claude Code plugins
	Plugin {
		#[command(subcommand)]
		action: plugin::PluginAction,
	},
	/// Manage skill sources (git repos you've installed skills from)
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
	Coverage {
		/// Emit a machine-readable JSON array instead of a table
		#[arg(long)]
		json: bool,
	},
	/// Diagnose installed skills: source, on-disk master, and lock health
	/// (read-only). Scope -g/-p/--all; default spans global + project.
	Doctor {
		/// Verify each selected agent's skill referrer against the Master.
		/// Supports one id, a comma-separated roster, or `-a all`.
		#[arg(long)]
		verify_links: bool,

		/// Emit a machine-readable JSON array instead of a table
		#[arg(long)]
		json: bool,
	},
	/// Show Claude skill usage counts, least-used first (read-only).
	///
	/// Reads Claude Code's own `skillUsage` counter from `~/.claude.json`;
	/// installed skills never dispatched show 0 uses. Claude-only — no other
	/// agent keeps such a counter.
	SkillUsage {
		/// Emit a machine-readable JSON array instead of a table
		#[arg(long)]
		json: bool,
	},
}

/// Actions for the `source` subcommand group.
#[derive(clap::Subcommand, Clone)]
pub enum SourceAction {
	/// List installed skill sources
	List {
		#[arg(long)]
		json: bool,
	},
	/// Show how a source differs from its installed skills
	Diff {
		source: String,
		#[arg(long = "ref", alias = "git-ref")]
		git_ref: Option<String>,
		#[arg(long)]
		json: bool,
	},
	/// Sync installed skills with a source
	Sync {
		source: String,
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
		#[arg(long)]
		yes: bool,
		#[arg(long)]
		json: bool,
	},
	/// Accept an upstream rename: install the new name and remove the old one
	/// as a single transaction (rolls back on any failure).
	AcceptRename {
		/// Locked name of the skill that was renamed upstream.
		old_name: String,
		/// New upstream name (from the source's `renamed.newName`).
		new_name: String,
		#[arg(long = "ref", alias = "git-ref")]
		git_ref: Option<String>,
		/// Commit changes. Default is a dry run.
		#[arg(long)]
		yes: bool,
		#[arg(long)]
		json: bool,
	},
}

#[derive(ValueEnum, Clone, Copy, Debug)]
enum ResourceType {
	#[value(alias = "skill")]
	Skills,
	#[value(alias = "mcp")]
	Mcps,
}

fn main() -> Result<()> {
	let cli = Cli::parse();

	// Set global verbose flag
	set_verbose(cli.verbose);

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
			 disable, source sync)"
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
		);
	}

	// `apply-update` is driven by the skill lock + Master, not an agent's
	// config. Dispatch it before adapter/ConfigManager setup so an unrelated
	// missing or malformed agent config cannot block the Resync.
	if let Commands::ApplyUpdate {
		resource,
		name,
		yes,
		json,
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
			*json,
		);
	}

	// Inference inventory is not agent-scoped (it's the shared provider store +
	// keyring). Dispatch it before the adapter/ConfigManager setup too.
	if let Commands::Inference { action } = &cli.command {
		return commands::inference::execute(action);
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
		);
	}
	if let Commands::Reconcile { action } = &cli.command {
		return commands::transfer::execute_reconcile(
			action,
			cli.global,
			cli.project,
		);
	}

	// `coverage` classifies EVERY registered agent against the per-scope master,
	// so it is not single-agent scoped; dispatch it before the adapter setup.
	if let Commands::Coverage { json } = &cli.command {
		return commands::coverage::execute(
			cli.global,
			cli.project,
			cli.all,
			*json,
		);
	}

	// `doctor` reconciles the skill lock against the on-disk master across
	// scopes; not single-agent scoped, so dispatch before adapter setup.
	if let Commands::Doctor { verify_links, json } = &cli.command {
		return commands::doctor::execute_with_options(
			cli.global,
			cli.project,
			*json,
			*verify_links,
			&cli.agent,
		);
	}

	// `skill-usage` reads Claude's global `skillUsage` counter — it is
	// Claude-global, not single-agent scoped, so dispatch before adapter setup.
	if let Commands::SkillUsage { json } = &cli.command {
		return commands::skill_usage::execute(cli.project, cli.all, *json);
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
			println!("{}", serde_json::to_string_pretty(&payload)?);
			Ok(())
		}
		None => Ok(()),
	}
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
			get::execute(&manager, resource).map(|()| None)
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
			describe::execute(&manager, resource, name).map(|()| None)
		}
		Commands::Check {
			resource,
			online,
			json,
		} => check::execute(
			resource,
			scope,
			project_root.as_deref(),
			online,
			json,
		)
		.map(|()| None),
		Commands::ApplyUpdate { .. } => {
			unreachable!(
				"`apply-update` is dispatched before agent-config setup"
			)
		}
		Commands::PruneLock { dry_run, yes, json } => prune::execute(
			scope,
			project_root.as_deref(),
			dry_run || !yes,
			json,
		)
		.map(|()| None),
		Commands::Plugin { action } => {
			// Plugin management is Claude-specific
			if agent_type != AgentType::Claude {
				return Err(anyhow::anyhow!(
					"Plugin management is only supported for Claude Code. Use -a claude"
				));
			}
			plugin::execute(action).map(|()| None)
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
		Commands::Coverage { .. } => {
			unreachable!("`coverage` is dispatched before agent-config setup")
		}
		Commands::Doctor { .. } => {
			unreachable!("`doctor` is dispatched before agent-config setup")
		}
		Commands::SkillUsage { .. } => {
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
	get::execute_all(resources, resource)
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
			get::execute_all(resources, resource)
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
			println!("{}", serde_json::to_string_pretty(&view)?);
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

// Describe command - outputs JSON
mod describe {
	use super::*;

	pub fn execute(
		manager: &ConfigManager,
		resource: ResourceType,
		name: String,
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
				println!("{}", serde_json::to_string_pretty(&view)?);
			}
			ResourceType::Mcps => {
				let mcp =
					config.mcps.iter().find(|m| m.name == name).with_context(
						|| format!("MCP server '{}' not found", name),
					)?;
				eprintln_verbose!("Found MCP server: {}", mcp.name);
				println!("{}", serde_json::to_string_pretty(mcp)?);
			}
		}

		Ok(())
	}
}
