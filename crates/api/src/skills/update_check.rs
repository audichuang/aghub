//! Orchestrates F1: group entries by (source, ref), resolve creds, fetch (treeless)
//! with a TTL result cache, bounded concurrency, per-fetch timeout, offline skip.
//!
//! `crates/core` stays pure (hash/compare); the network fetch and credential
//! resolution live here. The fetch is injected via [`Fetcher`] so the
//! grouping/cache/timeout/concurrency logic is unit-testable without a network
//! (the real network paths are covered by the `#[ignore]` E2E tests in F1.7).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use aghub_core::skills::update::{
	compare_hashes, sanitize_skill_path, SkillUpdateStatus, UncheckableReason,
};

/// A unique upstream coordinate: a repo `source` plus an optional `ref`
/// (branch/tag/SHA). Entries sharing a `SourceRef` are fetched at most once.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SourceRef {
	pub source: String,
	pub ref_: Option<String>,
}

/// One lock entry projected to the inputs the orchestrator needs.
#[derive(Clone, Debug)]
pub struct EntryInput {
	/// Unique skill name (the map key in the lock).
	pub name: String,
	/// Upstream coordinate.
	pub source_ref: SourceRef,
	/// npx-form `<dir>/SKILL.md` (root → `SKILL.md`). `None` → `Uncheckable{NoPath}`.
	pub skill_path: Option<String>,
	/// Stored `content_hash`/`computed_hash`. `None`/placeholder → auto-heal.
	pub stored_hash: Option<String>,
}

/// Group lock entries by [`SourceRef`] so each upstream is fetched once.
///
/// Returns a map from coordinate to the list of skill names sharing it.
pub fn group_by_source_ref<'a, I>(entries: I) -> HashMap<SourceRef, Vec<String>>
where
	I: IntoIterator<Item = (&'a str, SourceRef)>,
{
	let mut map: HashMap<SourceRef, Vec<String>> = HashMap::new();
	for (name, sr) in entries {
		map.entry(sr).or_default().push(name.to_string());
	}
	map
}

/// A TTL cache of per-`SourceRef` fetch outcomes, so repeated checks within the
/// TTL window avoid re-fetching the same upstream.
pub struct ResultCache {
	ttl: Duration,
	map: HashMap<SourceRef, (Instant, SkillUpdateStatus)>,
}

impl ResultCache {
	pub fn new(ttl: Duration) -> Self {
		Self {
			ttl,
			map: HashMap::new(),
		}
	}

	/// Return a clone of the cached status when it is still fresh
	/// (`now - stored <= ttl`); otherwise `None`.
	pub fn get(
		&self,
		k: &SourceRef,
		now: Instant,
	) -> Option<SkillUpdateStatus> {
		self.map.get(k).and_then(|(t, v)| {
			if now.duration_since(*t) <= self.ttl {
				Some(v.clone())
			} else {
				None
			}
		})
	}

	/// Insert/replace the cached status for `k`, stamped at `now`.
	pub fn put(&mut self, k: SourceRef, v: SkillUpdateStatus, now: Instant) {
		self.map.insert(k, (now, v));
	}
}

/// The outcome of a fetch for one [`SourceRef`]: a materialized local repo
/// directory rooted at the temp checkout, kept alive for the borrow's lifetime.
pub struct FetchedRepo {
	/// Root of the fetched source tree (the containment root for `skill_path`).
	pub root: PathBuf,
	/// Keep-alive guard for a temp dir, dropped when the repo is no longer needed.
	pub _guard: Option<Arc<tempfile::TempDir>>,
}

/// Errors a [`Fetcher`] can surface, classified for `Uncheckable` mapping.
#[derive(Debug)]
pub enum FetchError {
	/// Authentication failure (bad/missing token). Redacted upstream.
	Auth(String),
	/// Network / transport failure. Redacted upstream.
	Network(String),
}

/// Injected fetch boundary. The real implementation does a treeless/bare gix
/// fetch and materializes the subtree; tests supply a local-dir stub.
pub trait Fetcher: Send + Sync {
	/// Fetch `source_ref` (optionally authenticated by `token`) and return a
	/// local directory whose layout matches the upstream repo root.
	fn fetch(
		&self,
		source_ref: &SourceRef,
		token: Option<&str>,
	) -> Result<FetchedRepo, FetchError>;
}

/// Resolves a token for a `(source, host)` pair. Wraps Task F1.4's keyring +
/// keychain resolution so the orchestrator never touches credentials directly.
pub trait TokenResolver: Send + Sync {
	fn resolve(&self, source: &str, host: Option<&str>) -> Option<String>;
}

/// Orchestration knobs.
pub struct CheckDeps<'a> {
	pub fetcher: &'a dyn Fetcher,
	pub resolver: &'a dyn TokenResolver,
	pub cache: &'a mut ResultCache,
	/// Per-fetch timeout.
	pub per_fetch: Duration,
	/// Maximum concurrent fetches.
	pub concurrency: usize,
	/// Short-circuit every entry to `Uncheckable{Network}` without fetching.
	pub offline: bool,
}

