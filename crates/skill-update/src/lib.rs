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
pub use git::{GitFetcher, GitRefResolver};

pub mod mutation;

mod repository;
pub use repository::{
	skill_folder_from_lock_path, skill_repo_to_fetch_error, CatalogSkill,
	FetchSelection, PinnedSnapshot, SkillCatalog, SkillRepoError,
	SkillRepository,
};

pub mod sources;

/// Tokens are HTTPS-only. Passing one to ssh/scp/git would turn transport auth
/// that could succeed into a guaranteed credential-injection error.
pub(crate) fn https_only_token<'token>(
	url: &str,
	token: Option<&'token str>,
) -> Option<&'token str> {
	token.filter(|_| url.starts_with("https://"))
}

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
	/// Drives the tip preflight: an unchanged tip lets the group skip the
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
#[derive(Debug)]
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
/// `Network` carries the underlying reason so a caller can tell a DNS failure
/// from a 404 from a TLS problem. The payload is redacted of URL userinfo by
/// `aghub_git`'s error constructors, so it cannot leak a token — but it CAN
/// name an internal temp path, so a surface that must not disclose internals
/// (the HTTP API) matches the variant and prints its own generic message
/// instead of forwarding this string.
#[derive(Clone, Debug)]
pub enum FetchError {
	/// Authentication failure (bad/missing token).
	Auth,
	/// Network / transport failure, with the underlying reason.
	Network(String),
	/// Credential backend state could not be determined; no fetch was attempted.
	BackendUnavailable,
}

impl FetchError {
	/// A network failure with no more specific reason available.
	pub fn network(reason: impl Into<String>) -> Self {
		Self::Network(reason.into())
	}

	/// The underlying reason, when there is one.
	pub fn detail(&self) -> Option<&str> {
		match self {
			Self::Network(detail) => Some(detail),
			_ => None,
		}
	}
}

/// Injected fetch boundary. Production materializes only the
/// [`FetchSelection`]; tests supply a local-dir stub (may ignore selection).
pub trait Fetcher: Send + Sync {
	/// Fetch `source_ref` (optionally authenticated by `token`) and materialize
	/// only `selection` into a local directory.
	fn fetch(
		&self,
		source_ref: &SourceRef,
		token: Option<&str>,
		selection: FetchSelection<'_>,
	) -> Result<FetchedRepo, FetchError>;

	/// Fetch the snapshot a preflight already pinned, instead of re-resolving the
	/// coordinate.
	///
	/// Defaulted so only the production adapter has to care: ignoring the claim
	/// is still CORRECT, it just re-resolves (one more request, and a tip that may
	/// have moved since the decision). [`GitFetcher`] overrides it so the fetch
	/// operates on exactly the tip its preflight decided about.
	fn fetch_pinned(
		&self,
		source_ref: &SourceRef,
		token: Option<&str>,
		selection: FetchSelection<'_>,
		_pinned: &PinnedSnapshot,
	) -> Result<FetchedRepo, FetchError> {
		self.fetch(source_ref, token, selection)
	}
}

/// Outcome of a fail-closed-aware token resolution: distinguishes "no
/// credential needed" from "couldn't tell" (backend unreachable) so a caller
/// can fail the whole operation closed on the latter instead of proceeding as
/// if no credential were bound.
#[derive(Debug, PartialEq, Eq)]
pub enum TokenResolution {
	Token(String),
	NoToken,
	BackendUnavailable,
}

/// Resolves a token for a source. Adapters derive any host-specific lookup key
/// from the source so callers cannot supply an inconsistent pair.
pub trait TokenResolver: Send + Sync {
	fn resolve(&self, source: &str) -> TokenResolution;
}

/// What one tip preflight observed.
///
/// `commit_oid` is what the skip/fetch decision is made against. `pinned` is the
/// snapshot that produced it, when the resolver had one — and carrying it as a
/// VALUE to the following fetch is what guarantees the fetch operates on the tip
/// that was decided about. Routing that handoff through any coordinate-keyed
/// cache instead is last-writer-wins, and a slow observation of an older tip
/// overwriting a newer one makes a moved source report `UpToDate`.
#[derive(Clone, Debug)]
pub struct TipObservation {
	pub commit_oid: String,
	pub pinned: Option<PinnedSnapshot>,
}

