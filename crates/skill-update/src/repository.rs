//! Skill-aware composite over [`aghub_git::RepoFetchBackend`]: snapshot pin,
//! single REST→gix→system-git fallback owner, discovery (`list`), and
//! selection-scoped materialization (`fetch`). Surfaces never re-decide the
//! transport.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aghub_git::{
	github_api_host, Blob, Credentials, GitError, GithubRest, GixShallow,
	HttpTransport, RepoFetchBackend, RepoSnapshot, ReqwestTransport, TreeEntry,
};
use skill::{discover_from_entries, CandidateEntry, SkillPath};

use crate::{https_only_token, FetchError, FetchedRepo, SourceRef};

/// Per-fetch HTTP timeout for the gix shallow backend (matches the historical
/// `GitFetcher` bound so a stuck remote cannot hang forever).
const FETCH_HTTP_TIMEOUT: Duration = Duration::from_secs(30);
const CATALOG_MAX_DEPTH: usize = 10;

/// Which backend resolved a given `commit_oid`. Memoized so `list`/`fetch`
/// always hit the same slot that produced the snapshot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BackendKind {
	Rest,
	Gix,
}

#[derive(Clone)]
struct BackendMemo {
	kind: BackendKind,
	source: aghub_git::SourceRef,
	auth: Option<Credentials>,
}

/// What to materialize from a pinned snapshot.
#[derive(Clone, Copy, Debug)]
pub enum FetchSelection<'a> {
	/// Exact skill folders (root skill = empty [`SkillPath`]).
	Skills(&'a [SkillPath]),
	/// Whole catalog tree (source sync/diff classification).
	CatalogSnapshot,
}

/// Discovered skills for one immutable snapshot.
#[derive(Clone, Debug)]
pub struct SkillCatalog {
	pub snapshot: RepoSnapshot,
	pub skills: Vec<CatalogSkill>,
}

/// One skill listed from a snapshot's tree + frontmatter.
#[derive(Clone, Debug)]
pub struct CatalogSkill {
	/// Repo-relative folder, POSIX (`""` = root skill).
	pub skill_path: String,
	pub name: String,
	pub description: Option<String>,
	pub version: Option<String>,
	pub author: Option<String>,
}

/// Errors from [`SkillRepository`] operations.
#[derive(Debug)]
pub enum SkillRepoError {
	/// Authentication failure (401/403 / credential messages).
	Auth,
	/// Network / transport / other non-auth failure.
	Network,
	/// Root-skill whole-folder size preflight exceeded Source-hash bounds.
	RootSkillTooLarge,
}

impl SkillRepoError {
	/// Stable machine code for CLI/API mapping.
	pub fn code(&self) -> &'static str {
		match self {
			Self::Auth => "AUTH",
			Self::Network => "NETWORK",
			Self::RootSkillTooLarge => "ROOT_SKILL_TOO_LARGE",
		}
	}
}

/// Map a [`SkillRepoError`] into the thinner [`FetchError`] used by
/// [`crate::Fetcher`] surfaces (root-size refusal is not a Fetcher concern —
/// callers that need it use [`SkillRepository`] directly).
pub fn skill_repo_to_fetch_error(e: SkillRepoError) -> FetchError {
	match e {
		SkillRepoError::Auth => FetchError::Auth,
		SkillRepoError::Network | SkillRepoError::RootSkillTooLarge => {
			FetchError::Network
		}
	}
}

/// Skill-aware orchestrator: owns the REST→gix→system-git composite and
/// snapshot pinning.
pub struct SkillRepository {
	rest: Option<Arc<dyn RepoFetchBackend>>,
	gix: Arc<dyn RepoFetchBackend>,
	/// `commit_oid` → backend plus source/auth for a pinned late fallback.
	memo: Mutex<HashMap<String, BackendMemo>>,
}

impl Default for SkillRepository {
	fn default() -> Self {
		Self::new()
	}
}

impl SkillRepository {
	/// Production: `GithubRest(ReqwestTransport)` + `GixShallow`, whose final
	/// tail is system git for OS-credential-helper-only private hosts.
	pub fn new() -> Self {
		Self::with_http_transport(Arc::new(ReqwestTransport::new()))
	}

