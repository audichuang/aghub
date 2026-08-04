//! Ticket 06: the `GithubRest` backend (the partial-fetch optimization).
//!
//! Every REST call goes through an injectable [`HttpTransport`] fed canned
//! GitHub API JSON, and the seam RECORDS the request set — so these tests
//! assert observable outcomes (what was and was NOT requested, which condition
//! routes to the gix fallback, byte/hash parity) with no network.
//!
//! Unix-gated (matches the sibling `gix_shallow_backend` / `stage_hash_parity`
//! suites): the exec bit + symlink recreation are part of the byte-identity
//! and no-over-fetch claims, and symlink staging is Unix-only.
#![cfg(unix)]

use std::collections::{BTreeSet, HashMap};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use aghub_git::{
	github_api_host, Credentials, GitError, GithubRest, HttpRequest,
	HttpResponse, HttpTransport, RepoFetchBackend, ReqwestTransport, SourceRef,
};

// ─── Injectable, request-recording transport seam ───

/// A canned-response transport: records every request, then delegates to a
/// per-test responder closure. Shared behind `Arc`, so concurrent blob
/// downloads all funnel their requests into one recorded log.
struct FakeTransport<F> {
	responder: F,
	recorded: Arc<Mutex<Vec<HttpRequest>>>,
}

impl<F> HttpTransport for FakeTransport<F>
where
	F: Fn(&HttpRequest) -> Result<HttpResponse, GitError> + Send + Sync,
{
	fn execute(&self, request: HttpRequest) -> Result<HttpResponse, GitError> {
		self.recorded.lock().unwrap().push(request.clone());
		(self.responder)(&request)
	}
}

/// Build a transport from a responder, returning it plus a handle to the
/// recorded request log for assertions.
fn transport(
	responder: impl Fn(&HttpRequest) -> Result<HttpResponse, GitError>
		+ Send
		+ Sync
		+ 'static,
) -> (Arc<dyn HttpTransport>, Arc<Mutex<Vec<HttpRequest>>>) {
	let recorded = Arc::new(Mutex::new(Vec::new()));
	let t: Arc<dyn HttpTransport> = Arc::new(FakeTransport {
		responder,
		recorded: recorded.clone(),
	});
	(t, recorded)
}

fn json_ok(body: impl Into<Vec<u8>>) -> HttpResponse {
	HttpResponse {
		status: 200,
		headers: vec![(
			"content-type".into(),
			"application/json; charset=utf-8".into(),
		)],
		body: body.into(),
	}
}

fn raw_ok(bytes: impl Into<Vec<u8>>) -> HttpResponse {
	HttpResponse {
		status: 200,
		headers: vec![(
			"content-type".into(),
			"application/vnd.github.raw".into(),
		)],
		body: bytes.into(),
	}
}

fn status(code: u16, headers: &[(&str, &str)]) -> HttpResponse {
	HttpResponse {
		status: code,
		headers: headers
			.iter()
			.map(|(k, v)| ((*k).to_string(), (*v).to_string()))
			.collect(),
		body: Vec::new(),
	}
}

// ─── Request classifiers (what the backend hit) ───

fn strip_query(u: &str) -> &str {
	u.split('?').next().unwrap_or(u)
}
/// A single-tip commit read: `/repos/o/r/commits/<ref>`.
fn is_commit_resolve(u: &str) -> bool {
	u.contains("/commits/")
}
/// The commit LIST / log endpoint (history): `/repos/o/r/commits` (no ref).
fn is_commit_list(u: &str) -> bool {
	strip_query(u).ends_with("/commits")
}
fn is_tree(u: &str) -> bool {
	u.contains("/git/trees/")
}
fn blob_oid(u: &str) -> Option<String> {
	strip_query(u)
		.split("/git/blobs/")
		.nth(1)
		.map(|s| s.trim_end_matches('/').to_string())
}

// ─── A canned repo: two skills + unrelated large blobs ───

const COMMIT_OID: &str = "1111111111111111111111111111111111111111";
const TREE_OID: &str = "2222222222222222222222222222222222222222";
const OID_MUSIC_SKILL: &str = "3333333333333333333333333333333333333333";
const OID_MUSIC_RUN: &str = "4444444444444444444444444444444444444444";
const OID_MUSIC_LINK: &str = "5555555555555555555555555555555555555555";
const OID_OTHER_SKILL: &str = "6666666666666666666666666666666666666666";
const OID_OTHER_BIG: &str = "7777777777777777777777777777777777777777";
const OID_README: &str = "8888888888888888888888888888888888888888";

const MUSIC_SKILL_BODY: &[u8] =
	b"---\nname: music\ndescription: a sub-folder skill fixture\n---\n# body\n";
const MUSIC_RUN_BODY: &[u8] = b"#!/bin/sh\necho hi\n";
const MUSIC_LINK_TARGET: &[u8] = b"SKILL.md";

fn commit_json() -> String {
	format!(
		r#"{{"sha":"{COMMIT_OID}","commit":{{"tree":{{"sha":"{TREE_OID}"}},"committer":{{"date":"2026-07-17T00:00:00Z"}}}}}}"#
	)
}

