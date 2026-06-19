//! Cross-platform directory-link primitives ported from jiweiyeah/Skills-Manager
//! (MIT) — linker.rs: is_symlink_or_junction / remove_symlink_or_junction /
//! create_windows_symlink / normalize_path. SM's iflow copy-mode is intentionally
//! NOT ported: aghub bans copy as a skill-install outcome.

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
#[cfg_attr(not(test), allow(dead_code))]
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
		let e: LinkError = io::Error::new(io::ErrorKind::Other, "boom").into();
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
}
