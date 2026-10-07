//! Single core entry for skill deletion by name across multiple agents.
//!
//! Handles shared-first ordering, whole-batch dry-run preflight, prior-row credit,
//! sibling credit, lock-free preview vs locked commit, Master GC, and lock pruning.

use std::path::{Path, PathBuf};
use std::sync::Arc;

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
	/// Caller-supplied paths credited as already removed during dry-run preflight only.
	/// In commit mode, caller prior credit is ignored (seeded empty), but the preflight pass
	/// still accumulates earlier rows' plan.paths for sibling credit. The in-lock execution
	/// always rechecks the real disk (&[]).
	pub prior_removed_paths: Vec<PathBuf>,
	/// A copy in this batch keeps the Master alive; skips the exhaustiveness
	/// holder scan.
	pub keeps_master: bool,
}

/// Outcome row for a single agent in a batch skill removal.
#[derive(Debug, Clone)]
pub struct SkillRemovalRow {
	pub agent: AgentType,
	pub verdict: Verdict,
	pub error: Option<String>,
	pub typed_error: Option<Arc<ConfigError>>,
	pub is_load_error: bool,
	pub still_read_from: Vec<PathBuf>,
}

/// Response returned by the batch skill removal entry point.
#[derive(Debug, Clone)]
pub struct SkillRemovalResponse {
	pub rows: Vec<SkillRemovalRow>,
	pub prune: PruneStatus,
	pub exhaustive: bool,
	pub keepers: Vec<AgentType>,
	pub unreadable: Vec<&'static str>,
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
) -> Result<()> {
	if is_exhaustive && !unreadable.is_empty() {
		return Err(ConfigError::InvalidConfig(format!(
			"cannot decide whether removing '{name}' leaves the shared \
			 .aghub master unread: agent(s) '{}' could not be read \
			 (skills directory unreadable), and an agent aghub cannot read may \
			 still be holding it — naming it in --remove cannot authorize a \
			 collection this run is unable to carry out on it. Fix or remove \
			 those configs, then re-run.",
			unreadable.join("', '")
		)));
	}
	Ok(())
}

