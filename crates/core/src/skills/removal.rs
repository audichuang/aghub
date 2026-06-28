//! Layout-aware removal helpers + containment guard. Allow-listed skills roots:
//! `~/.config/agents/skills`, `~/.agents/skills`, `<project>/.agents/skills`, and
//! the agent's own skills dir. Used by F2 clean removal to ensure a `remove_dir_all`
//! never escapes a known skills root (defends against a symlink pointing out of tree).

use std::path::{Path, PathBuf};

use crate::skills::linker::Linker;

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
	let mut candidates: Vec<PathBuf> = Vec::new();

	// Universal global root: $XDG_CONFIG_HOME/agents/skills (dirs resolves XDG).
	if let Some(config) = dirs::config_dir() {
		candidates.push(config.join("agents").join("skills"));
	}
	if let Some(home) = dirs::home_dir() {
		// Explicit ~/.config fallback (in case XDG_CONFIG_HOME points elsewhere).
		candidates.push(home.join(".config").join("agents").join("skills"));
		// Legacy ~/.agents/skills.
		candidates.push(home.join(".agents").join("skills"));
	}
	if let Some(root) = project_root {
		candidates.push(root.join(".agents").join("skills"));
	}
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

pub fn assert_targets_contained(
	targets: &[PathBuf],
	agent_skill_dirs: &[PathBuf],
	project_root: Option<&Path>,
) -> std::io::Result<()> {
	let roots = allowed_skill_roots(agent_skill_dirs, project_root);
	for target in targets {
		if assert_contained(target, &roots).is_some() {
			continue;
		}
		if contained_nonexistent_target(target, &roots) {
			continue;
		}
		return Err(std::io::Error::new(
			std::io::ErrorKind::PermissionDenied,
			format!("target escapes allowed skill roots: {}", target.display()),
		));
	}
	Ok(())
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

fn contained_nonexistent_target(target: &Path, roots: &[PathBuf]) -> bool {
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
		parent.starts_with(root)
	})
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
		plan_copy_removal(skill, &safe, all_agent_dirs, &roots, all_agents)
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

	for dir in all_agent_dirs {
		let entry = dir.join(safe);
		let Ok(_meta) = std::fs::symlink_metadata(&entry) else {
			continue;
		};
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
	}
}

/// Copy layout (no `canonical_path`): default removes only the targeted agent's
/// copy (from `source_path`); `--all-agents` removes every same-named copy.
fn plan_copy_removal(
	skill: &crate::models::Skill,
	safe: &str,
	all_agent_dirs: &[PathBuf],
	roots: &[PathBuf],
	all_agents: bool,
) -> RemovalPlan {
	let mut paths: Vec<PathBuf> = Vec::new();
	let mut skipped: Vec<PathBuf> = Vec::new();

	if all_agents {
		for dir in all_agent_dirs {
			let copy = dir.join(safe);
			if copy.exists() {
				push_contained(copy, roots, &mut paths, &mut skipped);
			}
		}
		RemovalPlan {
			layout: Layout::Copy,
			paths,
			skipped,
			needs_confirm: true,
		}
	} else {
		if let Some(root) = crate::transfer::skill_root_unchecked(skill) {
			if root.exists() {
				// Guard against orphaning a live symlink: when this dir is a
				// shared universal master that ANOTHER in-scope agent view still
				// symlinks into (e.g. a `.agents/skills/<name>` master read
				// directly by this agent, but symlinked by Claude), keep it and
				// report it instead of `remove_dir_all`-ing it. Plain per-agent
				// copies have no inbound symlinks, so this never blocks them.
				if dir_has_external_referrer(&root, all_agent_dirs, safe) {
					skipped.push(root);
				} else {
					push_contained(root, roots, &mut paths, &mut skipped);
				}
			}
		}
		RemovalPlan {
			layout: Layout::Copy,
			paths,
			skipped,
			needs_confirm: false,
		}
	}
}

/// True when some in-scope agent skills dir holds a symlink named `safe` that
/// resolves to `target_dir` — i.e. `target_dir` is a shared master another view
/// still references. A copy-layout removal must NOT `remove_dir_all` such a dir
/// (it would orphan the live link); it keeps + reports it instead. The symlink
/// layout already does this via its `other_refs` sweep — this is the copy-path
/// equivalent for a master that was discovered as a real dir (canonical_path
/// None) by a direct `.agents/skills` reader.
pub fn dir_has_external_referrer(
	target_dir: &Path,
	all_agent_dirs: &[PathBuf],
	safe: &str,
) -> bool {
	let Ok(target_real) = target_dir.canonicalize() else {
		return false;
	};
	for dir in all_agent_dirs {
		let entry = dir.join(safe);
		if !Linker::is_link(&entry) {
			continue;
		}
		if std::fs::canonicalize(&entry)
			.map(|resolved| resolved == target_real)
			.unwrap_or(false)
		{
			return true;
		}
	}
	false
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
			},
			executed: false,
			prune: PruneStatus::Pruned(vec!["a".to_string()]),
		};
		assert_eq!(outcome.prune, PruneStatus::Pruned(vec!["a".to_string()]));
	}

	#[test]
	fn agent_skill_dirs_in_scope_global_is_nonempty() {
		let dirs = agent_skill_dirs_in_scope(
			crate::models::ResourceScope::GlobalOnly,
			None,
		);
		assert!(!dirs.is_empty(), "agents define global skill dirs");
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
		assert!(
			dir_has_external_referrer(&master, &agent_dirs, "foo"),
			"a junction referrer must count as an external referrer"
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
