//! Every `agent_roster!` row must name its OWN descriptor, under its OWN id.
//!
//! `registry::ALL_AGENTS` and `AgentType::ALL` are now generated from the one
//! declaration in `crates/agents/src/agents/mod.rs`, so they can no longer
//! disagree on MEMBERSHIP — a variant with no descriptor entry does not
//! compile, and `registry::get` has no `.unwrap_or(&claude::DESCRIPTOR)` left.
//! That closes the failure this file was written for: an agent absent from the
//! roster used to be served Claude's descriptor at runtime, writing its MCP
//! servers into `~/.claude.json` and linking its skills into Claude's
//! directory, with every registry-driven suite vacuously green.
//!
//! What the macro still cannot check is the OTHER TWO fields of a row. Only
//! the variant is compiler-enforced; the id literal and the module path are
//! free text, and a copy-pasted row compiles:
//!
//! - `Grok => "grok", claude, [];` — Grok is handed CLAUDE's descriptor, and
//!   `claude::DESCRIPTOR` appears in `ALL_DESCRIPTORS` twice
//! - `Grok => "claude", grok, [];` — two variants answer to the same id
//! - `Grok => "grokk", grok, [];` — the row's id drifts from the `id:` field
//!   inside `agents/grok.rs`, which is what every path and lock keys on
//!
//! Each of those is a live way back to the original failure, and each has its
//! own test below. Nothing here is a tautology; delete one and the
//! corresponding copy-paste ships.

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
			"registry::get({agent:?}) returned the descriptor whose id is \
			 '{}' — the agent_roster! row's id literal and the `id:` field in \
			 agents/<module>.rs must be the same string",
			descriptor.id
		);
	}
}

#[test]
fn no_agent_type_is_served_the_claude_fallback_by_accident() {
	// A row naming the wrong MODULE (`Grok => "grok", claude, [];`) compiles:
	// the id check above still passes for Claude's own row, and Grok's row
	// fails it only because the descriptor it reaches has Claude's id. Pointer
	// identity names the failure for what it is — Grok's config would be
	// written to Claude's files, which is exactly the old fallback bug.
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
			"'{}' appears twice in ALL_DESCRIPTORS — two agent_roster! rows \
			 name the same descriptor module, so one agent is being served \
			 another's config files",
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
		"registry holds {} agents, AgentType::ALL declares {} — both expand \
		 from the same agent_roster! rows, so this can only mean \
		 registry::ALL_AGENTS is no longer `= agents::ALL_DESCRIPTORS`",
		seen.len(),
		AgentType::ALL.len()
	);
}

#[test]
fn agent_type_all_lists_each_agent_exactly_once() {
	// The id LITERAL side: two rows may not answer to the same id. A duplicate
	// variant is a compile error, but `Grok => "claude", grok, [];` is not —
	// and `from_str("claude")` would then be decided by match-arm order.
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
