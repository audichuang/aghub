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
	compare_known_hashes, sanitize_skill_path, SkillUpdateStatus,
	UncheckableReason,
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
	/// Lock scope (`global` or `project`), used to disambiguate duplicate names.
	pub scope: String,
	/// Upstream coordinate.
	pub source_ref: SourceRef,
	/// npx-form `<dir>/SKILL.md` (root → `SKILL.md`). `None` → `Uncheckable{NoPath}`.
	pub skill_path: Option<String>,
	/// Stored `content_hash`/`computed_hash`. `None`/placeholder → auto-heal.
	pub stored_hash: Option<String>,
	/// Hash of the currently installed local skill folder, computed by the route
	/// before checking upstream. Used as the comparison baseline for legacy locks.
	pub local_hash: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct EntryKey {
	pub name: String,
	pub scope: String,
}

#[derive(Clone, Debug)]
pub struct CheckOutput {
	pub key: EntryKey,
	pub status: SkillUpdateStatus,
	/// Local hash that should be written back into the lock before returning.
	pub heal_hash: Option<String>,
}

/// Group lock entries by [`SourceRef`] so each upstream is fetched once.
///
/// Returns a map from coordinate to the list of skill names sharing it.
///
/// The production [`check_updates`] path groups inline (it needs the full
/// [`EntryInput`], not just names); this name-only helper exists for the
/// grouping unit test, hence the `#[cfg(test)]` gate.
#[cfg(test)]
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
	map: HashMap<SourceRef, (Instant, CachedGroup)>,
}

#[derive(Clone, Debug)]
pub(crate) enum CachedGroup {
	Terminal(SkillUpdateStatus),
	Hashes(HashMap<Option<String>, HashProbe>),
}

#[derive(Clone, Debug)]
pub(crate) enum HashProbe {
	Fresh(String),
	Uncheckable(UncheckableReason),
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
	pub(crate) fn get(
		&self,
		k: &SourceRef,
		now: Instant,
	) -> Option<CachedGroup> {
		self.map.get(k).and_then(|(t, v)| {
			if now.duration_since(*t) <= self.ttl {
				Some(v.clone())
			} else {
				None
			}
		})
	}

