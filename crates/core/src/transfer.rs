use crate::{
	create_adapter,
	errors::{ConfigError, Result},
	manager::ConfigManager,
	models::{AgentType, McpServer, Skill, SubAgent},
	registry,
};
use log::{info, warn};
use std::collections::HashSet;
#[cfg(test)]
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
	/// Duplicate of `success` under the name the OTHER batch family uses.
	///
	/// `core::batch`'s `AgentOpResultView` calls this field `ok`, and both
	/// families serialize into an envelope with the SAME top-level keys
	/// (`success_count` / `failed_count` / `results`). Each struct's own doc
	/// comment claims to be "the SINGLE place the wire shape is defined" —
	/// true per family, and the collision went unnoticed. A parser written
	/// against `row.ok` therefore read `undefined` for every transfer/reconcile
	/// row and scored SUCCESSES as failures. Emitting both names costs one bool
	/// and makes either spelling correct.
	pub ok: bool,
	/// The target already held this resource; nothing was written. Still a
	/// success row (`success`/`ok` true, no `error`).
	///
	/// Emitted UNCONDITIONALLY — no `skip_serializing_if`. A client talking to
	/// a mixed-version server cannot otherwise tell `false` from "this server
	/// does not report it", and that ambiguity is the whole reason the field
	/// exists.
	pub already_present: bool,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub error: Option<String>,
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

