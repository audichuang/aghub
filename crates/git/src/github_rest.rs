//! GitHub REST fast-path backend: fetch only the selected skill's latest files
//! via the GitHub REST API — no clone, no history, no unrelated blobs.
//!
//! Implements [`RepoFetchBackend`] for `github.com` / `*.github.com` (mapped to
//! `api.github.com`). Every REST call goes through an injectable
//! [`HttpTransport`] so tests feed canned GitHub API JSON without the network
//! and record the exact request set. Any transient / unsupported / not-GitHub
//! condition surfaces as [`GitError::RestFallback`] so the caller can route to
//! the gix fallback; a security-validation failure is a hard error and is never
//! reported as a fallback.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Instant;

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
pub const DEFAULT_CONCURRENCY: usize = 6;

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
pub struct ReqwestTransport {
	client: reqwest::blocking::Client,
}

impl ReqwestTransport {
	/// Build a transport with a default blocking client.
	pub fn new() -> Self {
		Self {
			client: reqwest::blocking::Client::new(),
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
}

/// GitHub REST fast-path backend. Constructed with an [`HttpTransport`]; an
/// optional absolute [`Instant`] deadline is honored inside the backend (the
/// orchestrator's outer `spawn_blocking` timeout cannot abort in-flight
/// blocking HTTP).
pub struct GithubRest {
	pub(crate) transport: Arc<dyn HttpTransport>,
	pub(crate) deadline: Option<Instant>,
	pub(crate) concurrency: usize,
	pub(crate) cache: Mutex<HashMap<String, RepoContext>>,
}

impl GithubRest {
	/// Create a backend over `transport` with the default concurrency and no
	/// deadline.
	pub fn new(transport: Arc<dyn HttpTransport>) -> Self {
		Self {
			transport,
			deadline: None,
			concurrency: DEFAULT_CONCURRENCY,
			cache: Mutex::new(HashMap::new()),
		}
	}

	/// Set an absolute deadline; requests issued at or after it fail with
	/// [`GitError::RestFallback`] without touching the network.
	pub fn with_deadline(mut self, deadline: Instant) -> Self {
		self.deadline = Some(deadline);
		self
	}

	/// Override the concurrent-blob-download count.
	pub fn with_concurrency(mut self, concurrency: usize) -> Self {
		self.concurrency = concurrency.max(1);
		self
	}

	/// `Err(RestFallback)` if the absolute deadline has passed.
	pub(crate) fn check_deadline(&self) -> Result<()> {
		if let Some(deadline) = self.deadline {
			if Instant::now() >= deadline {
				return Err(GitError::rest_fallback("deadline exceeded"));
			}
		}
		Ok(())
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
	) -> Result<HttpResponse> {
		self.check_deadline()?;
		let headers = build_headers(token, accept);
		let response = self
			.transport
			.execute(HttpRequest { url, headers })
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
		self.check_deadline()?;

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
		let response = self.request(url, token.as_deref(), ACCEPT_JSON)?;

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
		let ctx = self.get_context(&snapshot.commit_oid)?;
		// Prefer the snapshot pin; fall back to the resolve-time cache.
		let tree_oid = if !snapshot.tree_oid.is_empty() {
			snapshot.tree_oid.as_str()
		} else {
			ctx.tree_oid.as_str()
		};
		let url = format!(
			"https://{}/repos/{}/{}/git/trees/{}?recursive=1",
			ctx.api_host, ctx.owner, ctx.repo, tree_oid
		);
		let response = self.request(url, ctx.token.as_deref(), ACCEPT_JSON)?;

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
				("120000", _) => (StagedEntryMode::Symlink, None),
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

		Ok(RepoTree { entries })
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
		let mut out = Vec::with_capacity(unique.len());

		for chunk in unique.chunks(concurrency) {
			let chunk_blobs = std::thread::scope(|scope| {
				let handles: Vec<_> = chunk
					.iter()
					.map(|oid| {
						let transport = Arc::clone(&self.transport);
						let deadline = self.deadline;
						let api_host = ctx.api_host;
						let owner = ctx.owner.clone();
						let repo = ctx.repo.clone();
						let token = ctx.token.clone();
						let oid = oid.clone();
						scope.spawn(move || {
							fetch_blob(
								transport.as_ref(),
								deadline,
								api_host,
								&owner,
								&repo,
								token.as_deref(),
								&oid,
							)
						})
					})
					.collect();

				let mut blobs = Vec::with_capacity(handles.len());
				for handle in handles {
					let blob = handle.join().map_err(|_| {
						GitError::rest_fallback("blob download thread panicked")
					})??;
					blobs.push(blob);
				}
				Ok::<Vec<Blob>, GitError>(blobs)
			})?;
			out.extend(chunk_blobs);
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

		// Cheap request/byte budget from tree metadata (caps live elsewhere).
		let _entry_count = selected.len();
		let _byte_budget: u64 = selected.iter().filter_map(|e| e.size).sum();

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

		let blobs = self.read_blobs(snapshot, &blob_oids)?;
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

		stage_tree_entries(staged, dest).map_err(|e| {
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
	api_host: &str,
	owner: &str,
	repo: &str,
	token: Option<&str>,
	oid: &str,
) -> Result<Blob> {
	if let Some(deadline) = deadline {
		if Instant::now() >= deadline {
			return Err(GitError::rest_fallback("deadline exceeded"));
		}
	}

	let url =
		format!("https://{api_host}/repos/{owner}/{repo}/git/blobs/{oid}");
	let headers = build_headers(token, ACCEPT_RAW);
	let response =
		transport
			.execute(HttpRequest { url, headers })
			.map_err(|e| {
				GitError::rest_fallback(format!("transport error: {e}"))
			})?;
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