	/// Insert/replace the cached status for `k`, stamped at `now`.
	fn put(&mut self, k: SourceRef, v: CachedGroup, now: Instant) {
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
///
/// The variants are payload-free: any underlying message is redacted of URL
/// userinfo by `aghub_git` and then discarded at the boundary, so a token can
/// never leak through the error string into the response.
#[derive(Debug)]
pub enum FetchError {
	/// Authentication failure (bad/missing token).
	Auth,
	/// Network / transport failure.
	Network,
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
	pub fetcher: Arc<dyn Fetcher>,
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
pub(crate) fn host_of(source: &str) -> Option<String> {
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
fn probe_skill_hash_in_repo(
	repo_root: &std::path::Path,
	skill_path: Option<&str>,
) -> HashProbe {
	let Some(skill_path) = skill_path else {
		return HashProbe::Uncheckable(UncheckableReason::NoPath);
	};
	// `skill_path` is `<dir>/SKILL.md`; sanitize the file path (rejects abs/`..`
	// and verifies containment), then hash its PARENT folder.
	let Some(skill_file) = sanitize_skill_path(repo_root, skill_path) else {
		return HashProbe::Uncheckable(UncheckableReason::NoPath);
	};
	let folder = skill_file.parent().unwrap_or(repo_root);
	match skill::compute_skill_folder_hash(folder) {
		Ok(hash) => HashProbe::Fresh(hash),
		Err(_) => HashProbe::Uncheckable(UncheckableReason::Local),
	}
}

fn lock_hash_unknown(stored_hash: Option<&str>) -> bool {
	match stored_hash {
		None => true,
		Some("") => true,
		Some(hash) if skill::is_placeholder_digest(hash) => true,
		Some(_) => false,
	}
}

fn classify_member_from_probe(
	member: &EntryInput,
	probe: &HashProbe,
) -> CheckOutput {
	let key = EntryKey {
		name: member.name.clone(),
		scope: member.scope.clone(),
	};
	match probe {
		HashProbe::Uncheckable(reason) => CheckOutput {
			key,
			status: SkillUpdateStatus::Uncheckable {
				reason: reason.clone(),
			},
			heal_hash: None,
		},
		HashProbe::Fresh(fresh_hash) => {
			let unknown = lock_hash_unknown(member.stored_hash.as_deref());
			let baseline = if unknown {
				member.local_hash.as_deref()
			} else {
				member.stored_hash.as_deref()
			};
			let Some(baseline) = baseline else {
				return CheckOutput {
					key,
					status: SkillUpdateStatus::Uncheckable {
						reason: UncheckableReason::Local,
					},
					heal_hash: None,
				};
			};
			let status = compare_known_hashes(baseline, fresh_hash);
			CheckOutput {
				key,
				status,
				heal_hash: unknown.then(|| baseline.to_string()),
			}
		}
	}
}

fn terminal_output(
	member: &EntryInput,
	status: &SkillUpdateStatus,
) -> CheckOutput {
	CheckOutput {
		key: EntryKey {
			name: member.name.clone(),
			scope: member.scope.clone(),
		},
		status: status.clone(),
		heal_hash: lock_hash_unknown(member.stored_hash.as_deref())
			.then(|| member.local_hash.clone())
			.flatten(),
	}
}

fn apply_cached_group(
	members: &[EntryInput],
	cached: &CachedGroup,
) -> Vec<CheckOutput> {
	match cached {
		CachedGroup::Terminal(status) => members
			.iter()
			.map(|member| terminal_output(member, status))
			.collect(),
		CachedGroup::Hashes(hashes) => members
			.iter()
			.map(|member| {
				let probe = hashes.get(&member.skill_path).cloned().unwrap_or(
					HashProbe::Uncheckable(UncheckableReason::NoPath),
				);
				classify_member_from_probe(member, &probe)
			})
			.collect(),
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
) -> Vec<CheckOutput> {
	let mut out: Vec<CheckOutput> = Vec::new();

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
			out.extend(apply_cached_group(&members, &cached));
			continue;
		}

		// 2) Offline → Uncheckable{Network}; do not cache (transient).
		if deps.offline {
			for m in &members {
				out.push(terminal_output(
					m,
					&SkillUpdateStatus::Uncheckable {
						reason: UncheckableReason::Network,
					},
				));
			}
			continue;
		}

		// 3) Pinned SHA → UpToDate without fetching.
		if let Some(r) = &sr.ref_ {
			if is_pinned_sha(r) {
				deps.cache.put(
					sr.clone(),
					CachedGroup::Terminal(SkillUpdateStatus::UpToDate),
					now,
				);
				for m in &members {
					out.push(terminal_output(m, &SkillUpdateStatus::UpToDate));
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
				do_fetch(Arc::clone(&deps.fetcher), sr.clone(), token),
			)
			.await;
			match fetch_res {
				Err(_elapsed) => Err(UncheckableReason::Timeout),
				Ok(Err(FetchError::Auth)) => Err(UncheckableReason::Auth),
				Ok(Err(FetchError::Network)) => Err(UncheckableReason::Network),
				Ok(Ok(repo)) => Ok(repo),
			}
		};

		match fetched {
			Err(reason) => {
				let status = SkillUpdateStatus::Uncheckable { reason };
				deps.cache.put(
					sr.clone(),
					CachedGroup::Terminal(status.clone()),
					now,
				);
				for m in &members {
					out.push(terminal_output(m, &status));
				}
			}
			Ok(repo) => {
				let mut hashes: HashMap<Option<String>, HashProbe> =
					HashMap::new();
				for m in &members {
					hashes.entry(m.skill_path.clone()).or_insert_with(|| {
						probe_skill_hash_in_repo(
							&repo.root,
							m.skill_path.as_deref(),
						)
					});
				}
				let cached = CachedGroup::Hashes(hashes);
				out.extend(apply_cached_group(&members, &cached));
				deps.cache.put(sr.clone(), cached, now);
			}
		}
	}

	out
}

/// Bridge the synchronous [`Fetcher`] into the async timeout path.
async fn do_fetch(
	fetcher: Arc<dyn Fetcher>,
	sr: SourceRef,
	token: Option<String>,
) -> Result<FetchedRepo, FetchError> {
	tokio::task::spawn_blocking(move || fetcher.fetch(&sr, token.as_deref()))
		.await
		.unwrap_or(Err(FetchError::Network))
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
		c.put(
			k.clone(),
			CachedGroup::Terminal(SkillUpdateStatus::UpToDate),
			t0,
		);
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
					"auth" => FetchError::Auth,
					_ => FetchError::Network,
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
			scope: "global".into(),
			source_ref: SourceRef {
				source: src.into(),
				ref_: r.map(Into::into),
			},
			skill_path: Some("SKILL.md".into()),
			stored_hash: None,
			local_hash: None,
		}
	}

	#[tokio::test]
	async fn offline_short_circuits_to_network() {
		let fetcher = Arc::new(StubFetcher {
			root: None,
			err: None,
			calls: Mutex::new(0),
		});
		let resolver = StubResolver(None);
		let mut cache = ResultCache::new(Duration::from_secs(300));
		let deps = CheckDeps {
			fetcher: fetcher.clone(),
			resolver: &resolver,
			cache: &mut cache,
			per_fetch: Duration::from_secs(5),
			concurrency: 4,
			offline: true,
		};
		let out =
			check_updates(vec![entry("a", "o/r", Some("main"))], deps).await;
		assert_eq!(out.len(), 1);
		assert_eq!(out[0].key.name, "a");
		assert_eq!(
			out[0].status,
			SkillUpdateStatus::Uncheckable {
				reason: UncheckableReason::Network
			}
		);
		assert_eq!(*fetcher.calls.lock().unwrap(), 0, "offline must not fetch");
	}

	#[tokio::test]
	async fn pinned_sha_is_up_to_date_without_fetch() {
		let fetcher = Arc::new(StubFetcher {
			root: None,
			err: None,
			calls: Mutex::new(0),
		});
		let resolver = StubResolver(None);
		let mut cache = ResultCache::new(Duration::from_secs(300));
		let deps = CheckDeps {
			fetcher: fetcher.clone(),
			resolver: &resolver,
			cache: &mut cache,
			per_fetch: Duration::from_secs(5),
			concurrency: 4,
			offline: false,
		};
		let sha = "0123456789abcdef0123456789abcdef01234567";
		let out = check_updates(vec![entry("a", "o/r", Some(sha))], deps).await;
		assert_eq!(out.len(), 1);
		assert_eq!(out[0].status, SkillUpdateStatus::UpToDate);
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
			fetcher: Arc::new(fetcher),
			resolver: &resolver,
			cache: &mut cache,
			per_fetch: Duration::from_secs(5),
			concurrency: 4,
			offline: false,
		};
		let out =
			check_updates(vec![entry("a", "o/r", Some("main"))], deps).await;
		assert_eq!(out.len(), 1);
		assert_eq!(
			out[0].status,
			SkillUpdateStatus::Uncheckable {
				reason: UncheckableReason::Auth
			}
		);
	}

	#[tokio::test]
	async fn single_fetch_serves_grouped_members_and_caches() {
		let dir = tempfile::tempdir().unwrap();
		std::fs::write(dir.path().join("SKILL.md"), b"x").unwrap();
		let hash = skill::compute_skill_folder_hash(dir.path()).unwrap();
		let fetcher = StubFetcher {
			root: Some(dir.path().to_path_buf()),
			err: None,
			calls: Mutex::new(0),
		};
		let resolver = StubResolver(None);
		let mut cache = ResultCache::new(Duration::from_secs(300));
		let deps = CheckDeps {
			fetcher: Arc::new(fetcher),
			resolver: &resolver,
			cache: &mut cache,
			per_fetch: Duration::from_secs(5),
			concurrency: 4,
			offline: false,
		};
		let mut a = entry("a", "o/r", Some("main"));
		a.local_hash = Some(hash.clone());
		let mut b = entry("b", "o/r", Some("main"));
		b.local_hash = Some(hash.clone());
		let out = check_updates(vec![a, b], deps).await;
		// Both members resolve; legacy locks heal from their local baseline.
		assert_eq!(out.len(), 2);
		assert!(out.iter().all(|o| o.status == SkillUpdateStatus::UpToDate));
		assert!(out.iter().all(|o| o.heal_hash == Some(hash.clone())));
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
			fetcher: Arc::new(fetcher),
			resolver: &resolver,
			cache: &mut cache,
			per_fetch: Duration::from_secs(5),
			concurrency: 4,
			offline: false,
		};
		let mut e = entry("a", "o/r", Some("main"));
		e.skill_path = None;
		let out = check_updates(vec![e], deps).await;
		assert_eq!(out.len(), 1);
		assert_eq!(
			out[0].status,
			SkillUpdateStatus::Uncheckable {
				reason: UncheckableReason::NoPath
			}
		);
	}

