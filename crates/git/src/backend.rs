//! Git-object-only fetch backends: resolve a tip snapshot, list its tree,
//! read blobs, and materialize selected sub-trees.
//!
//! Implementations have no skill knowledge — callers (skill-update) own
//! discovery and selection policy. [`GixShallow`] is the shallow gix path;
//! a future `GithubRest` backend shares the same trait.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tempfile::TempDir;

use crate::credentials::Credentials;
use crate::error::{GitError, Result};
use crate::stage::{stage_tree_entries, StagedEntry, StagedEntryMode};
use crate::tree::is_safe_tree_entry_name;
use crate::RepoSnapshot;

/// A repo coordinate for a backend fetch: clone URL + optional ref.
pub struct SourceRef {
	pub url: String,
	/// Branch or tag name; `None` means the remote default branch.
	pub ref_: Option<String>,
}

/// One non-tree entry (blob / exec-blob / symlink / gitlink) in a snapshot's
/// tree, repo-relative POSIX path.
pub struct TreeEntry {
	pub path: String,
	pub mode: StagedEntryMode,
	/// Hex object id (blob oid; commit oid for gitlink).
	pub oid: String,
	/// Blob size in bytes; `None` for symlink / gitlink.
	pub size: Option<u64>,
}

/// Flat listing of a snapshot's non-tree entries.
pub struct RepoTree {
	pub entries: Vec<TreeEntry>,
}

/// Raw bytes for one blob oid.
pub struct Blob {
	pub oid: String,
	pub bytes: Vec<u8>,
}

/// Git-object-only fetch backend (no skill knowledge). One [`RepoSnapshot`] is
/// the shared key across all four calls.
pub trait RepoFetchBackend: Send + Sync {
	/// Resolve `source` to an immutable [`RepoSnapshot`] (fetches the tip).
	fn resolve(
		&self,
		source: &SourceRef,
		auth: Option<&Credentials>,
	) -> Result<RepoSnapshot>;

	/// Flat listing of the snapshot's non-tree entries.
	fn read_tree(&self, snapshot: &RepoSnapshot) -> Result<RepoTree>;

	/// Bytes for the requested blob oids (order not significant).
	fn read_blobs(
		&self,
		snapshot: &RepoSnapshot,
		oids: &[String],
	) -> Result<Vec<Blob>>;

	/// Write selected repo-relative folder sub-trees into `dest` through the
	/// shared Source-staging materializer. `""` = whole repo root.
	fn materialize(
		&self,
		snapshot: &RepoSnapshot,
		paths: &[&str],
		dest: &Path,
	) -> Result<()>;
}

/// Shallow (depth-1) gix bare-fetch backend. Fetches once in [`resolve`] and
/// reuses the cached bare repo for tree/blob/materialize calls.
pub struct GixShallow {
	timeout: Option<Duration>,
	/// commit_oid → fetched bare repo temp dir (kept alive for reuse).
	cache: Mutex<HashMap<String, Arc<TempDir>>>,
}

impl GixShallow {
	/// Create a backend with no network timeout.
	pub fn new() -> Self {
		Self::with_timeout(None)
	}

	/// Create a backend with an optional total network timeout.
	pub fn with_timeout(timeout: Option<Duration>) -> Self {
		Self {
			timeout,
			cache: Mutex::new(HashMap::new()),
		}
	}

	fn open_cached(&self, snapshot: &RepoSnapshot) -> Result<gix::Repository> {
		let cache = self.cache.lock().map_err(|_| {
			GitError::clone_failed("GixShallow cache lock poisoned")
		})?;
		let temp = cache.get(&snapshot.commit_oid).ok_or_else(|| {
			GitError::clone_failed(format!(
				"snapshot commit {} is not in the GixShallow cache; call resolve first",
				snapshot.commit_oid
			))
		})?;
		gix::open(temp.path()).map_err(|e| {
			GitError::clone_failed(format!(
				"Opening cached bare repo failed: {e}"
			))
		})
	}
}

impl Default for GixShallow {
	fn default() -> Self {
		Self::new()
	}
}

impl RepoFetchBackend for GixShallow {
	fn resolve(
		&self,
		source: &SourceRef,
		auth: Option<&Credentials>,
	) -> Result<RepoSnapshot> {
		let (temp, tip_oid) = crate::fetch::fetch_ref_to_temp(
			&source.url,
			source.ref_.as_deref(),
			auth,
			self.timeout,
		)?;

		let repo = gix::open(temp.path()).map_err(|e| {
			GitError::clone_failed(format!(
				"Opening fetched bare repo failed: {e}"
			))
		})?;

		let commit_oid = tip_oid.to_string();
		let tree = repo
			.find_object(tip_oid)
			.map_err(|e| {
				GitError::clone_failed(format!(
					"Looking up tip commit failed: {e}"
				))
			})?
			.peel_to_tree()
			.map_err(|e| {
				GitError::clone_failed(format!(
					"Peeling tip to tree failed: {e}"
				))
			})?;
		let tree_oid = tree.id.to_string();

		// Best-effort author time; None is acceptable (no chrono dep here).
		let commit_time = None;

		{
			let mut cache = self.cache.lock().map_err(|_| {
				GitError::clone_failed("GixShallow cache lock poisoned")
			})?;
			cache.insert(commit_oid.clone(), Arc::new(temp));
		}

		Ok(RepoSnapshot {
			commit_oid,
			tree_oid,
			commit_time,
		})
	}

