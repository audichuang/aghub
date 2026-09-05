use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use aghub_core::{
	adapters::create_adapter,
	errors::ConfigError,
	load_all_agents,
	manager::ConfigManager,
	models::{AgentSelection, AgentType, ResourceScope},
	paths::find_project_root,
};

mod commands;

use commands::{
	add, check, delete, disable, enable, get, inference, plugin, prune, repair,
	transfer, update,
};

/// Global verbose flag used by the eprintln_verbose macro
static VERBOSE: AtomicBool = AtomicBool::new(false);

/// Set when a command has ALREADY written its complete answer to stdout and is
/// returning `Err` only to carry a non-zero exit code.
///
/// The multi-agent batch envelope, `transfer`/`reconcile` and a partial
/// `prune-lock` all report per-row failures in the payload itself, then bail so
/// the exit code is non-zero. Their payload IS the answer, so
/// [`report_failure`] must not append a second JSON document — that turns
/// stdout into two concatenated documents and every `JSON.parse` on it fails
/// with "trailing characters".
static ANSWER_ON_STDOUT: AtomicBool = AtomicBool::new(false);

/// Declare that stdout already carries this command's full answer. Call it
/// BEFORE returning the `Err` that only sets the exit code.
pub fn note_answer_on_stdout() {
	ANSWER_ON_STDOUT.store(true, Ordering::Relaxed);
}

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

	/// Read AND write global config only.
	///
	/// This is the default for every command EXCEPT the cross-scope
	/// diagnostics `doctor`, `check`, `source list` and `source diff`, which
	/// span global + the current project unless you narrow them.
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
  -p/--project  the current project only; ANY command (reads included) fails
                if no project root is found from the current directory
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

  A --json FAILURE is JSON too: an `error` object carrying code, message and
  retryable, on stdout, exit 1. `code` is the same vocabulary the HTTP API
  sends. A clap usage error stays exit 2 with prose instead.

  Key casing is NOT uniform, and the split is deliberate: each command mirrors
  the API/desktop DTO it shares a wire shape with. snake_case for delete
  (dry_run/needs_confirm/deleted_path/outcome), reconcile (dry_run), coverage,
  and the batch envelope (success_count/failed_count). camelCase for prune-lock
  (dryRun), source sync (dryRun/skillPath/targetAgents), source list, doctor
  and check. Read `delete`'s `outcome` field (preview | removed | absent |
  partial) rather than deriving intent from dry_run/executed: `partial` means
  the removal ran and at least one path could NOT be deleted, which those two
  booleans cannot express at all.

  Multi-agent runs wrap rows in {success_count, failed_count, results:[…]}.
  A row carries BOTH `ok` and `success` (same value); a FAILED row replaces
  `output` with `error`, so do not assume `output` is present.

Destructive commands (delete, apply-update, prune-lock, source sync, source
accept-rename, reconcile with --remove) preview by default and only write with
--yes.";