	/// Build the production backend composite over an injectable HTTP
	/// transport. This is the production-construction seam used by deadline
	/// tests; fallback remains the real gix→system-git tail.
	#[doc(hidden)]
	pub fn with_http_transport(transport: Arc<dyn HttpTransport>) -> Self {
		let rest: Arc<dyn RepoFetchBackend> = Arc::new(
			GithubRest::new(transport).with_timeout(FETCH_HTTP_TIMEOUT),
		);
		let gix: Arc<dyn RepoFetchBackend> =
			Arc::new(GixShallow::with_timeout(Some(FETCH_HTTP_TIMEOUT)));
		Self::with_backends(Some(rest), gix)
	}

	/// Injected backends for tests. `rest = None` means gix-only.
	pub fn with_backends(
		rest: Option<Arc<dyn RepoFetchBackend>>,
		gix: Arc<dyn RepoFetchBackend>,
	) -> Self {
		Self {
			rest,
			gix,
			memo: Mutex::new(HashMap::new()),
		}
	}

	/// Resolve the tip to an immutable snapshot. THE single fallback owner:
	/// REST on github hosts when present, else gix; REST `RestFallback` → gix
	/// once. Memoizes which backend served the `commit_oid`.
	pub fn resolve(
		&self,
		sr: &SourceRef,
		token: Option<&str>,
	) -> Result<RepoSnapshot, SkillRepoError> {
		let resolved = aghub_git::resolve_remote_source(&sr.source)
			.map_err(|_| SkillRepoError::Network)?;
		let clone_url = resolved.clone_url;
		// Prefer the host from resolve_remote_source; fall back to URL parse.
		let host_owned = resolved.host.or_else(|| host_of_url(&clone_url));
		let auth = https_only_token(&clone_url, token)
			.map(|t| Credentials::new("x-access-token", t));
		let git_sr = aghub_git::SourceRef {
			url: clone_url,
			ref_: sr.ref_.clone(),
		};

		let try_rest =
			host_owned.as_deref().and_then(github_api_host).is_some()
				&& self.rest.is_some();

		if try_rest {
			let rest = self.rest.as_ref().expect("checked above");
			match rest.resolve(&git_sr, auth.as_ref()) {
				Ok(snap) => {
					self.remember(
						&snap.commit_oid,
						BackendKind::Rest,
						&git_sr,
						auth.as_ref(),
					)?;
					return Ok(snap);
				}
				Err(GitError::RestFallback(_)) => {
					// Single fall-through to gix; never re-decided later.
				}
				Err(e) => return Err(map_git_error(e)),
			}
		}

		let snap = self
			.gix
			.resolve(&git_sr, auth.as_ref())
			.map_err(map_git_error)?;
		self.remember(
			&snap.commit_oid,
			BackendKind::Gix,
			&git_sr,
			auth.as_ref(),
		)?;
		Ok(snap)
	}

	/// Read tree + shared discovery policy + SKILL.md frontmatter blobs.
	/// Carries the snapshot on the catalog for a later pinned `fetch`.
	pub fn list(
		&self,
		snapshot: &RepoSnapshot,
	) -> Result<SkillCatalog, SkillRepoError> {
		let (tree, blobs) =
			self.execute_backend(snapshot, |backend, snapshot| {
				backend.read_tree_and_blobs(snapshot, &|tree| {
					catalog_skill_md_entries(tree)
						.into_iter()
						.map(|(entry, _)| entry.oid.clone())
						.collect()
				})
			})?;
		let skill_md_entries = catalog_skill_md_entries(&tree);
		let blob_by_oid: HashMap<&str, &Blob> =
			blobs.iter().map(|b| (b.oid.as_str(), b)).collect();

		// Candidate stream for discovery (needs frontmatter `name`).
		let mut candidates: Vec<CandidateEntry> = Vec::new();
		// Keep full frontmatter next to the folder for the final catalog.
		let mut meta_by_folder: HashMap<String, FrontmatterMeta> =
			HashMap::new();

		for (entry, folder) in &skill_md_entries {
			let Some(blob) = blob_by_oid.get(entry.oid.as_str()) else {
				continue;
			};
			let content = String::from_utf8_lossy(&blob.bytes);
			let Some(meta) = parse_frontmatter_meta(&content) else {
				continue;
			};
			let depth = folder_depth(folder);
			candidates.push(CandidateEntry {
				path: PathBuf::from(folder.as_str()),
				depth,
				has_skill_md: true,
				name: Some(meta.name.clone()),
			});
			meta_by_folder.insert(folder.clone(), meta);
		}

		// Match install discovery: full_depth + generous max depth.
		let discovered =
			discover_from_entries(candidates, CATALOG_MAX_DEPTH, true);

		let mut skills = Vec::with_capacity(discovered.len());
		for path in discovered {
			let folder = path.to_string_lossy().replace('\\', "/");
			let Some(meta) = meta_by_folder.get(&folder) else {
				continue;
			};
			skills.push(CatalogSkill {
				skill_path: folder,
				name: meta.name.clone(),
				description: meta.description.clone(),
				version: meta.version.clone(),
				author: meta.author.clone(),
			});
		}

		Ok(SkillCatalog {
			snapshot: snapshot.clone(),
			skills,
		})
	}

