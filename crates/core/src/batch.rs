//! Multi-target mutation policy — the ONE place that defines how a batch of
//! agents receives the same mutation: a predictable-failure preflight BEFORE
//! any write, then an attempt on EVERY agent (no fail-fast) collected into a
//! per-agent attribution. CLI/API agent lists, transfer/reconcile, and shared
//! Source-to-Master installs all map to this module. Surface adapters retain
//! their own wire views; preflight, attempt-all execution, and attribution
//! ordering live here once.

use std::{
	collections::HashMap,
	fmt,
	path::{Path, PathBuf},
};

use crate::errors::ConfigError;
use crate::models::{AgentType, McpServer, ResourceScope};
use crate::registry;
use crate::scope::WriteScope;

/// Attribute one MCP create to every selected agent that reads the same
/// physical config. A duplicate counts as this batch's success only when an
/// earlier row wrote that backing and both agents can read the same complete
/// persisted server. A server present before the batch remains a conflict.
struct McpCreateAttribution {
	name: String,
	written_backings: HashMap<PathBuf, (McpServer, serde_json::Value)>,
}

impl McpCreateAttribution {
	fn new(name: impl Into<String>) -> Self {
		Self {
			name: name.into(),
			written_backings: HashMap::new(),
		}
	}

	fn attribute(
		&mut self,
		agent: AgentType,
		project_root: Option<&Path>,
		write_scope: ResourceScope,
		result: Result<serde_json::Value, ConfigError>,
	) -> Result<serde_json::Value, ConfigError> {
		let adapter = crate::create_adapter(agent);
		// mcp_backing_path creates the parent dir, so only rows whose write
		// reached disk may resolve it.
		let backing = || {
			adapter.mcp_config_path(project_root, write_scope).and_then(
				|path| crate::descriptor::mcp_backing_path(&path).ok(),
			)
		};
		let name = self.name.clone();
		let read = || {
			adapter.load_mcps(project_root, write_scope).ok().and_then(
				|servers| {
					servers.into_iter().find(|server| server.name == name)
				},
			)
		};
		match result {
			Ok(output) => {
				if let Some(backing) = backing() {
					if let Some(persisted) = read() {
						self.written_backings
							.insert(backing, (persisted, output.clone()));
						return Ok(output);
					}
				}
				Ok(output)
			}
			Err(err @ ConfigError::ResourceExists { .. }) => {
				if let Some((persisted, credited_output)) = backing()
					.as_ref()
					.and_then(|path| self.written_backings.get(path))
				{
					if let Some(observed) = read() {
						if &observed == persisted {
							return Ok(credited_output.clone());
						}
					}
				}
				Err(err)
			}
			Err(error) => Err(error),
		}
	}
}

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