/// Single core entry point for skill deletion by name across multiple agents.
pub fn remove_skill_batch(
	request: &SkillRemovalRequest,
) -> Result<SkillRemovalResponse> {
	let name = match &request.target {
		SkillRemovalTarget::ByName(name) => name.as_str(),
	};

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
		request.agents.clone()
	};

	let is_exhaustive = !request.keeps_master
		&& (request.all_agents
			|| (!holders.is_empty()
				&& holders.iter().all(|held| target_agents.contains(held))));

	check_unreadable_exhaustive(name, is_exhaustive, &unreadable)?;

	let keepers: Vec<AgentType> = holders
		.iter()
		.filter(|held| !target_agents.contains(held))
		.copied()
		.collect();

	if target_agents.is_empty() {
		return Ok(SkillRemovalResponse {
			rows: Vec::new(),
			prune: PruneStatus::NotRun,
			exhaustive: is_exhaustive,
			keepers,
			unreadable,
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
	let mut preflight_verdicts: Vec<(AgentType, Verdict, Vec<PathBuf>)> =
		Vec::new();
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

		let plan_result = manager.remove_skill_planned_for_agents_with_prior(
			name,
			is_exhaustive && holders.contains(&agent),
			true, // dry_run
			true, // confirm
			&target_agents,
			&accumulated_deletions,
		);

		match plan_result {
			Ok(outcome) => {
				preflight_verdicts.push((
					agent,
					outcome.verdict.clone(),
					outcome.plan.still_read_from.clone(),
				));
				if let Verdict::Refused { ref reason } = outcome.verdict {
					if !request.dry_run {
						let op = if request.all_agents {
							"remove from every agent"
						} else {
							"remove for this agent alone"
						};
						preflight_failures.push((
							agent,
							Arc::new(ConfigError::unsupported_operation(
								op,
								reason,
								agent.as_str(),
							)),
						));
					}
				} else if can_credit_prior {
					accumulated_deletions
						.extend(outcome.plan.paths.iter().cloned());
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
				preflight_failures.push((agent, Arc::new(err)));
			}
		}
	}

	// Preview (dry-run): does not acquire write lock, returns verdicts.
	if request.dry_run {
		let scope_str = scope_to_word(request.scope);
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
				let load_err =
					preflight_load_failures.iter().find(|(a, _)| *a == *agent);
				let plan_err =
					preflight_failures.iter().find(|(a, _)| *a == *agent);
				let err_entry = load_err.or(plan_err);
				let is_load_error = load_err.is_some();
				let typed_error = err_entry.map(|(_, err)| Arc::clone(err));
				let error = err_entry.map(|(_, err)| {
					format!("delete {} ({scope_str}): {err}", agent.as_str())
				});
				SkillRemovalRow {
					agent: *agent,
					verdict,
					error,
					typed_error,
					is_load_error,
					still_read_from,
				}
			})
			.collect();
		return Ok(SkillRemovalResponse {
			rows,
			prune: PruneStatus::NotRun,
			exhaustive: is_exhaustive,
			keepers,
			unreadable,
		});
	}

	// Commit path: if predictable failures occurred, reject the WHOLE batch before any write!
	if !preflight_failures.is_empty() {
		let all_unsupported = preflight_failures.iter().all(|(_, err)| {
			matches!(**err, ConfigError::UnsupportedOperation(_))
		});
		let scope_str = scope_to_word(request.scope);
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
		request.agents.clone()
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

	check_unreadable_exhaustive(name, is_exhaustive, &unreadable)?;

	let keepers: Vec<AgentType> = holders
		.iter()
		.filter(|held| !in_lock_target_agents.contains(held))
		.copied()
		.collect();

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
			execution_results.push(SkillRemovalRow {
				agent,
				verdict: Verdict::Absent,
				error: Some(e.to_string()),
				typed_error: Some(Arc::new(e)),
				is_load_error: true,
				still_read_from: Vec::new(),
			});
			continue;
		}

		let res = manager.remove_skill_planned_for_agents_with_prior(
			name,
			is_exhaustive && holders.contains(&agent),
			false, // dry_run
			true,  // confirm
			&in_lock_target_agents,
			&[],
		);

		match res {
			Ok(outcome) => {
				if outcome.executed {
					credits.credit(agent);
					match outcome.prune {
						PruneStatus::Failed { reason, pruned } => {
							all_pruned_keys.extend(pruned);
							batch_prune = PruneStatus::Failed {
								reason,
								pruned: all_pruned_keys.clone(),
							};
						}
						PruneStatus::Pruned(keys) => {
							for key in keys {
								if !all_pruned_keys.contains(&key) {
									all_pruned_keys.push(key);
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
				if outcome.failed_paths.is_empty() {
					execution_results.push(SkillRemovalRow {
						agent,
						verdict: outcome.verdict,
						error: None,
						typed_error: None,
						is_load_error: false,
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
						error: Some(err.clone()),
						typed_error: Some(Arc::new(
							ConfigError::InvalidConfig(err),
						)),
						is_load_error: false,
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
					typed_error: None,
					is_load_error: false,
					still_read_from: Vec::new(),
				});
			}
			Err(err @ ConfigError::ResourceNotFound { .. }) => {
				let outcome = manager.skill_noop_outcome(name);
				execution_results.push(SkillRemovalRow {
					agent,
					verdict: outcome.verdict,
					error: Some(err.to_string()),
					typed_error: Some(Arc::new(err)),
					is_load_error: false,
					still_read_from: outcome.plan.still_read_from,
				});
			}
			Err(err) => {
				execution_results.push(SkillRemovalRow {
					agent,
					verdict: Verdict::Absent,
					error: Some(err.to_string()),
					typed_error: Some(Arc::new(err)),
					is_load_error: false,
					still_read_from: Vec::new(),
				});
			}
		}
	}

	let rows = in_lock_target_agents
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
					typed_error: None,
					is_load_error: false,
					still_read_from: Vec::new(),
				})
		})
		.collect();

	Ok(SkillRemovalResponse {
		rows,
		prune: batch_prune,
		exhaustive: is_exhaustive,
		keepers,
		unreadable,
	})
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::skills::prune::test_lock::env_lock;
	#[cfg(unix)]
	use crate::skills::removal;
	use std::fs;
	use tempfile::tempdir;
	struct EnvVarGuard(&'static str, Option<std::ffi::OsString>);

	impl EnvVarGuard {
		fn set(key: &'static str, value: &Path) -> Self {
			let previous = std::env::var_os(key);
			std::env::set_var(key, value);
			Self(key, previous)
		}
	}

	impl Drop for EnvVarGuard {
		fn drop(&mut self) {
			match self.1.take() {
				Some(value) => std::env::set_var(self.0, value),
				None => std::env::remove_var(self.0),
			}
		}
	}

	fn isolate_env(temp: &tempfile::TempDir) -> (EnvVarGuard, EnvVarGuard) {
		let isolated_home = temp.path().join("home");
		let isolated_data = temp.path().join("data");
		fs::create_dir_all(&isolated_home).unwrap();
		fs::create_dir_all(&isolated_data).unwrap();
		(
			EnvVarGuard::set("HOME", &isolated_home),
			EnvVarGuard::set("AGHUB_DATA_DIR", &isolated_data),
		)
	}

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
		let other_master = root.join(".aghub/other-skill");
		fs::create_dir_all(&other_master).unwrap();
		fs::write(
			other_master.join("SKILL.md"),
			"---\nname: other-skill\ndescription: unremoved\n---\n\n# other-skill\n",
		)
		.unwrap();
		let lock_path = root.join("skills-lock.json");
		let lock_content = format!(
			r#"{{"version":1,"skills":{{"{name}":{{"source":"test","sourceType":"node_modules","computedHash":"abc123"}},"other-skill":{{"source":"test2","sourceType":"node_modules","computedHash":"def456"}}}}}}"#
		);
		fs::write(lock_path, lock_content).unwrap();
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

		// Shared slot readers first, private readers second
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
			AgentType::Claude,
		];

		// Private readers first, shared slot readers second
		let order_private_first = vec![
			AgentType::Claude,
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

		let temp_a = tempdir().unwrap();
		let root_a = temp_a.path().join("project");
		let res_a = {
			let _env_a = isolate_env(&temp_a);
			fs::create_dir_all(&root_a).unwrap();
			setup_shared_fixture(&root_a, "notebooklm");
			let req_a = SkillRemovalRequest {
				target: SkillRemovalTarget::ByName("notebooklm".to_string()),
				scope: ResourceScope::ProjectOnly,
				project_root: Some(root_a.clone()),
				agents: order_shared_first.clone(),
				dry_run: false,
				all_agents: false,
				prior_removed_paths: Vec::new(),
				keeps_master: false,
			};
			remove_skill_batch(&req_a)
				.expect("order_shared_first batch should succeed")
		};

		let temp_b = tempdir().unwrap();
		let root_b = temp_b.path().join("project");
		let res_b = {
			let _env_b = isolate_env(&temp_b);
			fs::create_dir_all(&root_b).unwrap();
			setup_shared_fixture(&root_b, "notebooklm");
			let req_b = SkillRemovalRequest {
				target: SkillRemovalTarget::ByName("notebooklm".to_string()),
				scope: ResourceScope::ProjectOnly,
				project_root: Some(root_b.clone()),
				agents: order_private_first.clone(),
				dry_run: false,
				all_agents: false,
				prior_removed_paths: Vec::new(),
				keeps_master: false,
			};
			remove_skill_batch(&req_b)
				.expect("order_private_first batch should succeed")
		};

		assert_eq!(res_a.rows.len(), 16);
		assert_eq!(res_b.rows.len(), 16);
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
		collect_normalized_tree(&root_a, &root_a, &mut tree_a);
		tree_a.sort_by(|a, b| a.0.cmp(&b.0));

		let mut tree_b = Vec::new();
		collect_normalized_tree(&root_b, &root_b, &mut tree_b);
		tree_b.sort_by(|a, b| a.0.cmp(&b.0));

		assert_eq!(
			tree_a, tree_b,
			"root_a and root_b disk and lock state must be identical"
		);
		assert!(!tree_a.is_empty(), "tree must not be empty");

		let lock_a =
			fs::read_to_string(root_a.join("skills-lock.json")).unwrap();
		let lock_b =
			fs::read_to_string(root_b.join("skills-lock.json")).unwrap();
		assert_eq!(
			lock_a, lock_b,
			"skills-lock.json contents must match across both orderings"
		);
		assert!(
			!lock_a.contains("notebooklm"),
			"lock entry for notebooklm must have been pruned by removal"
		);
		assert!(
			lock_a.contains("other-skill"),
			"lock entry for unremoved other-skill must be preserved"
		);

		for &agent in &order_shared_first {
			let dirs_a = crate::create_adapter(agent)
				.get_skills_paths(Some(&root_a), ResourceScope::ProjectOnly);
			let eff_a = removal::read_effect_after(&dirs_a, "notebooklm", &[]);
			assert!(
				eff_a.survivors.is_empty(),
				"agent {:?} in a has survivors",
				agent
			);

			let dirs_b = crate::create_adapter(agent)
				.get_skills_paths(Some(&root_b), ResourceScope::ProjectOnly);
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
	fn test_prior_row_credit_turns_refused_into_removed() {
		let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
		let temp = tempdir().unwrap();
		let _env = isolate_env(&temp);
		let root = temp.path().join("project");
		fs::create_dir_all(&root).unwrap();
		setup_shared_fixture(&root, "notebooklm");

		let req_without_credit = SkillRemovalRequest {
			target: SkillRemovalTarget::ByName("notebooklm".to_string()),
			scope: ResourceScope::ProjectOnly,
			project_root: Some(root.clone()),
			agents: vec![AgentType::OpenCode],
			dry_run: true,
			all_agents: false,
			prior_removed_paths: Vec::new(),
			keeps_master: false,
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
			project_root: Some(root.clone()),
			agents: vec![AgentType::OpenCode],
			dry_run: true,
			all_agents: false,
			prior_removed_paths: vec![root.join(".agents/skills/notebooklm")],
			keeps_master: false,
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
	fn test_internal_accumulation_credits_later_row() {
		let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
		let temp = tempdir().unwrap();
		let _env = isolate_env(&temp);
		let root = temp.path().join("project");
		fs::create_dir_all(&root).unwrap();
		setup_shared_fixture(&root, "notebooklm");

		// Disable all agents except Amp, OpenCode, and Claude (who keeps the Master).
		let ids: Vec<&str> = AgentType::ALL
			.iter()
			.map(|a| a.as_str())
			.filter(|id| *id != "opencode" && *id != "amp" && *id != "claude")
			.collect();
		let _off = crate::agent_settings::test_override::disable(&ids);

		// When asked alone, OpenCode (private reader) is Refused because .agents/skills/notebooklm still exists.
		let req_alone = SkillRemovalRequest {
			target: SkillRemovalTarget::ByName("notebooklm".to_string()),
			scope: ResourceScope::ProjectOnly,
			project_root: Some(root.clone()),
			agents: vec![AgentType::OpenCode],
			dry_run: true,
			all_agents: false,
			prior_removed_paths: Vec::new(),
			keeps_master: false,
		};
		let res_alone = remove_skill_batch(&req_alone).unwrap();
		assert!(
			matches!(res_alone.rows[0].verdict, Verdict::Refused { .. }),
			"expected Refused when asked alone, got {:?}",
			res_alone.rows[0].verdict
		);

		// When asked together (shared-slot writer Amp + private reader OpenCode),
		// earlier row (Amp) plans deletion of .agents/skills/notebooklm, crediting OpenCode internally
		// without caller-supplied prior_removed_paths.
		let req_batch = SkillRemovalRequest {
			target: SkillRemovalTarget::ByName("notebooklm".to_string()),
			scope: ResourceScope::ProjectOnly,
			project_root: Some(root.clone()),
			agents: vec![AgentType::Amp, AgentType::OpenCode],
			dry_run: true,
			all_agents: false,
			prior_removed_paths: Vec::new(),
			keeps_master: false,
		};
		let res_batch = remove_skill_batch(&req_batch).unwrap();
		let amp_row = res_batch
			.rows
			.iter()
			.find(|r| r.agent == AgentType::Amp)
			.unwrap();
		let opencode_row = res_batch
			.rows
			.iter()
			.find(|r| r.agent == AgentType::OpenCode)
			.unwrap();
		assert_eq!(amp_row.verdict, Verdict::Removed);
		assert_eq!(
			opencode_row.verdict,
			Verdict::Removed,
			"internal accumulation must turn OpenCode from Refused into Removed"
		);
	}

	#[cfg(unix)]
	#[test]
	fn test_whole_batch_preflight_rejection_writes_nothing() {
		let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
		let temp = tempdir().unwrap();
		let _env = isolate_env(&temp);
		let root = temp.path().join("project");
		fs::create_dir_all(&root).unwrap();
		setup_shared_fixture(&root, "notebooklm");

		let lock_path = root.join("skills-lock.json");
		let initial_lock_content =
			r#"{"version":1,"skills":{"notebooklm":{"source":"test"}}}"#;
		fs::write(&lock_path, initial_lock_content).unwrap();
		let initial_lock_bytes = fs::read(&lock_path).unwrap();

		let req = SkillRemovalRequest {
			target: SkillRemovalTarget::ByName("notebooklm".to_string()),
			scope: ResourceScope::ProjectOnly,
			project_root: Some(root.clone()),
			agents: vec![AgentType::Claude, AgentType::OpenCode],
			dry_run: false,
			all_agents: false,
			prior_removed_paths: Vec::new(),
			keeps_master: false,
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
		assert!(
			err_msg.contains("location shared with other agents"),
			"message must include refusal reason text: {err_msg}"
		);

		assert_eq!(
			fs::read(&lock_path).unwrap(),
			initial_lock_bytes,
			"lock file bytes must be unchanged after preflight rejection"
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
		let _env = isolate_env(&temp);
		let root = temp.path().join("project");
		fs::create_dir_all(&root).unwrap();

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

		let mut _off =
			Some(crate::agent_settings::test_override::disable(&["opencode"]));

		let req_unnamed = SkillRemovalRequest {
			target: SkillRemovalTarget::ByName("notebooklm".to_string()),
			scope: ResourceScope::ProjectOnly,
			project_root: Some(root.clone()),
			agents: vec![AgentType::Claude],
			dry_run: false,
			all_agents: false,
			prior_removed_paths: Vec::new(),
			keeps_master: false,
		};
		let res_unnamed = remove_skill_batch(&req_unnamed).unwrap();
		assert_eq!(res_unnamed.rows[0].verdict, Verdict::Removed);

		assert!(!root.join(".claude/skills/notebooklm").exists());
		assert!(root.join(".opencode/skills/notebooklm").exists());
		assert!(root.join(".aghub/notebooklm").exists());

		// all_agents=true with empty agents list: disabled OpenCode is the ONLY remaining reader.
		// Its Referrer AND the Master must stay.
		let req_all_disabled = SkillRemovalRequest {
			target: SkillRemovalTarget::ByName("notebooklm".to_string()),
			scope: ResourceScope::ProjectOnly,
			project_root: Some(root.clone()),
			agents: Vec::new(),
			dry_run: false,
			all_agents: true,
			prior_removed_paths: Vec::new(),
			keeps_master: false,
		};
		let res_disabled = remove_skill_batch(&req_all_disabled).unwrap();
		assert!(res_disabled.rows.is_empty());
		assert!(
			root.join(".opencode/skills/notebooklm").exists(),
			"disabled holder Referrer must survive all_agents expansion"
		);
		assert!(
			root.join(".aghub/notebooklm").exists(),
			"Master must survive while disabled holder is the only remaining reader"
		);

		let req_named = SkillRemovalRequest {
			target: SkillRemovalTarget::ByName("notebooklm".to_string()),
			scope: ResourceScope::ProjectOnly,
			project_root: Some(root.clone()),
			agents: vec![AgentType::OpenCode],
			dry_run: false,
			all_agents: false,
			prior_removed_paths: Vec::new(),
			keeps_master: false,
		};
		let res_named = remove_skill_batch(&req_named).unwrap();
		assert_eq!(res_named.rows[0].verdict, Verdict::Removed);

		assert!(!root.join(".opencode/skills/notebooklm").exists());
		assert!(!root.join(".aghub/notebooklm").exists());

		// Compare with the same fixture where the agent is enabled:
		// Re-create the single-reader fixture with OpenCode enabled.
		fs::create_dir_all(&master).unwrap();
		fs::write(
			master.join("SKILL.md"),
			"---\nname: notebooklm\ndescription: test\n---\n",
		)
		.unwrap();
		std::os::unix::fs::symlink(&master, opencode_skills.join("notebooklm"))
			.unwrap();
		drop(_off.take());

		let req_all_enabled = SkillRemovalRequest {
			target: SkillRemovalTarget::ByName("notebooklm".to_string()),
			scope: ResourceScope::ProjectOnly,
			project_root: Some(root.clone()),
			agents: Vec::new(),
			dry_run: false,
			all_agents: true,
			prior_removed_paths: Vec::new(),
			keeps_master: false,
		};
		let res_enabled = remove_skill_batch(&req_all_enabled).unwrap();
		assert_eq!(res_enabled.rows.len(), 1);
		assert_eq!(res_enabled.rows[0].agent, AgentType::OpenCode);
		assert_eq!(res_enabled.rows[0].verdict, Verdict::Removed);
		assert!(
			!root.join(".opencode/skills/notebooklm").exists(),
			"enabled holder Referrer must be removed by all_agents expansion"
		);
		assert!(
			!root.join(".aghub/notebooklm").exists(),
			"Master must be GC'd when enabled holder is removed"
		);
	}

	#[cfg(unix)]
	#[test]
	fn test_master_gc_and_prune_failure_reported_independently() {
		let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
		let temp = tempdir().unwrap();
		let _env = isolate_env(&temp);
		let root = temp.path().join("project");
		fs::create_dir_all(&root).unwrap();

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
			project_root: Some(root.clone()),
			agents: vec![AgentType::Claude],
			dry_run: false,
			all_agents: false,
			prior_removed_paths: Vec::new(),
			keeps_master: false,
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
			project_root: Some(root.clone()),
			agents: vec![AgentType::Cursor],
			dry_run: false,
			all_agents: false,
			prior_removed_paths: Vec::new(),
			keeps_master: false,
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
		let _env = isolate_env(&temp);
		let root = temp.path().join("project");
		fs::create_dir_all(&root).unwrap();
		setup_shared_fixture(&root, "notebooklm");

		let (acquired_tx, acquired_rx) = std::sync::mpsc::channel();
		let (release_tx, release_rx) = std::sync::mpsc::channel();
		let root_buf = root.clone();

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
			project_root: Some(root.clone()),
			agents: vec![AgentType::OpenCode],
			dry_run: true,
			all_agents: false,
			prior_removed_paths: vec![root.join(".agents/skills/notebooklm")],
			keeps_master: false,
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
			project_root: Some(root.clone()),
			agents: vec![AgentType::Claude],
			dry_run: false,
			all_agents: false,
			prior_removed_paths: Vec::new(),
			keeps_master: false,
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
		let _env = isolate_env(&temp);
		let root = temp.path().join("project");
		fs::create_dir_all(&root).unwrap();

		crate::testing::master_with_claude_referrer(&root, "notebooklm");
		fs::remove_file(root.join(".agents/skills/notebooklm")).unwrap();

		let (hook_tx, hook_rx) = std::sync::mpsc::channel();
		*COMMIT_PREFLIGHT_HOOK.lock().unwrap() = Some(hook_tx);

		struct HookReset;
		impl Drop for HookReset {
			fn drop(&mut self) {
				*COMMIT_PREFLIGHT_HOOK.lock().unwrap() = None;
			}
		}
		let _hook_reset = HookReset;

		// Test thread acquires the mutation write lock first.
		let test_lock = crate::skills::lock::mutation_guard(
			"test competing lock",
			ResourceScope::ProjectOnly,
			Some(&root),
		)
		.expect("test thread acquires mutation lock");

		let req_commit = SkillRemovalRequest {
			target: SkillRemovalTarget::ByName("notebooklm".to_string()),
			scope: ResourceScope::ProjectOnly,
			project_root: Some(root.clone()),
			agents: vec![AgentType::Claude],
			dry_run: false,
			all_agents: false,
			prior_removed_paths: Vec::new(),
			keeps_master: false,
		};

		// Spawn commit thread.
		// Commit will perform its unlocked preflight (which sees ONLY Claude holds notebooklm),
		// signals COMMIT_PREFLIGHT_HOOK, then attempts to acquire mutation write lock and BLOCKS!
		let (commit_tx, commit_rx) = std::sync::mpsc::channel();
		let req_commit_clone = req_commit.clone();
		let commit_thread = std::thread::spawn(move || {
			let res = remove_skill_batch(&req_commit_clone);
			let _ = commit_tx.send(res);
		});

		// Wait on hook: commit thread has completed preflight and is about to block on mutation lock.
		hook_rx
			.recv_timeout(std::time::Duration::from_secs(5))
			.expect("commit thread must signal hook after unlocked preflight");

		// While commit thread is blocked waiting for write lock, mutate disk state:
		// Cursor adds a referrer symlink to the Master!
		let cursor_dir = root.join(".cursor/skills");
		fs::create_dir_all(&cursor_dir).unwrap();
		std::os::unix::fs::symlink(
			root.join(".aghub/notebooklm"),
			cursor_dir.join("notebooklm"),
		)
		.unwrap();

		// Release the write lock so commit thread can proceed.
		drop(test_lock);

		// Commit acquires the lock, re-plans inside the lock, discovers Cursor now holds the skill,
		// removes Claude's referrer, but preserves the Master!
		let res_commit = commit_rx
			.recv_timeout(std::time::Duration::from_secs(5))
			.expect("commit thread should complete after lock release")
			.expect("commit should succeed");
		commit_thread.join().unwrap();

		assert_eq!(res_commit.rows[0].verdict, Verdict::Removed);
		assert!(
			!root.join(".claude/skills/notebooklm").exists(),
			"Claude referrer must be removed"
		);
		assert!(
			root.join(".cursor/skills/notebooklm").exists(),
			"Cursor referrer must remain"
		);
		assert!(
			root.join(".aghub/notebooklm").exists(),
			"Master must survive because Cursor was added while commit was blocked and commit re-planned inside lock"
		);
	}

	#[cfg(unix)]
	#[test]
	fn test_in_lock_replan_refuses_when_holder_becomes_unreadable() {
		let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
		let temp = tempdir().unwrap();
		let _env = isolate_env(&temp);
		let root = temp.path().join("project");
		fs::create_dir_all(&root).unwrap();

		let master = root.join(".aghub/mover");
		fs::create_dir_all(&master).unwrap();
		fs::write(
			master.join("SKILL.md"),
			"---\nname: mover\ndescription: test\n---\n",
		)
		.unwrap();

		fs::create_dir_all(root.join(".codex/skills")).unwrap();
		std::os::unix::fs::symlink(&master, root.join(".codex/skills/mover"))
			.unwrap();

		let (hook_tx, hook_rx) = std::sync::mpsc::channel();
		*COMMIT_PREFLIGHT_HOOK.lock().unwrap() = Some(hook_tx);

		struct HookReset;
		impl Drop for HookReset {
			fn drop(&mut self) {
				*COMMIT_PREFLIGHT_HOOK.lock().unwrap() = None;
			}
		}
		let _hook_reset = HookReset;

		// Acquire mutation lock first so commit thread blocks before entering lock.
		let test_lock = crate::skills::lock::mutation_guard(
			"test competing lock",
			ResourceScope::ProjectOnly,
			Some(&root),
		)
		.expect("test thread acquires mutation lock");

		let req_commit = SkillRemovalRequest {
			target: SkillRemovalTarget::ByName("mover".to_string()),
			scope: ResourceScope::ProjectOnly,
			project_root: Some(root.clone()),
			agents: vec![AgentType::Codex],
			dry_run: false,
			all_agents: true,
			prior_removed_paths: Vec::new(),
			keeps_master: false,
		};

		let (commit_tx, commit_rx) = std::sync::mpsc::channel();
		let req_commit_clone = req_commit.clone();
		let commit_thread = std::thread::spawn(move || {
			let res = remove_skill_batch(&req_commit_clone);
			let _ = commit_tx.send(res);
		});

		// Wait for commit thread to complete unlocked preflight and block on write lock.
		hook_rx
			.recv_timeout(std::time::Duration::from_secs(5))
			.expect("commit thread must signal hook after unlocked preflight");

		// While commit thread is blocked waiting for write lock, create unreadable Windsurf skills dir (symlink loop).
		fs::create_dir_all(root.join(".windsurf")).unwrap();
		std::os::unix::fs::symlink(
			std::path::Path::new("skills"),
			root.join(".windsurf/skills"),
		)
		.unwrap();

		// Release the write lock so commit thread can proceed.
		drop(test_lock);

		let res_commit = commit_rx
			.recv_timeout(std::time::Duration::from_secs(5))
			.expect("commit thread should complete after lock release");
		commit_thread.join().unwrap();

		let err = res_commit.expect_err(
			"in-lock replan must refuse when a holder's skills directory is unreadable",
		);
		let err_msg = err.to_string();
		assert!(
			err_msg.contains(
				"cannot decide whether removing 'mover' leaves the shared"
			),
			"message must match unreadable refusal: {err_msg}"
		);
		assert!(
			err_msg.contains("windsurf"),
			"message must name unreadable agent: {err_msg}"
		);
	}

	#[test]
	fn test_dry_run_reports_preflight_failure_in_row_error() {
		let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
		let temp = tempdir().unwrap();
		let _env = isolate_env(&temp);
		let root = temp.path().join("project");
		fs::create_dir_all(&root).unwrap();

		let req = SkillRemovalRequest {
			target: SkillRemovalTarget::ByName("notebooklm".to_string()),
			scope: ResourceScope::ProjectOnly,
			project_root: Some(root.clone()),
			agents: vec![AgentType::JetBrainsAi],
			dry_run: true,
			all_agents: false,
			prior_removed_paths: Vec::new(),
			keeps_master: false,
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
		assert!(matches!(
			res.rows[0].typed_error.as_deref(),
			Some(ConfigError::UnsupportedOperation(_))
		));
	}

	#[cfg(unix)]
	#[test]
	fn test_commit_mode_ignores_prior_removed_paths() {
		let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
		let temp = tempdir().unwrap();
		let _env = isolate_env(&temp);
		let root = temp.path().join("project");
		fs::create_dir_all(&root).unwrap();
		setup_shared_fixture(&root, "notebooklm");

		let req = SkillRemovalRequest {
			target: SkillRemovalTarget::ByName("notebooklm".to_string()),
			scope: ResourceScope::ProjectOnly,
			project_root: Some(root.clone()),
			agents: vec![AgentType::OpenCode],
			dry_run: false,
			all_agents: false,
			prior_removed_paths: vec![root.join(".agents/skills/notebooklm")],
			keeps_master: false,
		};
		// In commit mode, prior_removed_paths is ignored so preflight fails because .agents/skills/notebooklm actually exists on disk.
		let err = remove_skill_batch(&req).unwrap_err();
		assert!(err.to_string().contains("skill removal preflight failed"));
	}

	#[cfg(unix)]
	#[test]
	fn test_keeps_master_skips_unreadable_holder_scan_refusal() {
		let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
		let temp = tempdir().unwrap();
		let _env = isolate_env(&temp);
		let root = temp.path().join("project");
		fs::create_dir_all(&root).unwrap();

		let master = root.join(".aghub/mover");
		fs::create_dir_all(&master).unwrap();
		fs::write(
			master.join("SKILL.md"),
			"---\nname: mover\ndescription: test\n---\n",
		)
		.unwrap();

		fs::create_dir_all(root.join(".codex/skills")).unwrap();
		std::os::unix::fs::symlink(&master, root.join(".codex/skills/mover"))
			.unwrap();

		fs::create_dir_all(root.join(".windsurf")).unwrap();
		std::os::unix::fs::symlink(
			std::path::Path::new("skills"),
			root.join(".windsurf/skills"),
		)
		.unwrap();

		// With keeps_master: false, it would be exhaustive and fail because Windsurf is unreadable.
		let req_false = SkillRemovalRequest {
			target: SkillRemovalTarget::ByName("mover".to_string()),
			scope: ResourceScope::ProjectOnly,
			project_root: Some(root.clone()),
			agents: vec![AgentType::Codex, AgentType::Windsurf],
			dry_run: true,
			all_agents: false,
			prior_removed_paths: Vec::new(),
			keeps_master: false,
		};
		assert!(remove_skill_batch(&req_false).is_err());

		// With keeps_master: true, scan is skipped, so it does NOT fail wholesale.
		let req_true = SkillRemovalRequest {
			target: SkillRemovalTarget::ByName("mover".to_string()),
			scope: ResourceScope::ProjectOnly,
			project_root: Some(root.clone()),
			agents: vec![AgentType::Codex, AgentType::Windsurf],
			dry_run: true,
			all_agents: false,
			prior_removed_paths: Vec::new(),
			keeps_master: true,
		};
		let res = remove_skill_batch(&req_true)
			.expect("keeps_master skips unreadable scan refusal");
		assert!(!res.exhaustive);
		assert!(res.unreadable.is_empty());
	}

	#[cfg(unix)]
	#[test]
	fn test_in_lock_replan_holder_appearing_with_all_agents() {
		let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
		let temp = tempdir().unwrap();
		let _env = isolate_env(&temp);
		let root = temp.path().join("project");
		fs::create_dir_all(&root).unwrap();

		crate::testing::master_with_claude_referrer(&root, "notebooklm");
		fs::remove_file(root.join(".agents/skills/notebooklm")).unwrap();

		let (hook_tx, hook_rx) = std::sync::mpsc::channel();
		*COMMIT_PREFLIGHT_HOOK.lock().unwrap() = Some(hook_tx);

		struct HookReset;
		impl Drop for HookReset {
			fn drop(&mut self) {
				*COMMIT_PREFLIGHT_HOOK.lock().unwrap() = None;
			}
		}
		let _hook_reset = HookReset;

		// Test thread acquires the mutation write lock first.
		let test_lock = crate::skills::lock::mutation_guard(
			"test competing lock",
			ResourceScope::ProjectOnly,
			Some(&root),
		)
		.expect("test thread acquires mutation lock");

		let req_commit = SkillRemovalRequest {
			target: SkillRemovalTarget::ByName("notebooklm".to_string()),
			scope: ResourceScope::ProjectOnly,
			project_root: Some(root.clone()),
			agents: Vec::new(),
			dry_run: false,
			all_agents: true,
			prior_removed_paths: Vec::new(),
			keeps_master: false,
		};

		let (commit_tx, commit_rx) = std::sync::mpsc::channel();
		let req_commit_clone = req_commit.clone();
		let commit_thread = std::thread::spawn(move || {
			let res = remove_skill_batch(&req_commit_clone);
			let _ = commit_tx.send(res);
		});

		// Wait on hook: commit thread has completed preflight and will block on mutation lock.
		hook_rx
			.recv_timeout(std::time::Duration::from_secs(5))
			.expect("commit thread must signal hook after unlocked preflight");

		// While commit thread is blocked, add Cursor as a holder:
		let cursor_dir = root.join(".cursor/skills");
		fs::create_dir_all(&cursor_dir).unwrap();
		std::os::unix::fs::symlink(
			root.join(".aghub/notebooklm"),
			cursor_dir.join("notebooklm"),
		)
		.unwrap();

		// Release lock so commit thread proceeds:
		drop(test_lock);

		let res_commit = commit_rx
			.recv_timeout(std::time::Duration::from_secs(5))
			.expect("commit thread should complete after lock release")
			.expect("commit should succeed");
		commit_thread.join().unwrap();

		assert!(
			res_commit.rows.iter().any(|r| r.agent == AgentType::Cursor
				&& r.verdict == Verdict::Removed),
			"Cursor must be in execution results and removed: {:?}",
			res_commit.rows
		);
		assert!(
			!root.join(".cursor/skills/notebooklm").exists(),
			"Cursor referrer must be removed"
		);
		assert!(
			!root.join(".claude/skills/notebooklm").exists(),
			"Claude referrer must be removed"
		);
		assert!(
			!root.join(".aghub/notebooklm").exists(),
			"Master must be GC'd when both holders are removed"
		);
	}
}
