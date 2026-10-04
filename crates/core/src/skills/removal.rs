//! Layout-aware removal helpers + containment guard. Allow-listed skills roots:
//! the shared store roots ([`skill_store_roots`]) plus each agent's own skills
//! dir, so a `remove_dir_all` never escapes a known skills root (defends against
//! a symlink pointing out of tree).

use std::path::{Path, PathBuf};

use crate::models::{AgentType, ResourceScope};
use crate::skills::linker::Linker;

/// The shared skill-store roots for a scope: the `.aghub` Master store plus the
/// shared Referrer roots (`.agents/skills`, and the XDG `agents/skills`, which
/// has NO leading dot) that several agents read at once.
///
/// Separate from [`allowed_skill_roots`]: "may this be deleted at all" and "is
/// this SHARED" differ (a private copy is deletable, a Master is not). Both
/// consumers need the `.aghub` entries — without them every Master delete is
/// refused as out-of-tree, or a single-agent removal takes the Master.
/// See `.scratch/aghub-skill-store/spec.md` "Day-one hazards".
/// A linked PROJECT store is omitted (a project must not widen the mutation
/// roots by pointing `.aghub` outside itself); the global store stays even when
/// symlinked (dotfiles), since consumers canonicalize roots.
pub fn skill_store_roots(project_root: Option<&Path>) -> Vec<PathBuf> {
	let mut roots: Vec<PathBuf> = Vec::new();
	// Universal global root: $XDG_CONFIG_HOME/agents/skills (dirs resolves XDG).
	if let Some(config) = dirs::config_dir() {
		roots.push(config.join("agents").join("skills"));
	}
	if let Some(home) = dirs::home_dir() {
		// Explicit ~/.config fallback (in case XDG_CONFIG_HOME points elsewhere).
		roots.push(home.join(".config").join("agents").join("skills"));
		// Legacy ~/.agents/skills — a shared Referrer root, no longer the Master.
		roots.push(home.join(".agents").join("skills"));
		roots.push(home.join(crate::skills::linker::MASTER_STORE_DIR_NAME));
	}
	if let Some(root) = project_root {
		roots.push(root.join(".agents").join("skills"));
		let store = root.join(crate::skills::linker::MASTER_STORE_DIR_NAME);
		if !Linker::is_link(&store) {
			roots.push(store);
		}
	}
	roots
}

/// Collect the allow-listed skills roots for a scope.
///
/// Includes the universal global root (`$XDG_CONFIG_HOME/agents/skills` or
/// `~/.config/agents/skills`), the legacy `~/.agents/skills`, the project's
/// `<project>/.agents/skills`, and every agent-specific skills dir passed in.
/// Only roots that exist on disk are returned, each canonicalized so containment
/// checks compare real paths (resolving `/private` and similar symlink prefixes).
pub fn allowed_skill_roots(
	agent_skill_dirs: &[PathBuf],
	project_root: Option<&Path>,
) -> Vec<PathBuf> {
	let mut candidates: Vec<PathBuf> = skill_store_roots(project_root);
	candidates.extend(agent_skill_dirs.iter().cloned());

	let mut roots: Vec<PathBuf> = Vec::new();
	for c in candidates {
		if let Ok(canonical) = c.canonicalize() {
			if !roots.contains(&canonical) {
				roots.push(canonical);
			}
		}
	}
	roots
}

/// Resolve the on-disk root directory of an installed skill from its model
/// (`canonical_path` preferred, else `source_path`), expanding a leading `~/`
/// and stepping up from a `SKILL.md` file to its containing folder. The single
/// home shared by `installed_skill_roots` and the hash-baseline scans in the
/// CLI `check` and the API check-updates path.
pub fn skill_root(skill: &crate::models::Skill) -> Option<PathBuf> {
	let raw = skill
		.canonical_path
		.as_deref()
		.or(skill.source_path.as_deref())?;
	let path = if let Some(stripped) = raw.strip_prefix("~/") {
		dirs::home_dir().map(|home| home.join(stripped))?
	} else {
		PathBuf::from(raw)
	};
	let is_skill_file = path
		.file_name()
		.is_some_and(|name| name == std::ffi::OsStr::new("SKILL.md"));
	Some(if is_skill_file {
		path.parent().map(Path::to_path_buf).unwrap_or(path)
	} else {
		path
	})
}

/// Resolve the on-disk roots of every installed skill named `name` in the given
/// scope. A lock→disk resolver: loads all agents' skills, filters by name, and
/// returns each distinct skill folder root. The single home shared by the CLI
/// (`apply-update`), the API (git-sync / sources / check-updates), and the
/// `skill-update` sources service.
pub fn installed_skill_roots(
	name: &str,
	resource_scope: crate::models::ResourceScope,
	project_root: Option<&Path>,
) -> Vec<PathBuf> {
	installed_skill_roots_in(
		&crate::load_all_agents(resource_scope, project_root),
		name,
	)
}

/// [`installed_skill_roots`] against an ALREADY-loaded agent set. The scan is
/// the expensive half — every registered agent's config re-read from disk — and
/// it does not vary by name, so a caller resolving many names in one pass loads
/// once and reuses. Same filtering: name match, a resolvable [`skill_root`],
/// de-duplicated.
pub fn installed_skill_roots_in(
	agents: &[crate::AgentResources],
	name: &str,
) -> Vec<PathBuf> {
	let mut roots = Vec::new();
	for agent in agents {
		for skill in &agent.skills {
			if skill.name != name {
				continue;
			}
			let Some(root) = skill_root(skill) else {
				continue;
			};
			if !roots.contains(&root) {
				roots.push(root);
			}
		}
	}
	roots
}

/// Canonicalize `target` and assert it is a descendant of one allow-listed root.
/// Returns the canonical path if contained, else `None` (caller skips + warns).
///
/// Canonicalizing both sides means a symlink whose target escapes every root is
/// rejected — the resolved path is compared, not the link location.
pub fn assert_contained(target: &Path, roots: &[PathBuf]) -> Option<PathBuf> {
	let canonical = target.canonicalize().ok()?;
	for root in roots {
		let root_canonical =
			root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
		if canonical.starts_with(&root_canonical) {
			return Some(canonical);
		}
	}
	None
}

/// Canonicalize `target` and assert it is a strict descendant of one
/// allow-listed root. Unlike [`assert_contained`], the root itself is rejected
/// even when it is nested inside another root.
pub fn assert_strictly_contained(
	target: &Path,
	roots: &[PathBuf],
) -> Option<PathBuf> {
	let canonical = target.canonicalize().ok()?;
	let canonical_roots: Vec<PathBuf> = roots
		.iter()
		.map(|root| root.canonicalize().unwrap_or_else(|_| root.to_path_buf()))
		.collect();
	if !canonical_roots.iter().any(|r| r == &canonical)
		&& canonical_roots.iter().any(|r| canonical.starts_with(r))
	{
		Some(canonical)
	} else {
		None
	}
}

/// Why a by-path request cannot name a deletable skill. `Display` carries no
/// filesystem path: it reaches an API response verbatim.
/// See docs/history/api.md#delete-by-path-target-must-be-a-skill-root
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ByPathRefusal {
	/// Not a skill directory (a category folder, a subdirectory of a skill, a
	/// stray file) or its `SKILL.md` does not parse.
	NotSkillRoot,
	/// The path could not be resolved (symlink loop, permission, a non-directory
	/// ancestor): its shape is unknown, so nothing may be derived from it.
	Unresolvable,
}

impl std::fmt::Display for ByPathRefusal {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			Self::NotSkillRoot => f.write_str(
				"Refusing to delete: the target must be a skill directory with \
				 its own readable SKILL.md, or that SKILL.md itself",
			),
			Self::Unresolvable => f.write_str(
				"Refusing to delete: source_path could not be resolved \
				 (unreadable, or a symlink loop)",
			),
		}
	}
}

/// The skill directory a by-path `source_path` names: the path itself when it is
/// a directory, else the parent of a `SKILL.md`. Any other file is refused
/// (`<skill>/scripts/run.sh` must not resolve to the skill, nor to `scripts`).
/// A missing `SKILL.md` still yields its parent so the caller's already-gone
/// check answers; a missing anything else is refused.
pub fn by_path_skill_dir(source: &Path) -> Result<PathBuf, ByPathRefusal> {
	let skill_md_parent = || {
		source
			.file_name()
			.is_some_and(|name| name == "SKILL.md")
			.then(|| source.parent().map(Path::to_path_buf))
			.flatten()
			.ok_or(ByPathRefusal::NotSkillRoot)
	};
	match std::fs::metadata(source) {
		Ok(meta) if meta.is_dir() => Ok(source.to_path_buf()),
		Ok(_) => skill_md_parent(),
		Err(e) if e.kind() == std::io::ErrorKind::NotFound => skill_md_parent(),
		Err(_) => Err(ByPathRefusal::Unresolvable),
	}
}

/// The frontmatter `name` of the skill rooted at `dir`, which must hold its OWN
/// parsable `SKILL.md` and sit in the folder that name sanitizes to
/// (`sanitize_name`, what an install names it). An absent or unparsable
/// `SKILL.md` used to fall back to the folder name, which made a category
/// folder (`<slot>/team`) look like a skill named `team`.
pub fn by_path_skill_name(dir: &Path) -> Result<String, ByPathRefusal> {
	let skill_md = dir.join("SKILL.md");
	match std::fs::metadata(&skill_md) {
		Ok(meta) if meta.is_file() => {}
		Ok(_) => return Err(ByPathRefusal::NotSkillRoot),
		Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
			return Err(ByPathRefusal::NotSkillRoot)
		}
		Err(_) => return Err(ByPathRefusal::Unresolvable),
	}
	let name = skill::parser::parse(&skill_md)
		.map(|parsed| parsed.name)
		.map_err(|_| ByPathRefusal::NotSkillRoot)?;
	// The folder must be the slot name the frontmatter name sanitizes to (the
	// name an install gives it).
	let folder = dir.file_name().and_then(|n| n.to_str());
	if folder != Some(skill::sanitize::sanitize_name(&name).as_str()) {
		return Err(ByPathRefusal::NotSkillRoot);
	}
	Ok(name)
}

pub fn assert_targets_strictly_contained(
	targets: &[PathBuf],
	agent_skill_dirs: &[PathBuf],
	project_root: Option<&Path>,
) -> std::io::Result<()> {
	let roots = allowed_skill_roots(agent_skill_dirs, project_root);
	for target in targets {
		if assert_strictly_contained(target, &roots).is_some() {
			continue;
		}
		if contained_nonexistent_strict_target(target, &roots) {
			continue;
		}
		return Err(std::io::Error::new(
			std::io::ErrorKind::PermissionDenied,
			format!(
				"target is not a skill directory under allowed skill roots: {}",
				target.display()
			),
		));
	}
	Ok(())
}

fn contained_nonexistent_strict_target(
	target: &Path,
	roots: &[PathBuf],
) -> bool {
	if target.exists() {
		return false;
	}
	let Some(parent) = target.parent() else {
		return false;
	};
	let Ok(parent) = parent.canonicalize() else {
		return false;
	};
	roots.iter().any(|root| {
		let root = root.canonicalize().unwrap_or_else(|_| root.clone());
		parent.starts_with(&root)
	})
}

/// On-disk layout of an installed skill, deciding how it is removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
	/// `.agents/skills/<name>` canonical dir + per-agent symlinks resolving to it.
	Symlink,
	/// Independent per-agent copies (no `canonical_path`).
	Copy,
}

/// What a removal WOULD touch. Produced without deleting anything so a dry-run
/// can list the exact paths; the manager re-checks each path at delete time.
#[derive(Debug, Clone)]
pub struct RemovalPlan {
	/// At least one agent dir could not be enumerated completely, so `paths`
	/// may be SHORT and `skipped` names the dir that hid the rest. A field
	/// because the exhaustive executor (`accept_rename` → `execute_removal`)
	/// acts on `paths` alone and must not delete a partial result set.
	pub incomplete: bool,
	pub layout: Layout,
	/// Absolute paths that would be removed (symlinks unlinked, dirs `remove_dir_all`'d).
	pub paths: Vec<std::path::PathBuf>,
	/// Paths intentionally NOT removed (out-of-allowlist canonical, or canonical
	/// kept because another view still references it / canonicalize failed) — warn.
	pub skipped: Vec<std::path::PathBuf>,
	/// True when destructive execution requires an explicit confirm flag
	/// (symlink-layout removal, copy `--all-agents`, or releasing a real
	/// directory from a shared Referrer root).
	pub needs_confirm: bool,
	/// This removal took NOTHING, and the caller must not report otherwise.
	///
	/// Two producers, not interchangeable: a targeted removal that takes nothing
	/// away (a shared Referrer an unselected reader still needs, or
	/// `remove_skill_planned`'s `blocks` verdict) and is refused; or an
	/// EXHAUSTIVE sweep that took nothing with entries in `skipped` (it cannot
	/// prove nothing still holds the skill). `remove_skill_planned` answers the
	/// latter with a PREVIEW even when confirmed — `commit` would report
	/// `executed` and run the lock GC on a run that removed nothing.
	pub shared_master_kept: bool,
	/// Where the skill is STILL served from after this removal — the reason a
	/// refusal refuses, named.
	///
	/// Not folded into `skipped`, whose two meanings ("deliberately not taken",
	/// "could not be read") `all_survivors_reported` reads back. Populated ONLY
	/// on refusal, by the one verdict owner (`read_effect_after`, via
	/// `remove_skill_planned`), so every surface names the same paths.
	/// See docs/history/core-removal.md#reconcile-refusal-hid-the-agents-own-read-dir
	pub still_read_from: Vec<std::path::PathBuf>,
}

/// A path's identity for comparison, with the FINAL component left unresolved:
/// a Referrer and its Master canonicalize to the same path, so resolving the
/// leaf would read "delete this Referrer" as "delete the Master". The parent is
/// resolved so two spellings of one directory (macOS `/var`, Windows short
/// names) compare equal.
///
/// `pub(crate)` because `skills::shape`'s compat-Referrer sweep has the same
/// trap (a compat dir reached through a symlinked ANCESTOR, `.agent/skills` ->
/// `.agents/skills`, is a different `PathBuf` from the slot it aliases). There
/// is exactly one identity rule — do not re-derive it.
pub(crate) fn entry_identity(path: &Path) -> PathBuf {
	match (path.parent(), path.file_name()) {
		(Some(parent), Some(leaf)) => {
			crate::skills::linker::classify::canonicalize_lenient(parent)
				.join(leaf)
		}
		_ => path.to_path_buf(),
	}
}

/// The folder a discovered skill was read FROM — its own entry, never the
/// Master a Referrer resolves to.
///
/// [`skill_root`] answers the other question (`canonical_path` first) and is
/// the wrong one here: a removal plan lists ENTRIES, so entries are what a plan
/// can be checked against.
fn discovered_entry_dir(skill: &crate::models::Skill) -> Option<PathBuf> {
	let raw = skill.source_path.as_deref()?;
	let path = match raw.strip_prefix("~/") {
		Some(rest) => dirs::home_dir()?.join(rest),
		None => PathBuf::from(raw),
	};
	path.parent().map(Path::to_path_buf)
}