/// A recursive-tree response including dir entries (`type:tree`, which the
/// backend must skip), two skills, and a 50 MiB unrelated blob.
fn tree_json() -> String {
	format!(
		r#"{{"sha":"{TREE_OID}","truncated":false,"tree":[
{{"path":"README.md","mode":"100644","type":"blob","sha":"{OID_README}","size":10}},
{{"path":"skills","mode":"040000","type":"tree","sha":"deadbeef00000000000000000000000000000001"}},
{{"path":"skills/music","mode":"040000","type":"tree","sha":"deadbeef00000000000000000000000000000002"}},
{{"path":"skills/music/SKILL.md","mode":"100644","type":"blob","sha":"{OID_MUSIC_SKILL}","size":60}},
{{"path":"skills/music/scripts","mode":"040000","type":"tree","sha":"deadbeef00000000000000000000000000000003"}},
{{"path":"skills/music/scripts/run.sh","mode":"100755","type":"blob","sha":"{OID_MUSIC_RUN}","size":18}},
{{"path":"skills/music/link.md","mode":"120000","type":"blob","sha":"{OID_MUSIC_LINK}","size":8}},
{{"path":"skills/other","mode":"040000","type":"tree","sha":"deadbeef00000000000000000000000000000004"}},
{{"path":"skills/other/SKILL.md","mode":"100644","type":"blob","sha":"{OID_OTHER_SKILL}","size":40}},
{{"path":"skills/other/big.bin","mode":"100644","type":"blob","sha":"{OID_OTHER_BIG}","size":52428800}}
]}}"#
	)
}

fn blob_map() -> HashMap<String, Vec<u8>> {
	let mut m = HashMap::new();
	m.insert(OID_MUSIC_SKILL.to_string(), MUSIC_SKILL_BODY.to_vec());
	m.insert(OID_MUSIC_RUN.to_string(), MUSIC_RUN_BODY.to_vec());
	m.insert(OID_MUSIC_LINK.to_string(), MUSIC_LINK_TARGET.to_vec());
	m.insert(OID_OTHER_SKILL.to_string(), b"other skill".to_vec());
	m.insert(OID_OTHER_BIG.to_string(), vec![b'x'; 1024]);
	m.insert(OID_README.to_string(), b"readme".to_vec());
	m
}

/// The happy-path responder for the canned repo above.
fn happy_responder(
) -> impl Fn(&HttpRequest) -> Result<HttpResponse, GitError> + Send + Sync + 'static
{
	let commit = commit_json();
	let tree = tree_json();
	let blobs = blob_map();
	move |req: &HttpRequest| {
		let u = req.url.as_str();
		if let Some(oid) = blob_oid(u) {
			return match blobs.get(&oid) {
				Some(bytes) => Ok(raw_ok(bytes.clone())),
				None => Ok(status(404, &[])),
			};
		}
		if is_tree(u) {
			return Ok(json_ok(tree.clone().into_bytes()));
		}
		if is_commit_resolve(u) {
			return Ok(json_ok(commit.clone().into_bytes()));
		}
		Ok(status(404, &[]))
	}
}

fn github_source() -> SourceRef {
	SourceRef {
		url: "https://github.com/acme/skills.git".into(),
		ref_: Some("main".into()),
	}
}

// ─── CRUX: no over-fetch, no history ───

#[test]
fn materialize_one_skill_fetches_only_that_skills_blobs_and_no_history() {
	let tmp = tempfile::tempdir().unwrap();
	let (t, recorded) = transport(happy_responder());
	let backend = GithubRest::new(t);

	let snap = backend.resolve(&github_source(), None).unwrap();
	assert_eq!(snap.commit_oid, COMMIT_OID, "lock records the COMMIT oid");
	assert_eq!(snap.tree_oid, TREE_OID);
	assert_ne!(snap.commit_oid, snap.tree_oid, "OIDs stay distinct");

	let dest = tmp.path().join("staged");
	backend
		.materialize(&snap, &["skills/music"], &dest)
		.unwrap();

	let reqs = recorded.lock().unwrap().clone();

	// Blob requests are EXACTLY the selected skill's three blobs.
	let requested_blobs: BTreeSet<String> =
		reqs.iter().filter_map(|r| blob_oid(&r.url)).collect();
	let expected: BTreeSet<String> =
		[OID_MUSIC_SKILL, OID_MUSIC_RUN, OID_MUSIC_LINK]
			.iter()
			.map(|s| s.to_string())
			.collect();
	assert_eq!(
		requested_blobs, expected,
		"materialize must fetch ONLY the selected skill's blobs"
	);

	// The unrelated large blob and the other skill were NEVER requested.
	for unrelated in [OID_OTHER_BIG, OID_OTHER_SKILL, OID_README] {
		assert!(
			!requested_blobs.contains(unrelated),
			"unrelated blob {unrelated} must never be requested"
		);
	}

	// No history: exactly one single-tip resolve, no commit-list/log, no
	// commit-object (ancestor) fetch.
	assert_eq!(
		reqs.iter().filter(|r| is_commit_resolve(&r.url)).count(),
		1,
		"exactly one tip resolve"
	);
	assert!(
		reqs.iter().all(|r| !is_commit_list(&r.url)),
		"must never hit the commits LIST (history) endpoint"
	);
	assert!(
		reqs.iter().all(|r| !r.url.contains("/git/commits/")),
		"must never fetch commit objects / ancestors"
	);

	// The resolve targeted api.github.com for the right owner/repo.
	let commit_req = reqs.iter().find(|r| is_commit_resolve(&r.url)).unwrap();
	assert!(commit_req.url.contains("api.github.com"));
	assert!(commit_req.url.contains("acme/skills"));

	// And the selected folder actually materialized (only it).
	assert!(dest.join("skills/music/SKILL.md").exists());
	assert!(dest.join("skills/music/scripts/run.sh").exists());
	assert!(dest.join("skills/music/link.md").exists());
	assert!(!dest.join("skills/other").exists());
	assert!(!dest.join("README.md").exists());
}