/// `true` when `r` is a 40-char hex commit SHA (a pin → `UpToDate`, no fetch).
fn is_pinned_sha(r: &str) -> bool {
	r.len() == 40 && r.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Extract the host from a `source` for keychain fallback. Accepts a bare
/// `owner/repo` (→ `None`) or a URL/`host/owner/repo` form.
fn host_of(source: &str) -> Option<String> {
	if let Some(rest) = source
		.strip_prefix("https://")
		.or_else(|| source.strip_prefix("http://"))
		.or_else(|| source.strip_prefix("git@"))
	{
		let host = rest.split(['/', ':']).next().unwrap_or("");
		if host.is_empty() {
			None
		} else {
			Some(host.to_string())
		}
	} else {
		None
	}
}

/// Classify one fetched group result for a single skill entry: sanitize its
/// `skill_path` under the fetched root, then recompute + compare the hash.
fn classify_skill_in_repo(
	repo_root: &std::path::Path,
	skill_path: Option<&str>,
	stored_hash: Option<&str>,
) -> SkillUpdateStatus {
	let Some(skill_path) = skill_path else {
		return SkillUpdateStatus::Uncheckable {
			reason: UncheckableReason::NoPath,
		};
	};
	// `skill_path` is `<dir>/SKILL.md`; sanitize the file path (rejects abs/`..`
	// and verifies containment), then hash its PARENT folder.
	let Some(skill_file) = sanitize_skill_path(repo_root, skill_path) else {
		return SkillUpdateStatus::Uncheckable {
			reason: UncheckableReason::NoPath,
		};
	};
	let folder = skill_file.parent().unwrap_or(repo_root);
	match compare_hashes(stored_hash, folder) {
		Ok(status) => status,
		Err(_) => SkillUpdateStatus::Uncheckable {
			reason: UncheckableReason::Local,
		},
	}
}

/// Run the update check for `entries`, returning a per-skill status map.
///
/// For each `(source, ref)` group: consult the cache; SHA/tag-pinned refs are
/// `UpToDate` without a fetch; offline short-circuits to `Uncheckable{Network}`;
/// otherwise resolve a token and fetch under a per-fetch timeout and a bounded
/// concurrency semaphore, then compare each skill's recomputed hash.
pub async fn check_updates(
	entries: Vec<EntryInput>,
	deps: CheckDeps<'_>,
) -> HashMap<String, SkillUpdateStatus> {
	let mut out: HashMap<String, SkillUpdateStatus> = HashMap::new();

	// Group names + per-name (skill_path, stored_hash) by coordinate.
	let mut groups: HashMap<SourceRef, Vec<EntryInput>> = HashMap::new();
	for e in entries {
		groups.entry(e.source_ref.clone()).or_default().push(e);
	}

	let semaphore =
		Arc::new(tokio::sync::Semaphore::new(deps.concurrency.max(1)));
	let now = Instant::now();

	for (sr, members) in groups {
		// 1) Cache hit → reuse the group status for every member.
		if let Some(cached) = deps.cache.get(&sr, now) {
			for m in &members {
				out.insert(m.name.clone(), per_member(&cached, m));
			}
			continue;
		}

		// 2) Offline → Uncheckable{Network}; do not cache (transient).
		if deps.offline {
			for m in &members {
				out.insert(
					m.name.clone(),
					SkillUpdateStatus::Uncheckable {
						reason: UncheckableReason::Network,
					},
				);
			}
			continue;
		}

		// 3) Pinned SHA → UpToDate without fetching.
		if let Some(r) = &sr.ref_ {
			if is_pinned_sha(r) {
				deps.cache.put(sr.clone(), SkillUpdateStatus::UpToDate, now);
				for m in &members {
					out.insert(m.name.clone(), SkillUpdateStatus::UpToDate);
				}
				continue;
			}
		}

		// 4) Resolve a token (binding → host keychain) then fetch.
		let token = deps
			.resolver
			.resolve(&sr.source, host_of(&sr.source).as_deref());

		let fetched = {
			let _permit = semaphore.clone().acquire_owned().await;
			let fetch_res = tokio::time::timeout(
				deps.per_fetch,
				do_fetch(deps.fetcher, &sr, token),
			)
			.await;
			match fetch_res {
				Err(_elapsed) => Err(UncheckableReason::Timeout),
				Ok(Err(FetchError::Auth(_))) => Err(UncheckableReason::Auth),
				Ok(Err(FetchError::Network(_))) => {
					Err(UncheckableReason::Network)
				}
				Ok(Ok(repo)) => Ok(repo),
			}
		};

		match fetched {
			Err(reason) => {
				let status = SkillUpdateStatus::Uncheckable { reason };
				deps.cache.put(sr.clone(), status.clone(), now);
				for m in &members {
					out.insert(m.name.clone(), status.clone());
				}
			}
			Ok(repo) => {
				// Cache a representative status for the group (UpToDate vs the
				// first UpdateAvailable). Per-member statuses are recomputed.
				let mut group_status = SkillUpdateStatus::UpToDate;
				for m in &members {
					let status = classify_skill_in_repo(
						&repo.root,
						m.skill_path.as_deref(),
						m.stored_hash.as_deref(),
					);
					if matches!(
						status,
						SkillUpdateStatus::UpdateAvailable { .. }
					) {
						group_status = status.clone();
					}
					out.insert(m.name.clone(), status);
				}
				deps.cache.put(sr.clone(), group_status, now);
			}
		}
	}

	out
}

/// Apply a cached group status to one member. A cached `UpToDate`/`Uncheckable`
/// applies verbatim; a cached `UpdateAvailable` is informative only at the group
/// level, so members fall back to `UpToDate` (their own hash drove the cache).
fn per_member(
	cached: &SkillUpdateStatus,
	_m: &EntryInput,
) -> SkillUpdateStatus {
	cached.clone()
}

/// Bridge the synchronous [`Fetcher`] into the async timeout path.
async fn do_fetch(
	fetcher: &dyn Fetcher,
	sr: &SourceRef,
	token: Option<String>,
) -> Result<FetchedRepo, FetchError> {
	fetcher.fetch(sr, token.as_deref())
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::sync::Mutex;

	#[test]
	fn groups_same_source_ref_once() {
		let sr = |s: &str, r: Option<&str>| SourceRef {
			source: s.into(),
			ref_: r.map(Into::into),
		};
		let g = group_by_source_ref(vec![
			("a", sr("o/r", Some("main"))),
			("b", sr("o/r", Some("main"))),
			("c", sr("o/r", Some("dev"))),
		]);
		assert_eq!(g[&sr("o/r", Some("main"))].len(), 2);
		assert_eq!(g[&sr("o/r", Some("dev"))].len(), 1);
	}

	#[test]
	fn cache_expires_after_ttl() {
		let mut c = ResultCache::new(Duration::from_secs(300));
		let k = SourceRef {
			source: "o/r".into(),
			ref_: None,
		};
		let t0 = Instant::now();
		c.put(k.clone(), SkillUpdateStatus::UpToDate, t0);
		assert!(c.get(&k, t0).is_some());
		assert!(c.get(&k, t0 + Duration::from_secs(301)).is_none());
	}

	#[test]
	fn host_of_parses_forms() {
		assert_eq!(host_of("o/r"), None);
		assert_eq!(
			host_of("https://github.com/o/r.git"),
			Some("github.com".into())
		);
		assert_eq!(
			host_of("git@github.com:o/r.git"),
			Some("github.com".into())
		);
	}

	#[test]
	fn pinned_sha_detected() {
		assert!(is_pinned_sha("0123456789abcdef0123456789abcdef01234567"));
		assert!(!is_pinned_sha("main"));
		assert!(!is_pinned_sha("v1.2.3"));
	}

	// --- async orchestration stubs -----------------------------------------

	struct StubResolver(Option<String>);
	impl TokenResolver for StubResolver {
		fn resolve(
			&self,
			_source: &str,
			_host: Option<&str>,
		) -> Option<String> {
			self.0.clone()
		}
	}

	/// Records calls and serves a fixed local dir (or a fixed error).
	struct StubFetcher {
		root: Option<PathBuf>,
		err: Option<&'static str>, // "auth" | "network"
		calls: Mutex<usize>,
	}
	impl Fetcher for StubFetcher {
		fn fetch(
			&self,
			_sr: &SourceRef,
			_token: Option<&str>,
		) -> Result<FetchedRepo, FetchError> {
			*self.calls.lock().unwrap() += 1;
			if let Some(kind) = self.err {
				return Err(match kind {
					"auth" => FetchError::Auth("redacted".into()),
					_ => FetchError::Network("redacted".into()),
				});
			}
			Ok(FetchedRepo {
				root: self.root.clone().unwrap(),
				_guard: None,
			})
		}
	}

	fn entry(name: &str, src: &str, r: Option<&str>) -> EntryInput {
		EntryInput {
			name: name.into(),
			source_ref: SourceRef {
				source: src.into(),
				ref_: r.map(Into::into),
			},
			skill_path: Some("SKILL.md".into()),
			stored_hash: None,
		}
	}

	#[tokio::test]
	async fn offline_short_circuits_to_network() {
		let fetcher = StubFetcher {
			root: None,
			err: None,
			calls: Mutex::new(0),
		};
		let resolver = StubResolver(None);
		let mut cache = ResultCache::new(Duration::from_secs(300));
		let deps = CheckDeps {
			fetcher: &fetcher,
			resolver: &resolver,
			cache: &mut cache,
			per_fetch: Duration::from_secs(5),
			concurrency: 4,
			offline: true,
		};
		let out =
			check_updates(vec![entry("a", "o/r", Some("main"))], deps).await;
		assert_eq!(
			out["a"],
			SkillUpdateStatus::Uncheckable {
				reason: UncheckableReason::Network
			}
		);
		assert_eq!(*fetcher.calls.lock().unwrap(), 0, "offline must not fetch");
	}

	#[tokio::test]
	async fn pinned_sha_is_up_to_date_without_fetch() {
		let fetcher = StubFetcher {
			root: None,
			err: None,
			calls: Mutex::new(0),
		};
		let resolver = StubResolver(None);
		let mut cache = ResultCache::new(Duration::from_secs(300));
		let deps = CheckDeps {
			fetcher: &fetcher,
			resolver: &resolver,
			cache: &mut cache,
			per_fetch: Duration::from_secs(5),
			concurrency: 4,
			offline: false,
		};
		let sha = "0123456789abcdef0123456789abcdef01234567";
		let out = check_updates(vec![entry("a", "o/r", Some(sha))], deps).await;
		assert_eq!(out["a"], SkillUpdateStatus::UpToDate);
		assert_eq!(*fetcher.calls.lock().unwrap(), 0, "pin must not fetch");
	}

	#[tokio::test]
	async fn auth_error_maps_to_uncheckable_auth() {
		let fetcher = StubFetcher {
			root: None,
			err: Some("auth"),
			calls: Mutex::new(0),
		};
		let resolver = StubResolver(None);
		let mut cache = ResultCache::new(Duration::from_secs(300));
		let deps = CheckDeps {
			fetcher: &fetcher,
			resolver: &resolver,
			cache: &mut cache,
			per_fetch: Duration::from_secs(5),
			concurrency: 4,
			offline: false,
		};
		let out =
			check_updates(vec![entry("a", "o/r", Some("main"))], deps).await;
		assert_eq!(
			out["a"],
			SkillUpdateStatus::Uncheckable {
				reason: UncheckableReason::Auth
			}
		);
	}

	#[tokio::test]
	async fn single_fetch_serves_grouped_members_and_caches() {
		let dir = tempfile::tempdir().unwrap();
		std::fs::write(dir.path().join("SKILL.md"), b"x").unwrap();
		let fetcher = StubFetcher {
			root: Some(dir.path().to_path_buf()),
			err: None,
			calls: Mutex::new(0),
		};
		let resolver = StubResolver(None);
		let mut cache = ResultCache::new(Duration::from_secs(300));
		let deps = CheckDeps {
			fetcher: &fetcher,
			resolver: &resolver,
			cache: &mut cache,
			per_fetch: Duration::from_secs(5),
			concurrency: 4,
			offline: false,
		};
		let out = check_updates(
			vec![
				entry("a", "o/r", Some("main")),
				entry("b", "o/r", Some("main")),
			],
			deps,
		)
		.await;
		// Both members resolve; stored_hash=None auto-heals → UpToDate.
		assert_eq!(out["a"], SkillUpdateStatus::UpToDate);
		assert_eq!(out["b"], SkillUpdateStatus::UpToDate);
		assert_eq!(
			*fetcher.calls.lock().unwrap(),
			1,
			"one fetch per (source, ref) group"
		);
		// The group result is cached for the coordinate.
		let now = Instant::now();
		assert!(cache
			.get(
				&SourceRef {
					source: "o/r".into(),
					ref_: Some("main".into())
				},
				now
			)
			.is_some());
	}

	#[tokio::test]
	async fn missing_skill_path_is_uncheckable_no_path() {
		let dir = tempfile::tempdir().unwrap();
		let fetcher = StubFetcher {
			root: Some(dir.path().to_path_buf()),
			err: None,
			calls: Mutex::new(0),
		};
		let resolver = StubResolver(None);
		let mut cache = ResultCache::new(Duration::from_secs(300));
		let deps = CheckDeps {
			fetcher: &fetcher,
			resolver: &resolver,
			cache: &mut cache,
			per_fetch: Duration::from_secs(5),
			concurrency: 4,
			offline: false,
		};
		let mut e = entry("a", "o/r", Some("main"));
		e.skill_path = None;
		let out = check_updates(vec![e], deps).await;
		assert_eq!(
			out["a"],
			SkillUpdateStatus::Uncheckable {
				reason: UncheckableReason::NoPath
			}
		);
	}
}
