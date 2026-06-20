//! Cross-platform directory-link primitives ported from jiweiyeah/Skills-Manager
//! (MIT) — linker.rs: is_symlink_or_junction / remove_symlink_or_junction /
//! create_windows_symlink / normalize_path. SM's iflow copy-mode is intentionally
//! NOT ported: aghub bans copy as a skill-install outcome.

pub mod classify;
pub use classify::{classify_agent, classify_all, AgentLinkPlan, LinkNeed};

use std::io;
use std::path::{Component, Path, PathBuf, MAIN_SEPARATOR};

/// Resolve the `.agents/skills` canonical SKILLS-DIR for a scope.
///
/// `project_root.is_some()` => `<root>/.agents/skills`; `None` =>
/// `~/.agents/skills`. The returned path is absolute iff the input root is
/// absolute (callers MUST pass an absolute project_root — Decision 6).
pub fn universal_canonical_dir(project_root: Option<&Path>) -> Option<PathBuf> {
	match project_root {
		Some(root) => Some(root.join(".agents").join("skills")),
		None => {
			dirs::home_dir().map(|home| home.join(".agents").join("skills"))
		}
	}
}

/// Whether a created link's stored target is relative (project scope, portable)
/// or absolute (global scope). Windows junctions ALWAYS resolve to absolute
/// even when `Relative` is requested (junctions cannot store a relative target).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkTarget {
	Relative,
	Absolute,
}

/// Outcome of a single link attempt against one agent skills-dir.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkOutcome {
	/// Fresh link created (unix symlink / win symlink / win junction).
	Linked,
	/// A correct link to the same Master already existed (idempotent).
	AlreadyLinked,
	/// A foreign symlink/junction OR a real file/dir occupies the slot -
	/// NEVER clobbered.
	Conflict,
}

/// A link failure. Per-agent failures are folded into
/// [`UniversalInstallReport::failed`] (Decision 10); they are NOT propagated as
/// `Err` from the convenience layer except for pre-link invariant violations.
#[derive(Debug, thiserror::Error)]
pub enum LinkError {
	/// BOTH native symlink AND `cmd /C mklink /J` failed on Windows (or symlink
	/// is unsupported on a non-unix/non-windows platform). HARD per-agent
	/// error - NO copy fallback.
	#[error("could not link {link} -> {target}: {source}")]
	LinkUnsupported {
		target: PathBuf,
		link: PathBuf,
		source: io::Error,
	},
	/// Decision 6 violated: `abs_target` was not absolute, so a junction could
	/// not be created safely.
	#[error("junction target must be absolute: {target}")]
	NonAbsoluteTarget { target: PathBuf },
	#[error(transparent)]
	Io(#[from] io::Error),
}

/// Normalize path separators to the platform native separator. On Windows
/// `/`->`\` (feeds `cmd.exe` native separators); on Unix a no-op. Ported from
/// SM `normalize_path`.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn normalize_path(path: &Path) -> PathBuf {
	if MAIN_SEPARATOR == '\\' {
		PathBuf::from(path.to_string_lossy().replace('/', "\\"))
	} else {
		path.to_path_buf()
	}
}

/// Compute a relative path so a symlink created inside `from_dir` resolves to
/// `to_path`. Both should be absolute. Falls back to the absolute `to_path`
/// when the two share no common prefix (different roots).
#[cfg_attr(not(test), allow(dead_code))]
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

/// Names excluded when materializing a Master, mirroring upstream npx
/// `copyDirectory` (installer.ts) so the Master hashes identically to npx.
#[cfg_attr(not(test), allow(dead_code))]
const EXCLUDE_FILES: &[&str] = &["metadata.json"];
#[cfg_attr(not(test), allow(dead_code))]
const EXCLUDE_DIRS: &[&str] = &[".git", "__pycache__", "__pypackages__"];