// ─── Token-first auth ───

#[test]
fn token_is_sent_on_the_very_first_request() {
	let (t, recorded) = transport(happy_responder());
	let backend = GithubRest::new(t);
	let creds = Credentials::new("x-access-token", "ghp_TESTTOKEN");

	backend.resolve(&github_source(), Some(&creds)).unwrap();

	let reqs = recorded.lock().unwrap();
	let first = reqs.first().expect("a request was issued");
	let auth = first
		.headers
		.iter()
		.find(|(k, _)| k.eq_ignore_ascii_case("authorization"))
		.map(|(_, v)| v.as_str());
	assert!(
		matches!(auth, Some(v) if v.contains("ghp_TESTTOKEN")),
		"the FIRST request must carry the token (token-first), got {auth:?}"
	);
}

#[test]
fn anonymous_when_no_token() {
	let (t, recorded) = transport(happy_responder());
	let backend = GithubRest::new(t);
	backend.resolve(&github_source(), None).unwrap();
	let reqs = recorded.lock().unwrap();
	assert!(
		reqs[0]
			.headers
			.iter()
			.all(|(k, _)| !k.eq_ignore_ascii_case("authorization")),
		"no token -> no Authorization header"
	);
}

// ─── Fallback classification (each -> typed RestFallback, never panic) ───

#[test]
fn truncated_tree_routes_to_fallback() {
	let commit = commit_json();
	let (t, _r) = transport(move |req: &HttpRequest| {
		let u = req.url.as_str();
		if is_tree(u) {
			return Ok(json_ok(
				format!(r#"{{"sha":"{TREE_OID}","truncated":true,"tree":[]}}"#)
					.into_bytes(),
			));
		}
		Ok(json_ok(commit.clone().into_bytes()))
	});
	let backend = GithubRest::new(t);
	let snap = backend.resolve(&github_source(), None).unwrap();
	let err = backend.read_tree(&snap).unwrap_err();
	assert!(
		matches!(err, GitError::RestFallback(_)),
		"truncated tree must be a typed fallback, got {err:?}"
	);
}

#[test]
fn rate_limit_403_routes_to_fallback() {
	let (t, _r) = transport(|_req: &HttpRequest| {
		Ok(status(403, &[("x-ratelimit-remaining", "0")]))
	});
	let backend = GithubRest::new(t);
	let err = backend.resolve(&github_source(), None).unwrap_err();
	assert!(
		matches!(err, GitError::RestFallback(_)),
		"403 + x-ratelimit-remaining:0 must be a typed fallback, got {err:?}"
	);
}

#[test]
fn blob_phase_is_rejected_when_request_budget_exceeds_rate_limit() {
	const OID_SHARED: &str = "abababababababababababababababababababab";
	const OID_SUPPORT: &str = "cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd";
	let tree = format!(
		r#"{{"sha":"{TREE_OID}","truncated":false,"tree":[
{{"path":"skills/music/SKILL.md","mode":"100644",
"type":"blob","sha":"{OID_SHARED}","size":10}},
{{"path":"skills/music/copy.md","mode":"100644",
"type":"blob","sha":"{OID_SHARED}","size":10}},
{{"path":"skills/music/support.md","mode":"100644",
"type":"blob","sha":"{OID_SUPPORT}","size":20}}
]}}"#
	);
	let commit = commit_json();
	let (transport, recorded) = transport(move |request| {
		if blob_oid(&request.url).is_some() {
			return Ok(raw_ok(b"unexpected".to_vec()));
		}
		if is_tree(&request.url) {
			return Ok(HttpResponse {
				status: 200,
				headers: vec![
					("content-type".into(), "application/json".into()),
					("x-ratelimit-remaining".into(), "1".into()),
				],
				body: tree.clone().into_bytes(),
			});
		}
		Ok(json_ok(commit.clone().into_bytes()))
	});
	let backend = GithubRest::new(transport);
	let snapshot = backend.resolve(&github_source(), None).unwrap();
	let dest = tempfile::tempdir().unwrap();

	let error = backend
		.materialize(&snapshot, &["skills/music"], dest.path())
		.unwrap_err();

	assert!(matches!(error, GitError::RestFallback(_)));
	let message = error.to_string();
	assert!(message.contains("2 requests"), "{message}");
	assert!(message.contains("30 bytes"), "{message}");
	assert!(
		recorded
			.lock()
			.unwrap()
			.iter()
			.all(|request| blob_oid(&request.url).is_none()),
		"admission must happen before any blob request"
	);
}

#[test]
fn unauthorized_401_routes_to_fallback() {
	let (t, _r) = transport(|_req: &HttpRequest| Ok(status(401, &[])));
	let backend = GithubRest::new(t);
	let err = backend.resolve(&github_source(), None).unwrap_err();
	assert!(
		matches!(err, GitError::RestFallback(_)),
		"401 must be a typed fallback, got {err:?}"
	);
}

#[test]
fn malformed_body_routes_to_fallback() {
	let (t, _r) = transport(|_req: &HttpRequest| {
		Ok(json_ok(b"this is not json".to_vec()))
	});
	let backend = GithubRest::new(t);
	let err = backend.resolve(&github_source(), None).unwrap_err();
	assert!(
		matches!(err, GitError::RestFallback(_)),
		"an unexpected/malformed body must be a typed fallback, got {err:?}"
	);
}