impl TipObservation {
	/// An observation with no reusable snapshot — a ref advertisement yields an
	/// OID and no tree, so the fetch that follows resolves for itself.
	pub fn tip_only(commit_oid: impl Into<String>) -> Self {
		Self {
			commit_oid: commit_oid.into(),
			pinned: None,
		}
	}
}

/// Resolves the current tip commit OID of a `(source, ref)` **without
/// downloading objects**. Used for the preflight that skips the full fetch when
/// nothing changed — so an implementation that reads the tip by fetching (git's
/// own resolve-by-fetch) defeats the entire point. The production one is
/// [`GitRefResolver`]; build it from the fetcher.
pub trait RefResolver: Send + Sync {
	fn resolve(
		&self,
		source_ref: &SourceRef,
		token: Option<&str>,
	) -> Result<TipObservation, FetchError>;
}

/// Orchestration knobs.
pub struct CheckDeps<'a> {
	pub fetcher: Arc<dyn Fetcher>,
	/// Optional tip preflight. `None` disables the preflight entirely (the
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

/// Outcome of the tip preflight for one `(source, ref)` group.
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
/// The part of [`preflight_decision`] that needs NO network: whether any tip
/// value at all could produce a `Skip`.
///
/// A member with no `ref_commit`, an unknown `stored_hash`, or a drifted local
/// copy forces `Fetch` whatever the remote says — so asking the remote first is
/// a round trip whose answer cannot change the outcome. That used to be free
/// (a git ref advertisement costs no API quota); since the preflight resolves
/// tips over the GitHub REST API it spends one request from a 60/hour anonymous
/// budget, so the wasted call is now the difference between a working check and
/// `uncheckable/network`.
pub(crate) fn preflight_can_skip(members: &[EntryInput]) -> bool {
	let mut recorded: Option<&str> = None;
	members.iter().all(|m| {
		let Some(ref_commit) = m.ref_commit.as_deref() else {
			return false;
		};
		// Members recording DIFFERENT commits (reachable after a partial update)
		// can never all equal one remote tip, so no tip value could make this
		// group skip — the exact condition this gate exists to detect.
		if *recorded.get_or_insert(ref_commit) != ref_commit {
			return false;
		}
		locally_intact(m)
	})
}

/// The installed copy was READ and matches its recorded baseline.
///
/// Any answer that skips looking upstream — a preflight skip, a pinned-SHA
/// shortcut — is only honest if this holds: otherwise "nothing moved upstream"
/// gets reported as `UpToDate` for a skill whose folder is missing, unreadable,
/// or locally modified.
fn locally_intact(m: &EntryInput) -> bool {
	!lock_hash_unknown(m.stored_hash.as_deref())
		&& m.local_hash.is_some()
		&& m.local_hash == m.stored_hash
}

