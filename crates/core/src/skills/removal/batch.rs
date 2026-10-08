//! Single core entry for skill deletion by name across multiple agents.
//!
//! Handles shared-first ordering, whole-batch dry-run preflight, prior-row credit,
//! sibling credit, lock-free preview vs locked commit, Master GC, and lock pruning.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::{
	agent_skill_dirs_in_scope, allowed_skill_roots, assert_strictly_contained,
	by_path_skill_dir, by_path_skill_name, evaluate_removal_verdict,
	expand_tilde_path, plan_copy_removal, unmanaged_skill_dirs, PruneStatus,
	RemovalOutcome, Verdict,
};
use crate::batch::{Backing, RemovalCredits};
use crate::errors::{ConfigError, Result};
use crate::models::{AgentType, ResourceScope};
use crate::registry;
use crate::ConfigManager;

/// Target for a batch skill removal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillRemovalTarget {
	ByName(String),
	ByPath(PathBuf),
}

/// Request for a batch skill removal.
#[derive(Debug, Clone)]
pub struct SkillRemovalRequest {
	pub target: SkillRemovalTarget,
	pub scope: ResourceScope,
	pub project_root: Option<PathBuf>,
	pub agents: Vec<AgentType>,
	pub dry_run: bool,
	pub all_agents: bool,
	/// Caller-supplied paths credited as already removed during dry-run preflight only.
	/// In commit mode, caller prior credit is ignored (seeded empty), but the preflight pass
	/// still accumulates earlier rows' plan.paths for sibling credit. The in-lock execution
	/// always rechecks the real disk (&[]).
	pub prior_removed_paths: Vec<PathBuf>,
	/// A copy in this batch keeps the Master alive; skips the exhaustiveness
	/// preflight holder scan (surviving holders are still computed for outcome rows).
	pub keeps_master: bool,
	pub plugin_owner: Option<String>,
}

/// Outcome row for a single agent in a batch skill removal.
#[derive(Debug, Clone)]
pub struct SkillRemovalRow {
	pub agent: AgentType,
	pub verdict: Verdict,
	pub outcome: crate::dto::RemovalKind,
	pub error: Option<String>,
	pub typed_error: Option<Arc<ConfigError>>,
	pub is_load_error: bool,
	pub still_read_from: Vec<PathBuf>,
	pub paths: Vec<PathBuf>,
	pub skipped: Vec<PathBuf>,
	pub executed: bool,
	pub needs_confirm: bool,
}

fn still_read_paths(
	outcome: &crate::skills::removal::RemovalOutcome,
) -> Vec<PathBuf> {
	if !outcome.plan.still_read_from.is_empty() {
		outcome.plan.still_read_from.clone()
	} else if let Verdict::Kept {
		ref still_read_from,
	} = outcome.verdict
	{
		still_read_from.iter().map(|h| h.path.clone()).collect()
	} else {
		Vec::new()
	}
}

impl SkillRemovalRow {
	pub fn outcome_str(&self) -> &'static str {
		self.outcome.as_str()
	}

	pub fn wire_code(&self) -> Option<&'static str> {
		if let Some(ref err) = self.typed_error {
			Some(crate::error_codes::wire_code(err))
		} else {
			None
		}
	}

	pub fn from_outcome(
		agent: AgentType,
		outcome: &crate::skills::removal::RemovalOutcome,
		dry_run: bool,
	) -> Self {
		let kind = crate::dto::removal_kind_from_outcome(outcome, dry_run);
		let still_read_from = still_read_paths(outcome);
		Self {
			agent,
			verdict: outcome.verdict.clone(),
			outcome: kind,
			error: None,
			typed_error: None,
			is_load_error: false,
			still_read_from,
			paths: outcome.plan.paths.clone(),
			skipped: outcome.plan.skipped.clone(),
			executed: outcome.executed,
			needs_confirm: outcome.plan.needs_confirm,
		}
	}

	pub fn noop(
		agent: AgentType,
		verdict: Verdict,
		outcome: crate::dto::RemovalKind,
	) -> Self {
		Self {
			agent,
			verdict,
			outcome,
			error: None,
			typed_error: None,
			is_load_error: false,
			still_read_from: Vec::new(),
			paths: Vec::new(),
			skipped: Vec::new(),
			executed: false,
			needs_confirm: false,
		}
	}

	pub fn absent(agent: AgentType) -> Self {
		Self::noop(agent, Verdict::Absent, crate::dto::RemovalKind::Absent)
	}

	pub fn error_row(
		agent: AgentType,
		err: ConfigError,
		is_load_error: bool,
	) -> Self {
		let mut row = Self::absent(agent);
		row.error = Some(err.to_string());
		row.typed_error = Some(Arc::new(err));
		row.is_load_error = is_load_error;
		row
	}
}

/// Render a skill removal's [`PruneStatus`] onto a JSON `payload`.
pub fn apply_prune_fields(
	payload: &mut serde_json::Value,
	prune: &PruneStatus,
) {
	match prune {
		PruneStatus::NotRun => {}
		PruneStatus::WouldPrune(keys) => {
			payload["would_prune_lock_entries"] = serde_json::json!(keys);
		}
		PruneStatus::Pruned(keys) => {
			payload["pruned_lock_entries"] = serde_json::json!(keys);
		}
		PruneStatus::Failed { reason, pruned } => {
			payload["prune_error"] = serde_json::json!(reason);
			payload["pruned_lock_entries"] = serde_json::json!(pruned);
		}
	}
}

/// Wire keys `still_read_by{,_managed,_unmanaged}`; `None` when no holders.
pub type HolderKeys = (
	Option<Vec<String>>,
	Option<Vec<String>>,
	Option<Vec<String>>,
);

/// Holders of a skill that survive a removal, partitioned into managed and unmanaged.
#[derive(Debug, Clone, Default)]
pub struct SkillHoldersView {
	pub all: Vec<String>,
	pub managed: Vec<String>,
	pub unmanaged: Vec<String>,
}

impl SkillHoldersView {
	pub fn from_agents(agents: impl IntoIterator<Item = AgentType>) -> Self {
		let all: Vec<String> =
			agents.into_iter().map(|a| a.as_str().to_string()).collect();
		if all.is_empty() {
			return Self::default();
		}
		let (managed, unmanaged): (Vec<String>, Vec<String>) = all
			.iter()
			.cloned()
			.partition(|a| crate::agent_settings::is_managed(a));
		Self {
			all,
			managed,
			unmanaged,
		}
	}
	pub fn is_empty(&self) -> bool {
		self.all.is_empty()
	}

