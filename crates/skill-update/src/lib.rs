//! Orchestrates F1: group entries by (source, ref), resolve creds, fetch (treeless)
//! with a TTL result cache, bounded concurrency, per-fetch timeout, offline skip.
//!
//! `crates/core` stays pure (hash/compare); the network fetch and credential
//! resolution live here. The fetch is injected via [`Fetcher`] so the
//! grouping/cache/timeout/concurrency logic is unit-testable without a network
//! (the real network paths are covered by the `#[ignore]` E2E tests in F1.7).
//!
//! Extracted from `crates/api` into its own crate so both the desktop API
//! (`GET /skills/check-updates`) and the CLI (`aghub-cli check --online`) can
//! share one orchestrator. Each surface supplies its own [`TokenResolver`]; the
//! default git adapters ([`GitFetcher`]/[`GitRefResolver`]) live in [`mod@git`].

mod git;
pub use git::{GitFetcher, GitFetcherWithFallback, GitRefResolver};

pub mod sources;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use aghub_core::skills::update::{
	compare_known_hashes, detect_rename, sanitize_skill_path,
	SkillUpdateStatus, UncheckableReason,
};

/// The lock→disk resolver for an installed skill's on-disk roots now lives in
/// `aghub-core` (next to the containment guards it feeds); re-exported so the
/// sources service and the API/CLI update paths share one implementation.
pub use aghub_core::skills::removal::installed_skill_roots;

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
	/// Lock `source_type` (e.g. `github`/`local`). Drives pre-fetch source
	/// classification: `local` → `Uncheckable{Local}` without a fetch.
	pub source_type: String,
	/// npx-form `<dir>/SKILL.md` (root → `SKILL.md`). `None` → `Uncheckable{NoPath}`.
	pub skill_path: Option<String>,
	/// Stored `content_hash`/`computed_hash`. `None`/placeholder → auto-heal.
	pub stored_hash: Option<String>,
	/// Hash of the currently installed local skill folder, computed by the route
	/// before checking upstream. Used as the comparison baseline for legacy locks.
	pub local_hash: Option<String>,
	/// Stored repo-level commit OID (`refCommit`) from the lock, when present.
	/// Drives the ls-refs preflight: an unchanged tip lets the group skip the
	/// fetch. `None` (project lock / npx / legacy) → never a preflight skip.
	pub ref_commit: Option<String>,
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
	/// Resolved tip OID to write back into the GLOBAL lock's `refCommit` after a
	/// fresh fetch, so the next check can preflight. `None` outside global fetches.
	pub heal_oid: Option<String>,
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
	Fresh { hash: String, name: Option<String> },
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
	/// Immutable identity pin of the fetched tip. The lock records
	/// `snapshot.commit_oid` (via [`FetchedRepo::oid`]) — never the tree oid.
	pub snapshot: aghub_git::RepoSnapshot,
	/// Keep-alive guard for a temp dir, dropped when the repo is no longer needed.
	pub _guard: Option<Arc<tempfile::TempDir>>,
}

impl FetchedRepo {
	/// The commit oid to record in the lock's `refCommit`. Always the COMMIT
	/// oid, never the tree oid.
	pub fn oid(&self) -> &str {
		&self.snapshot.commit_oid
	}

