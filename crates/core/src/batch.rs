//! Multi-target mutation policy — the ONE place that defines how a batch of
//! agents receives the same mutation: a predictable-failure preflight BEFORE
//! any write, then an attempt on EVERY agent (no fail-fast) collected into a
//! per-agent attribution. CLI/API agent lists, transfer/reconcile, and shared
//! Source-to-Master installs all map to this module. Surface adapters retain
//! their own wire views; preflight, attempt-all execution, and attribution
//! ordering live here once.

use std::fmt;

use crate::models::{AgentType, ResourceScope};
use crate::registry;

/// Why a batch was rejected up front: every named agent that cannot receive
/// the operation, with its reason. Nothing was written.
#[derive(Debug, Clone)]
pub struct BatchUnsupported {
	/// `(agent id, reason)` pairs, in the order the agents were named.
	pub agents: Vec<(String, String)>,
	operation: &'static str,
}

impl BatchUnsupported {
	fn new(operation: &'static str, agents: Vec<(String, String)>) -> Self {
		Self { agents, operation }
	}
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
			"agent(s) {list} do not support this {} operation; nothing \
			 was written",
			self.operation
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

fn mcp_agent_preflight(
	agent: AgentType,
	write_scope: ResourceScope,
	toggle: bool,
	transport: Option<&crate::models::McpTransport>,
) -> Result<(), String> {
	let descriptor = registry::get(agent);
	if !descriptor.supports_mcp_scope(write_scope) {
		return Err(format!("no {} MCP config", scope_word(write_scope)));
	}
	if toggle && !descriptor.capabilities.mcp.enable_disable {
		return Err("no MCP enable/disable".to_string());
	}
	// A dialect with no native word for the transport refuses the write. Catch
	// it here, for EVERY target, or the batch writes the agents that can take
	// it and then fails on the one that cannot — the partial cross-agent state
	// this module exists to prevent.
	if let Some(transport) = transport {
		if !aghub_agents::descriptor::supports_mcp_transport(
			descriptor, transport,
		) {
			let name = match transport {
				crate::models::McpTransport::Stdio { .. } => "stdio",
				crate::models::McpTransport::Sse { .. } => "SSE",
				crate::models::McpTransport::StreamableHttp { .. } => {
					"streamable HTTP"
				}
			};
			return Err(format!("no {name} MCP transport"));
		}
	}
	Ok(())
}

fn skill_agent_preflight(
	agent: AgentType,
	write_scope: ResourceScope,
) -> Result<(), String> {
	let descriptor = registry::get(agent);
	if descriptor.supports_skill_scope(write_scope) {
		Ok(())
	} else {
		Err(format!("no {} skill config", scope_word(write_scope)))
	}
}

/// Preflight a skill mutation across every named agent. Capability failures
/// are collected in input order and guarantee that no mutation has run.
pub fn skill_batch_preflight(
	agents: &[AgentType],
	write_scope: ResourceScope,
) -> Result<(), BatchUnsupported> {
	let unsupported = agents
		.iter()
		.filter_map(|agent| {
			skill_agent_preflight(*agent, write_scope)
				.err()
				.map(|reason| (agent.as_str().to_string(), reason))
		})
		.collect::<Vec<_>>();
	if unsupported.is_empty() {
		Ok(())
	} else {
		Err(BatchUnsupported::new("skill", unsupported))
	}
}

/// Preflight for an MCP batch: every agent must hold MCPs in the scope the
/// batch WRITES, and a toggle batch (enable/disable) additionally needs the
/// enable/disable capability — the same descriptor bits the manager's own
/// per-agent guards check, evaluated for ALL agents BEFORE any write so a
/// capability mismatch cannot leave a partial batch.
#[cfg(test)]
pub fn mcp_batch_preflight(
	agents: &[AgentType],
	write_scope: ResourceScope,
	toggle: bool,
	transport: Option<&crate::models::McpTransport>,
) -> Result<(), BatchUnsupported> {
	let unsupported: Vec<(String, String)> = agents
		.iter()
		.filter_map(|agent| {
			mcp_agent_preflight(*agent, write_scope, toggle, transport)
				.err()
				.map(|reason| (agent.as_str().to_string(), reason))
		})
		.collect();
	if unsupported.is_empty() {
		Ok(())
	} else {
		Err(BatchUnsupported::new("MCP", unsupported))
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

/// One target rejected by the predictable-failure preflight. These rows are
/// returned together, in input order, before any mutation is attempted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MultiTargetMutationFailure<T, E> {
	pub target: T,
	pub reason: E,
}

/// Aggregate preflight rejection for a multi-target mutation. A non-empty
/// `failures` list guarantees that the mutation callback was never invoked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MultiTargetMutationError<T, E> {
	pub failures: Vec<MultiTargetMutationFailure<T, E>>,
}

/// One attempted target and its exact mutation outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MultiTargetMutationResult<T, O, E> {
	pub target: T,
	pub result: Result<O, E>,
}