	/// Returns (still_read_by, still_read_by_managed, still_read_by_unmanaged),
	/// each None when there are no holders, or Some containing the agent IDs.
	pub fn to_options(&self) -> HolderKeys {
		if self.is_empty() {
			(None, None, None)
		} else {
			(
				Some(self.all.clone()),
				Some(self.managed.clone()),
				Some(self.unmanaged.clone()),
			)
		}
	}
}

/// Unified wire view for a single-skill removal across one or more target agents.
#[derive(Debug, Clone)]
pub struct SingleSkillRemovalView {
	pub removal_view: crate::dto::RemovalView,
	pub holders: SkillHoldersView,
	pub prune: PruneStatus,
	pub code: Option<&'static str>,
}

pub fn clone_config_error(err: &ConfigError) -> ConfigError {
	match err {
		ConfigError::UnsupportedOperation {
			message,
			rejected_targets,
		} => ConfigError::UnsupportedOperation {
			message: message.clone(),
			rejected_targets: rejected_targets.clone(),
		},
		ConfigError::ResourceNotFound {
			resource_type,
			name,
		} => ConfigError::ResourceNotFound {
			resource_type: resource_type.clone(),
			name: name.clone(),
		},
		ConfigError::ResourceExists {
			resource_type,
			name,
		} => ConfigError::ResourceExists {
			resource_type: resource_type.clone(),
			name: name.clone(),
		},
		ConfigError::NotFound { path } => {
			ConfigError::NotFound { path: path.clone() }
		}
		ConfigError::ValidationFailed(s) => {
			ConfigError::ValidationFailed(s.clone())
		}
		ConfigError::InvalidConfig(s) => ConfigError::InvalidConfig(s.clone()),
		ConfigError::InvalidConfigWithTargets {
			message,
			rejected_targets,
		} => ConfigError::InvalidConfigWithTargets {
			message: message.clone(),
			rejected_targets: rejected_targets.clone(),
		},
		ConfigError::Io(e) => {
			ConfigError::Io(std::io::Error::new(e.kind(), e.to_string()))
		}
		ConfigError::Json(e) => ConfigError::Json(
			<serde_json::Error as serde::de::Error>::custom(e.to_string()),
		),
	}
}

/// Response returned by the batch skill removal entry point.
#[derive(Debug, Clone)]
pub struct SkillRemovalResponse {
	pub rows: Vec<SkillRemovalRow>,
	pub prune: PruneStatus,
	pub keepers: Vec<AgentType>,
	pub unreadable: Vec<&'static str>,
	pub master_reclaimed: bool,
	pub would_reclaim_master: bool,
}

impl SkillRemovalResponse {
	/// Holders of the skill that keep the master, split into managed vs unmanaged.
	pub fn holders_view(&self) -> SkillHoldersView {
		SkillHoldersView::from_agents(self.keepers.iter().copied())
	}

	/// Project this response into the shared [`AgentBatchView`] wire envelope.
	pub fn to_batch_view(
		&self,
		skill_name: &str,
		dry_run: bool,
	) -> crate::batch::AgentBatchView {
		let mut results = Vec::new();
		let mut success_count = 0;
		let mut failed_count = 0;

		for row in &self.rows {
			let is_absent_noop =
				matches!(row.verdict, Verdict::Absent | Verdict::LockOnly)
					&& matches!(
						row.typed_error.as_deref(),
						Some(ConfigError::ResourceNotFound { .. })
					);
			let outcome_str = row.outcome_str();
			let wire_code = if is_absent_noop {
				None
			} else {
				row.wire_code()
			};
			let error = if is_absent_noop {
				None
			} else {
				row.error.clone()
			};
			let ok = is_absent_noop
				|| (row.error.is_none()
					&& row.typed_error.is_none()
					&& row.verdict != Verdict::Partial);

			if ok {
				success_count += 1;
			} else {
				failed_count += 1;
			}

			let view = crate::dto::RemovalView {
				success: ok,
				dry_run,
				executed: row.executed,
				needs_confirm: row.needs_confirm,
				paths: row
					.paths
					.iter()
					.map(|p| p.display().to_string())
					.collect(),
				skipped: row
					.skipped
					.iter()
					.map(|p| p.display().to_string())
					.collect(),
				deleted_path: row
					.executed
					.then(|| row.paths.first().map(|p| p.display().to_string()))
					.flatten(),
				outcome: row.outcome,
			};
			let mut payload = serde_json::to_value(&view).unwrap();
			payload["type"] = serde_json::json!("skill");
			payload["name"] = serde_json::json!(skill_name);
			payload["code"] = serde_json::json!(wire_code);

			if !row.still_read_from.is_empty() {
				payload["still_read_from"] = serde_json::json!(row
					.still_read_from
					.iter()
					.map(|p| p.display().to_string())
					.collect::<Vec<_>>());
			}

			// Unrequested holders that keep the master, split into managed vs unmanaged.
			let (still_read_by, still_read_by_managed, still_read_by_unmanaged) =
				self.holders_view().to_options();
			if let (Some(all), Some(managed), Some(unmanaged)) = (
				still_read_by,
				still_read_by_managed,
				still_read_by_unmanaged,
			) {
				payload["still_read_by"] = serde_json::json!(all);
				payload["still_read_by_managed"] = serde_json::json!(managed);
				payload["still_read_by_unmanaged"] =
					serde_json::json!(unmanaged);
			}

			if dry_run {
				payload["master_reclaimed"] = serde_json::json!(false);
				if self.would_reclaim_master {
					payload["would_reclaim_master"] = serde_json::json!(true);
				}
			} else {
				payload["master_reclaimed"] =
					serde_json::json!(self.master_reclaimed);
			}
			apply_prune_fields(&mut payload, &self.prune);

			results.push(crate::batch::AgentOpResultView {
				agent: row.agent.as_str().to_string(),
				ok,
				outcome: Some(outcome_str.to_string()),
				code: wire_code.map(|c| c.to_string()),
				output: Some(payload),
				error,
			});
		}

		crate::batch::AgentBatchView {
			success_count,
			failed_count,
			results,
		}
	}