#[test]
fn transport_network_error_routes_to_fallback() {
	let (t, _r) = transport(|_req: &HttpRequest| {
		Err(GitError::clone_failed("connection refused"))
	});
	let backend = GithubRest::new(t);
	let err = backend.resolve(&github_source(), None).unwrap_err();
	assert!(
		matches!(err, GitError::RestFallback(_)),
		"a transport/network error must be a typed fallback, got {err:?}"
	);
}

// ─── Host gate ───

#[test]
fn host_gate_accepts_only_normalized_exact_github_com() {
	assert_eq!(github_api_host("github.com"), Some("api.github.com"));
	assert_eq!(github_api_host("GitHub.com"), Some("api.github.com"));
	assert_eq!(github_api_host("github.com."), Some("api.github.com"));
	assert_eq!(github_api_host("www.github.com"), None);
	assert_eq!(github_api_host("codeload.github.com"), None);
	assert_eq!(github_api_host("api.github.com"), None);
	// GHES custom domain is NOT GitHub.
	assert_eq!(github_api_host("github.example.com"), None);
	// A loose suffix must not slip through.
	assert_eq!(github_api_host("evilgithub.com"), None);
	assert_eq!(github_api_host("gitlab.com"), None);
}

#[test]
fn non_github_host_falls_back_without_touching_the_network() {
	let (t, recorded) = transport(happy_responder());
	let backend = GithubRest::new(t);
	let src = SourceRef {
		url: "https://gitlab.com/acme/skills.git".into(),
		ref_: Some("main".into()),
	};
	let err = backend.resolve(&src, None).unwrap_err();
	assert!(
		matches!(err, GitError::RestFallback(_)),
		"a non-GitHub host must be a typed fallback, got {err:?}"
	);
	assert!(
		recorded.lock().unwrap().is_empty(),
		"the host gate must reject before issuing any request"
	);
}

// ─── Absolute deadline ───

#[test]
fn past_deadline_falls_back_without_touching_the_network() {
	let (t, recorded) = transport(happy_responder());
	let backend = GithubRest::new(t)
		.with_deadline(Instant::now() - Duration::from_secs(1));
	let err = backend.resolve(&github_source(), None).unwrap_err();
	assert!(
		matches!(err, GitError::RestFallback(_)),
		"a passed deadline must be a typed fallback, got {err:?}"
	);
	assert!(
		recorded.lock().unwrap().is_empty(),
		"a passed deadline must not issue any request"
	);
}

#[test]
fn reqwest_transport_aborts_an_in_flight_request_at_the_deadline() {
	let listener = TcpListener::bind("127.0.0.1:0").unwrap();
	let address = listener.local_addr().unwrap();
	let (accepted_tx, accepted_rx) = mpsc::channel();
	std::thread::spawn(move || {
		let (_stream, _) = listener.accept().unwrap();
		accepted_tx.send(()).unwrap();
		std::thread::sleep(Duration::from_secs(1));
	});

	let transport = ReqwestTransport::new();
	let started = Instant::now();
	let error = transport
		.execute(HttpRequest {
			url: format!("http://{address}/stall"),
			headers: Vec::new(),
			timeout: Some(Duration::from_millis(120)),
		})
		.unwrap_err();
	let elapsed = started.elapsed();

	accepted_rx
		.recv_timeout(Duration::from_millis(500))
		.expect("the request must be in flight before its deadline aborts it");
	assert!(matches!(error, GitError::RestFallback(_)));
	assert!(
		elapsed < Duration::from_millis(700),
		"an in-flight request exceeded its deadline: {elapsed:?}"
	);
}

#[test]
fn materialize_tree_and_blobs_share_one_absolute_deadline() {
	let checked = Arc::new(std::sync::atomic::AtomicBool::new(false));
	let blob_checked = Arc::clone(&checked);
	let responder = happy_responder();
	let (transport, _) = transport(move |request| {
		if is_tree(&request.url) {
			std::thread::sleep(Duration::from_millis(100));
		}
		if blob_oid(&request.url).is_some() {
			let remaining = request.timeout.expect("blob request deadline");
			assert!(
				remaining < Duration::from_millis(250),
				"blob phase reset the operation deadline: {remaining:?}"
			);
			blob_checked.store(true, std::sync::atomic::Ordering::SeqCst);
		}
		responder(request)
	});
	let backend =
		GithubRest::new(transport).with_timeout(Duration::from_millis(300));
	let snapshot = backend.resolve(&github_source(), None).unwrap();
	let dest = tempfile::tempdir().unwrap();

	backend
		.materialize(&snapshot, &["skills/music"], dest.path())
		.unwrap();

	assert!(checked.load(std::sync::atomic::Ordering::SeqCst));
}

// ─── Security failure is a HARD error, NOT a silent fallback ───

