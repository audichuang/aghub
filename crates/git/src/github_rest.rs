//! GitHub REST fast-path backend: fetch only the selected skill's latest files
//! via the GitHub REST API — no clone, no history, no unrelated blobs.
//!
//! Implements [`RepoFetchBackend`] for exact, normalized `github.com` (mapped
//! to `api.github.com`). Every REST call goes through an injectable
//! [`HttpTransport`] so tests feed canned GitHub API JSON without the network
//! and record the exact request set. Any transient / unsupported / not-GitHub
//! condition surfaces as [`GitError::RestFallback`]. The CALLER decides by
//! timing: a `RestFallback` at **resolve** re-routes to the gix fallback; one
//! that surfaces **after** a successful resolve (chiefly a `truncated` tree) is
//! turned into a clean error by `SkillRepository`, not re-routed (gix 0.84
//! cannot re-fetch a pinned commit by OID). A security-validation failure is a
//! hard error and is never reported as a fallback.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::backend::{
	entry_matches_selection, Blob, RepoFetchBackend, RepoTree, SourceRef,
	TreeEntry,
};
use crate::credentials::Credentials;
use crate::error::{GitError, Result};
use crate::stage::{stage_tree_entries, StagedEntry, StagedEntryMode};
use crate::RepoSnapshot;

/// Default number of concurrent blob downloads (Decision: named constant, not a
/// range).
///
/// Measured on 41 blobs: 6 → 398ms, 16 → 253ms, 32 → 217ms — past 16 the gain
/// is marginal because `reqwest::blocking` drives every stream from ONE
/// current-thread runtime. `api.github.com` advertises
/// `MAX_CONCURRENT_STREAMS=100`, so 16 streams still multiplex on one h2
/// connection.
///
/// This is a **per-repository** ceiling, NOT a process-wide one: every
/// `SkillRepository` builds its own backend, and a desktop process can have
/// several in their blob phase at once (background check-updates, a pinned
/// source session, an install). Nothing here caps the process total — if
/// GitHub's documented 100-concurrent secondary limit ever starts returning
/// 403s, the fix is a shared semaphore, not a smaller constant.
pub const DEFAULT_CONCURRENCY: usize = 16;

/// Accept header for commit + tree JSON endpoints.
const ACCEPT_JSON: &str = "application/vnd.github+json";
/// Accept header for raw blob bytes (no base64).
const ACCEPT_RAW: &str = "application/vnd.github.raw";
/// Recommended GitHub API version header value.
const API_VERSION: &str = "2022-11-28";

/// One outbound REST request. Always a GET for this backend.
#[derive(Clone, Debug)]
pub struct HttpRequest {
	/// Absolute request URL.
	pub url: String,
	/// Header (name, value) pairs, e.g. `("Authorization", "Bearer …")`.
	pub headers: Vec<(String, String)>,
	/// Remaining absolute-deadline budget for the complete request.
	pub timeout: Option<Duration>,
}

/// One inbound REST response.
#[derive(Clone, Debug)]
pub struct HttpResponse {
	/// HTTP status code.
	pub status: u16,
	/// Response header (name, value) pairs.
	pub headers: Vec<(String, String)>,
	/// Raw response body bytes.
	pub body: Vec<u8>,
}

impl HttpResponse {
	/// Case-insensitive header lookup.
	pub fn header(&self, name: &str) -> Option<&str> {
		self.headers
			.iter()
			.find(|(k, _)| k.eq_ignore_ascii_case(name))
			.map(|(_, v)| v.as_str())
	}
}

/// Injectable HTTP transport. The production impl uses `reqwest`; tests feed
/// canned responses and record the request set. A transport-level error (a
/// genuine network failure) is returned as `Err` and the backend classifies it
/// into [`GitError::RestFallback`].
pub trait HttpTransport: Send + Sync {
	/// Execute one request, returning the response or a transport error.
	fn execute(&self, request: HttpRequest) -> Result<HttpResponse>;
}

/// Production transport: synchronous `reqwest` GET requests.
///
/// Every instance borrows ONE process-wide client, so its connection pool — and
/// therefore the ~80ms TCP+TLS handshake to `api.github.com` — is paid once per
/// process instead of once per `SkillRepository`. Auth stays per-request (see
/// [`build_headers`]): NEVER give this client `default_headers` or a cookie
/// store, or one source's token would ride along on another source's request.
/// Never give it a client-level `.timeout(...)` either — that would silently
/// cover requests whose caller forgot to pass a deadline budget.
pub struct ReqwestTransport {
	client: &'static reqwest::blocking::Client,
}