/// Candidates for an IDENTITY question — "does anything in `dir` resolve to that
/// directory?" — which needs no frontmatter. Wide on purpose: a Referrer whose
/// own `SKILL.md` will not parse never becomes a `Skill`, so a name-matched list
/// misses it and its target gets `remove_dir_all`'d under a dangling link. Only
/// callers that act on a canonical-target match may use it; a DELETE-by-name
/// sweep uses the narrow [`candidate_entries`].
///
/// The `bool` says the list may be SHORT (`dir` held something unreadable).
/// Every destructive caller must fail closed on it, never read it as "nothing
/// found". See docs/history/core-removal.md#slot-only-removal-sweeps
pub(crate) fn referrer_candidates(
	dir: &Path,
	safe: &str,
) -> (Vec<PathBuf>, bool) {
	let (mut out, incomplete) = crate::skills::discovery::entry_paths(dir);
	let slot = dir.join(safe);
	if !out.contains(&slot) {
		out.push(slot);
	}
	(out, incomplete)
}

/// Every folder in `dir` a removal of skill `name` must consider: the
/// `<dir>/<sanitized-name>` slot UNION whatever discovery reads as that skill
/// (an npx-era `<dir>/<folder>` under a different frontmatter name, or a
/// grouped `<dir>/<team>/<folder>`). The slot stays in the union so an empty
/// same-named folder is cleaned up instead of colliding with a reinstall.
///
/// Existence is NOT filtered: the symlink planner must see a DANGLING link,
/// which `exists()` hides. The `bool` is the same fail-closed incompleteness
/// flag as [`referrer_candidates`].
pub(crate) fn candidate_entries(
	dir: &Path,
	name: &str,
	safe: &str,
) -> (Vec<PathBuf>, bool) {
	let mut out = vec![dir.join(safe)];
	let (found, incomplete) =
		crate::skills::discovery::load_skills_from_dir_partial(dir);
	for skill in found {
		if skill.name != name {
			continue;
		}
		if let Some(entry) = discovered_entry_dir(&skill) {
			if !out.contains(&entry) {
				out.push(entry);
			}
		}
	}
	(out, incomplete)
}

/// What a removal actually does to ONE agent's view of a skill.
#[derive(Debug, Clone, Default)]
pub struct ReadEffect {
	/// Entries that go on handing this agent the skill afterwards, as
	/// DISCOVERED (not canonicalized): these get surfaced to the user, and
	/// every other member of `RemovalPlan::skipped` is a raw path.
	pub survivors: Vec<PathBuf>,
	/// The set of distinct LOCATIONS this agent reads the skill from got
	/// smaller — the removal took something away even when `survivors` is
	/// non-empty.
	pub changed: bool,
	/// At least one read dir could not be enumerated completely, so
	/// `survivors` may be SHORT. Only a caller asserting a postcondition
	/// ("nothing reads it anymore") needs this; a caller deciding whether to
	/// REFUSE must keep ignoring it, or one odd sibling makes a skill
	/// undeletable.
	pub incomplete: bool,
}

/// Did this removal take anything away from the agent reading `read_dirs`, and
/// what still hands it the skill afterwards? The ONE place that answers it — a
/// plan's path list cannot. Full rationale: crates/core/AGENTS.md "Did that
/// removal take anything away?".
///
/// It asks DISCOVERY (frontmatter name, recursive), never a
/// `dir.join(sanitize_name(name))` probe. `changed` FULLY canonicalizes each
/// entry (leaf included, unlike [`entry_identity`]): an npx-era Referrer beside
/// its Master resolves to the Master's location, so unlinking it changes
/// nothing (refuse); a private copy shadowing a Master has its own location, so
/// deleting it shrinks the set (allow).
///
/// Fail-OPEN on an unlistable read dir ("cannot tell" = no survivors, flagged in
/// `incomplete`): this guard REFUSES removals, so fail-closed would let one
/// unreadable directory make a skill undeletable. `transfer::skill_holders`
/// asks the opposite question and is fail-CLOSED for the same reason. Do not
/// unify them.
pub fn read_effect_after(
	read_dirs: &[PathBuf],
	name: &str,
	deleting: &[PathBuf],
) -> ReadEffect {
	use std::collections::BTreeSet;

	let doomed: Vec<PathBuf> =
		deleting.iter().map(|path| entry_identity(path)).collect();
	let mut before: BTreeSet<PathBuf> = BTreeSet::new();
	let mut after: BTreeSet<PathBuf> = BTreeSet::new();
	let mut survivors: Vec<PathBuf> = Vec::new();

	let mut incomplete = false;
	for dir in read_dirs {
		let (found, dir_incomplete) =
			crate::skills::discovery::load_skills_from_dir_partial(dir);
		incomplete |= dir_incomplete;
		for skill in found.iter().filter(|skill| skill.name == name) {
			let Some(entry) = discovered_entry_dir(skill) else {
				continue;
			};
			let resolved =
				crate::skills::linker::classify::canonicalize_lenient(&entry);
			before.insert(resolved.clone());
			// `starts_with`, not equality: deleting a folder takes every skill
			// nested under it with it.
			let id = entry_identity(&entry);
			if doomed.iter().any(|doomed| id.starts_with(doomed)) {
				continue;
			}
			if after.insert(resolved) {
				survivors.push(entry);
			}
		}
	}

	ReadEffect {
		changed: before != after,
		survivors,
		incomplete,
	}
}

/// Plan a layout-aware removal.
///
/// - `own_agent_dir`: the targeted agent's skills dir (single-agent default;
///   ignored when `all_agents`).
/// - `all_agent_dirs`: every in-scope agent skills dir, used to sweep symlinks
///   and to check whether any OTHER view still references the canonical dir.
/// - `project_root`: contributes `<project>/.agents/skills` to the allow-list.
pub fn plan_removal(
	skill: &crate::models::Skill,
	own_agent_dir: Option<&Path>,
	all_agent_dirs: &[PathBuf],
	project_root: Option<&Path>,
	all_agents: bool,
) -> RemovalPlan {
	plan_removal_for_agents(
		skill,
		own_agent_dir,
		all_agent_dirs,
		project_root,
		crate::models::ResourceScope::Both,
		all_agents,
		&[],
	)
}

/// The planner with the complete set of agents authorized by one batch.
/// A shared Referrer may go only when no reader outside that set uses it.
pub(crate) fn plan_removal_for_agents(
	skill: &crate::models::Skill,
	own_agent_dir: Option<&Path>,
	all_agent_dirs: &[PathBuf],
	project_root: Option<&Path>,
	scope: crate::models::ResourceScope,
	all_agents: bool,
	requested_agents: &[crate::models::AgentType],
) -> RemovalPlan {
	let roots = allowed_skill_roots(all_agent_dirs, project_root);
	let safe = skill::sanitize::sanitize_name(&skill.name);
	// "Every agent" means every MANAGED agent: a dir only disabled agents read
	// is never swept, and a Referrer found there still keeps the Master.
	let unmanaged = if all_agents {
		unmanaged_skill_dirs(all_agent_dirs, project_root, requested_agents)
	} else {
		Vec::new()
	};

	// A requested agent with its OWN link into a shared-slot real directory
	// reaches here through the symlink layout, whose sweep sees the other
	// requested agents' links as survivors and so made the batch verdict depend
	// on `-a` order. Ask the one real-directory rule first; on a release plan
	// the owned links plus the directory exactly like the copy layout, on any
	// keep fall through to the unchanged symlink planner.
	// See docs/history/core-removal.md#disabled-agent-blocked-a-single-agent-delete
	if !all_agents && skill.canonical_path.is_some() {
		if let Some(canonical) = crate::transfer::skill_root_unchecked(skill) {
			if !Linker::is_link(&canonical)
				&& canonical.is_dir()
				&& is_universal_master(&canonical, project_root)
				&& !requested_agents.is_empty()
				&& single_agent_keep_reason(
					&canonical,
					all_agent_dirs,
					&skill.name,
					project_root,
					scope,
					requested_agents,
				)
				.is_none()
			{
				return assemble_copy_release_plan(
					canonical,
					&roots,
					all_agent_dirs,
					&skill.name,
					project_root,
					scope,
					requested_agents,
				);
			}
		}
	}

	if skill.canonical_path.is_some() {
		let unselected_needs = |dir: &Path, deleting: &[PathBuf]| {
			unselected_reader_needs_referrer(
				dir,
				&skill.name,
				scope,
				project_root,
				requested_agents,
				deleting,
			)
		};
		plan_symlink_removal(
			skill,
			&safe,
			own_agent_dir,
			all_agent_dirs,
			&roots,
			all_agents.then_some(unmanaged.as_slice()),
			&unselected_needs,
		)
	} else {
		plan_copy_removal(
			skill,
			&safe,
			all_agent_dirs,
			&unmanaged,
			&roots,
			project_root,
			scope,
			all_agents,
			requested_agents,
		)
	}
}

/// Symlink/`.agents` layout: unlink the targeted per-agent symlinks, and delete
/// the canonical dir only when (a) it is inside an allow-listed root and (b) no
/// other view still references it (an inspection error other than NotFound
/// counts as "might still reference" — keep, never treat it as no-match).
fn plan_symlink_removal(
	skill: &crate::models::Skill,
	safe: &str,
	own_agent_dir: Option<&Path>,
	all_agent_dirs: &[PathBuf],
	roots: &[PathBuf],
	// `Some(dirs only disabled agents read)` = an `--all-agents` sweep.
	all_agents_except: Option<&[PathBuf]>,
	unselected_needs: &impl Fn(&Path, &[PathBuf]) -> bool,
) -> RemovalPlan {
	let all_agents = all_agents_except.is_some();
	let unmanaged = all_agents_except.unwrap_or_default();
	let canonical = crate::transfer::skill_root_unchecked(skill);
	let canonical_real = canonical.as_ref().and_then(|c| c.canonicalize().ok());

	let mut paths: Vec<PathBuf> = Vec::new();
	let mut skipped: Vec<PathBuf> = Vec::new();
	let mut other_refs = false;
	let mut unresolvable = false;
	let mut targeted_anything = false;
	let mut incomplete_scan = false;
	let mut shared_referrer_kept = false;
	let mut targeted_entries: Vec<(PathBuf, PathBuf)> = Vec::new();

	for dir in all_agent_dirs {
		// TWO lists, and the split is load-bearing: the wide `entries` is
		// matched by canonical identity (so it cannot touch an unrelated skill,
		// and still sees a Referrer whose target will not parse); the narrow
		// `named` is the only list that may clear a DANGLING link, which has no
		// identity. Every push into `paths` must be cleared by one of the two.
		let (named, _) = candidate_entries(dir, &skill.name, safe);
		let (entries, incomplete) = referrer_candidates(dir, safe);
		incomplete_scan |= incomplete;
		if incomplete {
			// The sweep could not see all of this dir, so it cannot promise it
			// unlinked every referrer in it. Same verdict as a referrer it CAN
			// see: keep the canonical and report the dir.
			other_refs = true;
			if !skipped.contains(dir) {
				skipped.push(dir.clone());
			}
		}
		let targeted = (all_agents && !unmanaged.contains(dir))
			|| own_agent_dir.is_some_and(|d| d == dir.as_path());
		for entry in entries {
			// NotFound: this agent does not hold it. Any other error: the entry
			// is THERE and unreadable — fail CLOSED, an unknown holder keeps the
			// shared master as a known one would.
			// See docs/history/core-removal.md#unstatable-entry-dropped-from-the-sweep
			match std::fs::symlink_metadata(&entry) {
				Ok(_) => {}
				Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
					continue;
				}
				Err(_) => {
					// Keep the canonical either way, but only REPORT entries
					// this removal names: `skipped` reads as "kept your skill
					// here" and feeds the manager's accounting. The dir is
					// already reported through `incomplete`.
					other_refs = true;
					if named.contains(&entry) {
						skipped.push(entry);
					}
					continue;
				}
			}
			match entry.canonicalize() {
				Ok(resolved) => {
					if canonical_real.as_deref() == Some(resolved.as_path()) {
						if targeted {
							if !all_agents {
								targeted_entries
									.push((dir.clone(), entry.clone()));
							}
							if Linker::is_link(&entry) {
								paths.push(entry);
							}
							targeted_anything = true;
						} else {
							other_refs = true;
						}
					}
					// Resolves to a DIFFERENT target => a same-named but unrelated
					// skill; never touch it (match by canonical identity, not name).
				}
				Err(error) => {
					// Dangling/broken link: no identity to clear it, so only a
					// link this removal NAMES may go ("a link, and targeted"
					// would unlink every unrelated broken link here). NotFound
					// cannot reference an existing Master; other errors leave
					// identity unknown and keep the Master.
					if named.contains(&entry)
						&& Linker::is_link(&entry)
						&& targeted
					{
						if !all_agents {
							targeted_entries.push((dir.clone(), entry.clone()));
						}
						paths.push(entry);
						targeted_anything = true;
					}
					unresolvable |=
						error.kind() != std::io::ErrorKind::NotFound;
				}
			}
		}
	}

	// Decide shared-reader safety against the WHOLE candidate deletion set.
	// Checking each Referrer alone lets two links to the same Master serve as
	// each other's apparent fallback, then schedules both and revokes an
	// unselected reader. Include the Master too: a real shared entry can be
	// taken through the canonical path rather than a link row.
	if !all_agents && !targeted_entries.is_empty() {
		let mut deleting = paths.clone();
		deleting.extend(canonical.iter().cloned());
		for (dir, entry) in targeted_entries {
			if unselected_needs(&dir, &deleting) {
				other_refs = true;
				shared_referrer_kept = true;
				if !skipped.contains(&entry) {
					skipped.push(entry.clone());
				}
				paths.retain(|path| path != &entry);
			}
		}
	}

	if let Some(canon) = canonical {
		let keep = other_refs || unresolvable;
		if keep {
			skipped.push(canon);
		} else if !targeted_anything && !all_agents {
			// A single-agent removal needs a targeted Referrer. An exhaustive
			// removal may also collect an unlinked Master from the store.
		} else if assert_contained(&canon, roots).is_some() {
			paths.push(canon);
		} else {
			skipped.push(canon); // out-of-tree canonical: never remove_dir_all
		}
	}

	debug_assert!(
		!shared_referrer_kept || paths.is_empty(),
		"a shared-referrer keep must plan no paths: unselected_needs is keyed by dir, so one kept entry means all targeted entries are kept"
	);
	// An exhaustive cleanup that kept everything must not report `removed`.
	let shared_master_kept = shared_referrer_kept
		|| (all_agents && paths.is_empty() && !skipped.is_empty());
	RemovalPlan {
		layout: Layout::Symlink,
		paths,
		skipped,
		needs_confirm: true,
		shared_master_kept,
		still_read_from: Vec::new(),
		incomplete: incomplete_scan,
	}
}