/// Recursively copy a skill source tree into the canonical Master directory,
/// applying the npx exclude lists and dereferencing symlinks.
///
/// NOTE: this copy materializes the single Master only; it is NOT a per-agent
/// copy fallback. The converged install model bans copy as a per-agent outcome.
fn copy_dir_recursive(from: &Path, to: &Path) -> io::Result<()> {
	std::fs::create_dir_all(to)?;
	for entry in std::fs::read_dir(from)? {
		let entry = entry?;
		let file_name = entry.file_name();
		let name = file_name.to_string_lossy();
		let file_type = entry.file_type()?;
		if EXCLUDE_FILES.contains(&name.as_ref())
			|| (file_type.is_dir() && EXCLUDE_DIRS.contains(&name.as_ref()))
		{
			continue;
		}
		let from_path = entry.path();
		let to_path = to.join(&file_name);
		if file_type.is_dir() {
			copy_dir_recursive(&from_path, &to_path)?;
		} else {
			match std::fs::metadata(&from_path) {
				Ok(meta) if meta.is_dir() => {
					copy_dir_recursive(&from_path, &to_path)?
				}
				Ok(_) => {
					std::fs::copy(&from_path, &to_path)?;
				}
				Err(e)
					if e.kind() == io::ErrorKind::NotFound
						&& file_type.is_symlink() => {}
				Err(e) => return Err(e),
			}
		}
	}
	Ok(())
}

/// What a symlink-only install did on disk. There is NO `copied_fallback`
/// field — the converged model bans copy. Per-agent hard failures land in
/// `failed` (Decision 10), never as an `Err` from the convenience layer.
#[derive(Debug, Default)]
pub struct UniversalInstallReport {
	/// `.agents/skills/<name>` master SKILL-DIR.
	pub canonical: PathBuf,
	/// Agent skills-dirs where a fresh link to the master was created.
	pub linked: Vec<PathBuf>,
	/// Agent skills-dirs where a correct link already existed (idempotent).
	pub already_linked: Vec<PathBuf>,
	/// Agent skills-dirs left untouched: a real file/dir or foreign link
	/// occupied the slot (never clobbered).
	pub conflicts: Vec<PathBuf>,
	/// Per-agent hard link failures (Decision 10): NOT propagated as `Err`.
	pub failed: Vec<(PathBuf, LinkError)>,
}

/// Materialize the Master from `source_root` (npx-identical copy +
/// exclusions) if absent, then link each agent skills-dir. A per-agent
/// link hard-error is collected into `report.failed`, NOT returned as
/// `Err`. `Err(LinkError)` is reserved for pre-link invariant violations
/// (`NonAbsoluteTarget`) or the Master copy itself failing.
pub fn install_universal(
	source_root: &Path,
	canonical: &Path,
	agent_skills_dirs: &[PathBuf],
	target: LinkTarget,
) -> Result<UniversalInstallReport, LinkError> {
	if !canonical.is_absolute() {
		return Err(LinkError::NonAbsoluteTarget {
			target: canonical.to_path_buf(),
		});
	}
	if !canonical.exists() {
		if let Some(parent) = canonical.parent() {
			std::fs::create_dir_all(parent)?;
		}
		copy_dir_recursive(source_root, canonical)?;
	}
	link_agents_to_canonical(canonical, agent_skills_dirs, target)
}

/// Link each agent skills-dir to an already-materialized Master. Same
/// per-agent-soft-fail contract as [`install_universal`].
pub fn link_agents_to_canonical(
	canonical: &Path,
	agent_skills_dirs: &[PathBuf],
	target: LinkTarget,
) -> Result<UniversalInstallReport, LinkError> {
	if !canonical.is_absolute() {
		return Err(LinkError::NonAbsoluteTarget {
			target: canonical.to_path_buf(),
		});
	}
	let name = canonical
		.file_name()
		.ok_or_else(|| {
			LinkError::Io(io::Error::new(
				io::ErrorKind::InvalidInput,
				format!(
					"canonical path has no final component: {}",
					canonical.display()
				),
			))
		})?
		.to_string_lossy()
		.into_owned();

	let mut report = UniversalInstallReport {
		canonical: canonical.to_path_buf(),
		..Default::default()
	};

	for agent_dir in agent_skills_dirs {
		let link_path = agent_dir.join(&name);
		match Linker::link(canonical, agent_dir, &name, target) {
			Ok(LinkOutcome::Linked) => report.linked.push(link_path),
			Ok(LinkOutcome::AlreadyLinked) => {
				report.already_linked.push(link_path)
			}
			Ok(LinkOutcome::Conflict) => report.conflicts.push(link_path),
			Err(e) => report.failed.push((link_path, e)),
		}
	}

	Ok(report)
}

/// Zero-sized, stateless namespace for the directory-link primitives.
pub struct Linker;