#[test]
fn out_of_root_symlink_is_a_hard_error_not_a_fallback() {
	// A skill whose only content is a symlink escaping its own folder.
	const OID_EVIL_LINK: &str = "9999999999999999999999999999999999999999";
	let commit = commit_json();
	let tree = format!(
		r#"{{"sha":"{TREE_OID}","truncated":false,"tree":[
{{"path":"skills/evil/link","mode":"120000","type":"blob","sha":"{OID_EVIL_LINK}","size":20}}
]}}"#
	);
	let (t, _r) = transport(move |req: &HttpRequest| {
		let u = req.url.as_str();
		if let Some(oid) = blob_oid(u) {
			assert_eq!(oid, OID_EVIL_LINK);
			return Ok(raw_ok(b"../../../../etc/passwd".to_vec()));
		}
		if is_tree(u) {
			return Ok(json_ok(tree.clone().into_bytes()));
		}
		Ok(json_ok(commit.clone().into_bytes()))
	});
	let backend = GithubRest::new(t);
	let snap = backend.resolve(&github_source(), None).unwrap();
	let dest = tempfile::tempdir().unwrap();
	let err = backend
		.materialize(&snap, &["skills/evil"], dest.path())
		.unwrap_err();
	assert!(
		!matches!(err, GitError::RestFallback(_)),
		"an out-of-root symlink is a security failure and must NOT be masked \
		 as a REST fallback, got {err:?}"
	);
}

// ─── Hash parity vs a real gix clone (the round-trip anchor) ───

fn git(args: &[&str], cwd: &Path) {
	let out = Command::new("git")
		.args(args)
		.current_dir(cwd)
		.env("GIT_CONFIG_GLOBAL", "/dev/null")
		.env("GIT_CONFIG_SYSTEM", "/dev/null")
		.env("GIT_AUTHOR_NAME", "t")
		.env("GIT_AUTHOR_EMAIL", "t@t")
		.env("GIT_COMMITTER_NAME", "t")
		.env("GIT_COMMITTER_EMAIL", "t@t")
		.output()
		.unwrap();
	assert!(
		out.status.success(),
		"git {args:?} failed: {}",
		String::from_utf8_lossy(&out.stderr)
	);
}

fn build_origin(root: &Path) -> std::path::PathBuf {
	let origin = root.join("origin");
	let skill = origin.join("skills/music");
	std::fs::create_dir_all(skill.join("scripts")).unwrap();
	std::fs::write(skill.join("SKILL.md"), MUSIC_SKILL_BODY).unwrap();
	let sh = skill.join("scripts/run.sh");
	std::fs::write(&sh, MUSIC_RUN_BODY).unwrap();
	use std::os::unix::fs::PermissionsExt;
	std::fs::set_permissions(&sh, std::fs::Permissions::from_mode(0o755))
		.unwrap();
	std::os::unix::fs::symlink("SKILL.md", skill.join("link.md")).unwrap();
	std::fs::write(origin.join("UNRELATED.txt"), b"noise\n").unwrap();
	git(&["init", "-q", "-b", "main"], &origin);
	git(&["add", "-A"], &origin);
	git(&["commit", "-q", "-m", "init"], &origin);
	origin
}

fn gix_checkout(origin: &Path, dest: &Path) {
	let url = format!("file://{}", origin.display());
	let (mut checkout, _) = gix::clone::PrepareFetch::new(
		url.as_str(),
		dest,
		gix::create::Kind::WithWorktree,
		Default::default(),
		Default::default(),
	)
	.unwrap()
	.fetch_then_checkout(
		gix::progress::Discard,
		&gix::interrupt::IS_INTERRUPTED,
	)
	.unwrap();
	checkout
		.main_worktree(gix::progress::Discard, &gix::interrupt::IS_INTERRUPTED)
		.unwrap();
}

/// Derive canned REST fixtures (commit JSON, tree JSON, blob map) from a REAL
/// git repo, so a materialization that drops/mangles a byte diverges from the
/// gix clone and FAILS.
fn rest_fixture_from_repo(
	origin: &Path,
) -> (String, String, HashMap<String, Vec<u8>>) {
	let repo = gix::open(origin).unwrap();
	let commit_oid = repo.head_id().unwrap().detach().to_string();
	let tree = repo.head_tree().unwrap();
	let tree_oid = tree.id.to_string();

	let mut entries: Vec<String> = Vec::new();
	let mut blobs: HashMap<String, Vec<u8>> = HashMap::new();
	walk_tree(&repo, &tree, "", &mut entries, &mut blobs);

	let commit = format!(
		r#"{{"sha":"{commit_oid}","commit":{{"tree":{{"sha":"{tree_oid}"}},"committer":{{"date":"2026-07-17T00:00:00Z"}}}}}}"#
	);
	let tree_json = format!(
		r#"{{"sha":"{tree_oid}","truncated":false,"tree":[{}]}}"#,
		entries.join(",")
	);
	(commit, tree_json, blobs)
}

fn walk_tree(
	repo: &gix::Repository,
	tree: &gix::Tree,
	prefix: &str,
	entries: &mut Vec<String>,
	blobs: &mut HashMap<String, Vec<u8>>,
) {
	for e in tree.iter() {
		let e = e.unwrap();
		let name = e.filename().to_string();
		let path = if prefix.is_empty() {
			name
		} else {
			format!("{prefix}/{name}")
		};
		let mode = e.mode();
		if mode.is_tree() {
			let sub = repo.find_tree(e.object_id()).unwrap();
			walk_tree(repo, &sub, &path, entries, blobs);
			continue;
		}
		let mode_str = if mode.is_link() {
			"120000"
		} else if format!("{:o}", mode.value()) == "100755" {
			"100755"
		} else {
			"100644"
		};
		let oid = e.object_id().to_string();
		let bytes = e.object().unwrap().data.clone();
		entries.push(format!(
			r#"{{"path":"{path}","mode":"{mode_str}","type":"blob","sha":"{oid}","size":{}}}"#,
			bytes.len()
		));
		blobs.insert(oid, bytes);
	}
}

