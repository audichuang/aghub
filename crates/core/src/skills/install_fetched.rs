//! No-network install of an ALREADY-FETCHED skill.
//!
//! This is the shared primitive behind both the API git-install route and the
//! CLI `source sync` command: given a skill that has already been fetched into a
//! local tree, install it into the resolved per-agent skills directories — as an
//! isolated copy OR in the universal `.agents/skills` layout — and write the
//! install lock. It performs NO network and NO credential resolution; fetch +
//! auth live in the caller.
//!
//! It returns PER-AGENT results so the API can rebuild its current per-agent
//! success / invalid-agent response and the CLI can report which agents got the
//! skill. An agent whose target skills dir cannot be resolved is reported as a
//! soft failure (`installed: false`, `error: Some(..)`), NOT a hard error.

use std::path::Path;

use crate::adapters::create_adapter;
use crate::models::ResourceScope;
use crate::skills::install_layout::{
	install_universal, universal_canonical_dir,
};
use crate::skills::skill_source_root;
use crate::skills::update::{detect_rename, skill_renamed_message};
use aghub_agents::models::AgentType;
use skill::sanitize::sanitize_name;

/// Recursively copy `from` into `to`, creating `to` (and parents) as needed.
///
/// This is the VERBATIM isolated-copy semantics the API git-install route used
/// (`crates/api/src/routes/skills.rs`): a plain deep copy that copies every
/// entry with NO exclusion list (unlike [`install_layout`]'s Master copy, which
/// drops `metadata.json`/`.git`/… to match npx). Moved into core so the API and
/// the CLI share one implementation. Returns [`std::io::Result`] — core cannot
/// depend on Rocket, so the API maps the error to its own type at the boundary.
///
/// [`install_layout`]: crate::skills::install_layout
pub fn copy_dir_recursive(from: &Path, to: &Path) -> std::io::Result<()> {
	std::fs::create_dir_all(to)?;
	for entry in std::fs::read_dir(from)? {
		let entry = entry?;
		let from_path = entry.path();
		let to_path = to.join(entry.file_name());
		let file_type = entry.file_type()?;
		if file_type.is_dir() {
			copy_dir_recursive(&from_path, &to_path)?;
		} else {
			std::fs::copy(&from_path, &to_path)?;
		}
	}
	Ok(())
}

/// Layout to install the fetched skill in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SkillInstallLayout {
	/// Copy the skill into each agent's own skills dir (never touches
	/// `.agents`). No-clobber: an existing dir is left untouched.
	IsolatedCopy,
	/// Materialize the master once in the `.agents/skills` canonical dir and
	/// symlink each target agent dir to it.
	Universal,
}

/// What happened for one target agent.
#[derive(Clone, Debug)]
pub struct AgentInstallResult {
	pub agent: AgentType,
	/// `true` when this agent received the skill on this call (a fresh copy or
	/// a fresh universal link). `false` for a soft skip (already present, or no
	/// resolvable skills dir — see `error`).
	pub installed: bool,
	pub error: Option<String>,
}

/// Inputs for [`install_fetched_skill_and_lock`].
pub struct FetchedSkillInstallRequest<'a> {
	/// `SKILL.md` inside the already-fetched tree (or its parent dir).
	pub skill_file: &'a Path,
	pub source: &'a skill::InstallLockSource,
	/// npx-form lock path, e.g. `"<dir>/SKILL.md"`.
	pub lock_skill_path: String,
	/// Repo tip OID for the lock `refCommit` heal (best-effort).
	pub ref_commit: Option<String>,
	/// Install scope. Only `GlobalOnly` / `ProjectOnly` are supported.
	pub scope: ResourceScope,
	pub project_root: Option<&'a Path>,
	pub target_agents: &'a [AgentType],
	pub layout: SkillInstallLayout,
	/// Rename guard: when `Some(n)`, the fetched frontmatter name MUST equal `n`
	/// or the install is refused before any write.
	pub expected_name: Option<&'a str>,
	/// Universal link style: relative links (project) vs absolute (global).
	pub use_relative_links: bool,
}

/// Result of [`install_fetched_skill_and_lock`].
#[derive(Clone, Debug)]
pub struct FetchedSkillInstallReport {
	/// Parsed (canonical) skill name.
	pub name: String,
	pub wrote_lock: bool,
	/// Content hash of the fetched source folder.
	pub installed_hash: String,
	pub agent_results: Vec<AgentInstallResult>,
}

/// Whether the lock should be (re)written for this scope.
///
/// Mirrors the API's `should_write_install_lock`
/// (`crates/api/src/routes/skills.rs`): write when at least one agent actually
/// received the skill on this call, OR there is no existing lock entry yet.
fn should_write_install_lock(
	skill_name: &str,
	installed_any: bool,
	scope: ResourceScope,
	project_root: Option<&Path>,
) -> bool {
	installed_any || !skill_lock_contains(skill_name, scope, project_root)
}

