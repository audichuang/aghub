//! Universal-mode skill install layout.
//!
//! The DEFAULT install mode copies a skill into each selected agent's own skills
//! directory and never touches `.agents` — so a skill installed "only for Claude"
//! stays out of the universal `.agents/skills` directory that agents like Codex
//! and OpenCode read. That isolation mode is just the existing copy behaviour and
//! lives in [`crate::manager`] / [`crate::transfer`].
//!
//! This module implements the OPT-IN *universal* mode, which mirrors the
//! `npx skills` layout: the real skill files live ONCE in the `.agents/skills`
//! canonical directory, and every selected agent that does not already read
//! `.agents` gets a symlink in its own skills directory pointing at the master
//! copy. Removal is already symlink-aware (see [`crate::skills::removal`]); this
//! module is the missing "create the symlink" half.
//!
//! Design choices (locked):
//! - canonical lives at `~/.agents/skills/<name>` (global) or
//!   `<project>/.agents/skills/<name>` (project);
//! - project-scope links are RELATIVE (portable across machines / git), global
//!   links are ABSOLUTE;
//! - if a platform cannot create a symlink (e.g. Windows without privilege) we
//!   FALL BACK to a real copy and record it, rather than failing the install;
//! - an existing correct symlink is left as-is (idempotent); a conflicting real
//!   file/dir or foreign symlink is NEVER clobbered — it is reported instead.

use std::io;
use std::path::{Component, Path, PathBuf};

/// Resolve the `.agents/skills` canonical store for a scope.
///
/// `project_root.is_some()` means project scope (`<root>/.agents/skills`);
/// `None` means global scope (`~/.agents/skills`).
pub fn universal_canonical_dir(project_root: Option<&Path>) -> Option<PathBuf> {
	match project_root {
		Some(root) => Some(root.join(".agents").join("skills")),
		None => {
			dirs::home_dir().map(|home| home.join(".agents").join("skills"))
		}
	}
}

/// What a universal-mode install actually did on disk.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct UniversalInstallReport {
	/// `.agents/skills/<name>` master directory.
	pub canonical: PathBuf,
	/// Agent skill dirs where a fresh symlink to the canonical dir was created.
	pub linked: Vec<PathBuf>,
	/// Agent skill dirs where a correct symlink already existed (idempotent skip).
	pub already_linked: Vec<PathBuf>,
	/// Agent skill dirs where a symlink could not be created and a real copy was
	/// written instead (e.g. Windows without symlink privilege).
	pub copied_fallback: Vec<PathBuf>,
	/// Agent skill dirs left untouched because a conflicting real file/dir or a
	/// foreign symlink already occupied the target (never clobbered).
	pub conflicts: Vec<PathBuf>,
}

enum LinkResult {
	Linked,
	AlreadyLinked,
	CopiedFallback,
	Conflict,
}

/// Install a skill in universal layout: materialize the master copy under
/// `canonical` (if absent) from `source_root`, then symlink each agent dir to it.
///
/// `canonical` is the FULL master path (`.agents/skills/<name>`), built by the
/// caller via [`universal_canonical_dir`] joined with the sanitized name.
/// `agent_skills_dirs` are the per-agent skills directories that should point at
/// the master; callers MUST exclude agents whose skills dir already *is* (or
/// reads) the canonical dir, since they see the master without a link.
pub fn install_universal(
	source_root: &Path,
	canonical: &Path,
	agent_skills_dirs: &[PathBuf],
	use_relative_links: bool,
) -> io::Result<UniversalInstallReport> {
	if !canonical.exists() {
		if let Some(parent) = canonical.parent() {
			std::fs::create_dir_all(parent)?;
		}
		copy_dir_recursive(source_root, canonical)?;
	}
	link_agents_to_canonical(canonical, agent_skills_dirs, use_relative_links)
}

/// Symlink each agent skills dir to an already-materialized `canonical` master.
///
/// Used directly when the caller has already written the master copy (e.g.
/// `add_skill` writes a generated `SKILL.md` into the canonical dir, then links).
pub fn link_agents_to_canonical(
	canonical: &Path,
	agent_skills_dirs: &[PathBuf],
	use_relative_links: bool,
) -> io::Result<UniversalInstallReport> {
	let name = canonical.file_name().ok_or_else(|| {
		io::Error::new(
			io::ErrorKind::InvalidInput,
			format!(
				"canonical path has no final component: {}",
				canonical.display()
			),
		)
	})?;

	// Resolve the canonical real path once for idempotency comparison.
	let canonical_real = std::fs::canonicalize(canonical)
		.unwrap_or_else(|_| canonical.to_path_buf());

	let mut report = UniversalInstallReport {
		canonical: canonical.to_path_buf(),
		..Default::default()
	};

	for agent_dir in agent_skills_dirs {
		let link_path = agent_dir.join(name);
		match link_one(
			agent_dir,
			&link_path,
			canonical,
			&canonical_real,
			use_relative_links,
		)? {
			LinkResult::Linked => report.linked.push(link_path),
			LinkResult::AlreadyLinked => report.already_linked.push(link_path),
			LinkResult::CopiedFallback => {
				report.copied_fallback.push(link_path)
			}
			LinkResult::Conflict => report.conflicts.push(link_path),
		}
	}

	Ok(report)
}

