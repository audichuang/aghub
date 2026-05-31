//! Layout-aware removal helpers + containment guard. Allow-listed skills roots:
//! `~/.config/agents/skills`, `~/.agents/skills`, `<project>/.agents/skills`, and
//! the agent's own skills dir. Used by F2 clean removal to ensure a `remove_dir_all`
//! never escapes a known skills root (defends against a symlink pointing out of tree).

use std::path::{Path, PathBuf};

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

	for dir in all_agent_dirs {
		let entry = dir.join(safe);
		let Ok(meta) = std::fs::symlink_metadata(&entry) else {
			continue;
		};
		if !meta.file_type().is_symlink() {
			continue; // sweep only symlink views in this layout
		}
		let targeted =
			all_agents || own_agent_dir.is_some_and(|d| d == dir.as_path());
		match entry.canonicalize() {
			Ok(resolved) => {
				if canonical_real.as_deref() == Some(resolved.as_path()) {
					if targeted {
						paths.push(entry);
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
				if targeted {
					paths.push(entry);
				}
				unresolvable = true;
			}
		}
	}

	if let Some(canon) = canonical {
		let keep = other_refs || unresolvable;
		if keep {
			skipped.push(canon);
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
				push_contained(root, roots, &mut paths, &mut skipped);
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

	#[test]
	fn allowed_roots_include_existing_agent_dirs() {
		let agent = tempdir().unwrap();
		let agent_skills = agent.path().join("skills");
		std::fs::create_dir_all(&agent_skills).unwrap();
		let roots = allowed_skill_roots(&[agent_skills.clone()], None);
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
		let roots = allowed_skill_roots(&[missing.clone()], None);
		assert!(
			!roots.iter().any(|r| r.ends_with("does-not-exist")),
			"non-existent dirs are not returned (canonicalize fails)"
		);
	}

	// ---- plan_removal -------------------------------------------------------

	use crate::models::Skill;
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
}