impl ReqwestTransport {
	/// Borrow the process-wide blocking client, building it on first use.
	///
	/// The initializer builds on its OWN OS thread because
	/// `reqwest::blocking::Client::new` panics when it runs on a Tokio worker
	/// ("Cannot drop a runtime in a context where blocking is not allowed"),
	/// and callers do construct one straight from an async handler — see
	/// `git_scan_skills` in `aghub-api`. Before this was a `OnceLock` every
	/// such call built its own client and every one of them was exposed; now
	/// only the process's first call would be, which is worse to diagnose.
	/// Owning the hop here means no caller has to know the rule.
	pub fn new() -> Self {
		static CLIENT: OnceLock<reqwest::blocking::Client> = OnceLock::new();
		Self {
			client: CLIENT.get_or_init(|| {
				std::thread::spawn(reqwest::blocking::Client::new)
					.join()
					.expect("shared blocking client initializer panicked")
			}),
		}
	}
}

impl Default for ReqwestTransport {
	fn default() -> Self {
		Self::new()
	}
}

impl HttpTransport for ReqwestTransport {
	fn execute(&self, request: HttpRequest) -> Result<HttpResponse> {
		let mut builder = self.client.get(&request.url);
		for (name, value) in &request.headers {
			builder = builder.header(name.as_str(), value.as_str());
		}
		if let Some(timeout) = request.timeout {
			// Reqwest applies this from connect through completion of the
			// response body, bounding connect, read, and overall request time.
			builder = builder.timeout(timeout);
		}
		let response = builder.send().map_err(|e| {
			GitError::rest_fallback(format!("HTTP request failed: {e}"))
		})?;
		let status = response.status().as_u16();
		let headers = response
			.headers()
			.iter()
			.map(|(k, v)| {
				(k.as_str().to_string(), v.to_str().unwrap_or("").to_string())
			})
			.collect();
		let body = response
			.bytes()
			.map_err(|e| {
				GitError::rest_fallback(format!("reading response body: {e}"))
			})?
			.to_vec();
		Ok(HttpResponse {
			status,
			headers,
			body,
		})
	}
}

/// Whether `host` normalizes to the one trusted public GitHub origin.
pub fn is_github_com_host(host: &str) -> bool {
	host.trim()
		.trim_end_matches('.')
		.eq_ignore_ascii_case("github.com")
}

/// Explicit trusted host mapping: only exact, normalized `github.com` maps to
/// `api.github.com`. Subdomains and GHES custom domains fall back to git.
pub fn github_api_host(host: &str) -> Option<&'static str> {
	if is_github_com_host(host) {
		Some("api.github.com")
	} else {
		None
	}
}

/// Resolve-time context for one snapshot, cached by `commit_oid` so the
/// auth-less `read_tree` / `read_blobs` / `materialize` calls can reach the same
/// repo with the same token (mirrors [`crate::backend::GixShallow`]'s cache).
#[derive(Clone)]
pub(crate) struct RepoContext {
	pub api_host: &'static str,
	pub owner: String,
	pub repo: String,
	/// Resolved token sent up front on every request (token-first auth).
	pub token: Option<String>,
	pub tree_oid: String,
	pub blob_cache: Arc<Mutex<HashMap<String, Vec<u8>>>>,
	/// tree_oid → already-parsed listing. A pinned oid names immutable content,
	/// so one fetch per snapshot serves list + preflight + materialize.
	pub tree_cache: Arc<Mutex<HashMap<String, RepoTree>>>,
	pub blob_admission: Arc<Mutex<BlobAdmission>>,
}

/// Preflight budget for one snapshot's blob phase.
///
/// KNOWN SCOPE LIMIT: this ledger is per-[`RepoContext`], but GitHub's quota is
/// per-credential and global. Two repositories fetching concurrently on the
/// same token each see the live count and each reserve against it, so together
/// they can reserve more than exists and collect 403s instead of one being
/// refused up front. Pre-dates the worker pool (the old batched path had the
/// same per-context ledger) and is widened by it. Fixing it properly means a
/// process-wide reservation bucket keyed by api_host + credential identity —
/// deliberately out of scope here, since this gate is a courtesy preflight and
/// the real limit is still enforced by the server.
#[derive(Default)]
pub(crate) struct BlobAdmission {
	remaining_requests: Option<u64>,
	/// Unix time at which the rate-limit window holding `remaining_requests`
	/// rolls over (`x-ratelimit-reset`). The tally is authoritative until then
	/// and meaningless after, so this bounds it exactly instead of guessing
	/// with a fixed TTL: a guess is either too short (the tally expires mid
	/// window and the budget check silently stops running) or too long (a
	/// pre-rollover 0 keeps refusing work the reset already allowed).
	///
	/// Compared against the wall clock, which is not monotonic — a clock jump
	/// can expire this early or hold it late. Accepted: the server sends the
	/// boundary in wall-clock terms, so tracking it any other way means
	/// estimating an offset, and being wrong here only mis-times a courtesy
	/// preflight.
	resets_at: Option<SystemTime>,
	byte_sizes: HashMap<String, u64>,
}