impl Linker {
	/// lstat-based reparse-point detection: true for a Unix symlink OR a
	/// Windows symlink/junction (FILE_ATTRIBUTE_REPARSE_POINT 0x0400). Never
	/// follows the link. Ported from SM `is_symlink_or_junction`.
	pub fn is_link(path: &Path) -> bool {
		if let Ok(meta) = path.symlink_metadata() {
			if meta.file_type().is_symlink() {
				return true;
			}
			#[cfg(windows)]
			{
				use std::os::windows::fs::MetadataExt;
				const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
				if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
					return true;
				}
			}
		}
		false
	}

	/// Remove a link without touching its target: on Windows `remove_dir` then
	/// `remove_file` (a junction is a dir reparse point; a Unix symlink-to-dir
	/// needs `remove_file`). Idempotent on a missing path. Ported from SM
	/// `remove_symlink_or_junction`. Uses `remove_dir`, NEVER `remove_dir_all`,
	/// so it only unlinks the reparse point and never recurses into the Master.
	pub fn unlink(path: &Path) -> io::Result<()> {
		let result = {
			#[cfg(windows)]
			{
				std::fs::remove_dir(path)
					.or_else(|_| std::fs::remove_file(path))
			}
			#[cfg(unix)]
			{
				std::fs::remove_file(path)
			}
			#[cfg(not(any(unix, windows)))]
			{
				std::fs::remove_file(path)
			}
		};
		match result {
			Ok(()) => Ok(()),
			Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
			Err(e) => Err(e),
		}
	}

	/// Create `agent_skills_dir/<skill_name>` -> `master_dir` (the
	/// `.agents/skills/<name>` canonical SKILL-DIR, which MUST already exist and
	/// MUST be absolute). Creates `agent_skills_dir` if absent. lstat-inspects
	/// the occupant WITHOUT following it (via [`Linker::is_link`], so a junction
	/// is recognized): returns `AlreadyLinked` / `Conflict` without writing on
	/// collision. On a clean target: Unix => symlink; Windows => symlink_dir,
	/// else `cmd /C mklink /J <ABSOLUTE master>`; both fail =>
	/// `LinkError::LinkUnsupported`. `master_dir` not absolute =>
	/// `NonAbsoluteTarget`.
	pub fn link(
		master_dir: &Path,
		agent_skills_dir: &Path,
		skill_name: &str,
		target: LinkTarget,
	) -> Result<LinkOutcome, LinkError> {
		if !master_dir.is_absolute() {
			return Err(LinkError::NonAbsoluteTarget {
				target: master_dir.to_path_buf(),
			});
		}
		let link_path = agent_skills_dir.join(skill_name);
		let master_real = std::fs::canonicalize(master_dir)
			.unwrap_or_else(|_| master_dir.to_path_buf());

		// Inspect the existing occupant WITHOUT following it.
		match std::fs::symlink_metadata(&link_path) {
			Ok(_) => {
				if Self::is_link(&link_path) {
					let resolves = std::fs::canonicalize(&link_path)
						.map(|r| r == master_real)
						.unwrap_or(false);
					return Ok(if resolves {
						LinkOutcome::AlreadyLinked
					} else {
						LinkOutcome::Conflict
					});
				}
				return Ok(LinkOutcome::Conflict);
			}
			Err(e) if e.kind() == io::ErrorKind::NotFound => {}
			Err(e) => return Err(LinkError::Io(e)),
		}

		std::fs::create_dir_all(agent_skills_dir)?;

		let requested = match target {
			LinkTarget::Relative => relative_path(agent_skills_dir, master_dir),
			LinkTarget::Absolute => master_dir.to_path_buf(),
		};
		create_link(&requested, master_dir, &link_path)?;
		Ok(LinkOutcome::Linked)
	}
}

/// Create a directory link at `link` pointing at `requested_target`
/// (possibly relative on Unix), falling back on Windows to a junction using
/// the absolute `abs_target`. Create-only: the caller has already verified the
/// slot is empty and `abs_target` is absolute.
#[cfg(unix)]
fn create_link(
	requested_target: &Path,
	_abs_target: &Path,
	link: &Path,
) -> Result<(), LinkError> {
	std::os::unix::fs::symlink(requested_target, link).map_err(|source| {
		LinkError::LinkUnsupported {
			target: requested_target.to_path_buf(),
			link: link.to_path_buf(),
			source,
		}
	})
}

