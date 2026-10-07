//! Single core entry for skill deletion by name across multiple agents.
//!
//! Handles shared-first ordering, whole-batch dry-run preflight, prior-row credit,
//! sibling credit, lock-free preview vs locked commit, Master GC, and lock pruning.

use std::path::{Path, PathBuf};

use crate::errors::{ConfigError, Result};
use crate::models::{AgentType, ResourceScope};
use crate::registry;
use crate::skills::removal::{PruneStatus, Verdict};
use crate::ConfigManager;

/// Target for a batch skill removal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillRemovalTarget {
	ByName(String),
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
	pub prior_removed_paths: Vec<PathBuf>,
}

/// Outcome row for a single agent in a batch skill removal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillRemovalRow {
	pub agent: AgentType,
	pub verdict: Verdict,
	pub error: Option<String>,
}

/// Response returned by the batch skill removal entry point.
#[derive(Debug, Clone)]
pub struct SkillRemovalResponse {
	pub rows: Vec<SkillRemovalRow>,
	pub prune: PruneStatus,
}

/// Scan every agent in the roster to discover which agents currently hold `name`
/// in the given scope. Fail-closed: an unreadable directory counts as a holder
/// and records the agent in `unreadable`.
pub fn find_skill_holders(
	name: &str,
	scope: ResourceScope,
	project_root: Option<&Path>,
) -> (Vec<AgentType>, Vec<&'static str>) {
	let mut holders = Vec::new();
	let mut unreadable = Vec::new();
	for descriptor in registry::iter_all() {
		let Ok(agent) = descriptor.id.parse::<AgentType>() else {
			continue;
		};
		let dirs =
			crate::create_adapter(agent).get_skills_paths(project_root, scope);
		match crate::skills::discovery::load_skills_from_dirs(&dirs) {
			Ok(skills) => {
				if skills.iter().any(|s| s.name == name) {
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Backing {
	node: Option<(u64, u64)>,
	path: PathBuf,
}

impl Backing {
	pub fn of(path: PathBuf) -> Self {
		let path = resolve_through_links(path);
		let node = node_id(&path);
		Self { node, path }
	}

	pub fn is(&self, other: &Self) -> bool {
		match (self.node, other.node) {
			(Some(a), Some(b)) => a == b,
			(None, None) => self.path == other.path,
			_ => false,
		}
	}
}

#[cfg(unix)]
fn node_id(path: &Path) -> Option<(u64, u64)> {
	use std::os::unix::fs::MetadataExt;
	let meta = std::fs::metadata(path).ok()?;
	Some((meta.dev(), meta.ino()))
}

#[cfg(not(unix))]
fn node_id(_path: &Path) -> Option<(u64, u64)> {
	None
}

fn resolve_through_links(path: PathBuf) -> PathBuf {
	if let Ok(real) = std::fs::canonicalize(&path) {
		return real;
	}
	match (path.parent(), path.file_name()) {
		(Some(parent), Some(name)) => std::fs::canonicalize(parent)
			.map(|real| real.join(name))
			.unwrap_or(path),
		_ => path,
	}
}

pub(crate) struct RemovalCredits {
	resolved: Vec<(AgentType, Backing)>,
	credited: Vec<AgentType>,
}

impl RemovalCredits {
	pub fn new<F>(agents: &[AgentType], mut resolve_backing: F) -> Self
	where
		F: FnMut(AgentType) -> Option<Backing>,
	{
		let resolved = agents
			.iter()
			.filter_map(|&agent| resolve_backing(agent).map(|b| (agent, b)))
			.collect();
		Self {
			resolved,
			credited: Vec::new(),
		}
	}

	pub fn backing_of(&self, agent: AgentType) -> Option<&Backing> {
		self.resolved
			.iter()
			.find(|(candidate, _)| *candidate == agent)
			.map(|(_, backing)| backing)
	}

	pub fn credit(&mut self, agent: AgentType) {
		if !self.credited.contains(&agent) {
			self.credited.push(agent);
		}
	}

	pub fn already_taken(&self, agent: AgentType) -> bool {
		let Some(mine) = self.backing_of(agent) else {
			return false;
		};
		self.credited.iter().any(|credited| {
			self.backing_of(*credited).is_some_and(|took| took.is(mine))
		})
	}
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

/// Single core entry point for skill deletion by name across multiple agents.
pub fn remove_skill_batch(
	request: &SkillRemovalRequest,
) -> Result<SkillRemovalResponse> {
	let name = match &request.target {
		SkillRemovalTarget::ByName(name) => name.as_str(),
	};

	let (holders, unreadable) = find_skill_holders(
		name,
		request.scope,
		request.project_root.as_deref(),
	);

	let is_exhaustive = request.all_agents
		|| (!holders.is_empty()
			&& holders.iter().all(|held| request.agents.contains(held)));

	if is_exhaustive && !unreadable.is_empty() {
		return Err(ConfigError::InvalidConfig(format!(
			"cannot decide whether removing '{}' leaves the shared \
			 .aghub master unread: agent(s) '{}' could not be read \
			 (skills directory unreadable), and an agent aghub cannot read may \
			 still be holding it — naming it in --remove cannot authorize a \
			 collection this run is unable to carry out on it. Fix or remove \
			 those configs, then re-run.",
			name,
			unreadable.join("', '")
		)));
	}

	let target_agents: Vec<AgentType> =
		if request.agents.is_empty() && request.all_agents {
			holders.clone()
		} else {
			request.agents.clone()
		};

	if target_agents.is_empty() {
		return Ok(SkillRemovalResponse {
			rows: Vec::new(),
			prune: PruneStatus::NotRun,
		});
	}

	// Shared-first sort keyed by full-roster slot-reader count
	let mut execution_order = target_agents.clone();
	execution_order.sort_by_cached_key(|agent| {
		let readers = crate::create_adapter(*agent)
			.target_skills_dir(request.project_root.as_deref(), request.scope)
			.map(|dir| {
				crate::skills::removal::slot_reader_count(
					&dir,
					request.scope,
					request.project_root.as_deref(),
				)
			})
			.unwrap_or(0);
		std::cmp::Reverse(readers)
	});

	// Whole-batch dry-run preflight
	let can_credit_prior = unreadable.is_empty();
	let mut accumulated_deletions = if can_credit_prior {
		request.prior_removed_paths.clone()
	} else {
		Vec::new()
	};
	let mut preflight_verdicts: Vec<(AgentType, Verdict)> = Vec::new();
	let mut preflight_failures: Vec<(AgentType, ConfigError)> = Vec::new();

	for &agent in &execution_order {
		let descriptor = registry::get(agent);
		if !descriptor.supports_skill_scope(request.scope) {
			let scope_word = match request.scope {
				ResourceScope::GlobalOnly => "global",
				ResourceScope::ProjectOnly => "project",
				ResourceScope::Both => "global+project",
			};
			let reason = format!("no {} skill config", scope_word);
			preflight_failures.push((
				agent,
				ConfigError::unsupported_operation(
					"remove for this agent alone",
					&reason,
					agent.as_str(),
				),
			));
			continue;
		}

		let mut manager = create_manager(
			agent,
			request.scope,
			request.project_root.as_deref(),
		);
		if let Err(e) = manager.load() {
			preflight_failures.push((agent, e));
			continue;
		}

		let plan_result = manager.remove_skill_planned_for_agents_with_prior(
			name,
			is_exhaustive && holders.contains(&agent),
			true, // dry_run
			true, // confirm
			&request.agents,
			&accumulated_deletions,
		);

		match plan_result {
			Ok(outcome) => {
				if let Verdict::Refused { ref reason } = outcome.verdict {
					let op = if request.all_agents {
						"remove from every agent"
					} else {
						"remove for this agent alone"
					};
					preflight_failures.push((
						agent,
						ConfigError::unsupported_operation(
							op,
							reason,
							agent.as_str(),
						),
					));
					preflight_verdicts.push((agent, outcome.verdict));
				} else {
					if can_credit_prior {
						accumulated_deletions
							.extend(outcome.plan.paths.iter().cloned());
					}
					preflight_verdicts.push((agent, outcome.verdict));
				}
			}
			Err(ConfigError::ResourceNotFound { .. }) => {
				let outcome = manager.skill_noop_outcome(name);
				preflight_verdicts.push((agent, outcome.verdict));
			}
			Err(err) => {
				preflight_failures.push((agent, err));
			}
		}
	}

	// Preview (dry-run): does not acquire write lock, returns verdicts.
	if request.dry_run {
		let rows = target_agents
			.iter()
			.map(|agent| {
				let verdict = preflight_verdicts
					.iter()
					.find(|(a, _)| *a == *agent)
					.map(|(_, v)| v.clone())
					.unwrap_or(Verdict::Absent);
				SkillRemovalRow {
					agent: *agent,
					verdict,
					error: None,
				}
			})
			.collect();
		return Ok(SkillRemovalResponse {
			rows,
			prune: PruneStatus::NotRun,
		});
	}

	// Commit path: if predictable failures occurred, reject the WHOLE batch before any write!
	if !preflight_failures.is_empty() {
		let all_unsupported = preflight_failures.iter().all(|(_, err)| {
			matches!(err, ConfigError::UnsupportedOperation(_))
		});
		let scope_str = match request.scope {
			ResourceScope::GlobalOnly => "global",
			ResourceScope::ProjectOnly => "project",
			ResourceScope::Both => "global+project",
		};
		let failures_str = preflight_failures
			.into_iter()
			.map(|(agent, err)| {
				format!("delete {} ({scope_str}): {err}", agent.as_str())
			})
			.collect::<Vec<_>>()
			.join("; ");
		let message = format!(
			"skill removal preflight failed; nothing was written: {failures_str}"
		);
		if all_unsupported {
			return Err(ConfigError::UnsupportedOperation(message));
		} else {
			return Err(ConfigError::InvalidConfig(message));
		}
	}

	// Commit re-plans inside the mutation write lock
	let _mutation_guard = crate::skills::lock::mutation_guard(
		"remove skill batch",
		request.scope,
		request.project_root.as_deref(),
	)
	.map_err(ConfigError::Io)?;

	let mut credits = RemovalCredits::new(&request.agents, |agent| {
		let mut mgr = create_manager(
			agent,
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

	let mut execution_results: Vec<(AgentType, (Verdict, Option<String>))> =
		Vec::new();
	let mut any_executed = false;

	for &agent in &execution_order {
		let mut manager = create_manager(
			agent,
			request.scope,
			request.project_root.as_deref(),
		);
		if let Err(e) = manager.load() {
			execution_results
				.push((agent, (Verdict::Absent, Some(e.to_string()))));
			continue;
		}

		let prior = if can_credit_prior {
			request.prior_removed_paths.as_slice()
		} else {
			&[]
		};
		let res = manager.remove_skill_planned_for_agents_with_prior(
			name,
			is_exhaustive && holders.contains(&agent),
			false, // dry_run
			true,  // confirm
			&request.agents,
			prior,
		);

		match res {
			Ok(outcome) => {
				if outcome.executed {
					credits.credit(agent);
					any_executed = true;
				}
				if outcome.failed_paths.is_empty() {
					execution_results.push((agent, (outcome.verdict, None)));
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
					execution_results
						.push((agent, (Verdict::Partial, Some(err))));
				}
			}
			Err(ConfigError::ResourceNotFound { .. })
				if credits.already_taken(agent) =>
			{
				execution_results.push((agent, (Verdict::Removed, None)));
			}
			Err(ConfigError::ResourceNotFound { .. }) => {
				let outcome = manager.skill_noop_outcome(name);
				execution_results.push((agent, (outcome.verdict, None)));
			}
			Err(err) => {
				execution_results
					.push((agent, (Verdict::Partial, Some(err.to_string()))));
			}
		}
	}

	let prune = if any_executed {
		crate::skills::prune::prune_lock_for_scope(
			request.scope,
			request.project_root.as_deref(),
		)
	} else {
		PruneStatus::NotRun
	};

	let rows = target_agents
		.iter()
		.map(|agent| {
			let (verdict, error) = execution_results
				.iter()
				.find(|(a, _)| *a == *agent)
				.cloned()
				.map(|(_, res)| res)
				.unwrap_or((Verdict::Absent, None));
			SkillRemovalRow {
				agent: *agent,
				verdict,
				error,
			}
		})
		.collect();

	Ok(SkillRemovalResponse { rows, prune })
}

/// Convenience alias for [`remove_skill_batch`].
pub fn remove_skill(
	request: &SkillRemovalRequest,
) -> Result<SkillRemovalResponse> {
	remove_skill_batch(request)
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::skills::prune::test_lock::env_lock;
	use crate::skills::removal;
	use std::fs;
	use tempfile::tempdir;

	#[cfg(unix)]
	fn setup_shared_fixture(root: &Path, name: &str) {
		let master = root.join(".aghub").join(name);
		fs::create_dir_all(&master).unwrap();
		fs::write(
			master.join("SKILL.md"),
			format!(
				"---\nname: {name}\ndescription: Shared\n---\n\n# {name}\n"
			),
		)
		.unwrap();
		let claude_skills = root.join(".claude/skills");
		fs::create_dir_all(&claude_skills).unwrap();
		std::os::unix::fs::symlink(&master, claude_skills.join(name)).unwrap();
		let shared = root.join(".agents/skills");
		fs::create_dir_all(&shared).unwrap();
		std::os::unix::fs::symlink(&master, shared.join(name)).unwrap();
		for dir in [".opencode", ".cursor", ".pi", ".grok", ".omp"] {
			let slot = root.join(dir).join("skills");
			fs::create_dir_all(&slot).unwrap();
			std::os::unix::fs::symlink(&master, slot.join(name)).unwrap();
		}
	}

	#[cfg(unix)]
	#[test]
	fn test_removal_ordering_independence() {
		let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
		let temp_a = tempdir().unwrap();
		let root_a = temp_a.path();
		setup_shared_fixture(root_a, "notebooklm");

		let temp_b = tempdir().unwrap();
		let root_b = temp_b.path();
		setup_shared_fixture(root_b, "notebooklm");

		// Shared slot readers first
		let order_shared_first = vec![
			AgentType::Codex,
			AgentType::Antigravity,
			AgentType::Gemini,
			AgentType::Cline,
			AgentType::Copilot,
			AgentType::Kimi,
			AgentType::Amp,
			AgentType::Warp,
			AgentType::ZCode,
			AgentType::Dsh,
			AgentType::OpenCode,
			AgentType::Cursor,
			AgentType::Pi,
			AgentType::Grok,
			AgentType::Omp,
		];

		// Private readers first
		let order_private_first = vec![
			AgentType::OpenCode,
			AgentType::Cursor,
			AgentType::Pi,
			AgentType::Grok,
			AgentType::Omp,
			AgentType::Codex,
			AgentType::Antigravity,
			AgentType::Gemini,
			AgentType::Cline,
			AgentType::Copilot,
			AgentType::Kimi,
			AgentType::Amp,
			AgentType::Warp,
			AgentType::ZCode,
			AgentType::Dsh,
		];

		let req_a = SkillRemovalRequest {
			target: SkillRemovalTarget::ByName("notebooklm".to_string()),
			scope: ResourceScope::ProjectOnly,
			project_root: Some(root_a.to_path_buf()),
			agents: order_shared_first.clone(),
			dry_run: false,
			all_agents: false,
			prior_removed_paths: Vec::new(),
		};
		let res_a = remove_skill_batch(&req_a)
			.expect("order_shared_first batch should succeed");

		let req_b = SkillRemovalRequest {
			target: SkillRemovalTarget::ByName("notebooklm".to_string()),
			scope: ResourceScope::ProjectOnly,
			project_root: Some(root_b.to_path_buf()),
			agents: order_private_first.clone(),
			dry_run: false,
			all_agents: false,
			prior_removed_paths: Vec::new(),
		};
		let res_b = remove_skill_batch(&req_b)
			.expect("order_private_first batch should succeed");

		assert_eq!(res_a.rows.len(), 15);
		assert_eq!(res_b.rows.len(), 15);
		for row in &res_a.rows {
			assert_eq!(row.verdict, Verdict::Removed, "agent {:?}", row.agent);
			assert!(row.error.is_none());
		}
		for row in &res_b.rows {
			assert_eq!(row.verdict, Verdict::Removed, "agent {:?}", row.agent);
			assert!(row.error.is_none());
		}

		for agent in &order_shared_first {
			let v_a = res_a.rows.iter().find(|r| r.agent == *agent).unwrap();
			let v_b = res_b.rows.iter().find(|r| r.agent == *agent).unwrap();
			assert_eq!(v_a.verdict, v_b.verdict);
			assert_eq!(v_a.error, v_b.error);
		}

		assert!(root_a.join(".claude/skills/notebooklm").exists());
		assert!(root_b.join(".claude/skills/notebooklm").exists());
		assert!(root_a.join(".aghub/notebooklm").exists());
		assert!(root_b.join(".aghub/notebooklm").exists());
		assert!(!root_a.join(".agents/skills/notebooklm").exists());
		assert!(!root_b.join(".agents/skills/notebooklm").exists());
		assert!(!root_a.join(".opencode/skills/notebooklm").exists());
		assert!(!root_b.join(".opencode/skills/notebooklm").exists());

		for &agent in &order_shared_first {
			let dirs_a = crate::create_adapter(agent)
				.get_skills_paths(Some(root_a), ResourceScope::ProjectOnly);
			let eff_a = removal::read_effect_after(&dirs_a, "notebooklm", &[]);
			assert!(
				eff_a.survivors.is_empty(),
				"agent {:?} in a has survivors",
				agent
			);

			let dirs_b = crate::create_adapter(agent)
				.get_skills_paths(Some(root_b), ResourceScope::ProjectOnly);
			let eff_b = removal::read_effect_after(&dirs_b, "notebooklm", &[]);
			assert!(
				eff_b.survivors.is_empty(),
				"agent {:?} in b has survivors",
				agent
			);
		}
	}

	#[cfg(unix)]
	#[test]
	fn test_prior_row_credit_turns_kept_into_removed() {
		let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
		let temp = tempdir().unwrap();
		let root = temp.path();
		setup_shared_fixture(root, "notebooklm");

		let req_without_credit = SkillRemovalRequest {
			target: SkillRemovalTarget::ByName("notebooklm".to_string()),
			scope: ResourceScope::ProjectOnly,
			project_root: Some(root.to_path_buf()),
			agents: vec![AgentType::OpenCode],
			dry_run: true,
			all_agents: false,
			prior_removed_paths: Vec::new(),
		};
		let res_without = remove_skill_batch(&req_without_credit).unwrap();
		assert!(
			matches!(res_without.rows[0].verdict, Verdict::Refused { .. }),
			"expected Refused without prior credit, got {:?}",
			res_without.rows[0].verdict
		);

		let req_with_credit = SkillRemovalRequest {
			target: SkillRemovalTarget::ByName("notebooklm".to_string()),
			scope: ResourceScope::ProjectOnly,
			project_root: Some(root.to_path_buf()),
			agents: vec![AgentType::OpenCode],
			dry_run: true,
			all_agents: false,
			prior_removed_paths: vec![root.join(".agents/skills/notebooklm")],
		};
		let res_with = remove_skill_batch(&req_with_credit).unwrap();
		assert_eq!(
			res_with.rows[0].verdict,
			Verdict::Removed,
			"prior-row credit must turn row into Removed"
		);
	}

	#[cfg(unix)]
	#[test]
	fn test_whole_batch_preflight_rejection_writes_nothing() {
		let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
		let temp = tempdir().unwrap();
		let root = temp.path();
		setup_shared_fixture(root, "notebooklm");

		let req = SkillRemovalRequest {
			target: SkillRemovalTarget::ByName("notebooklm".to_string()),
			scope: ResourceScope::ProjectOnly,
			project_root: Some(root.to_path_buf()),
			agents: vec![AgentType::Claude, AgentType::OpenCode],
			dry_run: false,
			all_agents: false,
			prior_removed_paths: Vec::new(),
		};

		let err = remove_skill_batch(&req).unwrap_err();
		let err_msg = err.to_string();
		assert!(
			err_msg.contains(
				"skill removal preflight failed; nothing was written"
			),
			"message was: {err_msg}"
		);
		assert!(
			err_msg.contains("opencode"),
			"message must list rejected target: {err_msg}"
		);

		assert!(root.join(".claude/skills/notebooklm").exists());
		assert!(root.join(".agents/skills/notebooklm").exists());
		assert!(root.join(".opencode/skills/notebooklm").exists());
		assert!(root.join(".aghub/notebooklm").exists());
	}

	#[cfg(unix)]
	#[test]
	fn test_disabled_agent_removal_behavior() {
		let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
		let temp = tempdir().unwrap();
		let root = temp.path();

		let master = root.join(".aghub/notebooklm");
		fs::create_dir_all(&master).unwrap();
		fs::write(
			master.join("SKILL.md"),
			"---\nname: notebooklm\ndescription: test\n---\n",
		)
		.unwrap();
		let claude_skills = root.join(".claude/skills");
		fs::create_dir_all(&claude_skills).unwrap();
		std::os::unix::fs::symlink(&master, claude_skills.join("notebooklm"))
			.unwrap();

		let opencode_skills = root.join(".opencode/skills");
		fs::create_dir_all(&opencode_skills).unwrap();
		std::os::unix::fs::symlink(&master, opencode_skills.join("notebooklm"))
			.unwrap();

		let _off = crate::agent_settings::test_override::disable(&["opencode"]);

		let req_unnamed = SkillRemovalRequest {
			target: SkillRemovalTarget::ByName("notebooklm".to_string()),
			scope: ResourceScope::ProjectOnly,
			project_root: Some(root.to_path_buf()),
			agents: vec![AgentType::Claude],
			dry_run: false,
			all_agents: false,
			prior_removed_paths: Vec::new(),
		};
		let res_unnamed = remove_skill_batch(&req_unnamed).unwrap();
		assert_eq!(res_unnamed.rows[0].verdict, Verdict::Removed);

		assert!(!root.join(".claude/skills/notebooklm").exists());
		assert!(root.join(".opencode/skills/notebooklm").exists());
		assert!(root.join(".aghub/notebooklm").exists());

		let req_named = SkillRemovalRequest {
			target: SkillRemovalTarget::ByName("notebooklm".to_string()),
			scope: ResourceScope::ProjectOnly,
			project_root: Some(root.to_path_buf()),
			agents: vec![AgentType::OpenCode],
			dry_run: false,
			all_agents: false,
			prior_removed_paths: Vec::new(),
		};
		let res_named = remove_skill_batch(&req_named).unwrap();
		assert_eq!(res_named.rows[0].verdict, Verdict::Removed);

		assert!(!root.join(".opencode/skills/notebooklm").exists());
		assert!(!root.join(".aghub/notebooklm").exists());
	}

	#[cfg(unix)]
	#[test]
	fn test_master_gc_and_prune_failure_reported_independently() {
		let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
		let temp = tempdir().unwrap();
		let root = temp.path();

		let master = root.join(".aghub/notebooklm");
		fs::create_dir_all(&master).unwrap();
		fs::write(
			master.join("SKILL.md"),
			"---\nname: notebooklm\ndescription: test\n---\n",
		)
		.unwrap();
		let claude_skills = root.join(".claude/skills");
		fs::create_dir_all(&claude_skills).unwrap();
		std::os::unix::fs::symlink(&master, claude_skills.join("notebooklm"))
			.unwrap();

		let cursor_skills = root.join(".cursor/skills");
		fs::create_dir_all(&cursor_skills).unwrap();
		std::os::unix::fs::symlink(&master, cursor_skills.join("notebooklm"))
			.unwrap();

		let req1 = SkillRemovalRequest {
			target: SkillRemovalTarget::ByName("notebooklm".to_string()),
			scope: ResourceScope::ProjectOnly,
			project_root: Some(root.to_path_buf()),
			agents: vec![AgentType::Claude],
			dry_run: false,
			all_agents: false,
			prior_removed_paths: Vec::new(),
		};
		let res1 = remove_skill_batch(&req1).unwrap();
		assert_eq!(res1.rows[0].verdict, Verdict::Removed);
		assert!(!root.join(".claude/skills/notebooklm").exists());
		assert!(root.join(".cursor/skills/notebooklm").exists());
		assert!(
			root.join(".aghub/notebooklm").exists(),
			"Master must stay while Cursor still holds it"
		);

		let lock_path = root.join("skills-lock.json");
		fs::write(&lock_path, "invalid json conflict {{{{").unwrap();

		let req2 = SkillRemovalRequest {
			target: SkillRemovalTarget::ByName("notebooklm".to_string()),
			scope: ResourceScope::ProjectOnly,
			project_root: Some(root.to_path_buf()),
			agents: vec![AgentType::Cursor],
			dry_run: false,
			all_agents: false,
			prior_removed_paths: Vec::new(),
		};
		let res2 = remove_skill_batch(&req2).unwrap();
		assert_eq!(res2.rows[0].verdict, Verdict::Removed);
		assert!(!root.join(".cursor/skills/notebooklm").exists());
		assert!(
			!root.join(".aghub/notebooklm").exists(),
			"Master must be GC'd when last referrer is removed"
		);
		assert!(
			matches!(res2.prune, PruneStatus::Failed { .. }),
			"expected PruneStatus::Failed, got {:?}",
			res2.prune
		);
	}

	#[cfg(unix)]
	#[test]
	fn test_preview_does_not_take_write_lock() {
		let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
		let temp = tempdir().unwrap();
		let root = temp.path();
		setup_shared_fixture(root, "notebooklm");

		let external_lock = crate::skills::lock::mutation_guard(
			"competing lock",
			ResourceScope::ProjectOnly,
			Some(root),
		)
		.unwrap();

		let req_preview = SkillRemovalRequest {
			target: SkillRemovalTarget::ByName("notebooklm".to_string()),
			scope: ResourceScope::ProjectOnly,
			project_root: Some(root.to_path_buf()),
			agents: vec![AgentType::OpenCode],
			dry_run: true,
			all_agents: false,
			prior_removed_paths: vec![root.join(".agents/skills/notebooklm")],
		};
		let res_preview = remove_skill_batch(&req_preview)
			.expect("preview must not block on write lock");
		assert_eq!(res_preview.rows[0].verdict, Verdict::Removed);
		assert_eq!(res_preview.prune, PruneStatus::NotRun);

		drop(external_lock);

		let req_commit = SkillRemovalRequest {
			target: SkillRemovalTarget::ByName("notebooklm".to_string()),
			scope: ResourceScope::ProjectOnly,
			project_root: Some(root.to_path_buf()),
			agents: vec![AgentType::Claude],
			dry_run: false,
			all_agents: false,
			prior_removed_paths: Vec::new(),
		};
		let res_commit =
			remove_skill_batch(&req_commit).expect("commit should succeed");
		assert_eq!(res_commit.rows[0].verdict, Verdict::Removed);
		assert!(!root.join(".claude/skills/notebooklm").exists());
	}
}