/// Verbatim examples for `source sync --help`. Kept out of the doc comment
/// because clap re-wraps those and ran the two lines together.
const SYNC_EXAMPLES: &str = "\
Examples:
  aghub-cli -p source sync <owner/repo> --install-missing --yes
  aghub-cli -p source sync <owner/repo> --skill <name> --install-missing --yes";

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
		/// `.aghub/<name>` master plus a per-agent link. The flag is accepted
		/// (so existing scripts don't error) but ignored; there is no copy
		/// install path.
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
	/// Delete a resource permanently.
	///
	/// SIDE EFFECT (skills, committed runs only): after removing the skill,
	/// this reconciles the WHOLE scope's lock against disk, so it also drops
	/// lock entries for OTHER skills that are no longer on disk. Committed runs
	/// report those keys as `pruned_lock_entries` in `--json`; the PREVIEW
	/// discloses the same keys under `would_prune_lock_entries` — a separate
	/// key on purpose, so a preview can never read as "these were dropped".
	///
	/// Read `outcome`, not `dry_run`/`executed`: `preview` | `removed` |
	/// `absent` | `partial` | `kept`. `kept` means the `.aghub/<name>` master
	/// was left because another agent still reads it — `success: true` AND THE
	/// SKILL IS STILL THERE; the payload's `skipped` names what was kept.
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
	/// Disable an MCP server (keeps it in config).
	///
	/// MCP servers only — see [`McpResource`]. Supported by codex, opencode and
	/// amp; every other agent's descriptor refuses it.
	Disable {
		#[arg(value_enum)]
		resource: McpResource,
		name: String,
	},
	/// Re-enable a previously disabled MCP server.
	///
	/// MCP servers only — see [`McpResource`]. Supported by codex, opencode and
	/// amp; every other agent's descriptor refuses it.
	Enable {
		#[arg(value_enum)]
		resource: McpResource,
		name: String,
	},
	/// Show detailed info about a resource
	Describe {
		#[arg(value_enum)]
		resource: ResourceType,
		name: String,
	},
	/// Check installed skills for available updates (read-only).
	///
	/// Scope defaults to BOTH global and the current project, like the other
	/// read-only diagnostics (`doctor`, `source list`, `source diff`); `-g` /
	/// `-p` still narrow it. It used to follow the plain global default and so,
	/// run inside a project, answered "up to date" from the global lock alone
	/// without ever reading the project's.
	///
	/// Offline by default: remote sources report `uncheckable`/`network` with
	/// `checked: false`. Pass `--online` for a real update check.
	Check {
		/// Skills only — see [`SkillResource`].
		#[arg(value_enum)]
		resource: SkillResource,

		/// Check upstream over the network: a tip preflight that downloads no
		/// objects, then a treeless fetch only when the tip moved. Default is
		/// offline (remote sources are reported `uncheckable`). Read-only on
		/// both locks.
		#[arg(long, visible_alias = "check-remote")]
		online: bool,

		/// Write a sidecar JSON summary (started/finished, counts, per-skill
		/// views). Check itself stays read-only and does not mutate locks.
		/// Omit PATH to use `$AGHUB_DATA_DIR/skill-check-last.json` (or the
		/// platform app data dir).
		#[arg(long = "write-result", value_name = "PATH", num_args = 0..=1, default_missing_value = "")]
		write_result: Option<PathBuf>,
	},
	/// Apply an available skill update from the lock's source/ref/skillPath.
	ApplyUpdate {
		/// Skills only — see [`SkillResource`].
		#[arg(value_enum)]
		resource: SkillResource,
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
	/// Repair a skill's on-disk layout: migrate, relink, or reconcile it.
	///
	/// One verb for every non-conformant shape — from inside an agent session you
	/// know the skill is misbehaving, not which shape you hit. Omit <NAME> to
	/// repair every skill the lock names at this scope. Defaults to a dry-run;
	/// pass --yes to apply. Exits 1 when something was refused.
	#[command(alias = "migrate")]
	Repair {
		/// Skill to repair. Omitted = every skill the lock names at this scope.
		name: Option<String>,

		/// Actually write. Without it repair only previews.
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
	/// Show which agents share a skills directory and which need a per-agent
	/// link (read-only).
	///
	/// A static per-agent CAPABILITY matrix — it does not name skills or count
	/// them, and its output is identical for an empty project and a full one.
	/// For per-skill link state use `doctor --verify-links`.
	Coverage,
	/// Diagnose installed skills: source, on-disk master, and lock health
	/// (read-only). Scope -g/-p/--all; default spans global + project.
	Doctor {
		/// Verify each selected agent's skill referrer against the Master.
		/// Supports one id, a comma-separated roster, or `-a all`.
		#[arg(long)]
		verify_links: bool,

		/// Exit non-zero when any issue is found, so `doctor` can gate a
		/// script or a CI step.
		///
		/// Opt-in on purpose: the default exit code is unchanged, so anyone
		/// already running `doctor` in CI is unaffected. Without it,
		/// `doctor --verify-links && echo healthy` prints healthy over a
		/// dangling referrer — the findings only ever went to stderr.
		///
		/// Counts BOTH axes: an ACTIONABLE `health` — `orphan-lock` (a lock
		/// entry with no master on disk) or `invalid-skill` (a master whose
		/// SKILL.md does not parse) — and, when `--verify-links` is given, any
		/// per-agent referrer problem.
		///
		/// Deliberately NOT every non-`ok` health. `untracked` (a master with no
		/// lock entry — a skill placed by hand) and `master-is-symlink` are
		/// supported resting states, and failing CI over them would only teach
		/// you to append `|| true`. `withheld` and `unsupported` are likewise
		/// correct — a skill deliberately not granted to that agent, or an
		/// agent that cannot hold a skill at all.
		#[arg(long)]
		fail_on_issues: bool,
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
	/// Show how a source's current content differs from your installed skills.
	///
	/// ALWAYS fetches over the network — there is no offline mode, unlike
	/// `check`, which is offline by default. Private repos read GIT_PASSWORD
	/// (any host) or GITHUB_TOKEN (github.com https-only) from the
	/// environment. In a sandbox with no egress this fails; that is expected.
	Diff {
		/// Repo the skills came from: `owner/repo`, an https git URL, or a
		/// source id from `source list`
		source: String,
		/// Branch/tag/commit to compare against (defaults to each skill's
		/// locked ref)
		#[arg(long = "ref", alias = "git-ref")]
		git_ref: Option<String>,
		/// Accepted and ignored: `diff` always goes to the network.
		///
		/// Here only because `check --online` exists, so a caller reasonably
		/// tries the same flag here and used to get a clap exit 2 whose "to
		/// pass '--online' as a value, use '-- --online'" tip reads like a
		/// quoting problem.
		#[arg(long, hide = true)]
		online: bool,
	},
	/// Install skills from a git repo, and refresh ones already installed.
	///
	/// This is also the INSTALL entry point — there is no separate `source
	/// add`. Neither --install-missing nor --update means "report only": both
	/// preview unless --yes is passed, and --yes with NEITHER is refused.
	/// Private repos read GIT_PASSWORD (any host) or GITHUB_TOKEN (github.com
	/// only) from the environment. Runnable examples are at the end of this
	/// help.
	#[command(visible_alias = "install")]
	// The examples live in `after_long_help`, NOT in the doc comment above:
	// clap re-wraps a `///` paragraph, which joined the two example lines into
	// ONE unrunnable command (`… --yes aghub-cli -p source sync …`). It was the
	// only worked example in the whole CLI, and it sat on the install entry
	// point. `after_long_help` is emitted verbatim.
	#[command(after_long_help = SYNC_EXAMPLES)]
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
		/// DEPRECATED — no-op. `source sync` is always symlink-only now (a
		/// single `.aghub/<name>` master plus per-agent links). Accepted so
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

/// Resource arg for the commands that ONLY work on skills.
///
/// `check` and `apply-update` shared the full `ResourceType`, so clap advertised
/// `[possible values: skills, mcps]` and their long help never said otherwise —
/// then the runtime bailed with bare prose on stderr and an EMPTY stdout, even
/// under `--json`. clap's `[possible values]` is the most authoritative
/// machine-readable signal there is; an agent enumerates the surface from it and
/// built `check mcps`. Rejecting at parse time makes the error precise and
/// self-correcting.
#[derive(Copy, Clone, ValueEnum)]
enum SkillResource {
	#[value(alias = "skill")]
	Skills,
}

impl From<SkillResource> for ResourceType {
	fn from(_: SkillResource) -> Self {
		ResourceType::Skills
	}
}

/// Resource arg for the commands that ONLY work on MCP servers.
///
/// `enable`/`disable skills` was a DEAD command: `set_skill_enabled` has no
/// success branch for any of the 25 agents (deliberately — `save()` serializes
/// MCPs only, so flipping `Skill::enabled` would silently rewrite `.mcp.json`
/// and strip fields aghub does not model; core calls that "worse than an honest
/// refusal", and it is right). But clap still advertised `skills`, so the only
/// way to learn no agent supports it was to enumerate agent ids by hand.
#[derive(Copy, Clone, ValueEnum)]
enum McpResource {
	#[value(alias = "mcp")]
	Mcps,
}

impl From<McpResource> for ResourceType {
	fn from(_: McpResource) -> Self {
		ResourceType::Mcps
	}
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

/// Reject `-a all` for a command that does not fan out.
///
/// The generic path rejects it inside `handle_all_agents`. `check` and
/// `prune-lock` are now dispatched BEFORE that, so they need the same refusal
/// explicitly — without it, moving them silently turned `-a all` into a no-op,
/// which is the exact shape of defect this whole change set is removing.
fn reject_agent_all(agent: &str) -> Result<()> {
	if matches!(AgentSelection::parse(agent), Ok(AgentSelection::All)) {
		anyhow::bail!(
			"--agent all is accepted by `get`, `doctor --verify-links` and \
			 `source sync` only. This command takes a single agent (or ignores \
			 -a entirely)."
		);
	}
	Ok(())
}

/// The resource a fan-out mutation targets, normalized to [`ResourceType`].
///
/// `enable`/`disable` take the narrowed [`McpResource`] (clap rejects `skills`
/// for them at parse time, because no agent supports it), so the fan-out match
/// cannot bind one `resource` across all five commands.
fn fanout_resource(command: &Commands) -> Option<ResourceType> {
	match command {
		Commands::Add { resource, .. }
		| Commands::Update { resource, .. }
		| Commands::Delete { resource, .. } => Some(*resource),
		Commands::Enable { resource, .. }
		| Commands::Disable { resource, .. } => Some((*resource).into()),
		_ => None,
	}
}

/// Routes the `log` crate to stderr.
///
/// Nothing in this workspace installed a logger outside its own tests, so every
/// `log::warn!` in `aghub-core` / `aghub-skill` / `aghub-git` went to the
/// no-op logger and vanished. That silence had teeth: both lock read paths
/// (`skill::lock::io` and `skill::lock::local`) fail OPEN on an unparseable
/// lock and announce it *only* through `log::warn!`, so a corrupt
/// `skills-lock.json` read as "no skills installed" with nothing on either
/// stream to contradict it.
///
/// Warnings and errors always print; `-v` opens it up to info/debug, matching
/// what `eprintln_verbose!` already does.
struct StderrLogger;

impl log::Log for StderrLogger {
	fn enabled(&self, metadata: &log::Metadata) -> bool {
		metadata.level() <= log::max_level()
	}

	fn log(&self, record: &log::Record) {
		if !self.enabled(record.metadata()) {
			return;
		}
		// Same `# ` prefix as `eprintln_verbose!` for the chatty levels, so a
		// caller can filter aghub's own commentary out of stderr; warnings and
		// errors are labelled instead, because they are not commentary.
		match record.level() {
			log::Level::Error => eprintln!("error: {}", record.args()),
			log::Level::Warn => eprintln!("warning: {}", record.args()),
			_ => eprintln!("# {}", record.args()),
		}
	}

	fn flush(&self) {}
}

static STDERR_LOGGER: StderrLogger = StderrLogger;

fn main() -> std::process::ExitCode {
	// clap handles its OWN usage errors (exit 2) inside `parse()`; everything
	// below is a runtime failure, which is exit 1.
	let cli = Cli::parse();
	let json = cli.json;
	match run(cli) {
		Ok(()) => std::process::ExitCode::SUCCESS,
		Err(error) => {
			report_failure(&error, json);
			std::process::ExitCode::FAILURE
		}
	}
}

/// Print a failure. Under `--json` it goes to STDOUT as JSON, matching where
/// the success payload goes.
///
/// A caller in `--json` mode used to get nothing on stdout and one line of
/// English on stderr, and every runtime failure — a policy refusal
/// (`apply-update` without `--yes`), a missing resource, an invalid agent id, a
/// rejected scope combination, a genuine failed write — was exit 1. The only
/// way to tell them apart was matching the prose, which is not stable and is
/// not even consistent: the same "resource is missing" condition reads
/// `Skill 'x' not found` from `describe` and `Resource not found: skill 'x'`
/// from `disable`.
///
/// `code` comes from `aghub_core::error_codes`, the same vocabulary the HTTP
/// API sends, and `retryable` answers the one question an automating caller
/// actually has to decide. The prose stays on stderr either way, for humans and
/// for anything already scraping it.
fn report_failure(error: &anyhow::Error, json: bool) {
	if json && !ANSWER_ON_STDOUT.load(Ordering::Relaxed) {
		// `anyhow` erases the type, so recover the `ConfigError` when it is in
		// the chain — that is where the shared code vocabulary applies. A
		// CLI-authored `bail!` has no ConfigError and is reported as
		// `CLI_ERROR`: still machine-readable, still exit 1, and honest about
		// having no finer classification.
		let (code, retryable) = match error.downcast_ref::<ConfigError>() {
			Some(config_error) => (
				aghub_core::error_codes::wire_code(config_error),
				aghub_core::error_codes::retryable(config_error),
			),
			None => ("CLI_ERROR", false),
		};
		let payload = serde_json::json!({
			"error": {
				"code": code,
				// `{:#}` walks the whole anyhow chain. `to_string()` returns
				// only the outermost context, so a wrapped failure read as
				// "Failed to load config" with the actual cause (which file,
				// which parse error, which line) stranded in the `Caused by:`
				// block that only stderr gets.
				"message": format!("{error:#}"),
				"retryable": retryable,
			}
		});
		// A serialization failure here must not swallow the real error, so fall
		// back to prose rather than unwrapping.
		match serde_json::to_string_pretty(&payload) {
			Ok(text) => println!("{text}"),
			Err(_) => eprintln!("Error: {error:?}"),
		}
	}
	eprintln!("Error: {error:?}");
}

fn run(cli: Cli) -> Result<()> {
	// Set global verbose flag
	set_verbose(cli.verbose);

	// Ignore the SetLoggerError: a logger already installed by something else
	// is fine, and failing to log must never fail the command.
	let _ = log::set_logger(&STDERR_LOGGER);
	log::set_max_level(if cli.verbose {
		log::LevelFilter::Debug
	} else {
		log::LevelFilter::Warn
	});

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

	// Validate the agent flag ONCE, here, before every early dispatch below.
	//
	// The full parse further down cannot move: `AgentSelection::All` routes into
	// `handle_all_agents`, which only the generic commands want. But VALIDITY is
	// universal, and it used to be checked only after eight early returns — so
	// `-a bogus` exited 1 on `get`/`check`/`prune-lock`/`delete` and exited 0,
	// silently ignoring the typo, on `coverage`/`doctor`/`source list`/
	// `skill-usage`. `doctor` and `doctor --verify-links` — the SAME
	// subcommand — disagreed about the same bad id. That left no command an
	// agent could use to check an id it had composed: a cheap read-only probe
	// said `bogus` was fine, and the wall came later, mid-write.
	//
	// Commands that ignore the agent flag keep ignoring it; only an invalid id
	// changes behaviour, and it now fails the same way everywhere.
	AgentSelection::parse(&cli.agent)
		.map_err(|e| anyhow::anyhow!("invalid --agent: {e}"))?;

	// `source` operates on installed skills / git sources, not on a single
	// agent's config. Dispatch it BEFORE the `-a all` special-case and the
	// adapter/ConfigManager setup so it never fails on a missing agent config.
	if let Commands::Source { action } = &cli.command {
		let resolved = resolve_cli_scope(&cli)?;
		return commands::source::execute(
			action, &resolved, &cli.agent, cli.json,
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
		let resolved = resolve_cli_scope(&cli)?;
		return commands::apply_update::execute(
			(*resource).into(),
			name.clone(),
			resolved.resource_scope(),
			resolved.project_root(),
			*yes,
			cli.json,
		);
	}

	// Inference inventory is not agent-scoped (it's the shared provider store +
	// keyring). Dispatch it before the adapter/ConfigManager setup too.
	if let Commands::Inference { action } = &cli.command {
		return commands::inference::execute(action, cli.json);
	}

	// `plugin` manages Claude Code's plugin store, not per-scope agent config —
	// root `--help` says outright that it IGNORES the scope flags. It therefore
	// must not go through `resolve_scope_and_root`: once the project-root guard
	// became unconditional (so read paths stop answering `[]` outside a
	// project), the generic path started failing `-p plugin list` with "no
	// project root found" for a command that never wanted a scope. Claude-only
	// is still enforced, here, where the message can name the fix.
	if let Commands::Plugin { action } = &cli.command {
		let agents = AgentSelection::parse(&cli.agent)
			.map_err(|e| anyhow::anyhow!("invalid --agent: {e}"))?;
		let claude_only = matches!(
			&agents,
			AgentSelection::List(list)
				if list.as_slice() == [AgentType::Claude]
		);
		if !claude_only {
			anyhow::bail!(
				"Plugin management is only supported for Claude Code. Use \
				 -a claude"
			);
		}
		return commands::plugin::execute(action.clone(), cli.json);
	}

	// `transfer` / `reconcile` span MULTIPLE agents (source + targets), so they
	// resolve their own per-target scope and are dispatched before the
	// single-agent adapter/ConfigManager setup. They take a single writing
	// scope (-g/-p); the top-level `--all` has no meaning here and is rejected
	// by `TRANSFER_SCOPE` in the policy table (mirrors `coverage`).
	if let Commands::Transfer { action } = &cli.command {
		let resolved = resolve_cli_scope(&cli)?;
		return commands::transfer::execute_transfer(
			action, &resolved, cli.json,
		);
	}
	if let Commands::Reconcile { action } = &cli.command {
		let resolved = resolve_cli_scope(&cli)?;
		return commands::transfer::execute_reconcile(
			action, &resolved, cli.json,
		);
	}

	// `coverage` classifies EVERY registered agent against the per-scope master,
	// so it is not single-agent scoped; dispatch it before the adapter setup.
	if let Commands::Coverage = &cli.command {
		let resolved = resolve_cli_scope(&cli)?;
		return commands::coverage::execute(&resolved, cli.json);
	}

	// `doctor` reconciles the skill lock against the on-disk master across
	// scopes; not single-agent scoped, so dispatch before adapter setup.
	if let Commands::Doctor {
		verify_links,
		fail_on_issues,
	} = &cli.command
	{
		let resolved = resolve_cli_scope(&cli)?;
		return commands::doctor::execute_with_options(
			&resolved,
			cli.json,
			*verify_links,
			&cli.agent,
			*fail_on_issues,
		);
	}

	// `skill-usage` reads Claude's global `skillUsage` counter — it is
	// Claude-global, not single-agent scoped, so dispatch before adapter setup.
	if let Commands::SkillUsage = &cli.command {
		// The resolved scope is passed in even though `skill-usage` has only
		// one: a dispatch that merely CALLED the resolver for its rejections
		// and dropped the result could be deleted without a compile error, so
		// the one command whose scope is pure validation was also the one that
		// could silently skip the policy table.
		let resolved = resolve_cli_scope(&cli)?;
		return commands::skill_usage::execute(&resolved, cli.json);
	}

	// `check` and `prune-lock` answer from the SKILL LOCK alone — they never
	// read or write an agent's config. Dispatched here, before the
	// adapter/ConfigManager setup, for the same reason the commands above are:
	// a malformed agent config must not block a command that never needed one.
	// Tightening `tolerate_missing` to only absorb NotFound (so a corrupt
	// config stops reading as an empty one) otherwise made a broken
	// `.mcp.json` fail `check skills` and `prune-lock`, which is unrelated to
	// either.
	if let Commands::Check {
		resource,
		online,
		write_result,
	} = &cli.command
	{
		reject_agent_all(&cli.agent)?;
		// `check` defaults to BOTH scopes, like the other read-only
		// diagnostics (`doctor`, `source list`, `source diff`). It used to
		// follow the plain global default and silently answered from the global
		// lock alone: run inside a project, it reported that the project's
		// skills needed no update — because it never looked at them. Measured
		// in this repo, 22 rows went missing.
		//
		// An explicit `-g` / `-p` / `--all` still means exactly what it says;
		// only the no-flag default moves. With no project root the default
		// degrades to global-only rather than failing — the unconditional
		// project-root guard fires for `ProjectOnly`, which an implicit default
		// never produces.
		let resolved = resolve_cli_scope(&cli)?;
		return check::execute(
			(*resource).into(),
			resolved.resource_scope(),
			resolved.project_root(),
			*online,
			cli.json,
			write_result.clone(),
		);
	}
	if let Commands::Repair { name, yes } = &cli.command {
		reject_agent_all(&cli.agent)?;
		let resolved = resolve_cli_scope(&cli)?;
		return repair::execute(
			resolved.resource_scope(),
			resolved.project_root(),
			name.as_deref(),
			!*yes,
			cli.json,
		);
	}
	if let Commands::PruneLock { dry_run, yes } = &cli.command {
		reject_agent_all(&cli.agent)?;
		let resolved = resolve_cli_scope(&cli)?;
		return prune::execute(
			resolved.resource_scope(),
			resolved.project_root(),
			*dry_run || !*yes,
			cli.json,
		);
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
			// A payload that says `success: false` must not exit 0. Today that is
			// `RemovalKind::Partial`: the removal RAN and at least one path could
			// not be deleted, so the resource is wholly or partly still there.
			// `delete --yes` on a read-only directory exited 0 with the skill
			// untouched. The report above IS the answer, so suppress the failure
			// renderer's second document.
			if payload.get("success").and_then(serde_json::Value::as_bool)
				== Some(false)
			{
				note_answer_on_stdout();
				anyhow::bail!(
					"the removal did not complete: some paths could not be \
					 deleted — see the report above"
				);
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
			format!(
				"enabled {} '{name}'\n",
				ResourceType::from(*resource).singular()
			)
		}
		Commands::Disable { resource, .. } => {
			format!(
				"disabled {} '{name}'\n",
				ResourceType::from(*resource).singular()
			)
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
	// `kept` is terminal: the master is shared and an executing call REFUSES.
	// Checked before the preview branch, which would otherwise print "re-run
	// with --yes to remove" — and `--yes` then fails with
	// `Unsupported operation`. That is the never-terminating hint
	// `RemovalKind::Kept` was introduced to eliminate; the JSON and the three
	// desktop consumers were fixed and the CLI's own human output was not.
	if payload.get("outcome").and_then(|v| v.as_str()) == Some("kept") {
		return format!(
			// Deliberately does NOT name a directory. The Master moved to the
			// `.aghub` store, and this string still said `.agents/skills` —
			// sending anyone who followed it to look in the wrong place. The
			// path is not needed to act on the message anyway; the two
			// remedies are.
			"{} '{name}' was NOT removed: this agent still reads it from a \
			 master shared with other agents. Delete it for all agents \
			 (--all-agents), or remove it from the other agents sharing that \
			 master first.\n",
			resource.singular()
		);
	}
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

	// `partial` is terminal in the other direction from `kept`: the removal RAN
	// and at least one path could not be deleted. Falling through would print
	// two lies at once — with every path failing, `paths` is empty, so the
	// branch below says "removed … : no installed files to remove", and then
	// every path that FAILED gets listed under "kept (shared with other
	// agents)", which is a different thing entirely.
	if payload.get("outcome").and_then(|v| v.as_str()) == Some("partial") {
		let mut out = format!(
			"{} '{name}' was only PARTIALLY removed — see the warnings above \
			 for why each path could not be deleted.\n",
			resource.singular()
		);
		if !paths.is_empty() {
			out.push_str("removed:\n");
			for p in &paths {
				out.push_str(&format!("  {p}\n"));
			}
		}
		if !skipped.is_empty() {
			out.push_str("still there:\n");
			for p in &skipped {
				out.push_str(&format!("  {p}\n"));
			}
		}
		return out;
	}

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
			// The path is printed immediately above, so naming a directory
			// here was redundant AND wrong once the store moved to `.aghub`.
			"note: the Master listed above is NOT removed. `source sync` \
			 refuses to overwrite an existing Master, so delete it by hand \
			 before reinstalling this skill from git.\n",
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

// ──────────────────────────── scope resolution ─────────────────────────────
//
// ONE table (`scope_policy`), ONE resolver (`resolve_scope`), ONE resolved
// value (`Scope`). Command modules receive the `Scope`, never the three
// booleans, so there is nothing left for them to re-derive — which is what
// used to grow a private resolver, and its own project-root bail, inside
// `source`, `coverage` and `transfer`.

/// The scope flags exactly as parsed. Only [`resolve_scope`] reads them.
#[derive(Clone, Copy, Default, Debug)]
struct ScopeFlags {
	global: bool,
	project: bool,
	all: bool,
}

impl From<&Cli> for ScopeFlags {
	fn from(cli: &Cli) -> Self {
		Self {
			global: cli.global,
			project: cli.project,
			all: cli.all,
		}
	}
}

/// The one project-root failure. It used to be copied, in five different
/// wordings, into every module that resolved a scope of its own.
const NO_PROJECT_ROOT: &str =
	"no project root found from the current directory; run this inside a \
	 project (a directory with an agent config marker, e.g. .claude/, \
	 .mcp.json, or skills-lock.json) or pass -g/--global";

/// What one subcommand accepts as a scope.
///
/// Each rejection carries its own verbatim message, so a per-command wording
/// survives WITHOUT a per-command resolver — that trade is the whole reason
/// the private resolvers existed.
#[derive(Clone, Copy, PartialEq, Eq)]
struct ScopePolicy {
	/// Rejection message for `--all`, or `None` when both scopes are allowed.
	reject_all: Option<&'static str>,
	/// Rejection message for `-p`, or `None` when the project scope is fine.
	reject_project: Option<&'static str>,
	/// Rejection message for "no scope flag at all", or `None` to default.
	require_explicit: Option<&'static str>,
	/// With no flag, span BOTH scopes (when a project root exists) instead of
	/// global-only.
	default_both: bool,
	/// Let a `-p` with NO project root through as `ProjectOnly` + no root
	/// instead of failing with [`NO_PROJECT_ROOT`].
	///
	/// True ONLY for `transfer`/`reconcile`, which never resolved the root in
	/// the CLI at all: they hand the scope to core, whose source lookup fails
	/// with a typed `ResourceNotFound` — so `--json` reports
	/// `code: RESOURCE_NOT_FOUND`, not the untyped `CLI_ERROR` an early bail
	/// here would produce. Bailing early reads better but silently rewrites a
	/// machine-readable error code that the HTTP API shares.
	rootless_project_passthrough: bool,
}

/// Read-only, any scope; no flag means global.
const READ_ANY_SCOPE: ScopePolicy = ScopePolicy {
	reject_all: None,
	reject_project: None,
	require_explicit: None,
	default_both: false,
	rootless_project_passthrough: false,
};

/// Read-only diagnostic: no flag means global PLUS the current project.
/// `doctor`, `source list`, `source diff` and `check` all share this — `check`
/// following the plain global default meant "this project is up to date" was
/// answered without ever reading the project lock.
const READ_BOTH_BY_DEFAULT: ScopePolicy = ScopePolicy {
	reject_all: None,
	reject_project: None,
	require_explicit: None,
	default_both: true,
	rootless_project_passthrough: false,
};

/// Generic single-agent CRUD mutation: exactly one write target, so a read
/// scope can never disagree with the config that is mutated.
const SINGLE_WRITE_SCOPE: ScopePolicy = ScopePolicy {
	reject_all: Some(
		"generic mutation does not support --all; pass -g/--global or \
		 -p/--project",
	),
	reject_project: None,
	require_explicit: None,
	default_both: false,
	rootless_project_passthrough: false,
};

const TRANSFER_SCOPE: ScopePolicy = ScopePolicy {
	reject_all: Some(
		"transfer/reconcile support only 'global' or 'project' scope, not \
		 'all'; pass -g/--global or -p/--project",
	),
	reject_project: None,
	require_explicit: None,
	default_both: false,
	// The ONE policy that does not bail on a rootless `-p`; see the field's
	// doc. Flipping this to `false` changes `--json`'s `error.code` from
	// `RESOURCE_NOT_FOUND` to `CLI_ERROR`, which
	// `rootless_project_transfer_keeps_resource_not_found_code` pins.
	rootless_project_passthrough: true,
};

const COVERAGE_SCOPE: ScopePolicy = ScopePolicy {
	reject_all: Some(
		"coverage supports only 'global' or 'project' scope, not 'all'; pass \
		 -g/--global or -p/--project",
	),
	reject_project: None,
	require_explicit: None,
	default_both: false,
	rootless_project_passthrough: false,
};

const SOURCE_SYNC_SCOPE: ScopePolicy = ScopePolicy {
	reject_all: Some(
		"`source sync` needs exactly one scope; --all is not allowed",
	),
	reject_project: None,
	require_explicit: Some(
		"`source sync` needs a scope: pass -g (global) or -p (project)",
	),
	default_both: false,
	rootless_project_passthrough: false,
};

/// `apply-update` reads the lock, but it REWRITES the skill on disk, and core
/// (`LockedResyncError::UnsupportedScope`) has always refused `Both`. Saying so
/// here, with core's own sentence verbatim, is what makes "every rejection runs
/// before the cwd is touched" true for it too: the refusal used to arrive after
/// a project-root lookup and a lock read.
const APPLY_UPDATE_SCOPE: ScopePolicy = ScopePolicy {
	reject_all: Some("apply-update requires --global or --project, not --all"),
	reject_project: None,
	require_explicit: None,
	default_both: false,
	rootless_project_passthrough: false,
};

const ACCEPT_RENAME_SCOPE: ScopePolicy = ScopePolicy {
	reject_all: Some(
		"`source accept-rename` needs exactly one scope; --all is not allowed",
	),
	reject_project: None,
	require_explicit: None,
	default_both: false,
	rootless_project_passthrough: false,
};

const CLAUDE_GLOBAL_ONLY_SCOPE: ScopePolicy = ScopePolicy {
	reject_all: Some(
		"skill-usage is Claude-global only; it does not accept -p/--project \
		 or --all",
	),
	reject_project: Some(
		"skill-usage is Claude-global only; it does not accept -p/--project \
		 or --all",
	),
	require_explicit: None,
	default_both: false,
	rootless_project_passthrough: false,
};

/// THE scope policy table.
///
/// Exhaustive on purpose. The old table ended in `_ => AllowBoth` and relied
/// on a comment ("Any new MUTATING subcommand must be added to the SingleWrite
/// arm above, or it silently bypasses the project-root guard") to keep itself
/// correct; now a new subcommand does not COMPILE until it is classified here.
/// The compiler forces a classification, not a correct one — but silence is no
/// longer an option.
///
/// `None` means the command ignores the scope flags entirely and must NOT go
/// through the resolver: `inference` and `plugin` manage a shared store, not
/// per-scope agent config, and `-p plugin list` would otherwise fail with "no
/// project root found" for a command that never wanted a scope.
fn scope_policy(command: &Commands) -> Option<ScopePolicy> {
	Some(match command {
		Commands::Add { .. }
		| Commands::Update { .. }
		| Commands::Delete { .. }
		| Commands::Enable { .. }
		| Commands::Disable { .. }
		| Commands::Repair { .. } => SINGLE_WRITE_SCOPE,
		Commands::Get { .. }
		| Commands::Describe { .. }
		| Commands::PruneLock { .. } => READ_ANY_SCOPE,
		Commands::ApplyUpdate { .. } => APPLY_UPDATE_SCOPE,
		Commands::Check { .. } | Commands::Doctor { .. } => {
			READ_BOTH_BY_DEFAULT
		}
		Commands::Coverage => COVERAGE_SCOPE,
		Commands::Transfer { .. } | Commands::Reconcile { .. } => {
			TRANSFER_SCOPE
		}
		Commands::SkillUsage => CLAUDE_GLOBAL_ONLY_SCOPE,
		// Nested, and exhaustive too: a new `source` action must be classified
		// here or it does not compile either.
		Commands::Source { action } => match action {
			SourceAction::List | SourceAction::Diff { .. } => {
				READ_BOTH_BY_DEFAULT
			}
			SourceAction::Sync { .. } => SOURCE_SYNC_SCOPE,
			SourceAction::AcceptRename { .. } => ACCEPT_RENAME_SCOPE,
		},
		Commands::Inference { .. } | Commands::Plugin { .. } => return None,
	})
}

/// The seal around [`Scope`]'s fields.
///
/// Rust privacy is per-DEFINING-module and reaches every DESCENDANT, so a
/// `Scope` declared in the crate root has "private" fields that
/// `commands::source` can still write: a command module could forge
/// `Scope { scope: ProjectOnly, project_root: None }` and skip the policy
/// table entirely — the exact state `write_target` calls unreachable. Inside
/// its own module the fields are reachable only here, so `resolve_scope` is
/// the only way to obtain one.
mod scope {
	use super::*;

	/// A resolved, validated scope. Its fields are private TO THIS MODULE and
	/// only [`resolve_scope`] builds one, so a command module holding a
	/// `Scope` has nothing left to derive.
	#[derive(Clone, Debug, PartialEq, Eq)]
	pub struct Scope {
		scope: ResourceScope,
		project_root: Option<PathBuf>,
	}

	impl Scope {
		pub fn resource_scope(&self) -> ResourceScope {
			self.scope
		}

		pub fn project_root(&self) -> Option<&std::path::Path> {
			self.project_root.as_deref()
		}

		/// The `scope` string the JSON payloads carry.
		pub fn label(&self) -> &'static str {
			match self.scope {
				ResourceScope::GlobalOnly => "global",
				ResourceScope::ProjectOnly => "project",
				ResourceScope::Both => "both",
			}
		}

		/// Which config file the SINGLE-AGENT path writes. `Both` writes the
		/// GLOBAL config when a project root exists (`with_scope(global = true)`
		/// sets `write_scope = GlobalOnly`) — surprising, but it is verbatim the
		/// answer the hand-rolled `use_global_config` ladder in `run_for_agent`
		/// gave from the raw flags, and `--all` is refused by every generic
		/// mutation anyway. Do NOT "fix" the arm to match a nicer sentence: it
		/// would flip which config `--all get mcps` reads.
		pub fn writes_global(&self) -> bool {
			match self.scope {
				ResourceScope::GlobalOnly => true,
				ResourceScope::ProjectOnly => false,
				ResourceScope::Both => self.project_root.is_some(),
			}
		}

		/// THE single write target for the commands that have exactly one:
		/// `Some(root)` = that project's store, `None` = the global one.
		///
		/// Fails for a scope no writing policy should ever produce. `source`'s
		/// `write_scope`, `accept-rename`'s `RenameScope` and `transfer`'s
		/// `install_scope` each used to close this same match with
		/// `_ => …::Global`, so a scope that slipped past the policy table became
		/// a silent write to the GLOBAL lock. Now it is an error, in one place.
		pub fn write_target(&self) -> Result<Option<&std::path::Path>> {
			match (self.scope, self.project_root.as_deref()) {
				(ResourceScope::GlobalOnly, _) => Ok(None),
				(ResourceScope::ProjectOnly, Some(root)) => Ok(Some(root)),
				// Unreachable through `resolve_scope` for every policy that
				// calls this: they reject `--all`, and a rootless `-p` bails.
				// (`TRANSFER_SCOPE` lets a rootless `-p` through, and for that
				// reason `transfer` maps the scope itself instead of calling
				// here.) That is exactly why it must not fall back to "global".
				(ResourceScope::ProjectOnly, None)
				| (ResourceScope::Both, _) => {
					anyhow::bail!(
					"internal: a single-write command resolved '{}' scope, which \
					 is not one write target — the scope policy table should have \
					 refused it",
					self.label()
				)
				}
			}
		}
	}

	/// Resolve the flags into a [`Scope`] under `policy`.
	///
	/// `find_root` is called ONLY when the answer depends on it: `-g` and the
	/// plain global default must not touch the cwd, because a global-only command
	/// has no business dying of a deleted cwd (`current_dir()` → ENOENT). Every
	/// rejection runs BEFORE it for the same reason — `--all source sync` must
	/// fail with the scope error, not "No such file or directory".
	pub fn resolve_scope(
		flags: ScopeFlags,
		policy: ScopePolicy,
		find_root: impl FnOnce() -> Result<Option<PathBuf>>,
	) -> Result<Scope> {
		if flags.all {
			if let Some(msg) = policy.reject_all {
				anyhow::bail!("{msg}");
			}
		}
		if flags.project {
			if let Some(msg) = policy.reject_project {
				anyhow::bail!("{msg}");
			}
		}
		if !(flags.global || flags.project || flags.all) {
			if let Some(msg) = policy.require_explicit {
				anyhow::bail!("{msg}");
			}
		}

		if flags.all {
			// Both scopes; simply no project half when there is no root.
			return Ok(Scope {
				scope: ResourceScope::Both,
				project_root: find_root()?,
			});
		}
		if flags.project {
			let project_root = find_root()?;
			// THE project-root guard: `-p` with no root fails here, before any
			// config is touched, rather than silently falling back to the global
			// write. NOT limited to mutations — gating it on writes let
			// `-p get skills --json` answer `[]` on exit 0 from a directory that
			// is not a project at all, byte-identical on all three channels to a
			// real project holding no skills.
			//
			// `transfer`/`reconcile` opt out (`rootless_project_passthrough`):
			// they never resolved a root in the CLI, so bailing here would swap
			// core's typed `RESOURCE_NOT_FOUND` for an untyped `CLI_ERROR`.
			if project_root.is_none() && !policy.rootless_project_passthrough {
				anyhow::bail!("{NO_PROJECT_ROOT}");
			}
			return Ok(Scope {
				scope: ResourceScope::ProjectOnly,
				project_root,
			});
		}
		if flags.global || !policy.default_both {
			return Ok(Scope {
				scope: ResourceScope::GlobalOnly,
				project_root: None,
			});
		}

		// No flag on a diagnostic that defaults to both scopes. With no project
		// root it degrades to global-only rather than failing: an implicit default
		// never asked for `ProjectOnly`, so the guard above must not fire.
		let project_root = find_root()?;
		let scope = if project_root.is_some() {
			ResourceScope::Both
		} else {
			ResourceScope::GlobalOnly
		};
		Ok(Scope {
			scope,
			project_root,
		})
	}
}

use scope::{resolve_scope, Scope};

/// [`resolve_scope`] against the real cwd.
fn resolve_scope_and_root(cli: &Cli, policy: ScopePolicy) -> Result<Scope> {
	resolve_scope(ScopeFlags::from(cli), policy, || {
		Ok(find_project_root(&std::env::current_dir()?))
	})
}

/// [`resolve_scope_and_root`] with the policy this command is classified under.
///
/// Panics for `inference`/`plugin`, which the table marks as scope-free and
/// which are dispatched before any scope is resolved.
fn resolve_cli_scope(cli: &Cli) -> Result<Scope> {
	let Some(policy) = scope_policy(&cli.command) else {
		unreachable!(
			"`inference` and `plugin` ignore the scope flags and are \
			 dispatched before any scope resolution"
		)
	};
	resolve_scope_and_root(cli, policy)
}

/// One batch row's outcome from `run_for_agent`'s payload.
///
/// A payload that says `success: false` is a FAILED row. The batch envelope
/// only knows what this closure tells it, and it used to be told that any
/// `Ok(_)` was a success — so `delete skills foo -a claude,cursor` where every
/// path failed with EACCES reported "2 succeeded, 0 failed" and exited 0, with
/// the skill still on disk for both. The single-agent path reads the same key;
/// this is the fan-out half of the same rule.
fn row_from_payload(
	payload: Option<serde_json::Value>,
) -> Result<serde_json::Value, String> {
	let payload = payload.unwrap_or(serde_json::Value::Null);
	if payload.get("success").and_then(serde_json::Value::as_bool)
		!= Some(false)
	{
		return Ok(payload);
	}
	let still_there: Vec<&str> = payload
		.get("skipped")
		.and_then(|v| v.as_array())
		.map(|a| a.iter().filter_map(|p| p.as_str()).collect())
		.unwrap_or_default();
	let detail = if still_there.is_empty() {
		String::new()
	} else {
		format!(" (still there: {})", still_there.join(", "))
	};
	Err(format!(
		"the removal did not complete: some paths could not be deleted{detail}"
	))
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
	let resolved = resolve_cli_scope(cli)?;
	let scope = resolved.resource_scope();

	eprintln_verbose!("Resource scope: {:?}", scope);
	if let Some(root) = resolved.project_root() {
		eprintln_verbose!("Project root: {}", root.display());
	}

	// Create adapter and manager with scope
	let adapter = create_adapter(agent_type);
	let mut manager = ConfigManager::with_scope(
		adapter,
		resolved.writes_global(),
		resolved.project_root(),
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
			//
			// The error KIND decides, not just the command: this used to match on
			// the command alone, so a config that EXISTS but does not parse was
			// tolerated exactly like an absent one. `delete --yes` then took
			// `config().is_none()` as "already gone" and reported
			// `{success:true, executed:false}` on exit 0 while the entry stayed
			// in the file — a silent failed removal. A malformed config is a
			// hard error for every command (`get` already reported it correctly).
			let missing = match &e {
				ConfigError::NotFound { .. } => true,
				ConfigError::Io(io) => {
					io.kind() == std::io::ErrorKind::NotFound
				}
				_ => false,
			};
			let tolerate_missing = missing
				&& matches!(
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
				// `anyhow!("… {}", e)` STRINGIFIES the ConfigError, so
				// `report_failure`'s downcast finds nothing and the shared
				// code degrades to `CLI_ERROR` — a malformed config reported
				// itself as an unclassified CLI error instead of
				// `JSON_PARSE_ERROR`. `Error::from(..).context(..)` keeps the
				// typed error in the chain, where the downcast can reach it.
				return Err(
					anyhow::Error::from(e).context("Failed to load config")
				);
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
			disable::execute(&mut manager, resource.into(), name).map(Some)
		}
		Commands::Enable { resource, name } => {
			enable::execute(&mut manager, resource.into(), name).map(Some)
		}
		Commands::Describe { resource, name } => {
			describe::execute(&manager, resource, name, cli.json).map(|()| None)
		}
		Commands::Check { .. } => {
			unreachable!("`check` is dispatched before agent-config setup")
		}
		Commands::ApplyUpdate { .. } => {
			unreachable!(
				"`apply-update` is dispatched before agent-config setup"
			)
		}
		Commands::PruneLock { .. } => {
			unreachable!("`prune-lock` is dispatched before agent-config setup")
		}
		Commands::Repair { .. } => {
			unreachable!("`repair` is dispatched before agent-config setup")
		}
		Commands::Plugin { .. } => {
			unreachable!("`plugin` is dispatched before agent-config setup")
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
			// Must match the `-a` long help verbatim. It used to say
			// "supports only 'get'", which contradicted both the help (`all`
			// also works with `doctor --verify-links` and `source sync`) and
			// the behaviour — and its suggested remedy was wrong too: a
			// comma-separated list is REJECTED by check, describe, coverage,
			// prune-lock and apply-update.
			return Err(anyhow::anyhow!(
				"--agent all is accepted by `get`, `doctor --verify-links` \
				 and `source sync` only. This command takes a single agent \
				 (or ignores -a entirely)."
			));
		}
	};

	let resolved = resolve_cli_scope(cli)?;
	let scope = resolved.resource_scope();
	eprintln_verbose!("Loading resources for all agents (scope: {:?})", scope);
	let resources = load_all_agents(scope, resolved.project_root());
	get::execute_all(resources, resource, cli.json)
}

// Handle a comma-separated --agent list: fan the command across the named
// agents. `get` aggregates (same JSON shape as `--agent all`); mutating
// commands map onto the SHARED core batch policy (`aghub_core::batch`):
// preflight before any write, attempt every agent, one JSON envelope on
// stdout, non-zero exit if any failed. (`source sync` is dispatched earlier
// and resolves the list itself; the top-of-main guard rejects lists on
// every other command.)
/// The transport an add/update batch is about to write, when the flags spell
/// one out. Returns `None` for commands that carry no transport and for input
/// the shared validator rejects — the batch preflight only decides whether
/// every target COULD take it; reporting bad input is `run_for_agent`'s job.
fn mcp_transport_for_preflight(
	command: &Commands,
) -> Option<aghub_core::models::McpTransport> {
	let (cmd, url, transport, headers, env_vars, timeout) = match command {
		Commands::Add {
			command,
			url,
			transport,
			headers,
			env_vars,
			timeout,
			..
		}
		| Commands::Update {
			command,
			url,
			transport,
			headers,
			env_vars,
			timeout,
			..
		} => (command, url, transport, headers, env_vars, timeout),
		_ => return None,
	};
	commands::parse_mcp_transport(
		cmd.clone(),
		url.clone(),
		transport,
		headers.clone(),
		env_vars.clone(),
		*timeout,
	)
	.ok()
	.flatten()
}

fn handle_agent_list(cli: &Cli, agents: &[AgentType]) -> Result<()> {
	match &cli.command {
		Commands::Get { resource } => {
			let resource = *resource;
			let resolved = resolve_cli_scope(cli)?;
			let mut resources = load_all_agents(
				resolved.resource_scope(),
				resolved.project_root(),
			);
			resources
				.retain(|r| agents.iter().any(|a| a.as_str() == r.agent_id));
			get::execute_all(resources, resource, cli.json)
		}
		Commands::Add { .. }
		| Commands::Update { .. }
		| Commands::Delete { .. }
		| Commands::Enable { .. }
		| Commands::Disable { .. } => {
			// Normalized here because `enable`/`disable` carry the narrowed
			// `McpResource` (clap rejects `skills` for them at parse time —
			// no agent supports it) while the other three carry the full
			// `ResourceType`. Same contract as the `unreachable!()` arms
			// below: the pattern above is what makes this total.
			let Some(resource) = fanout_resource(&cli.command) else {
				unreachable!(
					"the arm above matches exactly the commands \
					 `fanout_resource` covers"
				)
			};
			let resource = &resource;
			// Preflight judges the same write scope `run_for_agent` resolves;
			// the policy itself (which capabilities, all-before-any-write)
			// lives in core; MCPs share it with the API's /mcps/batch.
			let view = if matches!(resource, ResourceType::Mcps) {
				let write_scope = resolve_cli_scope(cli)?.resource_scope();
				let is_toggle = matches!(
					cli.command,
					Commands::Enable { .. } | Commands::Disable { .. }
				);
				// Give preflight the transport too: some dialects have a word
				// for only one remote transport and refuse the other, and a
				// refusal discovered mid-batch leaves the earlier agents
				// already written. A malformed transport stays None — its real
				// error belongs to `run_for_agent`, not to the preflight.
				let transport = mcp_transport_for_preflight(&cli.command);
				aghub_core::batch::run_mcp_agent_mutation(
					agents,
					write_scope,
					is_toggle,
					transport.as_ref(),
					|agent| {
						eprintln_verbose!(
							"Running for agent: {}",
							agent.as_str()
						);
						run_for_agent(cli, agent)
							.map_err(|e| format!("{e:#}"))
							.and_then(row_from_payload)
					},
				)
				.map_err(|e| anyhow::anyhow!("{e}"))?
			} else {
				let write_scope = resolve_cli_scope(cli)?.resource_scope();
				aghub_core::batch::run_skill_agent_mutation(
					agents,
					write_scope,
					|agent| {
						eprintln_verbose!(
							"Running for agent: {}",
							agent.as_str()
						);
						run_for_agent(cli, agent)
							.map_err(|e| format!("{e:#}"))
							// Mutating commands always yield a payload; Null keeps
							// the row well-formed if that invariant ever slips.
							.and_then(row_from_payload)
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
				// The envelope above already carries every per-agent verdict.
				note_answer_on_stdout();
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
				// A ConfigError, not an ad-hoc `anyhow` string: it carries
				// the shared `RESOURCE_NOT_FOUND` code into `--json`, and it
				// gives this the SAME wording every other command uses for the
				// same condition. `describe` said "Skill 'x' not found" while
				// `disable`/`transfer` said "Resource not found: skill 'x'", so
				// a caller matching one missed the other and read a plain
				// missing skill as an unknown fatal error.
				let skill =
					config.skills.iter().find(|s| s.name == name).ok_or_else(
						|| ConfigError::resource_not_found("skill", &name),
					)?;
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
						map.remove("shared_with");
						map.remove("already_installed");
					}
				}
				print_value(&value, json)?;
			}
			ResourceType::Mcps => {
				let mcp =
					config.mcps.iter().find(|m| m.name == name).ok_or_else(
						|| ConfigError::resource_not_found("MCP server", &name),
					)?;
				eprintln_verbose!("Found MCP server: {}", mcp.name);
				print_value(&serde_json::to_value(mcp)?, json)?;
			}
		}

		Ok(())
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::cell::Cell;

	/// Run the resolver against a fake project-root lookup, and report whether
	/// that lookup ran at all. The "did it run" half is the point: `-g`, the
	/// plain global default, and EVERY rejection must answer without touching
	/// the cwd, because a deleted cwd makes `current_dir()` fail and a
	/// global-only command has no business dying of it.
	fn resolve(
		flags: ScopeFlags,
		policy: ScopePolicy,
		root: Option<&str>,
	) -> (Result<Scope>, bool) {
		let looked_up = Cell::new(false);
		let out = resolve_scope(flags, policy, || {
			looked_up.set(true);
			Ok(root.map(PathBuf::from))
		});
		(out, looked_up.get())
	}

	const GLOBAL: ScopeFlags = ScopeFlags {
		global: true,
		project: false,
		all: false,
	};
	const PROJECT: ScopeFlags = ScopeFlags {
		global: false,
		project: true,
		all: false,
	};
	const ALL: ScopeFlags = ScopeFlags {
		global: false,
		project: false,
		all: true,
	};
	const NO_FLAG: ScopeFlags = ScopeFlags {
		global: false,
		project: false,
		all: false,
	};

	/// Every policy that accepts `-p` fails a rootless project scope with the
	/// SAME sentence. Before this table there were five wordings of it, spread
	/// over five modules.
	#[test]
	fn rootless_project_scope_bails_with_one_message_everywhere() {
		for policy in [
			READ_ANY_SCOPE,
			READ_BOTH_BY_DEFAULT,
			SINGLE_WRITE_SCOPE,
			COVERAGE_SCOPE,
			SOURCE_SYNC_SCOPE,
			ACCEPT_RENAME_SCOPE,
			APPLY_UPDATE_SCOPE,
		] {
			assert!(
				!policy.rootless_project_passthrough,
				"this list is the BAILING policies"
			);
			let (out, _) = resolve(PROJECT, policy, None);
			let err = out.expect_err("-p with no root must fail").to_string();
			assert_eq!(
				err, NO_PROJECT_ROOT,
				"every policy must fail with the one shared sentence"
			);
		}
	}

	/// `transfer`/`reconcile` are the ONE exception: a rootless `-p` resolves
	/// to `ProjectOnly` with no root and reaches core, whose source lookup
	/// fails with a typed `ResourceNotFound`. Bailing here instead would
	/// rewrite `--json`'s `error.code` to the untyped `CLI_ERROR` — the shape
	/// `rootless_project_transfer_keeps_resource_not_found_code` pins from the
	/// real binary.
	#[test]
	fn transfer_lets_a_rootless_project_scope_reach_core() {
		let (out, looked_up) = resolve(PROJECT, TRANSFER_SCOPE, None);
		let scope = out.expect("transfer must not bail on a rootless -p");
		assert!(looked_up, "it still asks for the root");
		assert_eq!(scope.resource_scope(), ResourceScope::ProjectOnly);
		assert_eq!(scope.project_root(), None);
	}

	/// A `-p` that DOES find a root resolves; nothing else may fire.
	#[test]
	fn project_scope_with_a_root_resolves() {
		let (out, looked_up) = resolve(PROJECT, READ_ANY_SCOPE, Some("/p"));
		let scope = out.unwrap();
		assert!(looked_up);
		assert_eq!(scope.resource_scope(), ResourceScope::ProjectOnly);
		assert_eq!(scope.project_root(), Some(std::path::Path::new("/p")));
		assert_eq!(scope.label(), "project");
		assert!(!scope.writes_global());
	}

	/// Every rejection answers BEFORE the cwd is touched. `--all source sync`
	/// from a deleted cwd must fail with the scope error, not with
	/// "No such file or directory".
	#[test]
	fn rejections_never_touch_the_cwd() {
		let cases: &[(ScopeFlags, ScopePolicy, &str)] = &[
			(ALL, SINGLE_WRITE_SCOPE, "does not support --all"),
			(ALL, TRANSFER_SCOPE, "not 'all'"),
			(ALL, COVERAGE_SCOPE, "not 'all'"),
			(ALL, SOURCE_SYNC_SCOPE, "--all is not allowed"),
			(ALL, APPLY_UPDATE_SCOPE, "not --all"),
			(ALL, ACCEPT_RENAME_SCOPE, "--all is not allowed"),
			(ALL, CLAUDE_GLOBAL_ONLY_SCOPE, "Claude-global only"),
			(PROJECT, CLAUDE_GLOBAL_ONLY_SCOPE, "Claude-global only"),
			(NO_FLAG, SOURCE_SYNC_SCOPE, "needs a scope"),
		];
		for (flags, policy, needle) in cases {
			let (out, looked_up) = resolve(*flags, *policy, Some("/p"));
			let err = out
				.expect_err(&format!("{flags:?} must be rejected"))
				.to_string();
			assert!(
				err.contains(needle),
				"{flags:?}: {err:?} must contain {needle:?}"
			);
			assert!(
				!looked_up,
				"{flags:?}: a rejection must not resolve a project root"
			);
		}
	}

	/// `-g` — and the plain global default — resolve without a cwd lookup.
	#[test]
	fn global_scope_never_touches_the_cwd() {
		for policy in [
			READ_ANY_SCOPE,
			READ_BOTH_BY_DEFAULT,
			SINGLE_WRITE_SCOPE,
			TRANSFER_SCOPE,
			COVERAGE_SCOPE,
			SOURCE_SYNC_SCOPE,
			ACCEPT_RENAME_SCOPE,
			CLAUDE_GLOBAL_ONLY_SCOPE,
		] {
			let (out, looked_up) = resolve(GLOBAL, policy, Some("/p"));
			let scope = out.unwrap();
			assert_eq!(scope.resource_scope(), ResourceScope::GlobalOnly);
			assert_eq!(scope.project_root(), None);
			assert!(!looked_up, "-g must not resolve a project root");
		}

		let (out, looked_up) = resolve(NO_FLAG, READ_ANY_SCOPE, Some("/p"));
		assert_eq!(out.unwrap().resource_scope(), ResourceScope::GlobalOnly);
		assert!(!looked_up, "the global default must not resolve a root");
	}

	/// The diagnostics' no-flag default spans both scopes, and degrades to
	/// global-only — never a bail — when there is no project.
	#[test]
	fn read_both_by_default_needs_a_root_and_degrades_without_one() {
		let (out, looked_up) =
			resolve(NO_FLAG, READ_BOTH_BY_DEFAULT, Some("/p"));
		let scope = out.unwrap();
		assert!(looked_up, "the both-scopes default depends on the root");
		assert_eq!(scope.resource_scope(), ResourceScope::Both);
		assert_eq!(scope.project_root(), Some(std::path::Path::new("/p")));

		let (out, _) = resolve(NO_FLAG, READ_BOTH_BY_DEFAULT, None);
		let scope = out.expect("an implicit default must never bail");
		assert_eq!(scope.resource_scope(), ResourceScope::GlobalOnly);
		assert_eq!(scope.project_root(), None);
	}

	/// `--all` spans whatever scopes exist; a missing project root is simply
	/// no project half, never a failure.
	#[test]
	fn all_scope_survives_a_missing_project_root() {
		let (out, _) = resolve(ALL, READ_ANY_SCOPE, None);
		let scope = out.expect("--all must not require a project root");
		assert_eq!(scope.resource_scope(), ResourceScope::Both);
		assert_eq!(scope.project_root(), None);
		assert_eq!(scope.label(), "both");
	}

	/// `writes_global` replaced a hand-rolled `if global {…} else if project
	/// {…} else if all {…}` ladder that re-read the raw flags inside
	/// `run_for_agent`, immediately below the resolver call. Same answers.
	#[test]
	fn writes_global_matches_the_flag_ladder_it_replaced() {
		let cases: &[(ScopeFlags, Option<&str>, bool)] = &[
			(GLOBAL, Some("/p"), true),
			(NO_FLAG, Some("/p"), true),
			(PROJECT, Some("/p"), false),
			// `--all` writes the project's config when there is one.
			(ALL, Some("/p"), true),
			(ALL, None, false),
		];
		for (flags, root, expected) in cases {
			let (out, _) = resolve(*flags, READ_ANY_SCOPE, *root);
			assert_eq!(
				out.unwrap().writes_global(),
				*expected,
				"{flags:?} with root {root:?}"
			);
		}
	}

	fn policy_for(argv: &[&str]) -> Option<ScopePolicy> {
		let cli = Cli::try_parse_from(argv)
			.unwrap_or_else(|e| panic!("{argv:?} must parse: {e}"));
		scope_policy(&cli.command)
	}

	/// One invocation per subcommand, with the policy it must resolve under.
	///
	/// `scope_policy_classifies_every_subcommand` checks the classifications;
	/// `every_subcommand_has_a_policy_case` checks that this list still names
	/// every subcommand clap knows about, so a NEW command cannot be added
	/// without landing here. Neither can prove a classification is the RIGHT
	/// one — that stays a review call.
	const POLICY_CASES: &[(&[&str], Option<ScopePolicy>)] = &[
		(
			&["aghub-cli", "add", "skills", "--name", "x"],
			Some(SINGLE_WRITE_SCOPE),
		),
		(
			&["aghub-cli", "update", "skills", "x"],
			Some(SINGLE_WRITE_SCOPE),
		),
		(
			&["aghub-cli", "delete", "skills", "x"],
			Some(SINGLE_WRITE_SCOPE),
		),
		(
			&["aghub-cli", "enable", "mcps", "x"],
			Some(SINGLE_WRITE_SCOPE),
		),
		(
			&["aghub-cli", "disable", "mcps", "x"],
			Some(SINGLE_WRITE_SCOPE),
		),
		(&["aghub-cli", "get", "skills"], Some(READ_ANY_SCOPE)),
		(
			&["aghub-cli", "describe", "skills", "x"],
			Some(READ_ANY_SCOPE),
		),
		(
			&["aghub-cli", "apply-update", "skills", "x"],
			Some(APPLY_UPDATE_SCOPE),
		),
		(&["aghub-cli", "prune-lock"], Some(READ_ANY_SCOPE)),
		// Repair resolves exactly ONE write scope: it moves directories, and
		// `--all` would leave it guessing which store to migrate into.
		(&["aghub-cli", "repair"], Some(SINGLE_WRITE_SCOPE)),
		(
			&["aghub-cli", "check", "skills"],
			Some(READ_BOTH_BY_DEFAULT),
		),
		(&["aghub-cli", "doctor"], Some(READ_BOTH_BY_DEFAULT)),
		(&["aghub-cli", "source", "list"], Some(READ_BOTH_BY_DEFAULT)),
		(
			&["aghub-cli", "source", "diff", "o/r"],
			Some(READ_BOTH_BY_DEFAULT),
		),
		(
			&["aghub-cli", "source", "sync", "o/r"],
			Some(SOURCE_SYNC_SCOPE),
		),
		(
			&["aghub-cli", "source", "accept-rename", "a", "b"],
			Some(ACCEPT_RENAME_SCOPE),
		),
		(&["aghub-cli", "coverage"], Some(COVERAGE_SCOPE)),
		(
			&[
				"aghub-cli",
				"transfer",
				"skill",
				"--from-agent",
				"claude",
				"--name",
				"x",
				"--to",
				"opencode",
			],
			Some(TRANSFER_SCOPE),
		),
		(
			&[
				"aghub-cli",
				"reconcile",
				"skill",
				"--from-agent",
				"claude",
				"--name",
				"x",
				"--add",
				"opencode",
			],
			Some(TRANSFER_SCOPE),
		),
		(
			&["aghub-cli", "skill-usage"],
			Some(CLAUDE_GLOBAL_ONLY_SCOPE),
		),
		// Scope-free: these manage a shared store, not per-scope agent
		// config, and must NOT reach the resolver — `-p plugin list` used
		// to fail with "no project root found" for a command that never
		// wanted a scope.
		(&["aghub-cli", "plugin", "list"], None),
		(&["aghub-cli", "inference", "list"], None),
	];

	/// The policy table itself. It is exhaustive over `Commands` (and over
	/// `SourceAction`), so a new subcommand does not compile until it is
	/// classified — this pins WHICH class each existing one landed in.
	#[test]
	fn scope_policy_classifies_every_subcommand() {
		for (argv, expected) in POLICY_CASES {
			assert!(
				policy_for(argv) == *expected,
				"{argv:?} is classified under the wrong scope policy"
			);
		}
	}

	/// …and the case list is complete, asked of CLAP rather than of a second
	/// hand-written list.
	///
	/// The exhaustive `match` in `scope_policy` forces a new subcommand to be
	/// CLASSIFIED, but nothing forced it to be TESTED: a command missing from
	/// `POLICY_CASES` was simply invisible to the test above, which is the one
	/// place a wrong classification would show up. Now it goes red.
	#[test]
	fn every_subcommand_has_a_policy_case() {
		use clap::CommandFactory;

		let command = Cli::command();
		let mut required: Vec<Vec<&str>> = Vec::new();
		for sub in command.get_subcommands() {
			let name = sub.get_name();
			// clap's own generated `help` subcommand is not ours.
			if name == "help" {
				continue;
			}
			// `source` is the ONLY command `scope_policy` branches on per
			// action (`SourceAction`), so it is the only one that needs a case
			// per action. `plugin`/`inference` actions all share one answer
			// (`None`).
			if name == "source" {
				required.extend(
					sub.get_subcommands()
						.map(clap::Command::get_name)
						.filter(|n| *n != "help")
						.map(|action| vec![name, action]),
				);
			} else {
				required.push(vec![name]);
			}
		}
		assert!(required.len() > 10, "clap introspection found nothing");

		for want in &required {
			assert!(
				POLICY_CASES.iter().any(|(argv, _)| {
					argv.len() > want.len() && argv[1..=want.len()] == want[..]
				}),
				"subcommand {want:?} has no case in POLICY_CASES: classify it \
				 in `scope_policy` and pin the classification here"
			);
		}
	}

	/// `write_target` is the ONE answer to "which store does this write?".
	/// `source`'s `write_scope`, `accept-rename`'s `RenameScope` and
	/// `transfer`'s `install_scope` each used to close this match with
	/// `_ => …::Global`, so a scope that got past the policy table became a
	/// silent write to the GLOBAL lock.
	///
	/// `ProjectOnly` with no root is not constructible through `resolve_scope`
	/// (the guard bails first), so only the `Both` arm is reachable here.
	#[test]
	fn write_target_refuses_a_scope_that_is_not_one_store() {
		let (out, _) = resolve(GLOBAL, READ_ANY_SCOPE, Some("/p"));
		assert_eq!(out.unwrap().write_target().unwrap(), None);

		let (out, _) = resolve(PROJECT, READ_ANY_SCOPE, Some("/p"));
		assert_eq!(
			out.unwrap().write_target().unwrap(),
			Some(std::path::Path::new("/p"))
		);

		let (out, _) = resolve(ALL, READ_ANY_SCOPE, Some("/p"));
		let err = out
			.unwrap()
			.write_target()
			.expect_err("'both' is not a single write target")
			.to_string();
		assert!(err.contains("not one write target"), "{err}");
	}

	/// `read_scopes` is the read-side counterpart, and nothing pinned it: a
	/// mutation making a rooted `-p` return NO scopes reddened one spawn test,
	/// indirectly. `source list -p` listing nothing is a silent wrong answer.
	#[test]
	fn read_scopes_spans_exactly_the_resolved_scopes() {
		use crate::commands::source::read_scopes;
		use skill_update::sources::SourceScope;

		// `SourceScope` has no `PartialEq`; describe it instead.
		fn describe(scopes: &[SourceScope]) -> Vec<String> {
			scopes
				.iter()
				.map(|s| match s {
					SourceScope::Global => "global".to_string(),
					SourceScope::Project { root } => {
						format!("project:{}", root.display())
					}
				})
				.collect()
		}
		let spans = |flags, root| {
			let (out, _) = resolve(flags, READ_BOTH_BY_DEFAULT, root);
			describe(&read_scopes(&out.unwrap()))
		};

		assert_eq!(spans(GLOBAL, Some("/p")), ["global"]);
		assert_eq!(spans(PROJECT, Some("/p")), ["project:/p"]);
		assert_eq!(spans(NO_FLAG, Some("/p")), ["global", "project:/p"]);
		// No project root: the both-scopes default degrades to global alone.
		assert_eq!(spans(NO_FLAG, None), ["global"]);
	}

	/// The generic mutations reject `--all` before anything is written. This
	/// is the rule the five-case spawn loop used to re-prove one subprocess at
	/// a time; the spawn test now keeps ONE case, for the on-disk proof that
	/// nothing leaked.
	#[test]
	fn every_generic_mutation_rejects_all_scope() {
		for argv in [
			&["aghub-cli", "--all", "add", "skills", "--name", "x"][..],
			&["aghub-cli", "--all", "update", "skills", "x"][..],
			&["aghub-cli", "--all", "delete", "skills", "x"][..],
			&["aghub-cli", "--all", "enable", "mcps", "x"][..],
			&["aghub-cli", "--all", "disable", "mcps", "x"][..],
		] {
			let policy = policy_for(argv).expect("a mutation has a policy");
			let (out, looked_up) = resolve(ALL, policy, Some("/p"));
			let err = out.expect_err("--all must be rejected").to_string();
			assert!(err.contains("does not support --all"), "{argv:?}: {err}");
			assert!(!looked_up, "{argv:?}: rejected before any cwd IO");
		}
	}

	/// `-p` with no project root fails on READ commands too. Gating the guard
	/// on mutations let `-p get skills --json` answer `[]` on exit 0 from a
	/// directory that is not a project at all.
	#[test]
	fn rootless_project_scope_fails_on_reads_too() {
		for argv in [
			&["aghub-cli", "-p", "get", "skills"][..],
			&["aghub-cli", "-p", "describe", "skills", "x"][..],
			&["aghub-cli", "-p", "check", "skills"][..],
			&["aghub-cli", "-p", "coverage"][..],
			&["aghub-cli", "-p", "source", "list"][..],
		] {
			let policy = policy_for(argv).expect("a read command has a policy");
			let (out, _) = resolve(PROJECT, policy, None);
			assert_eq!(
				out.expect_err("-p with no root must fail").to_string(),
				NO_PROJECT_ROOT,
				"{argv:?}"
			);
		}
	}
}