/// Successful preflight followed by one mutation result per input target, in
/// input order. Mutation failures do not stop later targets from running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MultiTargetMutationReport<T, O, E> {
	pub results: Vec<MultiTargetMutationResult<T, O, E>>,
}

impl<T, O, E> MultiTargetMutationReport<T, O, E> {
	pub fn success_count(&self) -> usize {
		self.results.iter().filter(|row| row.result.is_ok()).count()
	}

	pub fn failed_count(&self) -> usize {
		self.results.len() - self.success_count()
	}
}

fn collect_preflight_failures<T, E>(
	targets: &[T],
	mut preflight: impl FnMut(&T) -> Result<(), E>,
) -> Vec<MultiTargetMutationFailure<T, E>>
where
	T: Clone,
{
	targets
		.iter()
		.filter_map(|target| {
			preflight(target)
				.err()
				.map(|reason| MultiTargetMutationFailure {
					target: target.clone(),
					reason,
				})
		})
		.collect()
}

/// Apply one mutation across multiple targets under the repository-wide batch
/// policy: collect every predictable failure before writing anything; when the
/// preflight is clean, attempt every mutation without fail-fast behavior.
pub fn run_multi_target_mutation<T, O, E>(
	targets: &[T],
	preflight: impl FnMut(&T) -> Result<(), E>,
	mut mutate: impl FnMut(&T) -> Result<O, E>,
) -> Result<MultiTargetMutationReport<T, O, E>, MultiTargetMutationError<T, E>>
where
	T: Clone,
{
	let failures = collect_preflight_failures(targets, preflight);
	if !failures.is_empty() {
		return Err(MultiTargetMutationError { failures });
	}

	let results = targets
		.iter()
		.map(|target| MultiTargetMutationResult {
			target: target.clone(),
			result: mutate(target),
		})
		.collect();
	Ok(MultiTargetMutationReport { results })
}

/// Run one shared setup after a clean all-target preflight, then attribute the
/// prepared state to every target. A shared setup failure is represented as an
/// attempted failure row for every target so adapters retain full attribution.
pub fn run_shared_multi_target_mutation<T, S, O, E>(
	targets: &[T],
	preflight: impl FnMut(&T) -> Result<(), E>,
	shared_setup_once: impl FnOnce(&[T]) -> Result<S, E>,
	mut per_target_attribute: impl FnMut(&T, &S) -> Result<O, E>,
) -> Result<MultiTargetMutationReport<T, O, E>, MultiTargetMutationError<T, E>>
where
	T: Clone,
	E: Clone,
{
	let failures = collect_preflight_failures(targets, preflight);
	if !failures.is_empty() {
		return Err(MultiTargetMutationError { failures });
	}

	let prepared = match shared_setup_once(targets) {
		Ok(prepared) => prepared,
		Err(error) => {
			let results = targets
				.iter()
				.map(|target| MultiTargetMutationResult {
					target: target.clone(),
					result: Err(error.clone()),
				})
				.collect();
			return Ok(MultiTargetMutationReport { results });
		}
	};

	let results = targets
		.iter()
		.map(|target| MultiTargetMutationResult {
			target: target.clone(),
			result: per_target_attribute(target, &prepared),
		})
		.collect();
	Ok(MultiTargetMutationReport { results })
}