pub(crate) fn preflight_decision(
	members: &[EntryInput],
	tip_oid: &str,
) -> PreflightResult {
	if !preflight_can_skip(members)
		|| !members
			.iter()
			.all(|m| m.ref_commit.as_deref() == Some(tip_oid))
	{
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
			// The lock agreeing with upstream does NOT mean the installed copy is
			// current — it means upstream has not moved since we recorded it. With
			// no readable local copy (folder deleted, unreadable, or two agent
			// copies disagreeing) that came out as `UpToDate` for a skill that is
			// not on disk, which is the one answer a user acts on by doing
			// nothing. `UpdateAvailable` stays as-is: an update really does exist,
			// and applying it restores the folder.
			let status = if status == SkillUpdateStatus::UpToDate
				&& !unknown && member.local_hash.is_none()
			{
				SkillUpdateStatus::Uncheckable {
					reason: UncheckableReason::Local,
				}
			} else {
				status
			};
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

		// 2) Pinned SHA → UpToDate without fetching, but ONLY while every
		//    installed copy still matches its baseline.
		//
		//    A commit pin means upstream cannot have moved, so skipping the
		//    network is sound. It says nothing about the local copy, and this
		//    shortcut used to answer `UpToDate` for a pinned skill whose folder
		//    had been deleted. It is also the only thing standing behind
		//    `is_pinned_sha`, which infers immutability from SPELLING — a branch
		//    or force-moved tag named like a 40-hex OID lands here too, so
		//    falling through on local drift is what stops that inference from
		//    also skipping every other check.
		if let Some(r) = &sr.ref_ {
			if is_pinned_sha(r) && members.iter().all(locally_intact) {
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

		let token = match deps.resolver.resolve(&sr.source) {
			TokenResolution::Token(token) => Some(token),
			TokenResolution::NoToken => None,
			TokenResolution::BackendUnavailable => {
				let status = SkillUpdateStatus::Uncheckable {
					reason: UncheckableReason::Network,
				};
				for member in &fetch_members {
					out.push(terminal_output(member, &status));
				}
				continue;
			}
		};
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
			// The snapshot the preflight pinned, when it had one — handed to the
			// fetch below so both act on the same tip.
			let mut pinned: Option<PinnedSnapshot> = None;
			// Tip preflight, no object download (bounded by the per-fetch timeout):
			// skip the fetch when the tip is unchanged AND every member is
			// trustworthy. Any resolver error falls through to the full fetch.
			//
			// `preflight_can_skip` gates the round trip itself: a group that
			// cannot skip no matter what the remote answers must not spend a
			// request to be told so.
			if let Some(rr) =
				ref_resolver.filter(|_| preflight_can_skip(&job.members))
			{
				// This hop runs for EVERY group, including the all-clear case
				// where nothing else touches the network — so without its own
				// span a check that spends all its time here reports none of it,
				// and the route total looks unattributable.
				let started = Instant::now();
				let tip = tokio::time::timeout(
					per_fetch,
					do_resolve(rr, job.sr.clone(), job.token.clone()),
				)
				.await;
				let observed = match tip {
					Ok(Ok(observed)) => Some(observed),
					_ => None,
				};
				let decision = observed.as_ref().map(|observed| {
					preflight_decision(&job.members, &observed.commit_oid)
				});
				log::info!(
					"check-updates preflight [{}]: ref={:?} {} took={:?}",
					aghub_git::redact_source_credentials(&job.sr.source),
					job.sr.ref_,
					match &decision {
						Some(PreflightResult::Skip(_)) => "skip",
						Some(PreflightResult::Fetch) => "fetch",
						None => "resolve-failed",
					},
					started.elapsed()
				);
				if let Some(PreflightResult::Skip(cached)) = decision {
					return (id, job.sr, job.members, JobResult::Skip(cached));
				}
				pinned = observed.and_then(|observed| observed.pinned);
			}
			// Path-scoped fetch: only the locked skill folders for this group.
			let folders: Vec<skill::SkillPath> = job
				.members
				.iter()
				.filter_map(|m| {
					m.skill_path
						.as_deref()
						.and_then(skill_folder_from_lock_path)
				})
				.collect();
			let result = tokio::time::timeout(
				per_fetch,
				do_fetch(fetcher, job.sr.clone(), job.token, folders, pinned),
			)
			.await;
			let outcome = match result {
				Err(_elapsed) => JobResult::Failed(UncheckableReason::Timeout),
				Ok(Err(FetchError::Auth)) => {
					JobResult::Failed(UncheckableReason::Auth)
				}
				Ok(Err(FetchError::Network(_))) => {
					JobResult::Failed(UncheckableReason::Network)
				}
				Ok(Err(FetchError::BackendUnavailable)) => {
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
					//
					// ONLY on `UpToDate`: refCommit means "the commit the
					// INSTALLED content came from", which equals the fetched tip
					// only when the two match. Healing an UpdateAvailable /
					// Renamed row would make the next check preflight-skip (tip
					// unchanged, no local drift) and report UpToDate — the
					// pending update would vanish until upstream moved again.
					for (output, member) in group_out.iter_mut().zip(&members) {
						if member.scope == "global"
							&& output.status == SkillUpdateStatus::UpToDate
						{
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
	folders: Vec<skill::SkillPath>,
	pinned: Option<PinnedSnapshot>,
) -> Result<FetchedRepo, FetchError> {
	tokio::task::spawn_blocking(move || {
		let selection = FetchSelection::Skills(&folders);
		match &pinned {
			// The preflight decided `Fetch` about a SPECIFIC tip. Fetching that
			// exact snapshot is what makes the verdict and the content agree; a
			// re-resolve here could land on a tip nobody judged.
			Some(pinned) => {
				fetcher.fetch_pinned(&sr, token.as_deref(), selection, pinned)
			}
			None => fetcher.fetch(&sr, token.as_deref(), selection),
		}
	})
	.await
	.unwrap_or_else(|e| Err(FetchError::network(format!("fetch task: {e}"))))
}

/// Bridge the synchronous [`RefResolver`] into the async timeout path.
async fn do_resolve(
	resolver: Arc<dyn RefResolver>,
	sr: SourceRef,
	token: Option<String>,
) -> Result<TipObservation, FetchError> {
	tokio::task::spawn_blocking(move || resolver.resolve(&sr, token.as_deref()))
		.await
		.unwrap_or_else(|e| {
			Err(FetchError::network(format!("resolve task: {e}")))
		})
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

	#[test]
	fn token_resolver_interface_needs_only_the_source() {
		struct SourceOnlyResolver;

		impl TokenResolver for SourceOnlyResolver {
			fn resolve(&self, source: &str) -> TokenResolution {
				if source == "https://github.com/owner/repo.git" {
					TokenResolution::Token("token".to_string())
				} else {
					TokenResolution::NoToken
				}
			}
		}

		assert!(matches!(
			SourceOnlyResolver.resolve("https://github.com/owner/repo.git"),
			TokenResolution::Token(token) if token == "token"
		));
	}

	// --- async orchestration stubs -----------------------------------------

	struct StubResolver(Option<String>);
	impl TokenResolver for StubResolver {
		fn resolve(&self, _source: &str) -> TokenResolution {
			match &self.0 {
				Some(token) => TokenResolution::Token(token.clone()),
				None => TokenResolution::NoToken,
			}
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
			_selection: FetchSelection<'_>,
		) -> Result<FetchedRepo, FetchError> {
			*self.calls.lock().unwrap() += 1;
			if let Some(kind) = self.err {
				return Err(match kind {
					"auth" => FetchError::Auth,
					_ => FetchError::network("stub"),
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
		) -> Result<TipObservation, FetchError> {
			*self.calls.lock().unwrap() += 1;
			match self.err {
				Some("auth") => Err(FetchError::Auth),
				Some(_) => Err(FetchError::network("stub")),
				None => Ok(TipObservation::tip_only(self.oid.clone())),
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
		let ref_resolver = Arc::new(StubRefResolver {
			oid: tip.to_string(),
			err: None,
			calls: Mutex::new(0),
		});
		let resolver = StubResolver(None);
		let mut cache = ResultCache::new(Duration::from_secs(300));
		let deps = CheckDeps {
			fetcher: fetcher.clone(),
			ref_resolver: Some(ref_resolver.clone() as Arc<dyn RefResolver>),
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
		// Without this the test passed even if the tip was never looked up at
		// all — a fabricated `UpToDate` reads identically from the outside.
		assert_eq!(
			*ref_resolver.calls.lock().unwrap(),
			1,
			"the skip must be justified by an actual tip lookup"
		);
	}

	/// A preflight is an OPTIMIZATION, so failing it must cost only the
	/// optimization. Every resolver error is a soft failure that falls through to
	/// the full fetch — nothing here may turn a reachable source into
	/// `Uncheckable` just because the tip lookup broke.
	#[tokio::test]
	async fn preflight_failure_falls_through_to_a_successful_fetch() {
		for kind in ["auth", "network"] {
			let dir = tempfile::tempdir().unwrap();
			std::fs::write(dir.path().join("SKILL.md"), b"x").unwrap();
			let hash = skill::compute_skill_folder_hash(dir.path()).unwrap();
			let tip = "abc123def456abc123def456abc123def456abc1";
			let fetcher = Arc::new(StubFetcher {
				root: Some(dir.path().to_path_buf()),
				err: None,
				calls: Mutex::new(0),
			});
			let ref_resolver = Arc::new(StubRefResolver {
				oid: tip.to_string(),
				err: Some(kind),
				calls: Mutex::new(0),
			});
			let resolver = StubResolver(None);
			let mut cache = ResultCache::new(Duration::from_secs(300));
			let deps = CheckDeps {
				fetcher: fetcher.clone(),
				ref_resolver: Some(ref_resolver.clone() as Arc<dyn RefResolver>),
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

			assert_eq!(*ref_resolver.calls.lock().unwrap(), 1, "{kind}");
			assert_eq!(
				*fetcher.calls.lock().unwrap(),
				1,
				"a failed preflight ({kind}) must still be answered by a fetch"
			);
			assert_eq!(
				out[0].status,
				SkillUpdateStatus::UpToDate,
				"and the fetch's real answer must reach the user ({kind})"
			);
		}
	}

	/// Every local blocker, not just a missing `ref_commit`, must save the round
	/// trip — each one makes a skip impossible whatever the remote answers.
	#[tokio::test]
	async fn every_unskippable_reason_spends_zero_preflight_requests() {
		let tip = "abc123def456abc123def456abc123def456abc1";
		/// One way a group becomes unskippable, applied to an otherwise
		/// trustworthy member.
		type Blocker = (&'static str, fn(&mut EntryInput));
		let cases: Vec<Blocker> = vec![
			("no ref_commit", |e| e.ref_commit = None),
			("no stored hash", |e| e.stored_hash = None),
			("placeholder stored hash", |e| {
				e.stored_hash = Some(String::new())
			}),
			("no local hash", |e| e.local_hash = None),
			("local drift", |e| e.local_hash = Some("DRIFTED".into())),
		];
		for (label, break_it) in cases {
			let dir = tempfile::tempdir().unwrap();
			std::fs::write(dir.path().join("SKILL.md"), b"x").unwrap();
			let hash = skill::compute_skill_folder_hash(dir.path()).unwrap();
			let fetcher = Arc::new(StubFetcher {
				root: Some(dir.path().to_path_buf()),
				err: None,
				calls: Mutex::new(0),
			});
			let ref_resolver = Arc::new(StubRefResolver {
				oid: tip.to_string(),
				err: None,
				calls: Mutex::new(0),
			});
			let resolver = StubResolver(None);
			let mut cache = ResultCache::new(Duration::from_secs(300));
			let deps = CheckDeps {
				fetcher: fetcher.clone(),
				ref_resolver: Some(ref_resolver.clone() as Arc<dyn RefResolver>),
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
			break_it(&mut a);

			check_updates(vec![a], deps).await;

			assert_eq!(
				*ref_resolver.calls.lock().unwrap(),
				0,
				"{label}: no tip value could have produced a skip"
			);
			assert_eq!(
				*fetcher.calls.lock().unwrap(),
				1,
				"{label}: the group must still be answered by a fetch"
			);
		}
	}

	/// Two members of one group recording DIFFERENT commits can never both equal
	/// one remote tip, so the gate must recognise that locally too.
	#[test]
	fn members_on_different_commits_cannot_skip() {
		let mut a =
			trustworthy_member("aaa1aaa1aaa1aaa1aaa1aaa1aaa1aaa1aaa1aaa1");
		a.name = "a".into();
		let mut b =
			trustworthy_member("bbb2bbb2bbb2bbb2bbb2bbb2bbb2bbb2bbb2bbb2");
		b.name = "b".into();
		assert!(!preflight_can_skip(&[a, b]));
	}

	/// A group that cannot skip whatever upstream answers must not spend a tip
	/// round trip to be told so. Here the ONLY disqualifier is a missing
	/// `ref_commit` — the hashes agree — so this pins that the gate reads it.
	///
	/// That wasted request used to be free (a git ref advertisement costs no API
	/// quota). It now comes out of GitHub's 60-per-hour anonymous budget, where
	/// spending it is the difference between a working check and
	/// `uncheckable/network`.
	#[tokio::test]
	async fn an_unskippable_group_spends_no_preflight_round_trip() {
		let dir = tempfile::tempdir().unwrap();
		std::fs::write(dir.path().join("SKILL.md"), b"x").unwrap();
		let hash = skill::compute_skill_folder_hash(dir.path()).unwrap();
		let fetcher = Arc::new(StubFetcher {
			root: Some(dir.path().to_path_buf()),
			err: None,
			calls: Mutex::new(0),
		});
		let ref_resolver = Arc::new(StubRefResolver {
			oid: "abc123def456abc123def456abc123def456abc1".to_string(),
			err: None,
			calls: Mutex::new(0),
		});
		let resolver = StubResolver(None);
		let mut cache = ResultCache::new(Duration::from_secs(300));
		let deps = CheckDeps {
			fetcher: fetcher.clone(),
			ref_resolver: Some(ref_resolver.clone() as Arc<dyn RefResolver>),
			resolver: &resolver,
			cache: &mut cache,
			per_fetch: Duration::from_secs(5),
			concurrency: 4,
			offline: false,
			overall_deadline: Duration::from_secs(30),
		};
		let mut a = entry("a", "o/r", Some("main"));
		a.ref_commit = None; // project lock / npx / legacy → never a skip
		a.stored_hash = Some(hash.clone());
		a.local_hash = Some(hash);

		check_updates(vec![a], deps).await;

		assert_eq!(
			*ref_resolver.calls.lock().unwrap(),
			0,
			"no tip value could have produced a skip, so no tip may be bought"
		);
		assert_eq!(
			*fetcher.calls.lock().unwrap(),
			1,
			"the group must still be answered by a real fetch"
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

	/// A check that REPORTS a pending update must not advance `refCommit` to the
	/// upstream tip. Healing it there makes the very next check preflight-skip
	/// (tip unchanged + no local drift) and report `UpToDate` — the pending
	/// update silently vanishes from the UI until upstream moves again, which
	/// reads to the user as "the refresh button is cached".
	#[tokio::test]
	async fn update_available_does_not_heal_ref_commit() {
		let dir = tempfile::tempdir().unwrap();
		std::fs::write(dir.path().join("SKILL.md"), b"upstream-moved").unwrap();
		let old_oid = "0000000000000000000000000000000000000000";
		let fetcher = Arc::new(StubFetcher {
			root: Some(dir.path().to_path_buf()),
			err: None,
			calls: Mutex::new(0),
		});
		let ref_resolver: Arc<dyn RefResolver> = Arc::new(StubRefResolver {
			oid: STUB_FETCH_OID.to_string(),
			err: None,
			calls: Mutex::new(0),
		});
		let resolver = StubResolver(None);
		// Installed copy is intact but built from the PREVIOUS commit.
		let mut g = entry("g", "o/r", Some("main"));
		g.ref_commit = Some(old_oid.to_string());
		g.stored_hash = Some("INSTALLED_HASH".to_string());
		g.local_hash = Some("INSTALLED_HASH".to_string());

		let mut cache = ResultCache::new(Duration::from_secs(300));
		let first = check_updates(
			vec![g.clone()],
			CheckDeps {
				fetcher: fetcher.clone(),
				ref_resolver: Some(Arc::clone(&ref_resolver)),
				resolver: &resolver,
				cache: &mut cache,
				per_fetch: Duration::from_secs(5),
				concurrency: 4,
				offline: false,
				overall_deadline: Duration::from_secs(30),
			},
		)
		.await;
		assert!(matches!(
			first[0].status,
			SkillUpdateStatus::UpdateAvailable { .. }
		));

		// Replay what the API does with the outputs: heal the lock, then check
		// again on a cold cache (a second click of the refresh button).
		if let Some(oid) = &first[0].heal_oid {
			g.ref_commit = Some(oid.clone());
		}
		let mut cache = ResultCache::new(Duration::from_secs(300));
		let second = check_updates(
			vec![g],
			CheckDeps {
				fetcher,
				ref_resolver: Some(ref_resolver),
				resolver: &resolver,
				cache: &mut cache,
				per_fetch: Duration::from_secs(5),
				concurrency: 4,
				offline: false,
				overall_deadline: Duration::from_secs(30),
			},
		)
		.await;
		assert!(
			matches!(
				second[0].status,
				SkillUpdateStatus::UpdateAvailable { .. }
			),
			"the pending update must survive a second check, got {:?}",
			second[0].status
		);
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
			_selection: FetchSelection<'_>,
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
	async fn unavailable_credential_backend_makes_check_uncheckable_without_fetch(
	) {
		struct UnavailableResolver;

		impl TokenResolver for UnavailableResolver {
			fn resolve(&self, _source: &str) -> TokenResolution {
				TokenResolution::BackendUnavailable
			}
		}

		let fetcher = Arc::new(StubFetcher {
			root: None,
			err: Some("network"),
			calls: Mutex::new(0),
		});
		let resolver = UnavailableResolver;
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

		let out =
			check_updates(vec![entry("a", "o/r", Some("main"))], deps).await;

		assert_eq!(
			out[0].status,
			SkillUpdateStatus::Uncheckable {
				reason: UncheckableReason::Network,
			}
		);
		assert_eq!(
			*fetcher.calls.lock().unwrap(),
			0,
			"an indeterminate credential decision must not attempt a fetch",
		);
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
		let mut a = entry("a", "o/r", Some(sha));
		a.stored_hash = Some("H".into());
		a.local_hash = Some("H".into());
		let out = check_updates(vec![a], deps).await;
		assert_eq!(out.len(), 1);
		assert_eq!(out[0].status, SkillUpdateStatus::UpToDate);
		assert_eq!(out[0].heal_hash, None);
		assert_eq!(*fetcher.calls.lock().unwrap(), 0, "pin must not fetch");
	}

	/// A commit pin means UPSTREAM cannot move; it says nothing about the local
	/// copy. Answering `UpToDate` with no readable installed copy reported a
	/// deleted skill as current, and it is also the only guard behind
	/// `is_pinned_sha` inferring immutability from spelling alone.
	#[tokio::test]
	async fn pinned_sha_does_not_excuse_a_missing_local_copy() {
		let dir = tempfile::tempdir().unwrap();
		std::fs::write(dir.path().join("SKILL.md"), b"x").unwrap();
		let upstream = skill::compute_skill_folder_hash(dir.path()).unwrap();
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
		let sha = "0123456789abcdef0123456789abcdef01234567";
		let mut a = entry("a", "o/r", Some(sha));
		a.stored_hash = Some(upstream);
		a.local_hash = None; // the folder is gone

		let out = check_updates(vec![a], deps).await;

		assert_eq!(
			out[0].status,
			SkillUpdateStatus::Uncheckable {
				reason: UncheckableReason::Local
			},
			"a pinned skill with no readable copy must not read as current"
		);
		assert_eq!(
			*fetcher.calls.lock().unwrap(),
			1,
			"and the shortcut must give way to a real fetch"
		);
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
		a.stored_hash = Some(hash_a.clone());
		a.local_hash = Some(hash_a);
		let mut b = entry("b", "o/r", Some("main"));
		b.skill_path = Some("b/SKILL.md".into());
		b.stored_hash = Some("old".into());
		b.local_hash = Some("old".into());
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
			_selection: FetchSelection<'_>,
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