fn link_one(
	agent_dir: &Path,
	link_path: &Path,
	canonical: &Path,
	canonical_real: &Path,
	use_relative: bool,
) -> io::Result<LinkResult> {
	// Inspect the existing occupant WITHOUT following the link.
	match std::fs::symlink_metadata(link_path) {
		Ok(meta) => {
			if meta.file_type().is_symlink() {
				let resolves_to_canonical = std::fs::canonicalize(link_path)
					.map(|resolved| resolved == *canonical_real)
					.unwrap_or(false);
				return Ok(if resolves_to_canonical {
					LinkResult::AlreadyLinked
				} else {
					// Foreign symlink — never clobber.
					LinkResult::Conflict
				});
			}
			// Real file/dir already present — never clobber.
			return Ok(LinkResult::Conflict);
		}
		Err(e) if e.kind() == io::ErrorKind::NotFound => {}
		Err(e) => return Err(e),
	}

	std::fs::create_dir_all(agent_dir)?;

	let target = if use_relative {
		relative_path(agent_dir, canonical)
	} else {
		canonical.to_path_buf()
	};

	match create_dir_symlink(&target, link_path) {
		Ok(()) => Ok(LinkResult::Linked),
		Err(_) => {
			// Platform could not create the symlink (e.g. Windows without
			// privilege): fall back to a real copy so the install still works.
			copy_dir_recursive(canonical, link_path)?;
			Ok(LinkResult::CopiedFallback)
		}
	}
}

/// Compute a relative path so that a symlink created inside `from_dir` resolves
/// to `to_path`. Both should be absolute. Falls back to the absolute `to_path`
/// when the two share no common prefix (different roots).
fn relative_path(from_dir: &Path, to_path: &Path) -> PathBuf {
	let from: Vec<Component> = from_dir.components().collect();
	let to: Vec<Component> = to_path.components().collect();

	let mut common = 0;
	while common < from.len() && common < to.len() && from[common] == to[common]
	{
		common += 1;
	}
	if common == 0 {
		return to_path.to_path_buf();
	}

	let mut result = PathBuf::new();
	for _ in common..from.len() {
		result.push("..");
	}
	for component in &to[common..] {
		result.push(component.as_os_str());
	}
	if result.as_os_str().is_empty() {
		PathBuf::from(".")
	} else {
		result
	}
}

fn copy_dir_recursive(from: &Path, to: &Path) -> io::Result<()> {
	std::fs::create_dir_all(to)?;
	for entry in std::fs::read_dir(from)? {
		let entry = entry?;
		let from_path = entry.path();
		let to_path = to.join(entry.file_name());
		if entry.file_type()?.is_dir() {
			copy_dir_recursive(&from_path, &to_path)?;
		} else {
			std::fs::copy(&from_path, &to_path)?;
		}
	}
	Ok(())
}

#[cfg(unix)]
fn create_dir_symlink(target: &Path, link: &Path) -> io::Result<()> {
	std::os::unix::fs::symlink(target, link)
}

#[cfg(windows)]
fn create_dir_symlink(target: &Path, link: &Path) -> io::Result<()> {
	std::os::windows::fs::symlink_dir(target, link)
}

#[cfg(not(any(unix, windows)))]
fn create_dir_symlink(_target: &Path, _link: &Path) -> io::Result<()> {
	Err(io::Error::new(
		io::ErrorKind::Unsupported,
		"symlinks are not supported on this platform",
	))
}

#[cfg(all(test, unix))]
mod tests {
	use super::*;
	use std::fs;
	use tempfile::tempdir;

	fn make_source(base: &Path) -> PathBuf {
		let src = base.join("src/my-skill");
		fs::create_dir_all(&src).unwrap();
		fs::write(
			src.join("SKILL.md"),
			"---\nname: my-skill\ndescription: x\n---\nbody",
		)
		.unwrap();
		fs::create_dir_all(src.join("assets")).unwrap();
		fs::write(src.join("assets/a.txt"), "hello").unwrap();
		src
	}