fn validate_target(target: &InstallTarget) -> Result<()> {
	if target.scope == InstallScope::Project && target.project_root.is_none() {
		return Err(ConfigError::InvalidConfig(
			"project_root is required for project targets".to_string(),
		));
	}
	Ok(())
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

fn sub_agent_supported_for_target(target: &InstallTarget) -> Result<()> {
	let descriptor = registry::get(target.agent);
	if descriptor.supports_sub_agent_scope(target_resource_scope(target)) {
		return Ok(());
	}
	Err(ConfigError::unsupported_operation(
		"copy",
		"sub-agent",
		descriptor.id,
	))
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

/// Does the reconcile/transfer SOURCE resource exist? Read-only, mutates
/// nothing.
///
/// A `reconcile --dry-run` (and the implicit dry-run a `--remove` without
/// `--yes` takes) has to answer this itself. Its preview used to be a plain
/// echo of argv, so `--name totally-absent --remove opencode` printed a plan
/// and exited 0, and only the `--yes` run reported `Resource not found` —
/// exactly the sequence an agent uses to check before committing. These are
/// the same three loaders the real reconcile uses, so the existence rule
/// cannot drift between the preview and the mutation.
pub fn ensure_skill_exists(source: &ResourceLocator) -> Result<()> {
	load_source_skill(source).map(|_| ())
}

/// See [`ensure_skill_exists`].
pub fn ensure_mcp_exists(source: &ResourceLocator) -> Result<()> {
	load_source_mcp(source).map(|_| ())
}

/// See [`ensure_skill_exists`].
pub fn ensure_sub_agent_exists(source: &ResourceLocator) -> Result<()> {
	load_source_sub_agent(source).map(|_| ())
}

/// Does the target's installed skill hold the same content as `source_root`?
///
/// Can we PROVE the target now holds the source content?
///
/// Compared by the npx-compatible folder hash, the same digest the lock files
/// use, so "same" means the same thing here as everywhere else in the project.
/// That hash has blind spots by design — it skips symlinks and `.git` /
/// `node_modules`, and refuses above its file/size bounds — and this gates a
/// DESTRUCTIVE step, so a blind spot answers `Unprovable`, never `Landed`.
///
/// Three answers, not two: telling a caller "the content differs" when the real
/// story is "aghub could not look" sends them to reconcile a difference that
/// does not exist.
enum ContentProof {
	Landed,
	Differs,
	/// Why the comparison could not be trusted, phrased for the user.
	Unprovable(String),
}

/// Does this tree hold anything the folder hash cannot see?
///
/// Symlinks are skipped outright by `skill::hash`, so two trees differing ONLY
/// in a symlink hash EQUAL — and the removal that equality authorises then
/// destroys the difference. Verified: a source with a symlink and a Master
/// without one hashed the same, the reconcile reported "2 succeeded", and the
/// symlink was gone.
fn has_unhashed_entries(dir: &Path, depth: usize) -> bool {
	// The hash's OWN bound, not a second guess at it. A private `32` here
	// rejected trees 33-64 deep that the hash accepts, and blamed it on
	// symlinks — a legitimate move refused with a reason that was not true.
	// Past this depth we simply stop looking: the hash refuses the tree on its
	// own, and its refusal produces the accurate "could not be hashed" answer.
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

/// Resolve a path through symlinks even when it does not exist yet.
///
/// A plain `canonicalize` fails on a missing leaf and falls back to the literal
/// path, which is how `~/.gemini/skills` and `~/.claude/skills` read as two
/// different places while `~/.gemini` is a symlink to `~/.claude`. Falling back
/// one level up resolves the part that DOES exist.
fn resolve_through_links(path: PathBuf) -> PathBuf {
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

/// A backing file's identity — NOT its path.
///
/// `canonicalize` collapses symlinks but NOT hard links: two directory entries,
/// one inode. Every writer under here truncates in place (`fs::write`), so a
/// removal through one name empties the other, while comparing resolved path
/// strings sees two unrelated files. This is not exotic — dotfile setups make
/// them deliberately (`cp -l`), and de-duplicators (rdfind, jdupes) make them
/// by accident out of any two identical config files. Verified: with
/// `~/.claude.json` hard-linked to `~/.cursor/mcp.json`, a reconcile that named
/// claude as a COPY TARGET emptied claude's config and reported
/// "2 succeeded, 0 failed".
struct Backing {
	/// `(device, inode)` — identity proper. `None` when the path does not
	/// exist yet, and then there is nothing to alias.
	node: Option<(u64, u64)>,
	path: PathBuf,
}

impl Backing {
	fn of(path: PathBuf) -> Self {
		let path = resolve_through_links(path);
		let node = node_id(&path);
		Self { node, path }
	}

	fn is(&self, other: &Self) -> bool {
		match (self.node, other.node) {
			(Some(a), Some(b)) => a == b,
			// Neither exists: the resolved path is all there is.
			(None, None) => self.path == other.path,
			// One exists and one does not — not one file.
			_ => false,
		}
	}
}

#[cfg(unix)]
fn node_id(path: &Path) -> Option<(u64, u64)> {
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
fn node_id(_path: &Path) -> Option<(u64, u64)> {
	None
}

/// Refuse a removal that would take the resource from something that must
/// SURVIVE this reconcile.
///
/// Two things must survive: every agent we are copying INTO, and the SOURCE we
/// are copying FROM — unless the caller explicitly asked to remove the source
/// too, which is the ordinary "move it" shape.
///
/// The membership is a property of the FILE, not of the agent id, so this
/// compares resolved backing paths and never `AgentType` equality. Two ids land
/// on one file both by design (Claude's project MCP config is `.mcp.json`, and
/// Copilot uses that same file when it exists) and by accident (a symlinked
/// home or an agent-home env override collapses two declared-distinct
/// directories into one).
///
/// Left unchecked it does not merely fail, it DESTROYS: the copy finds an
/// equivalent entry and reports `already_present` — truthfully, it IS the same
/// file — the staged gate only asks whether the copy ERRORED, and the removal
/// then rewrites that one file without the entry. Every row reports success and
/// the resource is gone from everyone. Keying the protection on the agent id
/// missed the whole source half of it: `reconcile --from-agent claude --remove
/// grok` with `~/.grok` symlinked to `~/.claude` deleted the source's only copy
/// and exited 0, because `grok != claude` as an id.
/// What a backing lookup could determine about one target.
///
/// `Option<PathBuf>` conflated the two answers that matter to a PROTECTIVE
/// check: "this agent definitely does not hold it" and "its config would not
/// parse, so I cannot tell". `sub_agent_backing_path` loads a whole
/// `ConfigManager`, which parses MCPs too — an unrelated broken `config.toml`
/// on a sharing agent made the roster guard read it as a non-holder and delete
/// the file both of them read. A guard that exists to prevent data loss must
/// fail CLOSED on the second answer.
enum Backed {
	/// The resolved backing path this target reads.
	At(PathBuf),
	/// Determined: this target holds no such resource.
	Absent,
	/// Undeterminable — treat as a collision wherever that is the safe read.
	Unknown,
}

fn ensure_removals_spare<F>(
	protect: &[Protected],
	removing: &[InstallTarget],
	source_agent: AgentType,
	backing: F,
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
					return Err(ConfigError::InvalidConfig(format!(
						"cannot tell whether '{}' shares the same file as \
						 '{}' — its configuration failed to load, so removing \
						 from '{}' might take it from '{}' as well. Fix that \
						 agent's config, or name it in this reconcile too.",
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
				let (role, remedy) = if kept.target.agent == source_agent {
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
/// anything — so a preview can raise it too.
///
/// The documented pattern for a destructive verb is preview-then-confirm. A
/// preview that green-lights a plan the commit refuses is worse than no
/// preview: the caller learns about the refusal only by attempting the write,
/// and this is the one check that exists to stop data loss.
///
/// One definition, called by both the preview and the commit. Two would drift.
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
/// The named-partners-only list was blind in exactly the case that destroys
/// silently: an agent that shares the backing file but appears NOWHERE in the
/// command is in neither list, so the backing comparison never runs for it.
/// Claude and Copilot both resolve a project MCP to `<root>/.mcp.json`, and
/// `reconcile mcp --remove claude` rewrote that one file and reported success
/// while copilot lost the server too.
///
/// The roster is the REGISTRY, not the installed agents — same source as
/// `skill_holders`, and for the same reason: an agent we cannot see is not an
/// agent that does not read the file.
///
/// Skills deliberately pass `roster: false`. Eight project-scope agents share
/// `<root>/.agents/skills` as their own write dir BY DESIGN (granting to one
/// grants to all), so a roster protect list would refuse every removal from any
/// of them. What a skill removal really takes away is decided by
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

/// The removal rows of ONE reconcile, each with the backing it resolved to at
/// PREFLIGHT, plus the credential half: which of them actually took something
/// out.
///
/// This is the other half of [`ensure_removals_spare`]. That guard refuses a
/// removal whose file something else still reads, and the remedy it prints is
/// "add the sharer to `--remove` as well" — so the shape it sends the caller
/// back with must actually work. It did not: two rows rewriting one file means
/// the first takes the entry out and the second finds nothing to remove, so a
/// reconcile that did exactly what was asked reported `failed_count: 1` and
/// exited 1. A row whose resource a SIBLING ROW of this same command already
/// took is a success — for all three delete arms, `reconcile_skill`'s included,
/// where several project-scope agents share one write dir by design.
///
/// Sharing a backing is NOT on its own that credential. Copilot's project MCP
/// path falls back to claude's `<root>/.mcp.json` while neither that file nor
/// `.github/mcp.json` exists, so `--remove claude --remove copilot` against an
/// absent file made the two rows "siblings" of a deletion that never happened,
/// and both `ResourceNotFound`s were blessed: `success_count: 2` for removing
/// something nobody ever had. Only a row that REALLY emptied the backing
/// vouches for the later rows reading it.
///
/// Scoped to this command's own removal set, so it cannot bless a misspelled
/// agent that simply never held the resource: that agent shares its backing
/// with nobody and its row still errors.
///
/// Backings are resolved at PREFLIGHT, before any row runs, because a
/// sub-agent's backing IS the resource file — once the first row deletes it
/// there is nothing left to compare, and a delete-time answer would be `None`
/// for both.
struct RemovalCredits {
	/// One entry per removal target that resolves to a backing at all, in
	/// input order.
	resolved: Vec<(AgentType, Backing)>,
	/// The agents whose row returned a real deletion.
	credited: Vec<AgentType>,
}

impl RemovalCredits {
	fn new<F>(removing: &[InstallTarget], backing: F) -> Self
	where
		F: Fn(&InstallTarget) -> Backed,
	{
		Self {
			resolved: removing
				.iter()
				.filter_map(|target| match backing(target) {
					Backed::At(path) => Some((target.agent, Backing::of(path))),
					// No key means no credit and no forgiveness — the safe
					// direction for an undeterminable backing.
					Backed::Absent | Backed::Unknown => None,
				})
				.collect(),
			credited: Vec::new(),
		}
	}

	fn backing_of(&self, agent: AgentType) -> Option<&Backing> {
		self.resolved
			.iter()
			.find(|(candidate, _)| *candidate == agent)
			.map(|(_, backing)| backing)
	}

	/// This row really took the resource out, so its backing now vouches for
	/// the later rows that share it.
	fn credit(&mut self, agent: AgentType) {
		if !self.credited.contains(&agent) {
			self.credited.push(agent);
		}
	}

	/// Has an EARLIER row of this same command already emptied the backing this
	/// agent reads?
	fn already_taken(&self, agent: AgentType) -> bool {
		let Some(mine) = self.backing_of(agent) else {
			return false;
		};
		self.credited.iter().any(|credited| {
			self.backing_of(*credited).is_some_and(|took| took.is(mine))
		})
	}
}

/// Read a removal that found nothing as the success it is when a SIBLING ROW of
/// the same reconcile already took the entry out of the file both share — and
/// record a real deletion, so the later rows have something to claim.
///
/// `took` is the caller's OWN answer to "did this row really empty its
/// backing?", because only the caller can tell: `remove_mcp`/`remove_sub_agent`
/// delete or error, so their `Ok` is `true`, while `remove_skill_planned`
/// returns an unexecuted outcome for a removal it deliberately spared
/// (`shared_master_kept`) — success that took nothing, and no credential. The
/// return is always `Ok(false)`: a Delete row is never `already_present`, that
/// vocabulary belongs to the Copy direction.
///
/// One definition for ALL THREE reconcile delete arms (MCP, sub-agent, skill).
/// They answered this differently before — the skill arm blessed EVERY
/// `ResourceNotFound` unconditionally, reporting `success_count: 2` for two
/// agents that had never held the skill — and that is exactly the
/// hand-mirroring that drifts.
fn sibling_already_took_it(
	removed: Result<bool>,
	agent: AgentType,
	credits: &mut RemovalCredits,
) -> Result<bool> {
	match removed {
		Ok(took) => {
			if took {
				credits.credit(agent);
			}
			Ok(false)
		}
		Err(ConfigError::ResourceNotFound { .. })
			if credits.already_taken(agent) =>
		{
			Ok(false)
		}
		Err(error) => Err(error),
	}
}

/// The directory an agent writes ITS OWN skills into, for
/// [`ensure_removals_spare`].
///
/// Deliberately the agent's own dir and NOT the shared `.agents/skills` Master.
/// Several agents linking to one Master is the normal supported state, and
/// removing one agent's link is exactly what `remove_skill_planned` does — it
/// keeps the Master (`shared_master_kept`).
///
/// Two agent IDs whose own skills DIRECTORY is one directory IS a state the
/// world can be in — two of them, in fact. By design: eight project-scope
/// agents write into `<root>/.agents/skills`, and granting to one grants to
/// all. By accident: a symlinked home or an agent-home env override collapses
/// two declared-distinct dirs (`~/.gemini -> ~/.claude` made "remove from
/// gemini" delete claude's private skill and exit 0).
///
/// What this guard refuses is only the pair the caller NAMED — an add and a
/// remove that land in one directory cannot both be honoured, whichever of the
/// two reasons put them there. Unnamed sharers are deliberately unprotected
/// (`protected_targets` is called with `roster: false` for skills): on the
/// shared slot that sharing IS the grant model, and what a removal really takes
/// away is decided by `remove_skill_planned` / `removal::read_effect_after`.
fn skill_backing_dir(target: &InstallTarget) -> Backed {
	// A pure path derivation — it reads no agent config, so a failure here is
	// "this scope has no skills dir for that agent", not "cannot tell".
	match skill_target_dir(target) {
		Ok(dir) => Backed::At(dir),
		Err(_) => Backed::Absent,
	}
}

/// The folder one agent's skill of this name is actually READ FROM, for
/// [`RemovalCredits`].
///
/// A DIFFERENT question from [`skill_backing_dir`] above, and the two must not
/// be swapped: that one answers "which directory does this row rewrite", which
/// is what `ensure_removals_spare` needs. The credential needs "what does this
/// row TAKE" — and what a skill removal takes can be the shared Master, which
/// lives in NO agent's write dir. Keyed on write dirs, an exhaustive removal
/// (every reader named, so the Master goes) had its first row delete the Master
/// and every later row report `RESOURCE_NOT_FOUND` for a skill that command had
/// just taken from it.
///
/// `skill_root` prefers `canonical_path`, so a Referrer and the Master it points
/// at are ONE backing — which is exactly what that first row emptied.
///
/// `None` when this target has no such skill: it never held it, so no sibling's
/// deletion can vouch for its row.
fn skill_entry_backing(target: &InstallTarget, name: &str) -> Backed {
	let mut manager = build_manager(target);
	// Same reason as `sub_agent_backing_path`: `load()` parses this agent's
	// MCPs too, so an unrelated malformed config must not read as "no skill".
	if ensure_loaded(&mut manager).is_err() {
		return Backed::Unknown;
	}
	match manager
		.get_skill(name)
		.and_then(crate::skills::removal::skill_root)
	{
		Some(root) => Backed::At(root),
		None => Backed::Absent,
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
/// Sub-agents have no `config_path()` — each one IS its own `.md` file — so the
/// backing key is the resolved path of the same-named file this target actually
/// sees. That is also why enumerating the descriptors' declared directories and
/// finding them all distinct proves nothing: two dirs that differ on paper are
/// one dir behind a symlinked ancestor (deliberately allowed — see
/// `agents/src/sub_agents.rs`, where only symlinked LEAVES are refused) or an
/// agent-home env override. Ask the filesystem, not the table.
///
/// `None` when the target has no such sub-agent — the ordinary copy case, and
/// not a collision: two targets resolving to one directory either both see the
/// file or neither does.
fn sub_agent_backing_path(target: &InstallTarget, name: &str) -> Backed {
	let mut manager = build_manager(target);
	// `load()` parses this agent's MCPs too, so an unrelated malformed config
	// lands here. That is NOT "no such sub-agent" — reading it as one let a
	// roster-protected agent drop out of the guard and lose the file it shared
	// with the removal target.
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
/// Equivalence, not mere name collision: unlike a skill (one shared Master), a
/// same-named MCP entry can hold a completely different command or URL, and
/// reporting success for that would claim a copy that never happened while the
/// target keeps serving something else. A differing entry is still a hard
/// conflict — `update_mcp` is the seam for changing one.
///
/// Shared by `transfer_mcp` and `reconcile_mcp`'s Copy arm so the two cannot
/// disagree about what "already there" means; they disagreeing is the defect
/// this fixes.
///
// ponytail: equivalence compares the in-memory model. A dialect whose writer
// drops a field aghub does model will re-read unequal, so a repeat transfer
// into it still errors. Upgrade path: compare the dialect-projected form.
fn copy_mcp_into(target: &InstallTarget, mcp: &McpServer) -> Result<bool> {
	let mut manager = build_manager(target);
	ensure_loaded(&mut manager)?;
	if let Some(existing) = manager.get_mcp(&mcp.name) {
		// `config_source` is load-time provenance, not part of the value.
		let equivalent = existing.enabled == mcp.enabled
			&& existing.transport == mcp.transport
			&& existing.timeout == mcp.timeout;
		if equivalent {
			return Ok(true);
		}
		return Err(ConfigError::resource_exists("MCP server", &mcp.name));
	}
	manager.add_mcp(mcp.clone())?;
	Ok(false)
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
		let equivalent = existing.description == sub_agent.description
			&& existing.instruction == sub_agent.instruction;
		if equivalent {
			return Ok(true);
		}
		return Err(ConfigError::resource_exists("sub_agent", &sub_agent.name));
	}
	manager.add_sub_agent(sub_agent.clone())?;
	Ok(false)
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
/// Prefers `canonical_path` (the real master location for a symlinked skill),
/// falls back to `source_path`. Both go through the same tilde-expansion
/// (`resolve_skill_file`). When the resolved path is a `SKILL.md` file the
/// PARENT directory is returned (the skill folder); a directory is returned
/// as-is. Returns `None` only when the skill records no path at all.
///
/// This is the single shared resolver reused by `resolve_skill_root` (which
/// adds an existence check) and the layout-aware removal planner, so the
/// "canonical FILE path → take PARENT" rule (spec) lives in exactly one place.
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

/// Reject a transfer that names no destinations. An empty `--to` is almost
/// always a mistake; without this guard `transfer_*` returns `Ok([])` and the
/// caller exits 0 having copied nothing (finding #4). Both surfaces route
/// through `transfer_*`, so the guard lives here once.
fn ensure_destinations(destinations: &[InstallTarget]) -> Result<()> {
	if destinations.is_empty() {
		return Err(ConfigError::InvalidConfig(
			"no destination agents given; specify at least one target"
				.to_string(),
		));
	}
	Ok(())
}

/// Reject a reconcile that names the same agent in both `--add` and `--remove`.
/// The add loop runs before the remove loop, so `--add X --remove X` would
/// silently net to a delete and exit 0. Both surfaces (CLI + API) route through
/// `reconcile_*`, so the guard lives here once.
/// Preconditions every `reconcile_*` shares.
///
/// The destructive half is the point: a reconcile that REMOVES needs explicit
/// confirmation, and that policy lives HERE so the CLI's `--yes` and the API's
/// `confirm` are two adapters over one rule instead of two hand-kept copies.
/// The CLI still previews before it ever calls in, so from that surface this is
/// a backstop; for an API client it is the only gate there is.
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

/// Reject an agent that appears in BOTH the add and remove sets.
///
/// Public so a PREVIEW can apply it without touching anything: a dry-run used
/// to approve `--add opencode --remove opencode` and only the `--yes` run hit
/// this, which is the wrong order for a check whose entire job is telling the
/// caller what the commit will do. (`confirm = false` is NOT a dry-run switch —
/// it is the "refuses removals without confirmation" gate, and an add-only
/// reconcile with it still WRITES. That is why the preview needs read-only
/// preflights like this one rather than a planner call.)
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
	// Deduplicate BEFORE any row exists, so no `RemovalCredits` receipt can be
	// issued that a later duplicate of the same target then spends on itself:
	// `--remove claude --remove claude` used to delete once, credit the
	// backing, and let row two's `ResourceNotFound` be forgiven by row one —
	// reporting two successes for one deletion. This is the single place rows
	// are built, so preview and commit dedupe identically.
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
	// Keep the VARIANT when every row refused for the same domain reason.
	// Aggregating a batch is a transport concern and must not relabel the
	// answer: `delete skills x -a cursor` and `reconcile skills x --remove
	// cursor` are one refusal ("cursor reads it from the shared master"), and
	// flattening the second to `InvalidConfig` sent the API 400 for one and 422
	// for the other, so a client branching on `UNSUPPORTED_OPERATION` saw the
	// same domain error land in its "bad parameters" arm.
	let all_unsupported = !error.failures.is_empty()
		&& error
			.failures
			.iter()
			.all(|f| matches!(f.reason, ConfigError::UnsupportedOperation(_)));
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
		ConfigError::UnsupportedOperation(message)
	} else {
		ConfigError::InvalidConfig(message)
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
	// Before ANY write: an add and a remove that resolve to the same file
	// cannot both be honoured, and attempting it deletes from both. The protect
	// list is the ROSTER, not just the agents this command named — see
	// `protected_targets`.
	let removing: Vec<InstallTarget> =
		deletes.iter().map(|plan| plan.target.clone()).collect();
	let protect =
		protected_targets(&copies, &source, source_removed, &removing, true);
	ensure_removals_spare(&protect, &removing, source.agent, mcp_backing_path)?;
	// The remedy that refusal prints — "add the sharer to --remove too" — has
	// to lead somewhere: the row that finds the entry already gone because a
	// sibling row rewrote the shared file first is a success. It is a success
	// only once that sibling row has actually taken it, so the backings are
	// resolved here and the credentials are earned below.
	let mut credits = RemovalCredits::new(&removing, mcp_backing_path);
	// The copy targets, remembered for the RE-CHECK inside the delete arm. Some
	// resolvers are existence-dependent — Copilot's project path is `.mcp.json`
	// when that file exists, `.github/mcp.json` when only that one does, and
	// `.mcp.json` again when NEITHER exists — so the preflight above is a
	// SNAPSHOT: a copy that creates `.github/mcp.json` flips the delete target
	// onto it afterwards, and the preflight saw two different files.
	// Re-resolving at delete time is the only point where the paths are
	// settled.
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
			let outcome = (|| -> Result<bool> {
				match plan.action {
					// Same helper as `transfer_mcp` — the two must not disagree
					// about what "already there" means.
					OperationAction::Copy => copy_mcp_into(&plan.target, &mcp),
					OperationAction::Delete => {
						// Re-check now that every copy has run: this target
						// may have been resolved onto a file one of them just
						// created. Copilot's project path is `.mcp.json` when
						// that file exists, `.github/mcp.json` when only that
						// one does, and `.mcp.json` again when neither exists,
						// so the preflight above is only a SNAPSHOT.
						ensure_removals_spare(
							&protect,
							std::slice::from_ref(&plan.target),
							source.agent,
							mcp_backing_path,
						)?;
						let mut manager = build_manager(&plan.target);
						ensure_loaded(&mut manager)?;
						// `remove_mcp` errors when the entry is not there, so
						// its `Ok` is a real deletion — the credential.
						sibling_already_took_it(
							manager.remove_mcp(&source.name).map(|()| true),
							plan.target.agent,
							&mut credits,
						)
					}
				}
			})();
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
			sub_agent_supported_for_target(&plan.target)
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
	// Same shape as the MCP guard above, and the same destruction when it is
	// missing: the copy finds its OWN file, reports `already_present`
	// truthfully, the staged gate only asks whether the copy ERRORED, and the
	// delete then removes the one file both targets were reading. Two success
	// rows, resource gone from both.
	let removing: Vec<InstallTarget> =
		deletes.iter().map(|plan| plan.target.clone()).collect();
	let protect =
		protected_targets(&copies, &source, source_removed, &removing, true);
	ensure_removals_spare(&protect, &removing, source.agent, |target| {
		sub_agent_backing_path(target, &source.name)
	})?;
	// Same reason as the MCP arm: the refusal above tells the caller to add the
	// sharer to --remove, and that command has to be able to succeed. Here the
	// preflight is the ONLY place the backings can be resolved — the backing IS
	// the file, so after the first row deletes it neither target resolves to
	// anything.
	let mut credits = RemovalCredits::new(&removing, |target| {
		sub_agent_backing_path(target, &source.name)
	});
	// …and that preflight is only a SNAPSHOT, which for sub-agents is barely a
	// guard at all: the backing IS the resource file, so two agents sharing one
	// directory both resolve to `None` until a copy writes it. The real check is
	// the delete-time one below.
	let report = crate::batch::run_staged_multi_target_mutation(
		&copies,
		&deletes,
		|plan| {
			validate_target(&plan.target)?;
			if plan.action == OperationAction::Copy {
				sub_agent_supported_for_target(&plan.target)?;
			}
			Ok(())
		},
		|plan| {
			let outcome = (|| -> Result<bool> {
				match plan.action {
					// Same helper as `transfer_sub_agent`.
					OperationAction::Copy => {
						copy_sub_agent_into(&plan.target, &sub_agent)
					}
					OperationAction::Delete => {
						// Re-check now that every copy has run: the file this
						// target resolves to may be one a copy just created.
						ensure_removals_spare(
							&protect,
							std::slice::from_ref(&plan.target),
							source.agent,
							|target| {
								sub_agent_backing_path(target, &source.name)
							},
						)?;
						let mut manager = build_manager(&plan.target);
						ensure_loaded(&mut manager)?;
						// Same as the MCP arm: `Ok` only ever follows a real
						// delete, so it is the credential.
						sibling_already_took_it(
							manager
								.remove_sub_agent(&source.name)
								.map(|()| true),
							plan.target.agent,
							&mut credits,
						)
					}
				}
			})();
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
				// No pre-check: `add_skill_from_path` already owns the
				// already-present decision, and it is the only code that knows
				// which kind of "already there" this is. A `get_skill().is_some()`
				// guard here refused the two cases that are genuine no-ops —
				// the target reads the shared `.agents` Master (cursor, cline,
				// codex, opencode, warp all do), or it already holds a valid
				// link to it — while `reconcile --add` accepted exactly those.
				// Same operation, opposite verdict.
				//
				// A REAL foreign occupant (a same-named directory that is not a
				// link to the Master) is still refused, by
				// `add_skill_from_path_universal`. Content is deliberately not
				// compared: that is the documented `add_skill_from_path`
				// contract, shared with `aghub add skill --from`.
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
/// One extra scan, taken only to answer "will anyone still be reading the
/// Master after this reconcile?" — the per-agent removal planner cannot see
/// that, because a NativeReader leaves no artifact for it to count.
///
/// It walks the skill dirs DIRECTLY instead of going through
/// `load_all_agents`, and that is the point rather than an optimisation. A full
/// config load also parses MCPs and sub-agents and gives up on the FIRST error,
/// so one unparseable `.mcp.json` erased an agent's skills from this answer
/// while nothing about its skills was broken — the Master was then garbage
/// collected out from under a real holder. Patching that by treating any load
/// failure as "might hold it" only traded the data loss for the opposite
/// failure: an agent that cannot hold skills at all vetoed every removal in the
/// scope, with no override. Asking the filesystem the skill question directly
/// has neither failure mode.
///
/// FAIL-CLOSED on what is left: a read dir that EXISTS but cannot be listed
/// counts as a holder, because "holds nothing" and "cannot tell" are the same
/// empty list. An ABSENT read dir really does hold nothing and does not count —
/// widen that and every uninstalled agent becomes a holder, `exhaustive` is
/// never true again, and Master GC silently stops happening forever.
///
/// Naming an unreadable agent in `removed` makes the reconcile exhaustive
/// again. That is deliberate: an explicit "take it from that one too" is the
/// only thing that can authorize a collection we cannot verify.
///
/// Otherwise the direction leaves an orphan Master behind — `doctor` reports it
/// as `orphanMaster` and it is reclaimable. Recoverable noise beats
/// unrecoverable data loss; do not "fix" this back to fail-open.
fn skill_holders(
	name: &str,
	source: &ResourceLocator,
) -> (Vec<AgentType>, Vec<&'static str>) {
	let scope = match source.scope {
		InstallScope::Global => crate::models::ResourceScope::GlobalOnly,
		InstallScope::Project => crate::models::ResourceScope::ProjectOnly,
	};
	let mut holders = Vec::new();
	let mut unreadable = Vec::new();
	for descriptor in registry::iter_all() {
		let Ok(agent) = descriptor.id.parse::<AgentType>() else {
			continue;
		};
		// Through the ADAPTER, never the descriptor: the skills-path test
		// override lives only there, and bypassing it would answer about the
		// developer's real home instead of the fixture.
		let dirs = create_adapter(agent)
			.get_skills_paths(source.project_root.as_deref(), scope);
		// FAIL-CLOSED, and the direction is the whole point: `Err` means a
		// read dir EXISTS and could not be listed, and "holds nothing" and
		// "cannot tell" are the same empty list. An ABSENT dir is not an error
		// — it really does hold nothing, and widening that would make every
		// uninstalled agent a holder.
		match crate::skills::discovery::load_skills_from_dirs(&dirs) {
			Ok(skills) => {
				if skills.iter().any(|s| s.name == name) {
					holders.push(agent);
				}
			}
			Err(error) => {
				// SAY so even when the answer is safe anyway. Counting an
				// unreadable agent as a holder keeps the Master, but it also
				// makes the whole thing silent: the removal simply stops being
				// exhaustive and proceeds. The error names the path it failed
				// on (`discovery::at_path`), which is the only thing the user
				// can act on.
				log::warn!(
					"cannot read agent '{}' skills, counting it as a holder \
					 of '{name}': {error}",
					descriptor.id
				);
				unreadable.push(descriptor.id);
				holders.push(agent);
			}
		}
	}
	(holders, unreadable)
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
	/// Does this reconcile drop the skill from EVERY agent that holds it? Then
	/// the shared Master has no remaining reader and must go with it. Removing
	/// it per-agent instead refuses on every target (an agent reading the
	/// Master directly has nothing agent-specific to take) and leaves the
	/// Master orphaned — and the desktop's manage-agents dialog allows exactly
	/// that shape: deselect every agent, no adds.
	exhaustive: bool,
	/// Holders this reconcile does NOT remove: the reason the Master stays, and
	/// the only thing a refused caller can actually act on.
	keepers: Vec<&'static str>,
	/// Agents whose skill dirs could not be listed at all.
	unreadable: Vec<&'static str>,
	copies: Vec<OperationPlan>,
	deletes: Vec<OperationPlan>,
}

fn plan_reconcile_skill(
	source: &ResourceLocator,
	added: &[AgentType],
	removed: &[AgentType],
) -> Result<ReconcileSkillPlan> {
	let skill = load_source_skill(source)?;
	let source_root = resolve_skill_root(&skill)?;

	// The holder scan walks every agent's whole skill tree, so it runs only
	// where its answer can change the outcome. An add alone already guarantees
	// the Master gains a reader, and no removal has nothing to collect:
	// `exhaustive` is false either way without asking.
	let collectable = !removed.is_empty() && added.is_empty();
	let (holders, unreadable) = if collectable {
		skill_holders(&skill.name, source)
	} else {
		(Vec::new(), Vec::new())
	};
	let exhaustive =
		collectable && holders.iter().all(|held| removed.contains(held));

	// Naming the unreadable agent is NOT enough authority to collect the
	// Master. Counting it as a holder keeps `exhaustive` false while it is
	// unnamed — but the moment the caller names it, `exhaustive` flips true and
	// the batch will happily delete the Master from a READABLE row while the
	// unreadable agent's own row is still ahead of it: its preflight fails OPEN
	// on a config it cannot load, and rows use attempt-all semantics, so
	// ordering saves nothing. The result is the Master gone and an opaque copy
	// or Referrer left behind — the exact data loss the holder scan exists to
	// prevent, reached by the one input that was supposed to authorize it.
	//
	// "Take it from that one too" can only be honoured by a batch that CAN take
	// it from that one. Refuse before any row runs; fixing the directory is the
	// way through, and it is recoverable.
	if exhaustive && !unreadable.is_empty() {
		return Err(ConfigError::InvalidConfig(format!(
			"cannot decide whether removing '{}' leaves the shared \
			 .agents/skills master unread: agent(s) '{}' could not be read \
			 (skills directory unreadable), and an agent aghub cannot read may \
			 still be holding it — naming it in --remove cannot authorize a \
			 collection this run is unable to carry out on it. Fix or remove \
			 those configs, then re-run.",
			skill.name,
			unreadable.join("', '")
		)));
	}

	let (copies, deletes) = reconcile_plans(
		added.to_vec(),
		removed.to_vec(),
		source.scope,
		source.project_root.clone(),
	);
	Ok(ReconcileSkillPlan {
		skill,
		source_root,
		exhaustive,
		// Unreadable agents get their own clause in the refusal, so leaving
		// them out here keeps a message from naming the same agent twice.
		keepers: holders
			.iter()
			.filter(|held| {
				!removed.contains(held) && !unreadable.contains(&held.as_str())
			})
			.map(|held| held.as_str())
			.collect(),
		unreadable,
		copies,
		deletes,
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

	/// Refuse an END STATE that cannot exist, BEFORE the first write.
	///
	/// Removing an agent that reads the shared Master directly takes nothing
	/// away while the Master stays — and whether it stays is a fact about the
	/// WHOLE reconcile, not about this one row. So the copies used to land on
	/// disk first and the delete row failed afterwards, leaving a half-applied
	/// reconcile.
	///
	/// This is not a second implementation of that verdict: it asks the same
	/// planner the same question with the same `exhaustive`, just earlier. It
	/// runs for EVERY delete row, including a reconcile with no copies at all —
	/// the unreachable end state is what is refused, not the pairing with a
	/// copy.
	fn preflight_delete(&self, target: &InstallTarget) -> Result<()> {
		let mut manager = build_manager(target);
		// Fail OPEN on a config this row cannot even read: that is this row's
		// own problem, the mutate arm fails it identically, and escalating it
		// here would abort the whole batch — an unparseable `.mcp.json` would
		// cancel a perfectly good copy to a DIFFERENT agent.
		if ensure_loaded(&mut manager).is_err() {
			return Ok(());
		}
		// A dry-run plans under the guard this reconcile already holds and
		// writes nothing.
		let shared_master_kept = match manager.remove_skill_planned(
			&self.skill.name,
			self.exhaustive,
			true, // dry_run
			true,
		) {
			Ok(outcome) => outcome.plan.shared_master_kept,
			// The copy may make an absent target present before its delete row
			// runs, so absence only answers the on-disk half of this preflight.
			Err(ConfigError::ResourceNotFound { .. }) => false,
			Err(error) => return Err(error),
		};

		// Two ways this row takes nothing away, and only the first is visible
		// on the disk the preflight can see.
		//
		// The first is READ OFF the dry-run above rather than re-asked:
		// `remove_skill_planned` folds its own "this removal takes nothing
		// away" verdict into `shared_master_kept` before returning an
		// unexecuted outcome, so this inherits the commit's exact answer —
		// including its `--all-agents`/single-agent split — by construction. A
		// second call here re-derived it from the target's own read dirs alone
		// and could not see that split, which is precisely how a preflight
		// green-lights a row the commit then refuses.
		if shared_master_kept || self.a_copy_restores_it(target) {
			return Err(self.refuse_shared_master(target.agent.as_str()));
		}
		Ok(())
	}

	/// Will one of THIS reconcile's own copies leave the skill sitting in a
	/// directory `target` READS?
	///
	/// Preflight runs before the first write, so the probe above looks at a
	/// disk where the entries this reconcile is about to create do not exist
	/// yet. Reasoned about rather than observed because
	/// `run_staged_multi_target_mutation` runs EVERY preflight before ANY copy;
	/// without it, "add windsurf, remove cursor" on a cursor-private skill
	/// passed preflight, wrote the Master, deleted cursor's own folder and
	/// reported BOTH rows successful while cursor still saw the skill.
	///
	/// Both halves come from ONE home. WHERE a copy lands is
	/// [`Self::copy_entry_dirs`] (`agent_link_need` — root AGENTS.md "Link
	/// decision"); WHERE the delete target reads is its own
	/// `get_skills_paths`. Asking the classifier about the DELETE target alone
	/// — "is it a NativeReader of the Master?" — was only half the question,
	/// and the missing half is a data-loss shape, not a false refusal: Amp and
	/// Kimi BOTH read and write `~/.config/agents/skills` at global scope
	/// (`macros.rs` maps that path as read AND write for them), so
	/// `--add amp --remove kimi -g` planned a copy whose Referrer slot IS the
	/// entry Kimi's delete then unlinks. Copies run first, so both rows
	/// reported success and Amp — the agent the user was ADDING — ended up
	/// unable to see the skill. Comparing DIRS (not the delete's planned paths)
	/// is what catches it at preflight: the shared entry need not exist on disk
	/// yet for the collision to be certain.
	///
	/// Do NOT re-derive either half from `skill_store_roots` membership:
	/// that list includes the XDG `~/.config/agents/skills`, which Amp and Kimi
	/// read at global scope but no copy to a DIFFERENT agent ever writes
	/// (global installs materialise `~/.agents/skills`). Doing so refused
	/// `reconcile --add claude --remove amp -g` outright — batch preflight, so
	/// nothing was written at all.
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
	/// canonicalized so two spellings of one directory compare equal.
	///
	/// Installs are symlink-only, so a copy touches two places: it materialises
	/// the shared Master (`master_store_dir` — the same resolution
	/// `universal_install_prep` uses), and, for a `NeedsLink` agent, it links
	/// that agent's own skills dir to it. A `NativeReader` gets no link; the
	/// Master already IS one of its read dirs.
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
	/// takes nothing away."
	///
	/// Naming WHY the master stays is not decoration: without it the message
	/// names the delete target while the real reason is a different agent, and
	/// the user has nothing to act on.
	fn refuse_shared_master(&self, agent: &str) -> ConfigError {
		let ConfigError::UnsupportedOperation(mut message) =
			ConfigError::unsupported_operation(
				"remove for this agent alone",
				"skill it reads from the shared master",
				agent,
			)
		else {
			unreachable!("unsupported_operation builds UnsupportedOperation")
		};
		if !self.keepers.is_empty() {
			message.push_str(&format!(
				"; the shared master is still read by '{}'",
				self.keepers.join("', '")
			));
		} else if !self.copies.is_empty() {
			// No keepers were computed because an add short-circuits the holder
			// scan — the add IS the reason. Saying so is the difference between
			// a dead end and "drop the --add, or remove that agent too".
			message.push_str(
				"; this reconcile also adds the skill to another agent, so the \
				 shared master stays",
			);
		}
		if !self.unreadable.is_empty() {
			message.push_str(&format!(
				"; cannot verify agent '{}' (skills directory unreadable), so \
				 the shared master must stay",
				self.unreadable.join("', '")
			));
		}
		ConfigError::UnsupportedOperation(message)
	}
}

/// Everything a `reconcile_skill` would refuse, without touching anything.
///
/// Exists so a PREVIEW cannot green-light a plan the commit rejects — the CLI's
/// dry-run is the step an agent takes to decide whether to commit. It runs the
/// SAME per-row preflight over the SAME plan and reports it through the same
/// `batch_preflight_error`, so the preview and the commit are indistinguishable
/// down to the error code and message.
///
/// Advisory by construction: it takes no mutation lock, and the committing call
/// re-runs all of it under one. That is the right split — a preview that locked
/// would serialize read-only inspection against real work.
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
	// ONE guard for the whole reconcile, taken before the first READ that
	// decides a write. The holder scan and the preflight dry-runs are exactly
	// the "state read that decides the mutation" the lock exists for: computed
	// outside it, another aghub process could link a fresh Referrer into the
	// Master between the scan and the writes, and this flow would collect a
	// Master that had just gained a reader. Reentrant, so every
	// `guard_and_reload` underneath is free. It serializes aghub against aghub
	// only — `manager/skill.rs`'s executing refusal stays as the backstop for
	// anything else that touches these dirs.
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
	// Nothing we are copying INTO — and not the source either, unless the
	// caller asked to remove it — may share a skills DIRECTORY with something
	// we are removing from. A DIFFERENT question from `plan.preflight`, which
	// refuses an unreachable END STATE: this one refuses a target whose backing
	// dir another row in the same batch is about to delete. Both still asked.
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
	// Same credential as the MCP and sub-agent arms. Two removal rows really can
	// be one entry — eight project-scope agents share `<root>/.agents/skills`,
	// and an exhaustive removal takes the Master every named reader links to —
	// so the second row finding nothing left is a success. Only once an EARLIER
	// row actually took that entry: this arm used to forgive every
	// `ResourceNotFound`, so removing from two agents that had never held the
	// skill exited 0 reporting two deletions with the disk untouched.
	let mut credits = RemovalCredits::new(&removing, |target| {
		skill_entry_backing(target, &plan.skill.name)
	});
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
					// ONE guard across check → write → rollback, which is what
					// `mutation_guard`'s own doc says it is for. The manager's
					// internal guard is released the moment
					// `add_skill_from_path` returns, so the proof below and the
					// rollback after it used to run unlocked — able to unlink a
					// referrer another aghub process had just recreated. It is
					// reentrant, so the inner one costs nothing.
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

					// When this reconcile also REMOVES, the copy has to prove
					// the source content actually landed — not merely that the
					// call succeeded. `wrote_master` is the ONLY outcome that
					// proves it; both other outcomes can leave the source
					// unwritten:
					//
					// - `materialize_universal_master` preserves a pre-existing
					//   Master rather than overwriting it (deliberate: see
					//   `add_skill_from_path`). A target with no skill at all is
					//   then LINKED to an already-present Master holding DIFFERENT
					//   content — success, `already_installed` false, not one byte
					//   of the source written.
					// - A NativeReader that already reads such a Master reports
					//   `already_installed` — truthfully, it does hold a skill by
					//   that name — and that name is all the two have in common.
					//
					// Paired with the delete, the source content is then gone
					// while a same-named skill remains, so nothing looks wrong.
					// Hence: whatever the call reported, if we did not write the
					// Master ourselves, PROVE the content is there before removing
					// it from the source.
					//
					// Only the removing case is tightened. A plain
					// `transfer`/`--add` writes nothing away, so preserving the
					// Master there stays the documented behaviour.
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
								 (an existing .agents/skills master) whose \
								 content differs from the source, and aghub \
								 preserves an existing master rather than \
								 overwriting it — so the copy did not carry \
								 the source content over. Reconcile the master \
								 first, or drop the --remove.",
							)),
							// NOT folded into "differs": sending someone to
							// reconcile a difference that may not exist is its
							// own wrong answer.
							ContentProof::Unprovable(reason) => Some(format!(
								"aghub cannot PROVE the target now holds the \
								 source content — {reason}. It will not remove \
								 content it cannot account for. Drop the \
								 --remove, or copy the folder yourself and \
								 verify it."
							)),
						};
						if let Some(why) = why {
							// Undo THIS call's own work before refusing.
							// `add_skill_from_path` has already linked the
							// target to the master, so returning Err here
							// left the row saying `success: false` while the
							// target had gained a skill it never had, holding
							// content nobody asked to copy.
							// `created_referrer_dirs` is the materializer's
							// own receipt and exists for exactly this — its
							// doc: "a caller that cannot roll back is the bug
							// this exists to prevent". The master is NOT
							// touched: we only get here when we did not write
							// it, so it belongs to whoever did.
							//
							// Under `_copy_guard` below, which spans
							// check → write → rollback: unlocked, this could
							// unlink a referrer another aghub process had just
							// recreated.
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
					let mut manager = build_manager(&row.target);
					ensure_loaded(&mut manager)?;
					// `remove_skill_planned` REFUSES an executing removal that
					// would take nothing while keeping a shared Master, so that
					// shape arrives here as an `Err` and never as an `Ok` to
					// re-inspect. Do not add a second copy of the check here: it
					// is unreachable, and a reader who spots the duplicate may
					// delete the wrong one of the two.
					//
					// `executed` alone is NOT the credential:
					// `RemovalOutcome::commit` sets it for the whole execute
					// branch even when every `remove_dir_all` returned
					// `EACCES` (its own doc says so), so a row that left the
					// Master on disk reported a deletion AND vouched for the
					// sibling rows reading that same Master — exit 0 with the
					// skill still there. `failed_paths` is the truthful half,
					// and a row that could not empty its backing is a FAILED
					// row: reconcile has no `outcome` field to carry `delete`'s
					// `partial`, so `Err` is the only honest carrier. Never
					// `ResourceNotFound` — that is the one variant
					// `sibling_already_took_it` forgives.
					//
					// What remains a credential-free `Ok(false)`: the
					// `spared_everything` preview (kept because a peer links
					// into this agent's own dir) leaves `executed` false. The
					// executing "takes nothing away" shape never arrives here
					// at all — `remove_skill_planned` refuses it.
					sibling_already_took_it(
						manager
							.remove_skill_planned(
								&plan.skill.name,
								plan.exhaustive,
								false,
								true,
							)
							.and_then(|outcome| {
								if outcome.failed_paths.is_empty() {
									return Ok(outcome.executed);
								}
								Err(ConfigError::InvalidConfig(format!(
									"failed to remove skill '{}' for agent \
									 '{}': {} path(s) could not be deleted: {}",
									plan.skill.name,
									row.target.agent.as_str(),
									outcome.failed_paths.len(),
									outcome
										.failed_paths
										.iter()
										.map(|path| path.display().to_string())
										.collect::<Vec<_>>()
										.join(", "),
								)))
							}),
						row.target.agent,
						&mut credits,
					)
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
	Ok(operation_batch(report))
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::models::McpTransport;
	// The lib binary's ONE env mutex. A module-local copy here would not
	// serialize against `GlobalLockGuard`'s `XDG_STATE_HOME` swap, which is UB
	// (and made `manager::skill`'s prune tests resolve the wrong lock file).
	use crate::skills::prune::test_lock::env_lock;
	use tempfile::tempdir;

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

	// Cursor, NOT Claude: Claude and Copilot both resolve a project MCP to
	// `<root>/.mcp.json`, so removing from Claude here is refused by
	// `protected_targets`' roster list — copilot would lose the server too.
	// Cursor owns `<root>/.cursor/mcp.json` alone, which is what this test
	// needs to say anything about deletion at all.
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

	// Fix A regression test: a Copy that fails at RUNTIME (after preflight
	// already passed) must not let its paired Delete run. Claude supports
	// project-scope stdio MCPs (so `mcp_supported_for_target` preflight is
	// clean), but Claude's OWN mcp config already holds an unrelated MCP
	// named "filesystem" — `add_mcp`'s duplicate-name guard rejects the copy
	// only once it actually runs. Before the fix, `reconcile_mcp` built one
	// flat Copy-then-Delete plan and attempted every row regardless, so the
	// Cursor delete still ran: the MCP would vanish from Cursor without ever
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

	// Fixture shared by the shared-master preflight tests: a universal Master
	// every NativeReader (cursor, codex, …) reads directly, plus claude's
	// symlink Referrer into it.
	/// The Master in the store, plus TWO Referrers: claude's private one and the
	/// shared `.agents/skills` slot.
	///
	/// The shared link is not decoration. Cursor, codex, cline, warp and five
	/// others reach a project skill only by scanning that directory, and it used
	/// to hold the Master itself — so storing the skill granted it to all of
	/// them for free. Now the grant is a link, and a fixture that omits it is
	/// testing an agent that simply does not have the skill.
	#[cfg(unix)]
	fn master_with_claude_referrer(root: &std::path::Path, name: &str) {
		let master = root.join(".aghub").join(name);
		fs::create_dir_all(&master).unwrap();
		fs::write(
			master.join("SKILL.md"),
			format!(
				"---\nname: {name}\ndescription: Shared\n---\n\n# {name}\n"
			),
		)
		.unwrap();
		let claude_skills = root.join(".claude/skills");
		fs::create_dir_all(&claude_skills).unwrap();
		std::os::unix::fs::symlink(&master, claude_skills.join(name)).unwrap();
		let shared = root.join(".agents/skills");
		fs::create_dir_all(&shared).unwrap();
		std::os::unix::fs::symlink(&master, shared.join(name)).unwrap();
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
	// holder; the refusal then names claude as the reason the Master stays.
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
		let message = outcome
			.expect_err(
				"an unverifiable holder must block the Master's removal",
			)
			.to_string();
		assert!(message.contains("nothing was written"), "got: {message}");
		assert!(
			message.contains("claude"),
			"the refusal must name the holder that keeps the Master alive, or \
			 the user has no way to act on it; got: {message}"
		);
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
			vec![AgentType::Cline],
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

	// Fix A regression test (skill case): a Copy that fails at RUNTIME (after
	// preflight already passed) must not let its paired Delete run — same
	// policy as `reconcile_mcp_keeps_source_when_a_copy_fails_at_runtime`, but
	// for the highest-blast-radius resource, since a skill delete can
	// `remove_dir_all` an on-disk directory.
	//
	// The source skill here is a COPY-LAYOUT skill: a plain, hand-created
	// directory inside Claude's own skills dir with no `.agents/skills`
	// Master, so `canonical_path` is None and this directory is the SOLE
	// on-disk copy. Windsurf's own skills dir already holds a real directory
	// at the slot the copy would need to link into, so the universal
	// materializer's link step reports a conflict at write time — preflight
	// (`skill_target_dir`) only resolves the write dir, it never checks for an
	// existing occupant. Before the fix, `reconcile_skill` attempted the
	// Delete regardless: the source directory would be `remove_dir_all`'d
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
			exhaustive: false,
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
}