/// Copy layout (no `canonical_path`): default removes only the targeted agent's
/// copy (from `source_path`); `--all-agents` removes every same-named copy.
#[allow(clippy::too_many_arguments)]
fn plan_copy_removal(
	skill: &crate::models::Skill,
	safe: &str,
	all_agent_dirs: &[PathBuf],
	unmanaged: &[PathBuf],
	roots: &[PathBuf],
	project_root: Option<&Path>,
	scope: crate::models::ResourceScope,
	all_agents: bool,
	requested_agents: &[crate::models::AgentType],
) -> RemovalPlan {
	let mut paths: Vec<PathBuf> = Vec::new();
	let mut skipped: Vec<PathBuf> = Vec::new();

	let mut incomplete_scan = false;
	if all_agents {
		for dir in all_agent_dirs.iter().filter(|d| !unmanaged.contains(d)) {
			let (entries, incomplete) =
				candidate_entries(dir, &skill.name, safe);
			incomplete_scan |= incomplete;
			if incomplete && !skipped.contains(dir) {
				// `--all-agents` promises "gone everywhere" and this dir could
				// not be fully listed, so the promise is unverifiable. Report
				// it: `read_effect_after` finds no survivor in a dir it also
				// cannot read, so `skipped` is the only place the gap shows.
				skipped.push(dir.clone());
			}
			for copy in entries {
				if copy.exists() && !paths.contains(&copy) {
					// A tracked (or undecidable) real directory survives the
					// sweep; the survivor is what blocks the commit.
					if let Some(reason) =
						shared_slot_git_keep(&copy, project_root)
					{
						if let Some(hint) = git_keep_hint(&reason, &copy) {
							log::warn!("{hint}");
						}
						skipped.push(copy);
						continue;
					}
					push_contained(copy, roots, &mut paths, &mut skipped);
				}
			}
		}
		RemovalPlan {
			layout: Layout::Copy,
			paths,
			skipped,
			needs_confirm: true,
			shared_master_kept: false,
			still_read_from: Vec::new(),
			incomplete: incomplete_scan,
		}
	} else {
		let mut shared_master_kept = false;
		if let Some(root) = crate::transfer::skill_root_unchecked(skill) {
			if root.exists() {
				match single_agent_keep_reason(
					&root,
					all_agent_dirs,
					&skill.name,
					project_root,
					scope,
					requested_agents,
				) {
					Some(KeepReason::UniversalMaster) => {
						shared_master_kept = true;
						skipped.push(root);
					}
					Some(KeepReason::ExternalReferrer(referrer)) => {
						if is_universal_master(&root, project_root) {
							shared_master_kept = true;
							skipped.push(root);
							skipped.push(referrer);
						} else {
							// Name the referrer, not just the kept dir: `skipped`
							// lists only the caller's own path, so a keep decided
							// by another agent's dir is otherwise undiagnosable.
							// The reason CARRIES the path for this: an extraction
							// that dropped it would silently undo the diagnosis.
							log::warn!(
								"keeping {}: {} still references it",
								root.display(),
								referrer.display()
							);
							skipped.push(root);
						}
					}
					Some(
						reason @ (KeepReason::GitTracked
						| KeepReason::GitUndecided),
					) => {
						shared_master_kept = true;
						if let Some(hint) = git_keep_hint(&reason, &root) {
							log::warn!("{hint}");
						}
						skipped.push(root);
					}
					None => {
						return assemble_copy_release_plan(
							root,
							roots,
							all_agent_dirs,
							&skill.name,
							project_root,
							scope,
							requested_agents,
						);
					}
				}
			}
		}
		RemovalPlan {
			layout: Layout::Copy,
			paths,
			skipped,
			needs_confirm: false,
			shared_master_kept,
			still_read_from: Vec::new(),
			// The single-agent copy path asks `single_agent_keep_reason`, which
			// fails CLOSED on an unfinished listing by keeping the directory —
			// so an incomplete scan can only produce a KEEP here, never a short
			// `paths`.
			incomplete: false,
		}
	}
}

/// The plan for releasing a REAL directory the keep rule already let through:
/// its owned inbound links plus the directory itself, with the
/// `needs_confirm` contract of a shared-root release. One producer for the
/// by-name copy planner and the API by-path route, so the two cannot report
/// different `needs_confirm` for the same directory.
pub fn assemble_copy_release_plan(
	root: PathBuf,
	roots: &[PathBuf],
	all_agent_dirs: &[PathBuf],
	name: &str,
	project_root: Option<&Path>,
	scope: crate::models::ResourceScope,
	requested_agents: &[crate::models::AgentType],
) -> RemovalPlan {
	let mut paths = Vec::new();
	let mut skipped = Vec::new();
	// A shared-slot real-directory release came from the symlink policy and
	// keeps its confirmation contract; an ordinary private copy stays false.
	let needs_confirm = is_universal_master(&root, project_root);
	let start_len = paths.len();
	push_contained(root, roots, &mut paths, &mut skipped);
	if paths.len() > start_len {
		let root_path = paths.pop().expect("root was just pushed");
		paths = plan_dir_release_paths(
			root_path,
			all_agent_dirs,
			roots,
			name,
			project_root,
			scope,
			requested_agents,
		);
	}
	RemovalPlan {
		layout: Layout::Copy,
		paths,
		skipped,
		needs_confirm,
		shared_master_kept: false,
		still_read_from: Vec::new(),
		incomplete: false,
	}
}

/// True when `dir` lives inside a universal Master store — SHARED with every
/// agent reading that store, not one agent's private copy.
///
/// Containment, not path shape: discovery recurses, so a Master can sit at
/// `.agents/skills/<team>/<name>` (a `parent == "skills"` test misses it and
/// lets a single-agent removal take it), and a private copy at
/// `.claude/skills/agents/skills/<name>` must not match (else undeletable).
///
/// Shared alone is not a reason to keep — see [`skill_dir_readers_outside`].
fn is_universal_master(dir: &Path, project_root: Option<&Path>) -> bool {
	assert_strictly_contained(dir, &skill_store_roots(project_root)).is_some()
}

fn readers_outside(
	dir: &Path,
	scope: crate::models::ResourceScope,
	project_root: Option<&Path>,
	requested: &[crate::models::AgentType],
	include_disabled: bool,
) -> Vec<&'static str> {
	let disabled = crate::agent_settings::disabled_agents();
	let target = crate::skills::linker::classify::canonicalize_lenient(dir);
	crate::models::AgentType::ALL
		.iter()
		.filter(|agent| include_disabled || !disabled.contains(agent.as_str()))
		.filter(|agent| !requested.contains(agent))
		.filter(|agent| {
			crate::create_adapter(**agent)
				.get_skills_paths(project_root, scope)
				.iter()
				.any(|read_dir| {
					target.starts_with(
						crate::skills::linker::classify::canonicalize_lenient(
							read_dir,
						),
					)
				})
		})
		.map(|agent| crate::registry::get(*agent).id)
		.collect()
}

/// How many in-scope agents structurally read `dir` across the full roster.
///
/// Full roster: slot sharing is structural, disabled agents are unmanaged not
/// absent.
pub(crate) fn slot_reader_count(
	dir: &Path,
	scope: crate::models::ResourceScope,
	project_root: Option<&Path>,
) -> usize {
	readers_outside(dir, scope, project_root, &[], true).len()
}

// See docs/history/core-removal.md#disabled-agent-blocked-a-single-agent-delete
/// Which in-scope agents read the skill folder `dir` WITHOUT being named in
/// `requested`? The question a LOCATION delete asks: "is this a shared Master?"
/// alone made every Master undeletable through the desktop's per-location
/// dialog, which sends every agent installed at that path. It is the readers
/// LEFT OUT of a request that make it dangerous, not the layout.
///
/// Disabled agents are excluded because they are unmanaged.
///
/// Answered from each agent's read dirs, never a `load_all_agents` scan: an
/// agent whose config fails to parse loads zero skills and would read as "not a
/// reader" (fail OPEN). Containment, not equality, because discovery recurses.
pub fn skill_dir_readers_outside(
	dir: &Path,
	scope: crate::models::ResourceScope,
	project_root: Option<&Path>,
	requested: &[crate::models::AgentType],
) -> Vec<&'static str> {
	readers_outside(dir, scope, project_root, requested, false)
}

/// A shared Referrer is still needed when an unselected reader has no other
/// discovered copy after this entry goes away. An unreadable directory keeps
/// the grant: this check authorizes deletion, so uncertainty fails closed.
///
/// The `< 2` shortcut includes disabled agents (`include_disabled: true`)
/// because the initiator itself may be disabled (e.g. `aghub-cli -a <disabled>
/// delete skills ...`). If disabled agents were filtered out in the count, an
/// initiator that is disabled plus one enabled reader would count as 1, falsely
/// triggering the shortcut and deleting a shared Referrer that the enabled
/// reader still depends on.
fn unselected_reader_needs_referrer(
	dir: &Path,
	name: &str,
	scope: crate::models::ResourceScope,
	project_root: Option<&Path>,
	requested_agents: &[crate::models::AgentType],
	deleting: &[PathBuf],
) -> bool {
	if slot_reader_count(dir, scope, project_root) < 2 {
		return false;
	}
	skill_dir_readers_outside(dir, scope, project_root, requested_agents)
		.into_iter()
		.any(|id| {
			let Ok(agent) = id.parse::<crate::models::AgentType>() else {
				return true;
			};
			let read_dirs = crate::create_adapter(agent)
				.get_skills_paths(project_root, scope);
			let effect = read_effect_after(&read_dirs, name, deleting);
			effect.incomplete || effect.survivors.is_empty()
		})
}

/// True when some in-scope agent skills dir holds a discovered Referrer for
/// `name` that resolves to `target_dir` — i.e. `target_dir` is a shared Master
/// another view still references. A copy-layout removal must NOT
/// `remove_dir_all` such a dir (it would orphan the live link); it keeps +
/// reports it instead. The symlink layout already does this via its `other_refs`
/// sweep — this is the copy-path equivalent for a Master that was discovered as
/// a real dir (`canonical_path=None`) by a direct `.agents/skills` reader.
pub fn dir_has_external_referrer(
	target_dir: &Path,
	all_agent_dirs: &[PathBuf],
	name: &str,
) -> Option<PathBuf> {
	dir_has_external_referrer_excluding(
		target_dir,
		all_agent_dirs,
		name,
		&|_| false,
		None,
	)
}

/// Identical to [`dir_has_external_referrer`] except that when a link entry
/// resolves to the target and `ignore(&entry)` is true, it is skipped.
/// When `ignored` is `Some`, every link skipped via `ignore` is pushed into it.
pub fn dir_has_external_referrer_excluding(
	target_dir: &Path,
	all_agent_dirs: &[PathBuf],
	name: &str,
	ignore: &dyn Fn(&Path) -> bool,
	mut ignored: Option<&mut Vec<PathBuf>>,
) -> Option<PathBuf> {
	let target_real = target_dir.canonicalize().ok()?;
	let safe = skill::sanitize::sanitize_name(name);
	// Same union as the symlink sweep: a Referrer this loop cannot see is one
	// the caller then orphans with `remove_dir_all`, and the slot spelling never
	// names an npx-era or grouped layout.
	for dir in all_agent_dirs {
		let dir = dir.as_path();
		let (entries, incomplete) = referrer_candidates(dir, &safe);
		if incomplete {
			// An unfinished listing may hide a Referrer, and this fn only KEEPS:
			// treat it as an unknown referrer and name the DIR that could not be
			// read, not a guessed entry.
			return Some(dir.to_path_buf());
		}
		for entry in entries {
			// Fail closed only where a referrer could actually BE: `is_link` and
			// `canonicalize().unwrap_or(false)` both answer "no" to EACCES (hiding
			// a live inbound link), yet this loop runs over every agent in the
			// roster, so failing closed on every stat error lets one odd dir block
			// every copy-layout delete.
			// See docs/history/core-removal.md#unreadable-peer-dir-hid-an-inbound-link
			match std::fs::symlink_metadata(&entry) {
				Ok(_) => {}
				// Nothing there, and — for NotADirectory — nothing CAN be: the
				// parent itself is a file, so it holds no entries at all.
				Err(error)
					if matches!(
						error.kind(),
						std::io::ErrorKind::NotFound
							| std::io::ErrorKind::NotADirectory
					) =>
				{
					continue;
				}
				Err(_) => {
					// Cannot stat the entry, but a NAME needs no stat (mode 0400:
					// `read_dir` works, child stats fail). A complete listing
					// without the leaf proves no referrer. Asked of the entry's own
					// parent, since discovered entries may nest below `dir`.
					let listed = entry
						.file_name()
						.and_then(|leaf| leaf.to_str())
						.and_then(|leaf| {
							dir_lists_name(entry.parent().unwrap_or(dir), leaf)
						});
					match listed {
						Some(false) => continue,
						// Present but opaque, or unlistable: unknown keeps the dir
						// (costs a refused deletion, never data).
						Some(true) | None => return Some(entry),
					}
				}
			}
			if !Linker::is_link(&entry) {
				continue;
			}
			match std::fs::canonicalize(&entry) {
				Ok(resolved) => {
					if resolved == target_real {
						if ignore(&entry) {
							if let Some(ref mut list) = ignored {
								if !list.contains(&entry) {
									list.push(entry);
								}
							}
							continue;
						}
						return Some(entry);
					}
				}
				// A dangling link cannot be referencing a master that still exists.
				Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
				Err(_) => return Some(entry),
			}
		}
	}
	None
}

/// `Some(true)` / `Some(false)` when `dir`'s listing was read IN FULL and
/// `safe` was / was not among the names; `None` when the listing could not be
/// completed and the question stays open.
///
/// Deliberately name-only: `read_dir` yields names without stat'ing anything,
/// which is what makes it usable on a directory whose children cannot be
/// stat'd.
fn dir_lists_name(dir: &Path, safe: &str) -> Option<bool> {
	let mut found = false;
	for entry in std::fs::read_dir(dir).ok()? {
		// A per-entry error means the listing is INCOMPLETE — the name could
		// have been in the part we did not get.
		if entry.ok()?.file_name() == std::ffi::OsStr::new(safe) {
			found = true;
		}
	}
	Some(found)
}

/// Builds the refusal text when a delete is refused because git tracks the
/// skill directory. `--all-agents` is refused too, so it names no agent count.
pub fn untrack_hint(path: &Path) -> String {
	format!(
		"{} is tracked by git, so deleting it is refused; keep authoring it there, or untrack it deliberately with `git rm -r --cached {}` (and commit that), then delete again",
		path.display(),
		path.display()
	)
}

/// The refusal text when git could not say whether it tracks `path` (git
/// missing, unusable repository, `dubious ownership`, or a probe that timed
/// out). Deleting an authored directory on a guess is the worse failure.
pub fn git_undecided_hint(path: &Path) -> String {
	format!(
		"could not tell whether git tracks {} (git missing, unusable repository or timed out), so deleting it is refused; fix git for that repository and delete again, or remove the directory yourself",
		path.display()
	)
}