	#[test]
	fn creates_canonical_and_symlink_resolving_to_master() {
		let tmp = tempdir().unwrap();
		let src = make_source(tmp.path());
		let canonical = tmp.path().join(".agents/skills/my-skill");
		let claude_skills = tmp.path().join(".claude/skills");

		let report = install_universal(
			&src,
			&canonical,
			std::slice::from_ref(&claude_skills),
			true,
		)
		.unwrap();

		// Master copy holds the real files.
		assert!(canonical.join("SKILL.md").exists());
		assert!(canonical.join("assets/a.txt").exists());

		// Agent dir got a symlink that resolves through to the master content.
		let link = claude_skills.join("my-skill");
		assert!(fs::symlink_metadata(&link)
			.unwrap()
			.file_type()
			.is_symlink());
		assert_eq!(
			fs::read_to_string(link.join("assets/a.txt")).unwrap(),
			"hello"
		);

		assert_eq!(report.linked, vec![link]);
		assert!(report.already_linked.is_empty());
		assert!(report.copied_fallback.is_empty());
		assert!(report.conflicts.is_empty());
	}

	#[test]
	fn is_idempotent_on_existing_correct_symlink() {
		let tmp = tempdir().unwrap();
		let src = make_source(tmp.path());
		let canonical = tmp.path().join(".agents/skills/my-skill");
		let claude_skills = tmp.path().join(".claude/skills");

		install_universal(
			&src,
			&canonical,
			std::slice::from_ref(&claude_skills),
			true,
		)
		.unwrap();
		let second = install_universal(
			&src,
			&canonical,
			std::slice::from_ref(&claude_skills),
			true,
		)
		.unwrap();

		assert_eq!(second.already_linked, vec![claude_skills.join("my-skill")]);
		assert!(second.linked.is_empty());
	}

	#[test]
	fn never_clobbers_an_existing_real_directory() {
		let tmp = tempdir().unwrap();
		let src = make_source(tmp.path());
		let canonical = tmp.path().join(".agents/skills/my-skill");
		let claude_skills = tmp.path().join(".claude/skills");

		// Pre-existing real skill copy at the would-be link path.
		let real = claude_skills.join("my-skill");
		fs::create_dir_all(&real).unwrap();
		fs::write(real.join("SKILL.md"), "pre-existing").unwrap();

		let report = install_universal(
			&src,
			&canonical,
			std::slice::from_ref(&claude_skills),
			true,
		)
		.unwrap();

		assert_eq!(report.conflicts, vec![real.clone()]);
		assert!(report.linked.is_empty());
		// Original content preserved, NOT replaced by a symlink.
		assert!(!fs::symlink_metadata(&real)
			.unwrap()
			.file_type()
			.is_symlink());
		assert_eq!(
			fs::read_to_string(real.join("SKILL.md")).unwrap(),
			"pre-existing"
		);
	}

	#[test]
	fn relative_links_use_dotdot_global_links_are_absolute() {
		let tmp = tempdir().unwrap();
		let src = make_source(tmp.path());
		let canonical = tmp.path().join(".agents/skills/my-skill");
		let claude_skills = tmp.path().join(".claude/skills");

		// Relative.
		install_universal(
			&src,
			&canonical,
			std::slice::from_ref(&claude_skills),
			true,
		)
		.unwrap();
		let rel = fs::read_link(claude_skills.join("my-skill")).unwrap();
		assert!(rel.is_relative(), "expected relative link, got {rel:?}");
		assert_eq!(rel, PathBuf::from("../../.agents/skills/my-skill"));

		// Absolute (separate agent dir to avoid the idempotent skip).
		let cursor_skills = tmp.path().join(".cursor/skills");
		install_universal(
			&src,
			&canonical,
			std::slice::from_ref(&cursor_skills),
			false,
		)
		.unwrap();
		let abs = fs::read_link(cursor_skills.join("my-skill")).unwrap();
		assert!(abs.is_absolute(), "expected absolute link, got {abs:?}");
		assert_eq!(abs, canonical);
	}

	#[test]
	fn relative_path_computes_minimal_dotdot() {
		assert_eq!(
			relative_path(
				Path::new("/root/.cursor/skills"),
				Path::new("/root/.agents/skills/foo")
			),
			PathBuf::from("../../.agents/skills/foo")
		);
	}

	#[test]
	fn universal_canonical_dir_resolves_by_scope() {
		let project = Path::new("/tmp/proj");
		assert_eq!(
			universal_canonical_dir(Some(project)),
			Some(PathBuf::from("/tmp/proj/.agents/skills"))
		);
		// Global resolves under the home dir (when available).
		if let Some(home) = dirs::home_dir() {
			assert_eq!(
				universal_canonical_dir(None),
				Some(home.join(".agents/skills"))
			);
		}
	}
}
