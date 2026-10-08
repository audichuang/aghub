use crate::{
	batch::{Backing, RemovalCredits},
	create_adapter,
	errors::{ConfigError, Result},
	manager::{sub_agent::same_sub_agent_content, ConfigManager},
	models::{AgentType, McpServer, ResourceScope, Skill, SubAgent},
	registry, WriteScope,
};
use log::{info, warn};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InstallScope {
	Global,
	Project,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallTarget {
	pub agent: AgentType,
	pub scope: InstallScope,
	pub project_root: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct ResourceLocator {
	pub agent: AgentType,
	pub scope: InstallScope,
	pub project_root: Option<PathBuf>,
	pub name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationAction {
	Copy,
	Delete,
}

#[derive(Debug, Clone)]
struct OperationPlan {
	target: InstallTarget,
	action: OperationAction,
}

impl std::fmt::Display for OperationAction {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			Self::Copy => write!(f, "copy"),
			Self::Delete => write!(f, "delete"),
		}
	}
}

#[derive(Debug, Clone)]
pub struct OperationResult {
	pub target: InstallTarget,
	pub action: OperationAction,
	pub success: bool,
	/// The target ALREADY held this resource and nothing was written — still a
	/// success. Always `false` on a Delete row.
	///
	/// Do not repurpose this to mean "there was nothing to delete": that is
	/// `RemovalKind`'s vocabulary, in `crate::dto::removal`.
	pub already_present: bool,
	pub error: Option<String>,
	pub outcome: Option<crate::dto::RemovalKind>,
	pub still_read_by: Option<Vec<String>>,
	pub still_read_by_managed: Option<Vec<String>>,
	pub still_read_by_unmanaged: Option<Vec<String>>,
}

#[derive(Debug, Clone)]
pub struct OperationBatchResult {
	pub results: Vec<OperationResult>,
}

impl OperationBatchResult {
	pub fn success_count(&self) -> usize {
		self.results.iter().filter(|r| r.success).count()
	}

	pub fn failed_count(&self) -> usize {
		self.results.iter().filter(|r| !r.success).count()
	}
}

/// Serializable wire view of an [`OperationBatchResult`].
///
/// `OperationResult`/`InstallTarget`/`OperationAction` are deliberately NOT
/// `Serialize` (they carry filesystem paths), so this view is the SINGLE place
/// the batch wire shape is defined. Both surfaces use it: the API derives a
/// `ts-rs` DTO that mirrors it for type generation, and the CLI serializes it
/// directly — so neither hand-rolls a second mapping that could drift.
///
/// Field encoding is fixed and load-bearing (both surfaces agreed on it):
/// `scope` is lowercase, `action` is `"copy"`/`"delete"`, and
/// `project_root`/`error` are omitted when absent.
#[derive(Debug, Clone, serde::Serialize)]
pub struct OperationResultView {
	pub agent: String,
	pub scope: &'static str,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub project_root: Option<String>,
	pub action: String,
	pub success: bool,
	/// Duplicate of `success` under the name `core::batch`'s
	/// `AgentOpResultView` uses — both families share one envelope shape, so
	/// either spelling must read correctly.
	/// See docs/history/core-transfer.md#batch-row-ok-field
	pub ok: bool,
	/// The target already held this resource; nothing was written (still a
	/// success row). Emitted unconditionally so a mixed-version client can tell
	/// `false` from "this server does not report it".
	pub already_present: bool,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub error: Option<String>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub outcome: Option<String>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub still_read_by: Option<Vec<String>>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub still_read_by_managed: Option<Vec<String>>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub still_read_by_unmanaged: Option<Vec<String>>,
}

impl From<&OperationResult> for OperationResultView {
	fn from(r: &OperationResult) -> Self {
		OperationResultView {
			agent: r.target.agent.as_str().to_string(),
			scope: match r.target.scope {
				InstallScope::Global => "global",
				InstallScope::Project => "project",
			},
			project_root: r
				.target
				.project_root
				.as_ref()
				.map(|p| p.display().to_string()),
			action: r.action.to_string(),
			success: r.success,
			ok: r.success,
			already_present: r.already_present,
			error: r.error.clone(),
			outcome: r.outcome.map(|k| k.as_str().to_string()),
			still_read_by: r.still_read_by.clone(),
			still_read_by_managed: r.still_read_by_managed.clone(),
			still_read_by_unmanaged: r.still_read_by_unmanaged.clone(),
		}
	}
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct OperationBatchView {
	pub success_count: usize,
	pub failed_count: usize,
	pub results: Vec<OperationResultView>,
}

impl From<&OperationBatchResult> for OperationBatchView {
	fn from(batch: &OperationBatchResult) -> Self {
		OperationBatchView {
			success_count: batch.success_count(),
			failed_count: batch.failed_count(),
			results: batch.results.iter().map(Into::into).collect(),
		}
	}
}

fn build_manager(target: &InstallTarget) -> ConfigManager {
	let adapter = create_adapter(target.agent);
	match target.scope {
		InstallScope::Global => ConfigManager::new(adapter, true, None),
		InstallScope::Project => {
			ConfigManager::new(adapter, false, target.project_root.as_deref())
		}
	}
}

fn target_write_scope(
	scope: InstallScope,
	project_root: Option<&Path>,
) -> Result<WriteScope> {
	match scope {
		InstallScope::Global => Ok(WriteScope::Global),
		InstallScope::Project => {
			let root = project_root.ok_or_else(|| {
				ConfigError::InvalidConfig(
					"project_root is required for project targets".to_string(),
				)
			})?;
			Ok(WriteScope::project(root))
		}
	}
}

fn validate_target(target: &InstallTarget) -> Result<()> {
	target_write_scope(target.scope, target.project_root.as_deref()).map(|_| ())
}

fn target_resource_scope(
	target: &InstallTarget,
) -> crate::models::ResourceScope {
	match target.scope {
		InstallScope::Global => crate::models::ResourceScope::GlobalOnly,
		InstallScope::Project => crate::models::ResourceScope::ProjectOnly,
	}
}

fn mcp_supported_for_target(
	target: &InstallTarget,
	mcp: &McpServer,
	lossless: bool,
) -> Result<()> {
	let descriptor = registry::get(target.agent);
	if !descriptor.supports_mcp_scope(target_resource_scope(target)) {
		return Err(ConfigError::unsupported_operation(
			"copy",
			"MCP server",
			descriptor.id,
		));
	}
	// The whole server, not just its transport: a dialect with no persisted
	// toggle omits a DISABLED one, so the copy would report success while
	// nothing landed. `lossless` is for the callers that DELETE the original
	// afterwards (reconcile) — there a copy that silently shed the server's
	// timeout leaves the only surviving copy missing it. A plain copy keeps the
	// best-effort behaviour it has always had.
	match aghub_agents::descriptor::mcp_fit(descriptor, mcp) {
		aghub_agents::descriptor::McpFit::Exact => Ok(()),
		aghub_agents::descriptor::McpFit::Lossy if !lossless => Ok(()),
		aghub_agents::descriptor::McpFit::Lossy => {
			Err(ConfigError::unsupported_operation(
				"copy without losing fields",
				"MCP server",
				descriptor.id,
			))
		}
		aghub_agents::descriptor::McpFit::Unsupported => {
			Err(ConfigError::unsupported_operation(
				"copy incompatible",
				"MCP server",
				descriptor.id,
			))
		}
	}
}

fn sub_agent_supported_for_target(
	target: &InstallTarget,
	sub_agent: &SubAgent,
	lossless: bool,
) -> Result<()> {
	let descriptor = registry::get(target.agent);
	if !descriptor.supports_sub_agent_scope(target_resource_scope(target)) {
		return Err(ConfigError::unsupported_operation(
			"copy",
			"sub-agent",
			descriptor.id,
		));
	}
	if lossless
		&& target.agent == AgentType::Codex
		&& !sub_agent.extra_frontmatter.is_empty()
	{
		return Err(ConfigError::unsupported_operation(
			"copy without losing fields",
			"sub-agent",
			descriptor.id,
		));
	}
	Ok(())
}

fn load_source_mcp(source: &ResourceLocator) -> Result<McpServer> {
	let mut manager = build_manager(&InstallTarget {
		agent: source.agent,
		scope: source.scope,
		project_root: source.project_root.clone(),
	});
	manager.load()?;
	manager.get_mcp(&source.name).cloned().ok_or_else(|| {
		ConfigError::resource_not_found("MCP server", &source.name)
	})
}

fn ensure_mcp_source_fields_representable(
	source: &ResourceLocator,
	mcp: &McpServer,
) -> Result<()> {
	let path = build_manager(&InstallTarget {
		agent: source.agent,
		scope: source.scope,
		project_root: source.project_root.clone(),
	})
	.config_path()
	.ok_or_else(|| {
		ConfigError::InvalidConfig("MCP source path unavailable".to_string())
	})?;
	let content = fs::read_to_string(path)?;
	let serialize = registry::get(source.agent)
		.mcp_serialize_config
		.ok_or_else(|| {
			ConfigError::InvalidConfig(
				"MCP source serializer unavailable".to_string(),
			)
		})?;
	if aghub_agents::format::unmanaged_mcp_source_fields(
		mcp, &content, serialize,
	)? {
		return Err(ConfigError::InvalidConfig(format!(
			"MCP server '{}' has unmanaged fields that cannot be copied before removing its source",
			source.name
		)));
	}
	Ok(())
}

fn ensure_sub_agent_source_fields_representable(
	source: &ResourceLocator,
	sub_agent: &SubAgent,
) -> Result<()> {
	if source.agent != AgentType::Codex {
		return Ok(());
	}
	let path = sub_agent.source_path.as_deref().ok_or_else(|| {
		ConfigError::InvalidConfig(
			"Codex sub-agent source path unavailable".to_string(),
		)
	})?;
	if aghub_agents::agents::codex::sub_agent_has_unmanaged_fields(Path::new(
		path,
	))? {
		return Err(ConfigError::InvalidConfig(format!(
			"Codex sub-agent '{}' has unmanaged fields that cannot be copied before removing its source",
			source.name
		)));
	}
	Ok(())
}

fn load_source_skill(source: &ResourceLocator) -> Result<Skill> {
	let mut manager = build_manager(&InstallTarget {
		agent: source.agent,
		scope: source.scope,
		project_root: source.project_root.clone(),
	});
	manager.load()?;
	manager
		.get_skill(&source.name)
		.cloned()
		.ok_or_else(|| ConfigError::resource_not_found("skill", &source.name))
}

/// Does the reconcile/transfer SOURCE resource exist? Read-only.
///
/// The preview seam: uses the same loaders as the real reconcile so the
/// existence rule cannot drift.
/// See docs/history/core-transfer.md#reconcile-preview-approved-what-the-commit-refused
pub fn ensure_mcp_exists(source: &ResourceLocator) -> Result<()> {
	load_source_mcp(source).map(|_| ())
}

/// See [`ensure_mcp_exists`].
pub fn ensure_sub_agent_exists(source: &ResourceLocator) -> Result<()> {
	load_source_sub_agent(source).map(|_| ())
}

/// Can we PROVE the target now holds the source content?
///
/// Compared by the npx-compatible folder hash (the lock files' digest). It has
/// blind spots by design — symlinks, `.git` / `node_modules`, its file/size
/// bounds — and this gates a DESTRUCTIVE step, so a blind spot answers
/// `Unprovable`, never `Landed`. Three answers, not two: "differs" when aghub
/// could not look sends the caller after a difference that may not exist.
enum ContentProof {
	Landed,
	Differs,
	/// Why the comparison could not be trusted, phrased for the user.
	Unprovable(String),
}

/// Does this tree hold anything the folder hash cannot see? Two trees differing
/// only there hash EQUAL.
/// See docs/history/core-transfer.md#folder-hash-blind-spots-authorised-a-removal
fn has_unhashed_entries(dir: &Path, depth: usize) -> bool {
	// The hash's OWN depth bound, not a private guess; past it the hash refuses
	// the tree itself with the accurate "could not be hashed" answer.
	if depth >= skill::hash::MAX_DEPTH {
		return false;
	}
	let Ok(entries) = std::fs::read_dir(dir) else {
		return true;
	};
	for entry in entries.flatten() {
		let path = entry.path();
		let Ok(meta) = std::fs::symlink_metadata(&path) else {
			return true;
		};
		let ft = meta.file_type();
		if ft.is_symlink() {
			return true;
		}
		if ft.is_dir() {
			let name = entry.file_name();
			// The hash skips these two by name; content inside them is
			// invisible to it, so it cannot authorise a removal either.
			if name == ".git" || name == "node_modules" {
				return true;
			}
			if has_unhashed_entries(&path, depth + 1) {
				return true;
			}
		}
	}
	false
}

fn prove_content_landed(
	source_root: &Path,
	target: &InstallTarget,
	name: &str,
) -> ContentProof {
	let unprovable = |why: &str| ContentProof::Unprovable(why.to_string());
	let mut manager = build_manager(target);
	if ensure_loaded(&mut manager).is_err() {
		return unprovable("the target's config could not be read");
	}
	let Some(installed) = manager.get_skill(name) else {
		return unprovable("the target does not report holding it at all");
	};
	let Some(recorded) = installed
		.canonical_path
		.as_deref()
		.or(installed.source_path.as_deref())
	else {
		return unprovable("the target's entry records no path");
	};
	let installed_dir = resolve_skill_file(recorded);
	let Some(installed_dir) = installed_dir.parent() else {
		return unprovable("the target's recorded path has no directory");
	};
	if has_unhashed_entries(source_root, 0)
		|| has_unhashed_entries(installed_dir, 0)
	{
		return unprovable(
			"the skill folder contains symbolic links or a .git/node_modules \
			 directory, which the npx-compatible folder hash deliberately does \
			 not cover — so two folders differing only there compare EQUAL",
		);
	}
	match (
		skill::hash::compute_skill_folder_hash(source_root),
		skill::hash::compute_skill_folder_hash(installed_dir),
	) {
		(Ok(a), Ok(b)) if a == b => ContentProof::Landed,
		(Ok(_), Ok(_)) => ContentProof::Differs,
		_ => unprovable(
			"the folder could not be hashed (it may exceed the file or size \
			 bounds the lock format allows)",
		),
	}
}

/// What a backing lookup could determine about one target.
///
/// "Holds no such resource" and "config would not parse, cannot tell" are
/// different answers; a guard against data loss fails CLOSED on the second.
enum Backed {
	/// The resolved backing path this target reads.
	At(PathBuf),
	/// Determined: this target holds no such resource.
	Absent,
	/// Undeterminable — treat as a collision wherever that is the safe read.
	Unknown,
}

/// Refuse a removal that would take the resource from something that must
/// SURVIVE this reconcile: every copy target, and the source unless the caller
/// asked to remove it too (the "move it" shape).
///
/// Membership is a property of the FILE, never the agent id: two ids land on
/// one file by design (Claude and Copilot share project `.mcp.json`) and by
/// accident (symlinked home, agent-home env override). Unchecked, the copy
/// reports `already_present`, the removal rewrites the shared file, and every
/// row reports success with the resource gone.
/// See docs/history/core-transfer.md#shared-backing-destroyed-a-resource
fn ensure_removals_spare<F>(
	protect: &[Protected],
	removing: &[InstallTarget],
	source_agent: AgentType,
	backing: F,
) -> Result<()>
where
	F: Fn(&InstallTarget) -> Backed,
{
	ensure_removals_spare_for(
		protect,
		removing,
		source_agent,
		backing,
		Caller::Reconcile,
	)
}

/// Which command the refusal is addressed to: its remedy has to name a step
/// that command can actually take.
#[derive(Clone, Copy, PartialEq)]
enum Caller {
	Reconcile,
	Delete,
}

fn ensure_removals_spare_for<F>(
	protect: &[Protected],
	removing: &[InstallTarget],
	source_agent: AgentType,
	backing: F,
	caller: Caller,
) -> Result<()>
where
	F: Fn(&InstallTarget) -> Backed,
{
	for delete in removing {
		// The delete side stays permissive: a target whose own backing cannot
		// be read will fail its own row anyway, and refusing here would turn
		// that into a whole-batch abort.
		let Backed::At(delete_path) = backing(delete) else {
			continue;
		};
		let delete_backing = Backing::of(delete_path);
		for kept in protect {
			// The caller asked to remove it from THIS agent; that it also
			// holds it is the point, not a collision.
			if kept.target.agent == delete.agent {
				continue;
			}
			let kept_backing = match backing(&kept.target) {
				Backed::At(path) => Backing::of(path),
				Backed::Absent => continue,
				// Cannot tell whether this agent shares the file being
				// rewritten. Skipping is the answer that loses data.
				Backed::Unknown => {
					let remedy = match caller {
						Caller::Reconcile => "name it in this reconcile too",
						Caller::Delete => "include it in this delete too",
					};
					return Err(ConfigError::InvalidConfig(format!(
						"cannot tell whether '{}' shares the same file as \
						 '{}' — its configuration failed to load, so removing \
						 from '{}' might take it from '{}' as well. Fix that \
						 agent's config, or {remedy}.",
						kept.target.agent.as_str(),
						delete.agent.as_str(),
						delete.agent.as_str(),
						kept.target.agent.as_str(),
					)));
				}
			};
			if delete_backing.is(&kept_backing) {
				// Name WHY the partner is in the protect list, and give the
				// remedy that actually applies to it. Without the role the
				// message can cite an agent the caller never typed:
				// `--from-agent warp --add claude --remove cline` answering
				// "'cline' and 'warp'" reads as a non sequitur. And an agent
				// the command never named cannot be "dropped from this
				// reconcile" — the only way forward is to remove it too.
				let drop_it = format!(
					"Drop '{}' from this reconcile.",
					delete.agent.as_str()
				);
				let (role, remedy) = if caller == Caller::Delete {
					(
						"",
						format!(
							"Delete it from both in one request (CLI: \
							 `-a {},{}`; desktop: select both agents), or \
							 leave it in place.",
							delete.agent.as_str(),
							kept.target.agent.as_str()
						),
					)
				} else if kept.target.agent == source_agent {
					(" (the --from-agent source of this reconcile)", drop_it)
				} else if kept.named {
					("", drop_it)
				} else {
					(
						" — an agent this reconcile never named —",
						format!(
							"Add '{}' to --remove as well (repeat the flag; \
							 it takes no comma list), or drop '{}' from this \
							 reconcile.",
							kept.target.agent.as_str(),
							delete.agent.as_str()
						),
					)
				};
				return Err(ConfigError::InvalidConfig(format!(
					"'{}' and '{}'{} resolve to the same place on disk ({}), \
					 so removing it from the first would take it from the \
					 second as well — not a state that can exist. {remedy}",
					delete.agent.as_str(),
					kept.target.agent.as_str(),
					role,
					delete_backing.path.display(),
				)));
			}
		}
	}
	Ok(())
}

/// The shared-backing refusal a `--yes` run would raise, WITHOUT writing
/// anything — one definition for preview and commit, so a preview cannot
/// green-light what the commit refuses.
/// See docs/history/core-transfer.md#reconcile-preview-approved-what-the-commit-refused
fn ensure_reconcile_spares<F>(
	source: &ResourceLocator,
	added: &[AgentType],
	removed: &[AgentType],
	roster: bool,
	backing: F,
) -> Result<()>
where
	F: Fn(&InstallTarget) -> Backed,
{
	let (copies, deletes) = reconcile_plans(
		added.to_vec(),
		removed.to_vec(),
		source.scope,
		source.project_root.clone(),
	);
	let removing: Vec<InstallTarget> =
		deletes.iter().map(|plan| plan.target.clone()).collect();
	let protect = protected_targets(
		&copies,
		source,
		removed.contains(&source.agent),
		&removing,
		roster,
	);
	ensure_removals_spare(&protect, &removing, source.agent, backing)
}

/// The shared-reader guard for a direct MCP delete. `requested` is every agent
/// this one request removes the server from (a CLI `-a` list, a desktop
/// multi-select); an agent reading the same file outside that set must not
/// lose it behind the caller's back.
pub fn ensure_mcp_delete_spares(
	source: &ResourceLocator,
	requested: &[AgentType],
) -> Result<()> {
	let (_, deletes) = reconcile_plans(
		Vec::new(),
		requested.to_vec(),
		source.scope,
		source.project_root.clone(),
	);
	let removing: Vec<InstallTarget> =
		deletes.iter().map(|plan| plan.target.clone()).collect();
	let protect = protected_targets(&[], source, true, &removing, true);
	ensure_removals_spare_for(
		&protect,
		&removing,
		source.agent,
		mcp_backing_path,
		Caller::Delete,
	)
}

/// [`ensure_reconcile_spares`] for a skill — the preview seam.
pub fn ensure_skill_reconcile_spares(
	source: &ResourceLocator,
	added: &[AgentType],
	removed: &[AgentType],
) -> Result<()> {
	ensure_reconcile_spares(source, added, removed, false, skill_backing_dir)
}

/// [`ensure_reconcile_spares`] for an MCP — the preview seam.
pub fn ensure_mcp_reconcile_spares(
	source: &ResourceLocator,
	added: &[AgentType],
	removed: &[AgentType],
) -> Result<()> {
	ensure_reconcile_spares(source, added, removed, true, mcp_backing_path)
}

/// [`ensure_reconcile_spares`] for a sub-agent — the preview seam.
pub fn ensure_sub_agent_reconcile_spares(
	source: &ResourceLocator,
	added: &[AgentType],
	removed: &[AgentType],
) -> Result<()> {
	ensure_reconcile_spares(source, added, removed, true, |target| {
		sub_agent_backing_path(target, &source.name)
	})
}

/// One entry of the protect list, plus whether the caller NAMED it.
///
/// The flag exists for the refusal message only: a named partner is dropped
/// from the command, an agent the command never mentioned can only be added to
/// `--remove`.
struct Protected {
	target: InstallTarget,
	named: bool,
}

/// Everything a reconcile must not destroy: the copy targets, plus the source
/// unless the caller asked to remove the source too — and with `roster`, every
/// OTHER agent in the registry that is not itself being removed.
///
/// The roster is the REGISTRY, not the installed agents: an agent that appears
/// nowhere in the command can still share the backing file, and one we cannot
/// see is not one that does not read it.
/// See docs/history/core-transfer.md#shared-backing-destroyed-a-resource
///
/// Skills pass `roster: false`: `<root>/.agents/skills` is a shared read path
/// for most of the project roster, so a roster list would refuse every removal
/// touching it. What a skill removal takes away is decided by
/// `remove_skill_planned` / `removal::read_effect_after`, not here.
fn protected_targets(
	copies: &[OperationPlan],
	source: &ResourceLocator,
	source_removed: bool,
	removing: &[InstallTarget],
	roster: bool,
) -> Vec<Protected> {
	let mut protect: Vec<Protected> = copies
		.iter()
		.map(|plan| Protected {
			target: plan.target.clone(),
			named: true,
		})
		.collect();
	if !source_removed {
		protect.push(Protected {
			target: InstallTarget {
				agent: source.agent,
				scope: source.scope,
				project_root: source.project_root.clone(),
			},
			named: true,
		});
	}
	if roster {
		for descriptor in registry::iter_all() {
			let Ok(agent) = descriptor.id.parse::<AgentType>() else {
				continue;
			};
			if removing.iter().any(|target| target.agent == agent)
				|| protect.iter().any(|kept| kept.target.agent == agent)
			{
				continue;
			}
			protect.push(Protected {
				target: InstallTarget {
					agent,
					scope: source.scope,
					project_root: source.project_root.clone(),
				},
				named: false,
			});
		}
	}
	protect
}

/// Read a removal that found nothing as success when a SIBLING row of the same
/// reconcile already emptied the shared backing — and record a real deletion.
///
/// `took` is the caller's own answer to "did this row really empty its
/// backing?": `remove_mcp`/`remove_sub_agent` delete or error.
/// Always `Ok(false)`: a Delete row is never `already_present`.
/// Defined for MCP and sub-agent delete arms; the skill arm's sibling
/// credit moved into [`crate::skills::removal::remove_skill_batch`].
/// See docs/history/core-transfer.md#sibling-rows-sharing-one-backing
fn sibling_already_took_it(
	removed: Result<bool>,
	agent: AgentType,
	credits: &mut RemovalCredits<AgentType>,
) -> Result<bool> {
	match removed {
		Ok(took) => {
			if took {
				credits.credit(agent);
			}
			Ok(false)
		}
		Err(ConfigError::ResourceNotFound { .. })
			if credits.already_taken(&agent) =>
		{
			Ok(false)
		}
		Err(error) => Err(error),
	}
}

/// The directory an agent writes ITS OWN skills into, for
/// [`ensure_removals_spare`] — deliberately not the Master: several agents
/// linking one Master is the normal state, and `remove_skill_planned` keeps it.
///
/// Two agents can share this dir by design (the shared `.agents/skills` slot)
/// or by accident (symlinked home). Only the pair the caller NAMED is refused —
/// an add and a remove landing in one dir cannot both be honoured. Unnamed
/// sharers are unprotected (`roster: false`): on the shared slot, sharing IS
/// the grant model.
/// See docs/history/core-transfer.md#shared-backing-destroyed-a-resource
fn skill_backing_dir(target: &InstallTarget) -> Backed {
	// A pure path derivation — it reads no agent config, so a failure here is
	// "this scope has no skills dir for that agent", not "cannot tell".
	match skill_target_dir(target) {
		Ok(dir) => Backed::At(dir),
		Err(_) => Backed::Absent,
	}
}

/// The file an agent's MCP entries live in, for [`ensure_removals_spare`].
fn mcp_backing_path(target: &InstallTarget) -> Backed {
	// `config_path()` asks the descriptor where the file WOULD be; it does not
	// open or parse it, so `None` means "this agent has no MCP file at this
	// scope" and never "unreadable".
	match build_manager(target).config_path() {
		Some(path) => Backed::At(path),
		None => Backed::Absent,
	}
}

/// The file one agent's sub-agent of this name lives in, for
/// [`ensure_removals_spare`].
///
/// Each sub-agent IS its own `.md` file, so the key is the resolved path this
/// target sees. Ask the filesystem, not the descriptor table: dirs distinct on
/// paper can be one dir behind a symlinked ancestor (allowed — see
/// `agents/src/sub_agents.rs`) or an env override. `Absent` is the ordinary
/// copy case, not a collision: two targets resolving to one directory either
/// both see the file or neither does.
fn sub_agent_backing_path(target: &InstallTarget, name: &str) -> Backed {
	let mut manager = build_manager(target);
	// `load()` parses MCPs too; an unrelated malformed config is "cannot tell",
	// not "no such sub-agent".
	if ensure_loaded(&mut manager).is_err() {
		return Backed::Unknown;
	}
	match manager
		.get_sub_agent(name)
		.and_then(|s| s.source_path.clone())
	{
		Some(path) => Backed::At(PathBuf::from(path)),
		None => Backed::Absent,
	}
}

/// Copy one MCP into a target. `Ok(true)` = the target already had an
/// EQUIVALENT server and nothing was written.
///
/// Equivalence, not name collision: a same-named entry can serve a different
/// command or URL. A differing entry is a hard conflict (`update_mcp` changes
/// one). Shared by `transfer_mcp` and `reconcile_mcp` so they cannot disagree.
///
// ponytail: equivalence compares the in-memory model. A dialect whose writer
// drops a field aghub does model will re-read unequal, so a repeat transfer
// into it still errors. Upgrade path: compare the dialect-projected form.
fn copy_mcp_into(target: &InstallTarget, mcp: &McpServer) -> Result<bool> {
	let mut manager = build_manager(target);
	ensure_loaded(&mut manager)?;
	if let Some(existing) = manager.get_mcp(&mcp.name) {
		// `config_source` is load-time provenance, not part of the value.
		if mcp_value_matches(existing, mcp) {
			return Ok(true);
		}
		return Err(ConfigError::resource_exists("MCP server", &mcp.name));
	}
	manager.add_mcp(mcp.clone())?;
	Ok(false)
}

fn mcp_value_matches(actual: &McpServer, expected: &McpServer) -> bool {
	actual.name == expected.name
		&& actual.enabled == expected.enabled
		&& actual.transport == expected.transport
		&& actual.timeout == expected.timeout
}

fn delete_reconciled_mcp(
	source: &ResourceLocator,
	target: &InstallTarget,
	expected: &McpServer,
	protect: &[Protected],
	copies: &[OperationPlan],
	source_removed: bool,
) -> Result<bool> {
	let mut manager = build_manager(target);
	ensure_loaded(&mut manager)?;
	manager
		.remove_mcp_planned_checked(&source.name, false, true, |current| {
			// Re-resolve the backing after copies, under the same lock as the
			// fresh read and rewrite. A newly created config can change it.
			ensure_removals_spare(
				protect,
				std::slice::from_ref(target),
				source.agent,
				mcp_backing_path,
			)?;
			if source_removed
				&& current.iter().any(|server| server.name == source.name)
			{
				let source_target = InstallTarget {
					agent: source.agent,
					scope: source.scope,
					project_root: source.project_root.clone(),
				};
				let same_backing = match (
					mcp_backing_path(&source_target),
					mcp_backing_path(target),
				) {
					(Backed::At(source_path), Backed::At(target_path)) => {
						Backing::of(source_path).is(&Backing::of(target_path))
					}
					_ => {
						return Err(ConfigError::InvalidConfig(
							"cannot verify MCP source backing during reconcile"
								.into(),
						));
					}
				};
				if same_backing {
					// A sibling agent can delete the source first when both
					// descriptors read one file. Compare through the source's
					// dialect even when this delete row names the sibling.
					let actual = load_source_mcp(source)?;
					if !mcp_value_matches(&actual, expected) {
						return Err(ConfigError::InvalidConfig(format!(
							"MCP server '{}' changed in its source during reconcile; retry",
							source.name
						)));
					}
					if !copies.is_empty() {
						ensure_mcp_source_fields_representable(
							source, &actual,
						)?;
						for copy in copies {
							let mut copied = build_manager(&copy.target);
							ensure_loaded(&mut copied)?;
							if !copied.get_mcp(&source.name).is_some_and(
								|server| mcp_value_matches(server, expected),
							) {
								return Err(ConfigError::InvalidConfig(
									format!(
									"MCP server '{}' changed in target '{}' during reconcile; source kept",
									source.name,
									copy.target.agent.as_str()
								),
								));
							}
						}
					}
				}
			}
			Ok(())
		})
		.map(|_| true)
}

fn delete_reconciled_sub_agent(
	source: &ResourceLocator,
	target: &InstallTarget,
	expected: &SubAgent,
	protect: &[Protected],
	copies: &[OperationPlan],
	source_removed: bool,
) -> Result<bool> {
	// Re-check now that every copy has run: the file this
	// target resolves to may be one a copy just created.
	ensure_removals_spare(
		protect,
		std::slice::from_ref(target),
		source.agent,
		|t| sub_agent_backing_path(t, &source.name),
	)?;
	let mut manager = build_manager(target);
	ensure_loaded(&mut manager)?;
	// Same as the MCP arm: `Ok` only ever follows a real
	// delete, so it is the credential.
	let source_backing = expected.source_path.as_deref();
	let current_backing = manager
		.get_sub_agent(&source.name)
		.and_then(|agent| agent.source_path.as_deref());
	let shares_source = match (source_backing, current_backing) {
		(Some(source_path), Some(current_path)) => {
			skill::lock::resolve_existing(Path::new(source_path))
				== skill::lock::resolve_existing(Path::new(current_path))
		}
		_ => false,
	};
	let removes_source =
		target.agent == source.agent || (source_removed && shares_source);
	if removes_source {
		manager.remove_sub_agent_if_unchanged(
			&source.name,
			expected,
			!copies.is_empty(),
			|| ensure_sub_agent_copies_hold(copies, expected),
		)?;
	} else {
		manager.remove_sub_agent(&source.name)?;
	}
	Ok(true)
}

/// Copy one sub-agent into a target. See [`copy_mcp_into`] for why equivalence
/// (not name collision) decides. `source_path` / `config_source` are per-agent
/// file locations, so they are excluded from the comparison.
fn copy_sub_agent_into(
	target: &InstallTarget,
	sub_agent: &SubAgent,
) -> Result<bool> {
	let mut manager = build_manager(target);
	ensure_loaded(&mut manager)?;
	if let Some(existing) = manager.get_sub_agent(&sub_agent.name) {
		let equivalent = same_sub_agent_content(existing, sub_agent);
		if equivalent {
			return Ok(true);
		}
		return Err(ConfigError::resource_exists("sub_agent", &sub_agent.name));
	}
	manager.add_sub_agent(sub_agent.clone())?;
	Ok(false)
}

/// Before a reconcile deletes a sub-agent's source, confirm each copy target
/// still holds the copied content; a copy changed or removed since the copy
/// row ran would otherwise make the source's deletion lose the resource.
fn ensure_sub_agent_copies_hold(
	copies: &[OperationPlan],
	sub_agent: &SubAgent,
) -> Result<()> {
	for copy in copies {
		let mut copied = build_manager(&copy.target);
		ensure_loaded(&mut copied)?;
		let holds = copied
			.get_sub_agent(&sub_agent.name)
			.is_some_and(|held| same_sub_agent_content(held, sub_agent));
		if !holds {
			return Err(ConfigError::InvalidConfig(format!(
				"Sub-agent '{}' changed in target '{}' during reconcile; source kept",
				sub_agent.name,
				copy.target.agent.as_str()
			)));
		}
	}
	Ok(())
}

fn ensure_loaded(manager: &mut ConfigManager) -> Result<()> {
	match manager.load() {
		Ok(_) => Ok(()),
		Err(ConfigError::NotFound { .. }) => {
			manager.init_empty_config();
			Ok(())
		}
		Err(err) => Err(err),
	}
}

fn resolve_skill_file(path: &str) -> PathBuf {
	if let Some(stripped) = path.strip_prefix("~/") {
		if let Some(home) = dirs::home_dir() {
			home.join(stripped)
		} else {
			PathBuf::from(path)
		}
	} else {
		PathBuf::from(path)
	}
}

/// Resolve a skill's on-disk root directory WITHOUT requiring it to exist.
///
/// Prefers `canonical_path` over `source_path` (tilde-expanded); a `SKILL.md`
/// path yields its PARENT. `None` only when no path is recorded. The one
/// resolver behind `resolve_skill_root` and the removal planner.
pub(crate) fn skill_root_unchecked(skill: &Skill) -> Option<PathBuf> {
	let path = skill
		.canonical_path
		.as_deref()
		.or(skill.source_path.as_deref())
		.map(resolve_skill_file)?;

	let is_skill_file = path
		.file_name()
		.is_some_and(|name| name == std::ffi::OsStr::new("SKILL.md"));

	Some(if is_skill_file {
		path.parent().map(Path::to_path_buf).unwrap_or(path)
	} else {
		path
	})
}

fn resolve_skill_root(skill: &Skill) -> Result<PathBuf> {
	let root = skill_root_unchecked(skill).ok_or_else(|| {
		ConfigError::InvalidConfig(format!(
			"Skill '{}' has no source path to copy from",
			skill.name
		))
	})?;

	if !root.exists() {
		return Err(ConfigError::InvalidConfig(format!(
			"Skill source path '{}' does not exist",
			root.display()
		)));
	}

	Ok(root)
}

fn skill_target_dir(target: &InstallTarget) -> Result<PathBuf> {
	let adapter = create_adapter(target.agent);
	let dir = adapter.target_skills_dir(
		target.project_root.as_deref(),
		match target.scope {
			InstallScope::Global => crate::models::ResourceScope::GlobalOnly,
			InstallScope::Project => crate::models::ResourceScope::ProjectOnly,
		},
	);

	dir.ok_or_else(|| {
		ConfigError::unsupported_operation(
			"persist",
			"skill",
			registry::get(target.agent).id,
		)
	})
}

fn unique_targets(targets: Vec<InstallTarget>) -> Vec<InstallTarget> {
	let mut seen = HashSet::new();
	let mut unique = Vec::new();
	for target in targets {
		let key = format!(
			"{}|{:?}|{}",
			target.agent.as_str(),
			target.scope,
			target
				.project_root
				.as_ref()
				.map(|path| path.display().to_string())
				.unwrap_or_default()
		);
		if seen.insert(key) {
			unique.push(target);
		}
	}
	unique
}

/// Reject a transfer that names no destinations; otherwise it exits 0 having
/// copied nothing. Both surfaces route through `transfer_*`.
fn ensure_destinations(destinations: &[InstallTarget]) -> Result<()> {
	if destinations.is_empty() {
		return Err(ConfigError::InvalidConfig(
			"no destination agents given; specify at least one target"
				.to_string(),
		));
	}
	Ok(())
}

/// Preconditions every `reconcile_*` shares.
///
/// A reconcile that REMOVES needs explicit confirmation; the policy lives here
/// so CLI `--yes` and API `confirm` are adapters over one rule. For an API
/// client it is the only gate.
fn ensure_reconcilable(
	added: &[AgentType],
	removed: &[AgentType],
	confirm: bool,
) -> Result<()> {
	ensure_disjoint(added, removed)?;
	if !removed.is_empty() && !confirm {
		return Err(ConfigError::InvalidConfig(format!(
			"reconcile would remove this resource from {} agent(s); \
			 confirm the removal explicitly to proceed",
			removed.len()
		)));
	}
	Ok(())
}

/// Reject an agent in BOTH the add and remove sets (adds run first, so it would
/// silently net to a delete).
///
/// Public so a preview can apply it read-only: `confirm = false` is not a
/// dry-run switch.
/// See docs/history/core-transfer.md#reconcile-preview-approved-what-the-commit-refused
pub fn ensure_disjoint(
	added: &[AgentType],
	removed: &[AgentType],
) -> Result<()> {
	for agent in added {
		if removed.contains(agent) {
			return Err(ConfigError::InvalidConfig(format!(
				"agent '{}' appears in both add and remove",
				agent.as_str()
			)));
		}
	}
	Ok(())
}

fn copy_plans(destinations: Vec<InstallTarget>) -> Vec<OperationPlan> {
	destinations
		.into_iter()
		.map(|target| OperationPlan {
			target,
			action: OperationAction::Copy,
		})
		.collect()
}

/// Build the two reconcile groups separately (rather than one flat `Vec`) so
/// callers can hand them to
/// [`crate::batch::run_staged_multi_target_mutation`] as primary (copies) /
/// secondary (deletes) — a runtime copy failure must never let its paired
/// delete run.
fn reconcile_plans(
	added: Vec<AgentType>,
	removed: Vec<AgentType>,
	scope: InstallScope,
	project_root: Option<PathBuf>,
) -> (Vec<OperationPlan>, Vec<OperationPlan>) {
	let copies = added
		.into_iter()
		.map(|agent| OperationPlan {
			target: InstallTarget {
				agent,
				scope,
				project_root: project_root.clone(),
			},
			action: OperationAction::Copy,
		})
		.collect();
	// Deduplicate BEFORE any row exists, so a duplicate target cannot spend the
	// `RemovalCredits` receipt its twin earned. The single place rows are
	// built. See docs/history/core-transfer.md#sibling-rows-sharing-one-backing
	let deletes = unique_targets(
		removed
			.into_iter()
			.map(|agent| InstallTarget {
				agent,
				scope,
				project_root: project_root.clone(),
			})
			.collect(),
	)
	.into_iter()
	.map(|target| OperationPlan {
		target,
		action: OperationAction::Delete,
	})
	.collect();
	(copies, deletes)
}

fn batch_preflight_error(
	operation: &str,
	error: crate::batch::MultiTargetMutationError<OperationPlan, ConfigError>,
) -> ConfigError {
	// Keep the VARIANT when every row refused for the same domain reason: batch
	// aggregation must not relabel the answer (and its HTTP status).
	// See docs/history/core-transfer.md#batch-refusal-variant-was-flattened
	let all_unsupported = !error.failures.is_empty()
		&& error.failures.iter().all(|f| {
			matches!(f.reason, ConfigError::UnsupportedOperation { .. })
		});
	let rejected_targets: Vec<crate::errors::RejectedTarget> = error
		.failures
		.iter()
		.flat_map(|failure| {
			if let Some(targets) = failure.reason.rejected_targets() {
				if !targets.is_empty() {
					return targets
						.iter()
						.map(|target| {
							let mut t = target.clone();
							if t.agent.is_empty() {
								t.agent = failure
									.target
									.target
									.agent
									.as_str()
									.to_string();
							}
							t
						})
						.collect::<Vec<_>>();
				}
			}
			vec![crate::errors::RejectedTarget {
				agent: failure.target.target.agent.as_str().to_string(),
				reason: failure.reason.to_string(),
				kind: None,
				path: None,
				readers: None,
			}]
		})
		.collect();
	let failures = error
		.failures
		.into_iter()
		.map(|failure| {
			let scope = match failure.target.target.scope {
				InstallScope::Global => "global",
				InstallScope::Project => "project",
			};
			format!(
				"{} {} ({scope}): {}",
				failure.target.action,
				failure.target.target.agent.as_str(),
				failure.reason
			)
		})
		.collect::<Vec<_>>()
		.join("; ");
	let message = format!(
		"{operation} preflight failed; nothing was written: {failures}"
	);
	if all_unsupported {
		ConfigError::UnsupportedOperation {
			message,
			rejected_targets: Some(rejected_targets),
		}
	} else {
		ConfigError::InvalidConfigWithTargets {
			message,
			rejected_targets: Some(rejected_targets),
		}
	}
}

/// The success payload is `bool` = "the target already had it, nothing was
/// written". This is the ONE place that bool reaches the wire.
fn operation_batch(
	report: crate::batch::MultiTargetMutationReport<
		OperationPlan,
		bool,
		ConfigError,
	>,
) -> OperationBatchResult {
	OperationBatchResult {
		results: report
			.results
			.into_iter()
			.map(|row| {
				let (success, already_present, error) = match row.result {
					Ok(already_present) => (true, already_present, None),
					Err(error) => (false, false, Some(error.to_string())),
				};
				OperationResult {
					target: row.target.target,
					action: row.target.action,
					success,
					already_present,
					error,
					outcome: None,
					still_read_by: None,
					still_read_by_managed: None,
					still_read_by_unmanaged: None,
				}
			})
			.collect(),
	}
}

fn log_operation_outcome(
	resource: &str,
	name: &str,
	action: OperationAction,
	target: &InstallTarget,
	outcome: &Result<bool>,
) {
	let target_agent = registry::get(target.agent).id;
	let target_scope = match target.scope {
		InstallScope::Global => "global",
		InstallScope::Project => "project",
	};
	match outcome {
		Ok(_) => info!(
			"{} {} '{}' for agent '{}' in {} scope succeeded",
			action, resource, name, target_agent, target_scope
		),
		Err(error) => warn!(
			"{} {} '{}' for agent '{}' in {} scope failed: {}",
			action, resource, name, target_agent, target_scope, error
		),
	}
}

pub fn transfer_mcp(
	source: ResourceLocator,
	destinations: Vec<InstallTarget>,
) -> Result<OperationBatchResult> {
	let mcp = load_source_mcp(&source)?;
	let destinations = unique_targets(destinations);
	ensure_destinations(&destinations)?;
	info!(
		"transferring MCP '{}' to {} destination(s)",
		mcp.name,
		destinations.len()
	);
	let report = crate::batch::run_multi_target_mutation(
		&destinations,
		|target| {
			validate_target(target)?;
			mcp_supported_for_target(target, &mcp, false)
		},
		|target| {
			let outcome = copy_mcp_into(target, &mcp);
			log_operation_outcome(
				"MCP",
				&mcp.name,
				OperationAction::Copy,
				target,
				&outcome,
			);
			outcome
		},
	)
	.map_err(|error| {
		let failures = error
			.failures
			.into_iter()
			.map(|failure| {
				let scope = match failure.target.scope {
					InstallScope::Global => "global",
					InstallScope::Project => "project",
				};
				format!(
					"{} ({scope}): {}",
					failure.target.agent.as_str(),
					failure.reason
				)
			})
			.collect::<Vec<_>>()
			.join("; ");
		ConfigError::InvalidConfig(format!(
			"MCP transfer preflight failed; nothing was written: {failures}"
		))
	})?;

	let results = report
		.results
		.into_iter()
		.map(|row| {
			let (success, already_present, error) = match row.result {
				Ok(already_present) => (true, already_present, None),
				Err(error) => (false, false, Some(error.to_string())),
			};
			OperationResult {
				target: row.target,
				action: OperationAction::Copy,
				success,
				already_present,
				error,
				outcome: None,
				still_read_by: None,
				still_read_by_managed: None,
				still_read_by_unmanaged: None,
			}
		})
		.collect();

	Ok(OperationBatchResult { results })
}

pub fn reconcile_mcp(
	source: ResourceLocator,
	added: Vec<AgentType>,
	removed: Vec<AgentType>,
	confirm: bool,
) -> Result<OperationBatchResult> {
	ensure_reconcilable(&added, &removed, confirm)?;
	let mcp = load_source_mcp(&source)?;
	info!(
		"reconciling MCP '{}' with {} added and {} removed agent(s)",
		mcp.name,
		added.len(),
		removed.len()
	);
	// Strict only when THIS source is the copy that disappears. Removing some
	// OTHER agent leaves the faithful original in place, so its copies are as
	// best-effort as a plain transfer.
	let deletes_source = removed.contains(&source.agent);
	let source_removed = deletes_source;
	let (copies, deletes) = reconcile_plans(
		added,
		removed,
		source.scope,
		source.project_root.clone(),
	);
	if deletes_source && !copies.is_empty() {
		ensure_mcp_source_fields_representable(&source, &mcp)?;
	}
	// Before ANY write: an add and a remove resolving to one file cannot both
	// be honoured. The protect list is the roster — see `protected_targets`.
	let removing: Vec<InstallTarget> =
		deletes.iter().map(|plan| plan.target.clone()).collect();
	let protect =
		protected_targets(&copies, &source, source_removed, &removing, true);
	ensure_removals_spare(&protect, &removing, source.agent, mcp_backing_path)?;
	// The rows the refusal's remedy creates must succeed: a row finding the
	// entry gone because a sibling row took it is a success, credited below.
	let mut credits =
		RemovalCredits::from_mapped(
			&removing,
			|target| match mcp_backing_path(target) {
				Backed::At(path) => Some((target.agent, Backing::of(path))),
				Backed::Absent | Backed::Unknown => None,
			},
		);
	// Preflight is a SNAPSHOT: Copilot's project path depends on which of
	// `.mcp.json` / `.github/mcp.json` exists, so a copy can move the delete
	// target. The delete arm re-resolves once every path is settled.
	let report = crate::batch::run_staged_multi_target_mutation(
		&copies,
		&deletes,
		|plan| {
			validate_target(&plan.target)?;
			if plan.action == OperationAction::Copy {
				// Only a reconcile that REMOVES something can delete a source;
				// an add-only one is as best-effort as a plain copy.
				mcp_supported_for_target(&plan.target, &mcp, deletes_source)?;
			}
			Ok(())
		},
		|plan| {
			let outcome = match plan.action {
				// Same helper as `transfer_mcp` — the two must not disagree
				// about what "already there" means.
				OperationAction::Copy => copy_mcp_into(&plan.target, &mcp),
				OperationAction::Delete => {
					// The fresh value and shared-reader policy must be checked
					// while holding the same lock as the deletion.
					sibling_already_took_it(
						delete_reconciled_mcp(
							&source,
							&plan.target,
							&mcp,
							&protect,
							&copies,
							source_removed,
						),
						plan.target.agent,
						&mut credits,
					)
				}
			};
			let name = if plan.action == OperationAction::Copy {
				&mcp.name
			} else {
				&source.name
			};
			log_operation_outcome(
				"MCP",
				name,
				plan.action,
				&plan.target,
				&outcome,
			);
			outcome
		},
		|plan| {
			ConfigError::InvalidConfig(format!(
				"skipped delete of MCP '{}' for agent '{}': a copy to \
				 another agent failed first; nothing was removed",
				source.name,
				plan.target.agent.as_str(),
			))
		},
	)
	.map_err(|error| batch_preflight_error("MCP reconcile", error))?;
	Ok(operation_batch(report))
}

fn load_source_sub_agent(source: &ResourceLocator) -> Result<SubAgent> {
	let mut manager = build_manager(&InstallTarget {
		agent: source.agent,
		scope: source.scope,
		project_root: source.project_root.clone(),
	});
	manager.load()?;
	manager.get_sub_agent(&source.name).cloned().ok_or_else(|| {
		ConfigError::resource_not_found("sub-agent", &source.name)
	})
}

pub fn transfer_sub_agent(
	source: ResourceLocator,
	destinations: Vec<InstallTarget>,
) -> Result<OperationBatchResult> {
	let sub_agent = load_source_sub_agent(&source)?;
	let destinations = unique_targets(destinations);
	ensure_destinations(&destinations)?;
	info!(
		"transferring sub-agent '{}' to {} destination(s)",
		sub_agent.name,
		destinations.len()
	);
	let plans = copy_plans(destinations);
	let report = crate::batch::run_multi_target_mutation(
		&plans,
		|plan| {
			validate_target(&plan.target)?;
			sub_agent_supported_for_target(&plan.target, &sub_agent, false)
		},
		|plan| {
			let outcome = copy_sub_agent_into(&plan.target, &sub_agent);
			log_operation_outcome(
				"sub-agent",
				&sub_agent.name,
				plan.action,
				&plan.target,
				&outcome,
			);
			outcome
		},
	)
	.map_err(|error| batch_preflight_error("sub-agent transfer", error))?;
	Ok(operation_batch(report))
}

pub fn reconcile_sub_agent(
	source: ResourceLocator,
	added: Vec<AgentType>,
	removed: Vec<AgentType>,
	confirm: bool,
) -> Result<OperationBatchResult> {
	ensure_reconcilable(&added, &removed, confirm)?;
	let sub_agent = load_source_sub_agent(&source)?;
	info!(
		"reconciling sub-agent '{}' with {} added and {} removed agent(s)",
		sub_agent.name,
		added.len(),
		removed.len()
	);
	let source_removed = removed.contains(&source.agent);
	let (copies, deletes) = reconcile_plans(
		added,
		removed,
		source.scope,
		source.project_root.clone(),
	);
	if source_removed && !copies.is_empty() {
		ensure_sub_agent_source_fields_representable(&source, &sub_agent)?;
	}
	// Same shared-backing guard as the MCP arm.
	let removing: Vec<InstallTarget> =
		deletes.iter().map(|plan| plan.target.clone()).collect();
	let protect =
		protected_targets(&copies, &source, source_removed, &removing, true);
	ensure_removals_spare(&protect, &removing, source.agent, |target| {
		sub_agent_backing_path(target, &source.name)
	})?;
	// Same credential as the MCP arm; the backing IS the file, so preflight is
	// the only place it can be resolved.
	let mut credits = RemovalCredits::from_mapped(&removing, |target| {
		match sub_agent_backing_path(target, &source.name) {
			Backed::At(path) => Some((target.agent, Backing::of(path))),
			Backed::Absent | Backed::Unknown => None,
		}
	});
	// …and only a snapshot: two agents sharing a dir both resolve to `Absent`
	// until a copy writes the file. The delete-time re-check is the real one.
	let report = crate::batch::run_staged_multi_target_mutation(
		&copies,
		&deletes,
		|plan| {
			validate_target(&plan.target)?;
			if plan.action == OperationAction::Copy {
				sub_agent_supported_for_target(
					&plan.target,
					&sub_agent,
					source_removed,
				)?;
			}
			Ok(())
		},
		|plan| {
			let outcome = match plan.action {
				// Same helper as `transfer_sub_agent`.
				OperationAction::Copy => {
					copy_sub_agent_into(&plan.target, &sub_agent)
				}
				OperationAction::Delete => sibling_already_took_it(
					delete_reconciled_sub_agent(
						&source,
						&plan.target,
						&sub_agent,
						&protect,
						&copies,
						source_removed,
					),
					plan.target.agent,
					&mut credits,
				),
			};
			let name = if plan.action == OperationAction::Copy {
				&sub_agent.name
			} else {
				&source.name
			};
			log_operation_outcome(
				"sub-agent",
				name,
				plan.action,
				&plan.target,
				&outcome,
			);
			outcome
		},
		|plan| {
			ConfigError::InvalidConfig(format!(
				"skipped delete of sub-agent '{}' for agent '{}': a copy \
				 to another agent failed first; nothing was removed",
				source.name,
				plan.target.agent.as_str(),
			))
		},
	)
	.map_err(|error| batch_preflight_error("sub-agent reconcile", error))?;
	Ok(operation_batch(report))
}

pub fn transfer_skill(
	source: ResourceLocator,
	destinations: Vec<InstallTarget>,
) -> Result<OperationBatchResult> {
	let skill = load_source_skill(&source)?;
	let source_root = resolve_skill_root(&skill)?;
	let destinations = unique_targets(destinations);
	ensure_destinations(&destinations)?;
	info!(
		"transferring skill '{}' from '{}' to {} destination(s)",
		skill.name,
		source_root.display(),
		destinations.len()
	);
	let plans = copy_plans(destinations);
	let report = crate::batch::run_multi_target_mutation(
		&plans,
		|plan| {
			validate_target(&plan.target)?;
			skill_target_dir(&plan.target).map(|_| ())
		},
		|plan| {
			let outcome = (|| -> Result<bool> {
				let mut manager = build_manager(&plan.target);
				ensure_loaded(&mut manager)?;
				// No pre-check: `add_skill_from_path` owns the already-present
				// decision (`reconcile --add` uses the same call); a real
				// foreign occupant is refused by
				// `add_skill_from_path_universal`. Content is deliberately not
				// compared: that is the documented `add_skill_from_path`
				// contract, shared with `aghub add skill --from`.
				// See docs/history/core-transfer.md#transfer-skill-pre-check-refused-genuine-no-ops
				let added = manager.add_skill_from_path(&source_root)?;
				Ok(added.already_installed)
			})();
			log_operation_outcome(
				"skill",
				&skill.name,
				plan.action,
				&plan.target,
				&outcome,
			);
			outcome
		},
	)
	.map_err(|error| batch_preflight_error("skill transfer", error))?;
	Ok(operation_batch(report))
}

/// Every in-scope agent whose skill READ DIRS currently hold `name`, plus the
/// ids of the agents whose read dirs exist but could not be listed.
///
/// Answers "will anyone still read the Master after this reconcile?" — a
/// per-agent removal plan cannot see readers outside itself. Walks the skill
/// dirs directly, NOT `load_all_agents`: a full load fails on any MCP parse
/// error and would erase a real holder.
///
/// FAIL-CLOSED: an existing-but-unlistable read dir is a holder ("holds
/// nothing" and "cannot tell" are the same empty list); an ABSENT one is not,
/// or Master GC never happens again. Worst case is a reclaimable `orphanMaster`
/// — do not "fix" this back to fail-open.
/// See docs/history/core-transfer.md#holder-scan-reads-skill-dirs-directly
fn skill_holders(
	name: &str,
	source: &ResourceLocator,
) -> (Vec<AgentType>, Vec<&'static str>) {
	let scope = match source.scope {
		InstallScope::Global => crate::models::ResourceScope::GlobalOnly,
		InstallScope::Project => crate::models::ResourceScope::ProjectOnly,
	};
	crate::skills::removal::find_skill_holders(
		name,
		scope,
		source.project_root.as_deref(),
	)
}

/// Everything a skill reconcile decides BEFORE it writes anything: the resolved
/// source, the Master's fate, and the two plan lists.
///
/// One struct so the PREVIEW and the COMMIT answer the same question. A preview
/// that green-lights what the commit refuses is worse than no preview: it is
/// the step an agent takes to decide whether to commit.
struct ReconcileSkillPlan {
	skill: Skill,
	source_root: PathBuf,
	/// Holders this reconcile does NOT remove: the reason the Master stays, and
	/// the only thing a refused caller can actually act on.
	keepers: Vec<&'static str>,
	/// Agents whose skill dirs could not be listed at all.
	unreadable: Vec<&'static str>,
	copies: Vec<OperationPlan>,
	deletes: Vec<OperationPlan>,
	dry_run_delete_response:
		Option<crate::skills::removal::SkillRemovalResponse>,
	scope: ResourceScope,
	project_root: Option<PathBuf>,
}

/// Load the skill for reconcile: uses the caller's source if present, or
/// falls back to the first available holder in `removed` when removal-only.
/// See docs/history/core-transfer.md#missing-source-blocked-a-removal-only-reconcile
fn load_reconcile_skill(
	source: &ResourceLocator,
	added: &[AgentType],
	removed: &[AgentType],
) -> Result<Skill> {
	match load_source_skill(source) {
		Ok(skill) => Ok(skill),
		Err(ConfigError::ResourceNotFound { .. }) => {
			let (holders, _) = skill_holders(&source.name, source);
			if holders.is_empty() {
				return Err(ConfigError::resource_not_found(
					"skill",
					&source.name,
				));
			}
			if added.is_empty() {
				for &agent in removed {
					if holders.contains(&agent) {
						let fallback_source = ResourceLocator {
							agent,
							scope: source.scope,
							project_root: source.project_root.clone(),
							name: source.name.clone(),
						};
						if let Ok(skill) = load_source_skill(&fallback_source) {
							return Ok(skill);
						}
					}
				}
			}
			let scope_str = match source.scope {
				InstallScope::Global => "global",
				InstallScope::Project => "project",
			};
			let (managed_holders, disabled_holders): (Vec<_>, Vec<_>) = holders
				.iter()
				.map(|h| h.as_str())
				.partition(|h| crate::agent_settings::is_managed(h));
			// Named as holders, never as readers (see `keepers`): the user
			// needs the ids to act, and "refresh the list" cannot surface a
			// disabled agent.
			let hint = if managed_holders.is_empty() {
				format!(
					"Only agents disabled in aghub's settings still hold it: '{}'. To delete it, include them in the removal; to add it elsewhere, use one of them as the source.",
					disabled_holders.join("', '")
				)
			} else {
				format!(
					"Agents that still hold it: '{}'. Refresh the list, or use one of them as the source.",
					managed_holders.join("', '")
				)
			};
			Err(ConfigError::InvalidConfig(format!(
				"skill '{}' is no longer installed for source agent '{}' ({scope_str}); nothing was changed. {hint}",
				source.name,
				source.agent.as_str(),
			)))
		}
		Err(other) => Err(other),
	}
}

fn plan_reconcile_skill(
	source: &ResourceLocator,
	added: &[AgentType],
	removed: &[AgentType],
) -> Result<ReconcileSkillPlan> {
	let skill = load_reconcile_skill(source, added, removed)?;
	let source_root = resolve_skill_root(&skill)?;

	let (copies, deletes) = reconcile_plans(
		added.to_vec(),
		removed.to_vec(),
		source.scope,
		source.project_root.clone(),
	);
	// Output rows follow request order; the entry sorts only internally.
	// See docs/history/core-transfer.md#reconcile-delete-rows-preserve-request-order

	let write_scope =
		target_write_scope(source.scope, source.project_root.as_deref())?;
	let scope = match source.scope {
		InstallScope::Global => ResourceScope::GlobalOnly,
		InstallScope::Project => ResourceScope::ProjectOnly,
	};

	let (keepers, unreadable, dry_run_delete_response) = if !deletes.is_empty()
	{
		let req = crate::skills::removal::SkillRemovalRequest {
			target: crate::skills::removal::SkillRemovalTarget::ByName(
				skill.name.clone(),
			),
			scope: write_scope,
			agents: removed.to_vec(),
			dry_run: true,
			all_agents: false,
			prior_removed_paths: Vec::new(),
			keeps_master: !added.is_empty(),
			plugin_owner: None,
		};
		let resp = crate::skills::removal::remove_skill_batch(&req)?;
		let keepers: Vec<&'static str> = resp
			.keepers
			.iter()
			.filter(|held| {
				!resp.unreadable.contains(&held.as_str())
					&& crate::agent_settings::is_managed(held.as_str())
			})
			.map(|held| held.as_str())
			.collect();
		(keepers, resp.unreadable.clone(), Some(resp))
	} else {
		(Vec::new(), Vec::new(), None)
	};

	Ok(ReconcileSkillPlan {
		skill,
		source_root,
		keepers,
		unreadable,
		copies,
		deletes,
		dry_run_delete_response,
		scope,
		project_root: source.project_root.clone(),
	})
}

impl ReconcileSkillPlan {
	/// The read-only verdict for ONE row, run before any write in the batch and
	/// reused verbatim by [`reconcile_skill_preview`].
	fn preflight(&self, plan: &OperationPlan) -> Result<()> {
		validate_target(&plan.target)?;
		match plan.action {
			OperationAction::Copy => {
				skill_target_dir(&plan.target)?;
				Ok(())
			}
			OperationAction::Delete => self.preflight_delete(&plan.target),
		}
	}

	/// Refuse an unreachable END STATE before the first write, so copies never
	/// land ahead of a delete row that then fails. Same planner, same
	/// `exhaustive`, just earlier; runs for every delete row, with or without
	/// copies.
	fn preflight_delete(&self, target: &InstallTarget) -> Result<()> {
		let response = match &self.dry_run_delete_response {
			Some(resp) => resp,
			None => return Ok(()),
		};

		let row = response.rows.iter().find(|r| r.agent == target.agent);
		let verdict = row
			.map(|r| &r.verdict)
			.unwrap_or(&crate::skills::removal::Verdict::Absent);

		let still_read_from =
			row.map(|r| r.still_read_from.as_slice()).unwrap_or(&[]);

		if verdict.shared_master_kept() || self.a_copy_restores_it(target) {
			return Err(
				self.refuse_shared_master(target.agent, still_read_from)
			);
		}

		if let Some(r) = row {
			// Fail open on a config this row cannot load: the mutate arm fails it
			// anyway, and escalating would abort unrelated copies in the batch.
			// Only unsupported scope and planner errors pre-refuse before write.
			if !r.is_load_error {
				if let Some(ref err) = r.typed_error {
					return Err(
						crate::skills::removal::batch::clone_config_error(err),
					);
				}
				if let Some(ref err_str) = r.error {
					return Err(ConfigError::InvalidConfig(err_str.clone()));
				}
			}
		}

		Ok(())
	}

	/// Will one of THIS reconcile's own copies leave the skill in a directory
	/// `target` READS?
	///
	/// Preflight runs before any copy, so the disk cannot show it; reasoned
	/// from ONE home per half: where copies land is [`Self::copy_entry_dirs`]
	/// (`agent_link_need` — root AGENTS.md "Link decision"), where the target
	/// reads is its `get_skills_paths`. Compare DIRS, not planned paths — the
	/// shared entry need not exist yet. Do NOT derive either half from
	/// `skill_store_roots` (it includes a dir no cross-agent copy writes).
	/// See docs/history/core-transfer.md#paired-copy-undoes-the-removal
	fn a_copy_restores_it(&self, target: &InstallTarget) -> bool {
		let entry_dirs = self.copy_entry_dirs();
		if entry_dirs.is_empty() {
			return false;
		}
		create_adapter(target.agent)
			.get_skills_paths(
				target.project_root.as_deref(),
				target_resource_scope(target),
			)
			.iter()
			.map(|dir| {
				crate::skills::linker::classify::canonicalize_lenient(dir)
			})
			.any(|read_dir| {
				entry_dirs
					.iter()
					.any(|entry_dir| entry_dir.starts_with(&read_dir))
			})
	}

	/// Every directory this reconcile's copies leave a READABLE entry in,
	/// canonicalized so two spellings of one directory compare equal: the
	/// Master (`master_store_dir`, as `universal_install_prep` resolves it) and
	/// each copy target's Referrer dir.
	fn copy_entry_dirs(&self) -> Vec<PathBuf> {
		let mut dirs: Vec<PathBuf> = Vec::new();
		let mut push = |dir: &Path| {
			let canonical =
				crate::skills::linker::classify::canonicalize_lenient(dir);
			if !dirs.contains(&canonical) {
				dirs.push(canonical);
			}
		};
		for copy in &self.copies {
			let scope = target_resource_scope(&copy.target);
			let canonical_root = match scope {
				crate::models::ResourceScope::ProjectOnly => {
					copy.target.project_root.as_deref()
				}
				_ => None,
			};
			let Some(master) =
				crate::skills::linker::master_store_dir(canonical_root)
			else {
				continue;
			};
			if let crate::skills::linker::LinkNeed::NeedsLink { referrer_dir } =
				crate::skills::linker::agent_link_need(
					crate::registry::get(copy.target.agent),
					scope,
					copy.target.project_root.as_deref(),
				) {
				push(&referrer_dir);
			}
			push(&master);
		}
		dirs
	}

	/// "This agent reads the skill from the shared master, so removing it alone
	/// takes nothing away" — naming WHO keeps the master, so the user can act.
	fn refuse_shared_master(
		&self,
		agent: AgentType,
		still_read_from: &[PathBuf],
	) -> ConfigError {
		let ConfigError::UnsupportedOperation { mut message, .. } =
			ConfigError::unsupported_operation(
				"remove for this agent alone",
				"skill it reads from a location shared with other agents",
				agent.as_str(),
			)
		else {
			unreachable!("unsupported_operation builds UnsupportedOperation")
		};
		let other_keepers: Vec<&str> = self
			.keepers
			.iter()
			.copied()
			.filter(|k| *k != agent.as_str())
			.collect();
		if !other_keepers.is_empty() {
			message.push_str(&format!(
				"; the shared master is still read by '{}'",
				other_keepers.join("', '")
			));
		}
		if !self.copies.is_empty() {
			message.push_str(
				"; this reconcile also adds the skill to another agent, so the \
				 shared master stays",
			);
		}
		// Keepers = who else reads the Master; survivors = where THIS agent
		// still reads it from (e.g. a leftover compat-dir Referrer) — the
		// actionable one.
		if !still_read_from.is_empty() {
			message.push_str(&format!(
				"; it is still served to this agent from '{}'",
				still_read_from
					.iter()
					.map(|path| path.display().to_string())
					.collect::<Vec<_>>()
					.join("', '")
			));
		}
		if !self.unreadable.is_empty() {
			message.push_str(&format!(
				"; cannot verify agent '{}' (skills directory unreadable), so \
				 the shared master must stay",
				self.unreadable.join("', '")
			));
		}
		let excluding: Vec<AgentType> =
			self.deletes.iter().map(|d| d.target.agent).collect();
		let rejected_targets =
			crate::skills::removal::batch::build_rejected_targets(
				&[agent],
				&message,
				Some("shared"),
				still_read_from.first().map(|p| p.as_path()),
				self.scope,
				self.project_root.as_deref(),
				&excluding,
			);
		ConfigError::UnsupportedOperation {
			message,
			rejected_targets: Some(rejected_targets),
		}
	}
}

/// Everything a `reconcile_skill` would refuse, without touching anything.
///
/// Same per-row preflight over the same plan and the same
/// `batch_preflight_error`, so preview and commit match down to the code and
/// message. Advisory: no mutation lock (the commit re-runs it under one), so
/// inspection never serializes against real work.
/// See docs/history/core-transfer.md#reconcile-preview-approved-what-the-commit-refused
pub fn reconcile_skill_preview(
	source: &ResourceLocator,
	added: &[AgentType],
	removed: &[AgentType],
) -> Result<()> {
	ensure_disjoint(added, removed)?;
	let plan = plan_reconcile_skill(source, added, removed)?;
	let rows: Vec<OperationPlan> = plan
		.copies
		.iter()
		.chain(plan.deletes.iter())
		.cloned()
		.collect();
	let failures = crate::batch::collect_preflight_failures(&rows, |row| {
		plan.preflight(row)
	});
	if failures.is_empty() {
		return Ok(());
	}
	Err(batch_preflight_error(
		"skill reconcile",
		crate::batch::MultiTargetMutationError { failures },
	))
}

pub fn reconcile_skill(
	source: ResourceLocator,
	added: Vec<AgentType>,
	removed: Vec<AgentType>,
	confirm: bool,
) -> Result<OperationBatchResult> {
	ensure_reconcilable(&added, &removed, confirm)?;
	// ONE guard for the whole reconcile, taken before the holder scan and
	// preflight dry-runs — the state reads that decide the mutation. Reentrant,
	// so inner `guard_and_reload`s are free. It serializes aghub against aghub
	// only — `remove_skill_planned`'s executing refusal stays as the backstop.
	// See crates/core/AGENTS.md "Mutation attribution".
	let _mutation_guard = crate::skills::lock::mutation_guard(
		"reconcile skill",
		match source.scope {
			InstallScope::Global => crate::models::ResourceScope::GlobalOnly,
			InstallScope::Project => crate::models::ResourceScope::ProjectOnly,
		},
		source.project_root.as_deref(),
	)
	.map_err(ConfigError::Io)?;
	let plan = plan_reconcile_skill(&source, &added, &removed)?;
	info!(
		"reconciling skill '{}' with {} added and {} removed agent(s)",
		plan.skill.name,
		added.len(),
		removed.len()
	);
	// Does this reconcile take the skill AWAY from its source? Only then must a
	// copy prove the content actually landed — see the Copy arm below.
	let deletes_source = removed.contains(&source.agent);
	// No copy target (nor the source, unless removed) may share a skills
	// DIRECTORY with a removal target. Distinct from `plan.preflight`'s
	// end-state check; both run.
	let removing: Vec<InstallTarget> =
		plan.deletes.iter().map(|row| row.target.clone()).collect();
	let protect = protected_targets(
		&plan.copies,
		&source,
		deletes_source,
		&removing,
		false,
	);
	ensure_removals_spare(
		&protect,
		&removing,
		source.agent,
		skill_backing_dir,
	)?;
	let mut delete_batch_result: Option<
		Result<crate::skills::removal::SkillRemovalResponse>,
	> = None;
	let report = crate::batch::run_staged_multi_target_mutation(
		&plan.copies,
		&plan.deletes,
		|row| plan.preflight(row),
		|row| {
			let outcome = match row.action {
				OperationAction::Copy => (|| -> Result<bool> {
					let target_scope = match row.target.scope {
						InstallScope::Global => {
							crate::models::ResourceScope::GlobalOnly
						}
						InstallScope::Project => {
							crate::models::ResourceScope::ProjectOnly
						}
					};
					// ONE guard across check → write → rollback; the manager's
					// own guard ends when `add_skill_from_path` returns.
					// See crates/core/AGENTS.md "Mutation attribution".
					let _copy_guard = crate::skills::lock::mutation_guard(
						"reconcile skill copy",
						target_scope,
						row.target.project_root.as_deref(),
					)
					.map_err(ConfigError::Io)?;
					let mut manager = build_manager(&row.target);
					ensure_loaded(&mut manager)?;
					// `add_skill_from_path` owns the already-present decision;
					// `transfer_skill` defers to the same call.
					let added =
						manager.add_skill_from_path(&plan.source_root)?;

					// When this reconcile also REMOVES, the copy must prove the
					// source content landed. `wrote_master` is the only outcome
					// that proves it: a pre-existing Master is preserved, not
					// overwritten, so a target can be linked to (or already
					// hold) a same-named Master with DIFFERENT content. Paired
					// with the delete, the source content would be gone while
					// nothing looks wrong. A plain `transfer`/`--add` keeps
					// preserve-the-Master behaviour.
					if deletes_source && !added.wrote_master {
						let landed =
							crate::skills::skill_source_root(&plan.source_root);
						let why = match prove_content_landed(
							&landed,
							&row.target,
							&plan.skill.name,
						) {
							ContentProof::Landed => None,
							ContentProof::Differs => Some(String::from(
								"the target already holds a same-named skill \
								 (an existing .aghub master) whose \
								 content differs from the source, and aghub \
								 preserves an existing master rather than \
								 overwriting it — so the copy did not carry \
								 the source content over. Reconcile the master \
								 first, or drop the --remove.",
							)),
							// Not folded into "differs" — see [`ContentProof`].
							ContentProof::Unprovable(reason) => Some(format!(
								"aghub cannot PROVE the target now holds the \
								 source content — {reason}. It will not remove \
								 content it cannot account for. Drop the \
								 --remove, or copy the folder yourself and \
								 verify it."
							)),
						};
						if let Some(why) = why {
							// Undo THIS call's own work before refusing, from
							// the materializer's receipt
							// (`created_referrer_dirs`), or the failed row
							// leaves the target holding content nobody asked to
							// copy. The master is not touched: we did not write
							// it. Runs under `_copy_guard`; see
							// crates/core/AGENTS.md "Mutation attribution".
							crate::skills::rename::rollback_materialized_install(
								&plan.skill.name,
								target_scope,
								row.target.project_root.as_deref(),
								&added.created_referrer_dirs,
								false,
							);
							return Err(ConfigError::InvalidConfig(format!(
								"refusing to remove '{}' from the source: \
								 {why}",
								plan.skill.name
							)));
						}
					}
					Ok(added.already_installed)
				})(),
				// Use the planned-removal seam — never blind-delete a shared
				// universal master discovered through an agent's read dirs.
				OperationAction::Delete => (|| -> Result<bool> {
					// Re-check now that every copy has run: a copy can create
					// the very directory this target resolves through.
					ensure_removals_spare(
						&protect,
						std::slice::from_ref(&row.target),
						source.agent,
						skill_backing_dir,
					)?;
					if delete_batch_result.is_none() {
						let delete_agents: Vec<AgentType> = plan
							.deletes
							.iter()
							.filter(|r| {
								ensure_removals_spare(
									&protect,
									std::slice::from_ref(&r.target),
									source.agent,
									skill_backing_dir,
								)
								.is_ok()
							})
							.map(|r| r.target.agent)
							.collect();
						let scope = target_write_scope(
							row.target.scope,
							row.target.project_root.as_deref(),
						)?;
						let req = crate::skills::removal::SkillRemovalRequest {
							target: crate::skills::removal::SkillRemovalTarget::ByName(
								plan.skill.name.clone(),
							),
							scope,
							agents: delete_agents,
							dry_run: false,
							all_agents: false,
							prior_removed_paths: Vec::new(),
							keeps_master: !added.is_empty(),
							plugin_owner: None,
						};
						delete_batch_result = Some(
							crate::skills::removal::remove_skill_batch(&req),
						);
					}
					let resp = match delete_batch_result.as_ref().unwrap() {
						Ok(resp) => resp,
						Err(err) => return Err(
							crate::skills::removal::batch::clone_config_error(
								err,
							),
						),
					};
					if let Some(r) =
						resp.rows.iter().find(|r| r.agent == row.target.agent)
					{
						if let Some(ref err) = r.typed_error {
							return Err(
								crate::skills::removal::batch::clone_config_error(
									err,
								),
							);
						}
						if let Some(ref err) = r.error {
							return Err(ConfigError::InvalidConfig(
								err.clone(),
							));
						}
						match &r.verdict {
							crate::skills::removal::Verdict::Removed => {
								Ok(false)
							}
							crate::skills::removal::Verdict::Absent => {
								Err(ConfigError::resource_not_found(
									"skill",
									&plan.skill.name,
								))
							}
							crate::skills::removal::Verdict::Kept {
								..
							} => Ok(false),
							crate::skills::removal::Verdict::Refused {
								reason,
								..
							} => {
								if !r.still_read_from.is_empty() {
									Err(plan.refuse_shared_master(
										row.target.agent,
										&r.still_read_from,
									))
								} else {
									Err(ConfigError::unsupported_operation(
										"remove for this agent alone",
										reason,
										row.target.agent.as_str(),
									))
								}
							}
							_ => Ok(false),
						}
					} else {
						Err(ConfigError::resource_not_found(
							"skill",
							&plan.skill.name,
						))
					}
				})(),
			};
			log_operation_outcome(
				"skill",
				&plan.skill.name,
				row.action,
				&row.target,
				&outcome,
			);
			outcome
		},
		|row| {
			ConfigError::InvalidConfig(format!(
				"skipped delete of skill '{}' for agent '{}': a copy to \
				 another agent failed first; nothing was removed",
				plan.skill.name,
				row.target.agent.as_str(),
			))
		},
	)
	.map_err(|error| batch_preflight_error("skill reconcile", error))?;
	let mut batch_res = operation_batch(report);
	if let Some(Ok(ref resp)) = delete_batch_result {
		let (still_read_by, still_read_by_managed, still_read_by_unmanaged) =
			resp.holders_view().to_options();
		for r in &mut batch_res.results {
			if r.action == OperationAction::Delete {
				if let Some(row) =
					resp.rows.iter().find(|row| row.agent == r.target.agent)
				{
					r.outcome = Some(row.outcome);
				}
				r.still_read_by = still_read_by.clone();
				r.still_read_by_managed = still_read_by_managed.clone();
				r.still_read_by_unmanaged = still_read_by_unmanaged.clone();
			}
		}
	}
	Ok(batch_res)
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::models::McpTransport;
	// The lib binary's ONE env mutex. A module-local copy here would not
	// serialize against `GlobalLockGuard`'s `XDG_STATE_HOME` swap, which is UB
	// (and made `manager::skill`'s prune tests resolve the wrong lock file).
	use crate::skills::prune::test_lock::env_lock;
	#[cfg(unix)]
	use crate::testing::master_with_claude_referrer;
	use tempfile::tempdir;

	#[test]
	fn target_write_scope_rejects_rootless_project_target() {
		let err = target_write_scope(InstallScope::Project, None)
			.expect_err("rootless project must be rejected");
		match err {
			ConfigError::InvalidConfig(msg) => {
				assert_eq!(msg, "project_root is required for project targets");
			}
			other => panic!("expected InvalidConfig, got {other:?}"),
		}

		assert_eq!(
			target_write_scope(InstallScope::Global, None).unwrap(),
			WriteScope::Global
		);

		let temp = tempdir().unwrap();
		assert_eq!(
			target_write_scope(InstallScope::Project, Some(temp.path()))
				.unwrap(),
			WriteScope::project(temp.path())
		);
	}

	/// A reconcile deletes a sub-agent's source only while every copy still
	/// holds what was copied; a copy edited or removed in between keeps it.
	#[test]
	fn sub_agent_source_delete_requires_every_copy_to_hold() {
		let project = tempdir().unwrap();
		let root = project.path();
		let mut agent = SubAgent::new("mover");
		agent.instruction = Some("body".into());
		let copies = vec![OperationPlan {
			target: InstallTarget {
				agent: AgentType::OpenCode,
				scope: InstallScope::Project,
				project_root: Some(root.to_path_buf()),
			},
			action: OperationAction::Copy,
		}];
		assert!(ensure_sub_agent_copies_hold(&copies, &agent).is_err());
		copy_sub_agent_into(&copies[0].target, &agent).unwrap();
		ensure_sub_agent_copies_hold(&copies, &agent).unwrap();
		let mut edited = build_manager(&copies[0].target);
		ensure_loaded(&mut edited).unwrap();
		edited
			.update_sub_agent(
				"mover",
				crate::manager::sub_agent::SubAgentPatch {
					instruction: Some("edited".into()),
					..Default::default()
				},
			)
			.unwrap();
		assert!(ensure_sub_agent_copies_hold(&copies, &agent).is_err());
	}

	#[test]
	fn sub_agent_copy_hold_rejects_frontmatter_only_change() {
		let project = tempdir().unwrap();
		let root = project.path();
		let mut agent = SubAgent::new("mover");
		agent.instruction = Some("body".into());
		let mut extra = serde_yaml::Mapping::new();
		extra.insert(
			serde_yaml::Value::String("tools".into()),
			serde_yaml::Value::String("Read".into()),
		);
		agent.extra_frontmatter = extra;

		let copies = vec![OperationPlan {
			target: InstallTarget {
				agent: AgentType::OpenCode,
				scope: InstallScope::Project,
				project_root: Some(root.to_path_buf()),
			},
			action: OperationAction::Copy,
		}];

		copy_sub_agent_into(&copies[0].target, &agent).unwrap();

		let mut target_manager = build_manager(&copies[0].target);
		ensure_loaded(&mut target_manager).unwrap();
		let copied = target_manager
			.get_sub_agent("mover")
			.expect("copied sub-agent should exist in target");
		assert_eq!(copied.extra_frontmatter, agent.extra_frontmatter);
		ensure_sub_agent_copies_hold(&copies, &agent).unwrap();

		let copy_path = copied
			.source_path
			.as_ref()
			.expect("sub-agent should have source_path");
		let content = fs::read_to_string(copy_path).unwrap();
		let updated = content.replace("tools: Read", "tools: Write");
		assert_ne!(
			content, updated,
			"disk content should have contained 'tools: Read'"
		);
		fs::write(copy_path, updated).unwrap();

		let err = ensure_sub_agent_copies_hold(&copies, &agent)
			.expect_err("should reject copy with changed frontmatter");
		assert!(
			err.to_string().contains("changed in target"),
			"expected 'changed in target' in error message, got: {err}"
		);
	}

	#[cfg(unix)]
	struct EnvVarGuard(&'static str, Option<std::ffi::OsString>);

	#[cfg(unix)]
	impl EnvVarGuard {
		fn set(key: &'static str, value: &Path) -> Self {
			let previous = std::env::var_os(key);
			std::env::set_var(key, value);
			Self(key, previous)
		}
	}

	#[cfg(unix)]
	impl Drop for EnvVarGuard {
		fn drop(&mut self) {
			match self.1.take() {
				Some(value) => std::env::set_var(self.0, value),
				None => std::env::remove_var(self.0),
			}
		}
	}

	#[test]
	fn transfer_mcp_copies_to_other_agent_project() {
		let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
		let temp = tempdir().unwrap();
		let source_root = temp.path().join("source");
		let dest_root = temp.path().join("dest");
		fs::create_dir_all(&source_root).unwrap();
		fs::create_dir_all(&dest_root).unwrap();

		let mut source_manager = ConfigManager::new(
			create_adapter(AgentType::Claude),
			false,
			Some(&source_root),
		);
		source_manager.load().unwrap();
		source_manager
			.add_mcp(McpServer::new(
				"filesystem",
				McpTransport::stdio("npx", vec!["mcp-filesystem".to_string()]),
			))
			.unwrap();

		let result = transfer_mcp(
			ResourceLocator {
				agent: AgentType::Claude,
				scope: InstallScope::Project,
				project_root: Some(source_root.clone()),
				name: "filesystem".to_string(),
			},
			vec![InstallTarget {
				agent: AgentType::Cursor,
				scope: InstallScope::Project,
				project_root: Some(dest_root.clone()),
			}],
		)
		.unwrap();

		assert_eq!(result.success_count(), 1);

		let mut dest_manager = ConfigManager::new(
			create_adapter(AgentType::Cursor),
			false,
			Some(&dest_root),
		);
		dest_manager.load().unwrap();
		assert!(dest_manager.get_mcp("filesystem").is_some());
	}

	#[test]
	fn transfer_mcp_empty_destinations_is_rejected() {
		// Finding #4: a transfer with no destinations is a no-op the caller
		// almost certainly did not intend. It must be an actionable error, not
		// a silent `Ok` with an empty result set (which exits 0).
		let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
		let temp = tempdir().unwrap();
		let source_root = temp.path().join("source");
		fs::create_dir_all(&source_root).unwrap();

		let mut source_manager = ConfigManager::new(
			create_adapter(AgentType::Claude),
			false,
			Some(&source_root),
		);
		source_manager.load().unwrap();
		source_manager
			.add_mcp(McpServer::new(
				"filesystem",
				McpTransport::stdio("npx", vec!["mcp-filesystem".to_string()]),
			))
			.unwrap();

		let result = transfer_mcp(
			ResourceLocator {
				agent: AgentType::Claude,
				scope: InstallScope::Project,
				project_root: Some(source_root.clone()),
				name: "filesystem".to_string(),
			},
			vec![], // no destinations
		);

		assert!(
			result.is_err(),
			"empty destination list must be a hard error, not Ok([])"
		);
	}

	#[test]
	fn transfer_mcp_preflight_prevents_partial_writes() {
		let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
		let temporary = tempdir().unwrap();
		let source_root = temporary.path().join("source");
		let valid_root = temporary.path().join("valid-target");
		let unsupported_root = temporary.path().join("unsupported-target");
		fs::create_dir_all(&source_root).unwrap();
		fs::create_dir_all(&valid_root).unwrap();
		fs::create_dir_all(&unsupported_root).unwrap();

		let mut source_manager = ConfigManager::new(
			create_adapter(AgentType::Claude),
			false,
			Some(&source_root),
		);
		source_manager.load().unwrap();
		source_manager
			.add_mcp(McpServer::new(
				"filesystem",
				McpTransport::stdio("npx", vec!["mcp-filesystem".to_string()]),
			))
			.unwrap();

		let result = transfer_mcp(
			ResourceLocator {
				agent: AgentType::Claude,
				scope: InstallScope::Project,
				project_root: Some(source_root),
				name: "filesystem".to_string(),
			},
			vec![
				InstallTarget {
					agent: AgentType::Cursor,
					scope: InstallScope::Project,
					project_root: Some(valid_root.clone()),
				},
				InstallTarget {
					agent: AgentType::AugmentCode,
					scope: InstallScope::Project,
					project_root: Some(unsupported_root),
				},
			],
		);

		assert!(
			result.is_err(),
			"predictable target failure rejects the batch"
		);
		let mut valid_manager = ConfigManager::new(
			create_adapter(AgentType::Cursor),
			false,
			Some(&valid_root),
		);
		valid_manager.load().unwrap();
		assert!(
			valid_manager.get_mcp("filesystem").is_none(),
			"no target may be written before every target passes preflight",
		);
	}

	/// Reconcile DELETES the source once the copies land, so a copy that would
	/// silently shed a field has to fail preflight. Codex holds
	/// `tool_timeout_sec`; the JSON-map dialects have no per-server timeout key
	/// at all, so this copy is lossy — and without the check the only surviving
	/// copy would be the one missing the timeout.
	#[test]
	fn reconcile_mcp_refuses_a_lossy_copy_and_keeps_the_source() {
		let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
		let temp = tempdir().unwrap();
		let root = temp.path().join("project");
		fs::create_dir_all(&root).unwrap();

		let mut source = ConfigManager::new(
			create_adapter(AgentType::Codex),
			false,
			Some(&root),
		);
		source.load().unwrap();
		source
			.add_mcp(McpServer::new(
				"filesystem",
				McpTransport::Stdio {
					command: "npx".to_string(),
					args: vec!["mcp-filesystem".to_string()],
					env: None,
					timeout: Some(30),
				},
			))
			.unwrap();

		let error = reconcile_mcp(
			ResourceLocator {
				agent: AgentType::Codex,
				scope: InstallScope::Project,
				project_root: Some(root.clone()),
				name: "filesystem".to_string(),
			},
			vec![AgentType::Cursor], // added — cannot hold the timeout
			vec![AgentType::Codex],  // removed
			true,
		)
		.unwrap_err();
		assert!(
			error.to_string().contains("without losing fields"),
			"got: {error}"
		);

		// The source must still be on disk, with its timeout intact.
		let mut source = ConfigManager::new(
			create_adapter(AgentType::Codex),
			false,
			Some(&root),
		);
		source.load().unwrap();
		let kept = source.get_mcp("filesystem").expect(
			"reconcile must not delete a source whose copy was refused",
		);
		assert!(
			matches!(
				kept.transport,
				McpTransport::Stdio {
					timeout: Some(30),
					..
				}
			),
			"the source kept the field the copy would have dropped: {:?}",
			kept.transport
		);

		// A plain copy is best-effort and still allowed — it deletes nothing.
		// Assert the ROW succeeded and the server is on Cursor's disk:
		// `transfer_mcp` returns Ok even when an execution row failed.
		let copied = transfer_mcp(
			ResourceLocator {
				agent: AgentType::Codex,
				scope: InstallScope::Project,
				project_root: Some(root.clone()),
				name: "filesystem".to_string(),
			},
			vec![InstallTarget {
				agent: AgentType::Cursor,
				scope: InstallScope::Project,
				project_root: Some(root.clone()),
			}],
		)
		.unwrap();
		assert_eq!(copied.results.len(), 1);
		assert!(
			copied.results[0].error.is_none(),
			"best-effort copy must actually run: {:?}",
			copied.results[0].error
		);
		let mut cursor = ConfigManager::new(
			create_adapter(AgentType::Cursor),
			false,
			Some(&root),
		);
		cursor.load().unwrap();
		assert!(
			cursor.get_mcp("filesystem").is_some(),
			"the best-effort copy must land on disk"
		);
	}

	#[test]
	fn reconcile_mcp_refuses_unmodelled_opencode_options_before_writing() {
		let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
		let temp = tempdir().unwrap();
		let root = temp.path();
		let source = root.join("opencode.json");
		let target = root.join(".cursor/mcp.json");
		let original = r#"{"mcp":{"remote-srv":{"type":"remote","url":"https://example.com/mcp","oauth":{"clientId":"client-id"}}}}"#;
		fs::write(&source, original).unwrap();

		let error = reconcile_mcp(
			ResourceLocator {
				agent: AgentType::OpenCode,
				scope: InstallScope::Project,
				project_root: Some(root.to_path_buf()),
				name: "remote-srv".to_string(),
			},
			vec![AgentType::Cursor],
			vec![AgentType::OpenCode],
			true,
		).expect_err("native OAuth options cannot be copied through the normalized model");

		assert!(error.to_string().contains("unmanaged"), "got: {error}");
		assert_eq!(fs::read_to_string(&source).unwrap(), original);
		assert!(
			!target.exists(),
			"preflight refusal must precede destination writes"
		);

		fs::write(&source, r#"{"mcp":{"remote-srv":{"type":"remote","url":"https://example.com/mcp"}}}"#).unwrap();
		let result = reconcile_mcp(
			ResourceLocator {
				agent: AgentType::OpenCode,
				scope: InstallScope::Project,
				project_root: Some(root.to_path_buf()),
				name: "remote-srv".to_string(),
			},
			vec![AgentType::Cursor],
			vec![AgentType::OpenCode],
			true,
		)
		.unwrap();
		assert_eq!(result.success_count(), 2);
		assert!(target.exists());
	}

	#[test]
	fn reconcile_mcp_refuses_unmodelled_json_map_options_before_writing() {
		let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
		let temp = tempdir().unwrap();
		let root = temp.path();
		let source = root.join(".cursor/mcp.json");
		let target = root.join("opencode.json");
		fs::create_dir_all(source.parent().unwrap()).unwrap();
		let original = r#"{"mcpServers":{"remote-srv":{"type":"http","url":"https://example.com/mcp","oauth":{"clientId":"client-id"}}}}"#;
		fs::write(&source, original).unwrap();

		let error = reconcile_mcp(
			ResourceLocator {
				agent: AgentType::Cursor,
				scope: InstallScope::Project,
				project_root: Some(root.to_path_buf()),
				name: "remote-srv".to_string(),
			},
			vec![AgentType::OpenCode],
			vec![AgentType::Cursor],
			true,
		).expect_err("native JSON-map options cannot be copied through the normalized model");

		assert!(error.to_string().contains("unmanaged"), "got: {error}");
		assert_eq!(fs::read_to_string(&source).unwrap(), original);
		assert!(!target.exists());
	}

	#[test]
	fn reconcile_mcp_refuses_unmodelled_toml_options_before_writing() {
		let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
		let temp = tempdir().unwrap();
		let root = temp.path();
		let source = root.join(".codex/config.toml");
		let target = root.join(".cursor/mcp.json");
		fs::create_dir_all(source.parent().unwrap()).unwrap();
		let original = "[mcp_servers.remote-srv]\nurl = \"https://example.com/mcp\"\nbearer_token_env_var = \"TOKEN\"\n";
		fs::write(&source, original).unwrap();

		let error = reconcile_mcp(
			ResourceLocator {
				agent: AgentType::Codex,
				scope: InstallScope::Project,
				project_root: Some(root.to_path_buf()),
				name: "remote-srv".to_string(),
			},
			vec![AgentType::Cursor],
			vec![AgentType::Codex],
			true,
		)
		.expect_err("Codex auth field cannot survive cross-agent reconcile");

		assert!(error.to_string().contains("unmanaged"), "got: {error}");
		assert_eq!(fs::read_to_string(&source).unwrap(), original);
		assert!(!target.exists());
	}

	// Cursor, NOT Claude: Claude and Copilot both resolve a project MCP to
	// `<root>/.mcp.json`, so removing from Claude here is refused by
	// `protected_targets`' roster list — copilot would lose the server too.
	// Cursor owns `<root>/.cursor/mcp.json` alone, which is what this test
	// needs to say anything about deletion at all.
	#[test]
	fn reconcile_mcp_preserves_a_source_or_copy_changed_after_copy() {
		let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
		let temp = tempdir().unwrap();
		let root = temp.path();
		let source = ResourceLocator {
			agent: AgentType::Cursor,
			scope: InstallScope::Project,
			project_root: Some(root.to_path_buf()),
			name: "filesystem".to_string(),
		};
		let target = InstallTarget {
			agent: AgentType::OpenCode,
			scope: InstallScope::Project,
			project_root: Some(root.to_path_buf()),
		};
		let copies = vec![OperationPlan {
			target: target.clone(),
			action: OperationAction::Copy,
		}];
		let mut source_manager = build_manager(&InstallTarget {
			agent: source.agent,
			scope: source.scope,
			project_root: source.project_root.clone(),
		});
		ensure_loaded(&mut source_manager).unwrap();
		source_manager
			.add_mcp(McpServer::new(
				"filesystem",
				McpTransport::stdio("old-command", vec![]),
			))
			.unwrap();
		let original = load_source_mcp(&source).unwrap();
		copy_mcp_into(&target, &original).unwrap();

		// A second manager changes the source between the copy and delete.
		source_manager
			.update_mcp(
				"filesystem",
				McpServer::new(
					"filesystem",
					McpTransport::stdio("new-command", vec![]),
				),
			)
			.unwrap();
		let error = delete_reconciled_mcp(
			&source,
			&InstallTarget {
				agent: source.agent,
				scope: source.scope,
				project_root: source.project_root.clone(),
			},
			&original,
			&[],
			&copies,
			true,
		)
		.expect_err("a stale reconcile must not delete a newly edited source");
		assert!(error.to_string().contains("changed in its source"));
		let latest = load_source_mcp(&source).unwrap();
		assert!(matches!(
			latest.transport,
			McpTransport::Stdio { ref command, .. } if command == "new-command"
		));

		// The copied target can also be changed after its copy succeeds.
		let error = delete_reconciled_mcp(
			&source,
			&InstallTarget {
				agent: source.agent,
				scope: source.scope,
				project_root: source.project_root.clone(),
			},
			&latest,
			&[],
			&copies,
			true,
		)
		.expect_err(
			"the source must survive if its destination no longer matches",
		);
		assert!(error.to_string().contains("changed in target"));
		assert!(load_source_mcp(&source).is_ok());
	}

	#[test]
	fn reconcile_mcp_shared_sibling_cannot_delete_a_changed_source_first() {
		let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
		let temp = tempdir().unwrap();
		let root = temp.path();
		let source = ResourceLocator {
			agent: AgentType::Claude,
			scope: InstallScope::Project,
			project_root: Some(root.to_path_buf()),
			name: "filesystem".to_string(),
		};
		let sibling = InstallTarget {
			agent: AgentType::Copilot,
			scope: InstallScope::Project,
			project_root: Some(root.to_path_buf()),
		};
		let target = InstallTarget {
			agent: AgentType::Cursor,
			scope: InstallScope::Project,
			project_root: Some(root.to_path_buf()),
		};
		let mut manager = build_manager(&InstallTarget {
			agent: source.agent,
			scope: source.scope,
			project_root: source.project_root.clone(),
		});
		ensure_loaded(&mut manager).unwrap();
		manager
			.add_mcp(McpServer::new(
				"filesystem",
				McpTransport::stdio("old-command", vec![]),
			))
			.unwrap();
		let original = load_source_mcp(&source).unwrap();
		copy_mcp_into(&target, &original).unwrap();
		manager
			.update_mcp(
				"filesystem",
				McpServer::new(
					"filesystem",
					McpTransport::stdio("new-command", vec![]),
				),
			)
			.unwrap();
		let error = delete_reconciled_mcp(
			&source,
			&sibling,
			&original,
			&[],
			&[OperationPlan {
				target,
				action: OperationAction::Copy,
			}],
			true,
		)
		.expect_err("shared sibling must not bypass source compare-and-delete");
		assert!(error.to_string().contains("changed in its source"));
		let latest = load_source_mcp(&source).unwrap();
		assert!(matches!(
			latest.transport,
			McpTransport::Stdio { ref command, .. } if command == "new-command"
		));
	}

	#[test]
	fn reconcile_mcp_rechecks_native_source_fields_before_removal() {
		let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
		let temp = tempdir().unwrap();
		let root = temp.path();
		let source = ResourceLocator {
			agent: AgentType::Cursor,
			scope: InstallScope::Project,
			project_root: Some(root.to_path_buf()),
			name: "remote-srv".to_string(),
		};
		let target = InstallTarget {
			agent: AgentType::OpenCode,
			scope: InstallScope::Project,
			project_root: Some(root.to_path_buf()),
		};
		let source_path = root.join(".cursor/mcp.json");
		fs::create_dir_all(source_path.parent().unwrap()).unwrap();
		fs::write(
			&source_path,
			r#"{"mcpServers":{"remote-srv":{"type":"http","url":"https://example.com/mcp"}}}"#,
		)
		.unwrap();
		let expected = load_source_mcp(&source).unwrap();
		copy_mcp_into(&target, &expected).unwrap();
		let changed = r#"{"mcpServers":{"remote-srv":{"type":"http","url":"https://example.com/mcp","oauth":{"clientId":"new-secret"}}}}"#;
		fs::write(&source_path, changed).unwrap();
		let error = delete_reconciled_mcp(
			&source,
			&InstallTarget {
				agent: source.agent,
				scope: source.scope,
				project_root: source.project_root.clone(),
			},
			&expected,
			&[],
			&[OperationPlan {
				target,
				action: OperationAction::Copy,
			}],
			true,
		)
		.expect_err("native fields added after preflight must stay on disk");
		assert!(error.to_string().contains("unmanaged"));
		assert_eq!(fs::read_to_string(&source_path).unwrap(), changed);
	}

	#[test]
	fn reconcile_mcp_deletes_when_removed() {
		let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
		let temp = tempdir().unwrap();
		let root = temp.path().join("project");
		fs::create_dir_all(&root).unwrap();

		let mut manager = ConfigManager::new(
			create_adapter(AgentType::Cursor),
			false,
			Some(&root),
		);
		manager.load().unwrap();
		manager
			.add_mcp(McpServer::new(
				"filesystem",
				McpTransport::stdio("npx", vec!["mcp-filesystem".to_string()]),
			))
			.unwrap();

		let result = reconcile_mcp(
			ResourceLocator {
				agent: AgentType::Cursor,
				scope: InstallScope::Project,
				project_root: Some(root.clone()),
				name: "filesystem".to_string(),
			},
			vec![],                  // added
			vec![AgentType::Cursor], // removed
			true,                    // confirm
		)
		.unwrap();

		assert_eq!(result.results.len(), 1);
		assert_eq!(result.results[0].action, OperationAction::Delete);

		let mut manager = ConfigManager::new(
			create_adapter(AgentType::Cursor),
			false,
			Some(&root),
		);
		manager.load().unwrap();
		assert!(manager.get_mcp("filesystem").is_none());
	}

	// Regression: a Copy that fails at RUNTIME (after preflight already
	// passed) must not let its paired Delete run. Claude supports project-scope
	// stdio MCPs (so `mcp_supported_for_target` preflight is clean), but
	// Claude's OWN mcp config already holds an unrelated MCP named
	// "filesystem" — `add_mcp`'s duplicate-name guard rejects the copy only
	// once it actually runs. A flat Copy-then-Delete plan that attempts
	// every row regardless would still run the Cursor delete: the MCP would vanish from Cursor without ever
	// landing on Claude — gone from every agent. This test fails on that
	// regression because `cursor_manager.get_mcp("filesystem")` would be
	// `None` afterward.
	//
	// The source is Cursor and the failing copy target is Claude — the reverse
	// of the obvious pairing, because Claude shares `<root>/.mcp.json` with
	// Copilot and `protected_targets`' roster list refuses a removal from it
	// before any row runs. Nothing about the staging this pins depends on which
	// agent is which.
	#[test]
	fn reconcile_mcp_keeps_source_when_a_copy_fails_at_runtime() {
		let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
		let temp = tempdir().unwrap();
		let root = temp.path().join("project");
		fs::create_dir_all(&root).unwrap();

		let mut cursor_manager = ConfigManager::new(
			create_adapter(AgentType::Cursor),
			false,
			Some(&root),
		);
		cursor_manager.load().unwrap();
		cursor_manager
			.add_mcp(McpServer::new(
				"filesystem",
				McpTransport::stdio("npx", vec!["mcp-filesystem".to_string()]),
			))
			.unwrap();

		// Pre-populate Claude's OWN project config with an unrelated MCP of
		// the same name so its copy fails at write time, not at preflight.
		let mut claude_manager = ConfigManager::new(
			create_adapter(AgentType::Claude),
			false,
			Some(&root),
		);
		claude_manager.load().unwrap();
		claude_manager
			.add_mcp(McpServer::new(
				"filesystem",
				McpTransport::stdio("echo", vec!["conflict".to_string()]),
			))
			.unwrap();

		let result = reconcile_mcp(
			ResourceLocator {
				agent: AgentType::Cursor,
				scope: InstallScope::Project,
				project_root: Some(root.clone()),
				name: "filesystem".to_string(),
			},
			vec![AgentType::Claude], // added: fails at runtime
			vec![AgentType::Cursor], // removed: must be skipped
			true,                    // confirm
		)
		.unwrap();

		assert_eq!(result.results.len(), 2);
		let copy_row = result
			.results
			.iter()
			.find(|r| r.action == OperationAction::Copy)
			.expect("a copy row must be present");
		assert!(!copy_row.success, "the Claude copy must fail");

		let delete_row = result
			.results
			.iter()
			.find(|r| r.action == OperationAction::Delete)
			.expect("a delete row must be present");
		assert!(
			!delete_row.success,
			"the Cursor delete must be skipped, not attempted"
		);
		assert!(
			delete_row
				.error
				.as_ref()
				.is_some_and(|e| e.contains("skipped")),
			"the delete row must read as skipped, not as an attempted \
			 failure: {:?}",
			delete_row.error,
		);

		// The critical assertion: the source MCP must survive. Before the
		// fix this was deleted even though its only copy destination failed.
		let mut cursor_manager = ConfigManager::new(
			create_adapter(AgentType::Cursor),
			false,
			Some(&root),
		);
		cursor_manager.load().unwrap();
		assert!(
			cursor_manager.get_mcp("filesystem").is_some(),
			"source MCP must survive a reconcile whose only copy failed"
		);
	}

	#[test]
	fn transfer_skill_materializes_master_and_referrer() {
		let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
		let temp = tempdir().unwrap();
		let source_root = temp.path().join("source");
		let dest_root = temp.path().join("dest");
		fs::create_dir_all(&source_root).unwrap();
		fs::create_dir_all(&dest_root).unwrap();

		let mut source_manager = ConfigManager::new(
			create_adapter(AgentType::Claude),
			false,
			Some(&source_root),
		);
		source_manager.load().unwrap();
		let mut skill = Skill::new("repo-helper");
		skill.description = Some("Copies files".to_string());
		source_manager.add_skill(skill).unwrap();
		let asset_dir = source_root.join(".claude/skills/repo-helper/assets");
		fs::create_dir_all(&asset_dir).unwrap();
		fs::write(asset_dir.join("notes.txt"), "hello").unwrap();

		let result = transfer_skill(
			ResourceLocator {
				agent: AgentType::Claude,
				scope: InstallScope::Project,
				project_root: Some(source_root.clone()),
				name: "repo-helper".to_string(),
			},
			vec![InstallTarget {
				agent: AgentType::Windsurf,
				scope: InstallScope::Project,
				project_root: Some(dest_root.clone()),
			}],
		)
		.unwrap();

		assert_eq!(result.success_count(), 1);
		let master = dest_root.join(".aghub/repo-helper");
		let referrer = dest_root.join(".windsurf/skills/repo-helper");
		assert!(master.join("assets/notes.txt").exists());
		assert!(
			crate::skills::linker::Linker::is_link(&referrer),
			"skill transfer must use ConfigManager's Master + Referrer layout",
		);
	}

	#[test]
	fn skill_root_unchecked_returns_nonexistent_dir_as_is() {
		let temp = tempdir().unwrap();
		let missing = temp.path().join(".aghub/foo");
		let mut skill = Skill::new("foo");
		skill.canonical_path = Some(missing.to_string_lossy().to_string());

		assert_eq!(skill_root_unchecked(&skill), Some(missing));
	}

	#[test]
	fn project_private_grants_can_be_removed_independently() {
		let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
		for agent in [
			AgentType::Codex,
			AgentType::Antigravity,
			AgentType::Gemini,
			AgentType::Cline,
			AgentType::Copilot,
			AgentType::Kimi,
			AgentType::Warp,
		] {
			let temp = tempdir().unwrap();
			let root = temp.path();
			let mut manager =
				ConfigManager::new(create_adapter(agent), false, Some(root));
			manager.load().unwrap();
			manager.add_skill(Skill::new("independent")).unwrap();
			assert!(
				!root.join(".agents/skills/independent").exists(),
				"{agent:?}"
			);
			let holders = crate::load_all_agents(
				crate::models::ResourceScope::ProjectOnly,
				Some(root),
			);
			let holders: Vec<_> = holders
				.iter()
				.filter(|row| {
					row.skills.iter().any(|skill| skill.name == "independent")
				})
				.map(|row| row.agent_id)
				.collect();
			assert_eq!(
				holders,
				vec![agent.as_str()],
				"grant leaked from {agent:?}"
			);
			let result = transfer_skill(
				ResourceLocator {
					agent,
					scope: InstallScope::Project,
					project_root: Some(root.to_path_buf()),
					name: "independent".into(),
				},
				vec![InstallTarget {
					agent: AgentType::Claude,
					scope: InstallScope::Project,
					project_root: Some(root.to_path_buf()),
				}],
			);
			assert!(result.unwrap().results.iter().all(|row| row.success));
			let removed = manager
				.remove_skill_planned("independent", false, false, true)
				.unwrap();
			assert!(removed.failed_paths.is_empty());
			manager.load().unwrap();
			assert!(
				manager.get_skill("independent").is_none(),
				"{agent:?} still reads it"
			);
			assert!(root.join(".claude/skills/independent/SKILL.md").is_file());
			assert!(root.join(".aghub/independent/SKILL.md").is_file());
		}
	}

	#[test]
	fn reconcile_skill_deletes_when_removed() {
		let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
		let temp = tempdir().unwrap();
		let root = temp.path().join("project");
		fs::create_dir_all(&root).unwrap();

		let mut manager = ConfigManager::new(
			create_adapter(AgentType::Claude),
			false,
			Some(&root),
		);
		manager.load().unwrap();
		let mut skill = Skill::new("repo-helper");
		skill.description = Some("Copies files".to_string());
		manager.add_skill(skill).unwrap();

		let result = reconcile_skill(
			ResourceLocator {
				agent: AgentType::Claude,
				scope: InstallScope::Project,
				project_root: Some(root.clone()),
				name: "repo-helper".to_string(),
			},
			vec![],                  // added
			vec![AgentType::Claude], // removed
			true,                    // confirm
		)
		.unwrap();

		assert_eq!(result.results.len(), 1);
		assert_eq!(result.results[0].action, OperationAction::Delete);

		let mut manager = ConfigManager::new(
			create_adapter(AgentType::Claude),
			false,
			Some(&root),
		);
		manager.load().unwrap();
		assert!(manager.get_skill("repo-helper").is_none());
	}

	// Removing an agent that never held the skill FAILS THAT ROW and rejects
	// nothing else. Two halves, and the batch half is the one this test was
	// written for: the planner answers `ResourceNotFound`, and the preflight
	// added for the shared-master guard must map that to "allow" rather than
	// aborting the whole batch before any write — so `reconcile_skill` still
	// returns `Ok(batch)` and an untouched disk.
	//
	// The ROW half used to be a success, because the delete arm blessed every
	// `ResourceNotFound` it saw. That is what let `--remove cursor --remove
	// windsurf` against two agents that had never held the skill exit 0
	// reporting two deletions — the same misreport `reconcile mcp` had, one
	// subcommand over. Forgiveness now needs a credential (`RemovalCredits`):
	// an earlier row of this same command must really have taken the entry
	// these two share. A never-holder shares nothing and holds nothing, so its
	// row errors, exactly as the MCP and sub-agent arms already answered it.
	#[test]
	fn reconcile_skill_never_holder_fails_its_own_row_only() {
		let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
		let temp = tempdir().unwrap();
		let root = temp.path().join("project");
		// A PRIVATE copy under claude's own dir: no `.agents/skills` Master, so
		// no other agent can see it and windsurf is a genuine never-holder.
		let private = root.join(".claude/skills/never");
		fs::create_dir_all(&private).unwrap();
		fs::write(
			private.join("SKILL.md"),
			"---\nname: never\ndescription: Private\n---\n\n# Never\n",
		)
		.unwrap();

		let result = reconcile_skill(
			ResourceLocator {
				agent: AgentType::Claude,
				scope: InstallScope::Project,
				project_root: Some(root.clone()),
				name: "never".to_string(),
			},
			vec![],
			vec![AgentType::Windsurf],
			true, // confirm
		)
		.expect("a never-holder row must not abort the batch");

		assert_eq!(result.results.len(), 1);
		assert!(
			!result.results[0].success,
			"a removal that took nothing must not report a deletion"
		);
		assert!(
			result.results[0]
				.error
				.as_deref()
				.unwrap_or_default()
				.contains("not found"),
			"the row must say the skill was not there, got: {:?}",
			result.results[0].error
		);
		assert!(
			private.join("SKILL.md").exists(),
			"claude's own copy must not be touched"
		);
	}

	#[test]
	fn reconcile_skill_never_holder_with_lock_entry_fails_its_own_row() {
		let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
		let temp = tempdir().unwrap();
		let root = temp.path().join("project");
		// A PRIVATE copy under claude's own dir: windsurf is a genuine never-holder.
		let private = root.join(".claude/skills/never");
		fs::create_dir_all(&private).unwrap();
		fs::write(
			private.join("SKILL.md"),
			"---\nname: never\ndescription: Private\n---\n\n# Never\n",
		)
		.unwrap();

		// Seed a valid skills-lock.json entry for "never" in project scope.
		let lock_path = root.join("skills-lock.json");
		let lock_content = r#"{"version":1,"skills":{"never":{"source":"test","sourceType":"node_modules","computedHash":"abc123"}}}"#;
		fs::write(&lock_path, lock_content).unwrap();

		let result = reconcile_skill(
			ResourceLocator {
				agent: AgentType::Claude,
				scope: InstallScope::Project,
				project_root: Some(root.clone()),
				name: "never".to_string(),
			},
			vec![],
			vec![AgentType::Windsurf],
			true, // confirm
		)
		.expect("a never-holder row must not abort the batch");

		assert_eq!(result.results.len(), 1);
		assert!(
			!result.results[0].success,
			"a never-holder removal must not report a deletion even when lock entry exists"
		);
		assert!(
			result.results[0]
				.error
				.as_deref()
				.unwrap_or_default()
				.contains("not found"),
			"the row must say the skill was not found, got: {:?}",
			result.results[0].error
		);
		assert!(
			private.join("SKILL.md").exists(),
			"claude's own copy must not be touched"
		);
	}

	// A delete row whose backing SURVIVED is not a deletion, and it vouches for
	// nobody. `RemovalOutcome::executed` is true for the whole execute branch
	// even when every `remove_dir_all` returned `EACCES` — its own doc says so
	// — so mapping it straight to the credential let a FAILED row bless the
	// sibling rows that share the Master: exit 0, two reported deletions, and
	// `SKILL.md` still on disk.
	//
	// The Master dir is `0o555`: discovery still reads `SKILL.md` through the
	// referrers (so both rows get planned and the removal is exhaustive), while
	// unlinking the file inside it fails. Running as ROOT defeats that — the
	// unlink succeeds and the fixture measures nothing — so the surviving
	// `SKILL.md` is asserted FIRST, with a message naming root as the cause,
	// rather than letting the test pass quietly.
	#[cfg(unix)]
	#[test]
	fn reconcile_skill_failed_master_delete_credits_no_sibling() {
		use std::os::unix::fs::PermissionsExt;

		// Restore on unwind too: a panic before a plain chmod-back leaves the
		// tempdir undeletable.
		struct RestorePerms(PathBuf);
		impl Drop for RestorePerms {
			fn drop(&mut self) {
				let _ = fs::set_permissions(
					&self.0,
					fs::Permissions::from_mode(0o755),
				);
			}
		}

		let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
		let temp = tempdir().unwrap();
		let root = temp.path();
		let master = root.join(".aghub/my-skill");
		fs::create_dir_all(&master).unwrap();
		fs::write(
			master.join("SKILL.md"),
			"---\nname: my-skill\ndescription: Shared\n---\n\n# My Skill\n",
		)
		.unwrap();
		// Two agents with PRIVATE skill dirs, each linking to the one Master:
		// they share a backing (so a credit from one would forgive the other)
		// and naming both is what makes the removal exhaustive. The shared
		// `.agents/skills` slot is deliberately absent — it would make eight
		// more agents holders and the removal non-exhaustive.
		for dir in [".claude/skills", ".windsurf/skills"] {
			let referrer_dir = root.join(dir);
			fs::create_dir_all(&referrer_dir).unwrap();
			std::os::unix::fs::symlink(&master, referrer_dir.join("my-skill"))
				.unwrap();
		}
		fs::set_permissions(&master, fs::Permissions::from_mode(0o555))
			.unwrap();
		let _restore = RestorePerms(master.clone());

		let result = reconcile_skill(
			ResourceLocator {
				agent: AgentType::Claude,
				scope: InstallScope::Project,
				project_root: Some(root.to_path_buf()),
				name: "my-skill".to_string(),
			},
			vec![],
			vec![AgentType::Claude, AgentType::Windsurf],
			true, // confirm
		)
		.expect("a failed delete must not abort the batch");

		assert!(
			master.join("SKILL.md").exists(),
			"fixture broken: the Master went away under 0o555 — this test \
			 cannot run as root, where the unlink succeeds"
		);
		let row = |agent: AgentType| {
			result
				.results
				.iter()
				.find(|r| r.target.agent == agent)
				.unwrap_or_else(|| panic!("no row for {}", agent.as_str()))
		};
		let failed = row(AgentType::Claude);
		assert!(
			!failed.success,
			"a row that left the Master on disk must not report a deletion, \
			 got: {failed:?}"
		);
		assert!(
			!failed
				.error
				.as_deref()
				.unwrap_or_default()
				.contains("not found"),
			"the failing row must name the delete failure, not a missing \
			 skill, got: {:?}",
			failed.error
		);
		let sibling = row(AgentType::Windsurf);
		assert!(
			!sibling.success,
			"the sibling found nothing only because the first row FAILED, so \
			 no credential may forgive it, got: {sibling:?}"
		);
	}

	#[cfg(unix)]
	#[test]
	fn reconcile_removes_shared_referrers_before_private_fallback_readers() {
		let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
		let temp = tempdir().unwrap();
		let root = temp.path();
		master_with_claude_referrer(root, "notebooklm");
		let master = root.join(".aghub/notebooklm");
		let original = fs::read(master.join("SKILL.md")).unwrap();
		for dir in [".opencode", ".cursor", ".pi", ".grok", ".omp"] {
			let slot = root.join(dir).join("skills");
			fs::create_dir_all(&slot).unwrap();
			std::os::unix::fs::symlink(&master, slot.join("notebooklm"))
				.unwrap();
		}
		let source = ResourceLocator {
			agent: AgentType::Claude,
			scope: InstallScope::Project,
			project_root: Some(root.to_path_buf()),
			name: "notebooklm".into(),
		};
		// Private readers deliberately precede the shared-slot writers.
		let removed = vec![
			AgentType::OpenCode,
			AgentType::Cursor,
			AgentType::Pi,
			AgentType::Grok,
			AgentType::Omp,
			AgentType::Codex,
			AgentType::Antigravity,
			AgentType::Gemini,
			AgentType::Cline,
			AgentType::Copilot,
			AgentType::Kimi,
			AgentType::Amp,
			AgentType::Warp,
			AgentType::ZCode,
			AgentType::Dsh,
		];
		reconcile_skill_preview(&source, &[], &removed).unwrap();
		assert!(root.join(".agents/skills/notebooklm").is_symlink());
		let result =
			reconcile_skill(source, vec![], removed.clone(), true).unwrap();
		assert!(result.results.iter().all(|row| row.success), "{result:?}");
		for agent in removed {
			let dirs = create_adapter(agent).get_skills_paths(
				Some(root),
				crate::models::ResourceScope::ProjectOnly,
			);
			let effect = crate::skills::removal::read_effect_after(
				&dirs,
				"notebooklm",
				&[],
			);
			assert!(
				effect.survivors.is_empty(),
				"{agent:?}: {:?}",
				effect.survivors
			);
		}
		assert_eq!(fs::read(master.join("SKILL.md")).unwrap(), original);
		assert!(root.join(".claude/skills/notebooklm/SKILL.md").is_file());
	}

	#[cfg(unix)]
	#[test]
	fn reconcile_skill_delete_results_follow_request_order() {
		let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
		let temp = tempdir().unwrap();
		let root = temp.path();
		master_with_claude_referrer(root, "notebooklm");
		let master = root.join(".aghub/notebooklm");
		for dir in [".opencode", ".cursor", ".pi", ".grok", ".omp"] {
			let slot = root.join(dir).join("skills");
			fs::create_dir_all(&slot).unwrap();
			std::os::unix::fs::symlink(&master, slot.join("notebooklm"))
				.unwrap();
		}

		let source = ResourceLocator {
			agent: AgentType::Claude,
			scope: InstallScope::Project,
			project_root: Some(root.to_path_buf()),
			name: "notebooklm".into(),
		};

		// Request order puts private readers before shared-slot readers.
		// A shared-first sort would have reordered them (shared-slot readers before private readers).
		let removed = vec![
			AgentType::OpenCode,
			AgentType::Cursor,
			AgentType::Pi,
			AgentType::Grok,
			AgentType::Omp,
			AgentType::Codex,
			AgentType::Antigravity,
			AgentType::Gemini,
			AgentType::Cline,
			AgentType::Copilot,
			AgentType::Kimi,
			AgentType::Amp,
			AgentType::Warp,
			AgentType::ZCode,
			AgentType::Dsh,
		];
		let result =
			reconcile_skill(source, vec![], removed.clone(), true).unwrap();

		assert_eq!(result.results.len(), 15);
		let result_order: Vec<AgentType> =
			result.results.iter().map(|r| r.target.agent).collect();
		assert_eq!(
			result_order, removed,
			"delete result rows must follow request order exactly"
		);
		assert!(
			result.results.iter().all(|r| r.success),
			"all removals must succeed: {:?}",
			result.results
		);
	}

	#[cfg(unix)]
	#[test]
	fn reconcile_orders_shared_referrers_first_when_other_agents_disabled() {
		let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
		let temp = tempdir().unwrap();
		let root = temp.path();
		master_with_claude_referrer(root, "notebooklm");
		let master = root.join(".aghub/notebooklm");
		let original = fs::read(master.join("SKILL.md")).unwrap();
		for dir in [".opencode", ".cursor", ".pi", ".grok", ".omp"] {
			let slot = root.join(dir).join("skills");
			fs::create_dir_all(&slot).unwrap();
			std::os::unix::fs::symlink(&master, slot.join("notebooklm"))
				.unwrap();
		}
		let source = ResourceLocator {
			agent: AgentType::Claude,
			scope: InstallScope::Project,
			project_root: Some(root.to_path_buf()),
			name: "notebooklm".into(),
		};
		// Private readers deliberately precede the shared-slot writers.
		let removed = vec![
			AgentType::OpenCode,
			AgentType::Cursor,
			AgentType::Pi,
			AgentType::Grok,
			AgentType::Omp,
			AgentType::Codex,
			AgentType::Antigravity,
			AgentType::Gemini,
			AgentType::Cline,
			AgentType::Copilot,
			AgentType::Kimi,
			AgentType::Amp,
			AgentType::Warp,
			AgentType::ZCode,
			AgentType::Dsh,
		];
		let ids: Vec<&str> = AgentType::ALL
			.iter()
			.map(|a| a.as_str())
			.filter(|id| *id != "opencode" && *id != "claude")
			.collect();
		let _off = crate::agent_settings::test_override::disable(&ids);
		reconcile_skill_preview(&source, &[], &removed).unwrap();
		assert!(root.join(".agents/skills/notebooklm").is_symlink());
		let result =
			reconcile_skill(source, vec![], removed.clone(), true).unwrap();
		assert!(result.results.iter().all(|row| row.success), "{result:?}");
		for agent in removed {
			let dirs = create_adapter(agent).get_skills_paths(
				Some(root),
				crate::models::ResourceScope::ProjectOnly,
			);
			let effect = crate::skills::removal::read_effect_after(
				&dirs,
				"notebooklm",
				&[],
			);
			assert!(
				effect.survivors.is_empty(),
				"{agent:?}: {:?}",
				effect.survivors
			);
		}
		assert_eq!(fs::read(master.join("SKILL.md")).unwrap(), original);
		assert!(root.join(".claude/skills/notebooklm/SKILL.md").is_file());
	}

	// G3: "add to claude, remove from cursor" is an END STATE that cannot exist
	// — cursor reads the Master directly, and the add guarantees the Master
	// stays, so the removal can never take effect. It used to be discovered
	// AFTER the copy had already written claude's Referrer, leaving a
	// half-applied reconcile on disk. Asserting the error alone is not enough:
	// the old behaviour also exited non-zero.
	#[cfg(unix)]
	#[test]
	fn reconcile_skill_refuses_native_reader_removal_that_cannot_take_effect() {
		let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
		let temp = tempdir().unwrap();
		let root = temp.path().join("project");
		fs::create_dir_all(&root).unwrap();
		master_with_claude_referrer(&root, "mover");
		let master = root.join(".aghub/mover");
		// Claude already has a Referrer, so give the copy a fresh target.
		let windsurf_link = root.join(".windsurf/skills/mover");

		// Disk state is asserted BEFORE the return value: the old behaviour also
		// exited non-zero, so only "the copy never landed" separates them.
		let outcome = reconcile_skill(
			ResourceLocator {
				agent: AgentType::Cursor,
				scope: InstallScope::Project,
				project_root: Some(root.clone()),
				name: "mover".to_string(),
			},
			vec![AgentType::Windsurf],
			vec![AgentType::Cursor],
			true, // confirm
		);

		assert!(
			std::fs::symlink_metadata(&windsurf_link).is_err(),
			"the copy must NOT have landed before the impossible delete was \
			 discovered"
		);
		assert!(
			master.join("SKILL.md").exists(),
			"the Master must be untouched"
		);
		let error = outcome
			.expect_err(
				"an unreachable end state must be refused, not half-applied",
			)
			.to_string();
		assert!(error.contains("nothing was written"), "got: {error}");
		// A refusal the user cannot act on is barely better than a silent
		// no-op: this shape is unreachable BECAUSE of the add, and the message
		// has to say so.
		assert!(
			error.contains("adds the skill to another agent"),
			"the refusal must name the add that keeps the Master alive; got: \
			 {error}"
		);
	}

	// A holder whose CONFIG cannot be parsed is still a holder.
	//
	// `skill_holders` used to answer through `load_all_agents`, whose config
	// load parses MCPs first and gives up on the first error — so one
	// unparseable `.mcp.json` turned claude's very real Referrer into an empty
	// skill list. Read as "does not hold it", the reconcile believed it was
	// dropping the LAST holder and took the Master with it, deleting the
	// Referrer of an agent the user never named and exiting 0. Reading the
	// skill dirs directly is what stops an unrelated MCP file from hiding a
	// holder. Revoking the other agents must leave Claude and the Master intact.
	#[cfg(unix)]
	#[test]
	fn reconcile_skill_will_not_gc_the_master_when_a_holder_is_unreadable() {
		let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
		let temp = tempdir().unwrap();
		let root = temp.path().join("project");
		fs::create_dir_all(&root).unwrap();
		master_with_claude_referrer(&root, "mover");
		let master = root.join(".aghub/mover");
		let claude_referrer = root.join(".claude/skills/mover");
		// Copilot shares Claude's malformed `.mcp.json`; it is also an
		// unselected reader, so give it a private surviving Referrer.
		let copilot_private = root.join(".github/skills/mover");
		fs::create_dir_all(copilot_private.parent().unwrap()).unwrap();
		std::os::unix::fs::symlink(&master, &copilot_private).unwrap();
		// Claude's project MCP config exists but cannot be parsed, so its whole
		// config load fails and its Referrer used to become invisible.
		fs::write(root.join(".mcp.json"), "{ not json").unwrap();

		// Every holder the fail-OPEN scan can still see. Removing exactly these
		// is what used to make the reconcile look exhaustive; deriving the list
		// keeps that true as the agent roster grows.
		let readable_holders: Vec<AgentType> = crate::load_all_agents(
			crate::models::ResourceScope::ProjectOnly,
			Some(&root),
		)
		.into_iter()
		.filter(|agent| agent.skills.iter().any(|s| s.name == "mover"))
		.filter_map(|agent| agent.agent_id.parse::<AgentType>().ok())
		.collect();
		assert!(
			!readable_holders.is_empty(),
			"fixture is broken: no agent reads the Master"
		);
		assert!(
			!readable_holders.contains(&AgentType::Claude),
			"fixture is broken: claude's config still loads, so it is not the \
			 invisible holder this test needs"
		);

		// Disk state is asserted BEFORE the return value: the regression this
		// pins is data loss, not a different `Result` shape.
		let outcome = reconcile_skill(
			ResourceLocator {
				agent: AgentType::Cursor,
				scope: InstallScope::Project,
				project_root: Some(root.clone()),
				name: "mover".to_string(),
			},
			vec![],
			readable_holders,
			true, // confirm
		);

		assert!(
			master.join("SKILL.md").exists(),
			"the Master must survive: an agent whose config could not be read \
			 may still be reading it"
		);
		assert!(
			std::fs::symlink_metadata(&claude_referrer).is_ok(),
			"the Referrer of an agent the user never named must survive"
		);
		let result = outcome.expect("the readable skills can be revoked without collecting Claude's Master");
		assert!(result.results.iter().all(|row| row.success), "{result:?}");
		assert!(!root.join(".agents/skills/mover").exists());
	}

	// Fail-CLOSED is not fail-shut: one unreadable config makes `exhaustive`
	// false, it does NOT veto every removal. A NeedsLink agent still has its
	// own Referrer to give up, so unlinking it must go through.
	#[cfg(unix)]
	#[test]
	fn an_unreadable_agent_does_not_block_a_referrer_unlink() {
		let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
		let temp = tempdir().unwrap();
		let root = temp.path().join("project");
		fs::create_dir_all(&root).unwrap();
		master_with_claude_referrer(&root, "mover");
		let master = root.join(".aghub/mover");
		let windsurf_skills = root.join(".windsurf/skills");
		let windsurf_referrer = windsurf_skills.join("mover");
		fs::create_dir_all(&windsurf_skills).unwrap();
		std::os::unix::fs::symlink(&master, &windsurf_referrer).unwrap();
		fs::write(root.join(".mcp.json"), "{ not json").unwrap();

		let result = reconcile_skill(
			ResourceLocator {
				agent: AgentType::Windsurf,
				scope: InstallScope::Project,
				project_root: Some(root.clone()),
				name: "mover".to_string(),
			},
			vec![],
			vec![AgentType::Windsurf],
			true, // confirm
		)
		.expect(
			"an unrelated broken config must not veto a removal that can \
			 actually take effect",
		);

		assert!(result.results[0].success, "{:?}", result.results[0].error);
		assert!(
			std::fs::symlink_metadata(&windsurf_referrer).is_err(),
			"windsurf's Referrer must be gone"
		);
		assert!(
			master.join("SKILL.md").exists(),
			"the Master keeps its other readers"
		);
	}

	// The core guard refuses UNREACHABLE end states — it does NOT adopt the
	// desktop dialog's "add first, then remove" product rule. Moving a private
	// copy to another agent in one reconcile stays legal.
	#[cfg(unix)]
	#[test]
	fn reconcile_skill_still_allows_add_then_remove_of_a_private_copy() {
		let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
		let temp = tempdir().unwrap();
		let root = temp.path().join("project");
		let private = root.join(".claude/skills/solo");
		fs::create_dir_all(&private).unwrap();
		fs::write(
			private.join("SKILL.md"),
			"---\nname: solo\ndescription: Private\n---\n\n# Solo\n",
		)
		.unwrap();

		let result = reconcile_skill(
			ResourceLocator {
				agent: AgentType::Claude,
				scope: InstallScope::Project,
				project_root: Some(root.clone()),
				name: "solo".to_string(),
			},
			vec![AgentType::Windsurf],
			vec![AgentType::Claude],
			true, // confirm
		)
		.expect("moving a private copy between agents must stay legal");

		assert_eq!(result.results.len(), 2);
		for row in &result.results {
			assert!(row.success, "{:?}: {:?}", row.action, row.error);
		}
		assert!(
			root.join(".aghub/solo/SKILL.md").exists(),
			"the copy must have materialised the Master"
		);
		assert!(
			std::fs::symlink_metadata(root.join(".windsurf/skills/solo"))
				.is_ok(),
			"windsurf must be linked to it"
		);
		assert!(
			!private.exists(),
			"claude's private copy must be gone — that is the whole point of \
			 the move"
		);
	}

	// The copy runs BEFORE the delete, so a reconcile can hand the removed
	// agent the skill back through the Master it just created.
	//
	// Cursor holds `solo` as a private folder and also reads `.agents/skills`.
	// The preflight probe looks at a disk where that Master does not exist yet,
	// so every row passed: the copy materialised `.aghub/solo` +
	// windsurf's link, the delete took cursor's private folder, BOTH rows
	// reported success — and cursor could still see `solo`, now via the Master.
	// Nothing on disk recorded that anything was wrong.
	//
	// Deliberately NOT the same shape as
	// `reconcile_skill_still_allows_add_then_remove_of_a_private_copy`: there
	// the removed agent is claude, which does NOT read `.agents/skills`, so the
	// move really does take the skill away and must stay legal.
	#[cfg(unix)]
	#[test]
	fn reconcile_skill_refuses_a_removal_the_paired_copy_would_undo() {
		let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
		let temp = tempdir().unwrap();
		let root = temp.path().join("project");
		let private = root.join(".cursor/skills/solo");
		fs::create_dir_all(&private).unwrap();
		fs::write(
			private.join("SKILL.md"),
			"---\nname: solo\ndescription: Private\n---\n\n# Solo\n",
		)
		.unwrap();

		// The copy target must SHARE cursor's directory for the removal to be
		// unreachable. Cline writes the same `.agents/skills` cursor reads, so
		// "add cline, remove cursor" hands the skill straight back. A copy to
		// windsurf — which this test used to make — now writes only
		// `.windsurf/skills` and the store, reaches cursor not at all, and the
		// removal is perfectly legal.
		//
		// Disk state is asserted BEFORE the return value: the old behaviour
		// returned Ok, so only "nothing landed" separates the two.
		let outcome = reconcile_skill(
			ResourceLocator {
				agent: AgentType::Cursor,
				scope: InstallScope::Project,
				project_root: Some(root.clone()),
				name: "solo".to_string(),
			},
			vec![AgentType::Amp],
			vec![AgentType::Cursor],
			true, // confirm
		);

		assert!(
			private.join("SKILL.md").exists(),
			"cursor's copy must survive a removal that could not take effect"
		);
		assert!(
			!root.join(".aghub/solo").exists(),
			"the copy must NOT have landed: it is the thing that would hand \
			 the skill straight back to cursor"
		);
		let message = outcome
			.expect_err(
				"cline writes the very `.agents/skills` cursor reads, so the \
				 copy this reconcile makes would restore what the delete takes",
			)
			.to_string();
		assert!(message.contains("nothing was written"), "got: {message}");
		assert!(
			message.contains("adds the skill to another agent"),
			"the refusal must name the add that keeps the Master alive; got: \
			 {message}"
		);
	}

	// Every agent the fail-OPEN roster scan can see holding `name`. Derived
	// rather than listed so the fixtures keep working as the agent roster
	// grows, and deliberately NOT `skill_holders` — a test that asks the code
	// under test for its own expectation proves nothing.
	#[cfg(unix)]
	/// A protective check must fail CLOSED on "cannot tell".
	///
	/// `sub_agent_backing_path` / `skill_entry_backing` load a whole
	/// `ConfigManager`, which parses the agent's MCPs too — so an unrelated
	/// malformed config on a roster-protected agent used to answer `None`,
	/// indistinguishable from "holds nothing". The guard skipped it and the
	/// removal rewrote the file they shared, reporting success. `Backed`
	/// separates the two answers; this pins that the undeterminable one refuses.
	#[test]
	fn an_undeterminable_protected_backing_refuses_the_removal() {
		let target = |agent| InstallTarget {
			agent,
			scope: InstallScope::Global,
			project_root: None,
		};
		let removing = vec![target(AgentType::Claude)];
		let protect = vec![Protected {
			target: target(AgentType::Grok),
			// Not named by the command — exactly the case the roster protect
			// list exists for.
			named: false,
		}];

		let refusal = ensure_removals_spare(
			&protect,
			&removing,
			AgentType::Claude,
			|t: &InstallTarget| match t.agent {
				AgentType::Claude => {
					Backed::At(PathBuf::from("/tmp/shared.md"))
				}
				// Grok's config would not parse: unknown, not absent.
				_ => Backed::Unknown,
			},
		)
		.expect_err("an undeterminable sharer must not be skipped");
		let message = refusal.to_string();
		assert!(
			message.contains("grok") && message.contains("claude"),
			"the refusal must name both agents so it is actionable: {message}"
		);

		// The same shape, but Grok is KNOWN to hold nothing: that is a real
		// answer and must stay permissive, or every reconcile refuses.
		ensure_removals_spare(
			&protect,
			&removing,
			AgentType::Claude,
			|t: &InstallTarget| match t.agent {
				AgentType::Claude => {
					Backed::At(PathBuf::from("/tmp/shared.md"))
				}
				_ => Backed::Absent,
			},
		)
		.expect("a determined non-holder must not block the removal");
	}

	// Its three callers are all `#[cfg(unix)]` (they build symlinked or
	// chmod-ed layouts), so an ungated definition is dead code on Windows and
	// `-D warnings` fails there — a gap only the push-to-main Windows lint
	// sees, because a local `--target x86_64-pc-windows-msvc` cannot build
	// `zstd-sys`/`aws-lc-sys` without an MSVC C toolchain.
	#[cfg(unix)]
	fn holders_via_agent_roster(
		root: &std::path::Path,
		name: &str,
	) -> Vec<AgentType> {
		crate::load_all_agents(
			crate::models::ResourceScope::ProjectOnly,
			Some(root),
		)
		.into_iter()
		.filter(|agent| agent.skills.iter().any(|s| s.name == name))
		.filter_map(|agent| agent.agent_id.parse::<AgentType>().ok())
		.collect()
	}

	// FAIL-CLOSED IS NOT FAIL-SHUT, the load-failure half.
	//
	// `skill_holders` used to go through the whole config load, which parses
	// MCPs first and aborts on the first error. Reading that abort as "this
	// agent might hold the skill" made ANY agent with a broken MCP file veto
	// the removal — including roocode, which cannot even read `.agents/skills`
	// and never held this skill. The scope's universal skills then became
	// unremovable through every surface until an unrelated JSON file was fixed.
	// Asking the skill dirs directly is what makes the MCP file irrelevant.
	#[cfg(unix)]
	#[test]
	fn a_broken_mcp_file_of_a_non_holder_does_not_block_master_collection() {
		let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
		let temp = tempdir().unwrap();
		let root = temp.path().join("project");
		fs::create_dir_all(&root).unwrap();
		let master = root.join(".aghub/mover");
		fs::create_dir_all(&master).unwrap();
		fs::write(
			master.join("SKILL.md"),
			"---\nname: mover\ndescription: Shared\n---\n\n# Mover\n",
		)
		.unwrap();
		// Codex reads the shared `.agents/skills` slot and holds a Referrer
		// there. It used to need none: the Master itself lived in that
		// directory, so storing the skill granted it. Now the grant is the link.
		fs::create_dir_all(root.join(".agents/skills")).unwrap();
		std::os::unix::fs::symlink(&master, root.join(".agents/skills/mover"))
			.unwrap();
		// roocode holds no skill here and reads no `.agents/skills`; only its
		// MCP config is broken.
		fs::create_dir_all(root.join(".roo")).unwrap();
		fs::write(root.join(".roo/mcp.json"), "{ oops").unwrap();

		let holders = holders_via_agent_roster(&root, "mover");
		assert!(!holders.is_empty(), "fixture: nobody reads the Master");
		assert!(
			!holders.contains(&AgentType::RooCode),
			"fixture: roocode must NOT be a holder, or this proves nothing"
		);

		// Disk state is asserted BEFORE the return value: the regression is an
		// availability one — the Master that SHOULD be collected is still
		// there because an unrelated file could not be parsed.
		let outcome = reconcile_skill(
			ResourceLocator {
				agent: AgentType::Cursor,
				scope: InstallScope::Project,
				project_root: Some(root.clone()),
				name: "mover".to_string(),
			},
			vec![],
			holders,
			true, // confirm
		);

		assert!(
			!master.exists(),
			"dropping every holder must collect the Master; it survived, so \
			 something unrelated to skills refused the operation"
		);
		let result = outcome.expect(
			"an unrelated agent's broken MCP file must not veto the removal",
		);
		assert!(
			result.results.iter().all(|row| row.success),
			"{:?}",
			result.results
		);
	}

	// FAIL-CLOSED, the half that must stay closed: a holder whose skills
	// directory EXISTS but cannot be listed is "cannot tell", not "holds
	// nothing", and the two are the same empty list to every fail-open reader.
	// Treating it as nothing is what garbage-collected a Master out from under
	// an agent the user never named.
	//
	// The dir must be genuinely UNLISTABLE, not merely "not a directory": see
	// the fixture below for why the two are different answers.
	#[cfg(unix)]
	#[test]
	fn reconcile_skill_keeps_the_master_when_a_holders_dir_cannot_be_listed() {
		let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
		let temp = tempdir().unwrap();
		let root = temp.path().join("project");
		fs::create_dir_all(&root).unwrap();
		let master = root.join(".aghub/mover");
		fs::create_dir_all(&master).unwrap();
		fs::write(
			master.join("SKILL.md"),
			"---\nname: mover\ndescription: Shared\n---\n\n# Mover\n",
		)
		.unwrap();
		// Codex reads the shared `.agents/skills` slot and holds a Referrer
		// there. It used to need none: the Master itself lived in that
		// directory, so storing the skill granted it. Now the grant is the link.
		fs::create_dir_all(root.join(".agents/skills")).unwrap();
		std::os::unix::fs::symlink(&master, root.join(".agents/skills/mover"))
			.unwrap();
		fs::create_dir_all(root.join(".windsurf")).unwrap();
		// A self-referential symlink, NOT a plain file: `read_dir` fails with
		// ELOOP for any user, including a CI job running as root. A plain file
		// is the wrong fixture — a path that is not a directory holds no
		// entries at all, which is a COMPLETE answer ("nothing here"), not the
		// "cannot tell" this test is about.
		std::os::unix::fs::symlink(
			std::path::Path::new("skills"),
			root.join(".windsurf/skills"),
		)
		.unwrap();

		let holders = holders_via_agent_roster(&root, "mover");
		assert!(
			!holders.contains(&AgentType::Windsurf),
			"fixture: windsurf's dir is unlistable, so the fail-open scan must \
			 not see it as a holder"
		);

		// Disk state is asserted BEFORE the return value: the regression this
		// pins is data loss, not a different `Result` shape.
		let outcome = reconcile_skill(
			ResourceLocator {
				agent: AgentType::Cursor,
				scope: InstallScope::Project,
				project_root: Some(root.clone()),
				name: "mover".to_string(),
			},
			vec![],
			holders,
			true, // confirm
		);

		assert!(
			master.join("SKILL.md").exists(),
			"the Master must survive: an agent whose skills directory could \
			 not be listed may still be reading it"
		);
		let message = outcome
			.expect_err("an unverifiable holder must block the collection")
			.to_string();
		assert!(
			message.contains("windsurf")
				&& message.contains("skills directory unreadable"),
			"the refusal must name the agent it could not read and why, or the \
			 user has nothing to act on; got: {message}"
		);
	}

	// NAMING the unreadable holder is not authority to collect the Master.
	//
	// Counting it as a holder keeps `exhaustive` false while it is unnamed —
	// but `--remove windsurf` flips `exhaustive` true, and the batch would then
	// let a READABLE row delete the Master while windsurf's own row is still
	// ahead of it: its preflight fails OPEN on a config it cannot load, and
	// rows are attempt-all, so ordering saves nothing. Master gone, opaque copy
	// left behind — reached through the one input meant to authorize it.
	#[cfg(unix)]
	#[test]
	fn reconcile_skill_refuses_when_the_named_holder_is_the_unreadable_one() {
		let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
		let temp = tempdir().unwrap();
		let root = temp.path().join("project");
		fs::create_dir_all(&root).unwrap();
		let master = root.join(".aghub/mover");
		fs::create_dir_all(&master).unwrap();
		fs::write(
			master.join("SKILL.md"),
			"---\nname: mover\ndescription: Shared\n---\n\n# Mover\n",
		)
		.unwrap();
		// Codex reads the shared `.agents/skills` slot and holds a Referrer
		// there. It used to need none: the Master itself lived in that
		// directory, so storing the skill granted it. Now the grant is the link.
		fs::create_dir_all(root.join(".agents/skills")).unwrap();
		std::os::unix::fs::symlink(&master, root.join(".agents/skills/mover"))
			.unwrap();
		// A regular FILE where the skills dir belongs: `read_dir` fails for
		// every user, including a CI job running as root.
		fs::create_dir_all(root.join(".windsurf")).unwrap();
		// A self-referential symlink, NOT a plain file: `read_dir` fails with
		// ELOOP for any user, including a CI job running as root. A plain file
		// is the wrong fixture — a path that is not a directory holds no
		// entries at all, which is a COMPLETE answer ("nothing here"), not the
		// "cannot tell" this test is about.
		std::os::unix::fs::symlink(
			std::path::Path::new("skills"),
			root.join(".windsurf/skills"),
		)
		.unwrap();

		let mut removed = holders_via_agent_roster(&root, "mover");
		// THE input under test: the caller names the agent aghub cannot read.
		removed.push(AgentType::Windsurf);

		let outcome = reconcile_skill(
			ResourceLocator {
				agent: AgentType::Cursor,
				scope: InstallScope::Project,
				project_root: Some(root.clone()),
				name: "mover".to_string(),
			},
			vec![],
			removed,
			true, // confirm
		);

		// Disk first: the regression is data loss, not a `Result` shape.
		assert!(
			master.join("SKILL.md").exists(),
			"the Master must survive: this run cannot verify what windsurf \
			 holds, so it cannot honour \"take it from windsurf too\""
		);
		let message = outcome
			.expect_err(
				"naming an agent the run cannot mutate must not authorize the \
				 collection",
			)
			.to_string();
		assert!(
			message.contains("windsurf")
				&& message.contains("skills directory unreadable"),
			"the refusal must name the agent it could not read and why; got: \
			 {message}"
		);
	}

	#[cfg(unix)]
	#[test]
	fn reconcile_skill_move_with_unreadable_agent_is_not_refused_wholesale() {
		let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
		let temp = tempdir().unwrap();
		let root = temp.path().join("project");
		fs::create_dir_all(&root).unwrap();
		let master = root.join(".aghub/mover");
		fs::create_dir_all(&master).unwrap();
		fs::write(
			master.join("SKILL.md"),
			"---\nname: mover\ndescription: Shared\n---\n\n# Mover\n",
		)
		.unwrap();

		// Codex holds a private Referrer in its own skills dir.
		fs::create_dir_all(root.join(".codex/skills")).unwrap();
		let codex_referrer = root.join(".codex/skills/mover");
		std::os::unix::fs::symlink(&master, &codex_referrer).unwrap();

		// Windsurf has an unreadable skills directory (self-referential symlink).
		fs::create_dir_all(root.join(".windsurf")).unwrap();
		std::os::unix::fs::symlink(
			std::path::Path::new("skills"),
			root.join(".windsurf/skills"),
		)
		.unwrap();

		// Move: add Claude, remove Codex and Windsurf.
		// Since Claude is added, the Master will survive, so the unreadable Windsurf
		// does not threaten Master collection and must not cause wholesale refusal.
		let outcome = reconcile_skill(
			ResourceLocator {
				agent: AgentType::Codex,
				scope: InstallScope::Project,
				project_root: Some(root.clone()),
				name: "mover".to_string(),
			},
			vec![AgentType::Claude],
			vec![AgentType::Codex, AgentType::Windsurf],
			true, // confirm
		);

		let batch = outcome.expect(
			"move must not be refused wholesale due to unreadable Windsurf",
		);

		// Claude copy succeeded.
		let claude_row = batch
			.results
			.iter()
			.find(|r| {
				r.target.agent == AgentType::Claude
					&& r.action == OperationAction::Copy
			})
			.expect("claude copy row exists");
		assert!(claude_row.success, "claude copy must succeed");

		// Codex delete succeeded.
		let codex_row = batch
			.results
			.iter()
			.find(|r| {
				r.target.agent == AgentType::Codex
					&& r.action == OperationAction::Delete
			})
			.expect("codex delete row exists");
		assert!(codex_row.success, "codex delete must succeed");

		// Windsurf delete failed its own row.
		let windsurf_row = batch
			.results
			.iter()
			.find(|r| {
				r.target.agent == AgentType::Windsurf
					&& r.action == OperationAction::Delete
			})
			.expect("windsurf delete row exists");
		assert!(
			!windsurf_row.success,
			"windsurf delete must fail its own row"
		);
		assert!(windsurf_row.error.is_some(), "windsurf must have an error");

		// Disk state checks:
		// Master must survive because Claude was added.
		assert!(master.join("SKILL.md").exists(), "Master must survive");
		// Claude's referrer was created.
		assert!(
			root.join(".claude/skills/mover").exists(),
			"Claude referrer must exist"
		);
		// Codex's referrer was unlinked.
		assert!(!codex_referrer.exists(), "Codex referrer must be removed");
	}

	// An agent that reads BOTH a private dir and the Master defeats a verdict
	// read off the plan alone: the private artifact IS removable, so the plan
	// looks effective, while the agent keeps seeing the skill through the
	// Master. The row reported a removal that never happened.
	//
	// aghub does not create that artifact today (a NativeReader gets no
	// Referrer), but `npx skills` and older aghub releases did.
	#[cfg(unix)]
	#[test]
	fn reconcile_skill_refuses_a_removal_the_master_would_undo() {
		let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
		let temp = tempdir().unwrap();
		let root = temp.path().join("project");
		fs::create_dir_all(&root).unwrap();
		let master = root.join(".aghub/mover");
		fs::create_dir_all(&master).unwrap();
		fs::write(
			master.join("SKILL.md"),
			"---\nname: mover\ndescription: Shared\n---\n\n# Mover\n",
		)
		.unwrap();
		// Codex reads the shared `.agents/skills` slot and holds a Referrer
		// there. It used to need none: the Master itself lived in that
		// directory, so storing the skill granted it. Now the grant is the link.
		fs::create_dir_all(root.join(".agents/skills")).unwrap();
		std::os::unix::fs::symlink(&master, root.join(".agents/skills/mover"))
			.unwrap();
		// opencode reads `.opencode/skills` FIRST and `.agents/skills` second,
		// so this stale link is what its config load discovers.
		let stale = root.join(".opencode/skills/mover");
		fs::create_dir_all(stale.parent().unwrap()).unwrap();
		std::os::unix::fs::symlink(&master, &stale).unwrap();

		let outcome = reconcile_skill(
			ResourceLocator {
				agent: AgentType::OpenCode,
				scope: InstallScope::Project,
				project_root: Some(root.clone()),
				name: "mover".to_string(),
			},
			vec![],
			vec![AgentType::OpenCode],
			true, // confirm
		);

		assert!(
			std::fs::symlink_metadata(&stale).is_ok(),
			"nothing may be unlinked for a removal that cannot take effect"
		);
		assert!(
			master.join("SKILL.md").exists(),
			"the Master keeps its other readers"
		);
		let message = outcome
			.expect_err(
				"opencode still reads the Master, so removing it takes nothing \
				 away and must be refused",
			)
			.to_string();
		assert!(message.contains("nothing was written"), "got: {message}");
	}

	// A disabled agent is not a reader (docs/history/core-removal.md
	// #disabled-agent-blocked-a-single-agent-delete), so the refusal must not
	// name it as one. `keepers` comes from the full-roster Master-GC scan and
	// used to list every disabled holder.
	#[cfg(unix)]
	#[test]
	fn reconcile_refusal_does_not_name_a_disabled_agent_as_a_reader() {
		let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
		let temp = tempdir().unwrap();
		let root = temp.path().join("project");
		fs::create_dir_all(&root).unwrap();
		let master = root.join(".aghub/mover");
		fs::create_dir_all(&master).unwrap();
		fs::write(
			master.join("SKILL.md"),
			"---\nname: mover\ndescription: Shared\n---\n\n# Mover\n",
		)
		.unwrap();
		// Codex (disabled below) and claude (enabled) both hold the skill;
		// opencode's stale private link beside the shared one makes its own
		// removal a no-op, which is what refuses the row.
		for dir in [".agents/skills", ".claude/skills", ".opencode/skills"] {
			let slot = root.join(dir);
			fs::create_dir_all(&slot).unwrap();
			std::os::unix::fs::symlink(&master, slot.join("mover")).unwrap();
		}
		let ids: Vec<&str> = AgentType::ALL
			.iter()
			.map(|a| a.as_str())
			.filter(|id| *id != "opencode" && *id != "claude")
			.collect();
		let _off = crate::agent_settings::test_override::disable(&ids);

		let message = reconcile_skill(
			ResourceLocator {
				agent: AgentType::OpenCode,
				scope: InstallScope::Project,
				project_root: Some(root.clone()),
				name: "mover".to_string(),
			},
			vec![],
			vec![AgentType::OpenCode],
			true,
		)
		.expect_err("opencode still reads the shared link, so the row refuses")
		.to_string();

		assert!(
			message.contains("still read by 'claude'"),
			"the enabled keeper is named: {message}"
		);
		assert!(
			!message.contains("codex"),
			"a disabled agent is not a reader: {message}"
		);
	}

	// The preflight refuses UNREACHABLE end states, not rows that merely look
	// risky. A delete target whose own config cannot be parsed is neither: it
	// is that row's problem, and the mutate arm already fails it. Escalating it
	// to a batch-wide rejection would let one unrelated broken `.mcp.json`
	// cancel a perfectly good copy to a DIFFERENT agent.
	#[cfg(unix)]
	#[test]
	fn a_broken_config_on_a_delete_target_does_not_cancel_the_paired_copy() {
		let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
		let temp = tempdir().unwrap();
		let root = temp.path().join("project");
		fs::create_dir_all(&root).unwrap();
		let master = root.join(".aghub/mover");
		fs::create_dir_all(&master).unwrap();
		fs::write(
			master.join("SKILL.md"),
			"---\nname: mover\ndescription: Shared\n---\n\n# Mover\n",
		)
		.unwrap();
		// Codex reads the shared `.agents/skills` slot and holds a Referrer
		// there. It used to need none: the Master itself lived in that
		// directory, so storing the skill granted it. Now the grant is the link.
		fs::create_dir_all(root.join(".agents/skills")).unwrap();
		std::os::unix::fs::symlink(&master, root.join(".agents/skills/mover"))
			.unwrap();
		fs::create_dir_all(root.join(".cursor")).unwrap();
		fs::write(root.join(".cursor/mcp.json"), "{ oops").unwrap();

		// Disk state is asserted BEFORE the return value: the regression is a
		// copy that never ran, not a different `Result` shape.
		let outcome = reconcile_skill(
			ResourceLocator {
				agent: AgentType::Codex,
				scope: InstallScope::Project,
				project_root: Some(root.clone()),
				name: "mover".to_string(),
			},
			vec![AgentType::Windsurf],
			vec![AgentType::Cursor],
			true, // confirm
		);

		assert!(
			std::fs::symlink_metadata(root.join(".windsurf/skills/mover"))
				.is_ok(),
			"the copy must still land: nothing about it depends on cursor's \
			 MCP file"
		);
		let result = outcome
			.expect("one row's unreadable config must not abort the batch");
		let cursor_row = result
			.results
			.iter()
			.find(|row| row.target.agent == AgentType::Cursor)
			.expect("cursor row");
		assert!(
			!cursor_row.success,
			"the unreadable config still fails ITS OWN row: {cursor_row:?}"
		);
	}

	// The confirmation gate lives in core so the CLI's `--yes` and the API's
	// `confirm` cannot drift. Asserting the ERROR alone would still pass if the
	// guard ran AFTER the deletes, so this also proves the skill survived.
	#[test]
	fn reconcile_skill_without_confirm_refuses_and_removes_nothing() {
		let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
		let temp = tempdir().unwrap();
		let root = temp.path().join("project");
		fs::create_dir_all(&root).unwrap();

		let mut manager = ConfigManager::new(
			create_adapter(AgentType::Claude),
			false,
			Some(&root),
		);
		manager.load().unwrap();
		manager.add_skill(Skill::new("repo-helper")).unwrap();

		let error = reconcile_skill(
			ResourceLocator {
				agent: AgentType::Claude,
				scope: InstallScope::Project,
				project_root: Some(root.clone()),
				name: "repo-helper".to_string(),
			},
			vec![],
			vec![AgentType::Claude],
			false, // confirm withheld
		)
		.expect_err("a removing reconcile must refuse without confirmation");
		assert!(
			error.to_string().contains("confirm"),
			"error should name what is missing, got: {error}"
		);

		let mut manager = ConfigManager::new(
			create_adapter(AgentType::Claude),
			false,
			Some(&root),
		);
		manager.load().unwrap();
		assert!(
			manager.get_skill("repo-helper").is_some(),
			"unconfirmed reconcile must not delete the skill"
		);
	}

	// Adds are non-destructive, so withholding confirmation must NOT block
	// them — otherwise the guard silently breaks every install-only reconcile.
	#[test]
	fn reconcile_skill_adds_without_confirm() {
		let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
		let temp = tempdir().unwrap();
		let root = temp.path().join("project");
		fs::create_dir_all(&root).unwrap();

		let mut manager = ConfigManager::new(
			create_adapter(AgentType::Claude),
			false,
			Some(&root),
		);
		manager.load().unwrap();
		manager.add_skill(Skill::new("repo-helper")).unwrap();

		let referrer = root.join(".windsurf/skills/repo-helper");
		assert!(
			!referrer.exists(),
			"the destination must start uncovered, or the assertion at the \
			 end proves nothing"
		);

		let result = reconcile_skill(
			ResourceLocator {
				agent: AgentType::Claude,
				scope: InstallScope::Project,
				project_root: Some(root.clone()),
				name: "repo-helper".to_string(),
			},
			// Windsurf NEEDS a Referrer — a NativeReader destination would
			// read the Master the fixture already created, so the disk
			// assertion below would hold even if the reconcile did nothing.
			vec![AgentType::Windsurf],
			vec![],
			false, // confirm withheld — irrelevant to an add
		)
		.expect("an add-only reconcile needs no confirmation");

		// `failed_count() == 0` alone is vacuous — an empty result set also
		// satisfies it. Pin the copy AND the state it was supposed to produce.
		assert_eq!(result.results.len(), 1);
		assert_eq!(result.results[0].action, OperationAction::Copy);
		assert!(result.results[0].success);
		assert!(referrer.exists(), "the add must create Windsurf's referrer");
	}

	// Regression (skill case): a Copy that fails at RUNTIME (after preflight
	// already passed) must not let its paired Delete run — same policy as
	// `reconcile_mcp_keeps_source_when_a_copy_fails_at_runtime`, but for the
	// highest-blast-radius resource, since a skill delete can `remove_dir_all`
	// an on-disk directory.
	//
	// The source skill here is a COPY-LAYOUT skill: a plain, hand-created
	// directory inside Claude's own skills dir with no `.agents/skills`
	// Master, so `canonical_path` is None and this directory is the SOLE
	// on-disk copy. Windsurf's own skills dir already holds a real directory
	// at the slot the copy would need to link into, so the universal
	// materializer's link step reports a conflict at write time — preflight
	// (`skill_target_dir`) only resolves the write dir, it never checks for an
	// existing occupant. A reconcile that attempts the Delete regardless
	// loses the skill: the source directory would be `remove_dir_all`'d
	// even though the Windsurf copy never landed, destroying the skill
	// outright with no surviving copy anywhere. This test fails on that
	// regression because `skill_dir.join("SKILL.md").exists()` would be
	// `false` afterward.
	#[test]
	fn reconcile_skill_keeps_source_when_a_copy_fails_at_runtime() {
		let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
		let temp = tempdir().unwrap();
		let root = temp.path().join("project");

		let claude_skills = root.join(".claude/skills");
		let skill_dir = claude_skills.join("repo-helper");
		fs::create_dir_all(&skill_dir).unwrap();
		fs::write(
			skill_dir.join("SKILL.md"),
			"---\nname: repo-helper\ndescription: Copies files\n---\n",
		)
		.unwrap();

		// Pre-occupy the Windsurf destination slot with a real directory (not
		// a symlink) so `Linker::link` reports `Conflict` at runtime.
		let windsurf_slot = root.join(".windsurf/skills/repo-helper");
		fs::create_dir_all(&windsurf_slot).unwrap();
		fs::write(windsurf_slot.join("occupant.txt"), "conflict").unwrap();

		let mut claude_manager = ConfigManager::new(
			create_adapter(AgentType::Claude),
			false,
			Some(&root),
		);
		claude_manager.load().unwrap();
		let source_skill = claude_manager
			.get_skill("repo-helper")
			.expect("discovery must pick up the hand-created skill dir");
		assert!(
			source_skill.canonical_path.is_none(),
			"copy-layout precondition: no universal Master"
		);

		let result = reconcile_skill(
			ResourceLocator {
				agent: AgentType::Claude,
				scope: InstallScope::Project,
				project_root: Some(root.clone()),
				name: "repo-helper".to_string(),
			},
			vec![AgentType::Windsurf], // added: fails at runtime
			vec![AgentType::Claude],   // removed: must be skipped
			true,                      // confirm
		)
		.unwrap();

		assert_eq!(result.results.len(), 2);
		let copy_row = result
			.results
			.iter()
			.find(|r| r.action == OperationAction::Copy)
			.expect("a copy row must be present");
		assert!(!copy_row.success, "the Windsurf copy must fail");

		let delete_row = result
			.results
			.iter()
			.find(|r| r.action == OperationAction::Delete)
			.expect("a delete row must be present");
		assert!(
			!delete_row.success,
			"the Claude delete must be skipped, not attempted"
		);
		assert!(
			delete_row
				.error
				.as_ref()
				.is_some_and(|e| e.contains("skipped")),
			"the delete row must read as skipped, not as an attempted \
			 failure: {:?}",
			delete_row.error,
		);

		// The critical assertion: the source skill directory is the SOLE
		// on-disk copy and must survive.
		assert!(
			skill_dir.join("SKILL.md").exists(),
			"source skill dir must survive a reconcile whose only copy failed"
		);
	}

	#[cfg(unix)]
	// Smoke test only — the real data-loss guard is the Windows junction test below.
	#[test]
	fn reconcile_skill_unlinks_symlink_referrer_keeps_master() {
		use crate::adapter::set_skills_path_override;

		struct SkillsPathOverrideReset;

		impl Drop for SkillsPathOverrideReset {
			fn drop(&mut self) {
				set_skills_path_override("claude", None);
			}
		}

		let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
		let temp = tempdir().unwrap();
		let root = temp.path();
		let master = root.join(".aghub/my-skill");
		let claude_skills = root.join(".claude/skills");
		let referrer = claude_skills.join("my-skill");
		let skill_md =
			"---\nname: my-skill\ndescription: Shared\n---\n\n# My Skill\n";

		fs::create_dir_all(&master).unwrap();
		fs::write(master.join("SKILL.md"), skill_md).unwrap();
		fs::create_dir_all(&claude_skills).unwrap();
		std::os::unix::fs::symlink(&master, &referrer).unwrap();
		// Cursor reaches a project skill only through the shared
		// `.agents/skills` slot, which used to hold the Master itself. Without
		// this link cursor simply does not have the skill, and a test about
		// removing it from cursor measures nothing.
		let shared = root.join(".agents/skills");
		fs::create_dir_all(&shared).unwrap();
		std::os::unix::fs::symlink(&master, shared.join("my-skill")).unwrap();
		set_skills_path_override("claude", Some(claude_skills));
		let _reset_override = SkillsPathOverrideReset;

		let mut manager = ConfigManager::new(
			create_adapter(AgentType::Claude),
			false,
			Some(root),
		);
		manager.load().unwrap();
		assert!(manager.get_skill("my-skill").is_some());

		let result = reconcile_skill(
			ResourceLocator {
				agent: AgentType::Claude,
				scope: InstallScope::Project,
				project_root: Some(root.to_path_buf()),
				name: "my-skill".to_string(),
			},
			vec![],
			vec![AgentType::Claude],
			true, // confirm
		)
		.unwrap();

		assert_eq!(result.results.len(), 1);
		assert_eq!(result.results[0].action, OperationAction::Delete);
		assert!(std::fs::symlink_metadata(&referrer).is_err());
		let master_skill = master.join("SKILL.md");
		assert!(master_skill.exists());
		assert_eq!(fs::read_to_string(master_skill).unwrap(), skill_md);
	}

	// T-RECONCILE-NATIVE-READER: reconcile --remove for a NativeReader agent
	// (cursor reads `.agents/skills` directly) must NOT delete the shared
	// Master another agent still symlinks. The pre-seam code found the Master
	// via cursor's READ dirs and `remove_dir_all`'d it — data loss for every
	// referrer. This test fails if the removal path stops going through
	// `remove_skill_planned`'s classifier.
	//
	// The refusal MOVED: it used to be a failed row inside an `Ok` batch (the
	// row itself was never asserted), and is now a preflight rejection of the
	// whole reconcile. Same invariant, stronger claim — nothing is written.
	#[cfg(unix)]
	#[test]
	fn reconcile_skill_remove_native_reader_keeps_shared_master() {
		use crate::adapter::set_skills_path_override;

		struct SkillsPathOverrideReset;

		impl Drop for SkillsPathOverrideReset {
			fn drop(&mut self) {
				set_skills_path_override("claude", None);
			}
		}

		let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
		let temp = tempdir().unwrap();
		let root = temp.path();
		let master = root.join(".aghub/my-skill");
		let sentinel = master.join("sentinel.txt");
		let claude_skills = root.join(".claude/skills");
		let referrer = claude_skills.join("my-skill");
		let skill_md =
			"---\nname: my-skill\ndescription: Shared\n---\n\n# My Skill\n";

		fs::create_dir_all(&master).unwrap();
		fs::write(master.join("SKILL.md"), skill_md).unwrap();
		fs::write(&sentinel, "keep-me").unwrap();
		fs::create_dir_all(&claude_skills).unwrap();
		std::os::unix::fs::symlink(&master, &referrer).unwrap();
		// Cursor reaches a project skill only through the shared
		// `.agents/skills` slot, which used to hold the Master itself. Without
		// this link cursor simply does not have the skill, and a test about
		// removing it from cursor measures nothing.
		let shared = root.join(".agents/skills");
		fs::create_dir_all(&shared).unwrap();
		std::os::unix::fs::symlink(&master, shared.join("my-skill")).unwrap();
		set_skills_path_override("claude", Some(claude_skills));
		let _reset_override = SkillsPathOverrideReset;

		let error = reconcile_skill(
			ResourceLocator {
				agent: AgentType::Claude,
				scope: InstallScope::Project,
				project_root: Some(root.to_path_buf()),
				name: "my-skill".to_string(),
			},
			vec![],
			vec![AgentType::Cursor],
			true, // confirm
		)
		.expect_err(
			"removing a NativeReader while the Master stays cannot take \
			 effect and must be refused",
		);
		assert!(
			error.to_string().contains("nothing was written"),
			"the refusal must say the batch never ran, got: {error}"
		);

		// The shared Master and its contents must survive.
		assert!(
			master.join("SKILL.md").exists(),
			"Master SKILL.md must survive a NativeReader remove"
		);
		assert!(
			sentinel.exists(),
			"sentinel inside master must survive (remove_dir_all would \
			 have wiped it)"
		);
		// Claude's referrer must still resolve to the live Master.
		assert!(
			fs::canonicalize(&referrer).is_ok(),
			"claude referrer symlink must stay intact"
		);
		assert_eq!(
			fs::read_dir(root.join(".claude/skills")).unwrap().count(),
			1,
			"a refused reconcile must not add anything under .claude/skills"
		);
	}

	// T-RECONCILE-WIN-JUNCTION: the real data-loss guard.
	// remove_dir_all on a Windows JUNCTION follows the reparse point into the
	// shared Master and deletes its contents.  This test would FAIL if the fix
	// reverted to remove_dir_all.  The unix test above is a smoke test only.
	#[cfg(windows)]
	#[test]
	fn reconcile_skill_junction_referrer_removed_master_survives() {
		use crate::adapter::set_skills_path_override;
		use crate::skills::linker::create_junction;

		struct SkillsPathOverrideReset;

		impl Drop for SkillsPathOverrideReset {
			fn drop(&mut self) {
				set_skills_path_override("claude", None);
			}
		}

		let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
		let temp = tempdir().unwrap();
		let root = temp.path();
		let master = root.join(".aghub/my-skill");
		let sentinel = master.join("sentinel.txt");
		let claude_skills = root.join(".claude/skills");
		let referrer = claude_skills.join("my-skill");
		let skill_md =
			"---\nname: my-skill\ndescription: Shared\n---\n\n# My Skill\n";

		fs::create_dir_all(&master).unwrap();
		fs::write(master.join("SKILL.md"), skill_md).unwrap();
		fs::write(&sentinel, "keep-me").unwrap();
		fs::create_dir_all(&claude_skills).unwrap();

		// Build a Windows JUNCTION: referrer -> master.
		let abs_master = master.canonicalize().unwrap();
		create_junction(&abs_master, &referrer).unwrap();

		// A SECOND agent reading the same Master through its own junction.
		// Without it this test no longer proves anything: removing the LAST
		// Referrer now collects the Master by design, so the sentinel would
		// vanish legitimately and "recursed through the junction" would look
		// identical on disk to "collected correctly". With cursor still reading
		// it, the Master MUST survive — and the sentinel is once again the
		// evidence that `remove_dir_all` did not follow the junction.
		let cursor_skills = root.join(".cursor/skills");
		fs::create_dir_all(&cursor_skills).unwrap();
		create_junction(&abs_master, &cursor_skills.join("my-skill")).unwrap();

		set_skills_path_override("claude", Some(claude_skills));
		let _reset_override = SkillsPathOverrideReset;

		let mut manager = ConfigManager::new(
			create_adapter(AgentType::Claude),
			false,
			Some(root),
		);
		manager.load().unwrap();
		assert!(manager.get_skill("my-skill").is_some());

		let result = reconcile_skill(
			ResourceLocator {
				agent: AgentType::Claude,
				scope: InstallScope::Project,
				project_root: Some(root.to_path_buf()),
				name: "my-skill".to_string(),
			},
			vec![],
			vec![AgentType::Claude],
			true, // confirm
		)
		.unwrap();

		assert_eq!(result.results.len(), 1);
		assert_eq!(result.results[0].action, OperationAction::Delete);
		// The junction referrer must be gone.
		assert!(
			std::fs::symlink_metadata(&referrer).is_err(),
			"junction referrer must be removed"
		);
		// The shared Master directory and its contents must survive.
		assert!(
			master.join("SKILL.md").exists(),
			"Master SKILL.md must survive"
		);
		assert!(
			sentinel.exists(),
			"sentinel file inside master must survive (remove_dir_all \
			 would have wiped it)"
		);
	}

	#[test]
	fn transfer_sub_agent_copies_to_other_agent_project() {
		let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
		let temp = tempdir().unwrap();
		let source_root = temp.path().join("source");
		let dest_root = temp.path().join("dest");
		fs::create_dir_all(&source_root).unwrap();
		fs::create_dir_all(&dest_root).unwrap();

		let mut source_manager = ConfigManager::new(
			create_adapter(AgentType::Claude),
			false,
			Some(&source_root),
		);
		source_manager.load().unwrap();
		let mut sub_agent = SubAgent::new("coder");
		sub_agent.description = Some("Expert coder".to_string());
		sub_agent.instruction =
			Some("You are an expert programmer.".to_string());
		source_manager.add_sub_agent(sub_agent).unwrap();

		let result = transfer_sub_agent(
			ResourceLocator {
				agent: AgentType::Claude,
				scope: InstallScope::Project,
				project_root: Some(source_root.clone()),
				name: "coder".to_string(),
			},
			vec![InstallTarget {
				agent: AgentType::OpenCode,
				scope: InstallScope::Project,
				project_root: Some(dest_root.clone()),
			}],
		)
		.unwrap();

		assert_eq!(result.success_count(), 1);

		let mut dest_manager = ConfigManager::new(
			create_adapter(AgentType::OpenCode),
			false,
			Some(&dest_root),
		);
		dest_manager.load().unwrap();
		assert!(dest_manager.get_sub_agent("coder").is_some());
	}

	#[test]
	fn reconcile_sub_agent_adds_and_removes() {
		let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
		let temp = tempdir().unwrap();
		let root = temp.path().join("project");
		fs::create_dir_all(&root).unwrap();

		let mut manager = ConfigManager::new(
			create_adapter(AgentType::Claude),
			false,
			Some(&root),
		);
		manager.load().unwrap();
		let mut sub_agent = SubAgent::new("coder");
		sub_agent.description = Some("Expert coder".to_string());
		sub_agent.instruction =
			Some("You are an expert programmer.".to_string());
		manager.add_sub_agent(sub_agent).unwrap();

		let result = reconcile_sub_agent(
			ResourceLocator {
				agent: AgentType::Claude,
				scope: InstallScope::Project,
				project_root: Some(root.clone()),
				name: "coder".to_string(),
			},
			vec![AgentType::OpenCode], // added
			vec![AgentType::Claude],   // removed
			true,                      // confirm
		)
		.unwrap();

		assert_eq!(result.results.len(), 2);
		assert_eq!(result.results[0].action, OperationAction::Copy);
		assert_eq!(result.results[0].target.agent, AgentType::OpenCode);
		assert_eq!(result.results[1].action, OperationAction::Delete);
		assert_eq!(result.results[1].target.agent, AgentType::Claude);
		assert!(result.results.iter().all(|r| r.success));
	}

	#[cfg(unix)]
	#[test]
	fn reconcile_sub_agent_checks_shared_source_even_when_alias_deletes_first()
	{
		use std::os::unix::fs::symlink;
		let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
		let temp = tempdir().unwrap();
		let root = temp.path().join("project");
		fs::create_dir_all(root.join(".claude/agents")).unwrap();
		symlink(".claude", root.join(".opencode")).unwrap();
		let mut manager = ConfigManager::new(
			create_adapter(AgentType::Claude),
			false,
			Some(&root),
		);
		manager.load().unwrap();
		let mut sub_agent = SubAgent::new("coder");
		sub_agent.instruction = Some("shared source".into());
		manager.add_sub_agent(sub_agent).unwrap();
		let source_file = root.join(".claude/agents/coder.md");
		let result = reconcile_sub_agent(
			ResourceLocator {
				agent: AgentType::Claude,
				scope: InstallScope::Project,
				project_root: Some(root.clone()),
				name: "coder".into(),
			},
			vec![AgentType::Codex],
			vec![AgentType::OpenCode, AgentType::Claude],
			true,
		)
		.unwrap();
		assert_eq!(result.success_count(), 3);
		assert_eq!(result.failed_count(), 0);
		assert!(!source_file.exists());
		assert!(root.join(".codex/agents/coder.toml").exists());
	}

	#[test]
	fn reconcile_sub_agent_keeps_source_when_existing_frontmatter_differs() {
		let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
		let temp = tempdir().unwrap();
		let root = temp.path();
		let source = root.join(".claude/agents/coder.md");
		let target = root.join(".opencode/agents/coder.md");
		fs::create_dir_all(source.parent().unwrap()).unwrap();
		fs::create_dir_all(target.parent().unwrap()).unwrap();
		fs::write(&source, "---\nname: coder\ndescription: Coder\ntools: Read\n---\n\nDo work.\n").unwrap();
		fs::write(&target, "---\nname: coder\ndescription: Coder\ntools: Write\n---\n\nDo work.\n").unwrap();

		let result = reconcile_sub_agent(
			ResourceLocator {
				agent: AgentType::Claude,
				scope: InstallScope::Project,
				project_root: Some(root.to_path_buf()),
				name: "coder".to_string(),
			},
			vec![AgentType::OpenCode],
			vec![AgentType::Claude],
			true,
		)
		.unwrap();

		assert!(
			source.exists(),
			"a different target must not authorize source deletion"
		);
		assert_eq!(
			result.failed_count(),
			2,
			"copy conflict must also skip the delete"
		);
		assert!(fs::read_to_string(target).unwrap().contains("tools: Write"));
	}

	#[test]
	fn reconcile_sub_agent_keeps_source_when_copy_changed_after_copy(
	) -> Result<()> {
		let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
		let temp = tempdir().unwrap();
		let root = temp.path();
		let source_file = root.join(".claude/agents/coder.md");
		let target_file = root.join(".opencode/agents/coder.md");
		fs::create_dir_all(source_file.parent().unwrap()).unwrap();
		fs::write(
			&source_file,
			"---\nname: coder\ndescription: Coder\ntools: Read\n---\n\nDo work.\n",
		)
		.unwrap();

		let source = ResourceLocator {
			agent: AgentType::Claude,
			scope: InstallScope::Project,
			project_root: Some(root.to_path_buf()),
			name: "coder".to_string(),
		};
		let expected = load_source_sub_agent(&source)?;
		let target = InstallTarget {
			agent: AgentType::OpenCode,
			scope: InstallScope::Project,
			project_root: Some(root.to_path_buf()),
		};
		copy_sub_agent_into(&target, &expected)?;
		let copies = vec![OperationPlan {
			target: target.clone(),
			action: OperationAction::Copy,
		}];

		fs::write(
			&target_file,
			"---\nname: coder\ndescription: Coder\ntools: Write\n---\n\nDo work.\n",
		)
		.unwrap();

		let source_target = InstallTarget {
			agent: source.agent,
			scope: source.scope,
			project_root: source.project_root.clone(),
		};
		let error = delete_reconciled_sub_agent(
			&source,
			&source_target,
			&expected,
			&[],
			&copies,
			true,
		)
		.expect_err("source must survive if its copy changed after copy");
		assert!(error.to_string().contains("changed in target"));
		assert!(source_file.exists());
		assert!(fs::read_to_string(&source_file)
			.unwrap()
			.contains("tools: Read"));

		Ok(())
	}

	#[test]
	fn reconcile_sub_agent_refuses_unmodelled_codex_fields_before_writing() {
		let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
		let temp = tempdir().unwrap();
		let root = temp.path();
		let source = root.join(".codex/agents/coder.toml");
		let target = root.join(".opencode/agents/coder.md");
		fs::create_dir_all(source.parent().unwrap()).unwrap();
		fs::write(&source, "name = \"coder\"\ndescription = \"Coder\"\ndeveloper_instructions = \"Do work.\"\nmodel = \"gpt-5.4\"\n").unwrap();

		let error = reconcile_sub_agent(
			ResourceLocator {
				agent: AgentType::Codex,
				scope: InstallScope::Project,
				project_root: Some(root.to_path_buf()),
				name: "coder".to_string(),
			},
			vec![AgentType::OpenCode],
			vec![AgentType::Codex],
			true,
		)
		.expect_err(
			"native Codex fields cannot be copied through the normalized model",
		);

		assert!(error.to_string().contains("unmanaged"), "got: {error}");
		assert!(source.exists());
		assert!(
			!target.exists(),
			"preflight refusal must precede destination writes"
		);

		fs::write(&source, "name = \"coder\"\ndescription = \"Coder\"\ndeveloper_instructions = \"Do work.\"\n").unwrap();
		let result = reconcile_sub_agent(
			ResourceLocator {
				agent: AgentType::Codex,
				scope: InstallScope::Project,
				project_root: Some(root.to_path_buf()),
				name: "coder".to_string(),
			},
			vec![AgentType::OpenCode],
			vec![AgentType::Codex],
			true,
		)
		.unwrap();
		assert_eq!(result.success_count(), 2);
		assert!(target.exists());
	}

	#[test]
	fn reconcile_sub_agent_refuses_markdown_extras_for_codex_target() {
		let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
		let temp = tempdir().unwrap();
		let root = temp.path();
		let source = root.join(".claude/agents/coder.md");
		let target = root.join(".codex/agents/coder.toml");
		fs::create_dir_all(source.parent().unwrap()).unwrap();
		fs::write(&source, "---\nname: coder\ndescription: Coder\ntools: Read\n---\n\nDo work.\n").unwrap();

		let error = reconcile_sub_agent(
			ResourceLocator {
				agent: AgentType::Claude,
				scope: InstallScope::Project,
				project_root: Some(root.to_path_buf()),
				name: "coder".to_string(),
			},
			vec![AgentType::Codex],
			vec![AgentType::Claude],
			true,
		)
		.expect_err("Codex cannot write Markdown frontmatter extras");

		assert!(
			error.to_string().contains("without losing fields"),
			"got: {error}"
		);
		assert!(source.exists());
		assert!(!target.exists());
	}

	#[test]
	fn transfer_mcp_to_multiple_targets() {
		let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
		let temp = tempdir().unwrap();
		let source_root = temp.path().join("source");
		let dest_root_cursor = temp.path().join("dest_cursor");
		let dest_root_copilot = temp.path().join("dest_copilot");
		fs::create_dir_all(&source_root).unwrap();
		fs::create_dir_all(&dest_root_cursor).unwrap();
		fs::create_dir_all(&dest_root_copilot).unwrap();

		let mut source_manager = ConfigManager::new(
			create_adapter(AgentType::Claude),
			false,
			Some(&source_root),
		);
		source_manager.load().unwrap();
		source_manager
			.add_mcp(McpServer::new(
				"filesystem",
				McpTransport::stdio("npx", vec!["mcp-filesystem".to_string()]),
			))
			.unwrap();

		let result = transfer_mcp(
			ResourceLocator {
				agent: AgentType::Claude,
				scope: InstallScope::Project,
				project_root: Some(source_root.clone()),
				name: "filesystem".to_string(),
			},
			vec![
				InstallTarget {
					agent: AgentType::Cursor,
					scope: InstallScope::Project,
					project_root: Some(dest_root_cursor.clone()),
				},
				InstallTarget {
					agent: AgentType::Copilot,
					scope: InstallScope::Project,
					project_root: Some(dest_root_copilot.clone()),
				},
			],
		)
		.unwrap();

		assert_eq!(result.success_count(), 2);

		let mut cursor_manager = ConfigManager::new(
			create_adapter(AgentType::Cursor),
			false,
			Some(&dest_root_cursor),
		);
		cursor_manager.load().unwrap();
		assert!(cursor_manager.get_mcp("filesystem").is_some());

		let mut copilot_manager = ConfigManager::new(
			create_adapter(AgentType::Copilot),
			false,
			Some(&dest_root_copilot),
		);
		copilot_manager.load().unwrap();
		assert!(copilot_manager.get_mcp("filesystem").is_some());
	}

	#[test]
	fn transfer_skill_to_multiple_targets() {
		let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
		let temp = tempdir().unwrap();
		let source_root = temp.path().join("source");
		let dest_root_cursor = temp.path().join("dest_cursor");
		let dest_root_windsurf = temp.path().join("dest_windsurf");
		fs::create_dir_all(&source_root).unwrap();
		fs::create_dir_all(&dest_root_cursor).unwrap();
		fs::create_dir_all(&dest_root_windsurf).unwrap();

		let mut source_manager = ConfigManager::new(
			create_adapter(AgentType::Claude),
			false,
			Some(&source_root),
		);
		source_manager.load().unwrap();
		let mut skill = Skill::new("repo-helper");
		skill.description = Some("Copies files".to_string());
		source_manager.add_skill(skill).unwrap();

		let result = transfer_skill(
			ResourceLocator {
				agent: AgentType::Claude,
				scope: InstallScope::Project,
				project_root: Some(source_root.clone()),
				name: "repo-helper".to_string(),
			},
			vec![
				InstallTarget {
					agent: AgentType::Cursor,
					scope: InstallScope::Project,
					project_root: Some(dest_root_cursor.clone()),
				},
				InstallTarget {
					agent: AgentType::Windsurf,
					scope: InstallScope::Project,
					project_root: Some(dest_root_windsurf.clone()),
				},
			],
		)
		.unwrap();

		assert_eq!(result.success_count(), 2);
		assert!(dest_root_cursor
			.join(".aghub/repo-helper/SKILL.md")
			.exists());
		assert!(dest_root_windsurf
			.join(".aghub/repo-helper/SKILL.md")
			.exists());
		assert!(crate::skills::linker::Linker::is_link(
			&dest_root_windsurf.join(".windsurf/skills/repo-helper")
		));
	}

	#[test]
	fn transfer_skill_already_present_is_an_idempotent_success() {
		let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
		let temp = tempdir().unwrap();
		let source_root = temp.path().join("source");
		let dest_root = temp.path().join("dest");
		fs::create_dir_all(&source_root).unwrap();
		fs::create_dir_all(&dest_root).unwrap();

		// Create source skill
		let mut source_manager = ConfigManager::new(
			create_adapter(AgentType::Claude),
			false,
			Some(&source_root),
		);
		source_manager.load().unwrap();
		let mut skill = Skill::new("repo-helper");
		skill.description = Some("Copies files".to_string());
		source_manager.add_skill(skill).unwrap();

		// Create existing skill in destination
		let mut dest_manager = ConfigManager::new(
			create_adapter(AgentType::Cursor),
			false,
			Some(&dest_root),
		);
		dest_manager.load().unwrap();
		let mut existing_skill = Skill::new("repo-helper");
		existing_skill.description = Some("Existing skill".to_string());
		dest_manager.add_skill(existing_skill).unwrap();

		let result = transfer_skill(
			ResourceLocator {
				agent: AgentType::Claude,
				scope: InstallScope::Project,
				project_root: Some(source_root.clone()),
				name: "repo-helper".to_string(),
			},
			vec![InstallTarget {
				agent: AgentType::Cursor,
				scope: InstallScope::Project,
				project_root: Some(dest_root.clone()),
			}],
		)
		.unwrap();

		// The destination already holds a skill of this name, installed through
		// the same universal path — so its `.agents` Master IS the one being
		// transferred. That is an idempotent no-op, not a conflict.
		//
		// This used to assert `failed_count == 1`, because `transfer_skill`
		// carried its own `get_skill(..).is_some()` guard while
		// `reconcile_skill --add` had none: the same operation, opposite
		// verdicts. The guard is gone; `add_skill_from_path` decides, and it
		// still refuses a REAL foreign occupant (a same-named directory that is
		// not a link to the Master).
		assert_eq!(
			result.failed_count(),
			0,
			"an already-present skill is an idempotent success: {:?}",
			result.results[0].error
		);
		assert!(
			result.results[0].already_present,
			"and the row must SAY nothing was written, or the caller cannot \
			 tell this apart from a real copy"
		);
		assert!(result.results[0].error.is_none());

		// Nothing was rewritten: the destination keeps its own content.
		let mut after = ConfigManager::new(
			create_adapter(AgentType::Cursor),
			false,
			Some(&dest_root),
		);
		after.load().unwrap();
		assert_eq!(
			after
				.get_skill("repo-helper")
				.and_then(|s| s.description.clone())
				.as_deref(),
			Some("Existing skill"),
			"an already-present transfer must not overwrite the destination"
		);
	}

	#[test]
	fn reconcile_skill_adds_multiple_agents_to_same_dir() {
		let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
		let temp = tempdir().unwrap();
		let root = temp.path().join("project");
		fs::create_dir_all(&root).unwrap();

		// Setup: Add a skill to Claude within the project
		let mut claude_manager = ConfigManager::new(
			create_adapter(AgentType::Claude),
			false,
			Some(&root),
		);
		claude_manager.load().unwrap();
		let mut skill = Skill::new("shared-skill");
		skill.description = Some("Shared across agents".to_string());
		claude_manager.add_skill(skill).unwrap();

		// Reconcile: add to Cursor and Windsurf within the same project
		let result = reconcile_skill(
			ResourceLocator {
				agent: AgentType::Claude,
				scope: InstallScope::Project,
				project_root: Some(root.clone()),
				name: "shared-skill".to_string(),
			},
			vec![AgentType::Cursor, AgentType::Windsurf],
			vec![],
			false, // confirm
		)
		.unwrap();

		// Both should succeed: Cursor reads the Master natively; Windsurf gets a
		// Referrer to that same Master.
		assert_eq!(result.success_count(), 2);

		assert!(root.join(".aghub/shared-skill/SKILL.md").exists());
		assert!(crate::skills::linker::Linker::is_link(
			&root.join(".windsurf/skills/shared-skill")
		));

		// Verify both agents can see the skill
		let mut cursor_manager = ConfigManager::new(
			create_adapter(AgentType::Cursor),
			false,
			Some(&root),
		);
		cursor_manager.load().unwrap();
		assert!(cursor_manager.get_skill("shared-skill").is_some());

		let mut windsurf_manager = ConfigManager::new(
			create_adapter(AgentType::Windsurf),
			false,
			Some(&root),
		);
		windsurf_manager.load().unwrap();
		assert!(windsurf_manager.get_skill("shared-skill").is_some());
	}

	#[test]
	fn transfer_duplicate_targets_are_deduplicated() {
		let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
		let temp = tempdir().unwrap();
		let source_root = temp.path().join("source");
		let dest_root = temp.path().join("dest");
		fs::create_dir_all(&source_root).unwrap();
		fs::create_dir_all(&dest_root).unwrap();

		let mut source_manager = ConfigManager::new(
			create_adapter(AgentType::Claude),
			false,
			Some(&source_root),
		);
		source_manager.load().unwrap();
		let mut skill = Skill::new("repo-helper");
		skill.description = Some("Copies files".to_string());
		source_manager.add_skill(skill).unwrap();

		// Pass the same target twice
		let result = transfer_skill(
			ResourceLocator {
				agent: AgentType::Claude,
				scope: InstallScope::Project,
				project_root: Some(source_root.clone()),
				name: "repo-helper".to_string(),
			},
			vec![
				InstallTarget {
					agent: AgentType::Cursor,
					scope: InstallScope::Project,
					project_root: Some(dest_root.clone()),
				},
				InstallTarget {
					agent: AgentType::Cursor,
					scope: InstallScope::Project,
					project_root: Some(dest_root.clone()),
				},
			],
		)
		.unwrap();

		// Should only process once due to deduplication
		assert_eq!(result.results.len(), 1);
		assert_eq!(result.success_count(), 1);
	}

	#[test]
	fn ensure_disjoint_rejects_agent_in_both_add_and_remove() {
		// `--add cursor --remove cursor` would net to a silent delete + exit 0
		// without this guard.
		let err = ensure_disjoint(
			&[AgentType::Cursor, AgentType::Claude],
			&[AgentType::Cline, AgentType::Cursor],
		)
		.unwrap_err();
		assert!(
			matches!(err, ConfigError::InvalidConfig(msg) if msg.contains("cursor")),
			"overlap must be rejected naming the agent"
		);

		// Disjoint add/remove sets are fine.
		assert!(ensure_disjoint(
			&[AgentType::Cursor],
			&[AgentType::Cline, AgentType::Claude],
		)
		.is_ok());
	}

	#[cfg(unix)]
	#[test]
	fn copy_collision_is_checked_when_delete_target_is_initially_absent() {
		let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
		let home = tempdir().unwrap();
		let _home = EnvVarGuard::set("HOME", home.path());
		let _config =
			EnvVarGuard::set("XDG_CONFIG_HOME", &home.path().join(".config"));
		let _state = EnvVarGuard::set(
			"XDG_STATE_HOME",
			&home.path().join(".local/state"),
		);

		let source = home.path().join(".claude/skills/solo");
		fs::create_dir_all(&source).unwrap();
		fs::write(
			source.join("SKILL.md"),
			"---\nname: solo\ndescription: private\n---\n",
		)
		.unwrap();

		let result = reconcile_skill(
			ResourceLocator {
				agent: AgentType::Claude,
				scope: InstallScope::Global,
				project_root: None,
				name: "solo".to_string(),
			},
			vec![AgentType::Amp],
			vec![AgentType::Kimi],
			true,
		);

		assert!(
			!home.path().join(".aghub/solo").exists(),
			"preflight must reject before Amp's copy materialises the Master"
		);
		assert!(
			std::fs::symlink_metadata(
				home.path().join(".config/agents/skills/solo")
			)
			.is_err(),
			"the shared Amp/Kimi Referrer slot must remain untouched"
		);
		result.expect_err(
			"a copy that makes an absent delete target see the skill must be refused",
		);
	}

	#[cfg(unix)]
	#[test]
	fn copy_collision_uses_recursive_read_dir_containment() {
		let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
		let home = tempdir().unwrap();
		let _home = EnvVarGuard::set("HOME", home.path());
		let _config =
			EnvVarGuard::set("XDG_CONFIG_HOME", &home.path().join(".config"));
		let _state = EnvVarGuard::set(
			"XDG_STATE_HOME",
			&home.path().join(".local/state"),
		);
		let _hermes = EnvVarGuard::set(
			"HERMES_HOME",
			&home.path().join(".claude/skills"),
		);

		let master = home.path().join(".aghub/solo");
		fs::create_dir_all(&master).unwrap();
		fs::write(
			master.join("SKILL.md"),
			"---\nname: solo\ndescription: shared\n---\n",
		)
		.unwrap();
		let claude_referrer = home.path().join(".claude/skills/solo");
		fs::create_dir_all(claude_referrer.parent().unwrap()).unwrap();
		std::os::unix::fs::symlink(&master, &claude_referrer).unwrap();

		let result = reconcile_skill(
			ResourceLocator {
				agent: AgentType::Claude,
				scope: InstallScope::Global,
				project_root: None,
				name: "solo".to_string(),
			},
			vec![AgentType::Hermes],
			vec![AgentType::Claude],
			true,
		);

		assert!(
			std::fs::symlink_metadata(&claude_referrer).is_ok(),
			"preflight must reject before Claude's sweep can unlink either Referrer"
		);
		assert!(
			std::fs::symlink_metadata(
				home.path().join(".claude/skills/skills/solo")
			)
			.is_err(),
			"Hermes's nested Referrer must never be created"
		);
		result.expect_err(
			"a copy below the delete target's recursively-read root must be refused",
		);
	}

	fn global_target(agent: AgentType) -> InstallTarget {
		InstallTarget {
			agent,
			scope: InstallScope::Global,
			project_root: None,
		}
	}

	/// A `ReconcileSkillPlan` whose only content is the copy set — enough for
	/// the preflight predicates, which read no disk of their own.
	fn plan_copying_to(agents: &[AgentType]) -> ReconcileSkillPlan {
		ReconcileSkillPlan {
			skill: Skill::new("x"),
			source_root: PathBuf::from("/nonexistent/x"),
			keepers: vec![],
			unreadable: vec![],
			copies: agents
				.iter()
				.map(|agent| OperationPlan {
					target: global_target(*agent),
					action: OperationAction::Copy,
				})
				.collect(),
			deletes: vec![],
			dry_run_delete_response: None,
			scope: ResourceScope::GlobalOnly,
			project_root: None,
		}
	}

	/// "Will one of this reconcile's own copies leave the skill where the
	/// delete target reads?" must be answered from BOTH real locations —
	/// where the copy lands (`agent_link_need`) and where the target reads
	/// (`get_skills_paths`) — never from `skill_store_roots` membership.
	///
	/// The half that must still REFUSE is now slot sharing, not master reading:
	/// cline and warp have no private skills dir at global scope, so a copy to
	/// cline writes the very directory warp reads. A copy to claude, by
	/// contrast, writes only `~/.claude/skills` and the store — it reaches
	/// nobody else, which is the whole point of the change.
	#[test]
	fn a_copy_restores_it_asks_the_classifier_not_the_master_root_list() {
		// Reads HOME/XDG through `dirs`; see core AGENTS.md Testing.
		let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
		// Non-empty so the "no copies at all" short-circuit is not what is
		// being measured.
		let plan = plan_copying_to(&[AgentType::Claude]);

		for agent in [AgentType::Amp, AgentType::Kimi] {
			assert!(
				!plan.a_copy_restores_it(&global_target(agent)),
				"{agent:?} reads the XDG dir at global scope, which a copy to \
				 claude never writes — removing it must not be refused"
			);
		}
		assert!(
			!plan.a_copy_restores_it(&global_target(AgentType::OpenCode)),
			"opencode has its OWN referrer dir now; a copy to claude writes \
			 neither it nor the shared slot, so removing opencode is legal"
		);

		// The half that must keep refusing: cline and warp share one directory.
		let to_cline = plan_copying_to(&[AgentType::Cline]);
		assert!(
			to_cline.a_copy_restores_it(&global_target(AgentType::Warp)),
			"a copy to cline writes the very slot warp reads — removing warp \
			 in the same breath cannot take anything away"
		);
	}

	/// The other half: the copy's REFERRER dir, not just the Master.
	///
	/// Amp and Kimi both read and write `~/.config/agents/skills` at global
	/// scope, so a copy to Amp materialises its Referrer at the very entry
	/// Kimi's delete unlinks. Copies run first
	/// (`run_staged_multi_target_mutation`), so `--add amp --remove kimi -g`
	/// reported BOTH rows successful and left AMP — the agent being added —
	/// unable to see the skill. Asking only "is the delete target a
	/// NativeReader of the Master?" answers `false` here (Kimi is NeedsLink at
	/// global) and green-lights exactly that.
	#[test]
	fn a_copy_restores_it_sees_a_referrer_dir_the_copy_and_the_delete_share() {
		// Reads HOME/XDG through `dirs`; see core AGENTS.md Testing.
		let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
		let plan = plan_copying_to(&[AgentType::Amp]);

		assert!(
			plan.a_copy_restores_it(&global_target(AgentType::Kimi)),
			"amp's copy links into ~/.config/agents/skills, which is the SAME \
			 dir kimi reads and the same entry kimi's delete unlinks — this \
			 reconcile cannot be expressed and must be refused, not half-run"
		);
		assert!(
			!plan.a_copy_restores_it(&global_target(AgentType::Claude)),
			"claude reads ~/.claude/skills only — an amp copy touches neither \
			 that nor anything claude reads, so this must stay allowed"
		);
	}

	#[cfg(unix)]
	fn private_slot_for(agent: AgentType, root: &Path) -> PathBuf {
		let dir = create_adapter(agent)
			.target_skills_dir(
				Some(root),
				crate::models::ResourceScope::ProjectOnly,
			)
			.unwrap_or_else(|| {
				panic!("{agent:?} must have a target skills dir")
			});
		assert_eq!(
			crate::skills::removal::slot_reader_count(
				&dir,
				crate::models::ResourceScope::ProjectOnly,
				Some(root),
			),
			1,
			"{agent:?} must have a private slot for this fixture"
		);
		dir
	}

	#[cfg(unix)]
	#[test]
	fn reconcile_skill_removes_remaining_holders_when_source_referrer_is_gone()
	{
		let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
		let temp = tempdir().unwrap();
		let root = temp.path().join("project");
		fs::create_dir_all(&root).unwrap();
		let master = root.join(".aghub/gone-src");
		fs::create_dir_all(&master).unwrap();
		fs::write(
			master.join("SKILL.md"),
			"---\nname: gone-src\ndescription: Test\n---\n\n# Gone Src\n",
		)
		.unwrap();

		let codex_dir = private_slot_for(AgentType::Codex, &root);
		let cursor_dir = private_slot_for(AgentType::Cursor, &root);
		fs::create_dir_all(&codex_dir).unwrap();
		fs::create_dir_all(&cursor_dir).unwrap();
		std::os::unix::fs::symlink(&master, codex_dir.join("gone-src"))
			.unwrap();
		std::os::unix::fs::symlink(&master, cursor_dir.join("gone-src"))
			.unwrap();

		let source = ResourceLocator {
			agent: AgentType::Claude,
			scope: InstallScope::Project,
			project_root: Some(root.clone()),
			name: "gone-src".to_string(),
		};

		let res = reconcile_skill(
			source,
			vec![],
			vec![AgentType::Codex, AgentType::Cursor],
			true,
		)
		.unwrap();

		assert_eq!(res.results.len(), 2);
		assert!(res.results.iter().all(|r| r.success));
		assert!(codex_dir.join("gone-src").symlink_metadata().is_err());
		assert!(cursor_dir.join("gone-src").symlink_metadata().is_err());
		assert!(!master.exists());
	}

	#[cfg(unix)]
	#[test]
	fn reconcile_skill_missing_source_with_adds_refuses_and_names_holders() {
		let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
		let temp = tempdir().unwrap();
		let root = temp.path().join("project");
		fs::create_dir_all(&root).unwrap();
		let master = root.join(".aghub/gone-src");
		fs::create_dir_all(&master).unwrap();
		fs::write(
			master.join("SKILL.md"),
			"---\nname: gone-src\ndescription: Test\n---\n\n# Gone Src\n",
		)
		.unwrap();

		let codex_dir = private_slot_for(AgentType::Codex, &root);
		let cursor_dir = private_slot_for(AgentType::Cursor, &root);
		fs::create_dir_all(&codex_dir).unwrap();
		std::os::unix::fs::symlink(&master, codex_dir.join("gone-src"))
			.unwrap();

		let source = ResourceLocator {
			agent: AgentType::Claude,
			scope: InstallScope::Project,
			project_root: Some(root.clone()),
			name: "gone-src".to_string(),
		};

		let err =
			reconcile_skill(source, vec![AgentType::Cursor], vec![], true)
				.unwrap_err();

		let msg = err.to_string();
		assert!(
			msg.contains("'claude'"),
			"message must contain 'claude': {msg}"
		);
		assert!(
			msg.contains("no longer installed"),
			"message must contain 'no longer installed': {msg}"
		);
		assert!(
			msg.contains("nothing was changed"),
			"message must contain 'nothing was changed': {msg}"
		);
		assert!(
			msg.contains("'codex'"),
			"message must contain 'codex': {msg}"
		);
		assert!(!cursor_dir.join("gone-src").exists());
		assert!(codex_dir.join("gone-src").symlink_metadata().is_ok());
		assert!(master.exists());
	}

	#[cfg(unix)]
	#[test]
	fn reconcile_skill_preview_allows_removal_when_source_is_gone() {
		let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
		let temp = tempdir().unwrap();
		let root = temp.path().join("project");
		fs::create_dir_all(&root).unwrap();
		let master = root.join(".aghub/gone-src");
		fs::create_dir_all(&master).unwrap();
		fs::write(
			master.join("SKILL.md"),
			"---\nname: gone-src\ndescription: Test\n---\n\n# Gone Src\n",
		)
		.unwrap();

		let codex_dir = private_slot_for(AgentType::Codex, &root);
		let cursor_dir = private_slot_for(AgentType::Cursor, &root);
		fs::create_dir_all(&codex_dir).unwrap();
		fs::create_dir_all(&cursor_dir).unwrap();
		std::os::unix::fs::symlink(&master, codex_dir.join("gone-src"))
			.unwrap();
		std::os::unix::fs::symlink(&master, cursor_dir.join("gone-src"))
			.unwrap();

		let source = ResourceLocator {
			agent: AgentType::Claude,
			scope: InstallScope::Project,
			project_root: Some(root.clone()),
			name: "gone-src".to_string(),
		};

		let res = reconcile_skill_preview(
			&source,
			&[],
			&[AgentType::Codex, AgentType::Cursor],
		);

		assert!(
			res.is_ok(),
			"preview must succeed when source is gone: {res:?}"
		);
		assert!(codex_dir.join("gone-src").symlink_metadata().is_ok());
		assert!(cursor_dir.join("gone-src").symlink_metadata().is_ok());
		assert!(master.exists());
	}

	#[cfg(unix)]
	#[test]
	fn reconcile_skill_stale_source_in_removed_fails_only_its_own_row() {
		let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
		let temp = tempdir().unwrap();
		let root = temp.path().join("project");
		fs::create_dir_all(&root).unwrap();
		let master = root.join(".aghub/gone-src");
		fs::create_dir_all(&master).unwrap();
		fs::write(
			master.join("SKILL.md"),
			"---\nname: gone-src\ndescription: Test\n---\n\n# Gone Src\n",
		)
		.unwrap();

		let codex_dir = private_slot_for(AgentType::Codex, &root);
		let cursor_dir = private_slot_for(AgentType::Cursor, &root);
		fs::create_dir_all(&codex_dir).unwrap();
		fs::create_dir_all(&cursor_dir).unwrap();
		std::os::unix::fs::symlink(&master, codex_dir.join("gone-src"))
			.unwrap();
		std::os::unix::fs::symlink(&master, cursor_dir.join("gone-src"))
			.unwrap();

		let source = ResourceLocator {
			agent: AgentType::Claude,
			scope: InstallScope::Project,
			project_root: Some(root.clone()),
			name: "gone-src".to_string(),
		};

		let batch = reconcile_skill(
			source,
			vec![],
			vec![AgentType::Claude, AgentType::Codex, AgentType::Cursor],
			true,
		)
		.unwrap();

		let claude_row = batch
			.results
			.iter()
			.find(|r| r.target.agent == AgentType::Claude)
			.expect("claude row must be present");
		assert!(!claude_row.success, "claude row must fail");
		assert!(
			claude_row
				.error
				.as_deref()
				.unwrap_or("")
				.contains("not found"),
			"claude row error must mention 'not found': {:?}",
			claude_row.error
		);

		let codex_row = batch
			.results
			.iter()
			.find(|r| r.target.agent == AgentType::Codex)
			.expect("codex row must be present");
		assert!(codex_row.success, "codex row must succeed");

		let cursor_row = batch
			.results
			.iter()
			.find(|r| r.target.agent == AgentType::Cursor)
			.expect("cursor row must be present");
		assert!(cursor_row.success, "cursor row must succeed");

		assert!(codex_dir.join("gone-src").symlink_metadata().is_err());
		assert!(cursor_dir.join("gone-src").symlink_metadata().is_err());
	}

	/// A project Master `gone-src` linked from each of `holders`. Returns
	/// `(root, master, slots)`; `slots[i]` holds `holders[i]`'s link.
	#[cfg(unix)]
	fn linked_master_fixture(
		temp: &Path,
		holders: &[AgentType],
	) -> (PathBuf, PathBuf, Vec<PathBuf>) {
		let root = temp.join("project");
		let master = root.join(".aghub/gone-src");
		fs::create_dir_all(&master).unwrap();
		fs::write(
			master.join("SKILL.md"),
			"---\nname: gone-src\ndescription: Test\n---\n\n# Gone Src\n",
		)
		.unwrap();
		let slots = holders
			.iter()
			.map(|&agent| {
				let dir = private_slot_for(agent, &root);
				fs::create_dir_all(&dir).unwrap();
				std::os::unix::fs::symlink(&master, dir.join("gone-src"))
					.unwrap();
				dir.join("gone-src")
			})
			.collect();
		(root, master, slots)
	}

	#[cfg(unix)]
	fn claude_source(root: &Path) -> ResourceLocator {
		ResourceLocator {
			agent: AgentType::Claude,
			scope: InstallScope::Project,
			project_root: Some(root.to_path_buf()),
			name: "gone-src".to_string(),
		}
	}

	// Removal-only, but nothing in `removed` holds it: there is no copy to
	// fall back to, so refuse and name who does hold it.
	#[cfg(unix)]
	#[test]
	fn reconcile_skill_removal_naming_no_holder_refuses_and_names_holders() {
		let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
		let temp = tempdir().unwrap();
		let (root, master, slots) =
			linked_master_fixture(temp.path(), &[AgentType::Codex]);

		let err = reconcile_skill(
			claude_source(&root),
			vec![],
			vec![AgentType::Cursor],
			true,
		)
		.unwrap_err();

		assert!(
			matches!(err, ConfigError::InvalidConfig(_)),
			"must refuse as InvalidConfig, not 404: {err:?}"
		);
		let msg = err.to_string();
		assert!(msg.contains("nothing was changed"), "{msg}");
		assert!(msg.contains("Agents that still hold it: 'codex'"), "{msg}");
		assert!(slots[0].symlink_metadata().is_ok());
		assert!(master.exists());
	}

	// A disabled holder is hidden from the desktop list, so "refresh the
	// list" cannot help; the hint must name it and say what does.
	#[cfg(unix)]
	#[test]
	fn reconcile_skill_refusal_when_only_disabled_agents_hold_it() {
		let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
		let _off = crate::agent_settings::test_override::disable(&["codex"]);
		let temp = tempdir().unwrap();
		let (root, master, slots) =
			linked_master_fixture(temp.path(), &[AgentType::Codex]);
		let cursor_link =
			private_slot_for(AgentType::Cursor, &root).join("gone-src");

		let msg = reconcile_skill(
			claude_source(&root),
			vec![AgentType::Cursor],
			vec![],
			true,
		)
		.unwrap_err()
		.to_string();

		assert!(
			msg.contains(
				"Only agents disabled in aghub's settings still hold it: 'codex'"
			),
			"{msg}"
		);
		assert!(msg.contains("include them in the removal"), "{msg}");
		assert!(!msg.contains("Refresh the list"), "{msg}");
		assert!(cursor_link.symlink_metadata().is_err());
		assert!(slots[0].symlink_metadata().is_ok());
		assert!(master.exists());
	}

	// The fallback swaps only the content source: a holder left out of
	// `removed` still keeps the Master, enabled or disabled.
	#[cfg(unix)]
	#[test]
	fn reconcile_skill_fallback_keeps_master_while_an_unremoved_holder_remains()
	{
		let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
		for disabled in [&[][..], &["cursor"][..]] {
			let _off = crate::agent_settings::test_override::disable(disabled);
			let temp = tempdir().unwrap();
			let (root, master, slots) = linked_master_fixture(
				temp.path(),
				&[AgentType::Codex, AgentType::Cursor],
			);

			let batch = reconcile_skill(
				claude_source(&root),
				vec![],
				vec![AgentType::Codex],
				true,
			)
			.unwrap();

			assert!(
				batch.results.len() == 1 && batch.results[0].success,
				"disabled={disabled:?}: {:?}",
				batch.results
			);
			assert!(slots[0].symlink_metadata().is_err(), "codex unlinked");
			assert!(
				slots[1].symlink_metadata().is_ok(),
				"disabled={disabled:?}: cursor's link must survive"
			);
			assert!(
				master.join("SKILL.md").exists(),
				"disabled={disabled:?}: cursor still holds the Master"
			);
		}
	}

	// The reconcile face of docs/history/core-removal.md
	// #link-unlink-failure-deleted-the-master: the first (exhaustive) row plans
	// cursor's link and the Master; cursor's unlink fails, so the Master stays
	// and both rows say so instead of one reading "not found".
	#[cfg(unix)]
	#[test]
	fn reconcile_skill_keeps_master_when_a_sibling_link_cannot_be_unlinked() {
		use std::os::unix::fs::PermissionsExt;
		let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
		let temp = tempdir().unwrap();
		let (root, master, slots) = linked_master_fixture(
			temp.path(),
			&[AgentType::Claude, AgentType::Cursor],
		);
		let cursor_dir = slots[1].parent().unwrap().to_path_buf();
		fs::set_permissions(&cursor_dir, fs::Permissions::from_mode(0o555))
			.unwrap();
		let writable_anyway = fs::write(cursor_dir.join(".probe"), b"").is_ok();
		let result = reconcile_skill(
			claude_source(&root),
			vec![],
			vec![AgentType::Claude, AgentType::Cursor],
			true,
		);
		fs::set_permissions(&cursor_dir, fs::Permissions::from_mode(0o755))
			.unwrap();
		if writable_anyway {
			return;
		}

		let batch = result.unwrap();
		assert!(master.join("SKILL.md").exists(), "Master must survive");
		assert!(
			fs::metadata(&slots[1]).is_ok(),
			"cursor's link must still resolve"
		);
		assert!(
			batch.results.iter().all(|r| !r
				.error
				.as_deref()
				.unwrap_or("")
				.contains("not found")),
			"no row may claim the skill is gone: {:?}",
			batch.results
		);
	}

	#[cfg(unix)]
	#[test]
	fn reconcile_skill_copy_makes_later_delete_row_dir_coincide_with_protected_target(
	) {
		let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
		let temp = tempdir().unwrap();
		let root = temp.path().join("project");
		fs::create_dir_all(&root).unwrap();

		let master = root.join(".aghub/test-skill");
		fs::create_dir_all(&master).unwrap();
		fs::write(
			master.join("SKILL.md"),
			"---\nname: test-skill\ndescription: Test\n---\n\n# Test\n",
		)
		.unwrap();

		let codex_dir = private_slot_for(AgentType::Codex, &root);
		fs::create_dir_all(&codex_dir).unwrap();
		std::os::unix::fs::symlink(&master, codex_dir.join("test-skill"))
			.unwrap();

		// Claude is the copy target: .claude/skills does NOT exist initially.
		let claude_dir = private_slot_for(AgentType::Claude, &root);
		assert!(!claude_dir.exists(), "Claude dir must start absent");

		// OpenCode is delete target 1.
		let opencode_dir = private_slot_for(AgentType::OpenCode, &root);
		fs::create_dir_all(&opencode_dir).unwrap();
		std::os::unix::fs::symlink(&master, opencode_dir.join("test-skill"))
			.unwrap();

		// Windsurf is delete target 2: make Windsurf's skills dir a symlink pointing to
		// Claude's not-yet-created skills dir.
		let windsurf_dir = private_slot_for(AgentType::Windsurf, &root);
		fs::create_dir_all(windsurf_dir.parent().unwrap()).unwrap();
		std::os::unix::fs::symlink(&claude_dir, &windsurf_dir).unwrap();

		let source = ResourceLocator {
			agent: AgentType::Codex,
			scope: InstallScope::Project,
			project_root: Some(root.clone()),
			name: "test-skill".to_string(),
		};

		let batch = reconcile_skill(
			source,
			vec![AgentType::Claude],
			vec![AgentType::OpenCode, AgentType::Windsurf],
			true,
		)
		.expect("reconcile_skill returns Ok(batch) with per-row outcomes");

		let claude_res = batch
			.results
			.iter()
			.find(|r| r.target.agent == AgentType::Claude)
			.expect("Claude copy row must exist");
		assert!(claude_res.success, "Claude copy must succeed");

		let opencode_res = batch
			.results
			.iter()
			.find(|r| r.target.agent == AgentType::OpenCode)
			.expect("OpenCode delete row must exist");
		assert!(
			opencode_res.success,
			"OpenCode delete must succeed: {:?}",
			opencode_res.error
		);
		assert!(opencode_res.error.is_none());

		let windsurf_res = batch
			.results
			.iter()
			.find(|r| r.target.agent == AgentType::Windsurf)
			.expect("Windsurf delete row must exist");
		assert!(
			!windsurf_res.success,
			"Windsurf delete must fail due to spared check failure"
		);
		assert!(
			windsurf_res
				.error
				.as_deref()
				.unwrap_or("")
				.contains("resolve to the same place on disk"),
			"Windsurf delete error must come from ensure_removals_spare: {:?}",
			windsurf_res.error
		);

		// The protected target (Claude) must survive!
		let claude_skill = claude_dir.join("test-skill");
		assert!(
			claude_skill.exists(),
			"protected target Claude's copied skill must survive"
		);
		// OpenCode's skill was removed by the batch.
		let opencode_skill = opencode_dir.join("test-skill");
		assert!(!opencode_skill.exists(), "OpenCode's skill must be removed");
		// Codex's skill must survive.
		let codex_skill = codex_dir.join("test-skill");
		assert!(codex_skill.exists(), "Codex's skill must survive");
	}

	#[test]
	fn reconcile_skill_delete_preflight_failure_aborts_before_copy() {
		let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
		let temp = tempdir().unwrap();
		let root = temp.path().join("project");
		fs::create_dir_all(&root).unwrap();

		let master = root.join(".aghub/test-skill");
		fs::create_dir_all(&master).unwrap();
		fs::write(
			master.join("SKILL.md"),
			"---\nname: test-skill\ndescription: Test\n---\n\n# Test\n",
		)
		.unwrap();

		let codex_dir = private_slot_for(AgentType::Codex, &root);
		fs::create_dir_all(&codex_dir).unwrap();
		#[cfg(unix)]
		std::os::unix::fs::symlink(&master, codex_dir.join("test-skill"))
			.unwrap();
		#[cfg(windows)]
		std::os::windows::fs::symlink_dir(
			&master,
			codex_dir.join("test-skill"),
		)
		.unwrap();

		let claude_dir = private_slot_for(AgentType::Claude, &root);
		assert!(!claude_dir.exists(), "Claude dir must start absent");

		let source = ResourceLocator {
			agent: AgentType::Codex,
			scope: InstallScope::Project,
			project_root: Some(root.clone()),
			name: "test-skill".to_string(),
		};

		// Zed does not support project skill scope!
		// Reconcile: copy to Claude, delete from Zed.
		let preview_err = reconcile_skill_preview(
			&source,
			&[AgentType::Claude],
			&[AgentType::Zed],
		)
		.expect_err(
			"reconcile_skill_preview must fail due to Zed preflight error",
		);

		let reconcile_err = reconcile_skill(
			source,
			vec![AgentType::Claude],
			vec![AgentType::Zed],
			true,
		)
		.expect_err("reconcile_skill must fail before staging copies");

		// Both must return the EXACT same error (type and message)
		assert_eq!(
			preview_err.to_string(),
			reconcile_err.to_string(),
			"preview and reconcile must return the exact same error message"
		);
		match (&preview_err, &reconcile_err) {
			(
				ConfigError::UnsupportedOperation {
					message: p_msg,
					rejected_targets: p_rej,
				},
				ConfigError::UnsupportedOperation {
					message: r_msg,
					rejected_targets: r_rej,
				},
			) => {
				assert_eq!(p_msg, r_msg);
				assert!(
					p_msg.contains("no project skill config"),
					"error should mention unsupported project skill config: {p_msg}"
				);
				assert_eq!(p_rej, r_rej);
				let rej =
					p_rej.as_ref().expect("rejected_targets must be present");
				assert_eq!(rej.len(), 1);
				assert_eq!(rej[0].agent, "zed");
				assert!(
					rej[0].reason.contains("no project skill config"),
					"reason must contain failure detail: {}",
					rej[0].reason
				);
			}
			_ => panic!(
				"expected ConfigError::UnsupportedOperation, got preview: {preview_err:?}, reconcile: {reconcile_err:?}"
			),
		}

		// Observable outcome: Claude's copied skill must NOT have landed on disk!
		let claude_skill = claude_dir.join("test-skill");
		assert!(
			!claude_skill.exists(),
			"copy to Claude must not land when delete preflight fails"
		);
	}

	#[test]
	fn clone_config_error_preserves_json_error_message() {
		let raw_json_err =
			serde_json::from_str::<serde_json::Value>("{invalid").unwrap_err();
		let expected_msg = raw_json_err.to_string();
		let err = ConfigError::Json(raw_json_err);
		let cloned = crate::skills::removal::batch::clone_config_error(&err);
		let cloned_str = cloned.to_string();
		match &cloned {
			ConfigError::Json(e) => {
				assert_eq!(e.to_string(), expected_msg);
				assert!(!cloned_str.starts_with("Invalid configuration:"));
				assert!(cloned_str.starts_with("JSON parsing error:"));
			}
			other => panic!("expected ConfigError::Json, got {other:?}"),
		}
	}
}