fn skill_lock_contains(
	skill_name: &str,
	scope: ResourceScope,
	project_root: Option<&Path>,
) -> bool {
	match scope {
		ResourceScope::GlobalOnly => {
			skill::lock::global::get_skill_from_lock(skill_name).is_some()
		}
		ResourceScope::ProjectOnly => project_root.is_some_and(|root| {
			skill::lock::local::read_local_lock(Some(root))
				.skills
				.contains_key(skill_name)
		}),
		ResourceScope::Both => false,
	}
}

fn write_install_lock(
	skill_name: &str,
	scope: ResourceScope,
	project_root: Option<&Path>,
	source: &skill::InstallLockSource,
	lock_skill_path: String,
	source_dir: &Path,
	ref_commit: Option<String>,
) -> Result<(), crate::ConfigError> {
	match scope {
		ResourceScope::GlobalOnly => skill::write_global_install_lock(
			skill_name,
			source,
			Some(lock_skill_path),
			source_dir,
			ref_commit,
		)
		.map_err(crate::ConfigError::Io),
		ResourceScope::ProjectOnly => {
			let cwd = project_root.ok_or_else(|| {
				crate::ConfigError::InvalidConfig(
					"project root is required for project skill installs"
						.to_string(),
				)
			})?;
			skill::write_project_install_lock(
				skill_name,
				source,
				Some(lock_skill_path),
				source_dir,
				cwd,
				ref_commit,
			)
			.map_err(crate::ConfigError::Io)
		}
		ResourceScope::Both => Err(crate::ConfigError::InvalidConfig(
			"Combined skill scope is not supported for installs".to_string(),
		)),
	}
}

/// Install an already-fetched skill into the resolved agent dirs and write the
/// install lock. See module docs. Performs no network / credential work.
pub fn install_fetched_skill_and_lock(
	req: FetchedSkillInstallRequest<'_>,
) -> Result<FetchedSkillInstallReport, crate::ConfigError> {
	let parsed = skill::parser::parse(req.skill_file).map_err(|e| {
		crate::ConfigError::InvalidConfig(format!("Failed to parse skill: {e}"))
	})?;
	let name = parsed.name;

	// Rename guard: refuse before any write if the fetched name diverged.
	if let Some(expected) = req.expected_name {
		if let Some(found) = detect_rename(&name, expected) {
			return Err(crate::ConfigError::ValidationFailed(
				skill_renamed_message(expected, &found),
			));
		}
	}

	// Scope guard: only Global / Project installs are supported. Reject BEFORE
	// any source-root resolution / copy / link / lock work so an unsupported
	// scope can never leave a partial side effect (e.g. a written universal
	// master). The API rejects `Both` at the same point via `resource_scope`.
	if !matches!(
		req.scope,
		ResourceScope::GlobalOnly | ResourceScope::ProjectOnly
	) {
		return Err(crate::ConfigError::InvalidConfig(
			"Combined skill scope is not supported for installs".to_string(),
		));
	}

	let source_root = skill_source_root(req.skill_file);
	let safe_name = sanitize_name(&name);
	let installed_hash = skill::compute_skill_folder_hash(&source_root)
		.map_err(|e| {
			crate::ConfigError::InvalidConfig(format!(
				"Failed to hash fetched skill: {e}"
			))
		})?;

	// `agent_results` reports per-agent install success (mirrors the API's
	// per-skill `success` entries). `copied_any` is the SEPARATE lock-write
	// signal: a *fresh* write on this run. For an isolated copy every fresh copy
	// counts (API: `copied_any |= copied`); for the universal layout ONLY a
	// newly-written master counts (API: `copied_any |= wrote_master`,
	// skills.rs:2065) — an idempotent re-run where the master + links already
	// exist must NOT be treated as a fresh install, so it does not rewrite the
	// lock.
	let (agent_results, copied_any) = match req.layout {
		SkillInstallLayout::IsolatedCopy => {
			let results = install_isolated(
				&source_root,
				&safe_name,
				req.scope,
				req.project_root,
				req.target_agents,
			)?;
			let copied = results.iter().any(|r| r.installed);
			(results, copied)
		}
		SkillInstallLayout::Universal => install_universal_layout(
			&source_root,
			&safe_name,
			req.scope,
			req.project_root,
			req.target_agents,
			req.use_relative_links,
		)?,
	};

	// Gate the lock write on at least one agent actually receiving the skill on
	// THIS run, matching the API's outer `if installed { ... }` (skills.rs:2119):
	// when every target is a soft failure AND no lock entry exists yet, no lock
	// is written.
	let installed_any = agent_results.iter().any(|r| r.installed);
	let wrote_lock = installed_any
		&& should_write_install_lock(
			&name,
			copied_any,
			req.scope,
			req.project_root,
		);
	if wrote_lock {
		write_install_lock(
			&name,
			req.scope,
			req.project_root,
			req.source,
			req.lock_skill_path.clone(),
			&source_root,
			req.ref_commit.clone(),
		)?;
	}

	Ok(FetchedSkillInstallReport {
		name,
		wrote_lock,
		installed_hash,
		agent_results,
	})
}

