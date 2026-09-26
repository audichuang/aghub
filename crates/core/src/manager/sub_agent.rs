use crate::{
	errors::{ConfigError, Result},
	models::SubAgent,
	skills::removal::{Layout, PruneStatus, RemovalOutcome, RemovalPlan},
};
use log::{info, warn};
use std::path::{Path, PathBuf};

use super::ConfigManager;

impl ConfigManager {
	/// Every sub-agent mutation reloads after taking the scope's mutation lock
	/// (see [`ConfigManager::scoped_write_guard`]): a manager's earlier
	/// `load()` may predate another aghub process's write.
	fn guard_and_reload_sub_agents(
		&mut self,
	) -> Result<::skill::lock::MutationGuard> {
		if self.config.is_none() {
			return Err(ConfigError::InvalidConfig(
				"No configuration loaded".to_string(),
			));
		}
		if !self.adapter.supports_sub_agent_scope(self.write_scope) {
			return Err(ConfigError::unsupported_operation(
				"mutate",
				"sub-agent",
				self.adapter.name(),
			));
		}
		let guard = self.scoped_write_guard("sub-agent write")?;
		self.reload_sub_agents()?;
		Ok(guard)
	}

	fn reload_sub_agents(&mut self) -> Result<()> {
		let current = self
			.adapter
			.load_sub_agents(self.project_root.as_deref(), self.write_scope)?;
		self.config_mut()?.sub_agents = current;
		Ok(())
	}

	/// List all loaded sub-agents.
	pub fn list_sub_agents(&self) -> Vec<&SubAgent> {
		self.config
			.as_ref()
			.map(|c| c.sub_agents.iter().collect())
			.unwrap_or_default()
	}

	/// Get a single sub-agent by name.
	pub fn get_sub_agent(&self, name: &str) -> Option<&SubAgent> {
		self.config
			.as_ref()
			.and_then(|c| c.sub_agents.iter().find(|a| a.name == name))
	}

	/// Add a new sub-agent and persist via the adapter.
	pub fn add_sub_agent(&mut self, agent: SubAgent) -> Result<()> {
		let _guard = self.guard_and_reload_sub_agents()?;
		let previous = self.config_mut()?.sub_agents.clone();
		{
			let config = self.config_mut()?;
			if config.sub_agents.iter().any(|a| a.name == agent.name) {
				return Err(ConfigError::resource_exists(
					"sub_agent",
					&agent.name,
				));
			}
			config.sub_agents.push(agent);
		}
		info!(
			"added sub-agent, saving for agent '{}' in scope {:?}",
			self.adapter.name(),
			self.write_scope
		);
		let updated = self
			.config
			.as_ref()
			.and_then(|config| config.sub_agents.last())
			.expect("agent pushed above");
		if let Err(error) = self.save_sub_agent_entry(updated) {
			self.config_mut()?.sub_agents = previous;
			return Err(error);
		}
		Ok(())
	}

