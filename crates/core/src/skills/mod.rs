pub mod discovery;
pub mod install_fetched;
pub mod linker;
pub mod lock;
pub mod prune;
pub mod removal;
pub mod rename;
pub mod resync;
pub mod update;
pub mod usage;

pub use discovery::{load_skills_from_dir, load_skills_from_dirs};
/// Undo a `materialize_universal_master` from the caller's OWN receipt. Shared
/// by every flow that writes files before it writes the lock (rename, fetched
/// install, the API import route) so none of them hand-rolls a second one.
pub use rename::rollback_materialized_install;

use std::path::{Path, PathBuf};

/// Return the directory that should be treated as a skill's source root.
///
/// A skill on disk is a directory containing `SKILL.md` (plus any assets and
/// scripts). Callers sometimes hold the directory and sometimes the `SKILL.md`
/// path itself; this normalizes both to the directory: if `path` points at a
/// `SKILL.md` file we return its parent, otherwise `path` is already the
/// directory and is returned as-is.
pub fn skill_source_root(path: &Path) -> PathBuf {
	if path
		.file_name()
		.is_some_and(|name| name == std::ffi::OsStr::new("SKILL.md"))
	{
		path.parent()
			.map(Path::to_path_buf)
			.unwrap_or_else(|| path.to_path_buf())
	} else {
		path.to_path_buf()
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn skill_source_root_resolves_skill_md_to_parent() {
		let dir = PathBuf::from("/tmp/some-skill");
		assert_eq!(skill_source_root(&dir.join("SKILL.md")), dir);
	}

	#[test]
	fn skill_source_root_passes_through_directory() {
		let dir = PathBuf::from("/tmp/some-skill");
		assert_eq!(skill_source_root(&dir), dir);
	}
}
