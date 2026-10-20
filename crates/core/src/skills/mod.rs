pub mod adoption;
pub mod agent_dirs;
pub mod discovery;
pub(crate) mod enumerate;
pub mod health;
pub mod install_fetched;
pub mod install_local;
pub mod linker;
pub mod lock;
pub(crate) mod path_identity;
pub mod plugin_roots;
pub mod prune;
pub mod removal;
pub mod rename;
pub mod repair;
pub mod resync;
pub mod shape;
pub mod update;
pub mod usage;

pub use discovery::{load_skills_from_dir, load_skills_from_dirs};
pub use install_local::{
	install_local_skill, LocalInstallLock, LocalSkillInstallReport,
	LocalSkillInstallRequest,
};
/// Undo a `materialize_universal_master` from the caller's OWN receipt. Shared
/// by every flow that writes files before it writes the lock (rename, fetched
/// install, the API import route) so none of them hand-rolls a second one.
pub use rename::rollback_materialized_install;

use std::path::{Path, PathBuf};

/// Return the directory that should be treated as a skill's source root.
///
/// A skill on disk is a directory containing `SKILL.md` (plus any assets and
/// scripts). Callers sometimes hold the directory and sometimes the marker
/// path itself; this normalizes both to the directory: if `path` names a
/// marker (`enumerate::SKILL_MARKERS`, so `skill.md` too) and is not an
/// existing directory we return its parent, otherwise `path` is already the
/// directory and is returned as-is. The one disk probe keeps it idempotent on
/// a skill folder literally named `skill.md`; a missing path still resolves
/// lexically. The one "where is this skill's folder" rule;
/// `removal::skill_root` reads a model through it.
/// See docs/history/core-transfer.md#lowercase-skill-marker-resolved-to-the-file
pub fn skill_source_root(path: &Path) -> PathBuf {
	if path.file_name().is_some_and(|name| {
		enumerate::SKILL_MARKERS
			.iter()
			.any(|marker| name == std::ffi::OsStr::new(marker))
	}) && !path.is_dir()
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
	fn skill_source_root_resolves_lowercase_skill_md_to_parent() {
		let dir = PathBuf::from("/tmp/some-skill");
		assert_eq!(skill_source_root(&dir.join("skill.md")), dir);
	}

	#[test]
	fn skill_source_root_keeps_directory_named_like_a_marker() {
		let tmp = tempfile::tempdir().unwrap();
		let dir = tmp.path().join("skill.md");
		std::fs::create_dir(&dir).unwrap();
		assert_eq!(skill_source_root(&dir), dir);
		assert_eq!(skill_source_root(&skill_source_root(&dir)), dir);
	}

	#[test]
	fn skill_source_root_passes_through_directory() {
		let dir = PathBuf::from("/tmp/some-skill");
		assert_eq!(skill_source_root(&dir), dir);
	}
}
