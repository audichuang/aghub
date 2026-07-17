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
use std::net::TcpListener;
use std::path::Path;
use std::process::Command;
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