	/// Project this batch removal response into an aggregate single-skill removal view.
	///
	/// Yields the aggregate outcome, success, paths, and any fatal error across ALL rows.
	pub fn to_single_view(
		&self,
		dry_run: bool,
	) -> Result<SingleSkillRemovalView> {
		let has_removed = self
			.rows
			.iter()
			.any(|r| r.outcome == crate::dto::RemovalKind::Removed);
		let has_kept = self
			.rows
			.iter()
			.any(|r| r.outcome == crate::dto::RemovalKind::Kept);
		let has_refused_or_error = self
			.rows
			.iter()
			.any(|r| r.typed_error.is_some() || r.error.is_some());
		let has_partial = self
			.rows
			.iter()
			.any(|r| r.outcome == crate::dto::RemovalKind::Partial);

		for row in &self.rows {
			let is_absent_noop =
				matches!(row.verdict, Verdict::Absent | Verdict::LockOnly)
					&& matches!(
						row.typed_error.as_deref(),
						Some(ConfigError::ResourceNotFound { .. })
					);
			let is_refused_preview =
				dry_run && matches!(row.verdict, Verdict::Refused { .. });
			let is_partial = matches!(row.verdict, Verdict::Partial);
			let is_partial_mixed = !dry_run && has_removed;
			let is_fatal_error =
				(row.typed_error.is_some() || row.error.is_some())
					&& !is_absent_noop
					&& !is_refused_preview
					&& !is_partial && !is_partial_mixed;
			if is_fatal_error {
				if let Some(ref err) = row.typed_error {
					return Err(clone_config_error(err));
				}
				if let Some(ref msg) = row.error {
					return Err(ConfigError::InvalidConfig(msg.clone()));
				}
			}
		}

		let mut paths: Vec<String> = Vec::new();
		for row in &self.rows {
			for p in &row.paths {
				let s = p.display().to_string();
				if !paths.contains(&s) {
					paths.push(s);
				}
			}
		}

		let mut skipped: Vec<String> = Vec::new();
		for row in &self.rows {
			for p in &row.skipped {
				let s = p.display().to_string();
				if !skipped.contains(&s) {
					skipped.push(s);
				}
			}
		}

		let executed = self.rows.iter().any(|r| r.executed);
		let needs_confirm = self.rows.iter().any(|r| r.needs_confirm);
		let deleted_path = if executed {
			paths.first().cloned()
		} else {
			None
		};

		// Outcome precedence across rows: Partial > Removed > Preview > Kept > Absent.
		// When not dry-run, if rows contain both Removed and Kept (or a refused/error
		// row alongside a Removed row), fold into outcome=Partial, success=false.
		let outcome = if has_partial
			|| (!dry_run && has_removed && (has_kept || has_refused_or_error))
		{
			crate::dto::RemovalKind::Partial
		} else if has_removed {
			crate::dto::RemovalKind::Removed
		} else if self
			.rows
			.iter()
			.any(|r| r.outcome == crate::dto::RemovalKind::Preview)
		{
			crate::dto::RemovalKind::Preview
		} else if has_kept || (self.rows.is_empty() && !self.keepers.is_empty())
		{
			crate::dto::RemovalKind::Kept
		} else {
			crate::dto::RemovalKind::Absent
		};

		let success = outcome != crate::dto::RemovalKind::Partial
			&& self.rows.iter().all(|r| r.verdict != Verdict::Partial);

		let removal_view = crate::dto::RemovalView {
			success,
			dry_run,
			executed,
			needs_confirm,
			paths,
			skipped,
			deleted_path,
			outcome,
		};

		let code = self.rows.iter().find_map(|r| {
			if dry_run && matches!(r.verdict, Verdict::Refused { .. }) {
				Some(crate::error_codes::wire_code(
					&ConfigError::UnsupportedOperation {
						message: String::new(),
						rejected_targets: None,
					},
				))
			} else {
				None
			}
		});

		Ok(SingleSkillRemovalView {
			removal_view,
			holders: self.holders_view(),
			prune: self.prune.clone(),
			code,
		})
	}
}

fn scope_to_word(scope: ResourceScope) -> &'static str {
	match scope {
		ResourceScope::GlobalOnly => "global",
		ResourceScope::ProjectOnly => "project",
		ResourceScope::Both => "global+project",
	}
}

/// Scan every agent in the roster to discover which agents currently hold `name`
/// in the given scope. Fail-closed: an unreadable directory counts as a holder
/// and records the agent in `unreadable`.
pub fn find_skill_holders(
	name: &str,
	scope: ResourceScope,
	project_root: Option<&Path>,
) -> (Vec<AgentType>, Vec<&'static str>) {
	find_skill_holders_crediting(name, scope, project_root, &[])
}

/// Scan every agent in the roster to discover which agents hold `name` in the
/// given scope, crediting planned or executed deletions.
pub fn find_skill_holders_crediting(
	name: &str,
	scope: ResourceScope,
	project_root: Option<&Path>,
	deleting: &[PathBuf],
) -> (Vec<AgentType>, Vec<&'static str>) {
	let doomed: Vec<PathBuf> = deleting
		.iter()
		.map(|path| crate::skills::removal::entry_identity(path))
		.collect();
	let mut holders = Vec::new();
	let mut unreadable = Vec::new();
	for descriptor in registry::iter_all() {
		let Ok(agent) = descriptor.id.parse::<AgentType>() else {
			continue;
		};
		// Through the ADAPTER, never the descriptor: the skills-path test
		// override lives only there, and bypassing it would answer about the
		// developer's real home instead of the fixture.
		let dirs =
			crate::create_adapter(agent).get_skills_paths(project_root, scope);
		match crate::skills::discovery::load_skills_from_dirs(&dirs) {
			Ok(skills) => {
				let is_holder = if deleting.is_empty() {
					skills.iter().any(|s| s.name == name)
				} else {
					skills.iter().filter(|s| s.name == name).any(|skill| {
						if let Some(entry) =
							crate::skills::removal::discovered_entry_dir(skill)
						{
							let id =
								crate::skills::removal::entry_identity(&entry);
							let resolved =
								crate::skills::linker::classify::canonicalize_lenient(
									&entry,
								);
							!doomed.iter().any(|d| {
								id.starts_with(d) || resolved.starts_with(d)
							})
						} else {
							true
						}
					})
				};
				if is_holder {
					holders.push(agent);
				}
			}
			Err(error) => {
				log::warn!(
					"cannot read agent '{}' skills, counting it as a holder \
					 of '{name}': {error}",
					descriptor.id
				);
				unreadable.push(descriptor.id);
				holders.push(agent);
			}
		}
	}
	(holders, unreadable)
}