impl BlobAdmission {
	/// Record a live rate-limit reading — the ONLY writer.
	fn observe(&mut self, remaining: u64, resets_at: Option<SystemTime>) {
		self.remaining_requests = Some(remaining);
		// A reading without a reset header cannot be window-bounded; keep the
		// count (refusing loudly beats overrunning silently) rather than
		// inventing an expiry.
		self.resets_at = resets_at;
	}

	/// Requests known to be left as of `now`, or `None` once the window that
	/// produced the count has rolled over. Takes the clock so a test can cross
	/// a window boundary without sleeping.
	fn remaining_at(&self, now: SystemTime) -> Option<u64> {
		if self.resets_at.is_some_and(|at| now >= at) {
			return None;
		}
		self.remaining_requests
	}
}

/// One rate-limit reading. The two numbers are only meaningful together: a
/// `remaining` belongs to exactly the window named by its `reset`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RateLimitReading {
	reset: u64,
	remaining: u64,
}

/// Fold a reading into the running one. Kept as an indivisible pair on purpose:
/// aggregating `min(remaining)` and `max(reset)` separately can invent a state
/// no server ever reported — a pre-rollover `remaining=0` welded onto the NEXT
/// window's reset, which then refuses that whole window.
fn merge_reading(
	slot: &Mutex<Option<RateLimitReading>>,
	seen: RateLimitReading,
) {
	let Ok(mut current) = slot.lock() else { return };
	match *current {
		// A stale window's reading says nothing about the live one.
		Some(have) if seen.reset < have.reset => {}
		// Same window: the lowest count is the freshest truth.
		Some(have) if seen.reset == have.reset => {
			current.replace(RateLimitReading {
				reset: have.reset,
				remaining: have.remaining.min(seen.remaining),
			});
		}
		// Newer window: it supersedes rather than mixes.
		_ => {
			current.replace(seen);
		}
	}
}

/// Parse `x-ratelimit-reset` (Unix seconds). `None` when absent, unparseable,
/// or so far out that it is not a representable `SystemTime` — a header that
/// large is a broken proxy, not a deadline worth honoring.
fn rate_limit_reset_secs(response: &HttpResponse) -> Option<u64> {
	let seconds: u64 = response.header("x-ratelimit-reset")?.parse().ok()?;
	epoch_seconds_to_time(seconds).map(|_| seconds)
}

/// `UNIX_EPOCH + secs`, or `None` on overflow (`SystemTime`'s `Add` panics).
fn epoch_seconds_to_time(seconds: u64) -> Option<SystemTime> {
	UNIX_EPOCH.checked_add(Duration::from_secs(seconds))
}

/// GitHub REST fast-path backend. Constructed with an [`HttpTransport`]; an
/// optional absolute [`Instant`] deadline is honored inside the backend (the
/// orchestrator's outer `spawn_blocking` timeout cannot abort in-flight
/// blocking HTTP).
pub struct GithubRest {
	pub(crate) transport: Arc<dyn HttpTransport>,
	pub(crate) deadline: Option<Instant>,
	pub(crate) timeout: Option<Duration>,
	pub(crate) concurrency: usize,
	pub(crate) cache: Arc<Mutex<HashMap<String, RepoContext>>>,
}

impl GithubRest {
	/// Create a backend over `transport` with the default concurrency and no
	/// deadline.
	pub fn new(transport: Arc<dyn HttpTransport>) -> Self {
		Self {
			transport,
			deadline: None,
			timeout: None,
			concurrency: DEFAULT_CONCURRENCY,
			cache: Arc::new(Mutex::new(HashMap::new())),
		}
	}

	/// Set an absolute deadline; requests issued at or after it fail with
	/// [`GitError::RestFallback`] without touching the network.
	pub fn with_deadline(mut self, deadline: Instant) -> Self {
		self.deadline = Some(deadline);
		self
	}

	/// Set the budget used to derive a fresh absolute deadline for each backend
	/// operation. Unlike [`Self::with_deadline`], this is safe for repositories
	/// retained between a catalog scan and a later install.
	pub fn with_timeout(mut self, timeout: Duration) -> Self {
		self.timeout = Some(timeout);
		self
	}

