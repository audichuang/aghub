//! `registry::ALL_AGENTS` and `AgentType::ALL` must be a BIJECTION.
//!
//! `registry::get` is a linear find-by-id that ends in
//! `.unwrap_or(&claude::DESCRIPTOR)` — a SILENT fallback. An agent missing from
//! the roster therefore gets Claude's descriptor at runtime: its MCP servers
//! are written into `~/.claude.json` and its skills linked into Claude's
//! directory, with no compile error and no runtime error. Every registry-driven
//! test (`mcp_dialect_roundtrip`, `mcp_dialect_golden`, `test_agent_paths`) is
//! vacuously green for an agent the roster never mentions, and the CRUD suites
//! are worse than vacuous: they silently exercise Claude and pass.
//!
//! Length alone is not the check — a duplicated entry pads the length back to
//! matching while an agent stays missing — so identity and uniqueness are
//! asserted too.

use aghub_agents::AgentType;
use aghub_core::registry;
use std::collections::BTreeSet;

#[test]
fn every_agent_type_has_its_own_descriptor_in_the_registry() {
	for agent in AgentType::ALL {
		let descriptor = registry::get(*agent);
		assert_eq!(
			descriptor.id,
			agent.as_str(),
			"registry::get({agent:?}) fell back to '{}' — add \
			 &agents::…::DESCRIPTOR to agents::ALL_DESCRIPTORS",
			descriptor.id
		);
	}
}

#[test]
fn no_agent_type_is_served_the_claude_fallback_by_accident() {
	// The id check above cannot see this on its own: Claude's descriptor IS the
	// fallback, so an agent whose id happened to match would still slip past.
	// Pointer identity names the failure for what it is.
	let claude = registry::get(AgentType::Claude);
	for agent in AgentType::ALL {
		if matches!(agent, AgentType::Claude) {
			continue;
		}
		assert!(
			!std::ptr::eq(registry::get(*agent), claude),
			"{agent:?} is being served Claude's descriptor — its config would \
			 be written to Claude's files"
		);
	}
}

#[test]
fn the_registry_holds_no_duplicate_and_no_unknown_agents() {
	let mut seen = BTreeSet::new();
	for descriptor in registry::iter_all() {
		assert!(
			seen.insert(descriptor.id),
			"'{}' appears twice in ALL_DESCRIPTORS — a duplicate pads the \
			 length back to matching while another agent is missing",
			descriptor.id
		);
		let agent: AgentType = descriptor.id.parse().unwrap_or_else(|_| {
			panic!("registry holds '{}', which is no AgentType", descriptor.id)
		});
		assert_eq!(
			agent.as_str(),
			descriptor.id,
			"'{}' parses to {agent:?}, whose id is '{}'",
			descriptor.id,
			agent.as_str()
		);
	}
	assert_eq!(
		seen.len(),
		AgentType::ALL.len(),
		"registry holds {} agents, AgentType::ALL declares {}",
		seen.len(),
		AgentType::ALL.len()
	);
}

#[test]
fn agent_type_all_lists_each_agent_exactly_once() {
	// The other direction of the same roster problem: a duplicate here would
	// let the counts above agree while an agent is absent from BOTH lists.
	let mut seen = BTreeSet::new();
	for agent in AgentType::ALL {
		assert!(
			seen.insert(agent.as_str()),
			"'{}' appears twice in AgentType::ALL",
			agent.as_str()
		);
	}
	assert_eq!(seen.len(), AgentType::ALL.len());
}