	#[tokio::test]
	async fn legacy_hash_uses_local_baseline_before_upstream_compare() {
		let upstream = tempfile::tempdir().unwrap();
		std::fs::write(upstream.path().join("SKILL.md"), b"new").unwrap();
		let local = tempfile::tempdir().unwrap();
		std::fs::write(local.path().join("SKILL.md"), b"old").unwrap();
		let local_hash =
			skill::compute_skill_folder_hash(local.path()).unwrap();
		let upstream_hash =
			skill::compute_skill_folder_hash(upstream.path()).unwrap();
		let fetcher = StubFetcher {
			root: Some(upstream.path().to_path_buf()),
			err: None,
			calls: Mutex::new(0),
		};
		let resolver = StubResolver(None);
		let mut cache = ResultCache::new(Duration::from_secs(300));
		let deps = CheckDeps {
			fetcher: Arc::new(fetcher),
			resolver: &resolver,
			cache: &mut cache,
			per_fetch: Duration::from_secs(5),
			concurrency: 4,
			offline: false,
		};
		let mut e = entry("a", "o/r", Some("main"));
		e.local_hash = Some(local_hash.clone());
		let out = check_updates(vec![e], deps).await;
		assert_eq!(out.len(), 1);
		assert_eq!(out[0].heal_hash, Some(local_hash.clone()));
		assert_eq!(
			out[0].status,
			SkillUpdateStatus::UpdateAvailable {
				current: local_hash,
				available: upstream_hash,
			}
		);
	}