	/// Override the concurrent-blob-download count.
	pub fn with_concurrency(mut self, concurrency: usize) -> Self {
		self.concurrency = concurrency.max(1);
		self
	}

	fn operation_deadline(&self) -> Result<Option<Instant>> {
		if let Some(deadline) = self.deadline {
			remaining_timeout(Some(deadline))?;
			return Ok(Some(deadline));
		}
		Ok(self.timeout.map(|timeout| Instant::now() + timeout))
	}

	fn scoped_operation(&self) -> Result<Self> {
		Ok(Self {
			transport: Arc::clone(&self.transport),
			deadline: self.operation_deadline()?,
			timeout: None,
			concurrency: self.concurrency,
			cache: Arc::clone(&self.cache),
		})
	}

	fn get_context(&self, commit_oid: &str) -> Result<RepoContext> {
		let cache = self.cache.lock().map_err(|_| {
			GitError::clone_failed("GithubRest cache lock poisoned")
		})?;
		cache.get(commit_oid).cloned().ok_or_else(|| {
			GitError::clone_failed(format!(
				"snapshot commit {commit_oid} is not in the GithubRest cache; \
				 call resolve first"
			))
		})
	}

	/// Issue one GET via the transport. Deadline is checked first; transport
	/// errors and non-2xx statuses become [`GitError::RestFallback`].
	fn request(
		&self,
		url: String,
		token: Option<&str>,
		accept: &str,
		deadline: Option<Instant>,
	) -> Result<HttpResponse> {
		let headers = build_headers(token, accept);
		let response = self
			.transport
			.execute(HttpRequest {
				url,
				headers,
				timeout: remaining_timeout(deadline)?,
			})
			.map_err(|e| {
				GitError::rest_fallback(format!("transport error: {e}"))
			})?;
		if !(200..300).contains(&response.status) {
			return Err(GitError::rest_fallback(format!(
				"HTTP {}",
				response.status
			)));
		}
		Ok(response)
	}
}

impl RepoFetchBackend for GithubRest {
	fn resolve(
		&self,
		source: &SourceRef,
		auth: Option<&Credentials>,
	) -> Result<RepoSnapshot> {
		let deadline = self.operation_deadline()?;

		let parsed = url::Url::parse(&source.url).map_err(|e| {
			GitError::rest_fallback(format!("invalid source URL: {e}"))
		})?;
		let host = parsed
			.host_str()
			.ok_or_else(|| GitError::rest_fallback("source URL has no host"))?;
		let api_host = github_api_host(host).ok_or_else(|| {
			GitError::rest_fallback(format!("host '{host}' is not github"))
		})?;

		let (owner, repo) = parse_owner_repo(&parsed)?;
		let ref_name = source.ref_.as_deref().unwrap_or("HEAD");
		let token = auth.map(|c| c.password.clone());

		let url = commits_url(api_host, &owner, &repo, ref_name)?;
		let response =
			self.request(url, token.as_deref(), ACCEPT_JSON, deadline)?;

		let value: serde_json::Value = serde_json::from_slice(&response.body)
			.map_err(|e| {
			GitError::rest_fallback(format!("malformed commit response: {e}"))
		})?;

		let commit_oid = value
			.get("sha")
			.and_then(|v| v.as_str())
			.ok_or_else(|| {
				GitError::rest_fallback(
					"malformed commit response: missing sha",
				)
			})?
			.to_string();
		let tree_oid = value
			.pointer("/commit/tree/sha")
			.and_then(|v| v.as_str())
			.ok_or_else(|| {
				GitError::rest_fallback(
					"malformed commit response: missing tree sha",
				)
			})?
			.to_string();
		let commit_time = value
			.pointer("/commit/committer/date")
			.and_then(|v| v.as_str())
			.or_else(|| {
				value
					.pointer("/commit/author/date")
					.and_then(|v| v.as_str())
			})
			.map(|s| s.to_string());

		{
			let mut cache = self.cache.lock().map_err(|_| {
				GitError::clone_failed("GithubRest cache lock poisoned")
			})?;
			cache.insert(
				commit_oid.clone(),
				RepoContext {
					api_host,
					owner,
					repo,
					token,
					tree_oid: tree_oid.clone(),
					blob_cache: Arc::new(Mutex::new(HashMap::new())),
					tree_cache: Arc::new(Mutex::new(HashMap::new())),
					blob_admission: Arc::new(Mutex::new(
						BlobAdmission::default(),
					)),
				},
			);
		}

		Ok(RepoSnapshot {
			commit_oid,
			tree_oid,
			commit_time,
		})
	}