fn snapshot(root: &Path) -> BTreeSet<(String, String)> {
	let mut set = BTreeSet::new();
	collect(root, root, &mut set);
	set
}

fn collect(root: &Path, dir: &Path, set: &mut BTreeSet<(String, String)>) {
	use std::os::unix::fs::PermissionsExt;
	for e in std::fs::read_dir(dir).unwrap() {
		let e = e.unwrap();
		let p = e.path();
		let rel = p.strip_prefix(root).unwrap().to_string_lossy().to_string();
		let ft = e.file_type().unwrap();
		if ft.is_symlink() {
			let target = std::fs::read_link(&p).unwrap();
			set.insert((rel, format!("symlink:{}", target.display())));
		} else if ft.is_dir() {
			if e.file_name() == ".git" {
				continue;
			}
			collect(root, &p, set);
		} else {
			let bytes = std::fs::read(&p).unwrap();
			let exec = std::fs::metadata(&p).unwrap().permissions().mode()
				& 0o111 != 0;
			set.insert((rel, format!("file:exec={exec}:{}", hex(&bytes))));
		}
	}
}

fn hex(bytes: &[u8]) -> String {
	use std::fmt::Write;
	bytes.iter().fold(String::new(), |mut s, b| {
		let _ = write!(s, "{b:02x}");
		s
	})
}

#[test]
fn rest_materialized_skill_is_byte_and_hash_identical_to_gix_clone() {
	let tmp = tempfile::tempdir().unwrap();
	let origin = build_origin(tmp.path());

	// Ground truth: a real gix clone + checkout.
	let ground = tmp.path().join("ground");
	gix_checkout(&origin, &ground);

	// Under test: GithubRest fed canned fixtures derived from the SAME commit.
	let (commit, tree, blobs) = rest_fixture_from_repo(&origin);
	let (t, _r) = transport(move |req: &HttpRequest| {
		let u = req.url.as_str();
		if let Some(oid) = blob_oid(u) {
			return match blobs.get(&oid) {
				Some(b) => Ok(raw_ok(b.clone())),
				None => Ok(status(404, &[])),
			};
		}
		if is_tree(u) {
			return Ok(json_ok(tree.clone().into_bytes()));
		}
		Ok(json_ok(commit.clone().into_bytes()))
	});
	let backend = GithubRest::new(t);
	let snap = backend.resolve(&github_source(), None).unwrap();
	let dest = tmp.path().join("staged");
	backend
		.materialize(&snap, &["skills/music"], &dest)
		.unwrap();

	assert_eq!(
		snapshot(&dest.join("skills/music")),
		snapshot(&ground.join("skills/music")),
		"REST-materialized skill must be byte-identical to the gix clone"
	);
	let h_staged =
		skill::compute_skill_folder_hash(&dest.join("skills/music")).unwrap();
	let h_ground =
		skill::compute_skill_folder_hash(&ground.join("skills/music")).unwrap();
	assert_eq!(
		h_staged, h_ground,
		"Source hash of the REST skill must equal the clone's"
	);
	assert_ne!(h_staged, skill::hash::EMPTY_SKILLS_LOCK_DIGEST);
}

// ─── Transport-level performance invariants ───

/// The transport must advertise gzip and hand back DECOMPRESSED bytes. Drop the
/// `gzip` feature from the workspace `reqwest` and this reads a raw gzip frame.
#[test]
fn reqwest_transport_sends_accept_encoding_and_decompresses_gzip() {
	// gzip(br#"{"gzip":"ok"}"#) as fixed bytes — no compressor dev-dep needed.
	const GZIP_BODY: [u8; 33] = [
		0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0xff, 0xab, 0x56,
		0x4a, 0xaf, 0xca, 0x2c, 0x50, 0xb2, 0x52, 0xca, 0xcf, 0x56, 0xaa, 0x05,
		0x00, 0x68, 0x08, 0x54, 0x8a, 0x0d, 0x00, 0x00, 0x00,
	];

	let listener = TcpListener::bind("127.0.0.1:0").unwrap();
	let address = listener.local_addr().unwrap();
	let (head_tx, head_rx) = mpsc::channel();
	std::thread::spawn(move || {
		let (mut stream, _) = listener.accept().unwrap();
		let (mut head, mut byte) = (Vec::new(), [0u8; 1]);
		while !head.ends_with(b"\r\n\r\n") {
			if stream.read(&mut byte).unwrap_or(0) == 0 {
				break;
			}
			head.push(byte[0]);
		}
		head_tx
			.send(String::from_utf8_lossy(&head).to_ascii_lowercase())
			.unwrap();
		let mut out = format!(
			"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
			 Content-Encoding: gzip\r\nContent-Length: {}\r\n\
			 Connection: close\r\n\r\n",
			GZIP_BODY.len()
		)
		.into_bytes();
		out.extend_from_slice(&GZIP_BODY);
		let _ = stream.write_all(&out);
	});

	let response = ReqwestTransport::new()
		.execute(HttpRequest {
			url: format!("http://{address}/tree"),
			headers: Vec::new(),
			timeout: Some(Duration::from_secs(5)),
		})
		.unwrap();

	let head = head_rx.recv_timeout(Duration::from_secs(5)).unwrap();
	assert!(
		head.contains("accept-encoding: gzip"),
		"the transport must advertise gzip; request head was:\n{head}"
	);
	assert_eq!(
		response.body,
		br#"{"gzip":"ok"}"#.to_vec(),
		"the transport must hand back DECOMPRESSED bytes"
	);
}

