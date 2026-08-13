use crate::{
	create_adapter,
	errors::{ConfigError, Result},
	manager::ConfigManager,
	models::{AgentType, McpServer, Skill, SubAgent},
	registry,
};
use log::{info, warn};
use std::collections::HashSet;
#[cfg(test)]
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

#[derive(Debug, Clone)]
struct OperationPlan {
	target: InstallTarget,
	action: OperationAction,
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

fn target_resource_scope(
	target: &InstallTarget,
) -> crate::models::ResourceScope {
	match target.scope {
		InstallScope::Global => crate::models::ResourceScope::GlobalOnly,
		InstallScope::Project => crate::models::ResourceScope::ProjectOnly,
	}
}

fn mcp_supported_for_target(
	target: &InstallTarget,
	mcp: &McpServer,
) -> Result<()> {
	let descriptor = registry::get(target.agent);
	if !descriptor.supports_mcp_scope(target_resource_scope(target)) {
		return Err(ConfigError::unsupported_operation(
			"copy",
			"MCP server",
			descriptor.id,
		));
	}
	// The whole server, not just its transport. A dialect with no persisted
	// toggle omits a DISABLED one, so the copy would report success while
	// nothing landed. A cross-agent copy also refuses a LOSSY landing: the
	// caller may delete the original afterwards (reconcile does), and a
	// "successful" copy that silently shed the server's timeout would leave the
	// only surviving copy missing it.
	match aghub_agents::descriptor::mcp_fit(descriptor, mcp) {
		aghub_agents::descriptor::McpFit::Exact => Ok(()),
		aghub_agents::descriptor::McpFit::Lossy => {
			Err(ConfigError::unsupported_operation(
				"copy without losing fields",
				"MCP server",
				descriptor.id,
			))
		}
		aghub_agents::descriptor::McpFit::Unsupported => {
			Err(ConfigError::unsupported_operation(
				"copy incompatible",
				"MCP server",
				descriptor.id,
			))
		}
	}
}

fn sub_agent_supported_for_target(target: &InstallTarget) -> Result<()> {
	let descriptor = registry::get(target.agent);
	if descriptor.supports_sub_agent_scope(target_resource_scope(target)) {
		return Ok(());
	}
	Err(ConfigError::unsupported_operation(
		"copy",
		"sub-agent",
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
/// Preconditions every `reconcile_*` shares.
///
/// The destructive half is the point: a reconcile that REMOVES needs explicit
/// confirmation, and that policy lives HERE so the CLI's `--yes` and the API's
/// `confirm` are two adapters over one rule instead of two hand-kept copies.
/// The CLI still previews before it ever calls in, so from that surface this is
/// a backstop; for an API client it is the only gate there is.
fn ensure_reconcilable(
	added: &[AgentType],
	removed: &[AgentType],
	confirm: bool,
) -> Result<()> {
	ensure_disjoint(added, removed)?;
	if !removed.is_empty() && !confirm {
		return Err(ConfigError::InvalidConfig(format!(
			"reconcile would remove this resource from {} agent(s); \
			 confirm the removal explicitly to proceed",
			removed.len()
		)));
	}
	Ok(())
}

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

fn copy_plans(destinations: Vec<InstallTarget>) -> Vec<OperationPlan> {
	destinations
		.into_iter()
		.map(|target| OperationPlan {
			target,
			action: OperationAction::Copy,
		})
		.collect()
}

/// Build the two reconcile groups separately (rather than one flat `Vec`) so
/// callers can hand them to
/// [`crate::batch::run_staged_multi_target_mutation`] as primary (copies) /
/// secondary (deletes) — a runtime copy failure must never let its paired
/// delete run.
fn reconcile_plans(
	added: Vec<AgentType>,
	removed: Vec<AgentType>,
	scope: InstallScope,
	project_root: Option<PathBuf>,
) -> (Vec<OperationPlan>, Vec<OperationPlan>) {
	let copies = added
		.into_iter()
		.map(|agent| OperationPlan {
			target: InstallTarget {
				agent,
				scope,
				project_root: project_root.clone(),
			},
			action: OperationAction::Copy,
		})
		.collect();
	let deletes = removed
		.into_iter()
		.map(|agent| OperationPlan {
			target: InstallTarget {
				agent,
				scope,
				project_root: project_root.clone(),
			},
			action: OperationAction::Delete,
		})
		.collect();
	(copies, deletes)
}

fn batch_preflight_error(
	operation: &str,
	error: crate::batch::MultiTargetMutationError<OperationPlan, ConfigError>,
) -> ConfigError {
	let failures = error
		.failures
		.into_iter()
		.map(|failure| {
			let scope = match failure.target.target.scope {
				InstallScope::Global => "global",
				InstallScope::Project => "project",
			};
			format!(
				"{} {} ({scope}): {}",
				failure.target.action,
				failure.target.target.agent.as_str(),
				failure.reason
			)
		})
		.collect::<Vec<_>>()
		.join("; ");
	ConfigError::InvalidConfig(format!(
		"{operation} preflight failed; nothing was written: {failures}"
	))
}

fn operation_batch(
	report: crate::batch::MultiTargetMutationReport<
		OperationPlan,
		(),
		ConfigError,
	>,
) -> OperationBatchResult {
	OperationBatchResult {
		results: report
			.results
			.into_iter()
			.map(|row| OperationResult {
				target: row.target.target,
				action: row.target.action,
				success: row.result.is_ok(),
				error: row.result.err().map(|error| error.to_string()),
			})
			.collect(),
	}
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
	let report = crate::batch::run_multi_target_mutation(
		&destinations,
		|target| {
			validate_target(target)?;
			mcp_supported_for_target(target, &mcp)
		},
		|target| {
			let outcome = (|| -> Result<()> {
				let mut manager = build_manager(target);
				ensure_loaded(&mut manager)?;
				manager.add_mcp(mcp.clone())
			})();
			log_operation_outcome(
				"MCP",
				&mcp.name,
				OperationAction::Copy,
				target,
				&outcome,
			);
			outcome
		},
	)
	.map_err(|error| {
		let failures = error
			.failures
			.into_iter()
			.map(|failure| {
				let scope = match failure.target.scope {
					InstallScope::Global => "global",
					InstallScope::Project => "project",
				};
				format!(
					"{} ({scope}): {}",
					failure.target.agent.as_str(),
					failure.reason
				)
			})
			.collect::<Vec<_>>()
			.join("; ");
		ConfigError::InvalidConfig(format!(
			"MCP transfer preflight failed; nothing was written: {failures}"
		))
	})?;

	let results = report
		.results
		.into_iter()
		.map(|row| OperationResult {
			target: row.target,
			action: OperationAction::Copy,
			success: row.result.is_ok(),
			error: row.result.err().map(|error| error.to_string()),
		})
		.collect();

	Ok(OperationBatchResult { results })
}

pub fn reconcile_mcp(
	source: ResourceLocator,
	added: Vec<AgentType>,
	removed: Vec<AgentType>,
	confirm: bool,
) -> Result<OperationBatchResult> {
	ensure_reconcilable(&added, &removed, confirm)?;
	let mcp = load_source_mcp(&source)?;
	info!(
		"reconciling MCP '{}' with {} added and {} removed agent(s)",
		mcp.name,
		added.len(),
		removed.len()
	);
	let (copies, deletes) = reconcile_plans(
		added,
		removed,
		source.scope,
		source.project_root.clone(),
	);
	let report = crate::batch::run_staged_multi_target_mutation(
		&copies,
		&deletes,
		|plan| {
			validate_target(&plan.target)?;
			if plan.action == OperationAction::Copy {
				mcp_supported_for_target(&plan.target, &mcp)?;
			}
			Ok(())
		},
		|plan| {
			let outcome = (|| -> Result<()> {
				let mut manager = build_manager(&plan.target);
				ensure_loaded(&mut manager)?;
				match plan.action {
					OperationAction::Copy => manager.add_mcp(mcp.clone()),
					OperationAction::Delete => manager.remove_mcp(&source.name),
				}
			})();
			let name = if plan.action == OperationAction::Copy {
				&mcp.name
			} else {
				&source.name
			};
			log_operation_outcome(
				"MCP",
				name,
				plan.action,
				&plan.target,
				&outcome,
			);
			outcome
		},
		|plan| {
			ConfigError::InvalidConfig(format!(
				"skipped delete of MCP '{}' for agent '{}': a copy to \
				 another agent failed first; nothing was removed",
				source.name,
				plan.target.agent.as_str(),
			))
		},
	)
	.map_err(|error| batch_preflight_error("MCP reconcile", error))?;
	Ok(operation_batch(report))
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
	let plans = copy_plans(destinations);
	let report = crate::batch::run_multi_target_mutation(
		&plans,
		|plan| {
			validate_target(&plan.target)?;
			sub_agent_supported_for_target(&plan.target)
		},
		|plan| {
			let mut manager = build_manager(&plan.target);
			let outcome = (|| -> Result<()> {
				ensure_loaded(&mut manager)?;
				manager.add_sub_agent(sub_agent.clone())
			})();
			log_operation_outcome(
				"sub-agent",
				&sub_agent.name,
				plan.action,
				&plan.target,
				&outcome,
			);
			outcome
		},
	)
	.map_err(|error| batch_preflight_error("sub-agent transfer", error))?;
	Ok(operation_batch(report))
}

pub fn reconcile_sub_agent(
	source: ResourceLocator,
	added: Vec<AgentType>,
	removed: Vec<AgentType>,
	confirm: bool,
) -> Result<OperationBatchResult> {
	ensure_reconcilable(&added, &removed, confirm)?;
	let sub_agent = load_source_sub_agent(&source)?;
	info!(
		"reconciling sub-agent '{}' with {} added and {} removed agent(s)",
		sub_agent.name,
		added.len(),
		removed.len()
	);
	let (copies, deletes) = reconcile_plans(
		added,
		removed,
		source.scope,
		source.project_root.clone(),
	);
	let report = crate::batch::run_staged_multi_target_mutation(
		&copies,
		&deletes,
		|plan| {
			validate_target(&plan.target)?;
			if plan.action == OperationAction::Copy {
				sub_agent_supported_for_target(&plan.target)?;
			}
			Ok(())
		},
		|plan| {
			let outcome = (|| -> Result<()> {
				let mut manager = build_manager(&plan.target);
				ensure_loaded(&mut manager)?;
				match plan.action {
					OperationAction::Copy => {
						manager.add_sub_agent(sub_agent.clone())
					}
					OperationAction::Delete => {
						manager.remove_sub_agent(&source.name)
					}
				}
			})();
			let name = if plan.action == OperationAction::Copy {
				&sub_agent.name
			} else {
				&source.name
			};
			log_operation_outcome(
				"sub-agent",
				name,
				plan.action,
				&plan.target,
				&outcome,
			);
			outcome
		},
		|plan| {
			ConfigError::InvalidConfig(format!(
				"skipped delete of sub-agent '{}' for agent '{}': a copy \
				 to another agent failed first; nothing was removed",
				source.name,
				plan.target.agent.as_str(),
			))
		},
	)
	.map_err(|error| batch_preflight_error("sub-agent reconcile", error))?;
	Ok(operation_batch(report))
}

pub fn transfer_skill(
	source: ResourceLocator,
	destinations: Vec<InstallTarget>,
) -> Result<OperationBatchResult> {
	let skill = load_source_skill(&source)?;
	let source_root = resolve_skill_root(&skill)?;
	let destinations = unique_targets(destinations);
	ensure_destinations(&destinations)?;
	info!(
		"transferring skill '{}' from '{}' to {} destination(s)",
		skill.name,
		source_root.display(),
		destinations.len()
	);
	let plans = copy_plans(destinations);
	let report = crate::batch::run_multi_target_mutation(
		&plans,
		|plan| {
			validate_target(&plan.target)?;
			skill_target_dir(&plan.target).map(|_| ())
		},
		|plan| {
			let outcome = (|| -> Result<()> {
				let mut manager = build_manager(&plan.target);
				ensure_loaded(&mut manager)?;
				if manager.get_skill(&skill.name).is_some() {
					return Err(ConfigError::resource_exists(
						"skill",
						&skill.name,
					));
				}
				manager.add_skill_from_path(&source_root)?;
				Ok(())
			})();
			log_operation_outcome(
				"skill",
				&skill.name,
				plan.action,
				&plan.target,
				&outcome,
			);
			outcome
		},
	)
	.map_err(|error| batch_preflight_error("skill transfer", error))?;
	Ok(operation_batch(report))
}

/// Every in-scope agent whose config currently carries `name`.
///
/// One extra scan, taken only to answer "will anyone still be reading the
/// Master after this reconcile?" — the per-agent removal planner cannot see
/// that, because a NativeReader leaves no artifact for it to count.
fn skill_holders(name: &str, source: &ResourceLocator) -> Vec<AgentType> {
	let scope = match source.scope {
		InstallScope::Global => crate::models::ResourceScope::GlobalOnly,
		InstallScope::Project => crate::models::ResourceScope::ProjectOnly,
	};
	crate::load_all_agents(scope, source.project_root.as_deref())
		.into_iter()
		.filter(|agent| agent.skills.iter().any(|s| s.name == name))
		.filter_map(|agent| agent.agent_id.parse::<AgentType>().ok())
		.collect()
}

pub fn reconcile_skill(
	source: ResourceLocator,
	added: Vec<AgentType>,
	removed: Vec<AgentType>,
	confirm: bool,
) -> Result<OperationBatchResult> {
	ensure_reconcilable(&added, &removed, confirm)?;
	let skill = load_source_skill(&source)?;
	let source_root = resolve_skill_root(&skill)?;
	info!(
		"reconciling skill '{}' with {} added and {} removed agent(s)",
		skill.name,
		added.len(),
		removed.len()
	);
	// Does this reconcile drop the skill from EVERY agent that currently holds
	// it? Then the shared Master has no remaining reader and must go with it.
	// Removing it per-agent instead would refuse on every target (an agent
	// reading the Master directly has nothing agent-specific to take) and leave
	// the Master orphaned — and the desktop's manage-agents dialog allows
	// exactly this shape: deselect every agent, no adds.
	let exhaustive = !removed.is_empty()
		&& added.is_empty()
		&& skill_holders(&skill.name, &source)
			.iter()
			.all(|held| removed.contains(held));
	let (copies, deletes) = reconcile_plans(
		added,
		removed,
		source.scope,
		source.project_root.clone(),
	);
	let report = crate::batch::run_staged_multi_target_mutation(
		&copies,
		&deletes,
		|plan| {
			validate_target(&plan.target)?;
			if plan.action == OperationAction::Copy {
				skill_target_dir(&plan.target)?;
			}
			Ok(())
		},
		|plan| {
			let outcome = match plan.action {
				OperationAction::Copy => (|| -> Result<()> {
					let mut manager = build_manager(&plan.target);
					ensure_loaded(&mut manager)?;
					manager.add_skill_from_path(&source_root)?;
					Ok(())
				})(),
				// Use the planned-removal seam — never blind-delete a shared
				// universal master discovered through an agent's read dirs.
				OperationAction::Delete => (|| -> Result<()> {
					let mut manager = build_manager(&plan.target);
					ensure_loaded(&mut manager)?;
					match manager.remove_skill_planned(
						&skill.name,
						exhaustive,
						false,
						true,
					) {
						// Nothing removable is NOT success here. The planner
						// keeps a shared universal Master on a single-agent
						// removal (an agent that reads `.agents/skills`
						// directly leaves no per-agent artifact to take), so
						// reporting Ok told the user "removed from cursor"
						// while cursor still sees it — and, before the planner
						// fix, the alternative was worse: it deleted the Master
						// and every other agent lost the skill too.
						Ok(outcome)
							if outcome.plan.paths.is_empty()
								&& outcome.plan.shared_master_kept =>
						{
							Err(ConfigError::unsupported_operation(
								"remove for this agent alone",
								"skill it reads from the shared master",
								plan.target.agent.as_str(),
							))
						}
						Ok(_) | Err(ConfigError::ResourceNotFound { .. }) => {
							Ok(())
						}
						Err(error) => Err(error),
					}
				})(),
			};
			log_operation_outcome(
				"skill",
				&skill.name,
				plan.action,
				&plan.target,
				&outcome,
			);
			outcome
		},
		|plan| {
			ConfigError::InvalidConfig(format!(
				"skipped delete of skill '{}' for agent '{}': a copy to \
				 another agent failed first; nothing was removed",
				skill.name,
				plan.target.agent.as_str(),
			))
		},
	)
	.map_err(|error| batch_preflight_error("skill reconcile", error))?;
	Ok(operation_batch(report))
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
	fn transfer_mcp_preflight_prevents_partial_writes() {
		let _guard = env_lock().lock().unwrap();
		let temporary = tempdir().unwrap();
		let source_root = temporary.path().join("source");
		let valid_root = temporary.path().join("valid-target");
		let unsupported_root = temporary.path().join("unsupported-target");
		fs::create_dir_all(&source_root).unwrap();
		fs::create_dir_all(&valid_root).unwrap();
		fs::create_dir_all(&unsupported_root).unwrap();

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
				project_root: Some(source_root),
				name: "filesystem".to_string(),
			},
			vec![
				InstallTarget {
					agent: AgentType::Cursor,
					scope: InstallScope::Project,
					project_root: Some(valid_root.clone()),
				},
				InstallTarget {
					agent: AgentType::AugmentCode,
					scope: InstallScope::Project,
					project_root: Some(unsupported_root),
				},
			],
		);

		assert!(
			result.is_err(),
			"predictable target failure rejects the batch"
		);
		let mut valid_manager = ConfigManager::new(
			create_adapter(AgentType::Cursor),
			false,
			Some(&valid_root),
		);
		valid_manager.load().unwrap();
		assert!(
			valid_manager.get_mcp("filesystem").is_none(),
			"no target may be written before every target passes preflight",
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
			true,                    // confirm
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

	// Fix A regression test: a Copy that fails at RUNTIME (after preflight
	// already passed) must not let its paired Delete run. Cursor supports
	// project-scope stdio MCPs (so `mcp_supported_for_target` preflight is
	// clean), but Cursor's OWN mcp config already holds an unrelated MCP
	// named "filesystem" — `add_mcp`'s duplicate-name guard rejects the copy
	// only once it actually runs. Before the fix, `reconcile_mcp` built one
	// flat Copy-then-Delete plan and attempted every row regardless, so the
	// Claude delete still ran: the MCP would vanish from Claude without ever
	// landing on Cursor — gone from every agent. This test fails on that
	// regression because `claude_manager.get_mcp("filesystem")` would be
	// `None` afterward.
	#[test]
	fn reconcile_mcp_keeps_source_when_a_copy_fails_at_runtime() {
		let _guard = env_lock().lock().unwrap();
		let temp = tempdir().unwrap();
		let root = temp.path().join("project");
		fs::create_dir_all(&root).unwrap();

		let mut claude_manager = ConfigManager::new(
			create_adapter(AgentType::Claude),
			false,
			Some(&root),
		);
		claude_manager.load().unwrap();
		claude_manager
			.add_mcp(McpServer::new(
				"filesystem",
				McpTransport::stdio("npx", vec!["mcp-filesystem".to_string()]),
			))
			.unwrap();

		// Pre-populate Cursor's OWN project config with an unrelated MCP of
		// the same name so its copy fails at write time, not at preflight.
		let mut cursor_manager = ConfigManager::new(
			create_adapter(AgentType::Cursor),
			false,
			Some(&root),
		);
		cursor_manager.load().unwrap();
		cursor_manager
			.add_mcp(McpServer::new(
				"filesystem",
				McpTransport::stdio("echo", vec!["conflict".to_string()]),
			))
			.unwrap();

		let result = reconcile_mcp(
			ResourceLocator {
				agent: AgentType::Claude,
				scope: InstallScope::Project,
				project_root: Some(root.clone()),
				name: "filesystem".to_string(),
			},
			vec![AgentType::Cursor], // added: fails at runtime
			vec![AgentType::Claude], // removed: must be skipped
			true,                    // confirm
		)
		.unwrap();

		assert_eq!(result.results.len(), 2);
		let copy_row = result
			.results
			.iter()
			.find(|r| r.action == OperationAction::Copy)
			.expect("a copy row must be present");
		assert!(!copy_row.success, "the Cursor copy must fail");

		let delete_row = result
			.results
			.iter()
			.find(|r| r.action == OperationAction::Delete)
			.expect("a delete row must be present");
		assert!(
			!delete_row.success,
			"the Claude delete must be skipped, not attempted"
		);
		assert!(
			delete_row
				.error
				.as_ref()
				.is_some_and(|e| e.contains("skipped")),
			"the delete row must read as skipped, not as an attempted \
			 failure: {:?}",
			delete_row.error,
		);

		// The critical assertion: the source MCP must survive. Before the
		// fix this was deleted even though its only copy destination failed.
		let mut claude_manager = ConfigManager::new(
			create_adapter(AgentType::Claude),
			false,
			Some(&root),
		);
		claude_manager.load().unwrap();
		assert!(
			claude_manager.get_mcp("filesystem").is_some(),
			"source MCP must survive a reconcile whose only copy failed"
		);
	}

	#[test]
	fn transfer_skill_materializes_master_and_referrer() {
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
				agent: AgentType::Windsurf,
				scope: InstallScope::Project,
				project_root: Some(dest_root.clone()),
			}],
		)
		.unwrap();

		assert_eq!(result.success_count(), 1);
		let master = dest_root.join(".agents/skills/repo-helper");
		let referrer = dest_root.join(".windsurf/skills/repo-helper");
		assert!(master.join("assets/notes.txt").exists());
		assert!(
			crate::skills::linker::Linker::is_link(&referrer),
			"skill transfer must use ConfigManager's Master + Referrer layout",
		);
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
			true,                    // confirm
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

	// The confirmation gate lives in core so the CLI's `--yes` and the API's
	// `confirm` cannot drift. Asserting the ERROR alone would still pass if the
	// guard ran AFTER the deletes, so this also proves the skill survived.
	#[test]
	fn reconcile_skill_without_confirm_refuses_and_removes_nothing() {
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
		manager.add_skill(Skill::new("repo-helper")).unwrap();

		let error = reconcile_skill(
			ResourceLocator {
				agent: AgentType::Claude,
				scope: InstallScope::Project,
				project_root: Some(root.clone()),
				name: "repo-helper".to_string(),
			},
			vec![],
			vec![AgentType::Claude],
			false, // confirm withheld
		)
		.expect_err("a removing reconcile must refuse without confirmation");
		assert!(
			error.to_string().contains("confirm"),
			"error should name what is missing, got: {error}"
		);

		let mut manager = ConfigManager::new(
			create_adapter(AgentType::Claude),
			false,
			Some(&root),
		);
		manager.load().unwrap();
		assert!(
			manager.get_skill("repo-helper").is_some(),
			"unconfirmed reconcile must not delete the skill"
		);
	}

	// Adds are non-destructive, so withholding confirmation must NOT block
	// them — otherwise the guard silently breaks every install-only reconcile.
	#[test]
	fn reconcile_skill_adds_without_confirm() {
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
		manager.add_skill(Skill::new("repo-helper")).unwrap();

		let referrer = root.join(".windsurf/skills/repo-helper");
		assert!(
			!referrer.exists(),
			"the destination must start uncovered, or the assertion at the \
			 end proves nothing"
		);

		let result = reconcile_skill(
			ResourceLocator {
				agent: AgentType::Claude,
				scope: InstallScope::Project,
				project_root: Some(root.clone()),
				name: "repo-helper".to_string(),
			},
			// Windsurf NEEDS a Referrer — a NativeReader destination would
			// read the Master the fixture already created, so the disk
			// assertion below would hold even if the reconcile did nothing.
			vec![AgentType::Windsurf],
			vec![],
			false, // confirm withheld — irrelevant to an add
		)
		.expect("an add-only reconcile needs no confirmation");

		// `failed_count() == 0` alone is vacuous — an empty result set also
		// satisfies it. Pin the copy AND the state it was supposed to produce.
		assert_eq!(result.results.len(), 1);
		assert_eq!(result.results[0].action, OperationAction::Copy);
		assert!(result.results[0].success);
		assert!(referrer.exists(), "the add must create Windsurf's referrer");
	}

	// Fix A regression test (skill case): a Copy that fails at RUNTIME (after
	// preflight already passed) must not let its paired Delete run — same
	// policy as `reconcile_mcp_keeps_source_when_a_copy_fails_at_runtime`, but
	// for the highest-blast-radius resource, since a skill delete can
	// `remove_dir_all` an on-disk directory.
	//
	// The source skill here is a COPY-LAYOUT skill: a plain, hand-created
	// directory inside Claude's own skills dir with no `.agents/skills`
	// Master, so `canonical_path` is None and this directory is the SOLE
	// on-disk copy. Windsurf's own skills dir already holds a real directory
	// at the slot the copy would need to link into, so the universal
	// materializer's link step reports a conflict at write time — preflight
	// (`skill_target_dir`) only resolves the write dir, it never checks for an
	// existing occupant. Before the fix, `reconcile_skill` attempted the
	// Delete regardless: the source directory would be `remove_dir_all`'d
	// even though the Windsurf copy never landed, destroying the skill
	// outright with no surviving copy anywhere. This test fails on that
	// regression because `skill_dir.join("SKILL.md").exists()` would be
	// `false` afterward.
	#[test]
	fn reconcile_skill_keeps_source_when_a_copy_fails_at_runtime() {
		let _guard = env_lock().lock().unwrap();
		let temp = tempdir().unwrap();
		let root = temp.path().join("project");

		let claude_skills = root.join(".claude/skills");
		let skill_dir = claude_skills.join("repo-helper");
		fs::create_dir_all(&skill_dir).unwrap();
		fs::write(
			skill_dir.join("SKILL.md"),
			"---\nname: repo-helper\ndescription: Copies files\n---\n",
		)
		.unwrap();

		// Pre-occupy the Windsurf destination slot with a real directory (not
		// a symlink) so `Linker::link` reports `Conflict` at runtime.
		let windsurf_slot = root.join(".windsurf/skills/repo-helper");
		fs::create_dir_all(&windsurf_slot).unwrap();
		fs::write(windsurf_slot.join("occupant.txt"), "conflict").unwrap();

		let mut claude_manager = ConfigManager::new(
			create_adapter(AgentType::Claude),
			false,
			Some(&root),
		);
		claude_manager.load().unwrap();
		let source_skill = claude_manager
			.get_skill("repo-helper")
			.expect("discovery must pick up the hand-created skill dir");
		assert!(
			source_skill.canonical_path.is_none(),
			"copy-layout precondition: no universal Master"
		);

		let result = reconcile_skill(
			ResourceLocator {
				agent: AgentType::Claude,
				scope: InstallScope::Project,
				project_root: Some(root.clone()),
				name: "repo-helper".to_string(),
			},
			vec![AgentType::Windsurf], // added: fails at runtime
			vec![AgentType::Claude],   // removed: must be skipped
			true,                      // confirm
		)
		.unwrap();

		assert_eq!(result.results.len(), 2);
		let copy_row = result
			.results
			.iter()
			.find(|r| r.action == OperationAction::Copy)
			.expect("a copy row must be present");
		assert!(!copy_row.success, "the Windsurf copy must fail");

		let delete_row = result
			.results
			.iter()
			.find(|r| r.action == OperationAction::Delete)
			.expect("a delete row must be present");
		assert!(
			!delete_row.success,
			"the Claude delete must be skipped, not attempted"
		);
		assert!(
			delete_row
				.error
				.as_ref()
				.is_some_and(|e| e.contains("skipped")),
			"the delete row must read as skipped, not as an attempted \
			 failure: {:?}",
			delete_row.error,
		);

		// The critical assertion: the source skill directory is the SOLE
		// on-disk copy and must survive.
		assert!(
			skill_dir.join("SKILL.md").exists(),
			"source skill dir must survive a reconcile whose only copy failed"
		);
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
			true, // confirm
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
			true, // confirm
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
			true, // confirm
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
			true,                      // confirm
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
		let dest_root_copilot = temp.path().join("dest_copilot");
		fs::create_dir_all(&source_root).unwrap();
		fs::create_dir_all(&dest_root_cursor).unwrap();
		fs::create_dir_all(&dest_root_copilot).unwrap();

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
					agent: AgentType::Copilot,
					scope: InstallScope::Project,
					project_root: Some(dest_root_copilot.clone()),
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

		let mut copilot_manager = ConfigManager::new(
			create_adapter(AgentType::Copilot),
			false,
			Some(&dest_root_copilot),
		);
		copilot_manager.load().unwrap();
		assert!(copilot_manager.get_mcp("filesystem").is_some());
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
		assert!(dest_root_cursor
			.join(".agents/skills/repo-helper/SKILL.md")
			.exists());
		assert!(dest_root_windsurf
			.join(".agents/skills/repo-helper/SKILL.md")
			.exists());
		assert!(crate::skills::linker::Linker::is_link(
			&dest_root_windsurf.join(".windsurf/skills/repo-helper")
		));
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
			false, // confirm
		)
		.unwrap();

		// Both should succeed: Cursor reads the Master natively; Windsurf gets a
		// Referrer to that same Master.
		assert_eq!(result.success_count(), 2);

		assert!(root.join(".agents/skills/shared-skill/SKILL.md").exists());
		assert!(crate::skills::linker::Linker::is_link(
			&root.join(".windsurf/skills/shared-skill")
		));

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
