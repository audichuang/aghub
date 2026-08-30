use crate::{
	errors::{ConfigError, Result},
	models::SubAgent,
	skills::removal::{Layout, PruneStatus, RemovalOutcome, RemovalPlan},
};
use log::{info, warn};
use std::path::PathBuf;

use super::ConfigManager;

impl ConfigManager {
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
		self.save_sub_agents_current()
	}

	/// Patch an existing sub-agent by name and persist via the adapter.
	///
	/// Only the fields present in `patch` are overwritten; omitted fields keep
	/// their current value (true PATCH semantics — the config file is **not**
	/// re-scanned before the write).
	pub fn update_sub_agent(
		&mut self,
		name: &str,
		patch: SubAgentPatch,
	) -> Result<()> {
		// If the name changes we need to remove the old file first.
		let old_source_path = self
			.config
			.as_ref()
			.and_then(|c| c.sub_agents.iter().find(|a| a.name == name))
			.and_then(|a| a.source_path.clone());
		let name_changed =
			patch.name.as_deref().map(|n| n != name).unwrap_or(false);

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
		self.save_sub_agents_current()?;

		// Remove stale file when the name changed (a new file was written
		// under the new name by save_sub_agents_current). `save_scoped_sub_agents`
		// does NOT delete stale files, so a left-behind old `.md` reappears as a
		// phantom agent on reload. A non-NotFound delete failure is therefore
		// actionable — surface it (do not report success) so the caller knows the
		// orphan lingers; an already-gone file is idempotent success. Mirrors the
		// removal contract in `remove_sub_agent_planned`.
		if name_changed {
			if let Some(old_path) = old_source_path {
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
		for (_, tomb) in &tombstones {
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
