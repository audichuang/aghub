//! Source-staging materializer: mode-aware writes of fetched tree entries
//! into a private staging dir, producing a skill folder byte-identical to a
//! clone. Distinct from Master materialization (which dereferences symlinks
//! and applies npx excludes).

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::tree::is_safe_tree_entry_name;

/// Git tree entry mode as relevant to staging materialization.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StagedEntryMode {
	/// git `100644`
	Regular,
	/// git `100755`
	Executable,
	/// git `120000` — `bytes` is the raw link target
	Symlink,
	/// git `160000` — submodule; never written
	Gitlink,
}

/// One fetched tree entry ready for staging.
pub struct StagedEntry {
	/// Repo-relative POSIX path (skill-root-relative).
	pub path: String,
	/// Raw blob bytes; for a symlink, the raw target.
	pub bytes: Vec<u8>,
	pub mode: StagedEntryMode,
}

/// Errors from [`stage_tree_entries`].
#[derive(Debug, Error)]
pub enum StageError {
	#[error("unsafe or escaping entry path: {0}")]
	UnsafePath(String),
	#[error("symlink target escapes staging root: {0}")]
	SymlinkEscapes(String),
	#[error("symlink target is absolute: {0}")]
	SymlinkAbsolute(String),
	#[error("symlink target is self-referential: {0}")]
	SymlinkSelf(String),
	#[error("destination already exists: {0}")]
	Collision(String),
	#[error("symlink not supported on this platform: {0}")]
	SymlinkUnsupported(String),
	#[error("IO error: {0}")]
	Io(#[from] std::io::Error),
}

/// Write fetched tree `entries` into `dest` (a private staging dir), producing
/// a skill folder byte-identical to a clone.
///
/// Creates `dest` if needed. Parent directories for nested entries are created
/// as needed. Gitlink entries are skipped. On any `Err`, the offending entry
/// is not created; already-written siblings may remain (caller discards the
/// staging dir).
pub fn stage_tree_entries(
	entries: impl IntoIterator<Item = StagedEntry>,
	dest: &Path,
) -> Result<(), StageError> {
	fs::create_dir_all(dest)?;

	// Normalized relative paths of written regular/exec/symlink entries.
	let mut written: HashSet<String> = HashSet::new();

	for entry in entries {
		match entry.mode {
			StagedEntryMode::Gitlink => continue,
			StagedEntryMode::Regular
			| StagedEntryMode::Executable
			| StagedEntryMode::Symlink => {
				stage_one(entry, dest, &mut written)?;
			}
		}
	}
	Ok(())
}

fn stage_one(
	entry: StagedEntry,
	dest: &Path,
	written: &mut HashSet<String>,
) -> Result<(), StageError> {
	let components = validate_entry_path(&entry.path)?;
	let rel = components.join("/");
	let target_path = join_under_dest(dest, &components);

	if written.contains(&rel) || path_exists(&target_path) {
		return Err(StageError::Collision(entry.path));
	}

	match entry.mode {
		StagedEntryMode::Regular => {
			write_file(&target_path, &entry.bytes, false)?;
		}
		StagedEntryMode::Executable => {
			write_file(&target_path, &entry.bytes, true)?;
		}
		StagedEntryMode::Symlink => {
			write_symlink(&entry.path, &rel, &entry.bytes, &target_path)?;
		}
		StagedEntryMode::Gitlink => unreachable!(),
	}

	written.insert(rel);
	Ok(())
}

/// Split and validate an entry path. Rejects absolute, empty, `.`/`..`, and
/// names that fail [`is_safe_tree_entry_name`].
fn validate_entry_path(path: &str) -> Result<Vec<String>, StageError> {
	if path.is_empty() || path.starts_with('/') {
		return Err(StageError::UnsafePath(path.to_string()));
	}
	let mut components = Vec::new();
	for part in path.split('/') {
		if part.is_empty()
			|| part == "."
			|| part == ".."
			|| !is_safe_tree_entry_name(part.as_bytes())
		{
			return Err(StageError::UnsafePath(path.to_string()));
		}
		components.push(part.to_string());
	}
	if components.is_empty() {
		return Err(StageError::UnsafePath(path.to_string()));
	}
	Ok(components)
}

fn join_under_dest(dest: &Path, components: &[String]) -> PathBuf {
	let mut out = dest.to_path_buf();
	for c in components {
		out.push(c);
	}
	out
}

fn path_exists(p: &Path) -> bool {
	// Count broken symlinks as existing so we never overwrite them.
	fs::symlink_metadata(p).is_ok()
}

fn write_file(
	path: &Path,
	bytes: &[u8],
	executable: bool,
) -> Result<(), StageError> {
	if let Some(parent) = path.parent() {
		fs::create_dir_all(parent)?;
	}
	fs::write(path, bytes)?;
	if executable {
		set_executable(path)?;
	}
	Ok(())
}

#[cfg(unix)]
fn set_executable(path: &Path) -> Result<(), StageError> {
	use std::os::unix::fs::PermissionsExt;
	let mut perms = fs::metadata(path)?.permissions();
	perms.set_mode(0o755);
	fs::set_permissions(path, perms)?;
	Ok(())
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> Result<(), StageError> {
	// Exec bit is a no-op on non-Unix.
	Ok(())
}

fn write_symlink(
	entry_path: &str,
	link_rel: &str,
	target_bytes: &[u8],
	link_fs_path: &Path,
) -> Result<(), StageError> {
	// Lexical containment before any filesystem mutation.
	check_symlink_target(link_rel, target_bytes, entry_path)?;

	if let Some(parent) = link_fs_path.parent() {
		fs::create_dir_all(parent)?;
	}

	create_symlink(target_bytes, link_fs_path, entry_path)
}

/// Resolve `target_bytes` lexically against the link's parent (relative to
/// the staging root). Reject absolute, escaping, and self targets.
fn check_symlink_target(
	link_rel: &str,
	target_bytes: &[u8],
	entry_path: &str,
) -> Result<(), StageError> {
	if target_bytes.starts_with(b"/") {
		return Err(StageError::SymlinkAbsolute(entry_path.to_string()));
	}

	// Symlink targets are path-like byte strings; require UTF-8 for lexical
	// component walking (git skill trees use portable UTF-8 paths).
	let target = std::str::from_utf8(target_bytes)
		.map_err(|_| StageError::SymlinkEscapes(entry_path.to_string()))?;

	let parent = match link_rel.rsplit_once('/') {
		Some((p, _)) => p,
		None => "",
	};

	let mut stack: Vec<&str> = if parent.is_empty() {
		Vec::new()
	} else {
		parent.split('/').collect()
	};

	for comp in target.split('/') {
		match comp {
			"" | "." => {}
			".." => {
				if stack.pop().is_none() {
					return Err(StageError::SymlinkEscapes(
						entry_path.to_string(),
					));
				}
			}
			c => stack.push(c),
		}
	}

	let resolved = stack.join("/");
	if resolved == link_rel {
		return Err(StageError::SymlinkSelf(entry_path.to_string()));
	}
	Ok(())
}

#[cfg(unix)]
fn create_symlink(
	target_bytes: &[u8],
	link_fs_path: &Path,
	_entry_path: &str,
) -> Result<(), StageError> {
	use std::ffi::OsStr;
	use std::os::unix::ffi::OsStrExt;
	use std::os::unix::fs::symlink;

	let target = OsStr::from_bytes(target_bytes);
	symlink(target, link_fs_path)?;
	Ok(())
}

#[cfg(not(unix))]
fn create_symlink(
	_target_bytes: &[u8],
	_link_fs_path: &Path,
	entry_path: &str,
) -> Result<(), StageError> {
	Err(StageError::SymlinkUnsupported(entry_path.to_string()))
}