impl From<BatchUnsupported> for ConfigError {
	fn from(err: BatchUnsupported) -> Self {
		ConfigError::UnsupportedOperation {
			message: err.to_string(),
			rejected_targets: None,
		}
	}
}

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
) -> Result<(), ConfigError> {
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
		Err(ConfigError::from(BatchUnsupported::new(
			"skill",
			unsupported,
		)))
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
	pub outcome: Option<String>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub code: Option<String>,
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

pub(crate) fn collect_preflight_failures<T, E>(
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

/// STAGED policy: preflight covers every target (primary + secondary) up
/// front like [`run_multi_target_mutation`]; then every primary row is
/// attempted. If any primary failed, no secondary row runs (each gets a
/// synthesized failure row); otherwise secondaries run attempt-all.
///
/// `reconcile_{skill,mcp,sub_agent}`: primary = the "added" copies, secondary
/// = the "removed" deletes — a runtime copy failure must not let its paired
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
				outcome: None,
				code: None,
				output: Some(output),
				error: None,
			},
			Err(error) => AgentOpResultView {
				agent: row.target.as_str().to_string(),
				ok: false,
				outcome: None,
				code: None,
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
) -> Result<AgentBatchView, ConfigError> {
	run_agent_mutation_with_preflight(
		agents,
		"MCP",
		|agent| mcp_agent_preflight(agent, write_scope, toggle, transport),
		mutate,
	)
	.map_err(ConfigError::from)
}

/// Create one MCP server across multiple agents with predictable transport
/// capability preflight, single-agent write execution, duplicate detection
/// by error type ([`ConfigError::ResourceExists`]), and physical-backing dedup
/// attribution.
pub fn run_mcp_create_batch(
	agents: &[AgentType],
	scope: &WriteScope,
	server: &McpServer,
	mut mutate: impl FnMut(AgentType) -> Result<serde_json::Value, ConfigError>,
) -> Result<AgentBatchView, ConfigError> {
	let write_scope = scope.resource_scope();
	let project_root = scope.project_root();

	let unsupported: Vec<(String, String)> = agents
		.iter()
		.filter_map(|agent| {
			mcp_agent_preflight(
				*agent,
				write_scope,
				false,
				Some(&server.transport),
			)
			.err()
			.map(|reason| (agent.as_str().to_string(), reason))
		})
		.collect();

	if !unsupported.is_empty() {
		return Err(ConfigError::from(BatchUnsupported::new(
			"MCP",
			unsupported,
		)));
	}

	let mut attribution = McpCreateAttribution::new(&server.name);
	let mut results = Vec::with_capacity(agents.len());

	for agent in agents {
		let res = mutate(*agent);
		let attributed =
			attribution.attribute(*agent, project_root, write_scope, res);
		match attributed {
			Ok(output) => {
				results.push(AgentOpResultView {
					agent: agent.as_str().to_string(),
					ok: true,
					outcome: None,
					code: None,
					output: Some(output),
					error: None,
				});
			}
			Err(error) => {
				results.push(AgentOpResultView {
					agent: agent.as_str().to_string(),
					ok: false,
					outcome: None,
					code: None,
					output: None,
					error: Some(error.to_string()),
				});
			}
		}
	}

	let success_count = results.iter().filter(|r| r.ok).count();
	let failed_count = results.len() - success_count;

	Ok(AgentBatchView {
		success_count,
		failed_count,
		results,
	})
}

/// Run one skill mutation across agents with scope capability preflight owned
/// by the same interface, preserving ordered attempt-all wire attribution.
pub fn run_skill_agent_mutation(
	agents: &[AgentType],
	write_scope: ResourceScope,
	mutate: impl FnMut(AgentType) -> Result<serde_json::Value, String>,
) -> Result<AgentBatchView, ConfigError> {
	skill_batch_preflight(agents, write_scope)?;
	run_agent_mutation_with_preflight(agents, "skill", |_| Ok(()), mutate)
		.map_err(ConfigError::from)
}

/// A plain `canonicalize` fails on a missing leaf and falls back to the literal
/// path, which is how `~/.gemini/skills` and `~/.claude/skills` read as two
/// different places while `~/.gemini` is a symlink to `~/.claude`. Falling back
/// one level up resolves the part that DOES exist.
pub(crate) fn resolve_through_links(path: PathBuf) -> PathBuf {
	if let Ok(real) = std::fs::canonicalize(&path) {
		return real;
	}
	match (path.parent(), path.file_name()) {
		(Some(parent), Some(name)) => std::fs::canonicalize(parent)
			.map(|real| real.join(name))
			.unwrap_or(path),
		_ => path,
	}
}

#[cfg(unix)]
pub(crate) fn node_id(path: &Path) -> Option<(u64, u64)> {
	use std::os::unix::fs::MetadataExt;
	let meta = std::fs::metadata(path).ok()?;
	Some((meta.dev(), meta.ino()))
}

// ponytail: no stable std API for the Windows file index, so hard links there
// fall back to path comparison. Symlink and junction aliasing is still covered
// — that is `canonicalize`'s job — leaving only NTFS hard links between two
// agents' config files, which no documented aghub layout produces. Upgrade
// path: `GetFileInformationByHandle` via the `windows` crate if it ever bites.
#[cfg(not(unix))]
pub(crate) fn node_id(_path: &Path) -> Option<(u64, u64)> {
	None
}

/// A backing file's identity — NOT its path.
///
/// `canonicalize` collapses symlinks but NOT hard links (dotfile `cp -l`,
/// rdfind/jdupes), so identity is `(dev, ino)`.
/// See docs/history/core-transfer.md#shared-backing-destroyed-a-resource
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Backing {
	/// `(device, inode)` — identity proper. `None` when the path does not
	/// exist yet, and then there is nothing to alias.
	pub(crate) node: Option<(u64, u64)>,
	pub(crate) path: PathBuf,
}

impl Backing {
	pub(crate) fn of(path: PathBuf) -> Self {
		let path = resolve_through_links(path);
		let node = node_id(&path);
		Self { node, path }
	}

	pub(crate) fn is(&self, other: &Self) -> bool {
		match (self.node, other.node) {
			(Some(a), Some(b)) => a == b,
			// Neither exists: the resolved path is all there is.
			(None, None) => self.path == other.path,
			// One exists and one does not — not one file.
			_ => false,
		}
	}
}

/// The removal rows of ONE batch or reconcile, each with the backing it resolved
/// to at PREFLIGHT, plus which rows actually took something out.
///
/// The other half of [`ensure_removals_spare`]: its remedy ("add the sharer to
/// `--remove`") must work, so a row whose resource a SIBLING row of this
/// command already took is a success. Sharing a backing is not the credential —
/// only a row that REALLY emptied it vouches for later rows.
/// See docs/history/core-transfer.md#sibling-rows-sharing-one-backing
pub(crate) struct RemovalCredits<K> {
	/// One entry per removal target that resolves to a backing at all, in
	/// input order.
	resolved: Vec<(K, Backing)>,
	/// The keys whose row returned a real deletion.
	credited: Vec<K>,
}

impl<K: Clone + PartialEq> RemovalCredits<K> {
	pub(crate) fn new<I, F>(keys: I, mut resolve_backing: F) -> Self
	where
		I: IntoIterator<Item = K>,
		F: FnMut(&K) -> Option<Backing>,
	{
		let resolved = keys
			.into_iter()
			.filter_map(|key| {
				let backing = resolve_backing(&key)?;
				Some((key, backing))
			})
			.collect();
		Self {
			resolved,
			credited: Vec::new(),
		}
	}

	pub(crate) fn from_mapped<T, I, F>(items: I, map_fn: F) -> Self
	where
		I: IntoIterator<Item = T>,
		F: FnMut(T) -> Option<(K, Backing)>,
	{
		let resolved = items.into_iter().filter_map(map_fn).collect();
		Self {
			resolved,
			credited: Vec::new(),
		}
	}

	pub(crate) fn backing_of(&self, key: &K) -> Option<&Backing> {
		self.resolved
			.iter()
			.find(|(candidate, _)| candidate == key)
			.map(|(_, backing)| backing)
	}

	/// This row really took the resource out, so its backing now vouches for
	/// the later rows that share it.
	pub(crate) fn credit(&mut self, key: K) {
		if !self.credited.contains(&key) {
			self.credited.push(key);
		}
	}

	/// Has an EARLIER row of this same command already emptied the backing this
	/// agent/target reads?
	pub(crate) fn already_taken(&self, key: &K) -> bool {
		let Some(mine) = self.backing_of(key) else {
			return false;
		};
		self.credited.iter().any(|credited| {
			self.backing_of(credited).is_some_and(|took| took.is(mine))
		})
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

	struct EnvVarGuard(&'static str, Option<std::ffi::OsString>);

	impl EnvVarGuard {
		fn set(key: &'static str, val: impl AsRef<std::ffi::OsStr>) -> Self {
			let old = std::env::var_os(key);
			std::env::set_var(key, val);
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

	#[test]
	fn mcp_create_attribution_failed_row_creates_no_config_dir() {
		let root = tempfile::tempdir().unwrap();
		let scope = WriteScope::project(root.path());
		let server = McpServer::new(
			"x",
			crate::models::McpTransport::stdio("echo", vec![]),
		);
		let view = run_mcp_create_batch(
			&[AgentType::Cursor],
			&scope,
			&server,
			|_agent| {
				Err(ConfigError::ValidationFailed("bad header".to_string()))
			},
		)
		.expect("preflight passes");
		assert_eq!(view.failed_count, 1);
		assert!(!view.results[0].ok);
		assert!(
			!root.path().join(".cursor").exists(),
			"failed row created config dir"
		);
	}

	#[test]
	fn mcp_create_batch_all_succeed() {
		let _env = crate::skills::prune::test_lock::env_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		let home = tempfile::tempdir().unwrap();
		let _home = EnvVarGuard::set("HOME", home.path());
		let _aghub =
			EnvVarGuard::set("AGHUB_DATA_DIR", home.path().join("data"));
		let _config =
			EnvVarGuard::set("XDG_CONFIG_HOME", home.path().join(".config"));
		let _state =
			EnvVarGuard::set("XDG_STATE_HOME", home.path().join(".state"));

		let project = tempfile::tempdir().unwrap();
		let scope = WriteScope::project(project.path());
		let server = McpServer::new(
			"echo-server",
			crate::models::McpTransport::stdio("echo", vec!["hi".to_string()]),
		);

		let view = run_mcp_create_batch(
			&[AgentType::Claude, AgentType::Cursor],
			&scope,
			&server,
			|agent| {
				let mut manager = crate::ConfigManager::for_write(
					crate::create_adapter(agent),
					scope.clone(),
				);
				if let Err(ConfigError::NotFound { .. }) = manager.load() {
					manager.init_empty_config();
				}
				manager.add_mcp_exact(server.clone())?;
				Ok(serde_json::to_value(&server).unwrap())
			},
		)
		.expect("batch creation succeeds");

		assert_eq!(view.success_count, 2);
		assert_eq!(view.failed_count, 0);
		assert_eq!(view.results.len(), 2);
		assert_eq!(view.results[0].agent, "claude");
		assert!(view.results[0].ok);
		assert_eq!(view.results[1].agent, "cursor");
		assert!(view.results[1].ok);

		let claude_mcps = crate::create_adapter(AgentType::Claude)
			.load_mcps(scope.project_root(), scope.resource_scope())
			.unwrap();
		assert!(claude_mcps.iter().any(|s| s.name == "echo-server"));

		let cursor_mcps = crate::create_adapter(AgentType::Cursor)
			.load_mcps(scope.project_root(), scope.resource_scope())
			.unwrap();
		assert!(cursor_mcps.iter().any(|s| s.name == "echo-server"));
	}

	#[test]
	fn mcp_create_batch_partial_already_exists() {
		let _env = crate::skills::prune::test_lock::env_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		let home = tempfile::tempdir().unwrap();
		let _home = EnvVarGuard::set("HOME", home.path());
		let _aghub =
			EnvVarGuard::set("AGHUB_DATA_DIR", home.path().join("data"));
		let _config =
			EnvVarGuard::set("XDG_CONFIG_HOME", home.path().join(".config"));
		let _state =
			EnvVarGuard::set("XDG_STATE_HOME", home.path().join(".state"));

		let project = tempfile::tempdir().unwrap();
		let scope = WriteScope::project(project.path());
		let server = McpServer::new(
			"test-server",
			crate::models::McpTransport::stdio(
				"echo",
				vec!["test".to_string()],
			),
		);

		// Pre-seed Cursor with test-server BEFORE the batch runs
		{
			let mut manager = crate::ConfigManager::for_write(
				crate::create_adapter(AgentType::Cursor),
				scope.clone(),
			);
			if let Err(ConfigError::NotFound { .. }) = manager.load() {
				manager.init_empty_config();
			}
			manager.add_mcp_exact(server.clone()).unwrap();
		}

		// Cursor already has it before batch (conflict).
		// Claude writes .mcp.json in batch.
		// Copilot shares .mcp.json with Claude and is credited as already present.
		let view = run_mcp_create_batch(
			&[AgentType::Cursor, AgentType::Claude, AgentType::Copilot],
			&scope,
			&server,
			|agent| {
				let mut manager = crate::ConfigManager::for_write(
					crate::create_adapter(agent),
					scope.clone(),
				);
				if let Err(ConfigError::NotFound { .. }) = manager.load() {
					manager.init_empty_config();
				}
				manager.add_mcp_exact(server.clone())?;
				Ok(serde_json::to_value(&server).unwrap())
			},
		)
		.expect("batch completes with per-agent attribution");

		assert_eq!(view.success_count, 2);
		assert_eq!(view.failed_count, 1);
		assert_eq!(view.results.len(), 3);

		assert_eq!(view.results[0].agent, "cursor");
		assert!(!view.results[0].ok);
		assert!(
			view.results[0]
				.error
				.as_deref()
				.unwrap_or_default()
				.contains("test-server"),
			"Cursor error must name test-server: {:?}",
			view.results[0].error
		);

		assert_eq!(view.results[1].agent, "claude");
		assert!(view.results[1].ok);

		assert_eq!(view.results[2].agent, "copilot");
		assert!(view.results[2].ok);
		assert_eq!(
			view.results[2].output, view.results[1].output,
			"credited duplicate must preserve the writing row's exact output"
		);

		// Assert config file contents
		let cursor_mcps = crate::create_adapter(AgentType::Cursor)
			.load_mcps(scope.project_root(), scope.resource_scope())
			.unwrap();
		assert!(cursor_mcps.iter().any(|s| s.name == "test-server"));

		let claude_mcps = crate::create_adapter(AgentType::Claude)
			.load_mcps(scope.project_root(), scope.resource_scope())
			.unwrap();
		assert!(claude_mcps.iter().any(|s| s.name == "test-server"));

		let copilot_mcps = crate::create_adapter(AgentType::Copilot)
			.load_mcps(scope.project_root(), scope.resource_scope())
			.unwrap();
		assert!(copilot_mcps.iter().any(|s| s.name == "test-server"));
	}

	#[test]
	fn mcp_create_batch_partial_failure() {
		let _env = crate::skills::prune::test_lock::env_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		let home = tempfile::tempdir().unwrap();
		let _home = EnvVarGuard::set("HOME", home.path());
		let _aghub =
			EnvVarGuard::set("AGHUB_DATA_DIR", home.path().join("data"));
		let _config =
			EnvVarGuard::set("XDG_CONFIG_HOME", home.path().join(".config"));
		let _state =
			EnvVarGuard::set("XDG_STATE_HOME", home.path().join(".state"));

		let project = tempfile::tempdir().unwrap();
		let scope = WriteScope::project(project.path());
		let server = McpServer::new(
			"partial-server",
			crate::models::McpTransport::stdio(
				"echo",
				vec!["test".to_string()],
			),
		);

		let view = run_mcp_create_batch(
			&[AgentType::Claude, AgentType::Cursor],
			&scope,
			&server,
			|agent| {
				if agent == AgentType::Cursor {
					return Err(ConfigError::InvalidConfig(
						"simulated cursor failure".to_string(),
					));
				}
				let mut manager = crate::ConfigManager::for_write(
					crate::create_adapter(agent),
					scope.clone(),
				);
				if let Err(ConfigError::NotFound { .. }) = manager.load() {
					manager.init_empty_config();
				}
				manager.add_mcp_exact(server.clone())?;
				Ok(serde_json::to_value(&server).unwrap())
			},
		)
		.expect("batch completes");

		assert_eq!(view.success_count, 1);
		assert_eq!(view.failed_count, 1);
		assert_eq!(view.results[0].agent, "claude");
		assert!(view.results[0].ok);
		assert_eq!(view.results[1].agent, "cursor");
		assert!(!view.results[1].ok);
		assert!(view.results[1]
			.error
			.as_deref()
			.unwrap_or_default()
			.contains("simulated cursor failure"));

		let claude_mcps = crate::create_adapter(AgentType::Claude)
			.load_mcps(scope.project_root(), scope.resource_scope())
			.unwrap();
		assert!(claude_mcps.iter().any(|s| s.name == "partial-server"));

		assert!(
			!project.path().join(".cursor/mcp.json").exists(),
			"failed Cursor config must not be created"
		);
	}

	#[test]
	fn mcp_create_batch_transport_preflight_reject_writes_nothing() {
		let _env = crate::skills::prune::test_lock::env_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		let home = tempfile::tempdir().unwrap();
		let _home = EnvVarGuard::set("HOME", home.path());
		let _aghub =
			EnvVarGuard::set("AGHUB_DATA_DIR", home.path().join("data"));
		let _config =
			EnvVarGuard::set("XDG_CONFIG_HOME", home.path().join(".config"));
		let _state =
			EnvVarGuard::set("XDG_STATE_HOME", home.path().join(".state"));

		let project = tempfile::tempdir().unwrap();
		let scope = WriteScope::project(project.path());
		let server = McpServer::new(
			"sse-server",
			crate::models::McpTransport::sse("https://example.com/sse"),
		);

		let writes = std::cell::Cell::new(0);
		let err = run_mcp_create_batch(
			&[AgentType::Claude, AgentType::OpenCode],
			&scope,
			&server,
			|_agent| {
				writes.set(writes.get() + 1);
				Ok(serde_json::to_value(&server).unwrap())
			},
		)
		.expect_err("preflight must reject the batch");

		assert_eq!(
			crate::error_codes::wire_code(&err),
			"UNSUPPORTED_OPERATION"
		);
		assert!(err.to_string().contains("nothing was written"));
		assert_eq!(writes.get(), 0, "closure must never be invoked");

		assert!(
			!project.path().join(".mcp.json").exists(),
			"preflight rejection must not create config file"
		);
	}

	#[test]
	fn batch_preflight_rejections_convert_to_unsupported_operation() {
		struct Case {
			name: &'static str,
			error: ConfigError,
		}
		let cases = vec![
			Case {
				name: "mcp wrong scope",
				error: ConfigError::from(
					mcp_batch_preflight(
						&[AgentType::Claude, AgentType::AugmentCode],
						ResourceScope::ProjectOnly,
						false,
						None,
					)
					.unwrap_err(),
				),
			},
			Case {
				name: "skill wrong scope",
				error: skill_batch_preflight(
					&[AgentType::Claude, AgentType::JetBrainsAi],
					ResourceScope::GlobalOnly,
				)
				.unwrap_err(),
			},
			Case {
				name: "toggle",
				error: ConfigError::from(
					mcp_batch_preflight(
						&[AgentType::Hermes, AgentType::Windsurf],
						ResourceScope::GlobalOnly,
						true,
						None,
					)
					.unwrap_err(),
				),
			},
			Case {
				name: "transport",
				error: ConfigError::from(
					mcp_batch_preflight(
						&[AgentType::Claude, AgentType::OpenCode],
						ResourceScope::ProjectOnly,
						false,
						Some(&crate::models::McpTransport::sse(
							"https://example.com/v1/messages",
						)),
					)
					.unwrap_err(),
				),
			},
		];

		for case in cases {
			assert_eq!(
				crate::error_codes::wire_code(&case.error),
				"UNSUPPORTED_OPERATION",
				"{}: wire code must be UNSUPPORTED_OPERATION",
				case.name
			);
			assert!(
				!crate::error_codes::retryable(&case.error),
				"{}: batch preflight rejection must not be retryable",
				case.name
			);
			assert!(
				case.error.to_string().contains("nothing was written"),
				"{}: error message must promise nothing was written",
				case.name
			);
		}
	}
}