	/// Materialize ONLY the selection into a fresh TempDir; return
	/// [`FetchedRepo`] pinned to `snapshot` (never re-resolves the ref).
	pub fn fetch(
		&self,
		snapshot: &RepoSnapshot,
		selection: FetchSelection<'_>,
	) -> Result<FetchedRepo, SkillRepoError> {
		let path_owned: Vec<String> = match selection {
			FetchSelection::Skills(paths) => {
				paths.iter().map(|p| p.as_str().to_string()).collect()
			}
			FetchSelection::CatalogSnapshot => {
				let catalog = self.list(snapshot)?;
				let mut paths: Vec<String> = catalog
					.skills
					.into_iter()
					.map(|skill| skill.skill_path)
					.collect();
				paths.push("CHANGELOG.md".to_string());
				paths
			}
		};
		if path_owned.iter().any(String::is_empty) {
			let tree = self
				.execute_backend(snapshot, |backend, snapshot| {
					backend.read_tree(snapshot)
				})?;
			root_size_preflight(&tree)?;
		}
		let path_refs: Vec<&str> =
			path_owned.iter().map(String::as_str).collect();

		let dest =
			tempfile::TempDir::new().map_err(|_| SkillRepoError::Network)?;
		self.execute_backend(snapshot, |backend, snapshot| {
			backend.materialize(snapshot, &path_refs, dest.path())
		})?;

		Ok(FetchedRepo {
			root: dest.path().to_path_buf(),
			snapshot: snapshot.clone(),
			_guard: Some(Arc::new(dest)),
		})
	}

	fn remember(
		&self,
		commit_oid: &str,
		kind: BackendKind,
		source: &aghub_git::SourceRef,
		auth: Option<&Credentials>,
	) -> Result<(), SkillRepoError> {
		let mut memo = self.memo.lock().map_err(|_| SkillRepoError::Network)?;
		memo.insert(
			commit_oid.to_string(),
			BackendMemo {
				kind,
				source: source.clone(),
				auth: auth.cloned(),
			},
		);
		Ok(())
	}

	fn execute_backend<T>(
		&self,
		snapshot: &RepoSnapshot,
		operation: impl Fn(
			&dyn RepoFetchBackend,
			&RepoSnapshot,
		) -> aghub_git::Result<T>,
	) -> Result<T, SkillRepoError> {
		let memo = self.memo_for(&snapshot.commit_oid)?;
		match memo.kind {
			BackendKind::Gix => {
				operation(self.gix.as_ref(), snapshot).map_err(map_git_error)
			}
			BackendKind::Rest => {
				let rest = self.rest.as_ref().ok_or(SkillRepoError::Network)?;
				match operation(rest.as_ref(), snapshot) {
					Ok(value) => Ok(value),
					Err(GitError::RestFallback(_)) => {
						let gix_snapshot =
							self.fallback_to_gix(snapshot, memo)?;
						operation(self.gix.as_ref(), &gix_snapshot)
							.map_err(map_git_error)
					}
					Err(error) => Err(map_git_error(error)),
				}
			}
		}
	}