/// The refusal text for a git-driven keep, `None` for every other reason.
pub fn git_keep_hint(reason: &KeepReason, path: &Path) -> Option<String> {
	match reason {
		KeepReason::GitTracked => Some(untrack_hint(path)),
		KeepReason::GitUndecided => Some(git_undecided_hint(path)),
		KeepReason::UniversalMaster | KeepReason::ExternalReferrer(_) => None,
	}
}

/// Why a SINGLE-agent removal must keep a skill folder instead of deleting it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeepReason {
	/// A `.aghub` Master, or a real directory in a shared Referrer slot that an
	/// enabled reader outside the request (or an empty request) still needs.
	/// Reported to the caller as `shared_master_kept`.
	UniversalMaster,
	/// Not a Master, but another in-scope agent's symlink resolves into it;
	/// deleting it would orphan that live link. Carries THAT link, because the
	/// caller can only report a keep it can name. Links in private skills dirs
	/// owned by requested agents (with no unrequested enabled readers) are
	/// excluded so batch deletions remain order-independent.
	ExternalReferrer(PathBuf),
	/// A real directory in a shared slot that is tracked by git. Deleting it
	/// for only some agents is refused to avoid destroying authored source.
	GitTracked,
	/// Same population, but git could not answer. Fails CLOSED, like `repair`.
	GitUndecided,
}

fn link_owned_by_requested(
	link: &Path,
	requested: &[AgentType],
	project_root: Option<&Path>,
	scope: ResourceScope,
) -> bool {
	if requested.is_empty() {
		return false;
	}
	let Some(p) = link.parent() else {
		return false;
	};
	let resolved_p = ::skill::lock::resolve_existing(p);
	let store_roots: Vec<PathBuf> = skill_store_roots(project_root)
		.into_iter()
		.map(|r| ::skill::lock::resolve_existing(&r))
		.collect();

	let mut in_private_dir = false;
	for agent in requested {
		for dir in
			crate::create_adapter(*agent).get_skills_paths(project_root, scope)
		{
			let resolved_dir = ::skill::lock::resolve_existing(&dir);
			if store_roots
				.iter()
				.any(|root| resolved_dir.starts_with(root))
			{
				continue;
			}
			if resolved_p.starts_with(&resolved_dir) {
				in_private_dir = true;
				break;
			}
		}
		if in_private_dir {
			break;
		}
	}
	if !in_private_dir {
		return false;
	}

	skill_dir_readers_outside(p, scope, project_root, requested).is_empty()
}

/// Inbound links to `dir` that were ignored by [`dir_has_external_referrer_excluding`]
/// because they are owned by `requested` agents in their private skills dirs with
/// no outside enabled readers.
fn owned_inbound_links(
	dir: &Path,
	all_agent_dirs: &[PathBuf],
	name: &str,
	project_root: Option<&Path>,
	scope: ResourceScope,
	requested: &[AgentType],
) -> Vec<PathBuf> {
	let mut links = Vec::new();
	let _ = dir_has_external_referrer_excluding(
		dir,
		all_agent_dirs,
		name,
		&|link| link_owned_by_requested(link, requested, project_root, scope),
		Some(&mut links),
	);
	links
}

/// Collect and filter inbound links owned by `requested` agents that should be
/// deleted alongside a copy-layout directory.
///
/// Ensures each link still exists on disk, is not duplicated, and its parent
/// is contained in the allow-listed roots.
pub fn plan_owned_inbound_links(
	dir: &Path,
	all_agent_dirs: &[PathBuf],
	roots: &[PathBuf],
	name: &str,
	project_root: Option<&Path>,
	scope: ResourceScope,
	requested: &[AgentType],
) -> Vec<PathBuf> {
	let mut paths = Vec::new();
	let owned = owned_inbound_links(
		dir,
		all_agent_dirs,
		name,
		project_root,
		scope,
		requested,
	);
	for link in owned {
		if std::fs::symlink_metadata(&link).is_ok()
			&& !paths.contains(&link)
			&& link
				.parent()
				.is_some_and(|p| assert_contained(p, roots).is_some())
		{
			paths.push(link);
		}
	}
	paths
}

/// The paths that release a real directory: the directory FIRST, then the
/// inbound links the requested agents own. `execute_removal` skips a link into
/// a directory whose removal failed, so a failed `remove_dir_all` leaves those
/// links in place instead of half-releasing the skill.
/// See docs/history/core-removal.md#dir-delete-failure-left-links-unlinked
pub fn plan_dir_release_paths(
	dir: PathBuf,
	all_agent_dirs: &[PathBuf],
	roots: &[PathBuf],
	name: &str,
	project_root: Option<&Path>,
	scope: crate::models::ResourceScope,
	requested: &[crate::models::AgentType],
) -> Vec<PathBuf> {
	let mut paths = vec![dir];
	let owned = plan_owned_inbound_links(
		&paths[0],
		all_agent_dirs,
		roots,
		name,
		project_root,
		scope,
		requested,
	);
	paths.extend(owned);
	paths
}

/// The one rule for "may a single-agent / comma-list removal delete a real
/// directory skill folder?". Evaluated in order:
/// 1. Inside the `.aghub` store -> [`KeepReason::UniversalMaster`], always.
/// 2. Any in-scope symlink resolves into it (excluding links owned by
///    requested agents in private skills dirs with no outside enabled
///    readers) -> [`KeepReason::ExternalReferrer`].
/// 3. Non-empty `requested` and an ENABLED reader outside `requested` still
///    reads the dir (private or shared) ->
///    [`KeepReason::UniversalMaster`]. Empty `requested` fails closed
///    ONLY for shared Referrer roots (so the plain `ConfigManager::remove_skill`
///    seam, which passes no requested set, keeps deleting private copies).
/// 4. Inside a shared Referrer root and tracked by git -> [`KeepReason::GitTracked`];
///    git unable to answer -> [`KeepReason::GitUndecided`] (see [`shared_slot_git_keep`]).
/// 5. Otherwise `None` (a private copy or untracked shared slot).
///
/// Shared by [`plan_copy_removal`], the symlink-row release in
/// [`plan_removal_for_agents`], `ConfigManager::remove_skill` (which passes no
/// `requested` set, so it stays strict) and the API by-path route; never
/// hand-mirror it.
/// See docs/history/core-removal.md#disabled-agent-blocked-a-single-agent-delete
pub fn single_agent_keep_reason(
	dir: &Path,
	all_agent_dirs: &[PathBuf],
	name: &str,
	project_root: Option<&Path>,
	scope: ResourceScope,
	requested: &[AgentType],
) -> Option<KeepReason> {
	let master_roots: Vec<PathBuf> = skill_store_roots(project_root)
		.into_iter()
		.filter(|root| {
			root.file_name()
				== Some(std::ffi::OsStr::new(
					crate::skills::linker::MASTER_STORE_DIR_NAME,
				))
		})
		.collect();

	if assert_strictly_contained(dir, &master_roots).is_some() {
		return Some(KeepReason::UniversalMaster);
	}
	if let Some(referrer) = dir_has_external_referrer_excluding(
		dir,
		all_agent_dirs,
		name,
		&|link| link_owned_by_requested(link, requested, project_root, scope),
		None,
	) {
		return Some(KeepReason::ExternalReferrer(referrer));
	}
	// The `.aghub` roots already returned above, so any store root hit here is
	// a shared Referrer root.
	if !requested.is_empty()
		&& !skill_dir_readers_outside(dir, scope, project_root, requested)
			.is_empty()
	{
		return Some(KeepReason::UniversalMaster);
	}
	if requested.is_empty() && is_universal_master(dir, project_root) {
		return Some(KeepReason::UniversalMaster);
	}
	shared_slot_git_keep(dir, project_root)
}

/// The git half of the real-directory keep rule, shared by the single-agent
/// verdict and the `--all-agents` sweep so the two cannot disagree: a real
/// directory in a shared Referrer root that git tracks (or that git cannot
/// answer for) is authored source and is never deleted. `None` for anything
/// else, including a symlink — a tracked link is a Referrer, not a directory.
/// See docs/history/core-removal.md#git-probe-had-no-deadline
pub fn shared_slot_git_keep(
	dir: &Path,
	project_root: Option<&Path>,
) -> Option<KeepReason> {
	use crate::skills::shape::{git_tracked, GitTracked};
	if !is_universal_master(dir, project_root)
		|| !std::fs::symlink_metadata(dir).is_ok_and(|meta| meta.is_dir())
	{
		return None;
	}
	match git_tracked(dir) {
		GitTracked::Yes => Some(KeepReason::GitTracked),
		GitTracked::Undecided => Some(KeepReason::GitUndecided),
		GitTracked::No => None,
	}
}

fn push_contained(
	path: PathBuf,
	roots: &[PathBuf],
	paths: &mut Vec<PathBuf>,
	skipped: &mut Vec<PathBuf>,
) {
	if assert_contained(&path, roots).is_some() {
		paths.push(path);
	} else {
		skipped.push(path);
	}
}

/// Outcome of the post-delete lock prune attached to a [`RemovalOutcome`].
///
/// `Failed` is non-fatal (the deletion already happened). A `Both` prune
/// reconciles two independent locks in sequence, so `Failed.pruned` reports
/// global keys already dropped before the project prune failed — never claim
/// "lock unchanged" when it wasn't.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum PruneStatus {
	/// No prune attempted (dry-run / nothing executed).
	#[default]
	NotRun,
	/// Prune ran; the pruned lock keys (empty = nothing orphaned).
	Pruned(Vec<String>),
	/// Prune attempted but failed. `pruned` lists keys actually dropped before
	/// the failure (empty for a single-scope prune; possibly non-empty for the
	/// `Both` scope when the first lock pruned before the second errored).
	Failed { reason: String, pruned: Vec<String> },
	/// A PREVIEW: the keys a committed delete would drop; nothing written.
	/// Distinct from `Pruned` so the shared CLI/API/desktop wire shape never
	/// leaves `outcome` alone to separate "about to" from "did".
	WouldPrune(Vec<String>),
}

/// Result of a planned removal request: the (post-execution) plan plus whether
/// destructive deletion actually ran. `executed == false` means a dry-run or a
/// destructive op awaiting an explicit confirm — nothing was deleted. `prune`
/// records the post-delete lock prune (see [`PruneStatus`]).
#[derive(Debug, Clone)]
pub struct RemovalOutcome {
	pub plan: RemovalPlan,
	pub executed: bool,
	pub prune: PruneStatus,
	/// Paths the executing removal TRIED and failed to delete. `executed` is
	/// true for the whole execute branch and failures are also folded into
	/// `plan.skipped`, so this is the only signal that lets the wire view say
	/// `partial` instead of `removed`.
	/// See docs/history/core-removal.md#partial-removals-reported-as-removed
	pub failed_paths: Vec<std::path::PathBuf>,
	/// The resource was ALREADY GONE. Without it `executed: false` also meant
	/// "preview", whose contract says `--yes` WILL change something. Set only
	/// by [`RemovalOutcome::noop`]. See docs/history/core-removal.md#absent-was-indistinguishable-from-preview
	pub absent: bool,
}

impl RemovalOutcome {
	/// The PREVIEW of `plan` — what a commit would do, including the lock keys
	/// it would drop. The one producer for every surface (root AGENTS.md: never
	/// hand-mirror a transactional flow), and it runs `verify_shape` itself for
	/// the same reason. See docs/history/core-removal.md#preview-and-commit-duplicated-per-surface
	///
	/// `blocks` is the caller's own `read_effect_after` verdict, passed in: the
	/// `shared_master_kept && paths.is_empty()` proxy below cannot see an
	/// npx-era Referrer beside its Master (a real path to unlink, still
	/// refused). A caller with no verdict passes `false`.
	pub fn preview(
		plan: RemovalPlan,
		blocks: bool,
		scope: crate::models::ResourceScope,
		project_root: Option<&Path>,
		name: &str,
	) -> crate::errors::Result<Self> {
		crate::skills::shape::verify_shape(scope, project_root, name)?;
		// A kept Master never reaches `commit` (a single-agent keep refuses; an
		// exhaustive keep returns here as a preview even when confirmed), so a
		// promised prune would never run. `blocks` alone misses the second
		// shape, hence the plan flag.
		let prune =
			if blocks || (plan.shared_master_kept && plan.paths.is_empty()) {
				PruneStatus::NotRun
			} else {
				crate::skills::prune::preview_prune_for_removal(
					scope,
					project_root,
					&plan.paths,
				)
			};
		Ok(Self {
			plan,
			executed: false,
			prune,
			failed_paths: Vec::new(),
			// Callers reach a preview only AFTER their not-found check.
			absent: false,
		})
	}

	/// COMMIT `plan`: run the removal, fold what ACTUALLY happened back into the
	/// plan, and reconcile the per-scope lock. The only place a
	/// [`RemovalReport`] becomes an outcome (same rule as [`Self::preview`]).
	pub fn commit(
		mut plan: RemovalPlan,
		roots: &[PathBuf],
		scope: crate::models::ResourceScope,
		project_root: Option<&Path>,
		name: &str,
	) -> crate::errors::Result<Self> {
		// BEFORE the first delete. The same check the preview ran, from the
		// same producer, so the two cannot disagree — and a refusal here leaves
		// the disk untouched rather than half-removed.
		crate::skills::shape::verify_shape(scope, project_root, name)?;
		let report = execute_removal(&plan, roots)
			.map_err(crate::errors::ConfigError::Io)?;
		for path in &report.skipped {
			log::warn!(
				"skipped removal of '{}' (outside skills roots, or its directory \
				 could not be removed)",
				path.display()
			);
		}
		for (path, error) in &report.failed {
			log::warn!("failed removal of '{}': {}", path.display(), error);
		}
		// Reflect what actually happened on disk in the returned plan.
		plan.paths = report.removed;
		plan.skipped.extend(report.skipped);
		// Kept separately as well as folded into `skipped`: `skipped` also holds
		// paths refused for being outside the allow-list and a shared master
		// deliberately left behind, so it cannot answer "did anything FAIL?".
		let failed_paths: Vec<PathBuf> =
			report.failed.iter().map(|(path, _)| path.clone()).collect();
		plan.skipped
			.extend(report.failed.into_iter().map(|(path, _)| path));
		let prune =
			crate::skills::prune::prune_lock_for_scope(scope, project_root);
		Ok(Self {
			plan,
			executed: true,
			prune,
			failed_paths,
			absent: false,
		})
	}

	/// Idempotent-delete no-op: nothing on disk to remove (missing config or
	/// resource). One constructor so the CLI and API serialize the SAME
	/// "already gone" shape across skill/MCP/sub-agent deletes. `deleted_path`
	/// stays null because `executed` is false.
	pub fn noop() -> Self {
		RemovalOutcome {
			plan: RemovalPlan {
				layout: Layout::Copy,
				paths: vec![],
				skipped: vec![],
				needs_confirm: false,
				shared_master_kept: false,
				still_read_from: Vec::new(),
				incomplete: false,
			},
			executed: false,
			prune: PruneStatus::NotRun,
			failed_paths: vec![],
			absent: true,
		}
	}
}

