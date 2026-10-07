//! Single core entry for skill deletion by name across multiple agents.
//!
//! Handles shared-first ordering, whole-batch dry-run preflight, prior-row credit,
//! sibling credit, lock-free preview vs locked commit, Master GC, and lock pruning.

use std::path::{Path, PathBuf};

use crate::batch::{Backing, RemovalCredits};
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
	pub still_read_from: Vec<PathBuf>,
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

	// Shared slots must go first: a private Referrer cannot be revoked while
	// the same agent still reads the shared slot this batch is removing. Reader
	// count comes from `slot_reader_count` (full roster); never re-derive slot
	// sharing here.
	// The sort key counts the full roster (slot_reader_count) because slot
	// sharing is structural; filtering disabled agents ties shared and private
	// slots and can put a private row first, which preflight then refuses.
	// See docs/history/core-transfer.md#seventh-spelling-of-slot-sharing
	// and docs/history/core-removal.md#reconcile-delete-order-needs-the-full-roster
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
	let mut preflight_verdicts: Vec<(AgentType, Verdict, Vec<PathBuf>)> =
		Vec::new();
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
					preflight_verdicts.push((
						agent,
						outcome.verdict,
						outcome.plan.still_read_from,
					));
				} else {
					if can_credit_prior {
						accumulated_deletions
							.extend(outcome.plan.paths.iter().cloned());
					}
					preflight_verdicts.push((
						agent,
						outcome.verdict,
						outcome.plan.still_read_from,
					));
				}
			}
			Err(ConfigError::ResourceNotFound { .. }) => {
				let outcome = manager.skill_noop_outcome(name);
				preflight_verdicts.push((
					agent,
					outcome.verdict,
					outcome.plan.still_read_from,
				));
			}
			Err(err) => {
				preflight_failures.push((agent, err));
			}
		}
	}

	// Preview (dry-run): does not acquire write lock, returns verdicts.
	if request.dry_run {
		let scope_str = match request.scope {
			ResourceScope::GlobalOnly => "global",
			ResourceScope::ProjectOnly => "project",
			ResourceScope::Both => "global+project",
		};
		let rows = target_agents
			.iter()
			.map(|agent| {
				let verdict_entry =
					preflight_verdicts.iter().find(|(a, _, _)| *a == *agent);
				let verdict = verdict_entry
					.map(|(_, v, _)| v.clone())
					.unwrap_or(Verdict::Absent);
				let still_read_from = verdict_entry
					.map(|(_, _, s)| s.clone())
					.unwrap_or_default();
				let error = preflight_failures
					.iter()
					.find(|(a, _)| *a == *agent)
					.map(|(_, err)| {
						format!(
							"delete {} ({scope_str}): {err}",
							agent.as_str()
						)
					});
				SkillRemovalRow {
					agent: *agent,
					verdict,
					error,
					still_read_from,
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
			"skill removal preflight failed; no removal was performed: {failures_str}"
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

	let mut credits = RemovalCredits::new(request.agents.clone(), |agent| {
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
	let mut any_executed = false;

	for &agent in &execution_order {
		let mut manager = create_manager(
			agent,
			request.scope,
			request.project_root.as_deref(),
		);
		if let Err(e) = manager.load() {
			execution_results.push(SkillRemovalRow {
				agent,
				verdict: Verdict::Absent,
				error: Some(e.to_string()),
				still_read_from: Vec::new(),
			});
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
					execution_results.push(SkillRemovalRow {
						agent,
						verdict: outcome.verdict,
						error: None,
						still_read_from: outcome.plan.still_read_from,
					});
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
					execution_results.push(SkillRemovalRow {
						agent,
						verdict: Verdict::Partial,
						error: Some(err),
						still_read_from: outcome.plan.still_read_from,
					});
				}
			}
			Err(ConfigError::ResourceNotFound { .. })
				if credits.already_taken(&agent) =>
			{
				execution_results.push(SkillRemovalRow {
					agent,
					verdict: Verdict::Removed,
					error: None,
					still_read_from: Vec::new(),
				});
			}
			Err(ConfigError::ResourceNotFound { .. }) => {
				let outcome = manager.skill_noop_outcome(name);
				execution_results.push(SkillRemovalRow {
					agent,
					verdict: outcome.verdict,
					error: None,
					still_read_from: outcome.plan.still_read_from,
				});
			}
			Err(err) => {
				execution_results.push(SkillRemovalRow {
					agent,
					verdict: Verdict::Partial,
					error: Some(err.to_string()),
					still_read_from: Vec::new(),
				});
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
			execution_results
				.iter()
				.find(|r| r.agent == *agent)
				.cloned()
				.unwrap_or(SkillRemovalRow {
					agent: *agent,
					verdict: Verdict::Absent,
					error: None,
					still_read_from: Vec::new(),
				})
		})
		.collect();

	Ok(SkillRemovalResponse { rows, prune })
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
		crate::testing::master_with_claude_referrer(root, name);
		for dir in [".opencode", ".cursor", ".pi", ".grok", ".omp"] {
			let slot = root.join(dir).join("skills");
			fs::create_dir_all(&slot).unwrap();
			std::os::unix::fs::symlink(
				root.join(".aghub").join(name),
				slot.join(name),
			)
			.unwrap();
		}
	}

	#[cfg(unix)]
	fn collect_normalized_tree(
		root: &Path,
		current: &Path,
		acc: &mut Vec<(PathBuf, String)>,
	) {
		let read_dir = match fs::read_dir(current) {
			Ok(rd) => rd,
			Err(_) => return,
		};
		let mut entries: Vec<_> = read_dir.filter_map(|e| e.ok()).collect();
		entries.sort_by_key(|e| e.path());
		for entry in entries {
			let path = entry.path();
			let rel = path.strip_prefix(root).unwrap().to_path_buf();
			let meta = fs::symlink_metadata(&path).unwrap();
			if meta.file_type().is_symlink() {
				let target = fs::read_link(&path).unwrap();
				let norm_target =
					if let Ok(rel_target) = target.strip_prefix(root) {
						format!("<root>/{}", rel_target.display())
					} else {
						target.display().to_string()
					};
				acc.push((rel, format!("symlink -> {norm_target}")));
			} else if meta.is_dir() {
				acc.push((rel.clone(), "dir".to_string()));
				collect_normalized_tree(root, &path, acc);
			} else if meta.is_file() {
				let mut content = fs::read_to_string(&path).unwrap_or_default();
				content =
					content.replace(&root.display().to_string(), "<root>");
				acc.push((rel, format!("file: {content}")));
			}
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

		let mut tree_a = Vec::new();
		collect_normalized_tree(root_a, root_a, &mut tree_a);
		tree_a.sort_by(|a, b| a.0.cmp(&b.0));

		let mut tree_b = Vec::new();
		collect_normalized_tree(root_b, root_b, &mut tree_b);
		tree_b.sort_by(|a, b| a.0.cmp(&b.0));

		assert_eq!(
			tree_a, tree_b,
			"root_a and root_b disk and lock state must be identical"
		);
		assert!(!tree_a.is_empty(), "tree must not be empty");

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
				"skill removal preflight failed; no removal was performed"
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

		let (acquired_tx, acquired_rx) = std::sync::mpsc::channel();
		let (release_tx, release_rx) = std::sync::mpsc::channel();
		let root_buf = root.to_path_buf();

		let lock_thread = std::thread::spawn(move || {
			let _lock = crate::skills::lock::mutation_guard(
				"competing lock",
				ResourceScope::ProjectOnly,
				Some(&root_buf),
			)
			.unwrap();
			acquired_tx.send(()).unwrap();
			release_rx.recv().unwrap();
		});

		acquired_rx
			.recv_timeout(std::time::Duration::from_secs(5))
			.expect("competing lock must be acquired on another thread");

		let req_preview = SkillRemovalRequest {
			target: SkillRemovalTarget::ByName("notebooklm".to_string()),
			scope: ResourceScope::ProjectOnly,
			project_root: Some(root.to_path_buf()),
			agents: vec![AgentType::OpenCode],
			dry_run: true,
			all_agents: false,
			prior_removed_paths: vec![root.join(".agents/skills/notebooklm")],
		};

		let (preview_tx, preview_rx) = std::sync::mpsc::channel();
		let req_preview_clone = req_preview.clone();
		std::thread::spawn(move || {
			let res = remove_skill_batch(&req_preview_clone);
			let _ = preview_tx.send(res);
		});

		let res_preview = preview_rx
			.recv_timeout(std::time::Duration::from_secs(5))
			.expect(
				"preview must not block on write lock held by another thread",
			)
			.expect("preview should succeed");
		assert_eq!(res_preview.rows[0].verdict, Verdict::Removed);
		assert_eq!(res_preview.prune, PruneStatus::NotRun);

		release_tx.send(()).unwrap();
		lock_thread.join().unwrap();

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

	#[cfg(unix)]
	#[test]
	fn test_commit_follows_replan_after_disk_state_changes() {
		let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
		let temp = tempdir().unwrap();
		let root = temp.path();

		crate::testing::master_with_claude_referrer(root, "notebooklm");
		fs::remove_file(root.join(".agents/skills/notebooklm")).unwrap();

		let req = SkillRemovalRequest {
			target: SkillRemovalTarget::ByName("notebooklm".to_string()),
			scope: ResourceScope::ProjectOnly,
			project_root: Some(root.to_path_buf()),
			agents: vec![AgentType::Claude],
			dry_run: true,
			all_agents: false,
			prior_removed_paths: Vec::new(),
		};

		// Preview: Claude is the sole holder, so preview plans to remove Claude and GC master
		let res_preview = remove_skill_batch(&req).unwrap();
		assert_eq!(res_preview.rows[0].verdict, Verdict::Removed);

		// Disk state changes before commit: Cursor adds a referrer!
		let cursor_dir = root.join(".cursor/skills");
		fs::create_dir_all(&cursor_dir).unwrap();
		std::os::unix::fs::symlink(
			root.join(".aghub/notebooklm"),
			cursor_dir.join("notebooklm"),
		)
		.unwrap();

		let mut req_commit = req.clone();
		req_commit.dry_run = false;
		let res_commit = remove_skill_batch(&req_commit).unwrap();

		assert_eq!(res_commit.rows[0].verdict, Verdict::Removed);
		assert!(!root.join(".claude/skills/notebooklm").exists());
		assert!(root.join(".cursor/skills/notebooklm").exists());
		assert!(
			root.join(".aghub/notebooklm").exists(),
			"Master must survive because Cursor was added before commit and commit re-planned"
		);
	}

	#[test]
	fn test_dry_run_reports_preflight_failure_in_row_error() {
		let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
		let temp = tempdir().unwrap();
		let root = temp.path();

		let req = SkillRemovalRequest {
			target: SkillRemovalTarget::ByName("notebooklm".to_string()),
			scope: ResourceScope::ProjectOnly,
			project_root: Some(root.to_path_buf()),
			agents: vec![AgentType::JetBrainsAi],
			dry_run: true,
			all_agents: false,
			prior_removed_paths: Vec::new(),
		};

		let res = remove_skill_batch(&req).unwrap();
		assert_eq!(res.rows.len(), 1);
		assert_eq!(res.rows[0].agent, AgentType::JetBrainsAi);
		assert_eq!(res.rows[0].verdict, Verdict::Absent);
		let err = res.rows[0]
			.error
			.as_deref()
			.expect("dry run must report preflight failure in row error");
		assert!(
			err.contains("delete jetbrains-ai (project)"),
			"unexpected error message: {err}"
		);
		assert!(
			err.contains("no project skill config"),
			"unexpected error message: {err}"
		);
	}
}