	fn read_tree(&self, snapshot: &RepoSnapshot) -> Result<RepoTree> {
		let deadline = self.operation_deadline()?;
		let ctx = self.get_context(&snapshot.commit_oid)?;
		// Prefer the snapshot pin; fall back to the resolve-time cache.
		let tree_oid = if !snapshot.tree_oid.is_empty() {
			snapshot.tree_oid.as_str()
		} else {
			ctx.tree_oid.as_str()
		};
		// ponytail: unbounded, same as the blob_cache beside it — one ~2 MB
		// listing next to every blob's bytes is noise. LRU when blob_cache
		// gets one. A hit deliberately skips the rate-limit refresh below:
		// the local admission tally only ever under-counts what remains.
		{
			let cached = ctx.tree_cache.lock().map_err(|_| {
				GitError::clone_failed("GithubRest tree cache lock poisoned")
			})?;
			if let Some(tree) = cached.get(tree_oid) {
				return Ok(tree.clone());
			}
		}
		let url = format!(
			"https://{}/repos/{}/{}/git/trees/{}?recursive=1",
			ctx.api_host, ctx.owner, ctx.repo, tree_oid
		);
		let response =
			self.request(url, ctx.token.as_deref(), ACCEPT_JSON, deadline)?;

		let value: serde_json::Value = serde_json::from_slice(&response.body)
			.map_err(|e| {
			GitError::rest_fallback(format!("malformed tree response: {e}"))
		})?;

		if value.get("truncated").and_then(|v| v.as_bool()) == Some(true) {
			return Err(GitError::rest_fallback("tree truncated"));
		}

		let tree =
			value
				.get("tree")
				.and_then(|v| v.as_array())
				.ok_or_else(|| {
					GitError::rest_fallback(
						"malformed tree response: missing tree array",
					)
				})?;

		let mut entries = Vec::new();
		for entry in tree {
			let typ = entry.get("type").and_then(|v| v.as_str()).unwrap_or("");
			if typ == "tree" {
				continue;
			}

			let path = entry
				.get("path")
				.and_then(|v| v.as_str())
				.ok_or_else(|| {
					GitError::rest_fallback(
						"malformed tree entry: missing path",
					)
				})?
				.to_string();
			let oid = entry
				.get("sha")
				.and_then(|v| v.as_str())
				.ok_or_else(|| {
					GitError::rest_fallback("malformed tree entry: missing sha")
				})?
				.to_string();
			let mode = entry.get("mode").and_then(|v| v.as_str()).unwrap_or("");

			let (staged_mode, size) = match (mode, typ) {
				("100644", _) => {
					let size = entry.get("size").and_then(|v| v.as_u64());
					(StagedEntryMode::Regular, size)
				}
				("100755", _) => {
					let size = entry.get("size").and_then(|v| v.as_u64());
					(StagedEntryMode::Executable, size)
				}
				("120000", _) => {
					let size = entry.get("size").and_then(|v| v.as_u64());
					(StagedEntryMode::Symlink, size)
				}
				("160000", _) | (_, "commit") => {
					(StagedEntryMode::Gitlink, None)
				}
				_ => {
					// Unknown blob-like mode: treat as regular file.
					let size = entry.get("size").and_then(|v| v.as_u64());
					(StagedEntryMode::Regular, size)
				}
			};

			entries.push(TreeEntry {
				path,
				mode: staged_mode,
				oid,
				size,
			});
		}

		{
			let remaining = response
				.header("x-ratelimit-remaining")
				.and_then(|value| value.parse::<u64>().ok());
			let resets_at = rate_limit_reset_secs(&response)
				.and_then(epoch_seconds_to_time);
			let mut admission = ctx.blob_admission.lock().map_err(|_| {
				GitError::clone_failed(
					"GithubRest blob admission lock poisoned",
				)
			})?;
			if let Some(remaining) = remaining {
				admission.observe(remaining, resets_at);
			}
			admission.byte_sizes.clear();
			for entry in &entries {
				if let Some(size) = entry.size {
					admission
						.byte_sizes
						.entry(entry.oid.clone())
						.or_insert(size);
				}
			}
		}

		let tree = RepoTree { entries };
		// Insert LAST: truncated / non-2xx / malformed all returned above, so a
		// failed read is never cached.
		ctx.tree_cache
			.lock()
			.map_err(|_| {
				GitError::clone_failed("GithubRest tree cache lock poisoned")
			})?
			.insert(tree_oid.to_string(), tree.clone());
		Ok(tree)
	}