	fn memo_for(
		&self,
		commit_oid: &str,
	) -> Result<BackendMemo, SkillRepoError> {
		let memo = self.memo.lock().map_err(|_| SkillRepoError::Network)?;
		memo.get(commit_oid).cloned().ok_or(SkillRepoError::Network)
	}

	fn fallback_to_gix(
		&self,
		snapshot: &RepoSnapshot,
		memo: BackendMemo,
	) -> Result<RepoSnapshot, SkillRepoError> {
		let gix_snapshot = self
			.gix
			.resolve(&memo.source, memo.auth.as_ref())
			.map_err(map_git_error)?;
		if gix_snapshot.commit_oid != snapshot.commit_oid {
			return Err(SkillRepoError::Network);
		}
		self.remember(
			&snapshot.commit_oid,
			BackendKind::Gix,
			&memo.source,
			memo.auth.as_ref(),
		)?;
		Ok(gix_snapshot)
	}
}

fn catalog_skill_md_entries(
	tree: &aghub_git::RepoTree,
) -> Vec<(&TreeEntry, String)> {
	tree.entries
		.iter()
		.filter(|entry| is_skill_md_path(&entry.path))
		.map(|entry| {
			let folder = folder_of_skill_md(&entry.path);
			(entry, folder)
		})
		.filter(|(_, folder)| folder_depth(folder) <= CATALOG_MAX_DEPTH)
		.collect()
}

/// Convert a lock `skillPath` (`"<dir>/SKILL.md"` or `"SKILL.md"`) into a
/// validated skill-folder [`SkillPath`] (root → empty path).
pub fn skill_folder_from_lock_path(skill_path: &str) -> Option<SkillPath> {
	let folder = if skill_path == "SKILL.md" {
		""
	} else if let Some(rest) = skill_path.strip_suffix("/SKILL.md") {
		rest
	} else {
		skill_path
	};
	SkillPath::parse(folder).ok()
}

/// Whole-root size preflight: sum tree metadata (entry count + declared blob
/// sizes). REST refuses without downloading blobs. The gix 0.84 fallback has
/// already transferred the depth-1 tip's blobs, but still refuses before any
/// materialization (see the spec's documented known limitation).
fn root_size_preflight(
	tree: &aghub_git::RepoTree,
) -> Result<(), SkillRepoError> {
	let count = tree.entries.len();
	let bytes: u64 = tree
		.entries
		.iter()
		.filter_map(|e| e.size)
		.fold(0u64, |acc, s| acc.saturating_add(s));
	if count > skill::hash::MAX_FILES || bytes > skill::hash::MAX_TOTAL_BYTES {
		return Err(SkillRepoError::RootSkillTooLarge);
	}
	Ok(())
}

fn map_git_error(e: GitError) -> SkillRepoError {
	let msg = e.to_string();
	let lower = msg.to_lowercase();
	if lower.contains("auth")
		|| lower.contains("401")
		|| lower.contains("403")
		|| lower.contains("credential")
	{
		SkillRepoError::Auth
	} else {
		SkillRepoError::Network
	}
}

fn host_of_url(url: &str) -> Option<String> {
	let after_scheme = url.split_once("://")?.1;
	let authority = after_scheme.split(['/', '?', '#']).next()?;
	let authority = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
	let host = if let Some(rest) = authority.strip_prefix('[') {
		rest.split_once(']')?.0
	} else {
		authority.split(':').next()?
	};
	(!host.is_empty()).then(|| host.to_ascii_lowercase())
}

fn is_skill_md_path(path: &str) -> bool {
	Path::new(path)
		.file_name()
		.and_then(|n| n.to_str())
		.is_some_and(|n| n.eq_ignore_ascii_case("SKILL.md"))
}

fn folder_of_skill_md(path: &str) -> String {
	match path.rfind('/') {
		Some(i) => path[..i].to_string(),
		None => String::new(),
	}
}

fn folder_depth(folder: &str) -> usize {
	if folder.is_empty() {
		0
	} else {
		folder.split('/').filter(|c| !c.is_empty()).count()
	}
}