/// Run a mutation across two groups of targets under a STAGED policy:
/// preflight covers every target (primary + secondary) up front exactly like
/// [`run_multi_target_mutation`], so no predictable failure ever reaches a
/// write. Once preflight is clean, every primary row is attempted (no
/// fail-fast among primaries). If any primary row failed, the secondary rows
/// are never attempted at all — each gets a synthesized failure row instead,
/// so a caller can never remove a resource whose only copy to another target
/// failed. When every primary row succeeds, secondary rows run with the same
/// attempt-all behavior as before.
///
/// This is `reconcile_{skill,mcp,sub_agent}`'s policy: `primary` is the
/// "added" copies, `secondary` is the "removed" deletes — a copy that fails
/// at RUNTIME (after preflight already passed) must not let its paired
/// delete run, or the resource ends up gone from every agent.
pub fn run_staged_multi_target_mutation<T, O, E>(
	primary: &[T],
	secondary: &[T],
	mut preflight: impl FnMut(&T) -> Result<(), E>,
	mut mutate: impl FnMut(&T) -> Result<O, E>,
	skipped_secondary_reason: impl Fn(&T) -> E,
) -> Result<MultiTargetMutationReport<T, O, E>, MultiTargetMutationError<T, E>>
where
	T: Clone,
{
	let all_targets: Vec<T> =
		primary.iter().chain(secondary.iter()).cloned().collect();
	let failures = collect_preflight_failures(&all_targets, &mut preflight);
	if !failures.is_empty() {
		return Err(MultiTargetMutationError { failures });
	}

	let mut results: Vec<MultiTargetMutationResult<T, O, E>> = primary
		.iter()
		.map(|target| MultiTargetMutationResult {
			target: target.clone(),
			result: mutate(target),
		})
		.collect();

	let any_primary_failed = results.iter().any(|row| row.result.is_err());

	results.extend(secondary.iter().map(|target| {
		let result = if any_primary_failed {
			Err(skipped_secondary_reason(target))
		} else {
			mutate(target)
		};
		MultiTargetMutationResult {
			target: target.clone(),
			result,
		}
	}));

	Ok(MultiTargetMutationReport { results })
}

fn run_agent_mutation_with_preflight(
	agents: &[AgentType],
	operation: &'static str,
	mut preflight: impl FnMut(AgentType) -> Result<(), String>,
	mut mutate: impl FnMut(AgentType) -> Result<serde_json::Value, String>,
) -> Result<AgentBatchView, BatchUnsupported> {
	let report = run_multi_target_mutation(
		agents,
		|agent| preflight(*agent),
		|agent| mutate(*agent),
	)
	.map_err(|error| {
		BatchUnsupported::new(
			operation,
			error
				.failures
				.into_iter()
				.map(|failure| {
					(failure.target.as_str().to_string(), failure.reason)
				})
				.collect(),
		)
	})?;

	let success_count = report.success_count();
	let failed_count = report.failed_count();
	let results = report
		.results
		.into_iter()
		.map(|row| match row.result {
			Ok(output) => AgentOpResultView {
				agent: row.target.as_str().to_string(),
				ok: true,
				output: Some(output),
				error: None,
			},
			Err(error) => AgentOpResultView {
				agent: row.target.as_str().to_string(),
				ok: false,
				output: None,
				error: Some(error),
			},
		})
		.collect();
	Ok(AgentBatchView {
		success_count,
		failed_count,
		results,
	})
}

/// Run one MCP mutation across agents with capability preflight owned by the
/// same interface. Predictable scope/toggle failures reject the entire batch;
/// execution failures remain attributed per agent and never fail fast.
pub fn run_mcp_agent_mutation(
	agents: &[AgentType],
	write_scope: ResourceScope,
	toggle: bool,
	transport: Option<&crate::models::McpTransport>,
	mutate: impl FnMut(AgentType) -> Result<serde_json::Value, String>,
) -> Result<AgentBatchView, BatchUnsupported> {
	run_agent_mutation_with_preflight(
		agents,
		"MCP",
		|agent| mcp_agent_preflight(agent, write_scope, toggle, transport),
		mutate,
	)
}

