use super::ConfigManager;
use crate::{
	errors::{ConfigError, Result},
	models::McpServer,
	skills::removal::{Layout, PruneStatus, RemovalOutcome, RemovalPlan},
	transfer::{self, InstallScope, ResourceLocator},
};
use log::info;

impl ConfigManager {
	/// Direct creates promise that every supplied field survives a reload.
	/// Plain cross-agent copies use `add_mcp` because they keep the source and
	/// may intentionally copy only the fields the target dialect can hold.
	pub fn add_mcp_exact(&mut self, mcp: McpServer) -> Result<()> {
		if self.adapter.supports_mcp_operations()
			&& (mcp.timeout.is_some()
				|| aghub_agents::descriptor::mcp_fit(
					crate::registry::get(self.agent_type()),
					&mcp,
				) != aghub_agents::descriptor::McpFit::Exact)
		{
			return Err(ConfigError::unsupported_operation(
				"add without losing fields",
				"MCP server",
				self.adapter.name(),
			));
		}
		self.add_mcp(mcp)
	}

	pub fn add_mcp(&mut self, mcp: McpServer) -> Result<()> {
		if !self.adapter.supports_mcp_operations() {
			return Err(ConfigError::unsupported_operation(
				"add",
				"MCP server",
				self.adapter.name(),
			));
		}
		let agent_name = self.adapter.name().to_string();
		self.mutate_mcp(|mcps| {
			if mcps.iter().any(|server| server.name == mcp.name) {
				return Err(ConfigError::resource_exists(
					"MCP server",
					&mcp.name,
				));
			}
			info!("adding MCP '{}' for agent '{}'", mcp.name, agent_name);
			mcps.push(mcp);
			Ok(())
		})
	}

	pub fn get_mcp(&self, name: &str) -> Option<&McpServer> {
		self.config.as_ref()?.mcps.iter().find(|m| m.name == name)
	}

	pub fn update_mcp(&mut self, name: &str, mcp: McpServer) -> Result<()> {
		self.update_mcp_with(name, |current| {
			*current = mcp;
			Ok(())
		})
		.map(|_| ())
	}

	/// Apply a partial MCP update to the latest on-disk value while holding the
	/// backing lock. Callers must build their patch here, not from a prior load.
	pub fn update_mcp_with(
		&mut self,
		name: &str,
		change: impl FnOnce(&mut McpServer) -> Result<()>,
	) -> Result<McpServer> {
		if !self.adapter.supports_mcp_operations() {
			return Err(ConfigError::unsupported_operation(
				"update",
				"MCP server",
				self.adapter.name(),
			));
		}
		let agent_name = self.adapter.name().to_string();
		let agent = self.agent_type();
		let project_root = self.project_root.clone();
		let write_scope = self.write_scope;
		self.mutate_mcp(|mcps| {
			let index = mcps
				.iter()
				.position(|server| server.name == name)
				.ok_or_else(|| {
					ConfigError::resource_not_found("MCP server", name)
				})?;
			let mut updated = mcps[index].clone();
			change(&mut updated)?;
			if mcps.iter().enumerate().any(|(other, server)| {
				other != index && server.name == updated.name
			}) {
				return Err(ConfigError::resource_exists(
					"MCP server",
					&updated.name,
				));
			}
			// Edits promise that the returned value is persisted. The dialect
			// probe covers transport fields and enabled state; the separate
			// model-level timeout is not written by any current MCP dialect.
			if updated.timeout.is_some()
				|| aghub_agents::descriptor::mcp_fit(
					crate::registry::get(agent),
					&updated,
				) != aghub_agents::descriptor::McpFit::Exact
			{
				return Err(ConfigError::unsupported_operation(
					"update without losing fields",
					"MCP server",
					&agent_name,
				));
			}
			if updated.name != name {
				// Serializers merge native fields by entry name. A rename to a
				// new name would drop fields outside the normalized model.
				let path = crate::create_adapter(agent)
					.mcp_config_path(project_root.as_deref(), write_scope)
					.ok_or_else(|| {
						ConfigError::InvalidConfig(
							"MCP source path unavailable".to_string(),
						)
					})?;
				let original = std::fs::read_to_string(path)?;
				let serialize = crate::registry::get(agent)
					.mcp_serialize_config
					.ok_or_else(|| {
						ConfigError::InvalidConfig(
							"MCP serializer unavailable".to_string(),
						)
					})?;
				if aghub_agents::format::unmanaged_mcp_source_fields(
					&mcps[index],
					&original,
					serialize,
				)? {
					return Err(ConfigError::InvalidConfig(format!(
						"MCP server '{}' has unmanaged native fields that cannot be preserved by renaming it",
						name
					)));
				}
			}
			info!("updating MCP '{}' for agent '{}'", name, agent_name);
			mcps[index] = updated.clone();
			Ok(updated)
		})
	}