/// Two independently-constructed transports must land on ONE process-wide
/// client: a keep-alive server counting accepted connections sees exactly one.
/// Revert `ReqwestTransport::new` to a per-instance client and this reads 2.
#[test]
fn reqwest_transports_share_one_connection_pool() {
	let listener = TcpListener::bind("127.0.0.1:0").unwrap();
	let address = listener.local_addr().unwrap();
	let connections = Arc::new(AtomicUsize::new(0));
	let counter = Arc::clone(&connections);
	std::thread::spawn(move || {
		for stream in listener.incoming() {
			counter.fetch_add(1, SeqCst);
			let mut stream = stream.unwrap();
			std::thread::spawn(move || {
				// Byte-at-a-time so a split request head cannot flake the test.
				let (mut head, mut byte) = (Vec::new(), [0u8; 1]);
				while stream.read(&mut byte).unwrap_or(0) == 1 {
					head.push(byte[0]);
					if head.ends_with(b"\r\n\r\n") {
						head.clear();
						let _ = stream.write_all(
							b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok",
						);
					}
				}
			});
		}
	});

	for path in ["one", "two"] {
		let response = ReqwestTransport::new()
			.execute(HttpRequest {
				url: format!("http://{address}/{path}"),
				headers: Vec::new(),
				timeout: Some(Duration::from_secs(5)),
			})
			.expect("request must succeed");
		assert_eq!(response.status, 200);
	}

	assert_eq!(
		connections.load(SeqCst),
		1,
		"two transports must reuse one pooled connection, not handshake twice"
	);
}

/// A pinned snapshot's tree is immutable content, so list + preflight +
/// materialize must share ONE tree API call. Drop the cache and this reads 3.
#[test]
fn pinned_tree_is_fetched_once_across_operations() {
	let tmp = tempfile::tempdir().unwrap();
	let (t, recorded) = transport(happy_responder());
	let backend = GithubRest::new(t);
	let snap = backend.resolve(&github_source(), None).unwrap();

	backend.read_tree(&snap).unwrap();
	backend
		.materialize(&snap, &["skills/music"], &tmp.path().join("a"))
		.unwrap();
	backend
		.materialize(&snap, &["skills/other"], &tmp.path().join("b"))
		.unwrap();

	let trees = recorded
		.lock()
		.unwrap()
		.iter()
		.filter(|r| is_tree(&r.url))
		.count();
	assert_eq!(
		trees, 1,
		"an immutable pinned tree must cost exactly one API call, got {trees}"
	);
	// The cached listing still drives materialization correctly.
	assert!(tmp.path().join("b/skills/other/SKILL.md").exists());
}

/// A slow blob must not stall the ones queued behind it. Reverting to
/// `missing.chunks(concurrency)` caps in-flight starts at CONCURRENCY while the
/// first request is outstanding, and this assertion goes red.
#[test]
fn a_slow_blob_does_not_stall_the_blobs_queued_behind_it() {
	const CONCURRENCY: usize = 4;
	const BLOBS: usize = 12;

	let oids: Vec<String> = (0..BLOBS)
		.map(|i| format!("{:040x}", 0xb10b_0000u64 + i as u64))
		.collect();
	let entries: Vec<String> = oids
		.iter()
		.enumerate()
		.map(|(i, oid)| {
			format!(
				r#"{{"path":"skills/many/f{i}.md","mode":"100644","type":"blob","sha":"{oid}","size":4}}"#
			)
		})
		.collect();
	let tree = format!(
		r#"{{"sha":"{TREE_OID}","truncated":false,"tree":[{}]}}"#,
		entries.join(",")
	);
	let commit = commit_json();

	let started = Arc::new(AtomicUsize::new(0));
	let observed = Arc::new(AtomicUsize::new(0));
	let (s, o) = (Arc::clone(&started), Arc::clone(&observed));

	let (t, _recorded) = transport(move |req: &HttpRequest| {
		if blob_oid(&req.url).is_some() {
			// The FIRST blob request holds its slot and watches whether the
			// backend keeps issuing more while it is in flight.
			if s.fetch_add(1, SeqCst) == 0 {
				let stop = Instant::now() + Duration::from_secs(2);
				while s.load(SeqCst) <= CONCURRENCY && Instant::now() < stop {
					std::thread::sleep(Duration::from_millis(2));
				}
				o.store(s.load(SeqCst), SeqCst);
			}
			return Ok(raw_ok(b"body".to_vec()));
		}
		if is_tree(&req.url) {
			return Ok(json_ok(tree.clone().into_bytes()));
		}
		Ok(json_ok(commit.clone().into_bytes()))
	});

	let backend = GithubRest::new(t).with_concurrency(CONCURRENCY);
	let snap = backend.resolve(&github_source(), None).unwrap();
	let dest = tempfile::tempdir().unwrap();
	backend
		.materialize(&snap, &["skills/many"], dest.path())
		.unwrap();

	assert!(
		observed.load(SeqCst) > CONCURRENCY,
		"only {} blob requests had started while the first was still in flight \
		 (concurrency {CONCURRENCY}) — the pool is barriered, not continuously fed",
		observed.load(SeqCst)
	);
	for i in 0..BLOBS {
		assert!(dest.path().join(format!("skills/many/f{i}.md")).exists());
	}
}

