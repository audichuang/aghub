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
}