/// Resolve the write dir for one agent, or `None` (reported as a soft skip).
fn resolve_target_dir(
	agent: AgentType,
	scope: ResourceScope,
	project_root: Option<&Path>,
) -> Option<std::path::PathBuf> {
	create_adapter(agent).target_skills_dir(project_root, scope)
}

fn install_isolated(
	source_root: &Path,
	safe_name: &str,
	scope: ResourceScope,
	project_root: Option<&Path>,
	target_agents: &[AgentType],
) -> Result<Vec<AgentInstallResult>, crate::ConfigError> {
	let mut results = Vec::with_capacity(target_agents.len());
	for &agent in target_agents {
		let Some(dir) = resolve_target_dir(agent, scope, project_root) else {
			results.push(AgentInstallResult {
				agent,
				installed: false,
				error: Some(
					"Agent does not support persistent skill creation in \
					 this scope"
						.to_string(),
				),
			});
			continue;
		};

		// No-clobber: an existing dir is a success-no-op, never overwritten.
		let dest = dir.join(safe_name);
		if dest.exists() {
			results.push(AgentInstallResult {
				agent,
				installed: false,
				error: None,
			});
			continue;
		}

		match copy_dir_recursive(source_root, &dest) {
			Ok(()) => results.push(AgentInstallResult {
				agent,
				installed: true,
				error: None,
			}),
			Err(e) => results.push(AgentInstallResult {
				agent,
				installed: false,
				error: Some(e.to_string()),
			}),
		}
	}
	Ok(results)
}

/// Returns the per-agent results plus `wrote_master` — `true` only when the
/// canonical master was NEWLY written on this run. The API computes the same
/// signal as `!canonical.exists()` BEFORE calling `install_universal`
/// (`install_git_skill_universal`, skills.rs:686) and feeds it to the lock-write
/// decision as `copied_any` (skills.rs:2065).
fn install_universal_layout(
	source_root: &Path,
	safe_name: &str,
	scope: ResourceScope,
	project_root: Option<&Path>,
	target_agents: &[AgentType],
	use_relative_links: bool,
) -> Result<(Vec<AgentInstallResult>, bool), crate::ConfigError> {
	// Universal canonical dir follows the RESOLVED scope: project → the project
	// `.agents/skills`, global → `~/.agents/skills`.
	let canonical_root = if matches!(scope, ResourceScope::ProjectOnly) {
		project_root
	} else {
		None
	};
	let Some(canonical_skills_dir) = universal_canonical_dir(canonical_root)
	else {
		// No resolvable canonical dir → every target agent is a soft failure.
		let results = target_agents
			.iter()
			.map(|&agent| AgentInstallResult {
				agent,
				installed: false,
				error: Some(
					"Cannot resolve .agents canonical directory".to_string(),
				),
			})
			.collect();
		return Ok((results, false));
	};
	let canonical = canonical_skills_dir.join(safe_name);
	// Capture the fresh-master signal BEFORE `install_universal` materializes it,
	// matching the API's `wrote_master = !canonical.exists()` (skills.rs:686).
	let wrote_master = !canonical.exists();

	// Map each agent to its resolved write dir; agents with no dir are soft
	// failures. Agents whose dir IS the canonical dir see the master directly
	// and need no link.
	let mut results = Vec::with_capacity(target_agents.len());
	let mut symlink_dirs = Vec::new();
	let mut linked_agents = Vec::new();
	for &agent in target_agents {
		match resolve_target_dir(agent, scope, project_root) {
			Some(dir) if dir == canonical_skills_dir => {
				// Agent reads the master directly.
				results.push(AgentInstallResult {
					agent,
					installed: true,
					error: None,
				});
			}
			Some(dir) => {
				symlink_dirs.push(dir);
				linked_agents.push(agent);
			}
			None => results.push(AgentInstallResult {
				agent,
				installed: false,
				error: Some(
					"Agent does not support persistent skill creation in \
					 this scope"
						.to_string(),
				),
			}),
		}
	}

	match install_universal(
		source_root,
		&canonical,
		&symlink_dirs,
		use_relative_links,
	) {
		Ok(_report) => {
			for agent in linked_agents {
				results.push(AgentInstallResult {
					agent,
					installed: true,
					error: None,
				});
			}
		}
		Err(e) => {
			let msg = e.to_string();
			for agent in linked_agents {
				results.push(AgentInstallResult {
					agent,
					installed: false,
					error: Some(msg.clone()),
				});
			}
		}
	}

	Ok((results, wrote_master))
}