	/// Patch an existing sub-agent by name and persist via the adapter.
	///
	/// Only the fields present in `patch` are overwritten; omitted fields keep
	/// their current value from a fresh read under the backing-directory lock.
	pub fn update_sub_agent(
		&mut self,
		name: &str,
		patch: SubAgentPatch,
	) -> Result<()> {
		let _guard = self.guard_and_reload_sub_agents()?;
		let previous = self.config_mut()?.sub_agents.clone();
		// Capture the old path before patching; a rename may write to a
		// different file, or to this same file when names sanitize identically.
		let old_source_path = self
			.config
			.as_ref()
			.and_then(|c| c.sub_agents.iter().find(|a| a.name == name))
			.and_then(|a| a.source_path.clone());
		let name_changed =
			patch.name.as_deref().map(|n| n != name).unwrap_or(false);
		let effective_name =
			patch.name.clone().unwrap_or_else(|| name.to_string());
		if let Some(new_name) = patch.name.as_deref().filter(|_| name_changed) {
			if self.get_sub_agent(new_name).is_some() {
				return Err(ConfigError::resource_exists(
					"sub_agent",
					new_name,
				));
			}
		}

		{
			let config = self.config_mut()?;
			let agent = config
				.sub_agents
				.iter_mut()
				.find(|a| a.name == name)
				.ok_or_else(|| {
				ConfigError::resource_not_found("sub_agent", name)
			})?;
			patch.apply_to(agent);
		}

		info!(
			"updated sub-agent '{}', saving for agent '{}' in scope {:?}",
			name,
			self.adapter.name(),
			self.write_scope
		);
		let updated = self
			.get_sub_agent(&effective_name)
			.expect("agent patched above");
		if let Err(error) = self.save_sub_agent_entry(updated) {
			self.config_mut()?.sub_agents = previous;
			return Err(error);
		}

		// Remove stale file when the name changed (a new file was written
		// under the new name by save_sub_agent_entry). `save_scoped_sub_agents`
		// does NOT delete stale files, so a left-behind old `.md` reappears as a
		// phantom agent on reload. A non-NotFound delete failure is therefore
		// actionable — surface it (do not report success) so the caller knows the
		// orphan lingers; an already-gone file is idempotent success. Mirrors the
		// removal contract in `remove_sub_agent_planned`.
		if name_changed {
			if let Some(old_path) = old_source_path {
				let new_path = self
					.adapter
					.load_sub_agents(
						self.project_root.as_deref(),
						self.write_scope,
					)?
					.into_iter()
					.find(|agent| agent.name == effective_name)
					.and_then(|agent| agent.source_path)
					.ok_or_else(|| {
						ConfigError::InvalidConfig(format!(
							"Renamed sub-agent '{effective_name}' could not be read back"
						))
					})?;
				if ::skill::lock::resolve_existing(Path::new(&old_path))
					!= ::skill::lock::resolve_existing(Path::new(&new_path))
				{
					match std::fs::remove_file(&old_path) {
						Ok(()) => {}
						Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
						Err(e) => {
							warn!(
								"failed to delete stale sub-agent file '{}': {}",
								old_path, e
							);
							return Err(ConfigError::Io(e));
						}
					}
				}
			}
		}

		Ok(())
	}

	/// Plan (and optionally execute) removal of a sub-agent, mirroring the
	/// skill `remove_skill_planned` dry-run/confirm gate so all three resource
	/// types flow through one [`RemovalOutcome`] DTO.
	///
	/// Sub-agent removal is a flat operation: the plan is a `Layout::Copy` plan
	/// whose paths are the backing source `.md` file (empty for a config-only
	/// agent that was never persisted). It is never destructive of shared data,
	/// so `needs_confirm` is always false — the gate reduces to
	/// `executed == !dry_run`. The `dry_run`/`confirm` plumbing exists for a
	/// UNIFORM wire+CLI shape, not because sub-agent removal gates.
	pub fn remove_sub_agent_planned(
		&mut self,
		name: &str,
		dry_run: bool,
		confirm: bool,
	) -> Result<RemovalOutcome> {
		self.remove_sub_agent_planned_checked(name, dry_run, confirm, None)
	}

	/// Reconcile captured `expected` before copying it elsewhere. Refuse to
	/// remove that source if it changed during the copy stage.
	pub(crate) fn remove_sub_agent_if_unchanged(
		&mut self,
		name: &str,
		expected: &SubAgent,
		copying: bool,
	) -> Result<()> {
		self.remove_sub_agent_planned_checked(
			name,
			false,
			true,
			Some((expected, copying)),
		)
		.map(|_| ())
	}

