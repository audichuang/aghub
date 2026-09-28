use aghub_agents::{agents, AgentDescriptor, AgentType};

/// The shipped descriptors. Single-sourced from `aghub_agents::agents` so this
/// is not a second hand-written roster that can silently drift from
/// `AgentType::ALL` — see `tests/registry_bijection.rs`.
pub static ALL_AGENTS: &[&AgentDescriptor] = agents::ALL_DESCRIPTORS;

/// Total by construction — `AgentType::descriptor` is a `match` generated
/// from the same roster row as the variant. There is NO fallback: never add
/// one (an old `unwrap_or(claude)` wrote other agents' config into Claude's).
pub fn get(agent_type: AgentType) -> &'static AgentDescriptor {
	agent_type.descriptor()
}

pub fn iter_all() -> impl Iterator<Item = &'static AgentDescriptor> {
	ALL_AGENTS.iter().copied()
}