#[cfg(windows)]
fn create_link(
	requested_target: &Path,
	abs_target: &Path,
	link: &Path,
) -> Result<(), LinkError> {
	// Native symlink first (needs Dev Mode/admin); honors relative target.
	if std::os::windows::fs::symlink_dir(requested_target, link).is_ok() {
		return Ok(());
	}
	// Fallback: directory junction (no admin). MUST use the absolute target.
	create_junction(abs_target, link)
}

/// Create a directory junction at `link` pointing at the ABSOLUTE `abs_target`
/// via `cmd /C mklink /J`. Extracted as a named fn so tests can force the
/// junction path regardless of Developer Mode. Ported/adapted from SM
/// `create_windows_symlink` (junction arm); SM's pre-clean and GBK decoding are
/// dropped (the caller guarantees an empty, non-clobbered slot). Create-only.
#[cfg(windows)]
pub(crate) fn create_junction(
	abs_target: &Path,
	link: &Path,
) -> Result<(), LinkError> {
	use std::os::windows::process::CommandExt;
	use std::process::Command;

	let link_norm = normalize_path(link);
	let target_norm = normalize_path(abs_target);
	let output = Command::new("cmd")
		.args(["/C", "mklink", "/J"])
		.arg(&link_norm)
		.arg(&target_norm)
		.creation_flags(0x08000000) // CREATE_NO_WINDOW
		.output();

	match output {
		Ok(out) if out.status.success() => Ok(()),
		Ok(out) => {
			let stderr = String::from_utf8_lossy(&out.stderr);
			let stdout = String::from_utf8_lossy(&out.stdout);
			Err(LinkError::LinkUnsupported {
				target: abs_target.to_path_buf(),
				link: link.to_path_buf(),
				source: io::Error::other(format!(
					"mklink /J {} {} failed: {} {}",
					link_norm.display(),
					target_norm.display(),
					stderr.trim(),
					stdout.trim()
				)),
			})
		}
		Err(source) => Err(LinkError::LinkUnsupported {
			target: abs_target.to_path_buf(),
			link: link.to_path_buf(),
			source,
		}),
	}
}