/// What `execute_removal` actually did on disk.
#[derive(Debug, Default)]
pub struct RemovalReport {
	pub removed: Vec<PathBuf>,
	/// Dirs refused at delete time because they escaped the allow-list (TOCTOU).
	pub skipped: Vec<PathBuf>,
	pub failed: Vec<(PathBuf, std::io::Error)>,
}

/// Execute a [`RemovalPlan`]'s deletions with delete-time safety re-checks:
///
/// - `lstat` each path (never follow the link): a symlink is unlinked with
///   `remove_file` (its target is never touched); a directory is removed with
///   `remove_dir_all` ONLY after re-asserting containment (TOCTOU guard); a file
///   is removed.
/// - A path that has already vanished is tolerated (idempotent).
pub fn execute_removal(
	plan: &RemovalPlan,
	roots: &[PathBuf],
) -> std::io::Result<RemovalReport> {
	let mut report = RemovalReport::default();
	// Directories whose removal failed, resolved while they still exist.
	let mut failed_dirs: Vec<PathBuf> = Vec::new();
	for path in &plan.paths {
		let meta = match std::fs::symlink_metadata(path) {
			Ok(m) => m,
			Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
			Err(e) => {
				report.failed.push((path.clone(), e));
				continue;
			}
		};
		let ft = meta.file_type();
		if Linker::is_link(path) {
			// Keep a link into a directory we failed to delete: unlinking it
			// would revoke a grant while the content is still there.
			// See docs/history/core-removal.md#dir-delete-failure-left-links-unlinked
			if !failed_dirs.is_empty() {
				let target = ::skill::lock::resolve_existing(path);
				if failed_dirs.iter().any(|dir| target.starts_with(dir)) {
					report.skipped.push(path.clone());
					continue;
				}
			}
			match Linker::unlink(path) {
				Ok(()) => report.removed.push(path.clone()),
				Err(e) => report.failed.push((path.clone(), e)),
			}
		} else if ft.is_dir() {
			// Re-assert STRICT containment immediately before remove_dir_all: the
			// root itself must never be deleted.
			if assert_strictly_contained(path, roots).is_some() {
				match std::fs::remove_dir_all(path) {
					Ok(()) => report.removed.push(path.clone()),
					Err(e) => {
						failed_dirs.push(::skill::lock::resolve_existing(path));
						report.failed.push((path.clone(), e));
					}
				}
			} else {
				report.skipped.push(path.clone());
			}
		} else {
			match std::fs::remove_file(path) {
				Ok(()) => report.removed.push(path.clone()),
				Err(e) => report.failed.push((path.clone(), e)),
			}
		}
	}
	Ok(report)
}

/// The dirs in `dirs` that NO managed agent reads, at either scope.
///
/// A disabled agent the request NAMES counts as managed here: naming it is the
/// consent a sweep otherwise lacks, and skipping its dir anyway made a request
/// that listed every holder refuse itself. `--all-agents` alone names only the
/// initiator, so it still never touches a disabled agent it was not told about.
/// See docs/history/core-removal.md#naming-a-disabled-agent-was-not-consent
///
/// Spelled by the same adapter call as [`agent_skill_dirs_in_scope`], so a
/// plain `contains` matches. Empty when nothing is disabled.
pub(crate) fn unmanaged_skill_dirs(
	dirs: &[PathBuf],
	project_root: Option<&Path>,
	requested: &[crate::models::AgentType],
) -> Vec<PathBuf> {
	use crate::models::ResourceScope;
	let disabled = crate::agent_settings::disabled_agents();
	if disabled.is_empty() {
		return Vec::new();
	}
	let mut managed: Vec<PathBuf> = Vec::new();
	for agent in crate::models::AgentType::ALL.iter().filter(|agent| {
		!disabled.contains(agent.as_str()) || requested.contains(agent)
	}) {
		let adapter = crate::create_adapter(*agent);
		managed
			.extend(adapter.get_skills_paths(None, ResourceScope::GlobalOnly));
		if project_root.is_some() {
			managed.extend(
				adapter
					.get_skills_paths(project_root, ResourceScope::ProjectOnly),
			);
		}
	}
	dirs.iter()
		.filter(|dir| !managed.contains(dir))
		.cloned()
		.collect()
}

/// Union of every agent's skill read dirs for a resource scope — the set the
/// removal planner sweeps and the prune scanner reconciles against.
pub fn agent_skill_dirs_in_scope(
	scope: crate::models::ResourceScope,
	project_root: Option<&Path>,
) -> Vec<PathBuf> {
	let mut dirs: Vec<PathBuf> = Vec::new();
	for agent in crate::models::AgentType::ALL {
		let adapter = crate::create_adapter(*agent);
		for dir in adapter.get_skills_paths(project_root, scope) {
			if !dirs.contains(&dir) {
				dirs.push(dir);
			}
		}
	}
	dirs
}

#[cfg(test)]
pub(crate) mod tests {
	use super::*;
	use tempfile::tempdir;

	fn write_named_skill(dir: &Path, name: &str) {
		std::fs::create_dir_all(dir).unwrap();
		std::fs::write(
			dir.join("SKILL.md"),
			format!("---\nname: {name}\ndescription: d\n---\n"),
		)
		.unwrap();
	}

	#[test]
	fn by_path_skill_dir_accepts_dir_and_skill_md_refuses_other_files() {
		let tmp = tempdir().unwrap();
		let skill = tmp.path().join("z");
		write_named_skill(&skill, "z");
		std::fs::create_dir_all(skill.join("scripts")).unwrap();
		std::fs::write(skill.join("scripts/run.sh"), "x").unwrap();

		assert_eq!(by_path_skill_dir(&skill), Ok(skill.clone()));
		assert_eq!(
			by_path_skill_dir(&skill.join("SKILL.md")),
			Ok(skill.clone())
		);
		// A missing SKILL.md still names its parent: the caller answers "absent".
		assert_eq!(
			by_path_skill_dir(&tmp.path().join("gone/SKILL.md")),
			Ok(tmp.path().join("gone"))
		);
		// Any other file must not resolve to its parent (the skill, or `scripts`).
		for stray in [skill.join("scripts/run.sh"), skill.join("gone.txt")] {
			assert_eq!(
				by_path_skill_dir(&stray),
				Err(ByPathRefusal::NotSkillRoot),
				"{}",
				stray.display()
			);
		}
	}

	#[cfg(unix)]
	#[test]
	fn by_path_skill_dir_refuses_a_symlink_loop_as_unresolvable() {
		let tmp = tempdir().unwrap();
		let looped = tmp.path().join("loop");
		std::os::unix::fs::symlink("loop", &looped).unwrap();
		for target in [looped.clone(), looped.join("SKILL.md")] {
			assert_eq!(
				by_path_skill_dir(&target),
				Err(ByPathRefusal::Unresolvable)
			);
		}
		assert!(!ByPathRefusal::Unresolvable
			.to_string()
			.contains(&tmp.path().display().to_string()));
	}

	#[test]
	fn by_path_skill_name_needs_its_own_parsable_skill_md() {
		let tmp = tempdir().unwrap();
		// Category folder: skills only underneath, no root SKILL.md.
		write_named_skill(&tmp.path().join("team/a"), "a");
		assert_eq!(
			by_path_skill_name(&tmp.path().join("team")),
			Err(ByPathRefusal::NotSkillRoot)
		);
		// Unparsable SKILL.md does not fall back to the folder name.
		let broken = tmp.path().join("broken");
		std::fs::create_dir_all(&broken).unwrap();
		std::fs::write(broken.join("SKILL.md"), "no frontmatter").unwrap();
		assert_eq!(
			by_path_skill_name(&broken),
			Err(ByPathRefusal::NotSkillRoot)
		);
		// The folder must be the name's slot name.
		write_named_skill(&tmp.path().join("real-name"), "real-name");
		assert_eq!(
			by_path_skill_name(&tmp.path().join("real-name")),
			Ok("real-name".to_string())
		);
		write_named_skill(&tmp.path().join("my-skill"), "My Skill");
		assert_eq!(
			by_path_skill_name(&tmp.path().join("my-skill")),
			Ok("My Skill".to_string())
		);
		write_named_skill(&tmp.path().join("folder-v2"), "real-name");
		assert_eq!(
			by_path_skill_name(&tmp.path().join("folder-v2")),
			Err(ByPathRefusal::NotSkillRoot),
			"a folder that is not the name's slot is refused"
		);
	}

	/// Shared git-fixture helpers for tests that need a real git index.
	/// Used by both `removal::tests` and `manager::skill::tests`.
	#[cfg(unix)]
	pub(crate) mod git_fixture {
		use std::path::Path;

		/// Returns `true` when the `git` binary is on `PATH`.
		pub(crate) fn has_git() -> bool {
			std::process::Command::new("git")
				.arg("--version")
				.stdout(std::process::Stdio::null())
				.stderr(std::process::Stdio::null())
				.status()
				.map(|s| s.success())
				.unwrap_or(false)
		}

		/// Run `git -C <root> <args>` with developer-global config
		/// isolated out. Panics on failure (call only after
		/// [`has_git`] returned `true`).
		pub(crate) fn git(root: &Path, args: &[&str]) {
			let ok = std::process::Command::new("git")
				.arg("-C")
				.arg(root)
				.args(args)
				.env("GIT_CONFIG_GLOBAL", "/dev/null")
				.env("GIT_CONFIG_NOSYSTEM", "1")
				.env("HOME", root)
				.stdout(std::process::Stdio::null())
				.stderr(std::process::Stdio::null())
				.status()
				.expect("failed to spawn git")
				.success();
			assert!(ok, "git -C {} {} failed", root.display(), args.join(" "));
		}
	}

	#[test]
	fn contained_path_is_accepted() {
		let root = tempdir().unwrap();
		let sub = root.path().join("skills/a");
		std::fs::create_dir_all(&sub).unwrap();
		let roots = vec![root.path().to_path_buf()];
		assert_eq!(
			assert_contained(&sub, &roots),
			Some(sub.canonicalize().unwrap())
		);
	}

	#[test]
	fn strict_containment_rejects_root_itself() {
		let root = tempdir().unwrap();
		let sub = root.path().join("skills/a");
		std::fs::create_dir_all(&sub).unwrap();
		let roots = vec![root.path().to_path_buf()];

		assert_eq!(
			assert_strictly_contained(&sub, &roots),
			Some(sub.canonicalize().unwrap())
		);
		assert_eq!(assert_strictly_contained(root.path(), &roots), None);
		assert!(assert_targets_strictly_contained(
			&[root.path().to_path_buf()],
			&roots,
			None,
		)
		.is_err());
	}

	#[test]
	fn assert_strictly_contained_rejects_inner_root_nested_in_another_root() {
		let tmp = tempdir().unwrap();
		let outer = tmp.path().join("outer");
		let inner = outer.join("inner");
		let skill = inner.join("skill");
		std::fs::create_dir_all(&skill).unwrap();
		let roots = vec![outer.clone(), inner.clone()];

		assert_eq!(assert_strictly_contained(&inner, &roots), None);
		assert_eq!(assert_strictly_contained(&outer, &roots), None);
		assert_eq!(
			assert_strictly_contained(&skill, &roots),
			Some(skill.canonicalize().unwrap())
		);
	}

	#[test]
	fn outside_path_is_rejected() {
		let root = tempdir().unwrap();
		let outside = tempdir().unwrap();
		std::fs::create_dir_all(outside.path().join("x")).unwrap();
		let roots = vec![root.path().to_path_buf()];
		assert_eq!(assert_contained(&outside.path().join("x"), &roots), None);
	}

	#[cfg(unix)]
	#[test]
	fn symlink_escaping_root_is_rejected() {
		use std::os::unix::fs::symlink;
		let root = tempdir().unwrap();
		let outside = tempdir().unwrap();
		std::fs::create_dir_all(outside.path().join("evil")).unwrap();
		let link = root.path().join("evil");
		symlink(outside.path().join("evil"), &link).unwrap();
		let roots = vec![root.path().to_path_buf()];
		// canonicalize escapes root → rejected.
		assert_eq!(assert_contained(&link, &roots), None);
	}

	// macOS simulation: when the ROOT itself is reached through a symlink
	// (like /tmp -> /private/tmp on macOS), assert_contained canonicalizes
	// both target and root, so a legitimate path under that root is still
	// accepted. Guards against the /var->/private prefix shift breaking
	// containment for real skills — the class of bug that broke tarball
	// extraction on macOS/Windows.
	#[cfg(unix)]
	#[test]
	fn legit_target_under_symlinked_root_is_accepted() {
		use std::os::unix::fs::symlink;
		let real = tempdir().unwrap();
		let link_parent = tempdir().unwrap();
		let link_root = link_parent.path().join("root-link");
		symlink(real.path(), &link_root).unwrap();
		let sub = link_root.join("skills/a");
		std::fs::create_dir_all(&sub).unwrap();
		// Root supplied via the symlinked path (mimicking macOS /tmp).
		let roots = vec![link_root.clone()];
		assert_eq!(
			assert_contained(&sub, &roots),
			Some(sub.canonicalize().unwrap())
		);
	}

	#[test]
	fn allowed_roots_include_existing_agent_dirs() {
		let agent = tempdir().unwrap();
		let agent_skills = agent.path().join("skills");
		std::fs::create_dir_all(&agent_skills).unwrap();
		let roots =
			allowed_skill_roots(std::slice::from_ref(&agent_skills), None);
		let canonical = agent_skills.canonicalize().unwrap();
		assert!(
			roots.contains(&canonical),
			"agent skills dir must be an allowed root"
		);
	}

	#[test]
	fn allowed_roots_skip_nonexistent_dirs() {
		let agent = tempdir().unwrap();
		let missing = agent.path().join("does-not-exist");
		let roots = allowed_skill_roots(std::slice::from_ref(&missing), None);
		assert!(
			!roots.iter().any(|r| r.ends_with("does-not-exist")),
			"non-existent dirs are not returned (canonicalize fails)"
		);
	}

	// ---- plan_removal -------------------------------------------------------

	use crate::models::Skill;
	#[cfg(unix)]
	use std::path::PathBuf;

	// A Referrer is found by IDENTITY, not by name — so one whose target's
	// `SKILL.md` will not parse must still be seen.
	//
	// The name-matched list is built from parsed `Skill`s, so a target that
	// fails to parse produces no entry at all: the grouped link below was
	// invisible, the Master it points at went to `remove_dir_all`, and the link
	// was left dangling. Non-UTF-8 bytes, not a permission bit: the file is
	// perfectly readable, its CONTENT is what will not parse, and that is the
	// case a "can we read it?" check cannot catch.
	#[cfg(unix)]
	#[test]
	fn a_referrer_whose_target_will_not_parse_is_still_seen() {
		let tmp = tempdir().unwrap();
		let master = tmp.path().join(".agents/skills/demo");
		std::fs::create_dir_all(&master).unwrap();
		std::fs::write(master.join("SKILL.md"), [0xff]).unwrap();

		let peer = tmp.path().join(".gemini/skills");
		let group = peer.join("team");
		std::fs::create_dir_all(&group).unwrap();
		let referrer = group.join("legacy");
		symlink(&master, &referrer);

		assert_eq!(
			dir_has_external_referrer(
				&master,
				std::slice::from_ref(&peer),
				"demo"
			),
			Some(referrer),
			"a grouped referrer must be found by what it POINTS AT, not by a \
			 frontmatter name nothing can read"
		);
	}

