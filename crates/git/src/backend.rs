//! Git-object-only fetch backends: resolve a tip snapshot, list its tree,
//! read blobs, and materialize selected sub-trees.
//!
//! Implementations have no skill knowledge — callers (skill-update) own
//! discovery and selection policy. [`GixShallow`] is the shallow gix path and
//! `GithubRest` (see `github_rest`) is the GitHub REST fast-path; both implement
//! the same trait.

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
#[derive(Clone, Debug)]
pub struct SourceRef {
	pub url: String,
	/// Branch or tag name; `None` means the remote default branch.
	pub ref_: Option<String>,
}

/// One non-tree entry (blob / exec-blob / symlink / gitlink) in a snapshot's
/// tree, repo-relative POSIX path.
#[derive(Clone, Debug)]
pub struct TreeEntry {
	pub path: String,
	pub mode: StagedEntryMode,
	/// Hex object id (blob oid; commit oid for gitlink).
	pub oid: String,
	/// Blob size in bytes; `None` for symlink / gitlink.
	pub size: Option<u64>,
}

/// Flat listing of a snapshot's non-tree entries.
#[derive(Clone, Debug)]
pub struct RepoTree {
	pub entries: Vec<TreeEntry>,
}

/// Raw bytes for one blob oid.
#[derive(Clone, Debug)]
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

	/// Read tree metadata and the blobs selected from that metadata as one
	/// backend operation. Backends with an operation deadline override this so
	/// both phases share the same absolute cutoff.
	fn read_tree_and_blobs(
		&self,
		snapshot: &RepoSnapshot,
		select: &dyn Fn(&RepoTree) -> Vec<String>,
	) -> Result<(RepoTree, Vec<Blob>)> {
		let tree = self.read_tree(snapshot)?;
		let oids = select(&tree);
		let blobs = self.read_blobs(snapshot, &oids)?;
		Ok((tree, blobs))
	}

	/// Write selected repo-relative folder sub-trees into `dest` through the
	/// shared Source-staging materializer. `""` = whole repo root.
	fn materialize(
		&self,
		snapshot: &RepoSnapshot,
		paths: &[&str],
		dest: &Path,
	) -> Result<()>;
}