/// Query holders of a skill in the given scope, partitioned into managed and unmanaged.
pub fn get_skill_holders(
	name: &str,
	scope: ResourceScope,
	project_root: Option<&Path>,
) -> SkillHoldersView {
	let (holders, _) = find_skill_holders(name, scope, project_root);
	SkillHoldersView::from_agents(holders)
}

/// Find in-scope agents outside `excluding` reading the kept path `path`, partitioned by managed status.
pub fn find_readers_of_kept_path(
	path: &Path,
	scope: ResourceScope,
	project_root: Option<&Path>,
	excluding: &[AgentType],
) -> Vec<crate::errors::RejectedTargetReader> {
	crate::skills::removal::readers_outside(
		path,
		scope,
		project_root,
		excluding,
		true,
	)
	.into_iter()
	.map(|id| crate::errors::RejectedTargetReader {
		agent: id.to_string(),
		managed: crate::agent_settings::is_managed(id),
	})
	.collect()
}

/// Build [`RejectedTarget`]s for a set of rejected agents, sharing the populated `readers`.
/// Readers come from the first kept path only.
pub fn build_rejected_targets(
	agents: &[AgentType],
	reason: &str,
	kind: Option<&str>,
	path: Option<&Path>,
	scope: ResourceScope,
	project_root: Option<&Path>,
	excluding: &[AgentType],
) -> Vec<crate::errors::RejectedTarget> {
	let readers = if kind == Some("shared") {
		path.map(|p| {
			find_readers_of_kept_path(p, scope, project_root, excluding)
		})
	} else {
		None
	};
	agents
		.iter()
		.map(|agent| crate::errors::RejectedTarget {
			agent: agent.as_str().to_string(),
			reason: reason.to_string(),
			kind: kind.map(ToString::to_string),
			path: path.map(|p| p.display().to_string()),
			readers: readers.clone(),
		})
		.collect()
}

/// Shared slots must go first: a private Referrer cannot be revoked while
/// the same agent still reads the shared slot this batch is removing. Reader
/// count comes from `slot_reader_count` (full roster); never re-derive slot
/// sharing here.
/// The sort key counts the full roster (slot_reader_count) because slot
/// sharing is structural; filtering disabled agents ties shared and private
/// slots and can put a private row first, which preflight then refuses.
/// See docs/history/core-transfer.md#seventh-spelling-of-slot-sharing
/// and docs/history/core-removal.md#reconcile-delete-order-needs-the-full-roster
pub fn shared_first_order(
	agents: &[AgentType],
	scope: ResourceScope,
	project_root: Option<&Path>,
) -> Vec<AgentType> {
	let mut execution_order = agents.to_vec();
	execution_order.sort_by_cached_key(|agent| {
		let readers = crate::create_adapter(*agent)
			.target_skills_dir(project_root, scope)
			.map(|dir| {
				crate::skills::removal::slot_reader_count(
					&dir,
					scope,
					project_root,
				)
			})
			.unwrap_or(0);
		std::cmp::Reverse(readers)
	});
	execution_order
}

fn create_manager(
	agent: AgentType,
	scope: ResourceScope,
	project_root: Option<&Path>,
) -> ConfigManager {
	let adapter = crate::create_adapter(agent);
	let global = scope != ResourceScope::ProjectOnly;
	ConfigManager::with_scope(adapter, global, project_root, scope)
}

#[cfg(test)]
pub(crate) static COMMIT_PREFLIGHT_HOOK: std::sync::Mutex<
	Option<std::sync::mpsc::Sender<()>>,
> = std::sync::Mutex::new(None);

