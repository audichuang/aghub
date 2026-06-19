//! Cross-platform directory-link primitives ported from jiweiyeah/Skills-Manager
//! (MIT) — linker.rs: is_symlink_or_junction / remove_symlink_or_junction /
//! create_windows_symlink / normalize_path. SM's iflow copy-mode is intentionally
//! NOT ported: aghub bans copy as a skill-install outcome.

#[allow(unused_imports)]
use std::io;
#[allow(unused_imports)]
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
}
