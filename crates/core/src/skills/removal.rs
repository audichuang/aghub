//! Layout-aware removal helpers + containment guard. Allow-listed skills roots:
//! `~/.config/agents/skills`, `~/.agents/skills`, `<project>/.agents/skills`, and
//! the agent's own skills dir. Used by F2 clean removal to ensure a `remove_dir_all`
//! never escapes a known skills root (defends against a symlink pointing out of tree).

use std::path::{Path, PathBuf};

use crate::skills::linker::Linker;

/// The universal-Master stores for a scope — the shared `.agents/skills` (and
/// the XDG `agents/skills`, which has NO leading dot) that every agent may read,
/// as opposed to any single agent's private skills dir.
///
/// Kept separate from [`allowed_skill_roots`] (which adds the per-agent dirs)
/// because "may this path be deleted at all" and "is this path SHARED" are
/// different questions: a private per-agent copy is deletable, a Master is not.
pub fn universal_master_roots(project_root: Option<&Path>) -> Vec<PathBuf> {
	let mut roots: Vec<PathBuf> = Vec::new();
	// Universal global root: $XDG_CONFIG_HOME/agents/skills (dirs resolves XDG).
	if let Some(config) = dirs::config_dir() {
		roots.push(config.join("agents").join("skills"));
	}
	if let Some(home) = dirs::home_dir() {
		// Explicit ~/.config fallback (in case XDG_CONFIG_HOME points elsewhere).
		roots.push(home.join(".config").join("agents").join("skills"));
		// Legacy ~/.agents/skills.
		roots.push(home.join(".agents").join("skills"));
	}
	if let Some(root) = project_root {
		roots.push(root.join(".agents").join("skills"));
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
	let mut candidates: Vec<PathBuf> = universal_master_roots(project_root);
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
/// allow-listed root. Unlike [`assert_contained`], the root itself is rejected.
pub fn assert_strictly_contained(
	target: &Path,
	roots: &[PathBuf],
) -> Option<PathBuf> {
	let canonical = target.canonicalize().ok()?;
	for root in roots {
		let root_canonical =
			root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
		if canonical != root_canonical && canonical.starts_with(&root_canonical)
		{
			return Some(canonical);
		}
	}
	None
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
	pub layout: Layout,
	/// Absolute paths that would be removed (symlinks unlinked, dirs `remove_dir_all`'d).
	pub paths: Vec<std::path::PathBuf>,
	/// Paths intentionally NOT removed (out-of-allowlist canonical, or canonical
	/// kept because another view still references it / canonicalize failed) — warn.
	pub skipped: Vec<std::path::PathBuf>,
	/// True when destructive execution requires an explicit confirm flag
	/// (symlink-layout full removal, or copy `--all-agents`).
	pub needs_confirm: bool,
	/// The targeted removal resolved to the SHARED universal Master and was
	/// therefore refused. A single-agent removal cannot express "stop only this
	/// agent seeing it" when the agent reads the Master directly, so the caller
	/// must fail loudly instead of reporting a removal that did not happen.
	pub shared_master_kept: bool,
}

/// A path's identity for comparison, with the FINAL component left unresolved.
///
/// Resolving the leaf too would be wrong here: a Referrer and the Master it
/// points at canonicalize to the same path, so "this Referrer is being deleted"
/// would read as "the Master is being deleted". The parent still gets resolved
/// so two spellings of the same directory (macOS `/var`, Windows short names)
/// compare equal.
fn entry_identity(path: &Path) -> PathBuf {
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

/// Every folder in `dir` a removal of the skill `name` has to consider: the
/// `<dir>/<sanitized-name>` slot AND whatever DISCOVERY actually reads as that
/// skill there.
///
/// Both planners used to sweep the slot alone, which is only where aghub itself
/// installs. `npx skills` and older aghub releases wrote `<dir>/<folder>` with a
/// different frontmatter `name`, and discovery recurses, so a grouped layout
/// puts the skill at `<dir>/<team>/<folder>` — neither is at the slot. The
/// planner then found nothing to take while the agent went on reading it, which
/// `--all-agents` reported as a clean `removed` (nested copy untouched, the
/// other agent still discovering the skill) and the symlink path turned into a
/// hard `unsupported_operation` refusal of a delete the caller was entitled to.
///
/// The union, not a swap: the slot is kept because an empty same-named folder
/// holds no skill for discovery to find, and leaving one behind is what makes a
/// reinstall collide.
///
/// Existence is NOT filtered here — callers disagree on what counts (the
/// symlink planner must see a DANGLING link, which `exists()` hides), so each
/// keeps its own probe.
fn candidate_entries(dir: &Path, name: &str, safe: &str) -> Vec<PathBuf> {
	let mut out = vec![dir.join(safe)];
	// Fail-OPEN to the slot alone when the dir cannot be walked: that IS the
	// answer every earlier release gave, and the slot is still probed by the
	// caller's own fail-CLOSED stat, so an unreadable dir cannot silently
	// green-light a deletion here.
	for skill in
		crate::skills::discovery::load_skills_from_dir(dir).unwrap_or_default()
	{
		if skill.name != name {
			continue;
		}
		if let Some(entry) = discovered_entry_dir(&skill) {
			if !out.contains(&entry) {
				out.push(entry);
			}
		}
	}
	out
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
}

/// Did this removal take anything away from the agent reading `read_dirs`, and
/// what still hands it the skill afterwards?
///
/// The ONE place that answers it — the question every removal surface needs and
/// none of them can read off a plan. A plan lists paths, and a path list looks
/// the same whether the agent goes on reading the skill from somewhere else or
/// not.
///
/// It asks DISCOVERY, not a path guess. Every earlier spelling compared
/// `dir.join(sanitize_name(name))` against the plan, which misses in both
/// directions: a skill whose FOLDER name differs from its frontmatter `name`
/// (`npx skills` and older aghub releases both wrote that) was invisible to it,
/// and an empty directory that merely carries the name looked like a live
/// skill. Discovery reads the frontmatter and recurses into grouped layouts, so
/// it sees what the agent will see.
///
/// `changed` is why "are there survivors?" is not the whole verdict, and the
/// distinction is drawn by FULLY canonicalizing each entry (leaf included),
/// unlike [`entry_identity`]:
///
/// - an npx-era Referrer beside the Master it points at resolves to the SAME
///   location as the Master, so unlinking it leaves the set identical —
///   nothing was taken away, and a single-agent removal must still refuse;
/// - a private per-agent copy shadowing a Master resolves to a location of its
///   own, so deleting it really does shrink the set. Refusing that (which is
///   what asking survivors alone did) left no verb at all able to drop a
///   private copy whose content had drifted from the Master.
///
/// Fail-OPEN on an unlistable read dir (plain `load_skills_from_dir`, not the
/// `_checked` variant): "cannot tell" reads as no survivors, hence no refusal.
/// That is the safe direction HERE and only here — this guard REFUSES removals,
/// so fail-closed would let one unreadable directory make a skill undeletable.
/// `transfer::skill_holders` asks the opposite question (may the Master be
/// collected?) and is fail-CLOSED for the same reason. Do not unify them.
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

	for dir in read_dirs {
		for skill in crate::skills::discovery::load_skills_from_dir(dir)
			.unwrap_or_default()
			.iter()
			.filter(|skill| skill.name == name)
		{
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
	let roots = allowed_skill_roots(all_agent_dirs, project_root);
	let safe = skill::sanitize::sanitize_name(&skill.name);

	if skill.canonical_path.is_some() {
		plan_symlink_removal(
			skill,
			&safe,
			own_agent_dir,
			all_agent_dirs,
			&roots,
			all_agents,
		)
	} else {
		plan_copy_removal(
			skill,
			&safe,
			all_agent_dirs,
			&roots,
			project_root,
			all_agents,
		)
	}
}

/// Symlink/`.agents` layout: unlink the targeted per-agent symlinks, and delete
/// the canonical dir only when (a) it is inside an allow-listed root and (b) no
/// other view still references it (a canonicalize failure counts as "might still
/// reference" — keep, never silently treat as no-match).
fn plan_symlink_removal(
	skill: &crate::models::Skill,
	safe: &str,
	own_agent_dir: Option<&Path>,
	all_agent_dirs: &[PathBuf],
	roots: &[PathBuf],
	all_agents: bool,
) -> RemovalPlan {
	let canonical = crate::transfer::skill_root_unchecked(skill);
	let canonical_real = canonical.as_ref().and_then(|c| c.canonicalize().ok());

	let mut paths: Vec<PathBuf> = Vec::new();
	let mut skipped: Vec<PathBuf> = Vec::new();
	let mut other_refs = false;
	let mut unresolvable = false;
	let mut targeted_anything = false;

	// The union `candidate_entries` returns, not the `<dir>/<safe>` slot alone:
	// `npx skills` and older aghub releases wrote `<dir>/<folder>` under a
	// different frontmatter name, and a grouped layout nests it deeper still, so
	// a sweep keyed on the slot walked straight past a live referrer.
	for (dir, entry) in all_agent_dirs.iter().flat_map(|dir| {
		candidate_entries(dir, &skill.name, safe)
			.into_iter()
			.map(move |entry| (dir, entry))
	}) {
		// NotFound means this agent simply does not hold it. Any other error
		// means the entry is THERE and we could not look — dropping it out of
		// the sweep made `delete --all-agents` neither count it as a holder nor
		// unlink its referrer, while still reporting `success: true` with the
		// path silently missing from the JSON. Fail CLOSED: an unknown holder
		// keeps the shared master, exactly as a known one would.
		match std::fs::symlink_metadata(&entry) {
			Ok(_) => {}
			Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
				continue;
			}
			Err(_) => {
				other_refs = true;
				skipped.push(entry);
				continue;
			}
		}
		let targeted =
			all_agents || own_agent_dir.is_some_and(|d| d == dir.as_path());
		match entry.canonicalize() {
			Ok(resolved) => {
				if canonical_real.as_deref() == Some(resolved.as_path()) {
					if Linker::is_link(&entry) && targeted {
						paths.push(entry);
						targeted_anything = true;
					} else if targeted {
						targeted_anything = true;
					} else {
						other_refs = true;
					}
				}
				// Resolves to a DIFFERENT target => a same-named but unrelated
				// skill; never touch it (match by canonical identity, not name).
			}
			Err(_) => {
				// Dangling/broken link: unlinking it is safe, but we cannot
				// prove it does not reference the canonical, so keep canonical.
				if Linker::is_link(&entry) && targeted {
					paths.push(entry);
					targeted_anything = true;
				}
				unresolvable = true;
			}
		}
	}

	if let Some(canon) = canonical {
		let keep = other_refs || unresolvable;
		if keep {
			skipped.push(canon);
		} else if !targeted_anything {
			// No targeted link/direct-reader was found, so there is no removal
			// effect to pair with deleting the canonical master.
		} else if assert_contained(&canon, roots).is_some() {
			paths.push(canon);
		} else {
			skipped.push(canon); // out-of-tree canonical: never remove_dir_all
		}
	}

	RemovalPlan {
		layout: Layout::Symlink,
		paths,
		skipped,
		needs_confirm: true,
		shared_master_kept: false,
	}
}

/// Copy layout (no `canonical_path`): default removes only the targeted agent's
/// copy (from `source_path`); `--all-agents` removes every same-named copy.
fn plan_copy_removal(
	skill: &crate::models::Skill,
	safe: &str,
	all_agent_dirs: &[PathBuf],
	roots: &[PathBuf],
	project_root: Option<&Path>,
	all_agents: bool,
) -> RemovalPlan {
	let mut paths: Vec<PathBuf> = Vec::new();
	let mut skipped: Vec<PathBuf> = Vec::new();

	if all_agents {
		for dir in all_agent_dirs {
			for copy in candidate_entries(dir, &skill.name, safe) {
				if copy.exists() && !paths.contains(&copy) {
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
				) {
					Some(KeepReason::UniversalMaster) => {
						shared_master_kept = true;
						skipped.push(root);
					}
					Some(KeepReason::ExternalReferrer(referrer)) => {
						// Name the referrer, not just the kept directory:
						// `skipped` lists only the caller's own path, so a keep
						// decided by one of 25 OTHER agents' dirs was
						// previously undiagnosable. The reason CARRIES the
						// path for exactly this — an extraction that dropped
						// it would silently undo the diagnosis.
						log::warn!(
							"keeping {}: {} still references it",
							root.display(),
							referrer.display()
						);
						skipped.push(root);
					}
					None => {
						push_contained(root, roots, &mut paths, &mut skipped)
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
		}
	}
}

/// True when `dir` lives inside a universal Master store — i.e. it is SHARED
/// with every other agent reading that store, rather than one agent's private
/// copy.
///
/// Containment, not path shape. Discovery RECURSES (`collect_skills` descends
/// into any directory that is not itself a skill), so a Master can sit at
/// `.agents/skills/<team>/<name>`, which a `parent == "skills"` shape test
/// misses — and misses in the dangerous direction, letting a single-agent
/// removal take a shared Master. The shape test was wrong the other way too: a
/// private copy at `.claude/skills/agents/skills/<name>` matched it and became
/// undeletable.
///
/// "Is this SHARED?" is not on its own a reason to keep a directory — see
/// [`skill_dir_readers_outside`] for the question a location delete asks.
fn is_universal_master(dir: &Path, project_root: Option<&Path>) -> bool {
	assert_strictly_contained(dir, &universal_master_roots(project_root))
		.is_some()
}

/// Which in-scope agents read the skill folder `dir` WITHOUT being named in
/// `requested`?
///
/// The question a LOCATION delete has to ask. "Is this a shared Master?"
/// ([`is_universal_master`]) is not it: a Master is shared by construction, so
/// asking that alone made every Master permanently undeletable through the
/// desktop's per-location dialog — which groups installs by exact
/// `source_path` and sends EVERY agent installed at that path, i.e. the whole
/// set of readers. That request is the user saying "drop this location", and
/// it takes nothing from anybody who did not ask. A request naming only SOME
/// of them is the dangerous one, and it is the leftovers — not the layout —
/// that make it dangerous.
///
/// A PATH question on purpose, answered from each agent's own read dirs rather
/// than from a `load_all_agents` scan: an agent whose config fails to parse
/// loads zero skills, and reading "no skills" as "not a reader" would fail
/// OPEN — straight into deleting a folder another agent still reads. Nothing
/// here parses a config, so a broken one cannot hide a reader. Containment,
/// not equality, because discovery recurses: a Master at
/// `.agents/skills/<team>/<name>` is still read by every agent whose read dir
/// is `.agents/skills`.
pub fn skill_dir_readers_outside(
	dir: &Path,
	scope: crate::models::ResourceScope,
	project_root: Option<&Path>,
	requested: &[crate::models::AgentType],
) -> Vec<&'static str> {
	let target = crate::skills::linker::classify::canonicalize_lenient(dir);
	crate::models::AgentType::ALL
		.iter()
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
	let target_real = target_dir.canonicalize().ok()?;
	let safe = skill::sanitize::sanitize_name(name);
	// Same union as the symlink sweep: a Referrer this loop cannot see is one
	// the caller then orphans with `remove_dir_all`, and the slot spelling never
	// names an npx-era or grouped layout.
	for (dir, entry) in all_agent_dirs.iter().flat_map(|dir| {
		candidate_entries(dir, name, &safe)
			.into_iter()
			.map(move |entry| (dir.as_path(), entry))
	}) {
		// `Linker::is_link` and `canonicalize(..).unwrap_or(false)` both answer
		// "no" to EACCES, so an unreadable peer directory hid a live inbound
		// symlink and this function green-lit `remove_dir_all` on the directory
		// that link points at. Verified: identical runs differing only in the
		// peer dir's mode either skip the directory (0755) or delete it and
		// leave the peer's link dangling (0400), both exit 0.
		//
		// Failing closed on EVERY stat error was too blunt, though: this loop
		// runs over all 25 agents' skills dirs, so one odd directory blocked
		// copy-layout deletion of every skill. Narrow it to the cases where a
		// referrer could actually BE there.
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
				// Cannot stat the entry — but a NAME needs no stat. Mode 0400
				// is exactly this shape: `read_dir` succeeds, every child stat
				// fails. If the listing is complete and the leaf is not in it,
				// there is provably no referrer here. Asked of the entry's OWN
				// parent and leaf, because a discovered entry can sit nested
				// below `dir` — `<dir>/<safe>` is just the depth-0 case.
				let listed = entry
					.file_name()
					.and_then(|leaf| leaf.to_str())
					.and_then(|leaf| {
						dir_lists_name(entry.parent().unwrap_or(dir), leaf)
					});
				match listed {
					Some(false) => continue,
					// Present but opaque, or not even listable: unknown, and an
					// unknown referrer keeps the directory. This function only
					// ever KEEPS, so failing closed costs a refused deletion,
					// never data.
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
					return Some(entry);
				}
			}
			// A dangling link cannot be referencing a master that still exists.
			Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
			Err(_) => return Some(entry),
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

/// Why a SINGLE-agent removal must keep a skill folder instead of deleting it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeepReason {
	/// The dir is a shared universal Master — shared BY CONSTRUCTION, so no
	/// single-agent removal may take it. Reported to the caller as
	/// `shared_master_kept`.
	UniversalMaster,
	/// Not a Master, but another in-scope agent's symlink resolves into it;
	/// deleting it would orphan that live link. Carries THAT link, because the
	/// caller can only report a keep it can name.
	ExternalReferrer(PathBuf),
}

/// The one rule for "may a single-agent removal `remove_dir_all` this folder?".
///
/// Two criteria, and BOTH are load-bearing:
/// - [`is_universal_master`] — the referrer sweep alone is not enough, because
///   a NativeReader leaves NO symlink behind, so every other agent reading the
///   same Master is invisible to it and the Master got `remove_dir_all`'d out
///   from under them while the operation reported success.
/// - [`dir_has_external_referrer`] — a plain copy OUTSIDE the universal roots
///   can still have an inbound symlink, which the first criterion cannot see.
///
/// Shared on purpose: [`plan_copy_removal`] and `ConfigManager::remove_skill`
/// both answer this question, and hand-mirroring an OR across the two is how
/// the seam ended up enforcing only half of it.
pub fn single_agent_keep_reason(
	dir: &Path,
	all_agent_dirs: &[PathBuf],
	name: &str,
	project_root: Option<&Path>,
) -> Option<KeepReason> {
	if is_universal_master(dir, project_root) {
		Some(KeepReason::UniversalMaster)
	} else {
		dir_has_external_referrer(dir, all_agent_dirs, name)
			.map(KeepReason::ExternalReferrer)
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
/// `NotRun` on a dry-run or non-executed removal; `Pruned(keys)` lists the lock
/// entries dropped (may be empty when nothing was orphaned); `Failed` means the
/// prune scan/write errored (non-fatal — the deletion already happened).
///
/// `Failed.pruned` is the truthful partial-mutation record: for a single-scope
/// prune it is always empty (the lock was left unchanged), but a `Both` prune
/// reconciles two *independent* locks (global + project) in sequence, so the
/// global lock can already be pruned when the project prune fails. The dropped
/// global keys are reported in `pruned` rather than silently lost behind the
/// error — never claim "lock unchanged" when it wasn't.
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
	/// A PREVIEW: these are the keys a committed delete would drop. Nothing has
	/// been written.
	///
	/// Distinct from `Pruned` on purpose — the whole point is that a preview
	/// cannot claim entries WERE dropped, and the CLI, API and desktop share
	/// one wire shape, so reusing `pruned_lock_entries` would make `outcome`
	/// the only thing separating "about to" from "did".
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
	/// Paths the executing removal TRIED and failed to delete (permissions, a
	/// concurrent change, …). Empty on a clean run, and on the resources whose
	/// removal touches no path at all (MCP / sub-agent).
	///
	/// `executed` is set to `true` for the whole execute branch regardless, and
	/// the failed paths are folded into `plan.skipped`, so a run where EVERY
	/// delete failed still reported `executed: true` and — once a three-way
	/// outcome existed — `outcome: "removed"` for files that are all still
	/// there. Keeping the failures here lets the wire view say `partial`
	/// instead of claiming a removal that did not happen.
	pub failed_paths: Vec<std::path::PathBuf>,
	/// The resource was ALREADY GONE — nothing to remove, whatever the caller
	/// asked for.
	///
	/// Without this, `executed: false` conflated two different answers, and the
	/// wire view could only guess between them from the caller's intent: an
	/// unconfirmed delete of an absent resource reported `outcome: "preview"`,
	/// whose contract says re-running with `--yes` WILL change something. It
	/// will not — there is nothing there. Set by [`RemovalOutcome::noop`], the
	/// one constructor for that case.
	pub absent: bool,
}

impl RemovalOutcome {
	/// The PREVIEW of `plan` — what a commit would do, including the lock keys
	/// it would drop.
	///
	/// One producer, shared by every surface. Two hand-written copies of this
	/// (the manager's and the API by-path route's) is exactly the
	/// "NEVER hand-mirror a transactional flow across surfaces" the root
	/// `AGENTS.md` forbids, and it had already drifted: the route hard-coded
	/// `PruneStatus::NotRun`, so its preview silently under-reported the lock
	/// cleanup its own commit would perform.
	/// `blocks` is the caller's OWN "this removal takes nothing away" verdict
	/// (`read_effect_after`), passed in rather than re-derived here: the
	/// `shared_master_kept && paths.is_empty()` proxy below cannot see the
	/// npx-era Referrer sitting BESIDE the Master it points at, where there is
	/// a real path to unlink and the commit still refuses. A caller with no
	/// such verdict passes `false` and keeps the proxy.
	pub fn preview(
		plan: RemovalPlan,
		blocks: bool,
		scope: crate::models::ResourceScope,
		project_root: Option<&Path>,
	) -> Self {
		// Load-bearing: for a kept shared Master the COMMIT does not prune, it
		// REFUSES. Promising `would_prune_lock_entries` there would describe a
		// commit that can never happen.
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
		Self {
			plan,
			executed: false,
			prune,
			failed_paths: Vec::new(),
			// Callers reach a preview only AFTER their not-found check.
			absent: false,
		}
	}

	/// COMMIT `plan`: run the removal, fold what ACTUALLY happened back into the
	/// plan, and reconcile the per-scope lock.
	///
	/// The counterpart of [`Self::preview`], and the same rule applies — this is
	/// the only place that turns a [`RemovalReport`] into an outcome. The
	/// duplicate that used to live in the API by-path route hard-coded
	/// `failed_paths` empty, which made `RemovalKind::Partial` unreachable there:
	/// a delete where every `remove_dir_all` returned `EACCES` reported
	/// `outcome: "removed"` with the skill still on disk, and the desktop closes
	/// its dialog on `removed`.
	pub fn commit(
		mut plan: RemovalPlan,
		roots: &[PathBuf],
		scope: crate::models::ResourceScope,
		project_root: Option<&Path>,
	) -> std::io::Result<Self> {
		let report = execute_removal(&plan, roots)?;
		for path in &report.skipped {
			log::warn!(
				"skipped removal of '{}' (outside skills roots)",
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
	/// missing resource). One shared constructor so the CLI and API serialize
	/// the SAME `success:true, executed:false, dry_run:true` wire shape for the
	/// "already gone" path across skill/MCP/sub-agent deletes — they must not
	/// drift (API was lenient, CLI errored). `deleted_path` stays null because
	/// `executed` is false.
	pub fn noop() -> Self {
		RemovalOutcome {
			plan: RemovalPlan {
				layout: Layout::Copy,
				paths: vec![],
				skipped: vec![],
				needs_confirm: false,
				shared_master_kept: false,
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
			match Linker::unlink(path) {
				Ok(()) => report.removed.push(path.clone()),
				Err(e) => report.failed.push((path.clone(), e)),
			}
		} else if ft.is_dir() {
			// Re-assert containment immediately before remove_dir_all.
			if assert_contained(path, roots).is_some() {
				match std::fs::remove_dir_all(path) {
					Ok(()) => report.removed.push(path.clone()),
					Err(e) => report.failed.push((path.clone(), e)),
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
mod tests {
	use super::*;
	use tempfile::tempdir;

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
		// Dangling symlink: target does not exist -> canonicalize() fails.
		symlink(&tmp.path().join("gone"), &cursor.join("foo"));
		let agent_dirs = vec![claude.clone(), cursor.clone()];
		let skill = symlink_skill(&canonical, &claude);
		let plan =
			plan_removal(&skill, None, &agent_dirs, Some(tmp.path()), true);
		assert!(
			!plan.paths.contains(&canonical),
			"canonicalize failure => conservatively keep canonical"
		);
		assert!(plan.skipped.iter().any(|p| p == &canonical));
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
		// P0: a universal `.agents/skills/<name>` master read DIRECTLY (as a real
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
		};
		execute_removal(&plan, std::slice::from_ref(&skills)).unwrap();
		assert!(!foo.exists());
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
		// `is_universal_master` reads HOME / XDG_CONFIG_HOME through
		// `universal_master_roots`; one env mutex per test binary.
		let _env = crate::skills::prune::test_lock::env_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		let tmp = tempdir().unwrap();
		let root = tmp.path();

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
			single_agent_keep_reason(&master, &dirs, "shared", Some(root)),
			Some(KeepReason::UniversalMaster)
		);
		assert!(
			matches!(
				single_agent_keep_reason(&copy, &dirs, "linked", Some(root)),
				Some(KeepReason::ExternalReferrer(ref r))
					if r == &claude.join("linked")
			),
			"the reason must name the link that decided it: {:?}",
			single_agent_keep_reason(&copy, &dirs, "linked", Some(root))
		);
		assert_eq!(
			single_agent_keep_reason(&lone, &dirs, "lone", Some(root)),
			None
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
}

#[cfg(test)]
mod universal_master_tests {
	use super::{is_universal_master, universal_master_roots};

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
		let roots = universal_master_roots(Some(std::path::Path::new("/p")));
		assert!(roots.iter().any(|r| r.ends_with(".agents/skills")));
		assert!(roots
			.iter()
			.any(|r| r.ends_with("agents/skills")
				&& !r.ends_with(".agents/skills")));
	}
}