/// Run one skill mutation across agents with scope capability preflight owned
/// by the same interface, preserving ordered attempt-all wire attribution.
pub fn run_skill_agent_mutation(
	agents: &[AgentType],
	write_scope: ResourceScope,
	mutate: impl FnMut(AgentType) -> Result<serde_json::Value, String>,
) -> Result<AgentBatchView, BatchUnsupported> {
	skill_batch_preflight(agents, write_scope)?;
	run_agent_mutation_with_preflight(agents, "skill", |_| Ok(()), mutate)
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
			None,
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
		// hermes toggles MCPs; windsurf holds them but cannot toggle.
		let err = mcp_batch_preflight(
			&[AgentType::Hermes, AgentType::Windsurf],
			ResourceScope::GlobalOnly,
			true,
			None,
		)
		.unwrap_err();
		assert_eq!(err.agents.len(), 1, "hermes must not be blamed");
		assert_eq!(err.agents[0].0, "windsurf");
		assert!(err.agents[0].1.contains("enable/disable"));
		// The same pair passes a non-toggle mutation.
		assert!(mcp_batch_preflight(
			&[AgentType::Hermes, AgentType::Windsurf],
			ResourceScope::GlobalOnly,
			false,
			None,
		)
		.is_ok());
	}

	#[test]
	fn multi_target_mutation_attempts_every_target_after_preflight() {
		let attempts = std::cell::RefCell::new(Vec::new());
		let report = run_multi_target_mutation(
			&["claude", "codex", "grok"],
			|_target| Ok::<_, String>(()),
			|target| {
				attempts.borrow_mut().push(*target);
				if *target == "codex" {
					Err("boom".to_string())
				} else {
					Ok(json!({ "agent": target }))
				}
			},
		)
		.expect("preflight succeeds");

		assert_eq!(*attempts.borrow(), ["claude", "codex", "grok"]);
		assert_eq!(report.success_count(), 2);
		assert_eq!(report.failed_count(), 1);
		assert!(matches!(
			&report.results[1].result,
			Err(error) if error == "boom"
		));
	}

	#[test]
	fn multi_target_mutation_collects_preflight_failures_before_any_write() {
		let targets = ["claude", "pi", "augmentcode"];
		let writes = std::cell::Cell::new(0);

		let error = run_multi_target_mutation(
			&targets,
			|target| match *target {
				"pi" => Err("no skill config".to_string()),
				"augmentcode" => Err("global only".to_string()),
				_ => Ok(()),
			},
			|target| {
				writes.set(writes.get() + 1);
				Ok(target.to_string())
			},
		)
		.expect_err("predictable failures must reject the whole mutation");

		assert_eq!(writes.get(), 0, "preflight must precede every write");
		assert_eq!(error.failures.len(), 2);
		assert_eq!(error.failures[0].target, "pi");
		assert_eq!(error.failures[0].reason, "no skill config");
		assert_eq!(error.failures[1].target, "augmentcode");
	}

	#[test]
	fn preflight_rejects_a_transport_the_dialect_cannot_write() {
		use crate::models::McpTransport;
		// OpenCode's config has one remote type, so SSE has no native spelling
		// there; claude spells all three. Without this leg claude gets written
		// and opencode then fails, leaving the batch half applied.
		let sse = McpTransport::sse("https://example.com/v1/messages");
		let err = mcp_batch_preflight(
			&[AgentType::Claude, AgentType::OpenCode],
			ResourceScope::ProjectOnly,
			false,
			Some(&sse),
		)
		.unwrap_err();
		assert_eq!(err.agents.len(), 1, "claude must not be blamed");
		assert_eq!(err.agents[0].0, "opencode");
		assert!(err.agents[0].1.contains("SSE"), "{:?}", err.agents[0].1);
		assert!(err.to_string().contains("nothing was written"));

		// The same pair takes streamable HTTP, and stdio, without complaint.
		for transport in [
			McpTransport::streamable_http("https://example.com/v1/mcp"),
			McpTransport::stdio("echo", vec![]),
		] {
			assert!(
				mcp_batch_preflight(
					&[AgentType::Claude, AgentType::OpenCode],
					ResourceScope::ProjectOnly,
					false,
					Some(&transport),
				)
				.is_ok(),
				"{transport:?} must pass"
			);
		}
	}

	#[test]
	fn mcp_mutation_interface_owns_preflight_before_execution() {
		let writes = std::cell::Cell::new(0);
		let result = run_mcp_agent_mutation(
			&[AgentType::Claude, AgentType::Pi],
			ResourceScope::ProjectOnly,
			false,
			None,
			|agent| {
				writes.set(writes.get() + 1);
				Ok(serde_json::json!({ "agent": agent.as_str() }))
			},
		);

		assert!(result.is_err(), "Pi cannot receive a project MCP");
		assert_eq!(
			writes.get(),
			0,
			"the interface must not let callers run before preflight",
		);
	}

	#[test]
	fn skill_mutation_interface_owns_preflight_before_execution() {
		let writes = std::cell::Cell::new(0);
		let result = run_skill_agent_mutation(
			&[AgentType::Claude, AgentType::JetBrainsAi],
			ResourceScope::GlobalOnly,
			|agent| {
				writes.set(writes.get() + 1);
				Ok(serde_json::json!({ "agent": agent.as_str() }))
			},
		);

		assert!(result.is_err(), "JetBrains AI cannot receive a skill");
		assert_eq!(
			writes.get(),
			0,
			"the interface must not let callers run before preflight",
		);
	}

	#[test]
	fn shared_mutation_failure_is_attributed_to_every_target() {
		let setup_calls = std::cell::Cell::new(0);
		let report = run_shared_multi_target_mutation(
			&["claude", "codex", "opencode"],
			|_target| Ok::<_, String>(()),
			|_targets| {
				setup_calls.set(setup_calls.get() + 1);
				Err::<(), _>("master write failed".to_string())
			},
			|target, _prepared| Ok(target.to_string()),
		)
		.expect("preflight succeeds even when shared mutation fails");

		assert_eq!(setup_calls.get(), 1);
		assert_eq!(report.results.len(), 3);
		assert!(report.results.iter().all(|row| {
			matches!(
				&row.result,
				Err(error) if error == "master write failed"
			)
		}));
	}

	#[test]
	fn staged_mutation_skips_secondary_when_a_primary_fails() {
		let secondary_calls = std::cell::Cell::new(0);
		let report = run_staged_multi_target_mutation(
			&["claude", "cursor"],
			&["windsurf", "cline"],
			|_target| Ok::<_, String>(()),
			|target| match *target {
				"claude" => Err("copy failed".to_string()),
				"windsurf" | "cline" => {
					secondary_calls.set(secondary_calls.get() + 1);
					Ok(target.to_string())
				}
				other => Ok(other.to_string()),
			},
			|target| format!("skipped '{target}': a copy failed"),
		)
		.expect("preflight succeeds");

		assert_eq!(
			secondary_calls.get(),
			0,
			"secondary mutate must never run once a primary row failed"
		);
		assert_eq!(report.results.len(), 4);
		assert!(matches!(
			&report.results[0].result,
			Err(error) if error == "copy failed"
		));
		assert!(
			report.results[1].result.is_ok(),
			"cursor primary is still attempted (no fail-fast among primaries)"
		);
		assert!(matches!(
			&report.results[2].result,
			Err(error) if error.contains("skipped")
		));
		assert!(matches!(
			&report.results[3].result,
			Err(error) if error.contains("skipped")
		));
	}

	#[test]
	fn staged_mutation_runs_secondary_attempt_all_when_primaries_succeed() {
		let calls = std::cell::RefCell::new(Vec::new());
		let report = run_staged_multi_target_mutation(
			&["claude", "cursor"],
			&["windsurf", "cline"],
			|_target| Ok::<_, String>(()),
			|target| {
				calls.borrow_mut().push(*target);
				if *target == "cline" {
					Err("delete failed".to_string())
				} else {
					Ok(target.to_string())
				}
			},
			|target| format!("skipped {target}"),
		)
		.expect("preflight succeeds");

		assert_eq!(*calls.borrow(), ["claude", "cursor", "windsurf", "cline"]);
		assert_eq!(report.results.len(), 4);
		assert!(report.results[0].result.is_ok());
		assert!(report.results[1].result.is_ok());
		assert!(
			report.results[2].result.is_ok(),
			"windsurf must still be attempted"
		);
		assert!(matches!(
			&report.results[3].result,
			Err(error) if error == "delete failed"
		));
	}

	#[test]
	fn staged_mutation_with_no_primary_runs_secondary_as_before() {
		let empty: [&str; 0] = [];
		let calls = std::cell::Cell::new(0);
		let report = run_staged_multi_target_mutation(
			&empty,
			&["claude", "cursor"],
			|_target| Ok::<_, String>(()),
			|target| {
				calls.set(calls.get() + 1);
				Ok(target.to_string())
			},
			|target| format!("skipped {target}"),
		)
		.expect("preflight succeeds");

		assert_eq!(
			calls.get(),
			2,
			"the removals-only case must run every secondary row"
		);
		assert_eq!(report.results.len(), 2);
		assert!(report.results.iter().all(|row| row.result.is_ok()));
	}

	#[test]
	fn staged_mutation_preflight_covers_primary_and_secondary_before_any_write()
	{
		let writes = std::cell::Cell::new(0);
		let error = run_staged_multi_target_mutation(
			&["claude"],
			&["pi"],
			|target| match *target {
				"pi" => Err("no skill config".to_string()),
				_ => Ok(()),
			},
			|target| {
				writes.set(writes.get() + 1);
				Ok(target.to_string())
			},
			|target| format!("skipped {target}"),
		)
		.expect_err(
			"a secondary preflight failure rejects the whole staged mutation",
		);

		assert_eq!(writes.get(), 0, "preflight must precede every write");
		assert_eq!(error.failures.len(), 1);
		assert_eq!(error.failures[0].target, "pi");
	}
}
