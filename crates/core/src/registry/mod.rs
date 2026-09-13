use aghub_agents::{agents, AgentDescriptor, AgentType};

/// The shipped descriptors. Single-sourced from `aghub_agents::agents` so this
/// is not a second hand-written roster that can silently drift from
/// `AgentType::ALL` — see `tests/registry_bijection.rs`.
pub static ALL_AGENTS: &[&AgentDescriptor] = agents::ALL_DESCRIPTORS;

/// Total by construction — `AgentType::descriptor` is a `match` generated
/// from the same roster row as the variant, so there is no missing-entry case
/// left to fall back from. This used to be a find-by-id ending in
/// `.unwrap_or(&agents::claude::DESCRIPTOR)`: an agent absent from the roster
/// was served Claude's descriptor silently, writing its MCP servers into
/// `~/.claude.json` and linking its skills into Claude's directory.
pub fn get(agent_type: AgentType) -> &'static AgentDescriptor {
	agent_type.descriptor()
}

pub fn iter_all() -> impl Iterator<Item = &'static AgentDescriptor> {
	ALL_AGENTS.iter().copied()
}