/// Shallow (depth-1) gix bare-fetch backend with a final system-git fallback
/// for HTTPS non-GitHub hosts that rely on OS credential helpers. Fetches once
/// in [`resolve`] and reuses the cached repo for later calls.
pub struct GixShallow {
	timeout: Option<Duration>,
	/// commit_oid → fetched bare repo temp dir (kept alive for reuse).
	cache: Mutex<HashMap<String, Arc<TempDir>>>,
	/// tree_oid → walked listing. `materialize` re-reads the tree, and a walk
	/// touches every object in the tip, so one walk per snapshot is enough.
	/// If the repo cache above ever gains eviction, evict here in lockstep —
	/// otherwise a listing could outlive the bare repo its oids point into.
	tree_cache: Mutex<HashMap<String, RepoTree>>,
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
			tree_cache: Mutex::new(HashMap::new()),
		}
	}

	/// Open the cached bare repo for `snapshot`, returning the repository TOGETHER
	/// with a clone of its `TempDir` guard.
	///
	/// The guard must travel with the repository: gix reads objects LAZILY, so a
	/// caller holding only the `Repository` could have the directory deleted from
	/// under it the moment the last `Arc` elsewhere dropped — and the resulting
	/// object-lookup error surfaces to the user as `uncheckable/network` for a
	/// source that is perfectly reachable. Callers must keep the guard alive for
	/// every read they make.
	fn open_cached(
		&self,
		snapshot: &RepoSnapshot,
	) -> Result<(gix::Repository, Arc<TempDir>)> {
		let temp = {
			let cache = self.cache.lock().map_err(|_| {
				GitError::clone_failed("GixShallow cache lock poisoned")
			})?;
			Arc::clone(cache.get(&snapshot.commit_oid).ok_or_else(|| {
				GitError::clone_failed(format!(
					"snapshot commit {} is not in the GixShallow cache; call resolve first",
					snapshot.commit_oid
				))
			})?)
		};
		let repo = gix::open(temp.path()).map_err(|e| {
			GitError::clone_failed(format!(
				"Opening cached bare repo failed: {e}"
			))
		})?;
		Ok((repo, temp))
	}

	fn fetch_tip(
		&self,
		source: &SourceRef,
		auth: Option<&Credentials>,
	) -> Result<(TempDir, gix::ObjectId)> {
		match crate::fetch::fetch_ref_to_temp(
			&source.url,
			source.ref_.as_deref(),
			auth,
			self.timeout,
		) {
			Ok(fetched) => Ok(fetched),
			Err(gix_error) => {
				if !should_try_system_git(&source.url) {
					return Err(gix_error);
				}
				let temp = match crate::system_git::clone_to_temp_system_git(
					&source.url,
					source.ref_.as_deref(),
				) {
					Ok(temp) => temp,
					Err(_) => return Err(gix_error),
				};
				let repo = gix::open(temp.path()).map_err(|e| {
					GitError::clone_failed(format!(
						"Opening system-git clone failed: {e}"
					))
				})?;
				let tip = repo
					.head_id()
					.map_err(|e| {
						GitError::clone_failed(format!(
							"Resolving system-git HEAD failed: {e}"
						))
					})?
					.detach();
				Ok((temp, tip))
			}
		}
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
		let (temp, tip_oid) = self.fetch_tip(source, auth)?;

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
			// `or_insert`, NOT `insert`: a commit oid names immutable content, so
			// a second resolve landing on it describes the same objects — and
			// replacing the entry dropped the previous `TempDir` while another
			// group could still be lazily reading objects out of it.
			cache
				.entry(commit_oid.clone())
				.or_insert_with(|| Arc::new(temp));
		}

		Ok(RepoSnapshot {
			commit_oid,
			tree_oid,
			commit_time,
		})
	}

	fn read_tree(&self, snapshot: &RepoSnapshot) -> Result<RepoTree> {
		// A hit is itself proof this tree was already walked — and therefore
		// that its snapshot was resolved — so it may answer before
		// `open_cached`'s "call resolve first" precondition check.
		{
			let cached = self.tree_cache.lock().map_err(|_| {
				GitError::clone_failed("GixShallow tree cache lock poisoned")
			})?;
			if let Some(tree) = cached.get(&snapshot.tree_oid) {
				return Ok(tree.clone());
			}
		}
		// `_guard` is load-bearing: gix reads objects lazily, so the temp dir
		// must outlive every read made through `repo`.
		let (repo, _guard) = self.open_cached(snapshot)?;
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
		let walked = RepoTree { entries };
		self.tree_cache
			.lock()
			.map_err(|_| {
				GitError::clone_failed("GixShallow tree cache lock poisoned")
			})?
			.insert(snapshot.tree_oid.clone(), walked.clone());
		Ok(walked)
	}

	fn read_blobs(
		&self,
		snapshot: &RepoSnapshot,
		oids: &[String],
	) -> Result<Vec<Blob>> {
		// `_guard` is load-bearing: gix reads objects lazily, so the temp dir
		// must outlive every read made through `repo`.
		let (repo, _guard) = self.open_cached(snapshot)?;
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

		// `_guard` is load-bearing: gix reads objects lazily, so the temp dir
		// must outlive every read made through `repo`.
		let (repo, _guard) = self.open_cached(snapshot)?;
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

		stage_tree_entries(staged, paths, dest).map_err(|e| {
			GitError::clone_failed(format!("Staging materialize failed: {e}"))
		})
	}
}

fn should_try_system_git(url: &str) -> bool {
	let Ok(url) = url::Url::parse(url) else {
		return false;
	};
	url.scheme() == "https"
		&& url
			.host_str()
			.is_some_and(|host| !crate::github_rest::is_github_com_host(host))
		&& crate::system_git::system_git_available()
}

/// Keep entries under any selected folder. Empty path selects the whole repo.
/// Shared by every backend's `materialize` so the selection rule cannot drift.
pub(crate) fn entry_matches_selection(path: &str, paths: &[&str]) -> bool {
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

		let oid = entry.object_id();
		let size = match staged_mode {
			// The match is load-bearing: a gitlink's oid names a commit that a
			// shallow fetch never transferred, so it must not reach the odb.
			StagedEntryMode::Symlink | StagedEntryMode::Gitlink => None,
			StagedEntryMode::Regular | StagedEntryMode::Executable => {
				// Object header only — no blob decompression. A packed delta
				// reads its declared result size from the delta header
				// (20-byte probe), so this stays cheap inside a delta chain.
				let header = repo.find_header(oid).map_err(|e| {
					GitError::clone_failed(format!(
						"Reading blob header for '{path}' failed: {e}"
					))
				})?;
				Some(header.size())
			}
		};

		out.push(TreeEntry {
			path,
			mode: staged_mode,
			oid: oid.to_string(),
			size,
		});
	}
	Ok(())
}
