use super::ConfigManager;
use crate::{
	errors::{ConfigError, Result},
	models::McpServer,
	skills::removal::{Layout, PruneStatus, RemovalOutcome, RemovalPlan},
};
use log::info;

impl ConfigManager {
	pub fn add_mcp(&mut self, mcp: McpServer) -> Result<()> {
		if !self.adapter.supports_mcp_operations() {
			return Err(ConfigError::unsupported_operation(
				"add",
				"MCP server",
				self.adapter.name(),
			));
		}
		let agent_name = self.adapter.name().to_string();
		let config = self.config_mut()?;
		if config.mcps.iter().any(|m| m.name == mcp.name) {
			return Err(ConfigError::resource_exists("MCP server", &mcp.name));
		}
		info!("adding MCP '{}' for agent '{}'", mcp.name, agent_name);
		config.mcps.push(mcp);
		self.save_current()
	}

	pub fn get_mcp(&self, name: &str) -> Option<&McpServer> {
		self.config.as_ref()?.mcps.iter().find(|m| m.name == name)
	}

	pub fn update_mcp(&mut self, name: &str, mcp: McpServer) -> Result<()> {
		if !self.adapter.supports_mcp_operations() {
			return Err(ConfigError::unsupported_operation(
				"update",
				"MCP server",
				self.adapter.name(),
			));
		}
		let agent_name = self.adapter.name().to_string();
		let config = self.config_mut()?;
		let index =
			config.mcps.iter().position(|m| m.name == name).ok_or_else(
				|| ConfigError::resource_not_found("MCP server", name),
			)?;
		info!("updating MCP '{}' for agent '{}'", name, agent_name);
		config.mcps[index] = mcp;
		self.save_current()
	}

	/// Plan (and optionally execute) removal of an MCP server, mirroring the
	/// skill `remove_skill_planned` dry-run/confirm gate so all three resource
	/// types flow through one [`RemovalOutcome`] DTO.
	///
	/// MCP removal is a flat config-file rewrite: the plan is a `Layout::Copy`
	/// plan whose single path is the on-disk config file that `save_current`
	/// rewrites. It is never destructive of shared data, so `needs_confirm` is
	/// always false — the gate reduces to `executed == !dry_run`. The
	/// `dry_run`/`confirm` plumbing exists for a UNIFORM wire+CLI shape, not
	/// because MCP removal gates.
	pub fn remove_mcp_planned(
		&mut self,
		name: &str,
		dry_run: bool,
		confirm: bool,
	) -> Result<RemovalOutcome> {
		if !self.adapter.supports_mcp_operations() {
			return Err(ConfigError::unsupported_operation(
				"remove",
				"MCP server",
				self.adapter.name(),
			));
		}
		let agent_name = self.adapter.name().to_string();
		let config = self.config_mut()?;
		let index =
			config.mcps.iter().position(|m| m.name == name).ok_or_else(
				|| ConfigError::resource_not_found("MCP server", name),
			)?;

		// The plan describes the on-disk config file that would be rewritten —
		// the same path `save_current` (via `config_path`) writes to.
		let plan = RemovalPlan {
			layout: Layout::Copy,
			paths: self.config_path().into_iter().collect(),
			skipped: vec![],
			needs_confirm: false,
		};

		let executed = !dry_run && (!plan.needs_confirm || confirm);
		if !executed {
			return Ok(RemovalOutcome {
				plan,
				executed: false,
				prune: PruneStatus::NotRun,
			});
		}

		info!("removing MCP '{}' for agent '{}'", name, agent_name);
		self.config_mut()?.mcps.remove(index);
		self.save_current()?;
		Ok(RemovalOutcome {
			plan,
			executed: true,
			prune: PruneStatus::NotRun,
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
		let config = self.config_mut()?;
		let mcp = config.mcps.iter_mut().find(|m| m.name == name).ok_or_else(
			|| ConfigError::resource_not_found("MCP server", name),
		)?;
		info!(
			"setting MCP '{}' enabled={} for agent '{}'",
			name, enabled, agent_name
		);
		mcp.enabled = enabled;
		self.save_current()
	}

	pub fn disable_mcp(&mut self, name: &str) -> Result<()> {
		self.set_mcp_enabled(name, false)
	}

	pub fn enable_mcp(&mut self, name: &str) -> Result<()> {
		self.set_mcp_enabled(name, true)
	}
}