	// The wide referrer list may not become a wide DELETION list.
	//
	// A dangling link resolves to nothing, so the identity check that clears
	// every other entry cannot clear it — and the sweep now iterates every
	// entry in the agent's skills dir, not just the ones this skill names. On
	// "is a link, and we are targeted" alone it unlinked an unrelated broken
	// link belonging to a different skill, and counted having done so as the
	// removal taking effect.
	#[cfg(unix)]
	#[test]
	fn a_sweep_may_not_unlink_an_unrelated_dangling_link() {
		let tmp = tempdir().unwrap();
		let canonical = tmp.path().join(".agents/skills/foo");
		write_skill_md(&canonical);
		let claude = tmp.path().join(".claude/skills");
		std::fs::create_dir_all(&claude).unwrap();
		symlink(&canonical, &claude.join("foo"));
		// Broken, and nothing to do with `foo`.
		let stray = claude.join("other");
		symlink(&tmp.path().join("nowhere"), &stray);

		let skill = symlink_skill(&canonical, &claude);
		let plan = plan_removal(
			&skill,
			Some(claude.as_path()),
			std::slice::from_ref(&claude),
			Some(tmp.path()),
			false,
		);

		assert!(
			!plan.paths.contains(&stray),
			"a broken link belonging to another skill must survive: {:?}",
			plan.paths
		);
		assert!(
			plan.paths.contains(&claude.join("foo")),
			"and the targeted referrer must still go: {:?}",
			plan.paths
		);
		assert!(
			stray.symlink_metadata().is_ok(),
			"fixture: the stray link must exist for this to prove anything"
		);
	}

	fn write_skill_md(dir: &Path) {
		std::fs::create_dir_all(dir).unwrap();
		std::fs::write(
			dir.join("SKILL.md"),
			"---\nname: foo\ndescription: d\n---\n",
		)
		.unwrap();
	}

	#[cfg(unix)]
	fn symlink(target: &Path, link: &Path) {
		std::os::unix::fs::symlink(target, link).unwrap();
	}

	/// Build a project-scope symlink layout under `tmp`:
	/// canonical `<tmp>/.agents/skills/foo` + per-agent symlinks. Returns the
	/// canonical dir and the agent skills dirs (claude, cursor).
	#[cfg(unix)]
	fn symlink_layout(tmp: &Path) -> (PathBuf, Vec<PathBuf>) {
		let canonical = tmp.join(".agents/skills/foo");
		write_skill_md(&canonical);
		let claude = tmp.join(".claude/skills");
		let cursor = tmp.join(".cursor/skills");
		std::fs::create_dir_all(&claude).unwrap();
		std::fs::create_dir_all(&cursor).unwrap();
		symlink(&canonical, &claude.join("foo"));
		symlink(&canonical, &cursor.join("foo"));
		(canonical, vec![claude, cursor])
	}

	fn symlink_skill(canonical: &Path, source: &Path) -> Skill {
		let mut s = Skill::new("foo");
		s.canonical_path =
			Some(canonical.join("SKILL.md").to_string_lossy().to_string());
		s.source_path =
			Some(source.join("SKILL.md").to_string_lossy().to_string());
		s
	}

	#[test]
	fn skill_root_unchecked_takes_parent_of_canonical_skill_md() {
		// Pins reuse of the single shared resolver (no 4th tilde copy) + the
		// "canonical is a FILE path -> take PARENT dir" rule.
		let tmp = tempdir().unwrap();
		let canonical = tmp.path().join(".agents/skills/foo");
		write_skill_md(&canonical);
		let skill = symlink_skill(&canonical, &canonical);
		assert_eq!(
			crate::transfer::skill_root_unchecked(&skill),
			Some(canonical)
		);
	}

	#[cfg(unix)]
	#[test]
	fn plan_removal_symlink_all_agents_collects_canonical_and_all_symlinks() {
		let tmp = tempdir().unwrap();
		let (canonical, agent_dirs) = symlink_layout(tmp.path());
		let skill = symlink_skill(&canonical, &agent_dirs[0]);
		let plan =
			plan_removal(&skill, None, &agent_dirs, Some(tmp.path()), true);
		assert_eq!(plan.layout, Layout::Symlink);
		assert!(plan.needs_confirm);
		assert!(plan.skipped.is_empty(), "all in-tree: {:?}", plan.skipped);
		assert!(plan.paths.contains(&canonical), "canonical removed");
		assert!(plan.paths.contains(&agent_dirs[0].join("foo")));
		assert!(plan.paths.contains(&agent_dirs[1].join("foo")));
		assert_eq!(plan.paths.len(), 3);
	}

	/// "Every agent" is every MANAGED agent: a disabled agent's Referrer is
	/// neither unlinked nor ignored — it still holds the Master in place.
	#[cfg(unix)]
	#[test]
	fn plan_removal_all_agents_skips_and_keeps_for_a_disabled_agent() {
		let tmp = tempdir().unwrap();
		let (canonical, agent_dirs) = symlink_layout(tmp.path());
		let skill = symlink_skill(&canonical, &agent_dirs[0]);
		let _off = crate::agent_settings::test_override::disable(&["cursor"]);
		let plan =
			plan_removal(&skill, None, &agent_dirs, Some(tmp.path()), true);
		assert_eq!(plan.paths, vec![agent_dirs[0].join("foo")]);
		assert!(
			plan.skipped.contains(&canonical),
			"the disabled agent still reads the Master: {:?}",
			plan.skipped
		);
	}

	/// Naming a disabled agent in the request is consent to touch it: a sweep
	/// that skipped its dir anyway made a reconcile that listed EVERY holder
	/// (the disabled ones included) refuse all rows and take nothing.
	/// See docs/history/core-removal.md#naming-a-disabled-agent-was-not-consent
	#[cfg(unix)]
	#[test]
	fn plan_removal_all_agents_sweeps_a_disabled_agent_the_request_names() {
		use crate::models::{AgentType, ResourceScope};
		let tmp = tempdir().unwrap();
		let (canonical, agent_dirs) = symlink_layout(tmp.path());
		let skill = symlink_skill(&canonical, &agent_dirs[0]);
		let _off = crate::agent_settings::test_override::disable(&["cursor"]);
		let plan = plan_removal_for_agents(
			&skill,
			None,
			&agent_dirs,
			Some(tmp.path()),
			ResourceScope::Both,
			true,
			&[AgentType::Claude, AgentType::Cursor],
		);
		assert!(
			plan.paths.contains(&agent_dirs[1].join("foo")),
			"the named disabled agent's Referrer is unlinked: {:?}",
			plan.paths
		);
		assert!(
			plan.paths.contains(&canonical) && plan.skipped.is_empty(),
			"no reader is left, so the Master goes too: {:?} / {:?}",
			plan.paths,
			plan.skipped
		);
	}

	#[cfg(unix)]
	#[test]
	fn plan_removal_out_of_tree_symlink_canonical_is_skipped_not_in_paths() {
		let tmp = tempdir().unwrap();
		// Canonical lives OUTSIDE every allow-listed skills root.
		let outside = tmp.path().join("outside/foo");
		write_skill_md(&outside);
		let claude = tmp.path().join(".claude/skills");
		std::fs::create_dir_all(&claude).unwrap();
		symlink(&outside, &claude.join("foo"));
		let agent_dirs = vec![claude.clone()];
		let skill = symlink_skill(&outside, &claude);
		let plan =
			plan_removal(&skill, None, &agent_dirs, Some(tmp.path()), true);
		// The symlink itself is unlinked (safe), but the out-of-tree canonical
		// dir is NOT scheduled for remove_dir_all.
		assert!(plan.paths.contains(&claude.join("foo")));
		assert!(
			!plan.paths.contains(&outside),
			"out-of-tree must not delete"
		);
		assert!(plan.skipped.iter().any(|p| p == &outside));
	}

	#[cfg(unix)]
	#[test]
	fn plan_removal_keeps_canonical_when_another_view_still_references_it() {
		let tmp = tempdir().unwrap();
		let (canonical, agent_dirs) = symlink_layout(tmp.path());
		let claude = &agent_dirs[0];
		let skill = symlink_skill(&canonical, claude);
		// Single-agent removal targeting claude only; cursor symlink remains.
		let plan = plan_removal(
			&skill,
			Some(claude.as_path()),
			&agent_dirs,
			Some(tmp.path()),
			false,
		);
		assert!(plan.paths.contains(&claude.join("foo")));
		assert!(
			!plan.paths.contains(&canonical),
			"canonical kept while cursor still symlinks to it"
		);
		assert!(!plan.paths.contains(&agent_dirs[1].join("foo")));
	}

	#[cfg(unix)]
	#[test]
	fn plan_removal_keeps_canonical_when_direct_reader_still_references_it() {
		let tmp = tempdir().unwrap();
		let canonical = tmp.path().join(".agents/skills/foo");
		write_skill_md(&canonical);
		let claude = tmp.path().join(".claude/skills");
		let universal = tmp.path().join(".agents/skills");
		std::fs::create_dir_all(&claude).unwrap();
		symlink(&canonical, &claude.join("foo"));
		let agent_dirs = vec![claude.clone(), universal.clone()];
		let skill = symlink_skill(&canonical, &claude);

		let plan = plan_removal(
			&skill,
			Some(claude.as_path()),
			&agent_dirs,
			Some(tmp.path()),
			false,
		);

		assert!(plan.paths.contains(&claude.join("foo")));
		assert!(
			!plan.paths.contains(&canonical),
			"canonical kept while direct reader still resolves to it"
		);
		assert!(plan.skipped.iter().any(|p| p == &canonical));
	}

	#[cfg(unix)]
	#[test]
	fn plan_removal_no_target_does_not_schedule_canonical() {
		let tmp = tempdir().unwrap();
		let canonical = tmp.path().join(".agents/skills/foo");
		write_skill_md(&canonical);
		let claude = tmp.path().join(".claude/skills");
		std::fs::create_dir_all(&claude).unwrap();
		let agent_dirs = vec![claude.clone()];
		let skill = symlink_skill(&canonical, &claude);

		let plan = plan_removal(
			&skill,
			Some(claude.as_path()),
			&agent_dirs,
			Some(tmp.path()),
			false,
		);

		assert!(plan.paths.is_empty());
		assert!(
			!plan.paths.contains(&canonical),
			"canonical must not be scheduled when no target matched"
		);
	}

	#[cfg(unix)]
	#[test]
	fn plan_removal_canonicalize_failure_keeps_canonical_not_no_match() {
		let tmp = tempdir().unwrap();
		let canonical = tmp.path().join(".agents/skills/foo");
		write_skill_md(&canonical);
		let claude = tmp.path().join(".claude/skills");
		let cursor = tmp.path().join(".cursor/skills");
		std::fs::create_dir_all(&claude).unwrap();
		std::fs::create_dir_all(&cursor).unwrap();
		symlink(&canonical, &claude.join("foo"));
		// ENOTDIR is an inspection failure, unlike a definitively absent target.
		let opaque = tmp.path().join("not-a-directory");
		std::fs::write(&opaque, "file").unwrap();
		symlink(&opaque.join("foo"), &cursor.join("other"));
		let agent_dirs = vec![claude.clone(), cursor.clone()];
		let skill = symlink_skill(&canonical, &claude);
		let plan =
			plan_removal(&skill, None, &agent_dirs, Some(tmp.path()), true);
		assert!(
			!plan.paths.contains(&canonical),
			"canonicalize failure => conservatively keep canonical"
		);
		assert!(plan.skipped.iter().any(|p| p == &canonical));
		std::fs::remove_file(claude.join("foo")).unwrap();
		let orphan_plan =
			plan_removal(&skill, None, &agent_dirs, Some(tmp.path()), true);
		assert!(orphan_plan.paths.is_empty());
		assert!(
			orphan_plan.shared_master_kept,
			"an unlinked Master blocked by an inspection error is kept, not removed"
		);
	}

	#[test]
	fn plan_removal_copy_single_agent_removes_only_targeted_copy() {
		let tmp = tempdir().unwrap();
		let claude = tmp.path().join(".claude/skills");
		let cursor = tmp.path().join(".cursor/skills");
		write_skill_md(&claude.join("foo"));
		write_skill_md(&cursor.join("foo"));
		let agent_dirs = vec![claude.clone(), cursor.clone()];
		let mut skill = Skill::new("foo");
		skill.source_path =
			Some(claude.join("foo/SKILL.md").to_string_lossy().to_string());
		// canonical_path None => copy layout
		let plan = plan_removal(
			&skill,
			Some(claude.as_path()),
			&agent_dirs,
			Some(tmp.path()),
			false,
		);
		assert_eq!(plan.layout, Layout::Copy);
		assert!(!plan.needs_confirm);
		assert!(plan.paths.contains(&claude.join("foo")));
		assert!(
			!plan.paths.contains(&cursor.join("foo")),
			"other agent copy untouched"
		);
	}

	#[cfg(unix)]
	#[test]
	fn plan_removal_copy_keeps_master_when_another_agent_symlinks_to_it() {
		// A universal `.agents/skills/<name>` master read DIRECTLY (as a real
		// dir) by one agent has canonical_path=None -> classified Copy layout.
		// A single-agent removal must NOT `remove_dir_all` that master while
		// another agent's symlink still resolves to it (that would orphan the
		// link + lose the shared skill for every other agent).
		let tmp = tempdir().unwrap();
		let master = tmp.path().join(".agents/skills/foo");
		write_skill_md(&master);
		let universal = tmp.path().join(".agents/skills");
		let claude = tmp.path().join(".claude/skills");
		std::fs::create_dir_all(&claude).unwrap();
		symlink(&master, &claude.join("foo"));

		let agent_dirs = vec![universal.clone(), claude.clone()];
		let mut skill = Skill::new("foo");
		// Direct reader → source_path is the master, canonical_path is None.
		skill.source_path =
			Some(master.join("SKILL.md").to_string_lossy().to_string());

		let plan = plan_removal(
			&skill,
			Some(universal.as_path()),
			&agent_dirs,
			Some(tmp.path()),
			false,
		);

		assert_eq!(plan.layout, Layout::Copy);
		assert!(
			!plan.paths.contains(&master),
			"must NOT delete a shared master another agent symlinks to: {:?}",
			plan.paths
		);
		assert!(
			plan.skipped.iter().any(|p| p == &master),
			"kept master should be reported as skipped, got {:?}",
			plan.skipped
		);
	}

