//! The one place core answers "is this the same path?". Two questions:
//! [`entry_identity`] (which ENTRY is this — leaf left unresolved) and
//! [`resolved_location`] (where does it really land — fully resolved). Both sit
//! on `skill::lock::resolve_existing`, so `..` after a symlink, a missing tail
//! and a symlinked ancestor (macOS `/var` -> `/private/var`) resolve the same
//! way everywhere. Never re-derive either with a `parent()`/`file_name()` walk:
//! `file_name()` is `None` for a path ending in `..`.

use std::path::{Component, Path, PathBuf};

/// A path's identity as an ENTRY: everything up to the final component is
/// resolved, the final component is kept as spelled. A Referrer and its Master
/// resolve to the same location, so resolving the leaf would read "delete this
/// Referrer" as "delete the Master". The parent is resolved so two spellings of
/// one directory (macOS `/var`, Windows short names) compare equal.
///
/// `skills::shape`'s compat-Referrer sweep has the same trap (a compat dir
/// reached through a symlinked ANCESTOR is a different `PathBuf` from the slot
/// it aliases). There is exactly one identity rule — do not re-derive it.
///
/// A path that does not end in a normal component (ends in `..`, or is a root)
/// has no leaf to keep and is resolved whole.
pub(crate) fn entry_identity(path: &Path) -> PathBuf {
	let mut parts = path.components();
	match parts.next_back() {
		Some(Component::Normal(leaf)) => {
			::skill::lock::resolve_existing(parts.as_path()).join(leaf)
		}
		_ => ::skill::lock::resolve_existing(path),
	}
}

/// Where `path` actually lands: every existing component resolved (leaf
/// included), a missing tail appended lexically.
pub(crate) fn resolved_location(path: &Path) -> PathBuf {
	::skill::lock::resolve_existing(path)
}

#[cfg(all(test, unix))]
mod tests {
	use super::*;

	#[test]
	fn both_queries_resolve_every_spelling() {
		let tmp = tempfile::tempdir().unwrap();
		let root = std::fs::canonicalize(tmp.path()).unwrap();
		let real = root.join("real");
		std::fs::create_dir_all(real.join("inner")).unwrap();
		// A Referrer-like leaf link.
		std::os::unix::fs::symlink(real.join("inner"), real.join("ref"))
			.unwrap();
		// macOS `/var` -> `/private/var` stand-in.
		std::os::unix::fs::symlink(&real, root.join("alias")).unwrap();
		std::os::unix::fs::symlink(real.join("inner"), root.join("link"))
			.unwrap();
		let alias = root.join("alias");

		let rows: Vec<(PathBuf, PathBuf, PathBuf)> = vec![
			(real.join("inner").join(".."), real.clone(), real.clone()),
			(root.join("link").join(".."), real.clone(), real.clone()),
			(
				alias.join("missing").join("leaf"),
				real.join("missing").join("leaf"),
				real.join("missing").join("leaf"),
			),
			(alias.join("ref"), real.join("ref"), real.join("inner")),
			(
				PathBuf::from(format!("{}/", alias.join("ref").display())),
				real.join("ref"),
				real.join("inner"),
			),
		];

		for (input, want_entry, want_resolved) in rows {
			assert_eq!(
				entry_identity(&input),
				want_entry,
				"entry_identity row {}",
				input.display()
			);
			assert_eq!(
				resolved_location(&input),
				want_resolved,
				"resolved_location row {}",
				input.display()
			);
		}
	}
}