	/// Plan (and optionally execute) removal of an MCP server, mirroring the
	/// skill `remove_skill_planned` dry-run/confirm gate so all three resource
	/// types flow through one [`RemovalOutcome`] DTO.
	///
	/// MCP removal is a flat config-file rewrite: it deletes NO on-disk path
	/// (only a JSON entry out of the shared config file, which persists), so the
	/// `Layout::Copy` plan carries an EMPTY `paths` and `deleted_path` stays
	/// null. The shared-reader guard is separate from confirmation, so
	/// `needs_confirm` stays false — the gate reduces to `executed == !dry_run`. The
	/// `dry_run`/`confirm` plumbing exists for a UNIFORM wire+CLI shape, not
	/// because MCP removal gates.
	pub fn remove_mcp_planned(
		&mut self,
		name: &str,
		dry_run: bool,
		confirm: bool,
	) -> Result<RemovalOutcome> {
		self.remove_mcp_planned_checked(name, dry_run, confirm, |_| Ok(()))
	}

	/// Direct delete protects every agent outside `requested` that reads this
	/// backing. `requested` is the whole set one request removes the server
	/// from and must include this manager's agent. Reconcile uses
	/// `remove_mcp_planned` after its own full-set preflight.
	pub fn remove_mcp_planned_single_guarded(
		&mut self,
		name: &str,
		dry_run: bool,
		confirm: bool,
		requested: &[crate::models::AgentType],
	) -> Result<RemovalOutcome> {
		if !requested.contains(&self.agent_type()) {
			return Err(ConfigError::InvalidConfig(
				"removal request does not include the target agent".into(),
			));
		}
		let scope = match self.write_scope {
			crate::models::ResourceScope::GlobalOnly => InstallScope::Global,
			crate::models::ResourceScope::ProjectOnly => InstallScope::Project,
			crate::models::ResourceScope::Both => {
				return Err(ConfigError::InvalidConfig(
					"MCP removal requires one write scope".to_string(),
				))
			}
		};
		let source = ResourceLocator {
			agent: self.agent_type(),
			scope,
			project_root: self.project_root.clone(),
			name: name.to_string(),
		};
		self.remove_mcp_planned_checked(name, dry_run, confirm, |mcps| {
			if mcps.iter().any(|server| server.name == name) {
				transfer::ensure_mcp_delete_spares(&source, requested)
			} else {
				Ok(())
			}
		})
	}