	fn read_blobs(
		&self,
		snapshot: &RepoSnapshot,
		oids: &[String],
	) -> Result<Vec<Blob>> {
		let ctx = self.get_context(&snapshot.commit_oid)?;

		// One request per unique SHA.
		let mut seen = HashSet::new();
		let unique: Vec<String> = oids
			.iter()
			.filter(|oid| seen.insert((*oid).clone()))
			.cloned()
			.collect();

		if unique.is_empty() {
			return Ok(Vec::new());
		}

		let concurrency = self.concurrency;
		let operation_deadline = self.operation_deadline()?;
		let mut out = Vec::with_capacity(unique.len());
		let mut missing = Vec::new();
		{
			let cache = ctx.blob_cache.lock().map_err(|_| {
				GitError::clone_failed("GithubRest blob cache lock poisoned")
			})?;
			for oid in unique {
				match cache.get(&oid) {
					Some(bytes) => out.push(Blob {
						oid,
						bytes: bytes.clone(),
					}),
					None => missing.push(oid),
				}
			}
		}

		if !missing.is_empty() {
			let mut admission = ctx.blob_admission.lock().map_err(|_| {
				GitError::clone_failed(
					"GithubRest blob admission lock poisoned",
				)
			})?;
			let request_budget = missing.len() as u64;
			let byte_budget = missing
				.iter()
				.filter_map(|oid| admission.byte_sizes.get(oid))
				.sum::<u64>();
			if let Some(remaining) = admission.remaining_at(SystemTime::now()) {
				if request_budget > remaining {
					return Err(GitError::rest_fallback(format!(
						"blob admission needs {request_budget} requests and \
						 {byte_budget} bytes, but only {remaining} requests \
						 remain"
					)));
				}
				// Reserve for the batch; the responses below correct this back
				// to the server's real count.
				admission.remaining_requests = Some(remaining - request_budget);
			}
		}

		if !missing.is_empty() {
			// A continuously-fed pool, NOT `missing.chunks(concurrency)`: a
			// batch barrier makes every worker wait on its chunk's slowest blob
			// before the next request is even issued.
			let workers = concurrency.min(missing.len());
			let next = AtomicUsize::new(0);
			// ponytail: a hint, not a cancel token — a worker may still finish
			// the request it already sent. Waste ceiling ~1 request/worker.
			let aborted = AtomicBool::new(false);
			// The live rate-limit reading, kept as an indivisible (reset,
			// remaining) pair — see `merge_reading`. Since the tree cache
			// landed, this is the ONLY thing that corrects the reservation
			// above back to what the server actually charged.
			let observed: Mutex<Option<RateLimitReading>> = Mutex::new(None);
			let missing = &missing;
			let ctx = &ctx;
			let transport: &dyn HttpTransport = self.transport.as_ref();

			let (fetched, failure) = std::thread::scope(|scope| {
				// No `move`: the workers borrow the atomics and the context.
				let handles: Vec<_> = (0..workers)
					.map(|_| {
						scope.spawn(|| -> (Vec<Blob>, Option<GitError>) {
							let mut mine = Vec::new();
							while !aborted.load(Ordering::Relaxed) {
								let i = next.fetch_add(1, Ordering::Relaxed);
								let Some(oid) = missing.get(i) else { break };
								match fetch_blob(
									transport,
									operation_deadline,
									ctx,
									oid,
									&observed,
								) {
									Ok(blob) => mine.push(blob),
									Err(e) => {
										aborted.store(true, Ordering::Relaxed);
										// Keep `mine`: those blobs are paid for.
										return (mine, Some(e));
									}
								}
							}
							(mine, None)
						})
					})
					.collect();

				let mut blobs = Vec::with_capacity(missing.len());
				let mut first_err: Option<GitError> = None;
				for handle in handles {
					match handle.join() {
						Ok((mut mine, e)) => {
							blobs.append(&mut mine);
							if let Some(e) = e {
								first_err.get_or_insert(e);
							}
						}
						Err(_) => {
							first_err.get_or_insert_with(|| {
								GitError::rest_fallback(
									"blob download thread panicked",
								)
							});
						}
					}
				}
				(blobs, first_err)
			});

			// Commit BEFORE propagating any error: these blobs were paid for,
			// and the tally must reflect what the server actually charged.
			let mut cache = ctx.blob_cache.lock().map_err(|_| {
				GitError::clone_failed("GithubRest blob cache lock poisoned")
			})?;
			for blob in &fetched {
				cache.insert(blob.oid.clone(), blob.bytes.clone());
			}
			drop(cache);
			let seen = observed.lock().ok().and_then(|slot| *slot);
			if let Some(seen) = seen {
				if let Ok(mut admission) = ctx.blob_admission.lock() {
					admission.observe(
						seen.remaining,
						epoch_seconds_to_time(seen.reset),
					);
				}
			}
			if let Some(e) = failure {
				return Err(e);
			}
			out.extend(fetched);
		}

		Ok(out)
	}