	fn remove_sub_agent_planned_checked(
		&mut self,
		name: &str,
		dry_run: bool,
		confirm: bool,
		expected: Option<(&SubAgent, bool)>,
	) -> Result<RemovalOutcome> {
		// The executing path holds the physical backing lock across the fresh
		// read, source comparison, tombstone move, save, and rollback. Preview
		// refreshes without blocking another writer.
		let _guard = if dry_run {
			self.reload_sub_agents()?;
			None
		} else {
			Some(self.guard_and_reload_sub_agents()?)
		};
		// Capture the source path before mutating so we can delete the file.
		let source_path = self
			.config
			.as_ref()
			.and_then(|c| c.sub_agents.iter().find(|a| a.name == name))
			.and_then(|a| a.source_path.clone());

		// The plan describes the backing file that would be deleted. A
		// config-only agent (never written to disk) has no path.
		let paths: Vec<PathBuf> =
			source_path.iter().map(PathBuf::from).collect();
		let plan = RemovalPlan {
			layout: Layout::Copy,
			paths,
			skipped: vec![],
			needs_confirm: false,
			shared_master_kept: false,
			still_read_from: Vec::new(),
			incomplete: false,
		};

		// Determine presence up front so a dry-run still surfaces NotFound
		// (mirrors remove_skill_planned, which finds the skill before gating).
		if !self
			.config
			.as_ref()
			.is_some_and(|c| c.sub_agents.iter().any(|a| a.name == name))
		{
			return Err(ConfigError::resource_not_found("sub_agent", name));
		}
		if let Some((expected, copying)) = expected {
			let current =
				self.get_sub_agent(name).expect("presence checked above");
			if !same_sub_agent_source(current, expected) {
				return Err(ConfigError::InvalidConfig(format!(
					"Sub-agent '{name}' changed during reconcile; the original was not removed"
				)));
			}
			if copying && self.agent_type() == crate::models::AgentType::Codex {
				let path = current.source_path.as_deref().ok_or_else(|| {
					ConfigError::InvalidConfig(
						"Codex sub-agent source path unavailable".to_string(),
					)
				})?;
				if aghub_agents::agents::codex::sub_agent_has_unmanaged_fields(
					Path::new(path),
				)? {
					return Err(ConfigError::InvalidConfig(format!(
						"Sub-agent '{name}' gained unmanaged fields during reconcile; the original was not removed"
					)));
				}
			}
		}

		let executed = !dry_run && (!plan.needs_confirm || confirm);
		if !executed {
			return Ok(RemovalOutcome {
				plan,
				executed: false,
				prune: PruneStatus::NotRun,
				// Reached only AFTER the not-found check: the resource
				// exists, it just was not removed (dry-run/unconfirmed).
				failed_paths: vec![],
				absent: false,
			});
		}

		// Move the backing file to a tombstone FIRST, before mutating/saving
		// in-memory state. Unlike skills, `save_scoped_sub_agents` does NOT
		// delete stale files (crates/agents/src/sub_agents.rs), so a file left on
		// disk after an in-memory removal reappears as a phantom agent on the
		// next reload — and conversely, deleting it outright before the save
		// would lose the user's data if the save then fails. So this is
		// transactional: rename → mutate + save → on success drop the tombstone,
		// on save failure RESTORE it and re-insert the agent, so a reported
		// failure means nothing changed. A non-NotFound rename error surfaces and
		// leaves state untouched; an already-gone file is idempotent success.
		let mut tombstones: Vec<(PathBuf, PathBuf)> = Vec::new();
		for path in &plan.paths {
			let tomb = path.with_extension("md.aghub-tomb");
			match std::fs::symlink_metadata(&tomb) {
				Ok(_) => {
					return Err(ConfigError::InvalidConfig(format!(
						"Sub-agent recovery tombstone already exists: {}",
						tomb.display()
					)));
				}
				Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
				Err(e) => return Err(ConfigError::Io(e)),
			}
			match std::fs::rename(path, &tomb) {
				Ok(()) => tombstones.push((path.clone(), tomb)),
				Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
				Err(e) => {
					warn!("failed removal of '{}': {}", path.display(), e);
					// Best-effort restore of earlier tombstones before
					// bailing. The original rename error is the actionable
					// root cause we return; a restore that itself fails
					// leaves that file parked as a tomb, so warn (don't
					// swallow) so the orphan stays visible in logs.
					restore_tombstones(&tombstones);
					return Err(ConfigError::Io(e));
				}
			}
		}

		// Snapshot the agent so we can re-insert it if the save fails.
		let removed = self
			.config
			.as_ref()
			.and_then(|c| c.sub_agents.iter().find(|a| a.name == name))
			.cloned();
		{
			let config = self.config_mut()?;
			config.sub_agents.retain(|a| a.name != name);
		}

		info!(
			"removed sub-agent '{}', saving for agent '{}' in scope {:?}",
			name,
			self.adapter.name(),
			self.write_scope
		);
		if let Err(e) = self.save_sub_agents_current() {
			// Roll back: restore the on-disk file(s) and the in-memory agent so a
			// reported failure leaves no data lost and no phantom orphan. The save
			// error is the actionable root cause; a restore that itself fails is
			// surfaced via warn! (it leaves a parked tomb) rather than swallowed.
			restore_tombstones(&tombstones);
			if let (Some(agent), Some(config)) = (removed, self.config.as_mut())
			{
				config.sub_agents.push(agent);
			}
			return Err(e);
		}

		// Save succeeded — drop the tombstones permanently. A leftover
		// `.md.aghub-tomb` is litter, not data loss (the agent IS gone), so a
		// cleanup failure must NOT be reported as a clean success: surface it as
		// an actionable error (an already-gone tomb is benign NotFound).
		drop_tombstones(&tombstones)?;

		Ok(RemovalOutcome {
			plan,
			executed: true,
			prune: PruneStatus::NotRun,
			failed_paths: vec![],
			absent: false,
		})
	}