	#[test]
	fn plan_removal_copy_all_agents_removes_every_copy() {
		let tmp = tempdir().unwrap();
		let claude = tmp.path().join(".claude/skills");
		let cursor = tmp.path().join(".cursor/skills");
		write_skill_md(&claude.join("foo"));
		write_skill_md(&cursor.join("foo"));
		let agent_dirs = vec![claude.clone(), cursor.clone()];
		let mut skill = Skill::new("foo");
		skill.source_path =
			Some(claude.join("foo/SKILL.md").to_string_lossy().to_string());
		let plan = plan_removal(
			&skill,
			Some(claude.as_path()),
			&agent_dirs,
			Some(tmp.path()),
			true,
		);
		assert_eq!(plan.layout, Layout::Copy);
		assert!(plan.needs_confirm);
		assert!(plan.paths.contains(&claude.join("foo")));
		assert!(plan.paths.contains(&cursor.join("foo")));
		assert_eq!(plan.paths.len(), 2);
	}

	// ---- execute_removal (delete-time TOCTOU mechanics) --------------------

	#[cfg(unix)]
	#[test]
	fn execute_removal_unlinks_symlink_and_preserves_target() {
		let tmp = tempdir().unwrap();
		let canonical = tmp.path().join(".agents/skills/foo");
		write_skill_md(&canonical);
		let claude = tmp.path().join(".claude/skills");
		std::fs::create_dir_all(&claude).unwrap();
		symlink(&canonical, &claude.join("foo"));
		let plan = RemovalPlan {
			layout: Layout::Symlink,
			paths: vec![claude.join("foo")],
			skipped: vec![],
			needs_confirm: true,
			shared_master_kept: false,
			still_read_from: Vec::new(),
			incomplete: false,
		};
		let report =
			execute_removal(&plan, std::slice::from_ref(&claude)).unwrap();
		assert!(
			std::fs::symlink_metadata(claude.join("foo")).is_err(),
			"symlink unlinked"
		);
		assert!(canonical.join("SKILL.md").exists(), "target preserved");
		assert_eq!(report.removed, vec![claude.join("foo")]);
	}

	#[test]
	fn execute_removal_remove_dir_all_for_contained_dir() {
		let tmp = tempdir().unwrap();
		let skills = tmp.path().join("skills");
		let foo = skills.join("foo");
		write_skill_md(&foo);
		let plan = RemovalPlan {
			layout: Layout::Copy,
			paths: vec![foo.clone()],
			skipped: vec![],
			needs_confirm: false,
			shared_master_kept: false,
			still_read_from: Vec::new(),
			incomplete: false,
		};
		execute_removal(&plan, std::slice::from_ref(&skills)).unwrap();
		assert!(!foo.exists());
	}

	#[test]
	fn execute_removal_refuses_root_itself() {
		let tmp = tempdir().unwrap();
		let root = tmp.path().join("skills");
		let child = root.join("child");
		write_skill_md(&child);
		let plan = RemovalPlan {
			layout: Layout::Copy,
			paths: vec![root.clone(), child.clone()],
			skipped: vec![],
			needs_confirm: false,
			shared_master_kept: false,
			still_read_from: Vec::new(),
			incomplete: false,
		};
		let report =
			execute_removal(&plan, std::slice::from_ref(&root)).unwrap();
		assert!(root.exists(), "root itself must survive");
		assert!(report.skipped.contains(&root));
		assert!(!report.removed.contains(&root));
		assert!(!child.exists(), "child skill dir must be removed");
		assert!(report.removed.contains(&child));
	}

	#[test]
	fn execute_removal_skips_dir_outside_allowlist_toctou() {
		let tmp = tempdir().unwrap();
		let outside = tmp.path().join("outside/foo");
		write_skill_md(&outside);
		let skills = tmp.path().join("skills");
		std::fs::create_dir_all(&skills).unwrap();
		let plan = RemovalPlan {
			layout: Layout::Copy,
			paths: vec![outside.clone()],
			skipped: vec![],
			needs_confirm: false,
			shared_master_kept: false,
			still_read_from: Vec::new(),
			incomplete: false,
		};
		let report = execute_removal(&plan, &[skills]).unwrap();
		assert!(outside.exists(), "out-of-allowlist dir must survive");
		assert!(report.skipped.contains(&outside));
		assert!(report.removed.is_empty());
	}

	#[test]
	fn execute_removal_idempotent_when_path_missing() {
		let tmp = tempdir().unwrap();
		let missing = tmp.path().join("skills/gone");
		let plan = RemovalPlan {
			layout: Layout::Copy,
			paths: vec![missing],
			skipped: vec![],
			needs_confirm: false,
			shared_master_kept: false,
			still_read_from: Vec::new(),
			incomplete: false,
		};
		let report =
			execute_removal(&plan, &[tmp.path().to_path_buf()]).unwrap();
		assert!(report.removed.is_empty());
	}

	#[cfg(unix)]
	#[test]
	fn execute_removal_continues_after_one_failure_and_reports() {
		use std::os::unix::fs::PermissionsExt;

		let tmp = tempdir().unwrap();
		let root = tmp.path().join("skills");
		std::fs::create_dir_all(&root).unwrap();
		let first = root.join("first");
		let second = root.join("second");
		std::fs::write(&first, "first").unwrap();
		std::fs::write(&second, "second").unwrap();

		let blocked_parent = tmp.path().join("blocked");
		std::fs::create_dir_all(&blocked_parent).unwrap();
		let blocked = blocked_parent.join("blocked");
		std::fs::write(&blocked, "blocked").unwrap();
		let original_perms =
			std::fs::metadata(&blocked_parent).unwrap().permissions();
		std::fs::set_permissions(
			&blocked_parent,
			std::fs::Permissions::from_mode(0o500),
		)
		.unwrap();

		let plan = RemovalPlan {
			layout: Layout::Copy,
			paths: vec![first.clone(), blocked.clone(), second.clone()],
			skipped: vec![],
			needs_confirm: false,
			shared_master_kept: false,
			still_read_from: Vec::new(),
			incomplete: false,
		};
		let report =
			execute_removal(&plan, &[root.clone(), blocked_parent.clone()])
				.unwrap();
		std::fs::set_permissions(&blocked_parent, original_perms).unwrap();

		assert!(report.removed.contains(&first));
		assert!(report.removed.contains(&second));
		assert_eq!(report.failed.len(), 1);
		assert_eq!(report.failed[0].0, blocked);
		assert!(!first.exists());
		assert!(!second.exists());
		assert!(blocked.exists());
	}

	/// `commit` runs the shape check BEFORE the first delete.
	///
	/// This is a producer-level test on purpose. No CLI path can reach it: for
	/// every forked-copy fixture the `blocks` verdict in `remove_skill_planned`
	/// refuses earlier, so a CLI test of this would pass with the check deleted
	/// — it did, which is how this test came to live here instead. The API's
	/// `DELETE /skills/by-path` route builds its plan by hand and calls this
	/// producer DIRECTLY, bypassing that guard entirely, so the check has a real
	/// caller and needs a test that can actually fail.
	#[cfg(unix)]
	#[test]
	fn commit_refuses_a_forked_copy_before_deleting_anything() {
		let root = tempfile::tempdir().unwrap();
		let name = "forked-at-commit";

		let master = root.path().join(".aghub").join(name);
		std::fs::create_dir_all(&master).unwrap();
		std::fs::write(master.join("SKILL.md"), "master bytes").unwrap();

		// npx's `cleanAndCreateDirectory`: the Referrer is a real directory
		// holding content that exists nowhere else.
		let forked = root.path().join(".agents").join("skills").join(name);
		std::fs::create_dir_all(&forked).unwrap();
		std::fs::write(forked.join("SKILL.md"), "bytes only here").unwrap();

		let plan = RemovalPlan {
			layout: Layout::Copy,
			// The plan WOULD delete the forked directory. If the check ran
			// after `execute_removal` instead of before it, these bytes would
			// already be gone by the time the error was returned.
			paths: vec![forked.clone()],
			skipped: vec![],
			needs_confirm: false,
			shared_master_kept: false,
			still_read_from: Vec::new(),
			incomplete: false,
		};

		let result = RemovalOutcome::commit(
			plan,
			&[root.path().to_path_buf()],
			crate::models::ResourceScope::ProjectOnly,
			Some(root.path()),
			name,
		);

		assert!(
			result.is_err(),
			"a forked copy must refuse the commit, not be deleted"
		);
		assert_eq!(
			std::fs::read_to_string(forked.join("SKILL.md")).unwrap(),
			"bytes only here",
			"the refusal must land BEFORE execute_removal — these bytes exist \
			 nowhere else"
		);
		assert!(master.join("SKILL.md").is_file());
	}

	#[test]
	fn prune_status_default_is_not_run() {
		assert_eq!(PruneStatus::default(), PruneStatus::NotRun);
	}

	#[test]
	fn removal_outcome_carries_prune_field() {
		let outcome = RemovalOutcome {
			plan: RemovalPlan {
				layout: Layout::Copy,
				paths: vec![],
				skipped: vec![],
				needs_confirm: false,
				shared_master_kept: false,
				still_read_from: Vec::new(),
				incomplete: false,
			},
			executed: false,
			prune: PruneStatus::Pruned(vec!["a".to_string()]),
			failed_paths: vec![],
			absent: false,
		};
		assert_eq!(outcome.prune, PruneStatus::Pruned(vec!["a".to_string()]));
	}

	#[test]
	fn noop_is_idempotent_delete_success_shape() {
		// Pins the idempotent-delete contract (the single `RemovalOutcome::noop`
		// the CLI `plan_or_noop` and the API no-op both serialize): deleting an
		// absent resource is a SUCCESS no-op — executed:false, no paths, so the
		// wire `deleted_path` stays null. NOT an error.
		let n = RemovalOutcome::noop();
		assert!(!n.executed, "a no-op delete must not report execution");
		assert!(
			n.absent,
			"this IS the already-gone constructor: without `absent` the wire \
			 view can only guess from caller intent, and reported an \
			 unconfirmed delete of a missing resource as `preview` — a \
			 promise that re-running with --yes would change something"
		);
		assert!(n.plan.paths.is_empty(), "no-op deletes nothing");
		assert!(n.plan.skipped.is_empty());
		assert!(!n.plan.needs_confirm);
		assert_eq!(n.prune, PruneStatus::NotRun);
	}

	#[test]
	fn agent_skill_dirs_in_scope_global_is_nonempty() {
		let dirs = agent_skill_dirs_in_scope(
			crate::models::ResourceScope::GlobalOnly,
			None,
		);
		assert!(!dirs.is_empty(), "agents define global skill dirs");
	}

	// The shared single-agent rule, pinned on all three outcomes — both
	// criteria are load-bearing and each catches what the other cannot.
	#[cfg(unix)]
	#[test]
	fn single_agent_keep_reason_covers_both_criteria() {
		// `skill_store_roots` reads HOME / XDG_CONFIG_HOME;
		// one env mutex per test binary.
		let _env = crate::skills::prune::test_lock::env_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		let tmp = tempdir().unwrap();
		let root = tmp.path();
		let scope = crate::models::ResourceScope::ProjectOnly;

		// (1) A universal Master with NO inbound link: only the roots test
		// sees it, and every project Master has NativeReaders.
		let master = root.join(".agents/skills/shared");
		write_skill_md(&master);
		// (2) A private copy that another agent's symlink points into: outside
		// the universal roots, so only the referrer sweep sees it.
		let copy = root.join(".codex/skills/linked");
		write_skill_md(&copy);
		let claude = root.join(".claude/skills");
		std::fs::create_dir_all(&claude).unwrap();
		std::os::unix::fs::symlink(&copy, claude.join("linked")).unwrap();
		// (3) A private copy nothing references.
		let lone = root.join(".codex/skills/lone");
		write_skill_md(&lone);

		let dirs = vec![claude.clone(), root.join(".codex/skills")];
		assert_eq!(
			single_agent_keep_reason(
				&master,
				&dirs,
				"shared",
				Some(root),
				scope,
				&[],
			),
			Some(KeepReason::UniversalMaster)
		);
		assert!(
			matches!(
				single_agent_keep_reason(
					&copy,
					&dirs,
					"linked",
					Some(root),
					scope,
					&[],
				),
				Some(KeepReason::ExternalReferrer(ref r))
					if r == &claude.join("linked")
			),
			"the reason must name the link that decided it: {:?}",
			single_agent_keep_reason(
				&copy,
				&dirs,
				"linked",
				Some(root),
				scope,
				&[],
			)
		);
		assert_eq!(
			single_agent_keep_reason(
				&lone,
				&dirs,
				"lone",
				Some(root),
				scope,
				&[],
			),
			None
		);

		// (a) a real dir in the `.aghub` store is kept whoever is named.
		let aghub_mstr = root.join(".aghub/mstr");
		write_skill_md(&aghub_mstr);
		assert_eq!(
			single_agent_keep_reason(
				&aghub_mstr,
				&dirs,
				"mstr",
				Some(root),
				scope,
				&[
					crate::models::AgentType::Cursor,
					crate::models::AgentType::OpenCode,
				],
			),
			Some(KeepReason::UniversalMaster)
		);

		// (b) naming the whole roster leaves no reader outside: released.
		assert_eq!(
			single_agent_keep_reason(
				&master,
				&dirs,
				"shared",
				Some(root),
				scope,
				crate::models::AgentType::ALL,
			),
			None
		);

		// (c) an enabled reader (opencode) outside the request keeps it.
		let disabled: Vec<&str> = crate::models::AgentType::ALL
			.iter()
			.filter(|&&a| {
				a != crate::models::AgentType::Cursor
					&& a != crate::models::AgentType::OpenCode
			})
			.map(|a| a.as_str())
			.collect();
		let _guard = crate::agent_settings::test_override::disable(&disabled);
		assert_eq!(
			single_agent_keep_reason(
				&master,
				&dirs,
				"shared",
				Some(root),
				scope,
				&[crate::models::AgentType::Cursor],
			),
			Some(KeepReason::UniversalMaster)
		);

		// (d) link owned by requested agent in private dir:
		let claude_link = claude.join("linked");
		let requested_three = [
			crate::models::AgentType::Claude,
			crate::models::AgentType::Cursor,
			crate::models::AgentType::OpenCode,
		];
		let disabled_except_three: Vec<&str> = crate::models::AgentType::ALL
			.iter()
			.filter(|&&a| !requested_three.contains(&a))
			.map(|a| a.as_str())
			.collect();
		let _guard_three = crate::agent_settings::test_override::disable(
			&disabled_except_three,
		);
		assert_eq!(
			single_agent_keep_reason(
				&copy,
				&dirs,
				"linked",
				Some(root),
				scope,
				&requested_three,
			),
			None
		);
		assert_eq!(
			single_agent_keep_reason(
				&copy,
				&dirs,
				"linked",
				Some(root),
				scope,
				&[
					crate::models::AgentType::Cursor,
					crate::models::AgentType::OpenCode,
				],
			),
			Some(KeepReason::ExternalReferrer(claude_link))
		);
	}