	fn read_tree(&self, snapshot: &RepoSnapshot) -> Result<RepoTree> {
		let repo = self.open_cached(snapshot)?;
		let tree_oid = gix::ObjectId::from_hex(snapshot.tree_oid.as_bytes())
			.map_err(|e| {
				GitError::clone_failed(format!(
					"Invalid snapshot tree oid '{}': {e}",
					snapshot.tree_oid
				))
			})?;
		let tree = repo.find_tree(tree_oid).map_err(|e| {
			GitError::clone_failed(format!("Looking up root tree failed: {e}"))
		})?;

		let mut entries = Vec::new();
		walk_tree(&repo, &tree, "", &mut entries)?;
		Ok(RepoTree { entries })
	}

	fn read_blobs(
		&self,
		snapshot: &RepoSnapshot,
		oids: &[String],
	) -> Result<Vec<Blob>> {
		let repo = self.open_cached(snapshot)?;
		let mut out = Vec::with_capacity(oids.len());
		for oid_hex in oids {
			let oid =
				gix::ObjectId::from_hex(oid_hex.as_bytes()).map_err(|e| {
					GitError::clone_failed(format!(
						"Invalid blob oid '{oid_hex}': {e}"
					))
				})?;
			let object = repo.find_object(oid).map_err(|e| {
				GitError::clone_failed(format!(
					"Looking up blob {oid_hex} failed: {e}"
				))
			})?;
			out.push(Blob {
				oid: oid_hex.clone(),
				bytes: object.data.clone(),
			});
		}
		Ok(out)
	}

	fn materialize(
		&self,
		snapshot: &RepoSnapshot,
		paths: &[&str],
		dest: &Path,
	) -> Result<()> {
		let tree = self.read_tree(snapshot)?;
		let selected: Vec<&TreeEntry> = tree
			.entries
			.iter()
			.filter(|e| entry_matches_selection(&e.path, paths))
			.collect();

		let repo = self.open_cached(snapshot)?;
		let mut staged = Vec::with_capacity(selected.len());
		for entry in selected {
			let bytes = match entry.mode {
				StagedEntryMode::Gitlink => Vec::new(),
				StagedEntryMode::Regular
				| StagedEntryMode::Executable
				| StagedEntryMode::Symlink => {
					let oid = gix::ObjectId::from_hex(entry.oid.as_bytes())
						.map_err(|e| {
							GitError::clone_failed(format!(
								"Invalid entry oid '{}': {e}",
								entry.oid
							))
						})?;
					let object = repo.find_object(oid).map_err(|e| {
						GitError::clone_failed(format!(
							"Looking up entry {} failed: {e}",
							entry.oid
						))
					})?;
					object.data.clone()
				}
			};
			staged.push(StagedEntry {
				path: entry.path.clone(),
				bytes,
				mode: entry.mode,
			});
		}

		stage_tree_entries(staged, dest).map_err(|e| {
			GitError::clone_failed(format!("Staging materialize failed: {e}"))
		})
	}
}

/// Keep entries under any selected folder. Empty path selects the whole repo.
fn entry_matches_selection(path: &str, paths: &[&str]) -> bool {
	paths.iter().any(|p| {
		if p.is_empty() {
			true
		} else {
			path == *p || path.starts_with(&format!("{p}/"))
		}
	})
}

fn walk_tree(
	repo: &gix::Repository,
	tree: &gix::Tree<'_>,
	prefix: &str,
	out: &mut Vec<TreeEntry>,
) -> Result<()> {
	for entry in tree.iter() {
		let entry = entry.map_err(|e| {
			GitError::clone_failed(format!("Reading tree entry failed: {e}"))
		})?;
		let name_bstr = entry.filename();
		if !is_safe_tree_entry_name(name_bstr.as_ref()) {
			return Err(GitError::clone_failed(format!(
				"unsafe git tree entry name: {}",
				String::from_utf8_lossy(name_bstr.as_ref())
			)));
		}
		let name = String::from_utf8_lossy(name_bstr.as_ref()).into_owned();
		let path = if prefix.is_empty() {
			name
		} else {
			format!("{prefix}/{name}")
		};
		let mode = entry.mode();
		if mode.is_tree() {
			let sub = repo.find_tree(entry.object_id()).map_err(|e| {
				GitError::clone_failed(format!(
					"Looking up sub-tree for '{path}' failed: {e}"
				))
			})?;
			walk_tree(repo, &sub, &path, out)?;
			continue;
		}

		let staged_mode = if mode.is_link() {
			StagedEntryMode::Symlink
		} else if mode.is_commit() {
			StagedEntryMode::Gitlink
		} else if format!("{:o}", mode.value()) == "100755" {
			StagedEntryMode::Executable
		} else {
			StagedEntryMode::Regular
		};

		let oid = entry.object_id().to_string();
		let size = match staged_mode {
			StagedEntryMode::Symlink | StagedEntryMode::Gitlink => None,
			StagedEntryMode::Regular | StagedEntryMode::Executable => {
				let object = entry.object().map_err(|e| {
					GitError::clone_failed(format!(
						"Reading blob for '{path}' failed: {e}"
					))
				})?;
				Some(object.data.len() as u64)
			}
		};

		out.push(TreeEntry {
			path,
			mode: staged_mode,
			oid,
			size,
		});
	}
	Ok(())
}