	/// Best-effort RFC 3339 author time of the fetched tip commit.
	pub fn upstream_commit_time(&self) -> Option<String> {
		self.snapshot.commit_time.clone()
	}
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

/// Resolves the current tip commit OID of a `(source, ref)` via a git ref
/// advertisement (ls-refs) — no object download. Returns the 40-hex OID. Used
/// for the cheap preflight that skips the full fetch when nothing changed.
pub trait RefResolver: Send + Sync {
	fn resolve(
		&self,
		source_ref: &SourceRef,
		token: Option<&str>,
	) -> Result<String, FetchError>;
}

/// Orchestration knobs.
pub struct CheckDeps<'a> {
	pub fetcher: Arc<dyn Fetcher>,
	/// Optional ls-refs preflight. `None` disables the preflight entirely (the
	/// orchestrator always fetches), preserving the pre-preflight behavior.
	pub ref_resolver: Option<Arc<dyn RefResolver>>,
	pub resolver: &'a dyn TokenResolver,
	pub cache: &'a mut ResultCache,
	/// Per-fetch timeout.
	pub per_fetch: Duration,
	/// Maximum concurrent fetches.
	pub concurrency: usize,
	/// Short-circuit every entry to `Uncheckable{Network}` without fetching.
	pub offline: bool,
	/// Maximum wall-clock time for the whole check orchestration.
	pub overall_deadline: Duration,
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

pub fn keychain_host_for_source(source: &str) -> Option<String> {
	aghub_git::resolve_remote_source(source)
		.ok()
		.and_then(|resolved| host_of(&resolved.clone_url))
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
	let name = skill::parse(&skill_file).ok().map(|skill| skill.name);
	match skill::compute_skill_folder_hash(folder) {
		Ok(hash) => HashProbe::Fresh { hash, name },
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

/// Outcome of the ls-refs preflight for one `(source, ref)` group.
pub(crate) enum PreflightResult {
	/// Every member is provably up to date; reuse these synthesized per-skill
	/// probes and skip the fetch entirely.
	Skip(CachedGroup),
	/// At least one member is not trustworthy; fall through to the full fetch.
	Fetch,
}

/// Decide whether a `(source, ref)` group can skip the fetch given the remote
/// tip OID. Skips ONLY when EVERY member has `ref_commit == Some(tip)`, a known
/// (non-placeholder) `stored_hash`, and `local_hash == stored_hash` (the
/// installed copy has not drifted). Any failure → `Fetch`.
///
/// `Skip` carries a synthesized `CachedGroup::Hashes` (`HashProbe::Fresh(stored)`
/// per `skill_path`) so the normal `classify_member_from_probe` path runs
/// unchanged — yielding `UpToDate` with `heal_hash: None` — instead of a blanket
/// terminal status that would bypass the per-member heal logic.
pub(crate) fn preflight_decision(
	members: &[EntryInput],
	tip_oid: &str,
) -> PreflightResult {
	let trustworthy = |m: &EntryInput| {
		m.ref_commit.as_deref() == Some(tip_oid)
			&& !lock_hash_unknown(m.stored_hash.as_deref())
			&& m.local_hash.is_some()
			&& m.local_hash == m.stored_hash
	};
	if !members.iter().all(trustworthy) {
		return PreflightResult::Fetch;
	}
	let mut hashes: HashMap<Option<String>, HashProbe> = HashMap::new();
	for m in members {
		if let Some(stored) = m.stored_hash.clone() {
			hashes
				.entry(m.skill_path.clone())
				.or_insert(HashProbe::Fresh {
					hash: stored,
					name: None,
				});
		}
	}
	PreflightResult::Skip(CachedGroup::Hashes(hashes))
}

fn classify_member_from_probe(
	member: &EntryInput,
	probe: &HashProbe,
	upstream_commit_time: Option<String>,
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
			heal_oid: None,
		},
		HashProbe::Fresh {
			hash: fresh_hash,
			name,
		} => {
			if let Some(parsed_name) = name {
				if let Some(new_name) = detect_rename(parsed_name, &member.name)
				{
					return CheckOutput {
						key,
						status: SkillUpdateStatus::Renamed { new_name },
						heal_hash: None,
						heal_oid: None,
					};
				}
			}
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
					heal_oid: None,
				};
			};
			let status = compare_known_hashes(
				baseline,
				fresh_hash,
				upstream_commit_time,
			);
			CheckOutput {
				key,
				status,
				heal_hash: unknown.then(|| baseline.to_string()),
				heal_oid: None,
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
		heal_hash: None,
		heal_oid: None,
	}
}

fn apply_cached_group(
	members: &[EntryInput],
	cached: &CachedGroup,
	upstream_commit_time: Option<String>,
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
				classify_member_from_probe(
					member,
					&probe,
					upstream_commit_time.clone(),
				)
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
	let mut jobs: Vec<FetchJob> = Vec::new();

	for (sr, members) in groups {
		// 1) Offline → Uncheckable{Network}; do not cache (transient).
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

		// 2) Pinned SHA → UpToDate without fetching.
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

		let mut fetch_members = Vec::new();
		for member in members {
			if let Some(reason) = aghub_core::skills::update::precheck_source(
				&member.source_type,
				&sr.source,
			) {
				out.push(terminal_output(
					&member,
					&SkillUpdateStatus::Uncheckable { reason },
				));
			} else {
				fetch_members.push(member);
			}
		}
		if fetch_members.is_empty() {
			continue;
		}

		// 3) Cache hit → reuse the group status for every fetchable member.
		if let Some(cached) = deps.cache.get(&sr, now) {
			out.extend(apply_cached_group(&fetch_members, &cached, None));
			continue;
		}

		let token = deps.resolver.resolve(
			&sr.source,
			keychain_host_for_source(&sr.source).as_deref(),
		);
		jobs.push(FetchJob {
			sr,
			members: fetch_members,
			token,
		});
	}

	let mut pending: HashMap<usize, (SourceRef, Vec<EntryInput>)> =
		HashMap::new();
	let mut set = tokio::task::JoinSet::new();
	for (id, job) in jobs.into_iter().enumerate() {
		pending.insert(id, (job.sr.clone(), job.members.clone()));
		let fetcher = Arc::clone(&deps.fetcher);
		let ref_resolver = deps.ref_resolver.clone();
		let semaphore = Arc::clone(&semaphore);
		let per_fetch = deps.per_fetch;
		set.spawn(async move {
			let _permit = semaphore.clone().acquire_owned().await.ok();
			// Cheap ls-refs preflight (bounded by the same per-fetch timeout):
			// skip the fetch when the tip is unchanged AND every member is
			// trustworthy. Any resolver error falls through to the full fetch.
			if let Some(rr) = ref_resolver {
				let tip = tokio::time::timeout(
					per_fetch,
					do_resolve(rr, job.sr.clone(), job.token.clone()),
				)
				.await;
				if let Ok(Ok(tip)) = tip {
					if let PreflightResult::Skip(cached) =
						preflight_decision(&job.members, &tip)
					{
						return (
							id,
							job.sr,
							job.members,
							JobResult::Skip(cached),
						);
					}
				}
			}
			let result = tokio::time::timeout(
				per_fetch,
				do_fetch(fetcher, job.sr.clone(), job.token),
			)
			.await;
			let outcome = match result {
				Err(_elapsed) => JobResult::Failed(UncheckableReason::Timeout),
				Ok(Err(FetchError::Auth)) => {
					JobResult::Failed(UncheckableReason::Auth)
				}
				Ok(Err(FetchError::Network)) => {
					JobResult::Failed(UncheckableReason::Network)
				}
				Ok(Ok(repo)) => JobResult::Fetched(repo),
			};
			(id, job.sr, job.members, outcome)
		});
	}

	if tokio::time::timeout(deps.overall_deadline, async {
		while let Some(joined) = set.join_next().await {
			let Ok((id, sr, members, outcome)) = joined else {
				continue;
			};
			pending.remove(&id);
			match outcome {
				JobResult::Skip(cached) => {
					out.extend(apply_cached_group(&members, &cached, None));
					deps.cache.put(sr.clone(), cached, now);
				}
				JobResult::Failed(reason) => {
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
				JobResult::Fetched(repo) => {
					let mut hashes: HashMap<Option<String>, HashProbe> =
						HashMap::new();
					for m in &members {
						hashes.entry(m.skill_path.clone()).or_insert_with(
							|| {
								probe_skill_hash_in_repo(
									&repo.root,
									m.skill_path.as_deref(),
								)
							},
						);
					}
					let cached = CachedGroup::Hashes(hashes);
					let mut group_out = apply_cached_group(
						&members,
						&cached,
						repo.upstream_commit_time(),
					);
					// Self-heal refCommit for freshly-fetched GLOBAL entries so the
					// next check can preflight; the VCS-tracked project lock is
					// never silently mutated by a read-style check.
					for (output, member) in group_out.iter_mut().zip(&members) {
						if member.scope == "global" {
							output.heal_oid = Some(repo.oid().to_string());
						}
					}
					out.extend(group_out);
					deps.cache.put(sr.clone(), cached, now);
				}
			}
		}
	})
	.await
	.is_err()
	{
		set.abort_all();
		for (_id, (_sr, members)) in pending {
			for member in members {
				out.push(terminal_output(
					&member,
					&SkillUpdateStatus::Uncheckable {
						reason: UncheckableReason::Timeout,
					},
				));
			}
		}
	}

	out
}

/// What a spawned per-group job produced: a preflight skip (reuse these probes),
/// a real fetch result, or a classified failure.
enum JobResult {
	Skip(CachedGroup),
	Fetched(FetchedRepo),
	Failed(UncheckableReason),
}

struct FetchJob {
	sr: SourceRef,
	members: Vec<EntryInput>,
	token: Option<String>,
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

/// Bridge the synchronous [`RefResolver`] into the async timeout path.
async fn do_resolve(
	resolver: Arc<dyn RefResolver>,
	sr: SourceRef,
	token: Option<String>,
) -> Result<String, FetchError> {
	tokio::task::spawn_blocking(move || resolver.resolve(&sr, token.as_deref()))
		.await
		.unwrap_or(Err(FetchError::Network))
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::sync::atomic::{AtomicUsize, Ordering};
	use std::sync::Mutex;

	/// Fixed tip OID the test fetchers report, so heal-oid wiring is assertable.
	const STUB_FETCH_OID: &str = "f1f1f1f1f1f1f1f1f1f1f1f1f1f1f1f1f1f1f1f1";
	const STUB_TREE_OID: &str = "e2e2e2e2e2e2e2e2e2e2e2e2e2e2e2e2e2e2e2e2";

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
				snapshot: aghub_git::RepoSnapshot {
					commit_oid: STUB_FETCH_OID.to_string(),
					tree_oid: STUB_TREE_OID.to_string(),
					commit_time: None,
				},
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
			source_type: "github".into(),
			skill_path: Some("SKILL.md".into()),
			stored_hash: None,
			local_hash: None,
			ref_commit: None,
		}
	}

	#[test]
	fn preflight_decision_all_trustworthy_and_tip_match_returns_skip() {
		let tip = "abc123def456abc123def456abc123def456abc1";
		let mut a = entry("a", "o/r", Some("main"));
		a.ref_commit = Some(tip.to_string());
		a.stored_hash = Some("hash_a".to_string());
		a.local_hash = Some("hash_a".to_string());
		a.skill_path = Some("SKILL.md".into());
		match preflight_decision(&[a], tip) {
			PreflightResult::Skip(CachedGroup::Hashes(map)) => {
				assert!(matches!(
					map.get(&Some("SKILL.md".to_string())),
					Some(HashProbe::Fresh { hash, .. }) if hash == "hash_a"
				));
			}
			_ => panic!("expected Skip, got Fetch"),
		}
	}

	fn trustworthy_member(tip: &str) -> EntryInput {
		let mut m = entry("a", "o/r", Some("main"));
		m.ref_commit = Some(tip.to_string());
		m.stored_hash = Some("hash_a".to_string());
		m.local_hash = Some("hash_a".to_string());
		m
	}

	#[test]
	fn preflight_decision_local_drift_returns_fetch() {
		let tip = "abc123def456abc123def456abc123def456abc1";
		let mut m = trustworthy_member(tip);
		m.local_hash = Some("DRIFTED".to_string()); // installed copy edited
		assert!(matches!(
			preflight_decision(&[m], tip),
			PreflightResult::Fetch
		));
	}

	#[test]
	fn preflight_decision_oid_mismatch_returns_fetch() {
		let tip = "abc123def456abc123def456abc123def456abc1";
		let mut m = trustworthy_member(tip);
		m.ref_commit = Some("0000000000000000000000000000000000000000".into());
		assert!(matches!(
			preflight_decision(&[m], tip),
			PreflightResult::Fetch
		));
	}

	#[test]
	fn preflight_decision_legacy_unknown_stored_returns_fetch() {
		let tip = "abc123def456abc123def456abc123def456abc1";
		let mut m = trustworthy_member(tip);
		m.stored_hash = None; // legacy/npx entry: must fall through to heal
		assert!(matches!(
			preflight_decision(&[m], tip),
			PreflightResult::Fetch
		));
	}

	#[test]
	fn preflight_decision_is_all_or_nothing_per_group() {
		let tip = "abc123def456abc123def456abc123def456abc1";
		let good = trustworthy_member(tip);
		let mut bad = trustworthy_member(tip);
		bad.ref_commit = None; // one untrustworthy member fails the whole group
		assert!(matches!(
			preflight_decision(&[good, bad], tip),
			PreflightResult::Fetch
		));
	}

	struct StubRefResolver {
		oid: String,
		err: Option<&'static str>,
		calls: Mutex<usize>,
	}
	impl RefResolver for StubRefResolver {
		fn resolve(
			&self,
			_sr: &SourceRef,
			_token: Option<&str>,
		) -> Result<String, FetchError> {
			*self.calls.lock().unwrap() += 1;
			match self.err {
				Some("auth") => Err(FetchError::Auth),
				Some(_) => Err(FetchError::Network),
				None => Ok(self.oid.clone()),
			}
		}
	}

	#[tokio::test]
	async fn preflight_hit_skips_fetch() {
		let dir = tempfile::tempdir().unwrap();
		std::fs::write(dir.path().join("SKILL.md"), b"x").unwrap();
		let hash = skill::compute_skill_folder_hash(dir.path()).unwrap();
		let tip = "abc123def456abc123def456abc123def456abc1";
		let fetcher = Arc::new(StubFetcher {
			root: Some(dir.path().to_path_buf()),
			err: None,
			calls: Mutex::new(0),
		});
		let ref_resolver: Arc<dyn RefResolver> = Arc::new(StubRefResolver {
			oid: tip.to_string(),
			err: None,
			calls: Mutex::new(0),
		});
		let resolver = StubResolver(None);
		let mut cache = ResultCache::new(Duration::from_secs(300));
		let deps = CheckDeps {
			fetcher: fetcher.clone(),
			ref_resolver: Some(ref_resolver),
			resolver: &resolver,
			cache: &mut cache,
			per_fetch: Duration::from_secs(5),
			concurrency: 4,
			offline: false,
			overall_deadline: Duration::from_secs(30),
		};
		let mut a = entry("a", "o/r", Some("main"));
		a.ref_commit = Some(tip.to_string());
		a.stored_hash = Some(hash.clone());
		a.local_hash = Some(hash);
		let out = check_updates(vec![a], deps).await;
		assert_eq!(out.len(), 1);
		assert_eq!(out[0].status, SkillUpdateStatus::UpToDate);
		assert_eq!(
			*fetcher.calls.lock().unwrap(),
			0,
			"preflight hit must skip the fetch"
		);
	}

	#[tokio::test]
	async fn preflight_miss_drift_falls_through_to_fetch() {
		let dir = tempfile::tempdir().unwrap();
		std::fs::write(dir.path().join("SKILL.md"), b"x").unwrap();
		let tip = "abc123def456abc123def456abc123def456abc1";
		let fetcher = Arc::new(StubFetcher {
			root: Some(dir.path().to_path_buf()),
			err: None,
			calls: Mutex::new(0),
		});
		let ref_resolver: Arc<dyn RefResolver> = Arc::new(StubRefResolver {
			oid: tip.to_string(),
			err: None,
			calls: Mutex::new(0),
		});
		let resolver = StubResolver(None);
		let mut cache = ResultCache::new(Duration::from_secs(300));
		let deps = CheckDeps {
			fetcher: fetcher.clone(),
			ref_resolver: Some(ref_resolver),
			resolver: &resolver,
			cache: &mut cache,
			per_fetch: Duration::from_secs(5),
			concurrency: 4,
			offline: false,
			overall_deadline: Duration::from_secs(30),
		};
		let mut a = entry("a", "o/r", Some("main"));
		a.ref_commit = Some(tip.to_string());
		a.stored_hash = Some("STORED_OLD".to_string());
		a.local_hash = Some("DRIFTED".to_string());
		let out = check_updates(vec![a], deps).await;
		assert_eq!(
			*fetcher.calls.lock().unwrap(),
			1,
			"local drift must fall through to a real fetch"
		);
		assert!(matches!(
			out[0].status,
			SkillUpdateStatus::UpdateAvailable { .. }
		));
	}

	#[tokio::test]
	async fn fetch_heals_ref_commit_for_global_member() {
		let dir = tempfile::tempdir().unwrap();
		std::fs::write(dir.path().join("SKILL.md"), b"x").unwrap();
		let hash = skill::compute_skill_folder_hash(dir.path()).unwrap();
		let fetcher = Arc::new(StubFetcher {
			root: Some(dir.path().to_path_buf()),
			err: None,
			calls: Mutex::new(0),
		});
		let resolver = StubResolver(None);
		let mut cache = ResultCache::new(Duration::from_secs(300));
		let deps = CheckDeps {
			fetcher,
			ref_resolver: None,
			resolver: &resolver,
			cache: &mut cache,
			per_fetch: Duration::from_secs(5),
			concurrency: 4,
			offline: false,
			overall_deadline: Duration::from_secs(30),
		};
		let mut g = entry("g", "o/r", Some("main"));
		g.stored_hash = Some(hash.clone());
		g.local_hash = Some(hash);
		let out = check_updates(vec![g], deps).await;
		assert_eq!(out.len(), 1);
		// A fresh fetch records the resolved tip so the next check can preflight.
		assert_eq!(out[0].heal_oid, Some(STUB_FETCH_OID.to_string()));
	}

	/// Ticket 01: a fetcher whose snapshot carries DISTINCT commit/tree oids —
	/// as a GitHub REST trees fetch would (its root `sha` is a TREE oid).
	struct DistinctOidFetcher {
		root: PathBuf,
		commit_oid: String,
		tree_oid: String,
	}
	impl Fetcher for DistinctOidFetcher {
		fn fetch(
			&self,
			_sr: &SourceRef,
			_token: Option<&str>,
		) -> Result<FetchedRepo, FetchError> {
			Ok(FetchedRepo {
				root: self.root.clone(),
				snapshot: aghub_git::RepoSnapshot {
					commit_oid: self.commit_oid.clone(),
					tree_oid: self.tree_oid.clone(),
					commit_time: None,
				},
				_guard: None,
			})
		}
	}

	/// The value healed into the GLOBAL lock's `refCommit` MUST be the snapshot's
	/// COMMIT oid, never its tree oid. A test that would go green if the tree oid
	/// were recorded instead — the acceptance guard for Decision 8.
	#[tokio::test]
	async fn heal_records_commit_oid_never_tree_oid() {
		let dir = tempfile::tempdir().unwrap();
		std::fs::write(dir.path().join("SKILL.md"), b"x").unwrap();
		let hash = skill::compute_skill_folder_hash(dir.path()).unwrap();
		// Two DIFFERENT 40-hex oids so commit vs tree is observable.
		let commit_oid = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
		let tree_oid = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
		let fetcher = Arc::new(DistinctOidFetcher {
			root: dir.path().to_path_buf(),
			commit_oid: commit_oid.to_string(),
			tree_oid: tree_oid.to_string(),
		});
		let resolver = StubResolver(None);
		let mut cache = ResultCache::new(Duration::from_secs(300));
		let deps = CheckDeps {
			fetcher,
			ref_resolver: None,
			resolver: &resolver,
			cache: &mut cache,
			per_fetch: Duration::from_secs(5),
			concurrency: 4,
			offline: false,
			overall_deadline: Duration::from_secs(30),
		};
		let mut g = entry("g", "o/r", Some("main"));
		g.stored_hash = Some(hash.clone());
		g.local_hash = Some(hash);
		let out = check_updates(vec![g], deps).await;
		assert_eq!(out.len(), 1);
		assert_eq!(
			out[0].heal_oid.as_deref(),
			Some(commit_oid),
			"refCommit heal must record the COMMIT oid"
		);
		assert_ne!(
			out[0].heal_oid.as_deref(),
			Some(tree_oid),
			"the TREE oid must never reach refCommit"
		);
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
			ref_resolver: None,
			fetcher: fetcher.clone(),
			resolver: &resolver,
			cache: &mut cache,
			per_fetch: Duration::from_secs(5),
			concurrency: 4,
			offline: true,
			overall_deadline: Duration::from_secs(30),
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
	async fn offline_check_or_terminal_never_heals() {
		let fetcher = Arc::new(StubFetcher {
			root: None,
			err: None,
			calls: Mutex::new(0),
		});
		let resolver = StubResolver(None);
		let mut cache = ResultCache::new(Duration::from_secs(300));
		let deps = CheckDeps {
			ref_resolver: None,
			fetcher,
			resolver: &resolver,
			cache: &mut cache,
			per_fetch: Duration::from_secs(5),
			concurrency: 4,
			offline: true,
			overall_deadline: Duration::from_secs(30),
		};
		let mut input = entry("a", "o/r", Some("main"));
		input.local_hash = Some("abc".to_string());

		let out = check_updates(vec![input], deps).await;

		assert_eq!(out[0].heal_hash, None);
	}

	#[tokio::test]
	async fn local_source_type_is_uncheckable_without_fetch() {
		let fetcher = Arc::new(StubFetcher {
			root: None,
			err: None,
			calls: Mutex::new(0),
		});
		let resolver = StubResolver(None);
		let mut cache = ResultCache::new(Duration::from_secs(300));
		let deps = CheckDeps {
			ref_resolver: None,
			fetcher: fetcher.clone(),
			resolver: &resolver,
			cache: &mut cache,
			per_fetch: Duration::from_secs(5),
			concurrency: 4,
			offline: false,
			overall_deadline: Duration::from_secs(30),
		};
		let mut e = entry("local-skill", "/home/u/local-skill", None);
		e.source_type = "local".into();
		let out = check_updates(vec![e], deps).await;
		assert_eq!(out.len(), 1);
		assert_eq!(
			out[0].status,
			SkillUpdateStatus::Uncheckable {
				reason: UncheckableReason::Local
			}
		);
		assert_eq!(
			*fetcher.calls.lock().unwrap(),
			0,
			"local source must not fetch"
		);
	}

	#[tokio::test]
	async fn ssh_source_is_uncheckable_without_fetch() {
		let fetcher = Arc::new(StubFetcher {
			root: None,
			err: None,
			calls: Mutex::new(0),
		});
		let resolver = StubResolver(None);
		let mut cache = ResultCache::new(Duration::from_secs(300));
		let deps = CheckDeps {
			ref_resolver: None,
			fetcher: fetcher.clone(),
			resolver: &resolver,
			cache: &mut cache,
			per_fetch: Duration::from_secs(5),
			concurrency: 4,
			offline: false,
			overall_deadline: Duration::from_secs(30),
		};
		let out = check_updates(
			vec![entry("s", "git@github.com:o/r.git", None)],
			deps,
		)
		.await;
		assert_eq!(
			out[0].status,
			SkillUpdateStatus::Uncheckable {
				reason: UncheckableReason::Ssh
			}
		);
		assert_eq!(
			*fetcher.calls.lock().unwrap(),
			0,
			"ssh source must not fetch"
		);
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
			ref_resolver: None,
			fetcher: fetcher.clone(),
			resolver: &resolver,
			cache: &mut cache,
			per_fetch: Duration::from_secs(5),
			concurrency: 4,
			offline: false,
			overall_deadline: Duration::from_secs(30),
		};
		let sha = "0123456789abcdef0123456789abcdef01234567";
		let out = check_updates(vec![entry("a", "o/r", Some(sha))], deps).await;
		assert_eq!(out.len(), 1);
		assert_eq!(out[0].status, SkillUpdateStatus::UpToDate);
		assert_eq!(out[0].heal_hash, None);
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
			ref_resolver: None,
			fetcher: Arc::new(fetcher),
			resolver: &resolver,
			cache: &mut cache,
			per_fetch: Duration::from_secs(5),
			concurrency: 4,
			offline: false,
			overall_deadline: Duration::from_secs(30),
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
			ref_resolver: None,
			fetcher: Arc::new(fetcher),
			resolver: &resolver,
			cache: &mut cache,
			per_fetch: Duration::from_secs(5),
			concurrency: 4,
			offline: false,
			overall_deadline: Duration::from_secs(30),
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
			ref_resolver: None,
			fetcher: Arc::new(fetcher),
			resolver: &resolver,
			cache: &mut cache,
			per_fetch: Duration::from_secs(5),
			concurrency: 4,
			offline: false,
			overall_deadline: Duration::from_secs(30),
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
			ref_resolver: None,
			fetcher: Arc::new(fetcher),
			resolver: &resolver,
			cache: &mut cache,
			per_fetch: Duration::from_secs(5),
			concurrency: 4,
			offline: false,
			overall_deadline: Duration::from_secs(30),
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
				upstream_commit_time: None,
			}
		);
	}

	#[tokio::test]
	async fn upstream_skill_name_change_reports_renamed() {
		let upstream = tempfile::tempdir().unwrap();
		std::fs::write(
			upstream.path().join("SKILL.md"),
			b"---\nname: new-skill\ndescription: renamed\n---\nbody\n",
		)
		.unwrap();
		let fetcher = StubFetcher {
			root: Some(upstream.path().to_path_buf()),
			err: None,
			calls: Mutex::new(0),
		};
		let resolver = StubResolver(None);
		let mut cache = ResultCache::new(Duration::from_secs(300));
		let deps = CheckDeps {
			fetcher: Arc::new(fetcher),
			ref_resolver: None,
			resolver: &resolver,
			cache: &mut cache,
			per_fetch: Duration::from_secs(5),
			concurrency: 4,
			offline: false,
			overall_deadline: Duration::from_secs(30),
		};

		let out =
			check_updates(vec![entry("old-skill", "o/r", Some("main"))], deps)
				.await;

		assert_eq!(out.len(), 1);
		assert_eq!(
			out[0].status,
			SkillUpdateStatus::Renamed {
				new_name: "new-skill".to_string()
			}
		);
		assert_eq!(out[0].heal_hash, None);
	}

	/// `HashProbe::Fresh` carries the parsed `name` inside the value (not in
	/// the cache key, which is `SourceRef`), so a second `check_updates` call
	/// within the TTL must reuse the cached probe and keep reporting the
	/// rename without re-fetching.
	#[tokio::test]
	async fn cache_hit_preserves_rename_across_repeated_checks() {
		let upstream = tempfile::tempdir().unwrap();
		std::fs::write(
			upstream.path().join("SKILL.md"),
			b"---\nname: renamed-upstream\ndescription: d\n---\nbody\n",
		)
		.unwrap();
		let fetcher = Arc::new(StubFetcher {
			root: Some(upstream.path().to_path_buf()),
			err: None,
			calls: Mutex::new(0),
		});
		let resolver = StubResolver(None);
		let mut cache = ResultCache::new(Duration::from_secs(300));

		// First call: populates the cache, must report `Renamed`.
		let deps = CheckDeps {
			fetcher: fetcher.clone(),
			ref_resolver: None,
			resolver: &resolver,
			cache: &mut cache,
			per_fetch: Duration::from_secs(5),
			concurrency: 4,
			offline: false,
			overall_deadline: Duration::from_secs(30),
		};
		let first =
			check_updates(vec![entry("old-skill", "o/r", Some("main"))], deps)
				.await;
		assert_eq!(
			first[0].status,
			SkillUpdateStatus::Renamed {
				new_name: "renamed-upstream".to_string()
			}
		);
		let calls_after_first = *fetcher.calls.lock().unwrap();
		assert_eq!(calls_after_first, 1);

		// Second call: must hit the cache (no additional fetch) and still
		// report the same rename — the stale name on the lock entry is the
		// authoritative trigger, the cache just reuses the upstream probe.
		let deps = CheckDeps {
			fetcher: fetcher.clone(),
			ref_resolver: None,
			resolver: &resolver,
			cache: &mut cache,
			per_fetch: Duration::from_secs(5),
			concurrency: 4,
			offline: false,
			overall_deadline: Duration::from_secs(30),
		};
		let second =
			check_updates(vec![entry("old-skill", "o/r", Some("main"))], deps)
				.await;
		assert_eq!(
			second[0].status,
			SkillUpdateStatus::Renamed {
				new_name: "renamed-upstream".to_string()
			}
		);
		assert_eq!(
			*fetcher.calls.lock().unwrap(),
			calls_after_first,
			"second call must reuse the cached probe, not re-fetch"
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
			ref_resolver: None,
			fetcher: Arc::new(fetcher),
			resolver: &resolver,
			cache: &mut cache,
			per_fetch: Duration::from_secs(5),
			concurrency: 4,
			offline: false,
			overall_deadline: Duration::from_secs(30),
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
				upstream_commit_time: None,
			}
		);
	}

	#[tokio::test]
	async fn mixed_source_type_group_prechecks_per_member() {
		let dir = tempfile::tempdir().unwrap();
		std::fs::write(dir.path().join("SKILL.md"), b"x").unwrap();
		let hash = skill::compute_skill_folder_hash(dir.path()).unwrap();
		let fetcher = Arc::new(StubFetcher {
			root: Some(dir.path().to_path_buf()),
			err: None,
			calls: Mutex::new(0),
		});
		let resolver = StubResolver(None);
		let mut cache = ResultCache::new(Duration::from_secs(300));
		let deps = CheckDeps {
			ref_resolver: None,
			fetcher: fetcher.clone(),
			resolver: &resolver,
			cache: &mut cache,
			per_fetch: Duration::from_secs(5),
			concurrency: 4,
			offline: false,
			overall_deadline: Duration::from_secs(30),
		};
		let mut local = entry("local", "o/r", Some("main"));
		local.source_type = "local".to_string();
		let mut github = entry("github", "o/r", Some("main"));
		github.local_hash = Some(hash);

		let out = check_updates(vec![local, github], deps).await;
		let by_name: HashMap<_, _> =
			out.into_iter().map(|o| (o.key.name.clone(), o)).collect();

		assert_eq!(
			by_name["local"].status,
			SkillUpdateStatus::Uncheckable {
				reason: UncheckableReason::Local
			}
		);
		assert_eq!(by_name["github"].status, SkillUpdateStatus::UpToDate);
		assert_eq!(*fetcher.calls.lock().unwrap(), 1);
	}

	struct ConcurrentFetcher {
		root: PathBuf,
		current: AtomicUsize,
		max: AtomicUsize,
	}

	impl Fetcher for ConcurrentFetcher {
		fn fetch(
			&self,
			_sr: &SourceRef,
			_token: Option<&str>,
		) -> Result<FetchedRepo, FetchError> {
			let current = self.current.fetch_add(1, Ordering::SeqCst) + 1;
			self.max.fetch_max(current, Ordering::SeqCst);
			std::thread::sleep(Duration::from_millis(100));
			self.current.fetch_sub(1, Ordering::SeqCst);
			Ok(FetchedRepo {
				root: self.root.clone(),
				snapshot: aghub_git::RepoSnapshot {
					commit_oid: STUB_FETCH_OID.to_string(),
					tree_oid: STUB_TREE_OID.to_string(),
					commit_time: None,
				},
				_guard: None,
			})
		}
	}

	#[tokio::test]
	async fn concurrency_runs_fetches_in_parallel() {
		let dir = tempfile::tempdir().unwrap();
		std::fs::write(dir.path().join("SKILL.md"), b"x").unwrap();
		let hash = skill::compute_skill_folder_hash(dir.path()).unwrap();
		let fetcher = Arc::new(ConcurrentFetcher {
			root: dir.path().to_path_buf(),
			current: AtomicUsize::new(0),
			max: AtomicUsize::new(0),
		});
		let resolver = StubResolver(None);
		let mut cache = ResultCache::new(Duration::from_secs(300));
		let deps = CheckDeps {
			ref_resolver: None,
			fetcher: fetcher.clone(),
			resolver: &resolver,
			cache: &mut cache,
			per_fetch: Duration::from_secs(5),
			concurrency: 4,
			offline: false,
			overall_deadline: Duration::from_secs(30),
		};
		let entries = (0..8)
			.map(|i| {
				let mut e =
					entry(&format!("skill-{i}"), &format!("o/r-{i}"), None);
				e.local_hash = Some(hash.clone());
				e
			})
			.collect();

		let out = check_updates(entries, deps).await;

		assert_eq!(out.len(), 8);
		let max = fetcher.max.load(Ordering::SeqCst);
		assert!(max > 1, "fetches should overlap, max={max}");
		assert!(max <= 4, "semaphore should cap concurrency, max={max}");
	}
}