fn check_unreadable_exhaustive(
	name: &str,
	is_exhaustive: bool,
	unreadable: &[&'static str],
	scope: ResourceScope,
	project_root: Option<&Path>,
) -> Result<()> {
	if is_exhaustive && !unreadable.is_empty() {
		let unreadable_agents: Vec<String> = unreadable
			.iter()
			.map(|&id| {
				if let Ok(agent) = id.parse::<AgentType>() {
					let dirs = crate::create_adapter(agent)
						.get_skills_paths(project_root, scope);
					let failing_dirs: Vec<PathBuf> = dirs
						.iter()
						.filter(|d| {
							matches!(
								std::fs::read_dir(d),
								Err(e) if e.kind() != std::io::ErrorKind::NotFound
							)
						})
						.cloned()
						.collect();
					let reported_dirs = if failing_dirs.is_empty() {
						&dirs
					} else {
						&failing_dirs
					};
					let dirs_str = reported_dirs
						.iter()
						.map(|d| d.display().to_string())
						.collect::<Vec<_>>()
						.join(", ");
					if dirs_str.is_empty() {
						id.to_string()
					} else {
						format!("{id} ({dirs_str})")
					}
				} else {
					id.to_string()
				}
			})
			.collect();
		return Err(ConfigError::InvalidConfig(format!(
			"cannot decide whether removing '{name}' leaves the shared \
			 .aghub master unread: agent(s) '{}' could not be read \
			 (skills directory unreadable), and an agent aghub cannot read may \
			 still be holding it — naming it in --remove cannot authorize a \
			 collection this run is unable to carry out on it. Fix or remove \
			 those configs, then re-run.",
			unreadable_agents.join("', '")
		)));
	}
	Ok(())
}

#[derive(Debug, Clone)]
struct PreflightVerdict {
	agent: AgentType,
	verdict: Verdict,
	still_read_from: Vec<PathBuf>,
	paths: Vec<PathBuf>,
	skipped: Vec<PathBuf>,
	outcome: crate::dto::RemovalKind,
	needs_confirm: bool,
}

impl PreflightVerdict {
	fn to_row(&self) -> SkillRemovalRow {
		SkillRemovalRow {
			agent: self.agent,
			verdict: self.verdict.clone(),
			outcome: self.outcome,
			error: None,
			typed_error: None,
			is_load_error: false,
			still_read_from: self.still_read_from.clone(),
			paths: self.paths.clone(),
			skipped: self.skipped.clone(),
			executed: false,
			needs_confirm: self.needs_confirm,
		}
	}
}

/// Single core entry point for skill deletion across multiple agents.
pub fn remove_skill_batch(
	request: &SkillRemovalRequest,
) -> Result<SkillRemovalResponse> {
	match &request.target {
		SkillRemovalTarget::ByPath(raw_path) => {
			remove_skill_by_path(request, raw_path)
		}
		SkillRemovalTarget::ByName(name) => remove_skill_by_name(request, name),
	}
}

fn remove_skill_by_path(
	request: &SkillRemovalRequest,
	raw_path: &Path,
) -> Result<SkillRemovalResponse> {
	if request.agents.is_empty() {
		return Err(ConfigError::InvalidConfig(
			"No valid agent was provided".to_string(),
		));
	}

	if request.scope == ResourceScope::ProjectOnly
		&& request.project_root.is_none()
	{
		return Err(ConfigError::InvalidConfig(
			"project_root is required when scope is 'project'".to_string(),
		));
	}

	let target_agents = request.agents.clone();
	let agent_dirs: Vec<PathBuf> = target_agents
		.iter()
		.flat_map(|agent| {
			crate::create_adapter(*agent).get_skills_paths(
				request.project_root.as_deref(),
				request.scope,
			)
		})
		.collect();

	let skill_path = expand_tilde_path(raw_path);

	// See docs/history/api.md#delete-by-path-parent-dir-rule
	if skill_path
		.components()
		.any(|c| c == std::path::Component::ParentDir)
	{
		let safe = agent_dirs.iter().any(|dir| {
			skill_path.strip_prefix(dir).is_ok_and(|rest| {
				!rest
					.components()
					.any(|c| c == std::path::Component::ParentDir)
			})
		});
		if !safe {
			return Err(ConfigError::InvalidConfig(
				"Refusing to delete: source_path must not contain '..' under the agent skills directories"
					.to_string(),
			));
		}
	}

	let skill_dir = by_path_skill_dir(&skill_path)
		.map_err(|e| ConfigError::InvalidConfig(e.to_string()))?;

	let roots =
		allowed_skill_roots(&agent_dirs, request.project_root.as_deref());

	if assert_strictly_contained(&skill_dir, &roots).is_none() {
		return Err(ConfigError::InvalidConfig(
			"Refusing to delete: resolved path is not strictly inside an allow-listed skills root"
				.to_string(),
		));
	}

	if !skill_dir.exists() {
		let rows = target_agents
			.into_iter()
			.map(SkillRemovalRow::absent)
			.collect();
		return Ok(SkillRemovalResponse {
			rows,
			prune: PruneStatus::NotRun,
			keepers: Vec::new(),
			unreadable: Vec::new(),
			master_reclaimed: false,
			would_reclaim_master: false,
		});
	}

	if let Some(ref plugin_name) = request.plugin_owner {
		return Err(ConfigError::InvalidConfig(format!(
			"Cannot delete plugin-managed skill from plugin '{plugin_name}'"
		)));
	}

	for agent in &target_agents {
		let paths = crate::create_adapter(*agent)
			.get_skills_paths(request.project_root.as_deref(), request.scope);
		if !paths
			.iter()
			.any(|sp| skill_dir.starts_with(sp) || &skill_dir == sp)
		{
			let valid_paths: Vec<String> =
				paths.iter().map(|p| p.display().to_string()).collect();
			return Err(ConfigError::InvalidConfig(format!(
				"Path '{}' is not in agent's skills directories: {}",
				skill_dir.display(),
				valid_paths.join(", ")
			)));
		}
	}

	let _mutation_guard = if !request.dry_run {
		Some(
			crate::skills::lock::mutation_guard(
				"delete skill by path",
				request.scope,
				request.project_root.as_deref(),
			)
			.map_err(ConfigError::Io)?,
		)
	} else {
		None
	};

	// Re-check containment under the lock
	if assert_strictly_contained(&skill_dir, &roots).is_none() {
		return Err(ConfigError::InvalidConfig(
			"Refusing to delete: resolved path is not strictly inside an allow-listed skills root"
				.to_string(),
		));
	}

	let skill_name = by_path_skill_name(&skill_dir)
		.map_err(|e| ConfigError::InvalidConfig(e.to_string()))?;

	let first_agent = target_agents[0];

	let mut manager = create_manager(
		first_agent,
		request.scope,
		request.project_root.as_deref(),
	);
	if let Err(error) = manager.load() {
		return Err(ConfigError::InvalidConfig(format!(
			"Failed to load agent skills: {error}"
		)));
	}

	let path_is_link = crate::skills::linker::Linker::is_link(&skill_dir);
	let canonical_layout = manager
		.get_skill(&skill_name)
		.and_then(|skill| skill.canonical_path.as_ref())
		.is_some()
		|| path_is_link;

	let outcome = if !canonical_layout {
		let all_in_scope = agent_skill_dirs_in_scope(
			request.scope,
			request.project_root.as_deref(),
		);
		let unmanaged = unmanaged_skill_dirs(
			&all_in_scope,
			request.project_root.as_deref(),
			&target_agents,
		);
		let safe = skill::sanitize::sanitize_name(&skill_name);
		let mut skill = manager
			.get_skill(&skill_name)
			.cloned()
			.unwrap_or_else(|| crate::models::Skill::new(&skill_name));
		skill.source_path = Some(skill_dir.to_string_lossy().to_string());

		let mut plan = plan_copy_removal(
			&skill,
			&safe,
			&all_in_scope,
			&unmanaged,
			&roots,
			request.project_root.as_deref(),
			request.scope,
			false,
			&target_agents,
		);

		let read_dirs = crate::create_adapter(first_agent)
			.get_skills_paths(request.project_root.as_deref(), request.scope);

		let verdict = evaluate_removal_verdict(
			&mut plan,
			&skill_name,
			&read_dirs,
			&[],
			&all_in_scope,
			request.scope,
			request.project_root.as_deref(),
			false,
			&target_agents,
		);

		if !request.dry_run {
			if let Verdict::Refused {
				ref reason,
				ref kind,
				ref path,
			} = verdict
			{
				let rejected_targets = build_rejected_targets(
					&target_agents,
					reason,
					Some(kind),
					path.as_deref(),
					request.scope,
					request.project_root.as_deref(),
					&target_agents,
				);
				return Err(ConfigError::unsupported_operation_with_targets(
					"remove for this agent alone",
					reason,
					crate::create_adapter(first_agent).name(),
					Some(rejected_targets),
				));
			}
		}

		if matches!(verdict, Verdict::Kept { .. }) || request.dry_run {
			RemovalOutcome::preview(
				plan,
				verdict,
				request.scope,
				request.project_root.as_deref(),
				&skill_name,
			)?
		} else {
			RemovalOutcome::commit(
				plan,
				&roots,
				request.scope,
				request.project_root.as_deref(),
				&skill_name,
			)?
		}
	} else {
		// By-path removes the targeted entry only; all_agents is ignored and stays false.
		manager.remove_skill_planned_at_dir_for_agents(
			&skill_name,
			&skill_dir,
			false,
			request.dry_run,
			!request.dry_run,
			&target_agents,
		)?
	};

	let rows: Vec<SkillRemovalRow> = target_agents
		.into_iter()
		.map(|agent| {
			let mut row =
				SkillRemovalRow::from_outcome(agent, &outcome, request.dry_run);
			if let Verdict::Refused { ref reason, .. } = outcome.verdict {
				row.error = Some(reason.clone());
			}
			row
		})
		.collect();

	let (keepers, unreadable) = if request.dry_run {
		let mut planned_deletions: Vec<PathBuf> = Vec::new();
		for row in &rows {
			for p in &row.paths {
				if !planned_deletions.contains(p) {
					planned_deletions.push(p.clone());
				}
			}
		}
		find_skill_holders_crediting(
			&skill_name,
			request.scope,
			request.project_root.as_deref(),
			&planned_deletions,
		)
	} else {
		find_skill_holders(
			&skill_name,
			request.scope,
			request.project_root.as_deref(),
		)
	};

	Ok(SkillRemovalResponse {
		rows,
		prune: outcome.prune,
		keepers,
		unreadable,
		master_reclaimed: false,
		would_reclaim_master: false,
	})
}

fn remove_skill_by_name(
	request: &SkillRemovalRequest,
	name: &str,
) -> Result<SkillRemovalResponse> {
	let target_agents = request.agents.clone();

	let (holders, unreadable) = if request.keeps_master {
		(Vec::new(), Vec::new())
	} else {
		find_skill_holders(name, request.scope, request.project_root.as_deref())
	};

	let target_agents: Vec<AgentType> = if request.agents.is_empty()
		&& request.all_agents
	{
		holders
			.iter()
			.filter(|agent| crate::agent_settings::is_managed(agent.as_str()))
			.copied()
			.collect()
	} else {
		target_agents
	};

	let is_exhaustive = !request.keeps_master
		&& (request.all_agents
			|| (!holders.is_empty()
				&& holders.iter().all(|held| target_agents.contains(held))));

	check_unreadable_exhaustive(
		name,
		is_exhaustive,
		&unreadable,
		request.scope,
		request.project_root.as_deref(),
	)?;

	if target_agents.is_empty() {
		return Ok(SkillRemovalResponse {
			rows: Vec::new(),
			prune: PruneStatus::NotRun,
			keepers: holders,
			unreadable,
			master_reclaimed: false,
			would_reclaim_master: false,
		});
	}

	let execution_order = shared_first_order(
		&target_agents,
		request.scope,
		request.project_root.as_deref(),
	);

	// Whole-batch dry-run preflight
	// Prior-row credit applies only to non-exhaustive runs; an exhaustive run
	// removes the Master as part of the batch itself.
	let can_credit_prior = !is_exhaustive && unreadable.is_empty();
	let mut accumulated_deletions = if can_credit_prior && request.dry_run {
		request.prior_removed_paths.clone()
	} else {
		Vec::new()
	};
	let mut preflight_verdicts: Vec<PreflightVerdict> = Vec::new();
	let mut preflight_failures: Vec<(AgentType, Arc<ConfigError>)> = Vec::new();
	let mut preflight_load_failures: Vec<(AgentType, Arc<ConfigError>)> =
		Vec::new();

	for &agent in &execution_order {
		let descriptor = registry::get(agent);
		if !descriptor.supports_skill_scope(request.scope) {
			let scope_word = scope_to_word(request.scope);
			let reason = format!("no {} skill config", scope_word);
			preflight_failures.push((
				agent,
				Arc::new(ConfigError::unsupported_operation(
					"remove for this agent alone",
					&reason,
					agent.as_str(),
				)),
			));
			continue;
		}

		let mut manager = create_manager(
			agent,
			request.scope,
			request.project_root.as_deref(),
		);
		if let Err(e) = manager.load() {
			preflight_load_failures.push((agent, Arc::new(e)));
			continue;
		}

		let is_agent_exhaustive = is_exhaustive && holders.contains(&agent);
		let plan_result = manager.remove_skill_planned_for_agents_with_prior(
			name,
			is_agent_exhaustive || request.all_agents,
			true, // dry_run
			true, // confirm
			&target_agents,
			&accumulated_deletions,
		);

		match plan_result {
			Ok(outcome) => {
				let kind = crate::dto::removal_kind_from_outcome(
					&outcome,
					request.dry_run,
				);
				let still_read_from = still_read_paths(&outcome);
				let needs_confirm =
					if is_agent_exhaustive && !request.all_agents {
						manager
							.remove_skill_planned_for_agents_with_prior(
								name,
								request.all_agents,
								true, // dry_run
								true, // confirm
								&target_agents,
								&accumulated_deletions,
							)
							.map(|o| o.plan.needs_confirm)
							.unwrap_or(outcome.plan.needs_confirm)
					} else {
						outcome.plan.needs_confirm
					};
				preflight_verdicts.push(PreflightVerdict {
					agent,
					verdict: outcome.verdict.clone(),
					still_read_from,
					paths: outcome.plan.paths.clone(),
					skipped: outcome.plan.skipped.clone(),
					outcome: kind,
					needs_confirm,
				});
				if let Verdict::Refused {
					ref reason,
					ref kind,
					ref path,
				} = outcome.verdict
				{
					let op = if request.all_agents {
						"remove from every agent"
					} else {
						"remove for this agent alone"
					};
					let rejected_target = build_rejected_targets(
						&[agent],
						reason,
						Some(kind),
						path.as_deref(),
						request.scope,
						request.project_root.as_deref(),
						&request.agents,
					)
					.remove(0);
					preflight_failures.push((
						agent,
						Arc::new(
							ConfigError::unsupported_operation_with_targets(
								op,
								reason,
								agent.as_str(),
								Some(vec![rejected_target]),
							),
						),
					));
				} else if can_credit_prior {
					accumulated_deletions
						.extend(outcome.plan.paths.iter().cloned());
				}
			}
			Err(ConfigError::ResourceNotFound { .. }) => {
				let outcome = manager.skill_noop_outcome(name);
				let kind = crate::dto::removal_kind_from_outcome(
					&outcome,
					request.dry_run,
				);
				let still_read_from = still_read_paths(&outcome);
				preflight_verdicts.push(PreflightVerdict {
					agent,
					verdict: outcome.verdict,
					still_read_from,
					paths: outcome.plan.paths,
					skipped: outcome.plan.skipped,
					outcome: kind,
					needs_confirm: outcome.plan.needs_confirm,
				});
			}
			Err(err) => {
				preflight_failures.push((agent, Arc::new(err)));
			}
		}
	}

	// Preview (dry-run): does not acquire write lock, returns verdicts.
	if request.dry_run {
		let master_p = crate::skills::shape::master_path(
			request.scope,
			request.project_root.as_deref(),
			name,
		);
		let had_master = master_p.as_ref().map(|p| p.exists()).unwrap_or(false);
		let scope_str = scope_to_word(request.scope);
		let rows: Vec<SkillRemovalRow> = target_agents
			.iter()
			.map(|agent| {
				let verdict_entry =
					preflight_verdicts.iter().find(|e| e.agent == *agent);
				let mut row = verdict_entry
					.map(|e| e.to_row())
					.unwrap_or_else(|| SkillRemovalRow::absent(*agent));
				let load_err =
					preflight_load_failures.iter().find(|(a, _)| *a == *agent);
				let plan_err =
					preflight_failures.iter().find(|(a, _)| *a == *agent);
				let err_entry = load_err.or(plan_err);
				row.is_load_error = load_err.is_some();
				row.typed_error = err_entry.map(|(_, err)| Arc::clone(err));
				row.error = err_entry.map(|(_, err)| {
					format!("delete {} ({scope_str}): {err}", agent.as_str())
				});
				row
			})
			.collect();
		let shared_master_kept = request.keeps_master
			|| rows.iter().any(|r| r.verdict.shared_master_kept());
		let prune = if shared_master_kept {
			PruneStatus::NotRun
		} else {
			let mut union_paths: Vec<PathBuf> = Vec::new();
			for row in &rows {
				for p in &row.paths {
					if !union_paths.contains(p) {
						union_paths.push(p.clone());
					}
				}
			}
			crate::skills::prune::preview_prune_for_removal(
				request.scope,
				request.project_root.as_deref(),
				&union_paths,
			)
		};
		let mut planned_deletions: Vec<PathBuf> = Vec::new();
		for row in &rows {
			for p in &row.paths {
				if !planned_deletions.contains(p) {
					planned_deletions.push(p.clone());
				}
			}
		}
		let (keepers, _) = find_skill_holders_crediting(
			name,
			request.scope,
			request.project_root.as_deref(),
			&planned_deletions,
		);
		return Ok(SkillRemovalResponse {
			rows,
			prune,
			keepers,
			unreadable,
			master_reclaimed: false,
			would_reclaim_master: is_exhaustive && had_master,
		});
	}

	// Commit path: if predictable failures occurred, reject the WHOLE batch before any write!
	if !preflight_failures.is_empty() {
		let all_unsupported = preflight_failures.iter().all(|(_, err)| {
			matches!(**err, ConfigError::UnsupportedOperation { .. })
		});
		let rejected_targets: Vec<crate::errors::RejectedTarget> =
			preflight_failures
				.iter()
				.map(|(agent, err)| {
					if let Some(targets) = err.rejected_targets() {
						if let Some(target) = targets.first() {
							return crate::errors::RejectedTarget {
								agent: agent.as_str().to_string(),
								reason: err.to_string(),
								kind: target.kind.clone(),
								path: target.path.clone(),
								readers: target.readers.clone(),
							};
						}
					}
					crate::errors::RejectedTarget {
						agent: agent.as_str().to_string(),
						reason: err.to_string(),
						kind: None,
						path: None,
						readers: None,
					}
				})
				.collect();
		let scope_str = scope_to_word(request.scope);
		let failures_str = preflight_failures
			.into_iter()
			.map(|(agent, err)| {
				format!("delete {} ({scope_str}): {err}", agent.as_str())
			})
			.collect::<Vec<_>>()
			.join("; ");
		let message = format!(
			"skill removal preflight failed; no removal was performed (nothing was written): {failures_str}"
		);
		if all_unsupported {
			return Err(ConfigError::UnsupportedOperation {
				message,
				rejected_targets: Some(rejected_targets),
			});
		} else {
			return Err(ConfigError::InvalidConfigWithTargets {
				message,
				rejected_targets: Some(rejected_targets),
			});
		}
	}

	#[cfg(test)]
	if let Some(ref tx) = *COMMIT_PREFLIGHT_HOOK.lock().unwrap() {
		let _ = tx.send(());
	}

	// Commit re-plans inside the mutation write lock
	let _mutation_guard = crate::skills::lock::mutation_guard(
		"remove skill batch",
		request.scope,
		request.project_root.as_deref(),
	)
	.map_err(ConfigError::Io)?;

	let master_p = crate::skills::shape::master_path(
		request.scope,
		request.project_root.as_deref(),
		name,
	);
	let had_master = master_p.as_ref().map(|p| p.exists()).unwrap_or(false);

	let (holders, unreadable) = if request.keeps_master {
		(Vec::new(), Vec::new())
	} else {
		find_skill_holders(name, request.scope, request.project_root.as_deref())
	};

	let in_lock_target_agents: Vec<AgentType> = if request.agents.is_empty()
		&& request.all_agents
	{
		holders
			.iter()
			.filter(|agent| crate::agent_settings::is_managed(agent.as_str()))
			.copied()
			.collect()
	} else {
		target_agents
	};

	let execution_order = shared_first_order(
		&in_lock_target_agents,
		request.scope,
		request.project_root.as_deref(),
	);

	let is_exhaustive = !request.keeps_master
		&& (request.all_agents
			|| (!holders.is_empty()
				&& holders
					.iter()
					.all(|held| in_lock_target_agents.contains(held))));

	check_unreadable_exhaustive(
		name,
		is_exhaustive,
		&unreadable,
		request.scope,
		request.project_root.as_deref(),
	)?;

	let mut credits =
		RemovalCredits::new(in_lock_target_agents.clone(), |agent| {
			let mut mgr = create_manager(
				*agent,
				request.scope,
				request.project_root.as_deref(),
			);
			if mgr.load().is_err() {
				return None;
			}
			mgr.get_skill(name)
				.and_then(crate::skills::removal::skill_root)
				.map(Backing::of)
		});

	let mut execution_results: Vec<SkillRemovalRow> = Vec::new();
	let mut batch_prune = PruneStatus::NotRun;
	let mut all_pruned_keys: Vec<String> = Vec::new();

	for &agent in &execution_order {
		let mut manager = create_manager(
			agent,
			request.scope,
			request.project_root.as_deref(),
		);
		if let Err(e) = manager.load() {
			execution_results.push(SkillRemovalRow::error_row(agent, e, true));
			continue;
		}

		let is_agent_exhaustive = is_exhaustive && holders.contains(&agent);
		let caller_needs_confirm = if is_agent_exhaustive && !request.all_agents
		{
			manager
				.remove_skill_planned_for_agents_with_prior(
					name,
					request.all_agents,
					true, // dry_run
					true, // confirm
					&in_lock_target_agents,
					&[],
				)
				.map(|o| o.plan.needs_confirm)
				.ok()
		} else {
			None
		};
		let res = manager.remove_skill_planned_for_agents_with_prior(
			name,
			is_agent_exhaustive || request.all_agents,
			false, // dry_run
			true,  // confirm
			&in_lock_target_agents,
			&[],
		);

		match res {
			Ok(outcome) => {
				if outcome.executed {
					credits.credit(agent);
					match &outcome.prune {
						PruneStatus::Failed { reason, pruned } => {
							all_pruned_keys.extend(pruned.iter().cloned());
							batch_prune = PruneStatus::Failed {
								reason: reason.clone(),
								pruned: all_pruned_keys.clone(),
							};
						}
						PruneStatus::Pruned(keys) => {
							for key in keys {
								if !all_pruned_keys.contains(key) {
									all_pruned_keys.push(key.clone());
								}
							}
							if !matches!(
								batch_prune,
								PruneStatus::Failed { .. }
							) {
								batch_prune = PruneStatus::Pruned(
									all_pruned_keys.clone(),
								);
							} else if let PruneStatus::Failed {
								ref reason,
								..
							} = batch_prune
							{
								batch_prune = PruneStatus::Failed {
									reason: reason.clone(),
									pruned: all_pruned_keys.clone(),
								};
							}
						}
						_ => {}
					}
				}
				let mut row = SkillRemovalRow::from_outcome(
					agent,
					&outcome,
					request.dry_run,
				);
				if let Some(nc) = caller_needs_confirm {
					row.needs_confirm = nc;
				}
				if outcome.failed_paths.is_empty() {
					execution_results.push(row);
				} else {
					let err = format!(
						"failed to remove skill '{}' for agent '{}': {} path(s) could not be deleted: {}",
						name,
						agent.as_str(),
						outcome.failed_paths.len(),
						outcome
							.failed_paths
							.iter()
							.map(|p| p.display().to_string())
							.collect::<Vec<_>>()
							.join(", ")
					);
					row.verdict = Verdict::Partial;
					row.outcome = crate::dto::RemovalKind::Partial;
					row.error = Some(err.clone());
					row.typed_error =
						Some(Arc::new(ConfigError::InvalidConfig(err)));
					execution_results.push(row);
				}
			}
			Err(ConfigError::ResourceNotFound { .. })
				if credits.already_taken(&agent) =>
			{
				execution_results.push(SkillRemovalRow::noop(
					agent,
					Verdict::Removed,
					crate::dto::RemovalKind::Removed,
				));
			}
			Err(err @ ConfigError::ResourceNotFound { .. }) => {
				let outcome = manager.skill_noop_outcome(name);
				let mut row = SkillRemovalRow::from_outcome(
					agent,
					&outcome,
					request.dry_run,
				);
				row.error = Some(err.to_string());
				row.typed_error = Some(Arc::new(err));
				execution_results.push(row);
			}
			Err(err) => {
				execution_results
					.push(SkillRemovalRow::error_row(agent, err, false));
			}
		}
	}

	let rows: Vec<SkillRemovalRow> = in_lock_target_agents
		.iter()
		.map(|agent| {
			execution_results
				.iter()
				.find(|r| r.agent == *agent)
				.cloned()
				.unwrap_or_else(|| SkillRemovalRow::absent(*agent))
		})
		.collect();

	if !request.keeps_master
		&& !rows.is_empty()
		&& rows.iter().all(|r| {
			matches!(r.verdict, Verdict::Absent | Verdict::LockOnly)
				&& r.typed_error.as_ref().is_none_or(|err| {
					matches!(**err, ConfigError::ResourceNotFound { .. })
				})
		}) {
		batch_prune = crate::skills::prune::prune_lock_for_scope(
			request.scope,
			request.project_root.as_deref(),
		);
	}

	let master_reclaimed = is_exhaustive
		&& had_master
		&& master_p.as_ref().map(|p| !p.exists()).unwrap_or(false)
		&& execution_results
			.iter()
			.all(|r| r.verdict != Verdict::Partial);
	let (keepers, _) = find_skill_holders(
		name,
		request.scope,
		request.project_root.as_deref(),
	);

	Ok(SkillRemovalResponse {
		rows,
		prune: batch_prune,
		keepers,
		unreadable,
		master_reclaimed,
		would_reclaim_master: false,
	})
}

#[cfg(test)]
mod tests;