#[derive(Clone, Debug)]
struct FrontmatterMeta {
	name: String,
	description: Option<String>,
	version: Option<String>,
	author: Option<String>,
}

fn parse_frontmatter_meta(content: &str) -> Option<FrontmatterMeta> {
	// Reuse the skill crate's frontmatter parser (name + description required).
	let skill = skill::parse_skill_md(content).ok()?;
	Some(FrontmatterMeta {
		name: skill.name,
		description: Some(skill.description),
		version: skill.version,
		author: skill.author,
	})
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::sync::atomic::{AtomicUsize, Ordering};

	/// Minimal backend that serves a one-skill tree via `read_tree`/`read_blobs`.
	struct ListFixtureBackend {
		skill_md: Vec<u8>,
		read_tree_calls: AtomicUsize,
	}

	impl RepoFetchBackend for ListFixtureBackend {
		fn resolve(
			&self,
			_source: &aghub_git::SourceRef,
			_auth: Option<&Credentials>,
		) -> aghub_git::Result<RepoSnapshot> {
			Ok(RepoSnapshot {
				commit_oid: "c".into(),
				tree_oid: "t".into(),
				commit_time: None,
			})
		}

		fn read_tree(
			&self,
			_s: &RepoSnapshot,
		) -> aghub_git::Result<aghub_git::RepoTree> {
			self.read_tree_calls.fetch_add(1, Ordering::SeqCst);
			Ok(aghub_git::RepoTree {
				entries: vec![TreeEntry {
					path: "skills/demo/SKILL.md".into(),
					mode: aghub_git::StagedEntryMode::Regular,
					oid: "blob1".into(),
					size: Some(self.skill_md.len() as u64),
				}],
			})
		}

		fn read_blobs(
			&self,
			_s: &RepoSnapshot,
			oids: &[String],
		) -> aghub_git::Result<Vec<Blob>> {
			Ok(oids
				.iter()
				.filter(|o| o.as_str() == "blob1")
				.map(|o| Blob {
					oid: o.clone(),
					bytes: self.skill_md.clone(),
				})
				.collect())
		}

		fn materialize(
			&self,
			_s: &RepoSnapshot,
			_p: &[&str],
			_d: &Path,
		) -> aghub_git::Result<()> {
			Ok(())
		}
	}

	#[test]
	fn list_discovers_skill_from_tree_and_frontmatter() {
		let body = b"---\nname: demo\ndescription: a demo skill\n\
			version: 1.0.0\nauthor: acme\n---\n# hi\n";
		let backend = Arc::new(ListFixtureBackend {
			skill_md: body.to_vec(),
			read_tree_calls: AtomicUsize::new(0),
		});
		let repo = SkillRepository::with_backends(
			None,
			backend.clone() as Arc<dyn RepoFetchBackend>,
		);
		let snap = repo
			.resolve(
				&SourceRef {
					source: "https://example.com/o/r.git".into(),
					ref_: Some("main".into()),
				},
				None,
			)
			.unwrap();
		let catalog = repo.list(&snap).unwrap();
		assert_eq!(catalog.skills.len(), 1);
		assert_eq!(catalog.skills[0].skill_path, "skills/demo");
		assert_eq!(catalog.skills[0].name, "demo");
		assert_eq!(
			catalog.skills[0].description.as_deref(),
			Some("a demo skill")
		);
		assert_eq!(catalog.skills[0].version.as_deref(), Some("1.0.0"));
		assert_eq!(catalog.skills[0].author.as_deref(), Some("acme"));
		assert_eq!(catalog.snapshot.commit_oid, snap.commit_oid);
		assert!(
			backend.read_tree_calls.load(Ordering::SeqCst) >= 1,
			"list must read the tree"
		);
	}

	#[test]
	fn skill_folder_from_lock_path_root_and_nested() {
		assert_eq!(
			skill_folder_from_lock_path("SKILL.md").unwrap().as_str(),
			""
		);
		assert_eq!(
			skill_folder_from_lock_path("skills/music/SKILL.md")
				.unwrap()
				.as_str(),
			"skills/music"
		);
	}
}