/// A worker that fails must not discard what its batch already paid for: those
/// blobs were charged against the rate limit either way, so the next attempt
/// has to be served from cache rather than re-downloaded.
#[test]
fn a_failed_blob_batch_keeps_the_ones_already_paid_for() {
	let happy = happy_responder();
	let (t, recorded) = transport(move |req: &HttpRequest| {
		if blob_oid(&req.url).as_deref() == Some(OID_MUSIC_RUN) {
			return Ok(status(500, &[]));
		}
		happy(req)
	});
	// Concurrency 1 makes the tree order the fetch order, so SKILL.md is
	// downloaded before run.sh fails the batch.
	let backend = GithubRest::new(t).with_concurrency(1);
	let snap = backend.resolve(&github_source(), None).unwrap();
	let dest = tempfile::tempdir().unwrap();

	backend
		.materialize(&snap, &["skills/music"], dest.path())
		.expect_err("a 500 on one blob must fail the operation");

	let before = recorded.lock().unwrap().len();
	let blobs = backend
		.read_blobs(&snap, &[OID_MUSIC_SKILL.to_string()])
		.expect("the surviving blob is cached, so this needs no network");
	assert_eq!(blobs[0].bytes, MUSIC_SKILL_BODY);
	assert_eq!(
		recorded.lock().unwrap().len(),
		before,
		"a blob downloaded before the failure must be served from cache, \
		 not re-fetched"
	);
}

/// The rate-limit tally must be corrected by what the server actually charged.
/// Reserving the whole batch up front and never reconciling leaves the tally
/// permanently short after an aborted batch, which then refuses a retry that
/// GitHub would have allowed.
#[test]
fn an_aborted_batch_reconciles_the_rate_limit_tally_from_blob_responses() {
	// GitHub sends `remaining` and `reset` on every response; the backend only
	// trusts a count it can attribute to a window, so both must be present.
	let reset = (std::time::SystemTime::now()
		.duration_since(std::time::UNIX_EPOCH)
		.unwrap()
		.as_secs()
		+ 3600)
		.to_string();
	let happy = happy_responder();
	let (t, _recorded) = transport(move |req: &HttpRequest| {
		// 3 is exactly the batch size, so an un-reconciled reservation drains
		// the tally to 0 and refuses the single-blob retry below.
		let headers = [
			("x-ratelimit-remaining", "3"),
			("x-ratelimit-reset", reset.as_str()),
		];
		if blob_oid(&req.url).as_deref() == Some(OID_MUSIC_RUN) {
			return Ok(status(500, &headers));
		}
		let mut response = happy(req)?;
		if response.status == 200 {
			for (name, value) in headers {
				response.headers.push((name.into(), value.into()));
			}
		}
		Ok(response)
	});
	let backend = GithubRest::new(t).with_concurrency(1);
	let snap = backend.resolve(&github_source(), None).unwrap();
	let dest = tempfile::tempdir().unwrap();

	backend
		.materialize(&snap, &["skills/music"], dest.path())
		.expect_err("a 500 on one blob must fail the operation");

	// The server still reports 3 requests left, so a 1-blob read must be
	// admitted. Without reconciliation the tally would sit at 3 - 3 = 0 and
	// refuse it.
	backend
		.read_blobs(&snap, &[OID_MUSIC_LINK.to_string()])
		.expect(
			"the tally must follow the server, not the aborted reservation",
		);
}

/// Two resolves landing on the SAME commit must not throw away the first one's
/// content caches.
///
/// A source diff fetches once per ref-cohort, and cohorts routinely converge on
/// one commit: `ref=None` means "the default branch", which is usually the very
/// branch the other cohort names. `resolve` used to overwrite the cached
/// `RepoContext` wholesale, handing the second cohort empty blob/tree caches, so
/// it re-downloaded a tree and blobs the first had just fetched — measured on a
/// real host as a duplicated ~1.2s tree read plus ~1.3s of blobs, per source.
///
/// The re-resolve itself is expected (the tip must be re-checked); only the
/// content caches must survive.
#[test]
fn a_second_resolve_of_the_same_commit_keeps_the_fetched_tree() {
	let (t, recorded) = transport(|req| {
		let url = req.url.as_str();
		if is_commit_resolve(url) {
			Ok(json_ok(commit_json()))
		} else if is_tree(url) {
			Ok(json_ok(tree_json()))
		} else {
			Err(GitError::rest_fallback("unexpected request"))
		}
	});
	let backend = GithubRest::new(t);
	let source = SourceRef {
		url: "https://github.com/o/r.git".into(),
		ref_: Some("main".into()),
	};

	let first = backend.resolve(&source, None).expect("first resolve");
	backend.read_tree(&first).expect("first tree read");
	let tree_reads_after_first = recorded
		.lock()
		.unwrap()
		.iter()
		.filter(|r| is_tree(r.url.as_str()))
		.count();
	assert_eq!(tree_reads_after_first, 1, "the first read must fetch");

	// Second cohort: same repo, no explicit ref — resolves to the same commit.
	let second = backend
		.resolve(
			&SourceRef {
				url: "https://github.com/o/r.git".into(),
				ref_: None,
			},
			None,
		)
		.expect("second resolve");
	assert_eq!(
		second.commit_oid, first.commit_oid,
		"the fixture pins both cohorts to one commit"
	);
	backend.read_tree(&second).expect("second tree read");

	let tree_reads_total = recorded
		.lock()
		.unwrap()
		.iter()
		.filter(|r| is_tree(r.url.as_str()))
		.count();
	assert_eq!(
		tree_reads_total, 1,
		"the second resolve must not discard the cached tree — it re-fetched it"
	);
}