#[cfg(not(any(unix, windows)))]
fn create_link(
	requested_target: &Path,
	_abs_target: &Path,
	link: &Path,
) -> Result<(), LinkError> {
	Err(LinkError::LinkUnsupported {
		target: requested_target.to_path_buf(),
		link: link.to_path_buf(),
		source: io::Error::new(
			io::ErrorKind::Unsupported,
			"symlinks are not supported on this platform",
		),
	})
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn universal_canonical_dir_resolves_by_scope() {
		let project = Path::new("/tmp/proj");
		assert_eq!(
			universal_canonical_dir(Some(project)),
			Some(PathBuf::from("/tmp/proj/.agents/skills"))
		);
		if let Some(home) = dirs::home_dir() {
			assert_eq!(
				universal_canonical_dir(None),
				Some(home.join(".agents/skills"))
			);
		}
	}

	#[test]
	fn is_link_false_for_real_dir_and_missing() {
		use tempfile::tempdir;
		let tmp = tempdir().unwrap();
		let real = tmp.path().join("real");
		std::fs::create_dir_all(&real).unwrap();
		assert!(!Linker::is_link(&real), "a real dir is not a link");
		assert!(
			!Linker::is_link(&tmp.path().join("missing")),
			"a missing path is not a link"
		);
	}

	#[cfg(unix)]
	#[test]
	fn is_link_true_for_unix_symlink() {
		use tempfile::tempdir;
		let tmp = tempdir().unwrap();
		let target = tmp.path().join("target");
		std::fs::create_dir_all(&target).unwrap();
		let link = tmp.path().join("link");
		std::os::unix::fs::symlink(&target, &link).unwrap();
		assert!(Linker::is_link(&link), "a unix symlink IS a link");
	}

	#[test]
	fn unlink_is_idempotent_on_missing_path() {
		use tempfile::tempdir;
		let tmp = tempdir().unwrap();
		Linker::unlink(&tmp.path().join("nope"))
			.expect("unlinking a missing path is a no-op");
	}

	#[cfg(unix)]
	#[test]
	fn unlink_removes_symlink_but_keeps_target() {
		use tempfile::tempdir;
		let tmp = tempdir().unwrap();
		let target = tmp.path().join("target");
		std::fs::create_dir_all(&target).unwrap();
		std::fs::write(target.join("keep.txt"), "keep").unwrap();
		let link = tmp.path().join("link");
		std::os::unix::fs::symlink(&target, &link).unwrap();

		Linker::unlink(&link).unwrap();

		assert!(!Linker::is_link(&link), "link must be gone");
		assert!(
			std::fs::symlink_metadata(&link).is_err(),
			"link path must not exist"
		);
		assert!(
			target.join("keep.txt").exists(),
			"unlink must never touch the target"
		);
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
	fn link_error_from_io_maps_to_io_variant() {
		let e: LinkError = io::Error::other("boom").into();
		assert!(matches!(e, LinkError::Io(_)));
	}

	#[test]
	fn non_absolute_target_constructs() {
		let e = LinkError::NonAbsoluteTarget {
			target: PathBuf::from("rel/path"),
		};
		assert!(matches!(e, LinkError::NonAbsoluteTarget { .. }));
	}

	#[test]
	fn link_creates_a_real_link_resolving_to_master() {
		use tempfile::tempdir;
		let tmp = tempdir().unwrap();
		let master = tmp.path().join(".agents/skills/my-skill");
		std::fs::create_dir_all(&master).unwrap();
		std::fs::write(master.join("SKILL.md"), "real").unwrap();
		let claude = tmp.path().join(".claude/skills");

		let outcome =
			Linker::link(&master, &claude, "my-skill", LinkTarget::Absolute)
				.unwrap();

		assert_eq!(outcome, LinkOutcome::Linked);
		let link = claude.join("my-skill");
		assert!(Linker::is_link(&link), "must be a real link, not a copy");
		// Write a sentinel into the Master AFTER linking, read it THROUGH the
		// link — proves a true link, not a coincidentally-identical copy.
		std::fs::write(master.join("sentinel.txt"), "via-link").unwrap();
		assert_eq!(
			std::fs::read_to_string(link.join("sentinel.txt")).unwrap(),
			"via-link"
		);
	}

	#[test]
	fn link_rejects_non_absolute_master() {
		let err = Linker::link(
			Path::new("rel/master"),
			Path::new("/tmp/agent/skills"),
			"x",
			LinkTarget::Absolute,
		)
		.unwrap_err();
		assert!(matches!(err, LinkError::NonAbsoluteTarget { .. }));
	}

	#[test]
	fn copy_dir_recursive_excludes_vcs_cache_and_metadata() {
		use tempfile::tempdir;
		let tmp = tempdir().unwrap();
		let src = tmp.path().join("src");
		std::fs::create_dir_all(src.join(".git")).unwrap();
		std::fs::write(src.join(".git/config"), "x").unwrap();
		std::fs::create_dir_all(src.join("__pycache__")).unwrap();
		std::fs::write(src.join("__pycache__/m.pyc"), "x").unwrap();
		std::fs::create_dir_all(src.join("__pypackages__")).unwrap();
		std::fs::write(src.join("__pypackages__/p"), "x").unwrap();
		std::fs::write(src.join("metadata.json"), "{}").unwrap();
		std::fs::write(src.join("SKILL.md"), "real").unwrap();
		std::fs::create_dir_all(src.join("assets")).unwrap();
		std::fs::write(src.join("assets/a.txt"), "keep").unwrap();

		let dest = tmp.path().join("dest");
		copy_dir_recursive(&src, &dest).unwrap();

		assert!(dest.join("SKILL.md").exists());
		assert!(dest.join("assets/a.txt").exists());
		assert!(!dest.join(".git").exists(), ".git must be excluded");
		assert!(!dest.join("__pycache__").exists(), "__pycache__ excluded");
		assert!(
			!dest.join("__pypackages__").exists(),
			"__pypackages__ excluded"
		);
		assert!(
			!dest.join("metadata.json").exists(),
			"metadata.json must be excluded"
		);
	}

	#[test]
	fn link_is_idempotent_on_existing_correct_link() {
		use tempfile::tempdir;
		let tmp = tempdir().unwrap();
		let master = tmp.path().join(".agents/skills/my-skill");
		std::fs::create_dir_all(&master).unwrap();
		let claude = tmp.path().join(".claude/skills");

		let first =
			Linker::link(&master, &claude, "my-skill", LinkTarget::Absolute)
				.unwrap();
		let second =
			Linker::link(&master, &claude, "my-skill", LinkTarget::Absolute)
				.unwrap();

		assert_eq!(first, LinkOutcome::Linked);
		assert_eq!(second, LinkOutcome::AlreadyLinked);
	}

	#[test]
	fn link_never_clobbers_a_real_directory() {
		use tempfile::tempdir;
		let tmp = tempdir().unwrap();
		let master = tmp.path().join(".agents/skills/my-skill");
		std::fs::create_dir_all(&master).unwrap();
		let claude = tmp.path().join(".claude/skills");
		let occupied = claude.join("my-skill");
		std::fs::create_dir_all(&occupied).unwrap();
		std::fs::write(occupied.join("SKILL.md"), "pre-existing").unwrap();

		let outcome =
			Linker::link(&master, &claude, "my-skill", LinkTarget::Absolute)
				.unwrap();

		assert_eq!(outcome, LinkOutcome::Conflict);
		assert!(!Linker::is_link(&occupied), "must stay a real dir");
		assert_eq!(
			std::fs::read_to_string(occupied.join("SKILL.md")).unwrap(),
			"pre-existing"
		);
	}

	#[cfg(unix)]
	#[test]
	fn link_never_clobbers_a_foreign_link() {
		use tempfile::tempdir;
		let tmp = tempdir().unwrap();
		let master = tmp.path().join(".agents/skills/my-skill");
		std::fs::create_dir_all(&master).unwrap();
		let other = tmp.path().join("somewhere-else");
		std::fs::create_dir_all(&other).unwrap();
		std::fs::write(other.join("foreign.txt"), "foreign").unwrap();
		let claude = tmp.path().join(".claude/skills");
		std::fs::create_dir_all(&claude).unwrap();
		let slot = claude.join("my-skill");
		std::os::unix::fs::symlink(&other, &slot).unwrap();

		let outcome =
			Linker::link(&master, &claude, "my-skill", LinkTarget::Absolute)
				.unwrap();

		assert_eq!(outcome, LinkOutcome::Conflict);
		assert_eq!(
			std::fs::read_to_string(slot.join("foreign.txt")).unwrap(),
			"foreign",
			"foreign link must still resolve to its original target"
		);
	}

	#[cfg(unix)]
	#[test]
	fn link_hard_errors_when_symlink_create_is_denied() {
		use std::os::unix::fs::PermissionsExt;
		use tempfile::tempdir;
		// EACCES does not apply to root — skip there (matches removal.rs).
		if unsafe { libc::geteuid() } == 0 {
			return;
		}
		let tmp = tempdir().unwrap();
		let master = tmp.path().join(".agents/skills/my-skill");
		std::fs::create_dir_all(&master).unwrap();
		std::fs::write(master.join("SKILL.md"), "real").unwrap();
		// Pre-create the agent dir 0o500 so creating the link inside EACCESes.
		let claude = tmp.path().join(".claude/skills");
		std::fs::create_dir_all(&claude).unwrap();
		let original = std::fs::metadata(&claude).unwrap().permissions();
		std::fs::set_permissions(
			&claude,
			std::fs::Permissions::from_mode(0o500),
		)
		.unwrap();

		let result =
			Linker::link(&master, &claude, "my-skill", LinkTarget::Absolute);

		std::fs::set_permissions(&claude, original).unwrap();

		let err = result.unwrap_err();
		assert!(
			matches!(err, LinkError::LinkUnsupported { .. }),
			"denied symlink must be a hard LinkUnsupported, got {err:?}"
		);
		// No link created; Master (written first) intact — no copy fallback.
		assert!(
			std::fs::symlink_metadata(claude.join("my-skill")).is_err(),
			"no link must exist after a hard error (no copy fallback)"
		);
		assert_eq!(
			std::fs::read_to_string(master.join("SKILL.md")).unwrap(),
			"real",
			"Master must be intact"
		);
	}

	fn make_source(base: &Path) -> PathBuf {
		let src = base.join("src/my-skill");
		std::fs::create_dir_all(&src).unwrap();
		std::fs::write(
			src.join("SKILL.md"),
			"---\nname: my-skill\ndescription: x\n---\nbody",
		)
		.unwrap();
		std::fs::create_dir_all(src.join("assets")).unwrap();
		std::fs::write(src.join("assets/a.txt"), "hello").unwrap();
		src
	}

	#[test]
	fn install_universal_materializes_master_and_links_each_agent() {
		use tempfile::tempdir;
		let tmp = tempdir().unwrap();
		let root = std::fs::canonicalize(tmp.path()).unwrap();
		let src = make_source(&root);
		let canonical = root.join(".agents/skills/my-skill");
		let claude = root.join(".claude/skills");

		let report = install_universal(
			&src,
			&canonical,
			std::slice::from_ref(&claude),
			LinkTarget::Absolute,
		)
		.unwrap();

		assert!(canonical.join("SKILL.md").exists());
		assert!(canonical.join("assets/a.txt").exists());
		let link = claude.join("my-skill");
		assert!(Linker::is_link(&link));
		assert_eq!(report.linked, vec![link]);
		assert!(report.already_linked.is_empty());
		assert!(report.conflicts.is_empty());
		assert!(report.failed.is_empty());
		assert_eq!(report.canonical, canonical);
	}

	// T-MASTER-HASH-STABLE: linking N agents to a materialized Master must
	// never change the Master's folder hash (links live OUTSIDE the Master;
	// the npx round-trip contract, Decision 7, depends on this).
	#[test]
	fn linking_agents_does_not_mutate_master_folder_hash() {
		use tempfile::tempdir;
		let tmp = tempdir().unwrap();
		let root = std::fs::canonicalize(tmp.path()).unwrap();
		let src = make_source(&root);
		let canonical = root.join(".agents/skills/my-skill");
		// Materialize the Master once (copy + npx exclusions), no links yet.
		install_universal(&src, &canonical, &[], LinkTarget::Absolute).unwrap();
		let before = skill::compute_skill_folder_hash(&canonical).unwrap();

		// Now link three agents to the SAME Master.
		let agents: Vec<std::path::PathBuf> = ["claude", "zed", "windsurf"]
			.iter()
			.map(|a| root.join(format!(".{a}/skills")))
			.collect();
		link_agents_to_canonical(&canonical, &agents, LinkTarget::Absolute)
			.unwrap();
		let after = skill::compute_skill_folder_hash(&canonical).unwrap();

		assert_eq!(before, after, "linking must not mutate the Master hash");
	}

	#[test]
	fn install_universal_rejects_non_absolute_canonical() {
		let report = install_universal(
			Path::new("/does/not/matter"),
			Path::new("rel/.agents/skills/x"),
			&[PathBuf::from("/tmp/agent")],
			LinkTarget::Absolute,
		);
		let err = report.unwrap_err();
		assert!(matches!(err, LinkError::NonAbsoluteTarget { .. }));
	}

	#[test]
	fn link_agents_to_canonical_folds_per_agent_into_report() {
		use tempfile::tempdir;
		let tmp = tempdir().unwrap();
		let root = std::fs::canonicalize(tmp.path()).unwrap();
		let canonical = root.join(".agents/skills/my-skill");
		std::fs::create_dir_all(&canonical).unwrap();
		std::fs::write(canonical.join("SKILL.md"), "real").unwrap();
		let claude = root.join(".claude/skills");
		let occupied = claude.join("my-skill");
		std::fs::create_dir_all(&occupied).unwrap();
		std::fs::write(occupied.join("SKILL.md"), "pre").unwrap();

		let report = link_agents_to_canonical(
			&canonical,
			std::slice::from_ref(&claude),
			LinkTarget::Absolute,
		)
		.unwrap();

		assert!(report.linked.is_empty());
		assert_eq!(report.conflicts, vec![occupied]);
		assert!(report.failed.is_empty());
	}

	#[cfg(unix)]
	#[test]
	fn relative_links_use_dotdot_global_links_are_absolute() {
		use tempfile::tempdir;
		let tmp = tempdir().unwrap();
		let root = std::fs::canonicalize(tmp.path()).unwrap();
		let src = make_source(&root);
		let canonical = root.join(".agents/skills/my-skill");
		let claude = root.join(".claude/skills");

		install_universal(
			&src,
			&canonical,
			std::slice::from_ref(&claude),
			LinkTarget::Relative,
		)
		.unwrap();
		let rel = std::fs::read_link(claude.join("my-skill")).unwrap();
		assert!(rel.is_relative(), "expected relative link, got {rel:?}");
		assert_eq!(rel, PathBuf::from("../../.agents/skills/my-skill"));

		let cursor = root.join(".cursor/skills");
		install_universal(
			&src,
			&canonical,
			std::slice::from_ref(&cursor),
			LinkTarget::Absolute,
		)
		.unwrap();
		let abs = std::fs::read_link(cursor.join("my-skill")).unwrap();
		assert!(abs.is_absolute(), "expected absolute link, got {abs:?}");
		assert_eq!(abs, canonical);
	}

	#[cfg(windows)]
	mod windows_specific {
		use super::super::*;
		use tempfile::tempdir;

		// T-WIN-JUNCTION-DETECT: force the junction path directly so a junction
		// is exercised even when Developer Mode would let symlink_dir succeed.
		#[test]
		fn create_junction_makes_a_reparse_point_recognized_by_is_link() {
			let tmp = tempdir().unwrap();
			let master = std::fs::canonicalize(tmp.path())
				.unwrap()
				.join(".agents\\skills\\my-skill");
			std::fs::create_dir_all(&master).unwrap();
			std::fs::write(master.join("SKILL.md"), "real").unwrap();
			let claude = std::fs::canonicalize(tmp.path())
				.unwrap()
				.join(".claude\\skills");
			std::fs::create_dir_all(&claude).unwrap();
			let link = claude.join("my-skill");

			create_junction(&master, &link).unwrap();

			assert!(Linker::is_link(&link), "junction must be a link");
			assert!(
				!std::fs::symlink_metadata(&link)
					.unwrap()
					.file_type()
					.is_symlink(),
				"a junction reports is_symlink()==false (0x0400 branch)"
			);
			// T-WIN-JUNCTION-REMOVE: unlink removes the junction, keeps Master.
			Linker::unlink(&link).unwrap();
			assert!(!Linker::is_link(&link), "junction must be gone");
			assert!(
				master.join("SKILL.md").exists(),
				"Master must survive unlink (remove_dir, not remove_dir_all)"
			);
		}

		#[test]
		fn linker_link_on_spaced_path_master_produces_junction() {
			let tmp = tempdir().unwrap();
			// Use canonicalize so the path is absolute (required by junction)
			let root = std::fs::canonicalize(tmp.path()).unwrap();
			// Master path contains a space - exercises the mklink /J path
			let master = root.join(".agents\\skills\\my skill");
			std::fs::create_dir_all(&master).unwrap();
			std::fs::write(master.join("SKILL.md"), "real").unwrap();
			let claude = root.join(".claude\\skills");
			std::fs::create_dir_all(&claude).unwrap();

			let outcome = Linker::link(
				&master,
				&claude,
				"my skill",
				LinkTarget::Absolute,
			)
			.unwrap();

			assert_eq!(outcome, LinkOutcome::Linked);
			let link = claude.join("my skill");
			assert!(
				Linker::is_link(&link),
				"spaced-path link must be a reparse point"
			);
			assert!(
				link.join("SKILL.md").exists(),
				"link must resolve through to the master"
			);
		}

		#[test]
		fn linker_relative_request_yields_absolute_junction_target() {
			let tmp = tempdir().unwrap();
			let root = std::fs::canonicalize(tmp.path()).unwrap();
			let master = root.join(".agents\\skills\\my-skill");
			std::fs::create_dir_all(&master).unwrap();
			std::fs::write(master.join("SKILL.md"), "real").unwrap();
			let claude = root.join(".claude\\skills");

			// Request Relative - on Windows the junction still uses an abs
			// target.
			Linker::link(&master, &claude, "my-skill", LinkTarget::Relative)
				.unwrap();

			let link = claude.join("my-skill");
			assert!(
				Linker::is_link(&link),
				"must be a reparse point (junction)"
			);
			assert!(
				link.join("SKILL.md").exists(),
				"junction must resolve to the master"
			);
			// Confirm the junction target is absolute (not relative).
			// On Windows, junctions do not support read_link in the same way;
			// instead verify via canonicalize that the target is absolute.
			let resolved = std::fs::canonicalize(&link).unwrap();
			assert!(
				resolved.is_absolute(),
				"junction target must resolve to an absolute path"
			);
		}
	}
}