	#[tokio::test]
	async fn cache_keeps_member_hashes_separate_in_same_repo_group() {
		let repo = tempfile::tempdir().unwrap();
		std::fs::create_dir_all(repo.path().join("a")).unwrap();
		std::fs::create_dir_all(repo.path().join("b")).unwrap();
		std::fs::write(repo.path().join("a/SKILL.md"), b"a").unwrap();
		std::fs::write(repo.path().join("b/SKILL.md"), b"b").unwrap();
		let hash_a =
			skill::compute_skill_folder_hash(&repo.path().join("a")).unwrap();
		let hash_b =
			skill::compute_skill_folder_hash(&repo.path().join("b")).unwrap();
		let fetcher = StubFetcher {
			root: Some(repo.path().to_path_buf()),
			err: None,
			calls: Mutex::new(0),
		};
		let resolver = StubResolver(None);
		let mut cache = ResultCache::new(Duration::from_secs(300));
		let deps = CheckDeps {
			fetcher: Arc::new(fetcher),
			resolver: &resolver,
			cache: &mut cache,
			per_fetch: Duration::from_secs(5),
			concurrency: 4,
			offline: false,
		};
		let mut a = entry("a", "o/r", Some("main"));
		a.skill_path = Some("a/SKILL.md".into());
		a.stored_hash = Some(hash_a);
		let mut b = entry("b", "o/r", Some("main"));
		b.skill_path = Some("b/SKILL.md".into());
		b.stored_hash = Some("old".into());
		let out = check_updates(vec![a, b], deps).await;
		let by_name: HashMap<_, _> =
			out.into_iter().map(|o| (o.key.name.clone(), o)).collect();
		assert_eq!(by_name["a"].status, SkillUpdateStatus::UpToDate);
		assert_eq!(
			by_name["b"].status,
			SkillUpdateStatus::UpdateAvailable {
				current: "old".into(),
				available: hash_b,
			}
		);
	}
}
