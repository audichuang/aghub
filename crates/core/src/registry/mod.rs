use aghub_agents::{agents, AgentDescriptor, AgentType};

/// The shipped descriptors. Single-sourced from `aghub_agents::agents` so this
/// is not a second hand-written roster that can silently drift from
/// `AgentType::ALL` — see `tests/registry_bijection.rs`.
pub static ALL_AGENTS: &[&AgentDescriptor] = agents::ALL_DESCRIPTORS;

pub fn get(agent_type: AgentType) -> &'static AgentDescriptor {
	let id = agent_type.as_str();
	ALL_AGENTS
		.iter()
		.find(|d| d.id == id)
		.copied()
		.unwrap_or(&agents::claude::DESCRIPTOR)
}

pub fn iter_all() -> impl Iterator<Item = &'static AgentDescriptor> {
	ALL_AGENTS.iter().copied()
}