	/// Remove a sub-agent by name and persist via the adapter.
	pub fn remove_sub_agent(&mut self, name: &str) -> Result<()> {
		self.remove_sub_agent_planned(name, false, true).map(|_| ())
	}

	/// Each built-in descriptor saves sub-agents as individual files. Writing
	/// only the changed entry avoids touching unrelated siblings and prevents a
	/// failed create from leaving renamed copies of those siblings behind.
	fn save_sub_agent_entry(&self, agent: &SubAgent) -> Result<()> {
		self.adapter.save_sub_agents(
			self.project_root.as_deref(),
			self.write_scope,
			std::slice::from_ref(agent),
		)
	}
}

/// Delete the tombstones of a removal whose save succeeded. A leftover tomb is
/// litter the caller must hear about, so any failure but NotFound surfaces.
fn drop_tombstones(tombstones: &[(PathBuf, PathBuf)]) -> Result<()> {
	for (_, tomb) in tombstones {
		match std::fs::remove_file(tomb) {
			Ok(()) => {}
			Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
			Err(e) => {
				warn!(
					"failed to clean up tombstone '{}': {}",
					tomb.display(),
					e
				);
				return Err(ConfigError::Io(e));
			}
		}
	}
	Ok(())
}

/// Best-effort restore of `(orig, tomb)` pairs on a removal error path: rename
/// each tombstone back to its original. Used only when an earlier error is
/// already being returned, so a restore that itself fails is logged (the file
/// stays parked as a `.aghub-tomb`) rather than swallowed — the returned error
/// is the actionable root cause, the warn surfaces the parked orphan.
fn restore_tombstones(tombstones: &[(PathBuf, PathBuf)]) {
	for (orig, tomb) in tombstones {
		if let Err(e) = std::fs::rename(tomb, orig) {
			warn!(
				"failed to restore tombstone '{}' -> '{}': {}",
				tomb.display(),
				orig.display(),
				e
			);
		}
	}
}

fn same_sub_agent_source(current: &SubAgent, expected: &SubAgent) -> bool {
	let same_path = match (
		current.source_path.as_deref(),
		expected.source_path.as_deref(),
	) {
		(Some(current), Some(expected)) => {
			::skill::lock::resolve_existing(Path::new(current))
				== ::skill::lock::resolve_existing(Path::new(expected))
		}
		(None, None) => true,
		_ => false,
	};
	current.name == expected.name
		&& current.description == expected.description
		&& current.instruction == expected.instruction
		&& current.extra_frontmatter == expected.extra_frontmatter
		&& same_path
}

