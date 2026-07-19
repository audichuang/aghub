//! Multi-target mutation policy — the ONE place that defines how a batch of
//! agents receives the same mutation: a predictable-failure preflight BEFORE
//! any write, then an attempt on EVERY agent (no fail-fast) collected into a
//! per-agent attribution both surfaces serialize verbatim. The CLI's
//! `-a a,b <mutating-cmd>` and the API's `/mcps/batch` (behind the desktop's
//! multi-agent create) both map to this module — policy lives here, the
//! surfaces stay transport adapters. Mirrors the transfer-module precedent:
//! one wire view, no hand-rolled second mapping that could drift.

use std::fmt;

use crate::models::{AgentType, ResourceScope};
use crate::registry;

/// Why a batch was rejected up front: every named agent that cannot receive
/// the operation, with its reason. Nothing was written.
#[derive(Debug, Clone)]
pub struct BatchUnsupported {
	/// `(agent id, reason)` pairs, in the order the agents were named.
	pub agents: Vec<(String, String)>,
}

impl fmt::Display for BatchUnsupported {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		let list = self
			.agents
			.iter()
			.map(|(agent, reason)| format!("{agent} ({reason})"))
			.collect::<Vec<_>>()
			.join(", ");
		write!(
			f,
			"agent(s) {list} do not support this MCP operation; nothing \
			 was written"
		)
	}
}

impl std::error::Error for BatchUnsupported {}

fn scope_word(scope: ResourceScope) -> &'static str {
	match scope {
		ResourceScope::GlobalOnly => "global",
		ResourceScope::ProjectOnly => "project",
		ResourceScope::Both => "global+project",
	}
}

/// Preflight for an MCP batch: every agent must hold MCPs in the scope the
/// batch WRITES, and a toggle batch (enable/disable) additionally needs the
/// enable/disable capability — the same descriptor bits the manager's own
/// per-agent guards check, evaluated for ALL agents BEFORE any write so a
/// capability mismatch cannot leave a partial batch.
pub fn mcp_batch_preflight(
	agents: &[AgentType],
	write_scope: ResourceScope,
	toggle: bool,
) -> Result<(), BatchUnsupported> {
	let unsupported: Vec<(String, String)> = agents
		.iter()
		.filter_map(|a| {
			let descriptor = registry::get(*a);
			if !descriptor.supports_mcp_scope(write_scope) {
				return Some((
					a.as_str().to_string(),
					format!("no {} MCP config", scope_word(write_scope)),
				));
			}
			if toggle && !descriptor.capabilities.mcp.enable_disable {
				return Some((
					a.as_str().to_string(),
					"no MCP enable/disable".to_string(),
				));
			}
			None
		})
		.collect();
	if unsupported.is_empty() {
		Ok(())
	} else {
		Err(BatchUnsupported {
			agents: unsupported,
		})
	}
}

/// One agent's outcome in a multi-agent batch. snake_case wire shape shared
/// by the CLI's stdout envelope and the API batch response (the ts-rs DTO
/// mirrors it) — define it once, serialize it everywhere.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AgentOpResultView {
	pub agent: String,
	pub ok: bool,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub output: Option<serde_json::Value>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub error: Option<String>,
}

/// The whole batch's attribution: counts plus per-agent rows in the order
/// the agents were named.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AgentBatchView {
	pub success_count: usize,
	pub failed_count: usize,
	pub results: Vec<AgentOpResultView>,
}

/// Attempt EVERY agent and collect — a mid-batch failure must not silently
/// skip the rest. The caller decides how to surface `failed_count > 0`
/// (non-zero exit on the CLI, HTTP status on the API).
pub fn run_agent_batch(
	agents: &[AgentType],
	mut op: impl FnMut(AgentType) -> Result<serde_json::Value, String>,
) -> AgentBatchView {
	let results: Vec<AgentOpResultView> = agents
		.iter()
		.map(|agent| match op(*agent) {
			Ok(output) => AgentOpResultView {
				agent: agent.as_str().to_string(),
				ok: true,
				output: Some(output),
				error: None,
			},
			Err(error) => AgentOpResultView {
				agent: agent.as_str().to_string(),
				ok: false,
				output: None,
				error: Some(error),
			},
		})
		.collect();
	let success_count = results.iter().filter(|r| r.ok).count();
	let failed_count = results.len() - success_count;
	AgentBatchView {
		success_count,
		failed_count,
		results,
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use serde_json::json;

	#[test]
	fn preflight_rejects_wrong_scope_and_names_every_offender() {
		// pi holds no MCPs anywhere; augmentcode is global-only.
		let err = mcp_batch_preflight(
			&[AgentType::Claude, AgentType::Pi, AgentType::AugmentCode],
			ResourceScope::ProjectOnly,
			false,
		)
		.unwrap_err();
		let ids: Vec<&str> =
			err.agents.iter().map(|(a, _)| a.as_str()).collect();
		assert_eq!(ids, vec!["pi", "augmentcode"], "claude must pass");
		let msg = err.to_string();
		assert!(msg.contains("project"), "reason names the scope: {msg}");
		assert!(msg.contains("nothing was written"), "{msg}");
	}

	#[test]
	fn preflight_toggle_requires_enable_disable_capability() {
		// hermes toggles MCPs; cline holds them but cannot toggle.
		let err = mcp_batch_preflight(
			&[AgentType::Hermes, AgentType::Cline],
			ResourceScope::GlobalOnly,
			true,
		)
		.unwrap_err();
		assert_eq!(err.agents.len(), 1, "hermes must not be blamed");
		assert_eq!(err.agents[0].0, "cline");
		assert!(err.agents[0].1.contains("enable/disable"));
		// The same pair passes a non-toggle mutation.
		assert!(mcp_batch_preflight(
			&[AgentType::Hermes, AgentType::Cline],
			ResourceScope::GlobalOnly,
			false,
		)
		.is_ok());
	}

	#[test]
	fn run_agent_batch_attempts_every_agent_and_counts() {
		// claude fails mid-batch; grok must still be attempted.
		let view =
			run_agent_batch(&[AgentType::Claude, AgentType::Grok], |agent| {
				match agent {
					AgentType::Claude => Err("boom".to_string()),
					other => Ok(json!({ "agent": other.as_str() })),
				}
			});
		assert_eq!(view.success_count, 1);
		assert_eq!(view.failed_count, 1);
		assert_eq!(view.results.len(), 2);
		assert!(!view.results[0].ok);
		assert_eq!(view.results[0].error.as_deref(), Some("boom"));
		assert!(view.results[1].ok, "grok attempted despite claude failing");
		// Wire shape: snake_case fields, absent optionals omitted.
		let wire = serde_json::to_value(&view).unwrap();
		assert!(wire["results"][0].get("output").is_none());
		assert!(wire["results"][1].get("error").is_none());
		assert_eq!(wire["failed_count"], 1);
	}
}
