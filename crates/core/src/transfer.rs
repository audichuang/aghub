use crate::{
	create_adapter,
	errors::{ConfigError, Result},
	manager::ConfigManager,
	models::{AgentType, McpServer, Skill, SubAgent},
	registry,
};
use log::{info, warn};
use skill::sanitize::sanitize_name;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InstallScope {
	Global,
	Project,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallTarget {
	pub agent: AgentType,
	pub scope: InstallScope,
	pub project_root: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct ResourceLocator {
	pub agent: AgentType,
	pub scope: InstallScope,
	pub project_root: Option<PathBuf>,
	pub name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationAction {
	Copy,
	Delete,
}

impl std::fmt::Display for OperationAction {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			Self::Copy => write!(f, "copy"),
			Self::Delete => write!(f, "delete"),
		}
	}
}

#[derive(Debug, Clone)]
pub struct OperationResult {
	pub target: InstallTarget,
	pub action: OperationAction,
	pub success: bool,
	pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct OperationBatchResult {
	pub results: Vec<OperationResult>,
}

impl OperationBatchResult {
	pub fn success_count(&self) -> usize {
		self.results.iter().filter(|r| r.success).count()
	}

	pub fn failed_count(&self) -> usize {
		self.results.iter().filter(|r| !r.success).count()
	}
}

/// Serializable wire view of an [`OperationBatchResult`].
///
/// `OperationResult`/`InstallTarget`/`OperationAction` are deliberately NOT
/// `Serialize` (they carry filesystem paths), so this view is the SINGLE place
/// the batch wire shape is defined. Both surfaces use it: the API derives a
/// `ts-rs` DTO that mirrors it for type generation, and the CLI serializes it
/// directly — so neither hand-rolls a second mapping that could drift.
///
/// Field encoding is fixed and load-bearing (both surfaces agreed on it):
/// `scope` is lowercase, `action` is `"copy"`/`"delete"`, and
/// `project_root`/`error` are omitted when absent.
#[derive(Debug, Clone, serde::Serialize)]
pub struct OperationResultView {
	pub agent: String,
	pub scope: &'static str,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub project_root: Option<String>,
	pub action: String,
	pub success: bool,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub error: Option<String>,
}

impl From<&OperationResult> for OperationResultView {
	fn from(r: &OperationResult) -> Self {
		OperationResultView {
			agent: r.target.agent.as_str().to_string(),
			scope: match r.target.scope {
				InstallScope::Global => "global",
				InstallScope::Project => "project",
			},
			project_root: r
				.target
				.project_root
				.as_ref()
				.map(|p| p.display().to_string()),
			action: r.action.to_string(),
			success: r.success,
			error: r.error.clone(),
		}
	}
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct OperationBatchView {
	pub success_count: usize,
	pub failed_count: usize,
	pub results: Vec<OperationResultView>,
}

impl From<&OperationBatchResult> for OperationBatchView {
	fn from(batch: &OperationBatchResult) -> Self {
		OperationBatchView {
			success_count: batch.success_count(),
			failed_count: batch.failed_count(),
			results: batch.results.iter().map(Into::into).collect(),
		}
	}
}

fn build_manager(target: &InstallTarget) -> ConfigManager {
	let adapter = create_adapter(target.agent);
	match target.scope {
		InstallScope::Global => ConfigManager::new(adapter, true, None),
		InstallScope::Project => {
			ConfigManager::new(adapter, false, target.project_root.as_deref())
		}
	}
}

fn validate_target(target: &InstallTarget) -> Result<()> {
	if target.scope == InstallScope::Project && target.project_root.is_none() {
		return Err(ConfigError::InvalidConfig(
			"project_root is required for project targets".to_string(),
		));
	}
	Ok(())
}

fn mcp_supported_for_target(
	target: &InstallTarget,
	mcp: &McpServer,
) -> Result<()> {
	let adapter = create_adapter(target.agent);
	let descriptor = registry::get(target.agent);
	let supported = adapter.mcp_supports_transport(&mcp.transport);

	if supported {
		return Ok(());
	}

	Err(ConfigError::unsupported_operation(
		"copy incompatible",
		"MCP server",
		descriptor.id,
	))
}

fn load_source_mcp(source: &ResourceLocator) -> Result<McpServer> {
	let mut manager = build_manager(&InstallTarget {
		agent: source.agent,
		scope: source.scope,
		project_root: source.project_root.clone(),
	});
	manager.load()?;
	manager.get_mcp(&source.name).cloned().ok_or_else(|| {
		ConfigError::resource_not_found("MCP server", &source.name)
	})
}

fn load_source_skill(source: &ResourceLocator) -> Result<Skill> {
	let mut manager = build_manager(&InstallTarget {
		agent: source.agent,
		scope: source.scope,
		project_root: source.project_root.clone(),
	});
	manager.load()?;
	manager
		.get_skill(&source.name)
		.cloned()
		.ok_or_else(|| ConfigError::resource_not_found("skill", &source.name))
}

fn ensure_loaded(manager: &mut ConfigManager) -> Result<()> {
	match manager.load() {
		Ok(_) => Ok(()),
		Err(ConfigError::NotFound { .. }) => {
			manager.init_empty_config();
			Ok(())
		}
		Err(err) => Err(err),
	}
}

fn resolve_skill_file(path: &str) -> PathBuf {
	if let Some(stripped) = path.strip_prefix("~/") {
		if let Some(home) = dirs::home_dir() {
			home.join(stripped)
		} else {
			PathBuf::from(path)
		}
	} else {
		PathBuf::from(path)
	}
}

/// Resolve a skill's on-disk root directory WITHOUT requiring it to exist.
///
/// Prefers `canonical_path` (the real master location for a symlinked skill),
/// falls back to `source_path`. Both go through the same tilde-expansion
/// (`resolve_skill_file`). When the resolved path is a `SKILL.md` file the
/// PARENT directory is returned (the skill folder); a directory is returned
/// as-is. Returns `None` only when the skill records no path at all.
///
/// This is the single shared resolver reused by `resolve_skill_root` (which
/// adds an existence check) and the layout-aware removal planner, so the
/// "canonical FILE path → take PARENT" rule (spec) lives in exactly one place.
pub(crate) fn skill_root_unchecked(skill: &Skill) -> Option<PathBuf> {
	let path = skill
		.canonical_path
		.as_deref()
		.or(skill.source_path.as_deref())
		.map(resolve_skill_file)?;

	let is_skill_file = path
		.file_name()
		.is_some_and(|name| name == std::ffi::OsStr::new("SKILL.md"));

	Some(if is_skill_file {
		path.parent().map(Path::to_path_buf).unwrap_or(path)
	} else {
		path
	})
}

fn resolve_skill_root(skill: &Skill) -> Result<PathBuf> {
	let root = skill_root_unchecked(skill).ok_or_else(|| {
		ConfigError::InvalidConfig(format!(
			"Skill '{}' has no source path to copy from",
			skill.name
		))
	})?;

	if !root.exists() {
		return Err(ConfigError::InvalidConfig(format!(
			"Skill source path '{}' does not exist",
			root.display()
		)));
	}

	Ok(root)
}

fn copy_dir_recursive(from: &Path, to: &Path) -> Result<()> {
	fs::create_dir_all(to)?;
	for entry in fs::read_dir(from)? {
		let entry = entry?;
		let from_path = entry.path();
		let to_path = to.join(entry.file_name());
		let file_type = entry.file_type()?;
		if file_type.is_dir() {
			copy_dir_recursive(&from_path, &to_path)?;
		} else {
			fs::copy(&from_path, &to_path)?;
		}
	}
	Ok(())
}

fn skill_target_dir(target: &InstallTarget) -> Result<PathBuf> {
	let adapter = create_adapter(target.agent);
	let dir = adapter.target_skills_dir(
		target.project_root.as_deref(),
		match target.scope {
			InstallScope::Global => crate::models::ResourceScope::GlobalOnly,
			InstallScope::Project => crate::models::ResourceScope::ProjectOnly,
		},
	);

	dir.ok_or_else(|| {
		ConfigError::unsupported_operation(
			"persist",
			"skill",
			registry::get(target.agent).id,
		)
	})
}

fn group_agents_by_target_dir(
	agents: &[AgentType],
	scope: InstallScope,
	project_root: Option<&PathBuf>,
) -> HashMap<PathBuf, Vec<AgentType>> {
	let mut dir_to_agents: HashMap<PathBuf, Vec<AgentType>> = HashMap::new();
	for agent in agents {
		let target = InstallTarget {
			agent: *agent,
			scope,
			project_root: project_root.cloned(),
		};
		if let Ok(target_dir) = skill_target_dir(&target) {
			dir_to_agents.entry(target_dir).or_default().push(*agent);
		}
	}
	dir_to_agents
}

fn unique_targets(targets: Vec<InstallTarget>) -> Vec<InstallTarget> {
	let mut seen = HashSet::new();
	let mut unique = Vec::new();
	for target in targets {
		let key = format!(
			"{}|{:?}|{}",
			target.agent.as_str(),
			target.scope,
			target
				.project_root
				.as_ref()
				.map(|path| path.display().to_string())
				.unwrap_or_default()
		);
		if seen.insert(key) {
			unique.push(target);
		}
	}
	unique
}

/// Reject a transfer that names no destinations. An empty `--to` is almost
/// always a mistake; without this guard `transfer_*` returns `Ok([])` and the
/// caller exits 0 having copied nothing (finding #4). Both surfaces route
/// through `transfer_*`, so the guard lives here once.
fn ensure_destinations(destinations: &[InstallTarget]) -> Result<()> {
	if destinations.is_empty() {
		return Err(ConfigError::InvalidConfig(
			"no destination agents given; specify at least one target"
				.to_string(),
		));
	}
	Ok(())
}

/// Reject a reconcile that names the same agent in both `--add` and `--remove`.
/// The add loop runs before the remove loop, so `--add X --remove X` would
/// silently net to a delete and exit 0. Both surfaces (CLI + API) route through
/// `reconcile_*`, so the guard lives here once.
fn ensure_disjoint(added: &[AgentType], removed: &[AgentType]) -> Result<()> {
	for agent in added {
		if removed.contains(agent) {
			return Err(ConfigError::InvalidConfig(format!(
				"agent '{}' appears in both add and remove",
				agent.as_str()
			)));
		}
	}
	Ok(())
}

fn log_operation_outcome(
	resource: &str,
	name: &str,
	action: OperationAction,
	target: &InstallTarget,
	outcome: &Result<()>,
) {
	let target_agent = registry::get(target.agent).id;
	let target_scope = match target.scope {
		InstallScope::Global => "global",
		InstallScope::Project => "project",
	};
	match outcome {
		Ok(()) => info!(
			"{} {} '{}' for agent '{}' in {} scope succeeded",
			action, resource, name, target_agent, target_scope
		),
		Err(error) => warn!(
			"{} {} '{}' for agent '{}' in {} scope failed: {}",
			action, resource, name, target_agent, target_scope, error
		),
	}
}

pub fn transfer_mcp(
	source: ResourceLocator,
	destinations: Vec<InstallTarget>,
) -> Result<OperationBatchResult> {
	let mcp = load_source_mcp(&source)?;
	let destinations = unique_targets(destinations);
	ensure_destinations(&destinations)?;
	info!(
		"transferring MCP '{}' to {} destination(s)",
		mcp.name,
		destinations.len()
	);
	let mut results = Vec::new();

	for target in destinations {
		let outcome = (|| -> Result<()> {
			validate_target(&target)?;
			mcp_supported_for_target(&target, &mcp)?;
			let mut manager = build_manager(&target);
			ensure_loaded(&mut manager)?;
			manager.add_mcp(mcp.clone())
		})();
		log_operation_outcome(
			"MCP",
			&mcp.name,
			OperationAction::Copy,
			&target,
			&outcome,
		);

		results.push(OperationResult {
			target,
			action: OperationAction::Copy,
			success: outcome.is_ok(),
			error: outcome.err().map(|err| err.to_string()),
		});
	}

	Ok(OperationBatchResult { results })
}

pub fn reconcile_mcp(
	source: ResourceLocator,
	added: Vec<AgentType>,
	removed: Vec<AgentType>,
) -> Result<OperationBatchResult> {
	ensure_disjoint(&added, &removed)?;
	let mcp = load_source_mcp(&source)?;
	info!(
		"reconciling MCP '{}' with {} added and {} removed agent(s)",
		mcp.name,
		added.len(),
		removed.len()
	);
	let mut results = Vec::new();

	let target_scope = source.scope;
	let target_project_root = source.project_root.clone();

	for agent in added {
		let target = InstallTarget {
			agent,
			scope: target_scope,
			project_root: target_project_root.clone(),
		};
		let outcome = (|| -> Result<()> {
			validate_target(&target)?;
			mcp_supported_for_target(&target, &mcp)?;
			let mut manager = build_manager(&target);
			ensure_loaded(&mut manager)?;
			manager.add_mcp(mcp.clone())
		})();
		log_operation_outcome(
			"MCP",
			&mcp.name,
			OperationAction::Copy,
			&target,
			&outcome,
		);

		results.push(OperationResult {
			target,
			action: OperationAction::Copy,
			success: outcome.is_ok(),
			error: outcome.err().map(|err| err.to_string()),
		});
	}

	for agent in removed {
		let target = InstallTarget {
			agent,
			scope: target_scope,
			project_root: target_project_root.clone(),
		};
		let outcome = (|| -> Result<()> {
			validate_target(&target)?;
			let mut manager = build_manager(&target);
			ensure_loaded(&mut manager)?;
			manager.remove_mcp(&source.name)
		})();
		log_operation_outcome(
			"MCP",
			&source.name,
			OperationAction::Delete,
			&target,
			&outcome,
		);

		results.push(OperationResult {
			target,
			action: OperationAction::Delete,
			success: outcome.is_ok(),
			error: outcome.err().map(|err| err.to_string()),
		});
	}

	Ok(OperationBatchResult { results })
}

fn load_source_sub_agent(source: &ResourceLocator) -> Result<SubAgent> {
	let mut manager = build_manager(&InstallTarget {
		agent: source.agent,
		scope: source.scope,
		project_root: source.project_root.clone(),
	});
	manager.load()?;
	manager.get_sub_agent(&source.name).cloned().ok_or_else(|| {
		ConfigError::resource_not_found("sub-agent", &source.name)
	})
}

pub fn transfer_sub_agent(
	source: ResourceLocator,
	destinations: Vec<InstallTarget>,
) -> Result<OperationBatchResult> {
	let sub_agent = load_source_sub_agent(&source)?;
	let destinations = unique_targets(destinations);
	ensure_destinations(&destinations)?;
	info!(
		"transferring sub-agent '{}' to {} destination(s)",
		sub_agent.name,
		destinations.len()
	);
	let mut results = Vec::new();

	for target in destinations {
		let outcome = (|| -> Result<()> {
			validate_target(&target)?;
			let descriptor = registry::get(target.agent);
			let scope = match target.scope {
				InstallScope::Global => {
					crate::models::ResourceScope::GlobalOnly
				}
				InstallScope::Project => {
					crate::models::ResourceScope::ProjectOnly
				}
			};
			if !descriptor.supports_sub_agent_scope(scope) {
				return Err(ConfigError::unsupported_operation(
					"copy",
					"sub-agent",
					descriptor.id,
				));
			}
			let mut manager = build_manager(&target);
			ensure_loaded(&mut manager)?;
			manager.add_sub_agent(sub_agent.clone())
		})();
		log_operation_outcome(
			"sub-agent",
			&sub_agent.name,
			OperationAction::Copy,
			&target,
			&outcome,
		);

		results.push(OperationResult {
			target,
			action: OperationAction::Copy,
			success: outcome.is_ok(),
			error: outcome.err().map(|err| err.to_string()),
		});
	}

	Ok(OperationBatchResult { results })
}

pub fn reconcile_sub_agent(
	source: ResourceLocator,
	added: Vec<AgentType>,
	removed: Vec<AgentType>,
) -> Result<OperationBatchResult> {
	ensure_disjoint(&added, &removed)?;
	let sub_agent = load_source_sub_agent(&source)?;
	info!(
		"reconciling sub-agent '{}' with {} added and {} removed agent(s)",
		sub_agent.name,
		added.len(),
		removed.len()
	);
	let mut results = Vec::new();

	let target_scope = source.scope;
	let target_project_root = source.project_root.clone();

	for agent in added {
		let target = InstallTarget {
			agent,
			scope: target_scope,
			project_root: target_project_root.clone(),
		};
		let outcome = (|| -> Result<()> {
			validate_target(&target)?;
			let descriptor = registry::get(target.agent);
			let scope = match target.scope {
				InstallScope::Global => {
					crate::models::ResourceScope::GlobalOnly
				}
				InstallScope::Project => {
					crate::models::ResourceScope::ProjectOnly
				}
			};
			if !descriptor.supports_sub_agent_scope(scope) {
				return Err(ConfigError::unsupported_operation(
					"copy",
					"sub-agent",
					descriptor.id,
				));
			}
			let mut manager = build_manager(&target);
			ensure_loaded(&mut manager)?;
			manager.add_sub_agent(sub_agent.clone())
		})();
		log_operation_outcome(
			"sub-agent",
			&sub_agent.name,
			OperationAction::Copy,
			&target,
			&outcome,
		);

		results.push(OperationResult {
			target,
			action: OperationAction::Copy,
			success: outcome.is_ok(),
			error: outcome.err().map(|err| err.to_string()),
		});
	}

	for agent in removed {
		let target = InstallTarget {
			agent,
			scope: target_scope,
			project_root: target_project_root.clone(),
		};
		let outcome = (|| -> Result<()> {
			validate_target(&target)?;
			let mut manager = build_manager(&target);
			ensure_loaded(&mut manager)?;
			manager.remove_sub_agent(&source.name)
		})();
		log_operation_outcome(
			"sub-agent",
			&source.name,
			OperationAction::Delete,
			&target,
			&outcome,
		);

		results.push(OperationResult {
			target,
			action: OperationAction::Delete,
			success: outcome.is_ok(),
			error: outcome.err().map(|err| err.to_string()),
		});
	}

	Ok(OperationBatchResult { results })
}

pub fn transfer_skill(
	source: ResourceLocator,
	destinations: Vec<InstallTarget>,
) -> Result<OperationBatchResult> {
	let skill = load_source_skill(&source)?;
	let source_root = resolve_skill_root(&skill)?;
	let safe_name = sanitize_name(&skill.name);
	let destinations = unique_targets(destinations);
	ensure_destinations(&destinations)?;
	info!(
		"transferring skill '{}' from '{}' to {} destination(s)",
		skill.name,
		source_root.display(),
		destinations.len()
	);
	let mut results = Vec::new();

	for target in destinations {
		let outcome = (|| -> Result<()> {
			validate_target(&target)?;
			let target_dir = skill_target_dir(&target)?;
			let mut manager = build_manager(&target);
			ensure_loaded(&mut manager)?;
			if manager.get_skill(&skill.name).is_some() {
				return Err(ConfigError::resource_exists("skill", &skill.name));
			}

			let dest_root = target_dir.join(&safe_name);
			if dest_root.exists() {
				return Err(ConfigError::resource_exists("skill", &skill.name));
			}

			copy_dir_recursive(&source_root, &dest_root)
		})();
		log_operation_outcome(
			"skill",
			&skill.name,
			OperationAction::Copy,
			&target,
			&outcome,
		);

		results.push(OperationResult {
			target,
			action: OperationAction::Copy,
			success: outcome.is_ok(),
			error: outcome.err().map(|err| err.to_string()),
		});
	}

	Ok(OperationBatchResult { results })
}

pub fn reconcile_skill(
	source: ResourceLocator,
	added: Vec<AgentType>,
	removed: Vec<AgentType>,
) -> Result<OperationBatchResult> {
	ensure_disjoint(&added, &removed)?;
	let skill = load_source_skill(&source)?;
	let source_root = resolve_skill_root(&skill)?;
	let safe_name = sanitize_name(&skill.name);
	info!(
		"reconciling skill '{}' with {} added and {} removed agent(s)",
		skill.name,
		added.len(),
		removed.len()
	);
	let mut results = Vec::new();

	let target_scope = source.scope;
	let target_project_root = source.project_root.clone();

	// Group agents by target directory to avoid redundant copies
	let dir_to_agents = group_agents_by_target_dir(
		&added,
		target_scope,
		target_project_root.as_ref(),
	);

	// Process each unique directory
	for (target_dir, agents) in dir_to_agents {
		let dest_root = target_dir.join(&safe_name);
		let already_exists = dest_root.exists();

		// Copy once per directory (if doesn't exist)
		if !already_exists {
			if let Err(e) = copy_dir_recursive(&source_root, &dest_root) {
				// If copy fails, all agents in this group fail
				for agent in agents {
					results.push(OperationResult {
						target: InstallTarget {
							agent,
							scope: target_scope,
							project_root: target_project_root.clone(),
						},
						action: OperationAction::Copy,
						success: false,
						error: Some(e.to_string()),
					});
				}
				continue;
			}
		}

		// All agents in this group succeed (skill is auto-discovered from dir)
		for agent in agents {
			results.push(OperationResult {
				target: InstallTarget {
					agent,
					scope: target_scope,
					project_root: target_project_root.clone(),
				},
				action: OperationAction::Copy,
				success: true,
				error: None,
			});
		}
	}

	// Remove per agent through the planned-removal seam (#5) — the same
	// classifier every delete surface uses (symlink sweep, shared-master
	// referrer keep, containment, lock prune). Never blind-delete paths
	// found via READ dirs: a NativeReader's read dirs include the shared
	// `.agents/skills` master, and `remove_dir_all`-ing it would orphan
	// every other agent's referrer.
	for agent in removed {
		let target = InstallTarget {
			agent,
			scope: target_scope,
			project_root: target_project_root.clone(),
		};
		let outcome = (|| -> Result<()> {
			validate_target(&target)?;
			let mut manager = build_manager(&target);
			ensure_loaded(&mut manager)?;
			match manager.remove_skill_planned(
				&skill.name,
				false, // single-agent removal, never an all-agents sweep
				false, // not a dry-run — the CLI/desktop gate confirmation
				true,  // execute; the plan still keeps shared masters
			) {
				Ok(_) => Ok(()),
				// Already absent from this agent = the desired state;
				// keep the old NotFound tolerance.
				Err(ConfigError::ResourceNotFound { .. }) => Ok(()),
				Err(err) => Err(err),
			}
		})();
		log_operation_outcome(
			"skill",
			&skill.name,
			OperationAction::Delete,
			&target,
			&outcome,
		);

		results.push(OperationResult {
			target,
			action: OperationAction::Delete,
			success: outcome.is_ok(),
			error: outcome.err().map(|err| err.to_string()),
		});
	}

	Ok(OperationBatchResult { results })
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::models::McpTransport;
	use std::sync::{Mutex, OnceLock};
	use tempfile::tempdir;

	fn env_lock() -> &'static Mutex<()> {
		static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
		LOCK.get_or_init(|| Mutex::new(()))
	}

	#[test]
	fn transfer_mcp_copies_to_other_agent_project() {
		let _guard = env_lock().lock().unwrap();
		let temp = tempdir().unwrap();
		let source_root = temp.path().join("source");
		let dest_root = temp.path().join("dest");
		fs::create_dir_all(&source_root).unwrap();
		fs::create_dir_all(&dest_root).unwrap();

		let mut source_manager = ConfigManager::new(
			create_adapter(AgentType::Claude),
			false,
			Some(&source_root),
		);
		source_manager.load().unwrap();
		source_manager
			.add_mcp(McpServer::new(
				"filesystem",
				McpTransport::stdio("npx", vec!["mcp-filesystem".to_string()]),
			))
			.unwrap();

		let result = transfer_mcp(
			ResourceLocator {
				agent: AgentType::Claude,
				scope: InstallScope::Project,
				project_root: Some(source_root.clone()),
				name: "filesystem".to_string(),
			},
			vec![InstallTarget {
				agent: AgentType::Cursor,
				scope: InstallScope::Project,
				project_root: Some(dest_root.clone()),
			}],
		)
		.unwrap();

		assert_eq!(result.success_count(), 1);

		let mut dest_manager = ConfigManager::new(
			create_adapter(AgentType::Cursor),
			false,
			Some(&dest_root),
		);
		dest_manager.load().unwrap();
		assert!(dest_manager.get_mcp("filesystem").is_some());
	}

	#[test]
	fn transfer_mcp_empty_destinations_is_rejected() {
		// Finding #4: a transfer with no destinations is a no-op the caller
		// almost certainly did not intend. It must be an actionable error, not
		// a silent `Ok` with an empty result set (which exits 0).
		let _guard = env_lock().lock().unwrap();
		let temp = tempdir().unwrap();
		let source_root = temp.path().join("source");
		fs::create_dir_all(&source_root).unwrap();

		let mut source_manager = ConfigManager::new(
			create_adapter(AgentType::Claude),
			false,
			Some(&source_root),
		);
		source_manager.load().unwrap();
		source_manager
			.add_mcp(McpServer::new(
				"filesystem",
				McpTransport::stdio("npx", vec!["mcp-filesystem".to_string()]),
			))
			.unwrap();

		let result = transfer_mcp(
			ResourceLocator {
				agent: AgentType::Claude,
				scope: InstallScope::Project,
				project_root: Some(source_root.clone()),
				name: "filesystem".to_string(),
			},
			vec![], // no destinations
		);

		assert!(
			result.is_err(),
			"empty destination list must be a hard error, not Ok([])"
		);
	}

	#[test]
	fn reconcile_mcp_deletes_when_removed() {
		let _guard = env_lock().lock().unwrap();
		let temp = tempdir().unwrap();
		let root = temp.path().join("project");
		fs::create_dir_all(&root).unwrap();

		let mut manager = ConfigManager::new(
			create_adapter(AgentType::Claude),
			false,
			Some(&root),
		);
		manager.load().unwrap();
		manager
			.add_mcp(McpServer::new(
				"filesystem",
				McpTransport::stdio("npx", vec!["mcp-filesystem".to_string()]),
			))
			.unwrap();

		let result = reconcile_mcp(
			ResourceLocator {
				agent: AgentType::Claude,
				scope: InstallScope::Project,
				project_root: Some(root.clone()),
				name: "filesystem".to_string(),
			},
			vec![],                  // added
			vec![AgentType::Claude], // removed
		)
		.unwrap();

		assert_eq!(result.results.len(), 1);
		assert_eq!(result.results[0].action, OperationAction::Delete);

		let mut manager = ConfigManager::new(
			create_adapter(AgentType::Claude),
			false,
			Some(&root),
		);
		manager.load().unwrap();
		assert!(manager.get_mcp("filesystem").is_none());
	}

	#[test]
	fn transfer_skill_copies_whole_folder() {
		let _guard = env_lock().lock().unwrap();
		let temp = tempdir().unwrap();
		let source_root = temp.path().join("source");
		let dest_root = temp.path().join("dest");
		fs::create_dir_all(&source_root).unwrap();
		fs::create_dir_all(&dest_root).unwrap();

		let mut source_manager = ConfigManager::new(
			create_adapter(AgentType::Claude),
			false,
			Some(&source_root),
		);
		source_manager.load().unwrap();
		let mut skill = Skill::new("repo-helper");
		skill.description = Some("Copies files".to_string());
		source_manager.add_skill(skill).unwrap();
		let asset_dir = source_root.join(".claude/skills/repo-helper/assets");
		fs::create_dir_all(&asset_dir).unwrap();
		fs::write(asset_dir.join("notes.txt"), "hello").unwrap();

		let result = transfer_skill(
			ResourceLocator {
				agent: AgentType::Claude,
				scope: InstallScope::Project,
				project_root: Some(source_root.clone()),
				name: "repo-helper".to_string(),
			},
			vec![InstallTarget {
				agent: AgentType::Cursor,
				scope: InstallScope::Project,
				project_root: Some(dest_root.clone()),
			}],
		)
		.unwrap();

		assert_eq!(result.success_count(), 1);
		assert!(dest_root
			.join(".cursor/skills/repo-helper/assets/notes.txt")
			.exists());
	}

	#[test]
	fn skill_root_unchecked_returns_nonexistent_dir_as_is() {
		let temp = tempdir().unwrap();
		let missing = temp.path().join(".agents/skills/foo");
		let mut skill = Skill::new("foo");
		skill.canonical_path = Some(missing.to_string_lossy().to_string());

		assert_eq!(skill_root_unchecked(&skill), Some(missing));
	}

	#[test]
	fn reconcile_skill_deletes_when_removed() {
		let _guard = env_lock().lock().unwrap();
		let temp = tempdir().unwrap();
		let root = temp.path().join("project");
		fs::create_dir_all(&root).unwrap();

		let mut manager = ConfigManager::new(
			create_adapter(AgentType::Claude),
			false,
			Some(&root),
		);
		manager.load().unwrap();
		let mut skill = Skill::new("repo-helper");
		skill.description = Some("Copies files".to_string());
		manager.add_skill(skill).unwrap();

		let result = reconcile_skill(
			ResourceLocator {
				agent: AgentType::Claude,
				scope: InstallScope::Project,
				project_root: Some(root.clone()),
				name: "repo-helper".to_string(),
			},
			vec![],                  // added
			vec![AgentType::Claude], // removed
		)
		.unwrap();

		assert_eq!(result.results.len(), 1);
		assert_eq!(result.results[0].action, OperationAction::Delete);

		let mut manager = ConfigManager::new(
			create_adapter(AgentType::Claude),
			false,
			Some(&root),
		);
		manager.load().unwrap();
		assert!(manager.get_skill("repo-helper").is_none());
	}

	#[cfg(unix)]
	// Smoke test only — the real data-loss guard is the Windows junction test below.
	#[test]
	fn reconcile_skill_unlinks_symlink_referrer_keeps_master() {
		use crate::adapter::set_skills_path_override;

		struct SkillsPathOverrideReset;

		impl Drop for SkillsPathOverrideReset {
			fn drop(&mut self) {
				set_skills_path_override("claude", None);
			}
		}

		let _guard = env_lock().lock().unwrap();
		let temp = tempdir().unwrap();
		let root = temp.path();
		let master = root.join(".agents/skills/my-skill");
		let claude_skills = root.join(".claude/skills");
		let referrer = claude_skills.join("my-skill");
		let skill_md =
			"---\nname: my-skill\ndescription: Shared\n---\n\n# My Skill\n";

		fs::create_dir_all(&master).unwrap();
		fs::write(master.join("SKILL.md"), skill_md).unwrap();
		fs::create_dir_all(&claude_skills).unwrap();
		std::os::unix::fs::symlink(&master, &referrer).unwrap();
		set_skills_path_override("claude", Some(claude_skills));
		let _reset_override = SkillsPathOverrideReset;

		let mut manager = ConfigManager::new(
			create_adapter(AgentType::Claude),
			false,
			Some(root),
		);
		manager.load().unwrap();
		assert!(manager.get_skill("my-skill").is_some());

		let result = reconcile_skill(
			ResourceLocator {
				agent: AgentType::Claude,
				scope: InstallScope::Project,
				project_root: Some(root.to_path_buf()),
				name: "my-skill".to_string(),
			},
			vec![],
			vec![AgentType::Claude],
		)
		.unwrap();

		assert_eq!(result.results.len(), 1);
		assert_eq!(result.results[0].action, OperationAction::Delete);
		assert!(std::fs::symlink_metadata(&referrer).is_err());
		let master_skill = master.join("SKILL.md");
		assert!(master_skill.exists());
		assert_eq!(fs::read_to_string(master_skill).unwrap(), skill_md);
	}

	// T-RECONCILE-NATIVE-READER: reconcile --remove for a NativeReader agent
	// (cursor reads `.agents/skills` directly) must NOT delete the shared
	// Master another agent still symlinks. The pre-seam code found the Master
	// via cursor's READ dirs and `remove_dir_all`'d it — data loss for every
	// referrer. This test fails if the removal path stops going through
	// `remove_skill_planned`'s classifier.
	#[cfg(unix)]
	#[test]
	fn reconcile_skill_remove_native_reader_keeps_shared_master() {
		use crate::adapter::set_skills_path_override;

		struct SkillsPathOverrideReset;

		impl Drop for SkillsPathOverrideReset {
			fn drop(&mut self) {
				set_skills_path_override("claude", None);
			}
		}

		let _guard = env_lock().lock().unwrap();
		let temp = tempdir().unwrap();
		let root = temp.path();
		let master = root.join(".agents/skills/my-skill");
		let sentinel = master.join("sentinel.txt");
		let claude_skills = root.join(".claude/skills");
		let referrer = claude_skills.join("my-skill");
		let skill_md =
			"---\nname: my-skill\ndescription: Shared\n---\n\n# My Skill\n";

		fs::create_dir_all(&master).unwrap();
		fs::write(master.join("SKILL.md"), skill_md).unwrap();
		fs::write(&sentinel, "keep-me").unwrap();
		fs::create_dir_all(&claude_skills).unwrap();
		std::os::unix::fs::symlink(&master, &referrer).unwrap();
		set_skills_path_override("claude", Some(claude_skills));
		let _reset_override = SkillsPathOverrideReset;

		let result = reconcile_skill(
			ResourceLocator {
				agent: AgentType::Claude,
				scope: InstallScope::Project,
				project_root: Some(root.to_path_buf()),
				name: "my-skill".to_string(),
			},
			vec![],
			vec![AgentType::Cursor],
		)
		.unwrap();

		assert_eq!(result.results.len(), 1);
		assert_eq!(result.results[0].action, OperationAction::Delete);
		// The shared Master and its contents must survive.
		assert!(
			master.join("SKILL.md").exists(),
			"Master SKILL.md must survive a NativeReader remove"
		);
		assert!(
			sentinel.exists(),
			"sentinel inside master must survive (remove_dir_all would \
			 have wiped it)"
		);
		// Claude's referrer must still resolve to the live Master.
		assert!(
			fs::canonicalize(&referrer).is_ok(),
			"claude referrer symlink must stay intact"
		);
	}

	// T-RECONCILE-WIN-JUNCTION: the real data-loss guard.
	// remove_dir_all on a Windows JUNCTION follows the reparse point into the
	// shared Master and deletes its contents.  This test would FAIL if the fix
	// reverted to remove_dir_all.  The unix test above is a smoke test only.
	#[cfg(windows)]
	#[test]
	fn reconcile_skill_junction_referrer_removed_master_survives() {
		use crate::adapter::set_skills_path_override;
		use crate::skills::linker::create_junction;

		struct SkillsPathOverrideReset;

		impl Drop for SkillsPathOverrideReset {
			fn drop(&mut self) {
				set_skills_path_override("claude", None);
			}
		}

		let _guard = env_lock().lock().unwrap();
		let temp = tempdir().unwrap();
		let root = temp.path();
		let master = root.join(".agents/skills/my-skill");
		let sentinel = master.join("sentinel.txt");
		let claude_skills = root.join(".claude/skills");
		let referrer = claude_skills.join("my-skill");
		let skill_md =
			"---\nname: my-skill\ndescription: Shared\n---\n\n# My Skill\n";

		fs::create_dir_all(&master).unwrap();
		fs::write(master.join("SKILL.md"), skill_md).unwrap();
		fs::write(&sentinel, "keep-me").unwrap();
		fs::create_dir_all(&claude_skills).unwrap();

		// Build a Windows JUNCTION: referrer -> master.
		let abs_master = master.canonicalize().unwrap();
		create_junction(&abs_master, &referrer).unwrap();

		set_skills_path_override("claude", Some(claude_skills));
		let _reset_override = SkillsPathOverrideReset;

		let mut manager = ConfigManager::new(
			create_adapter(AgentType::Claude),
			false,
			Some(root),
		);
		manager.load().unwrap();
		assert!(manager.get_skill("my-skill").is_some());

		let result = reconcile_skill(
			ResourceLocator {
				agent: AgentType::Claude,
				scope: InstallScope::Project,
				project_root: Some(root.to_path_buf()),
				name: "my-skill".to_string(),
			},
			vec![],
			vec![AgentType::Claude],
		)
		.unwrap();

		assert_eq!(result.results.len(), 1);
		assert_eq!(result.results[0].action, OperationAction::Delete);
		// The junction referrer must be gone.
		assert!(
			std::fs::symlink_metadata(&referrer).is_err(),
			"junction referrer must be removed"
		);
		// The shared Master directory and its contents must survive.
		assert!(
			master.join("SKILL.md").exists(),
			"Master SKILL.md must survive"
		);
		assert!(
			sentinel.exists(),
			"sentinel file inside master must survive (remove_dir_all \
			 would have wiped it)"
		);
	}

	#[test]
	fn transfer_sub_agent_copies_to_other_agent_project() {
		let _guard = env_lock().lock().unwrap();
		let temp = tempdir().unwrap();
		let source_root = temp.path().join("source");
		let dest_root = temp.path().join("dest");
		fs::create_dir_all(&source_root).unwrap();
		fs::create_dir_all(&dest_root).unwrap();

		let mut source_manager = ConfigManager::new(
			create_adapter(AgentType::Claude),
			false,
			Some(&source_root),
		);
		source_manager.load().unwrap();
		let mut sub_agent = SubAgent::new("coder");
		sub_agent.description = Some("Expert coder".to_string());
		sub_agent.instruction =
			Some("You are an expert programmer.".to_string());
		source_manager.add_sub_agent(sub_agent).unwrap();

		let result = transfer_sub_agent(
			ResourceLocator {
				agent: AgentType::Claude,
				scope: InstallScope::Project,
				project_root: Some(source_root.clone()),
				name: "coder".to_string(),
			},
			vec![InstallTarget {
				agent: AgentType::OpenCode,
				scope: InstallScope::Project,
				project_root: Some(dest_root.clone()),
			}],
		)
		.unwrap();

		assert_eq!(result.success_count(), 1);

		let mut dest_manager = ConfigManager::new(
			create_adapter(AgentType::OpenCode),
			false,
			Some(&dest_root),
		);
		dest_manager.load().unwrap();
		assert!(dest_manager.get_sub_agent("coder").is_some());
	}

	#[test]
	fn reconcile_sub_agent_adds_and_removes() {
		let _guard = env_lock().lock().unwrap();
		let temp = tempdir().unwrap();
		let root = temp.path().join("project");
		fs::create_dir_all(&root).unwrap();

		let mut manager = ConfigManager::new(
			create_adapter(AgentType::Claude),
			false,
			Some(&root),
		);
		manager.load().unwrap();
		let mut sub_agent = SubAgent::new("coder");
		sub_agent.description = Some("Expert coder".to_string());
		sub_agent.instruction =
			Some("You are an expert programmer.".to_string());
		manager.add_sub_agent(sub_agent).unwrap();

		let result = reconcile_sub_agent(
			ResourceLocator {
				agent: AgentType::Claude,
				scope: InstallScope::Project,
				project_root: Some(root.clone()),
				name: "coder".to_string(),
			},
			vec![AgentType::OpenCode], // added
			vec![AgentType::Claude],   // removed
		)
		.unwrap();

		assert_eq!(result.results.len(), 2);
		assert_eq!(result.results[0].action, OperationAction::Copy);
		assert_eq!(result.results[0].target.agent, AgentType::OpenCode);
		assert_eq!(result.results[1].action, OperationAction::Delete);
		assert_eq!(result.results[1].target.agent, AgentType::Claude);
		assert!(result.results.iter().all(|r| r.success));
	}

	#[test]
	fn transfer_mcp_to_multiple_targets() {
		let _guard = env_lock().lock().unwrap();
		let temp = tempdir().unwrap();
		let source_root = temp.path().join("source");
		let dest_root_cursor = temp.path().join("dest_cursor");
		let dest_root_windsurf = temp.path().join("dest_windsurf");
		fs::create_dir_all(&source_root).unwrap();
		fs::create_dir_all(&dest_root_cursor).unwrap();
		fs::create_dir_all(&dest_root_windsurf).unwrap();

		let mut source_manager = ConfigManager::new(
			create_adapter(AgentType::Claude),
			false,
			Some(&source_root),
		);
		source_manager.load().unwrap();
		source_manager
			.add_mcp(McpServer::new(
				"filesystem",
				McpTransport::stdio("npx", vec!["mcp-filesystem".to_string()]),
			))
			.unwrap();

		let result = transfer_mcp(
			ResourceLocator {
				agent: AgentType::Claude,
				scope: InstallScope::Project,
				project_root: Some(source_root.clone()),
				name: "filesystem".to_string(),
			},
			vec![
				InstallTarget {
					agent: AgentType::Cursor,
					scope: InstallScope::Project,
					project_root: Some(dest_root_cursor.clone()),
				},
				InstallTarget {
					agent: AgentType::Windsurf,
					scope: InstallScope::Project,
					project_root: Some(dest_root_windsurf.clone()),
				},
			],
		)
		.unwrap();

		assert_eq!(result.success_count(), 2);

		let mut cursor_manager = ConfigManager::new(
			create_adapter(AgentType::Cursor),
			false,
			Some(&dest_root_cursor),
		);
		cursor_manager.load().unwrap();
		assert!(cursor_manager.get_mcp("filesystem").is_some());

		let mut windsurf_manager = ConfigManager::new(
			create_adapter(AgentType::Windsurf),
			false,
			Some(&dest_root_windsurf),
		);
		windsurf_manager.load().unwrap();
		assert!(windsurf_manager.get_mcp("filesystem").is_some());
	}

	#[test]
	fn transfer_skill_to_multiple_targets() {
		let _guard = env_lock().lock().unwrap();
		let temp = tempdir().unwrap();
		let source_root = temp.path().join("source");
		let dest_root_cursor = temp.path().join("dest_cursor");
		let dest_root_windsurf = temp.path().join("dest_windsurf");
		fs::create_dir_all(&source_root).unwrap();
		fs::create_dir_all(&dest_root_cursor).unwrap();
		fs::create_dir_all(&dest_root_windsurf).unwrap();

		let mut source_manager = ConfigManager::new(
			create_adapter(AgentType::Claude),
			false,
			Some(&source_root),
		);
		source_manager.load().unwrap();
		let mut skill = Skill::new("repo-helper");
		skill.description = Some("Copies files".to_string());
		source_manager.add_skill(skill).unwrap();

		let result = transfer_skill(
			ResourceLocator {
				agent: AgentType::Claude,
				scope: InstallScope::Project,
				project_root: Some(source_root.clone()),
				name: "repo-helper".to_string(),
			},
			vec![
				InstallTarget {
					agent: AgentType::Cursor,
					scope: InstallScope::Project,
					project_root: Some(dest_root_cursor.clone()),
				},
				InstallTarget {
					agent: AgentType::Windsurf,
					scope: InstallScope::Project,
					project_root: Some(dest_root_windsurf.clone()),
				},
			],
		)
		.unwrap();

		assert_eq!(result.success_count(), 2);
		assert!(dest_root_cursor.join(".cursor/skills/repo-helper").exists());
		assert!(dest_root_windsurf
			.join(".windsurf/skills/repo-helper")
			.exists());
	}

	#[test]
	fn transfer_skill_fails_when_already_exists() {
		let _guard = env_lock().lock().unwrap();
		let temp = tempdir().unwrap();
		let source_root = temp.path().join("source");
		let dest_root = temp.path().join("dest");
		fs::create_dir_all(&source_root).unwrap();
		fs::create_dir_all(&dest_root).unwrap();

		// Create source skill
		let mut source_manager = ConfigManager::new(
			create_adapter(AgentType::Claude),
			false,
			Some(&source_root),
		);
		source_manager.load().unwrap();
		let mut skill = Skill::new("repo-helper");
		skill.description = Some("Copies files".to_string());
		source_manager.add_skill(skill).unwrap();

		// Create existing skill in destination
		let mut dest_manager = ConfigManager::new(
			create_adapter(AgentType::Cursor),
			false,
			Some(&dest_root),
		);
		dest_manager.load().unwrap();
		let mut existing_skill = Skill::new("repo-helper");
		existing_skill.description = Some("Existing skill".to_string());
		dest_manager.add_skill(existing_skill).unwrap();

		let result = transfer_skill(
			ResourceLocator {
				agent: AgentType::Claude,
				scope: InstallScope::Project,
				project_root: Some(source_root.clone()),
				name: "repo-helper".to_string(),
			},
			vec![InstallTarget {
				agent: AgentType::Cursor,
				scope: InstallScope::Project,
				project_root: Some(dest_root.clone()),
			}],
		)
		.unwrap();

		assert_eq!(result.failed_count(), 1);
		assert!(result.results[0]
			.error
			.as_ref()
			.unwrap()
			.contains("already exists"));
	}

	#[test]
	fn reconcile_skill_adds_multiple_agents_to_same_dir() {
		let _guard = env_lock().lock().unwrap();
		let temp = tempdir().unwrap();
		let root = temp.path().join("project");
		fs::create_dir_all(&root).unwrap();

		// Setup: Add a skill to Claude within the project
		let mut claude_manager = ConfigManager::new(
			create_adapter(AgentType::Claude),
			false,
			Some(&root),
		);
		claude_manager.load().unwrap();
		let mut skill = Skill::new("shared-skill");
		skill.description = Some("Shared across agents".to_string());
		claude_manager.add_skill(skill).unwrap();

		// Reconcile: add to Cursor and Windsurf within the same project
		let result = reconcile_skill(
			ResourceLocator {
				agent: AgentType::Claude,
				scope: InstallScope::Project,
				project_root: Some(root.clone()),
				name: "shared-skill".to_string(),
			},
			vec![AgentType::Cursor, AgentType::Windsurf],
			vec![],
		)
		.unwrap();

		// Both should succeed - Cursor and Windsurf use the same skills directory
		assert_eq!(result.success_count(), 2);

		// Verify directory was copied to the project's skills directory
		// Cursor and Windsurf both use .cursor/skills/ directory
		let skill_dir = root.join(".cursor/skills/shared-skill");
		assert!(skill_dir.exists());

		// Verify both agents can see the skill
		let mut cursor_manager = ConfigManager::new(
			create_adapter(AgentType::Cursor),
			false,
			Some(&root),
		);
		cursor_manager.load().unwrap();
		assert!(cursor_manager.get_skill("shared-skill").is_some());

		let mut windsurf_manager = ConfigManager::new(
			create_adapter(AgentType::Windsurf),
			false,
			Some(&root),
		);
		windsurf_manager.load().unwrap();
		assert!(windsurf_manager.get_skill("shared-skill").is_some());
	}

	#[test]
	fn transfer_duplicate_targets_are_deduplicated() {
		let _guard = env_lock().lock().unwrap();
		let temp = tempdir().unwrap();
		let source_root = temp.path().join("source");
		let dest_root = temp.path().join("dest");
		fs::create_dir_all(&source_root).unwrap();
		fs::create_dir_all(&dest_root).unwrap();

		let mut source_manager = ConfigManager::new(
			create_adapter(AgentType::Claude),
			false,
			Some(&source_root),
		);
		source_manager.load().unwrap();
		let mut skill = Skill::new("repo-helper");
		skill.description = Some("Copies files".to_string());
		source_manager.add_skill(skill).unwrap();

		// Pass the same target twice
		let result = transfer_skill(
			ResourceLocator {
				agent: AgentType::Claude,
				scope: InstallScope::Project,
				project_root: Some(source_root.clone()),
				name: "repo-helper".to_string(),
			},
			vec![
				InstallTarget {
					agent: AgentType::Cursor,
					scope: InstallScope::Project,
					project_root: Some(dest_root.clone()),
				},
				InstallTarget {
					agent: AgentType::Cursor,
					scope: InstallScope::Project,
					project_root: Some(dest_root.clone()),
				},
			],
		)
		.unwrap();

		// Should only process once due to deduplication
		assert_eq!(result.results.len(), 1);
		assert_eq!(result.success_count(), 1);
	}

	#[test]
	fn ensure_disjoint_rejects_agent_in_both_add_and_remove() {
		// `--add cursor --remove cursor` would net to a silent delete + exit 0
		// without this guard.
		let err = ensure_disjoint(
			&[AgentType::Cursor, AgentType::Claude],
			&[AgentType::Cline, AgentType::Cursor],
		)
		.unwrap_err();
		assert!(
			matches!(err, ConfigError::InvalidConfig(msg) if msg.contains("cursor")),
			"overlap must be rejected naming the agent"
		);

		// Disjoint add/remove sets are fine.
		assert!(ensure_disjoint(
			&[AgentType::Cursor],
			&[AgentType::Cline, AgentType::Claude],
		)
		.is_ok());
	}
}