/// Patch DTO used by `update_sub_agent` — all fields are optional so only
/// the provided ones are overwritten.
#[derive(Debug, Default)]
pub struct SubAgentPatch {
	pub name: Option<String>,
	pub description: Option<String>,
	pub instruction: Option<String>,
}

impl SubAgentPatch {
	fn apply_to(self, agent: &mut SubAgent) {
		if let Some(name) = self.name {
			agent.name = name;
		}
		if let Some(desc) = self.description {
			agent.description = Some(desc);
		}
		if let Some(instr) = self.instruction {
			agent.instruction = Some(instr);
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::{create_adapter, models::AgentType};

	/// A tomb that cannot be removed (here: it is a directory, EISDIR on every
	/// platform and as root) must not be reported as a clean success; an
	/// already-gone tomb is.
	#[test]
	fn tombstone_cleanup_failure_is_not_clean_success() {
		let tmp = tempfile::tempdir().unwrap();
		let stuck = tmp.path().join("stuck.md.aghub-tomb");
		std::fs::create_dir(&stuck).unwrap();
		let gone = tmp.path().join("gone.md.aghub-tomb");
		assert!(matches!(
			drop_tombstones(&[(PathBuf::new(), stuck.clone())]),
			Err(ConfigError::Io(_))
		));
		assert!(stuck.exists());
		drop_tombstones(&[(PathBuf::new(), gone)]).unwrap();
	}

	fn manager(root: &Path, agent: AgentType) -> ConfigManager {
		let mut manager =
			ConfigManager::new(create_adapter(agent), false, Some(root));
		manager.load().unwrap();
		manager
	}

	#[test]
	fn conditional_removal_preserves_a_source_changed_after_copy_snapshot() {
		let tmp = tempfile::tempdir().unwrap();
		let root = tmp.path();
		let mut original = manager(root, AgentType::Claude);
		let mut agent = SubAgent::new("reviewer");
		agent.instruction = Some("original".into());
		original.add_sub_agent(agent).unwrap();
		original.load().unwrap();
		let expected = original.get_sub_agent("reviewer").unwrap().clone();
		let mut concurrent = manager(root, AgentType::Claude);
		concurrent
			.update_sub_agent(
				"reviewer",
				SubAgentPatch {
					instruction: Some("newer change".into()),
					..Default::default()
				},
			)
			.unwrap();
		let error = original
			.remove_sub_agent_if_unchanged("reviewer", &expected, true)
			.unwrap_err();
		assert!(matches!(error, ConfigError::InvalidConfig(_)));
		let content =
			std::fs::read_to_string(root.join(".claude/agents/reviewer.md"))
				.unwrap();
		assert!(content.contains("newer change"), "{content}");
	}

	#[test]
	fn conditional_removal_rechecks_unmodelled_codex_fields() {
		let tmp = tempfile::tempdir().unwrap();
		let root = tmp.path();
		let file = root.join(".codex/agents/reviewer.toml");
		std::fs::create_dir_all(file.parent().unwrap()).unwrap();
		std::fs::write(
			&file,
			"name = \"reviewer\"\ndescription = \"review\"\ndeveloper_instructions = \"original\"\n",
		)
		.unwrap();
		let mut original = manager(root, AgentType::Codex);
		let expected = original.get_sub_agent("reviewer").unwrap().clone();
		std::fs::write(
			&file,
			"name = \"reviewer\"\ndescription = \"review\"\ndeveloper_instructions = \"original\"\nmodel = \"newer-native-value\"\n",
		)
		.unwrap();
		let fresh = manager(root, AgentType::Codex);
		assert!(same_sub_agent_source(
			fresh.get_sub_agent("reviewer").unwrap(),
			&expected,
		));
		let error = original
			.remove_sub_agent_if_unchanged("reviewer", &expected, true)
			.unwrap_err();
		assert!(matches!(error, ConfigError::InvalidConfig(_)));
		let content = std::fs::read_to_string(&file).unwrap();
		assert!(content.contains("newer-native-value"), "{content}");
	}
}
