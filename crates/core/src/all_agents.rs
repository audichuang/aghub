use crate::{
	adapters::AgentAdapter,
	manager::ConfigManager,
	models::{ConfigSource, McpServer, ResourceScope, Skill, SubAgent},
	registry,
};
use log::{debug, warn};
use std::path::Path;

/// Resources loaded for a single agent
pub struct AgentResources {
	pub agent_id: &'static str,
	pub skills: Vec<Skill>,
	pub mcps: Vec<McpServer>,
	pub sub_agents: Vec<SubAgent>,
	/// This agent's config could NOT be read, so the empty lists above mean
	/// "unknown", not "nothing".
	///
	/// The loader fails OPEN (right for a listing). Callers that DECIDE
	/// something must read this flag — e.g. `transfer::skill_holders` would
	/// otherwise count an unreadable agent as a non-reader and delete its master.
	pub load_failed: bool,
}

/// Load resources for all registered agents.
///
/// Agents with no config or a missing config file return empty skills/mcps rather
/// than propagating an error. A malformed config file is also silently skipped.
pub fn load_all_agents(
	scope: ResourceScope,
	project_root: Option<&Path>,
) -> Vec<AgentResources> {
	debug!("loading resources for all agents in scope {:?}", scope);
	registry::iter_all()
		.map(|descriptor| {
			let adapter: Box<dyn AgentAdapter> = Box::new(descriptor);
			let is_global = scope == ResourceScope::GlobalOnly
				|| scope == ResourceScope::Both;
			let mut manager = ConfigManager::with_scope(
				adapter,
				is_global,
				project_root,
				scope,
			);
			if scope == ResourceScope::Both {
				match manager.load_both_annotated_checked() {
					Ok(((skills, mcps, sub_agents), any_failed)) => {
						AgentResources {
							agent_id: descriptor.id,
							skills,
							mcps,
							// The merge fails OPEN per scope, so an `Ok` here
							// can still hide a scope that did not load.
							load_failed: any_failed,
							sub_agents,
						}
					}
					Err(error) => {
						warn!(
							"failed to load both-scope resources for agent '{}': {}",
							descriptor.id,
							error
						);
						AgentResources {
							agent_id: descriptor.id,
							skills: vec![],
							mcps: vec![],
							sub_agents: vec![],
							load_failed: true,
						}
					}
				}
			} else {
				match manager.load() {
					Ok(config) => {
						let config_source = match scope {
							ResourceScope::GlobalOnly => {
								Some(ConfigSource::Global)
							}
							ResourceScope::ProjectOnly => {
								Some(ConfigSource::Project)
							}
							_ => None,
						};
						let skills: Vec<Skill> = config
							.skills
							.iter()
							.cloned()
							.map(|mut s| {
								s.config_source = config_source;
								s
							})
							.collect();
						let sub_agents: Vec<SubAgent> = config
							.sub_agents
							.iter()
							.cloned()
							.map(|mut a| {
								a.config_source = config_source;
								a
							})
							.collect();
						AgentResources {
							agent_id: descriptor.id,
							skills,
							mcps: config
								.mcps
								.iter()
								.cloned()
								.map(|mut m| {
									m.config_source = config_source;
									m
								})
								.collect(),
							sub_agents,
							load_failed: false,
						}
					}
					Err(error) => {
						warn!(
							"failed to load resources for agent '{}': {}",
							descriptor.id, error
						);
						AgentResources {
							agent_id: descriptor.id,
							skills: vec![],
							mcps: vec![],
							sub_agents: vec![],
							load_failed: true,
						}
					}
				}
			}
		})
		.collect()
}

/// [`load_all_agents`] minus the agents the user disabled
/// ([`crate::agent_settings`]).
///
/// For fan-outs that WRITE to the agents they find (resync, rename). A caller
/// asking "who else still holds this" must keep [`load_all_agents`]: a
/// disabled agent is unmanaged, not absent, and dropping it there would delete
/// a Master it still reads.
pub fn load_managed_agents(
	scope: ResourceScope,
	project_root: Option<&Path>,
) -> Vec<AgentResources> {
	let disabled = crate::agent_settings::disabled_agents();
	load_all_agents(scope, project_root)
		.into_iter()
		.filter(|agent| !disabled.contains(agent.agent_id))
		.collect()
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn managed_load_drops_only_disabled_agents() {
		let tmp = tempfile::tempdir().unwrap();
		let load = || {
			load_managed_agents(ResourceScope::ProjectOnly, Some(tmp.path()))
		};
		assert_eq!(load().len(), registry::ALL_AGENTS.len());

		let _off = crate::agent_settings::test_override::disable(&["claude"]);
		let managed = load();
		assert!(managed.iter().all(|a| a.agent_id != "claude"));
		assert_eq!(managed.len(), registry::ALL_AGENTS.len() - 1);
	}
}