	#[cfg(unix)]
	#[test]
	fn single_agent_keep_reason_protects_private_dir_shared_by_dotfiles_layout()
	{
		let _env = crate::skills::prune::test_lock::env_lock()
			.lock()
			.unwrap_or_else(|error| error.into_inner());
		let tmp = tempdir().unwrap();
		let root = tmp.path();
		let scope = crate::models::ResourceScope::ProjectOnly;
		let skill = root.join(".claude/skills/x");
		write_skill_md(&skill);
		std::fs::create_dir_all(root.join(".cursor")).unwrap();
		std::os::unix::fs::symlink(
			root.join(".claude/skills"),
			root.join(".cursor/skills"),
		)
		.unwrap();
		std::fs::create_dir_all(root.join(".opencode")).unwrap();
		let disabled: Vec<&str> = crate::models::AgentType::ALL
			.iter()
			.filter(|&&agent| {
				!matches!(
					agent,
					crate::models::AgentType::Claude
						| crate::models::AgentType::Cursor
						| crate::models::AgentType::OpenCode
				)
			})
			.map(|agent| agent.as_str())
			.collect();
		let _disabled =
			crate::agent_settings::test_override::disable(&disabled);
		let dirs = agent_skill_dirs_in_scope(scope, Some(root));

		for requested in [
			vec![crate::models::AgentType::Cursor],
			vec![crate::models::AgentType::Claude],
		] {
			assert_eq!(
				single_agent_keep_reason(
					&skill,
					&dirs,
					"x",
					Some(root),
					scope,
					&requested,
				),
				Some(KeepReason::UniversalMaster),
				"the other enabled reader must keep the shared private directory"
			);
		}
		assert_eq!(
			single_agent_keep_reason(
				&skill,
				&dirs,
				"x",
				Some(root),
				scope,
				&[
					crate::models::AgentType::Claude,
					crate::models::AgentType::Cursor,
				],
			),
			None,
			"naming both enabled readers releases the directory"
		);
		assert_eq!(
			single_agent_keep_reason(
				&skill,
				&dirs,
				"x",
				Some(root),
				scope,
				&[],
			),
			None,
			"an empty request fails closed only inside shared roots"
		);

		let private_root = root.join("private-project");
		let private = private_root.join(".claude/skills/y");
		write_skill_md(&private);
		std::fs::create_dir_all(private_root.join(".cursor/skills")).unwrap();
		let private_dirs =
			agent_skill_dirs_in_scope(scope, Some(&private_root));
		assert_eq!(
			single_agent_keep_reason(
				&private,
				&private_dirs,
				"y",
				Some(&private_root),
				scope,
				&[crate::models::AgentType::Claude],
			),
			None,
			"a private copy with no co-reader stays deletable"
		);
	}

	#[cfg(unix)]
	#[test]
	fn single_agent_keep_reason_git_tracked_refuses_untracked_allows() {
		let _env = crate::skills::prune::test_lock::env_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		if !git_fixture::has_git() {
			eprintln!("skipping test: git binary unavailable");
			return;
		}

		let tmp = tempdir().unwrap();
		let root = tmp.path();
		let scope = crate::models::ResourceScope::ProjectOnly;
		let slot = root.join(".agents/skills/git-test-skill");
		write_skill_md(&slot);

		git_fixture::git(root, &["init", "-q"]);

		// When untracked: None (with all agents requested)
		assert_eq!(
			single_agent_keep_reason(
				&slot,
				&[],
				"git-test-skill",
				Some(root),
				scope,
				crate::models::AgentType::ALL,
			),
			None
		);

		git_fixture::git(
			root,
			&["add", "--", ".agents/skills/git-test-skill/SKILL.md"],
		);
		let other = tempdir().unwrap();
		git_fixture::git(other.path(), &["init", "-q"]);
		let old_git_dir = std::env::var_os("GIT_DIR");
		struct RestoreGitDir(Option<std::ffi::OsString>);
		impl Drop for RestoreGitDir {
			fn drop(&mut self) {
				match &self.0 {
					Some(value) => std::env::set_var("GIT_DIR", value),
					None => std::env::remove_var("GIT_DIR"),
				}
			}
		}
		let _restore_git_dir = RestoreGitDir(old_git_dir);
		std::env::set_var("GIT_DIR", other.path().join(".git"));

		// When tracked: Some(KeepReason::GitTracked)
		assert_eq!(
			single_agent_keep_reason(
				&slot,
				&[],
				"git-test-skill",
				Some(root),
				scope,
				crate::models::AgentType::ALL,
			),
			Some(KeepReason::GitTracked)
		);
	}

	// T-PLAN-JUNCTION-REFERRER: a targeted junction referrer is planned for
	// unlink (not orphaned). windows-latest.
	#[cfg(windows)]
	#[test]
	fn plan_symlink_removal_schedules_junction_referrer() {
		use crate::skills::linker::create_junction;
		let tmp = tempdir().unwrap();
		let canonical = tmp.path().join(".agents/skills/foo");
		write_skill_md(&canonical);
		let claude = tmp.path().join(".claude/skills");
		std::fs::create_dir_all(&claude).unwrap();
		let link = claude.join("foo");
		create_junction(&canonical.canonicalize().unwrap(), &link).unwrap();

		let agent_dirs = vec![claude.clone()];
		let mut skill = Skill::new("foo");
		skill.canonical_path =
			Some(canonical.join("SKILL.md").to_string_lossy().to_string());
		let plan = plan_removal(
			&skill,
			Some(claude.as_path()),
			&agent_dirs,
			Some(tmp.path()),
			false,
		);
		assert_eq!(plan.layout, Layout::Symlink);
		assert!(
			plan.paths.contains(&link),
			"junction referrer must be planned for unlink, got {:?}",
			plan.paths
		);
	}

	// T-EXTERNAL-JUNCTION-REFERRER: dir_has_external_referrer sees a junction,
	// so a shared Master with a live junction referrer is NOT removed.
	// windows-latest.
	#[cfg(windows)]
	#[test]
	fn dir_has_external_referrer_detects_junction() {
		use crate::skills::linker::create_junction;
		let tmp = tempdir().unwrap();
		let master = tmp.path().join(".agents/skills/foo");
		write_skill_md(&master);
		let claude = tmp.path().join(".claude/skills");
		std::fs::create_dir_all(&claude).unwrap();
		create_junction(&master.canonicalize().unwrap(), &claude.join("foo"))
			.unwrap();

		let agent_dirs = vec![claude.clone()];
		// Returns the referrer PATH, not a bool: the sweep runs over every
		// agent dir, so a keep decided by one of them has to be able to name
		// which one.
		assert_eq!(
			dir_has_external_referrer(&master, &agent_dirs, "foo"),
			Some(claude.join("foo")),
			"a junction referrer must count as an external referrer, and the \
			 junction itself must be what is named"
		);
	}

	#[cfg(unix)]
	#[test]
	fn plan_removal_symlink_gc_canonical_when_last_referrer_removed() {
		let tmp = tempdir().unwrap();
		let canonical = tmp.path().join(".agents/skills/foo");
		write_skill_md(&canonical);
		let claude = tmp.path().join(".claude/skills");
		std::fs::create_dir_all(&claude).unwrap();
		symlink(&canonical, &claude.join("foo"));
		// Only this single agent's dir; no universal .agents/skills included.
		let agent_dirs = vec![claude.clone()];
		let skill = symlink_skill(&canonical, &claude);

		let plan = plan_removal(
			&skill,
			Some(claude.as_path()),
			&agent_dirs,
			Some(tmp.path()),
			false,
		);

		assert!(
			plan.paths.contains(&canonical),
			"last referrer removed → canonical GC'd into paths, \
			 got paths={:?} skipped={:?}",
			plan.paths,
			plan.skipped,
		);
		assert!(
			plan.paths.contains(&claude.join("foo")),
			"referrer symlink must be in paths",
		);
		assert!(
			plan.skipped.is_empty(),
			"nothing should be skipped: {:?}",
			plan.skipped,
		);

		// execute_removal must actually remove both the symlink and the Master.
		let roots = allowed_skill_roots(&agent_dirs, Some(tmp.path()));
		let report = execute_removal(&plan, &roots).unwrap();
		assert!(report.failed.is_empty(), "no failures: {:?}", report.failed,);
		assert!(
			!canonical.exists(),
			"orphan canonical Master must be removed on disk",
		);
		assert!(
			!claude.join("foo").exists(),
			"referrer symlink must be unlinked on disk",
		);
	}

	#[cfg(unix)]
	#[test]
	fn plan_removal_symlink_keeps_canonical_when_one_of_two_referrers_remains()
	{
		let tmp = tempdir().unwrap();
		let (canonical, agent_dirs) = symlink_layout(tmp.path());
		let claude = &agent_dirs[0];
		let skill = symlink_skill(&canonical, claude);

		// Remove only claude; cursor symlink remains → canonical must be kept.
		let plan = plan_removal(
			&skill,
			Some(claude.as_path()),
			&agent_dirs,
			Some(tmp.path()),
			false,
		);

		assert!(
			!plan.paths.contains(&canonical),
			"canonical must NOT be GC'd while another referrer remains: \
			 paths={:?}",
			plan.paths,
		);
		assert!(
			plan.skipped.iter().any(|p| p == &canonical),
			"canonical must be in skipped when referrer remains: {:?}",
			plan.skipped,
		);
		assert!(
			plan.paths.contains(&claude.join("foo")),
			"targeted claude symlink must be scheduled for removal",
		);
		assert!(
			!plan.paths.contains(&agent_dirs[1].join("foo")),
			"untargeted cursor symlink must NOT be scheduled",
		);
	}

	/// A real directory inside the `.aghub` store has no reader at all, so even
	/// a request naming EVERY agent must not release it.
	#[cfg(unix)]
	#[test]
	fn single_agent_keep_reason_aghub_store_real_dir_is_kept_when_everyone_is_requested(
	) {
		let _env = crate::skills::prune::test_lock::env_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		let tmp = tempdir().unwrap();
		let root = tmp.path();
		let master = root.join(".aghub/store-dir");
		write_skill_md(&master);
		assert_eq!(
			single_agent_keep_reason(
				&master,
				&[],
				"store-dir",
				Some(root),
				crate::models::ResourceScope::ProjectOnly,
				crate::models::AgentType::ALL,
			),
			Some(KeepReason::UniversalMaster)
		);
	}

	/// Empty `requested` with a real dir in a shared slot must fail closed
	/// (`UniversalMaster`); naming every enabled reader releases it (`None`).
	#[cfg(unix)]
	#[test]
	fn single_agent_keep_reason_empty_request_fails_closed_for_shared_slot_real_dir(
	) {
		let _env = crate::skills::prune::test_lock::env_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		let tmp = tempdir().unwrap();
		let root = tmp.path();
		let scope = crate::models::ResourceScope::ProjectOnly;
		let slot = root.join(".agents/skills/closed-test");
		write_skill_md(&slot);

		// Empty requested => fail closed.
		assert_eq!(
			single_agent_keep_reason(
				&slot,
				&[],
				"closed-test",
				Some(root),
				scope,
				&[],
			),
			Some(KeepReason::UniversalMaster)
		);

		// Naming every enabled reader => released (None).
		let disabled: Vec<&str> = crate::models::AgentType::ALL
			.iter()
			.filter(|&&a| {
				a != crate::models::AgentType::Cursor
					&& a != crate::models::AgentType::OpenCode
			})
			.map(|a| a.as_str())
			.collect();
		let _guard = crate::agent_settings::test_override::disable(&disabled);
		assert_eq!(
			single_agent_keep_reason(
				&slot,
				&[],
				"closed-test",
				Some(root),
				scope,
				&[
					crate::models::AgentType::Cursor,
					crate::models::AgentType::OpenCode,
				],
			),
			None
		);
	}
}

#[cfg(test)]
mod universal_master_tests {
	use super::{is_universal_master, skill_store_roots};

	/// Containment, not path shape. Discovery recurses, so a Master can live at
	/// `.agents/skills/<team>/<name>` — a `parent == "skills"` test misses it
	/// and lets a single-agent removal delete a SHARED Master.
	#[test]
	fn nested_master_under_the_store_still_counts() {
		let tmp = tempfile::tempdir().unwrap();
		let root = tmp.path();
		let nested = root.join(".agents/skills/team/foo");
		let flat = root.join(".agents/skills/flat");
		std::fs::create_dir_all(&nested).unwrap();
		std::fs::create_dir_all(&flat).unwrap();

		assert!(is_universal_master(&flat, Some(root)), "flat master");
		assert!(is_universal_master(&nested, Some(root)), "nested master");
	}

	/// The `.aghub` Master store must be in the roots for BOTH consumers, and
	/// neither tolerates its absence: `allowed_skill_roots` would refuse every
	/// Master deletion as out-of-tree, and `is_universal_master` would let a
	/// single-agent removal `remove_dir_all` the one physical copy.
	///
	/// Drop either `.aghub` line from `skill_store_roots` and this goes red.
	#[test]
	fn the_aghub_master_store_is_a_protected_root() {
		let tmp = tempfile::tempdir().unwrap();
		let root = tmp.path();
		let master = root.join(".aghub/foo");
		std::fs::create_dir_all(&master).unwrap();

		assert!(
			is_universal_master(&master, Some(root)),
			"a skill in the .aghub store is shared by construction and must \
			 never be deleted by a single-agent removal"
		);
		let roots = super::allowed_skill_roots(&[], Some(root));
		let store = std::fs::canonicalize(root.join(".aghub")).unwrap();
		assert!(
			roots.contains(&store),
			"the store must be allow-listed or every Master deletion is \
			 refused as out-of-tree; got {roots:?}"
		);
	}

	/// The inverse error: a private per-agent copy that merely LOOKS like the
	/// store must stay deletable, or single-agent delete silently stops working.
	#[test]
	fn agent_private_dirs_are_not_masters_even_when_shaped_like_one() {
		let tmp = tempfile::tempdir().unwrap();
		let root = tmp.path();
		let decoy = root.join(".claude/skills/agents/skills/foo");
		std::fs::create_dir_all(&decoy).unwrap();
		let plain = root.join(".claude/skills/foo");
		std::fs::create_dir_all(&plain).unwrap();

		assert!(!is_universal_master(&decoy, Some(root)));
		assert!(!is_universal_master(&plain, Some(root)));
	}

	/// The store root itself is not a skill, and a missing path is not a Master.
	#[test]
	fn store_root_and_missing_paths_are_not_masters() {
		let tmp = tempfile::tempdir().unwrap();
		let root = tmp.path();
		let store = root.join(".agents/skills");
		std::fs::create_dir_all(&store).unwrap();

		assert!(!is_universal_master(&store, Some(root)));
		assert!(!is_universal_master(&store.join("ghost"), Some(root)));
	}

	/// Both spellings are stores: `.agents/skills` and XDG `agents/skills`
	/// (no leading dot — root AGENTS.md).
	#[test]
	fn both_store_spellings_are_listed() {
		let roots = skill_store_roots(Some(std::path::Path::new("/p")));
		assert!(roots.iter().any(|r| r.ends_with(".agents/skills")));
		assert!(roots
			.iter()
			.any(|r| r.ends_with("agents/skills")
				&& !r.ends_with(".agents/skills")));
	}
}