	fn read_tree_and_blobs(
		&self,
		snapshot: &RepoSnapshot,
		select: &dyn Fn(&RepoTree) -> Vec<String>,
	) -> Result<(RepoTree, Vec<Blob>)> {
		let operation = self.scoped_operation()?;
		let tree = operation.read_tree(snapshot)?;
		let oids = select(&tree);
		let blobs = operation.read_blobs(snapshot, &oids)?;
		Ok((tree, blobs))
	}

	fn materialize(
		&self,
		snapshot: &RepoSnapshot,
		paths: &[&str],
		dest: &Path,
	) -> Result<()> {
		let operation = self.scoped_operation()?;
		let tree = operation.read_tree(snapshot)?;
		let selected: Vec<&TreeEntry> = tree
			.entries
			.iter()
			.filter(|e| entry_matches_selection(&e.path, paths))
			.collect();

		let blob_oids: Vec<String> = selected
			.iter()
			.filter(|e| {
				matches!(
					e.mode,
					StagedEntryMode::Regular
						| StagedEntryMode::Executable
						| StagedEntryMode::Symlink
				)
			})
			.map(|e| e.oid.clone())
			.collect();

		let blobs = operation.read_blobs(snapshot, &blob_oids)?;
		let mut by_oid: HashMap<String, Vec<u8>> =
			HashMap::with_capacity(blobs.len());
		for blob in blobs {
			by_oid.insert(blob.oid, blob.bytes);
		}

		let mut staged = Vec::with_capacity(selected.len());
		for entry in selected {
			let bytes = match entry.mode {
				StagedEntryMode::Gitlink => Vec::new(),
				StagedEntryMode::Regular
				| StagedEntryMode::Executable
				| StagedEntryMode::Symlink => {
					by_oid.get(&entry.oid).cloned().ok_or_else(|| {
						GitError::rest_fallback(format!(
							"missing blob bytes for oid {}",
							entry.oid
						))
					})?
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

fn build_headers(token: Option<&str>, accept: &str) -> Vec<(String, String)> {
	let mut headers = vec![
		("User-Agent".into(), "aghub".into()),
		("X-GitHub-Api-Version".into(), API_VERSION.into()),
		("Accept".into(), accept.into()),
	];
	if let Some(token) = token {
		headers.push(("Authorization".into(), format!("Bearer {token}")));
	}
	headers
}

/// Extract `owner` / `repo` from a GitHub clone URL path.
fn parse_owner_repo(url: &url::Url) -> Result<(String, String)> {
	let mut segments = url
		.path()
		.trim_start_matches('/')
		.split('/')
		.filter(|s| !s.is_empty());
	let owner = segments.next().ok_or_else(|| {
		GitError::rest_fallback("malformed github URL: missing owner")
	})?;
	let repo_raw = segments.next().ok_or_else(|| {
		GitError::rest_fallback("malformed github URL: missing repo")
	})?;
	let repo = repo_raw
		.strip_suffix(".git")
		.unwrap_or(repo_raw)
		.to_string();
	if owner.is_empty() || repo.is_empty() {
		return Err(GitError::rest_fallback(
			"malformed github URL: empty owner or repo",
		));
	}
	Ok((owner.to_string(), repo))
}

/// Build `GET /repos/{owner}/{repo}/commits/{ref}` with a percent-encoded ref.
fn commits_url(
	api_host: &str,
	owner: &str,
	repo: &str,
	ref_name: &str,
) -> Result<String> {
	let mut url =
		url::Url::parse(&format!("https://{api_host}/")).map_err(|e| {
			GitError::rest_fallback(format!("invalid api host URL: {e}"))
		})?;
	{
		let mut segs = url
			.path_segments_mut()
			.map_err(|_| GitError::rest_fallback("cannot set path segments"))?;
		segs.clear();
		segs.push("repos")
			.push(owner)
			.push(repo)
			.push("commits")
			.push(ref_name);
	}
	Ok(url.into())
}

/// Fetch one blob via the raw media type. Used from worker threads.
fn fetch_blob(
	transport: &dyn HttpTransport,
	deadline: Option<Instant>,
	ctx: &RepoContext,
	oid: &str,
	observed: &Mutex<Option<RateLimitReading>>,
) -> Result<Blob> {
	let url = format!(
		"https://{}/repos/{}/{}/git/blobs/{oid}",
		ctx.api_host, ctx.owner, ctx.repo
	);
	let headers = build_headers(ctx.token.as_deref(), ACCEPT_RAW);
	let response = transport
		.execute(HttpRequest {
			url,
			headers,
			timeout: remaining_timeout(deadline)?,
		})
		.map_err(|e| {
			GitError::rest_fallback(format!("transport error: {e}"))
		})?;
	if let (Some(remaining), Some(reset)) = (
		response
			.header("x-ratelimit-remaining")
			.and_then(|v| v.parse::<u64>().ok()),
		rate_limit_reset_secs(&response),
	) {
		merge_reading(observed, RateLimitReading { reset, remaining });
	}
	if !(200..300).contains(&response.status) {
		return Err(GitError::rest_fallback(format!(
			"HTTP {} fetching blob {oid}",
			response.status
		)));
	}
	Ok(Blob {
		oid: oid.to_string(),
		bytes: response.body,
	})
}

fn remaining_timeout(deadline: Option<Instant>) -> Result<Option<Duration>> {
	let Some(deadline) = deadline else {
		return Ok(None);
	};
	deadline
		.checked_duration_since(Instant::now())
		.filter(|remaining| !remaining.is_zero())
		.map(Some)
		.ok_or_else(|| GitError::rest_fallback("deadline exceeded"))
}

#[cfg(test)]
mod admission_tests {
	use super::*;

	fn at(base: SystemTime, secs: u64) -> SystemTime {
		base + Duration::from_secs(secs)
	}

	/// A pinned source session lives for minutes between a scan and an install,
	/// and a cache hit on the tree means no fresh header arrives in between. A
	/// tally bounded by elapsed time instead of by its window would expire in
	/// that gap and the budget check would silently stop running.
	#[test]
	fn a_tally_stays_authoritative_for_its_whole_window() {
		let observed = SystemTime::now();
		let mut admission = BlobAdmission::default();
		admission.observe(1, Some(at(observed, 3600)));

		assert_eq!(
			admission.remaining_at(at(observed, 600)),
			Some(1),
			"ten minutes into an hour-long window the count still holds"
		);
	}

	/// …and the mirror image: once the window is spent, its count must stop
	/// refusing work the rollover already allowed.
	#[test]
	fn a_tally_expires_exactly_when_its_window_does() {
		let observed = SystemTime::now();
		let mut admission = BlobAdmission::default();
		admission.observe(0, Some(at(observed, 60)));

		assert_eq!(admission.remaining_at(at(observed, 59)), Some(0));
		assert_eq!(
			admission.remaining_at(at(observed, 61)),
			None,
			"a spent window's zero must not refuse the next window"
		);
	}

	/// Aggregating min-remaining and max-reset separately would weld a spent
	/// window's `0` onto the next window's reset and refuse that whole window.
	#[test]
	fn a_rolled_over_reading_supersedes_rather_than_mixes() {
		let slot = Mutex::new(None);
		merge_reading(
			&slot,
			RateLimitReading {
				reset: 1_000,
				remaining: 0,
			},
		);
		merge_reading(
			&slot,
			RateLimitReading {
				reset: 4_600,
				remaining: 4_999,
			},
		);

		assert_eq!(
			*slot.lock().unwrap(),
			Some(RateLimitReading {
				reset: 4_600,
				remaining: 4_999
			}),
			"the newer window replaces the older one whole"
		);
	}

	#[test]
	fn readings_in_one_window_keep_the_lowest_count() {
		let slot = Mutex::new(None);
		for remaining in [40, 12, 27] {
			merge_reading(
				&slot,
				RateLimitReading {
					reset: 1_000,
					remaining,
				},
			);
		}
		assert_eq!(slot.lock().unwrap().unwrap().remaining, 12);
	}

	/// A worker still finishing a request from the previous window must not
	/// drag the ledger backwards.
	#[test]
	fn a_stale_window_reading_is_ignored() {
		let slot = Mutex::new(None);
		merge_reading(
			&slot,
			RateLimitReading {
				reset: 4_600,
				remaining: 4_999,
			},
		);
		merge_reading(
			&slot,
			RateLimitReading {
				reset: 1_000,
				remaining: 0,
			},
		);
		assert_eq!(slot.lock().unwrap().unwrap().remaining, 4_999);
	}

	/// A broken proxy can return a syntactically valid but unrepresentable
	/// reset; `UNIX_EPOCH + Duration` panics on those.
	#[test]
	fn an_unrepresentable_reset_header_is_rejected_not_panicked_on() {
		let response = HttpResponse {
			status: 200,
			headers: vec![("x-ratelimit-reset".into(), u64::MAX.to_string())],
			body: Vec::new(),
		};
		assert_eq!(rate_limit_reset_secs(&response), None);
		assert_eq!(epoch_seconds_to_time(u64::MAX), None);
	}
}