	/// Run a shared-reader safety check under the same lock as the removal.
	/// The caller supplies the policy (for example, reconcile's full roster
	/// check); this method owns its placement before the fresh read and write.
	pub(crate) fn remove_mcp_planned_checked(
		&mut self,
		name: &str,
		dry_run: bool,
		confirm: bool,
		preflight: impl FnOnce(&[McpServer]) -> Result<()>,
	) -> Result<RemovalOutcome> {
		if !self.adapter.supports_mcp_operations() {
			return Err(ConfigError::unsupported_operation(
				"remove",
				"MCP server",
				self.adapter.name(),
			));
		}
		let agent_name = self.adapter.name().to_string();
		// An MCP removal deletes NO on-disk path — it rewrites a JSON entry out
		// of the shared config file (which persists). `paths` stays empty so
		// `deleted_path` is null; otherwise the preview would claim the whole
		// config file (e.g. `~/.claude.json`) is being deleted.
		let plan = RemovalPlan {
			layout: Layout::Copy,
			paths: vec![],
			skipped: vec![],
			needs_confirm: false,
			shared_master_kept: false,
			still_read_from: Vec::new(),
			incomplete: false,
		};

		let executed = !dry_run && (!plan.needs_confirm || confirm);
		if !executed {
			// Preview only reads; the executing path holds the lock across its
			// fresh read, shared-reader guard and rewrite.
			let current = self
				.adapter
				.load_mcps(self.project_root.as_deref(), self.write_scope)?;
			self.config_mut()?.mcps = current;
			if !self.config_mut()?.mcps.iter().any(|m| m.name == name) {
				return Err(ConfigError::resource_not_found(
					"MCP server",
					name,
				));
			}
			preflight(&self.config_mut()?.mcps)?;
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

		info!("removing MCP '{}' for agent '{}'", name, agent_name);
		self.mutate_mcp_checked(preflight, |mcps| {
			let index = mcps
				.iter()
				.position(|server| server.name == name)
				.ok_or_else(|| {
					ConfigError::resource_not_found("MCP server", name)
				})?;
			mcps.remove(index);
			Ok(())
		})?;
		Ok(RemovalOutcome {
			plan,
			executed: true,
			prune: PruneStatus::NotRun,
			failed_paths: vec![],
			absent: false,
		})
	}

	pub fn remove_mcp(&mut self, name: &str) -> Result<()> {
		self.remove_mcp_planned(name, false, true).map(|_| ())
	}

	fn set_mcp_enabled(&mut self, name: &str, enabled: bool) -> Result<()> {
		if !self.adapter.supports_mcp_enable_disable() {
			return Err(ConfigError::unsupported_operation(
				if enabled { "enable" } else { "disable" },
				"MCP server",
				self.adapter.name(),
			));
		}
		let agent_name = self.adapter.name().to_string();
		self.mutate_mcp(|mcps| {
			let mcp = mcps
				.iter_mut()
				.find(|server| server.name == name)
				.ok_or_else(|| {
					ConfigError::resource_not_found("MCP server", name)
				})?;
			info!(
				"setting MCP '{}' enabled={} for agent '{}'",
				name, enabled, agent_name
			);
			mcp.enabled = enabled;
			Ok(())
		})
	}

	pub fn disable_mcp(&mut self, name: &str) -> Result<()> {
		self.set_mcp_enabled(name, false)
	}

	pub fn enable_mcp(&mut self, name: &str) -> Result<()> {
		self.set_mcp_enabled(name, true)
	}

	/// Serialize the full read → mutate → write span across threads and aghub
	/// processes that target the same physical config file.
	fn mutate_mcp<T>(
		&mut self,
		change: impl FnOnce(&mut Vec<McpServer>) -> Result<T>,
	) -> Result<T> {
		self.mutate_mcp_checked(|_| Ok(()), change)
	}

	fn mutate_mcp_checked<T>(
		&mut self,
		preflight: impl FnOnce(&[McpServer]) -> Result<()>,
		change: impl FnOnce(&mut Vec<McpServer>) -> Result<T>,
	) -> Result<T> {
		if self.config.is_none() {
			return Err(ConfigError::InvalidConfig(
				"No configuration loaded".to_string(),
			));
		}
		let _guards = self.mcp_write_guards()?;
		let current = self
			.adapter
			.load_mcps(self.project_root.as_deref(), self.write_scope)?;
		let config = self.config_mut()?;
		config.mcps = current;
		preflight(&config.mcps)?;
		let previous = config.mcps.clone();
		let result = match change(&mut config.mcps) {
			Ok(result) => result,
			Err(error) => {
				config.mcps = previous;
				return Err(error);
			}
		};
		if let Err(error) =
			self.save_unlocked(self.config.as_ref().expect("loaded above"))
		{
			self.config_mut()?.mcps = previous;
			return Err(error);
		}
		Ok(result)
	}

	/// Lock an MCP write through [`ConfigManager::scoped_write_guard`].
	///
	/// `None` when this manager writes an overridden path (`TestConfig`): the
	/// file is private to that manager and has no scope lock to share.
	pub(super) fn mcp_write_guards(
		&self,
	) -> Result<Option<::skill::lock::MutationGuard>> {
		let Some(path) = self.config_path() else {
			return Ok(None);
		};
		let descriptor = crate::registry::get(self.agent_type());
		let native =
			descriptor.mcp_path(self.project_root.as_deref(), self.write_scope);
		if native.as_ref() != Some(&path) {
			return Ok(None);
		}
		self.scoped_write_guard("MCP write").map(Some)
	}
}

#[cfg(all(test, unix))]
mod tests {
	use crate::models::{AgentType, ResourceScope};
	use crate::skills::prune::test_lock::env_lock;
	use crate::{create_adapter, ConfigManager};

	struct EnvVarGuard(&'static str, Option<std::ffi::OsString>);

	impl EnvVarGuard {
		fn set(key: &'static str, value: &std::path::Path) -> Self {
			let old = std::env::var_os(key);
			std::env::set_var(key, value);
			Self(key, old)
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

	/// Acquisition never degrades to unlocked: every real descriptor, in every
	/// scope it can write MCPs to, takes the mutation lock. Only a manager
	/// whose adapter overrides the path (`TestConfig`) goes without.
	#[test]
	fn every_mcp_writer_takes_the_mutation_lock() {
		let _env = env_lock().lock().unwrap_or_else(|e| e.into_inner());
		let home = tempfile::tempdir().unwrap();
		let _home = EnvVarGuard::set("HOME", home.path());
		let _config =
			EnvVarGuard::set("XDG_CONFIG_HOME", &home.path().join(".config"));
		let _state =
			EnvVarGuard::set("XDG_STATE_HOME", &home.path().join(".state"));
		let project = tempfile::tempdir().unwrap();
		let mut checked = 0;
		for &agent in AgentType::ALL {
			for (global, scope) in [
				(true, ResourceScope::GlobalOnly),
				(false, ResourceScope::ProjectOnly),
			] {
				let manager = ConfigManager::new(
					create_adapter(agent),
					global,
					(!global).then_some(project.path()),
				);
				if !manager.adapter.supports_mcp_scope(scope)
					|| manager.config_path().is_none()
				{
					continue;
				}
				checked += 1;
				assert!(
					manager.mcp_write_guards().unwrap().is_some(),
					"{} {scope:?} writes MCPs without the mutation lock",
					agent.as_str()
				);
			}
		}
		assert!(checked > 20, "only {checked} writers checked");
	}
}
