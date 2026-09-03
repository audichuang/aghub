//! Ticket 07: the `SkillRepository` contract — the skill-aware composite that
//! owns snapshot pinning and the SINGLE REST→gix fallback route.
//!
//! Every REST call goes through the T06 injectable [`HttpTransport`] fed canned
//! GitHub API JSON, and the seam RECORDS the request set — so these tests assert
//! observable outcomes (what was and was NOT requested, which condition routes
//! to the gix fallback, the pinned commit) with no network.
//!
//! Unix-gated (matches the sibling `github_rest` / `gix_shallow_backend` suites):
//! exec bit + symlink recreation are part of the no-over-fetch / parity claims,
//! and symlink staging is Unix-only.
#![cfg(unix)]

use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::net::TcpListener;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aghub_git::{
	Blob, Credentials, GitError, GithubRest, GixShallow, HttpRequest,
	HttpResponse, HttpTransport, RepoFetchBackend, RepoSnapshot, RepoTree,
	ReqwestTransport, SourceRef as GitSourceRef,
};
use skill::SkillPath;
use skill_update::{
	FetchSelection, SkillRepoError, SkillRepository, SourceRef,
};

// ─── Injectable, request-recording transport seam (mirrors tests/github_rest) ─

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

fn strip_query(u: &str) -> &str {
	u.split('?').next().unwrap_or(u)
}
fn is_commit_resolve(u: &str) -> bool {
	u.contains("/commits/")
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

fn github_source() -> SourceRef {
	SourceRef {
		source: "https://github.com/acme/skills.git".into(),
		ref_: Some("main".into()),
	}
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

// ─── Fake backends for the fallback-owner + never-touched slots ───

/// A gix-slot backend that must NEVER be consulted (the REST path served the
/// request). Any call is a routing bug.
#[derive(Default)]
struct NeverBackend;

impl RepoFetchBackend for NeverBackend {
	fn resolve(
		&self,
		_source: &GitSourceRef,
		_auth: Option<&Credentials>,
	) -> aghub_git::Result<RepoSnapshot> {
		unreachable!("gix slot must not be reached when REST succeeds");
	}
	fn read_tree(&self, _s: &RepoSnapshot) -> aghub_git::Result<RepoTree> {
		unreachable!("gix slot must not be reached when REST succeeds");
	}
	fn read_blobs(
		&self,
		_s: &RepoSnapshot,
		_o: &[String],
	) -> aghub_git::Result<Vec<Blob>> {
		unreachable!("gix slot must not be reached when REST succeeds");
	}
	fn materialize(
		&self,
		_s: &RepoSnapshot,
		_p: &[&str],
		_d: &Path,
	) -> aghub_git::Result<()> {
		unreachable!("gix slot must not be reached when REST succeeds");
	}
}

/// A REST-slot backend that always signals `RestFallback`, and counts the calls
/// so a test can prove the fallback was attempted once (never re-decided).
#[derive(Default)]
struct AlwaysFallbackRest {
	resolve_calls: AtomicUsize,
	materialize_calls: AtomicUsize,
}

#[derive(Default)]
struct TruncatedTreeRest {
	materialize_calls: AtomicUsize,
}

impl RepoFetchBackend for TruncatedTreeRest {
	fn resolve(
		&self,
		_source: &GitSourceRef,
		_auth: Option<&Credentials>,
	) -> aghub_git::Result<RepoSnapshot> {
		Ok(RepoSnapshot {
			commit_oid: "9999999999999999999999999999999999999999".into(),
			tree_oid: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
			commit_time: None,
		})
	}

	fn read_tree(
		&self,
		_snapshot: &RepoSnapshot,
	) -> aghub_git::Result<RepoTree> {
		Err(GitError::rest_fallback("tree truncated"))
	}

	fn read_blobs(
		&self,
		_snapshot: &RepoSnapshot,
		_oids: &[String],
	) -> aghub_git::Result<Vec<Blob>> {
		Ok(Vec::new())
	}

	fn materialize(
		&self,
		_snapshot: &RepoSnapshot,
		_paths: &[&str],
		_dest: &Path,
	) -> aghub_git::Result<()> {
		self.materialize_calls.fetch_add(1, Ordering::SeqCst);
		unreachable!("list failure must not produce staging output")
	}
}
impl RepoFetchBackend for AlwaysFallbackRest {
	fn resolve(
		&self,
		_source: &GitSourceRef,
		_auth: Option<&Credentials>,
	) -> aghub_git::Result<RepoSnapshot> {
		self.resolve_calls.fetch_add(1, Ordering::SeqCst);
		Err(GitError::rest_fallback("rate limited"))
	}
	fn read_tree(&self, _s: &RepoSnapshot) -> aghub_git::Result<RepoTree> {
		Err(GitError::rest_fallback("rate limited"))
	}
	fn read_blobs(
		&self,
		_s: &RepoSnapshot,
		_o: &[String],
	) -> aghub_git::Result<Vec<Blob>> {
		Err(GitError::rest_fallback("rate limited"))
	}
	fn materialize(
		&self,
		_s: &RepoSnapshot,
		_p: &[&str],
		_d: &Path,
	) -> aghub_git::Result<()> {
		self.materialize_calls.fetch_add(1, Ordering::SeqCst);
		Err(GitError::rest_fallback("rate limited"))
	}
}

/// A gix-slot backend that serves a prebuilt local fixture dir, counting calls
/// so a test can prove the fallback landed here exactly once.
struct LocalDirBackend {
	base: std::path::PathBuf,
	resolve_calls: AtomicUsize,
	materialize_calls: AtomicUsize,
}
impl LocalDirBackend {
	fn new(base: &Path) -> Self {
		Self {
			base: base.to_path_buf(),
			resolve_calls: AtomicUsize::new(0),
			materialize_calls: AtomicUsize::new(0),
		}
	}
}
fn copy_dir(src: &Path, dst: &Path) {
	fs::create_dir_all(dst).unwrap();
	for e in fs::read_dir(src).unwrap() {
		let e = e.unwrap();
		let p = e.path();
		let d = dst.join(e.file_name());
		if p.is_dir() {
			copy_dir(&p, &d);
		} else {
			fs::copy(&p, &d).unwrap();
		}
	}
}
impl RepoFetchBackend for LocalDirBackend {
	fn resolve(
		&self,
		_source: &GitSourceRef,
		_auth: Option<&Credentials>,
	) -> aghub_git::Result<RepoSnapshot> {
		self.resolve_calls.fetch_add(1, Ordering::SeqCst);
		Ok(RepoSnapshot {
			commit_oid: "9999999999999999999999999999999999999999".into(),
			tree_oid: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
			commit_time: None,
		})
	}
	fn read_tree(&self, _s: &RepoSnapshot) -> aghub_git::Result<RepoTree> {
		Ok(RepoTree {
			entries: Vec::new(),
		})
	}
	fn read_blobs(
		&self,
		_s: &RepoSnapshot,
		_o: &[String],
	) -> aghub_git::Result<Vec<Blob>> {
		Ok(Vec::new())
	}
	fn materialize(
		&self,
		_s: &RepoSnapshot,
		paths: &[&str],
		dest: &Path,
	) -> aghub_git::Result<()> {
		self.materialize_calls.fetch_add(1, Ordering::SeqCst);
		for p in paths {
			if p.is_empty() {
				copy_dir(&self.base, dest);
			} else {
				copy_dir(&self.base.join(p), &dest.join(p));
			}
		}
		Ok(())
	}
}

// ═══════════════════ Test 1: only the selected skill is fetched ══════════════

#[test]
fn fetch_downloads_only_the_selected_skills_blobs_no_over_fetch() {
	let (t, recorded) = transport(happy_responder());
	let rest: Arc<dyn RepoFetchBackend> = Arc::new(GithubRest::new(t));
	let repo =
		SkillRepository::with_backends(Some(rest), Arc::new(NeverBackend));

	let snap = repo.resolve(&github_source(), None).unwrap();
	assert_eq!(snap.commit_oid, COMMIT_OID, "lock records the COMMIT oid");
	assert_ne!(snap.commit_oid, snap.tree_oid, "OIDs stay distinct");

	let music = SkillPath::parse("skills/music").unwrap();
	let fetched = repo.fetch(&snap, FetchSelection::Skills(&[music])).unwrap();

	// The selected skill (and only it) materialized.
	assert!(fetched.root.join("skills/music/SKILL.md").exists());
	assert!(fetched.root.join("skills/music/scripts/run.sh").exists());
	assert!(fetched.root.join("skills/music/link.md").exists());
	assert!(!fetched.root.join("skills/other").exists());
	assert!(!fetched.root.join("README.md").exists());

	let requested: BTreeSet<String> = recorded
		.lock()
		.unwrap()
		.iter()
		.filter_map(|r| blob_oid(&r.url))
		.collect();
	let expected: BTreeSet<String> =
		[OID_MUSIC_SKILL, OID_MUSIC_RUN, OID_MUSIC_LINK]
			.iter()
			.map(|s| s.to_string())
			.collect();
	assert_eq!(
		requested, expected,
		"must fetch ONLY the selected skill's blobs"
	);
	for unrelated in [OID_OTHER_BIG, OID_OTHER_SKILL, OID_README] {
		assert!(
			!requested.contains(unrelated),
			"unrelated blob {unrelated} must never be requested"
		);
	}
}

// ═══════════════════ Test 2: snapshot isolation (pinned commit) ══════════════

#[test]
fn fetch_uses_the_pinned_snapshot_not_a_moved_branch_tip() {
	const COMMIT_A: &str = "aaaa1111aaaa1111aaaa1111aaaa1111aaaa1111";
	const TREE_A: &str = "aaaa2222aaaa2222aaaa2222aaaa2222aaaa2222";
	const OID_MUSIC_A: &str = "aaaa3333aaaa3333aaaa3333aaaa3333aaaa3333";
	const COMMIT_B: &str = "bbbb1111bbbb1111bbbb1111bbbb1111bbbb1111";
	const TREE_B: &str = "bbbb2222bbbb2222bbbb2222bbbb2222bbbb2222";
	const OID_MUSIC_B: &str = "bbbb3333bbbb3333bbbb3333bbbb3333bbbb3333";

	let advanced = Arc::new(AtomicBool::new(false));
	let adv = advanced.clone();
	let (t, recorded) = transport(move |req: &HttpRequest| {
		let u = req.url.as_str();
		if let Some(oid) = blob_oid(u) {
			return Ok(raw_ok(if oid == OID_MUSIC_A {
				b"A version".to_vec()
			} else if oid == OID_MUSIC_B {
				b"B version".to_vec()
			} else {
				return Ok(status(404, &[]));
			}));
		}
		if is_tree(u) {
			let (tree_oid, music) = if u.contains(TREE_B) {
				(TREE_B, OID_MUSIC_B)
			} else {
				(TREE_A, OID_MUSIC_A)
			};
			return Ok(json_ok(
				format!(
					r#"{{"sha":"{tree_oid}","truncated":false,"tree":[
{{"path":"skills/music/SKILL.md","mode":"100644","type":"blob","sha":"{music}","size":9}}
]}}"#
				)
				.into_bytes(),
			));
		}
		if is_commit_resolve(u) {
			let (commit, tree) = if adv.load(Ordering::SeqCst) {
				(COMMIT_B, TREE_B)
			} else {
				(COMMIT_A, TREE_A)
			};
			return Ok(json_ok(
				format!(
					r#"{{"sha":"{commit}","commit":{{"tree":{{"sha":"{tree}"}},"committer":{{"date":"2026-07-17T00:00:00Z"}}}}}}"#
				)
				.into_bytes(),
			));
		}
		Ok(status(404, &[]))
	});

	let rest: Arc<dyn RepoFetchBackend> = Arc::new(GithubRest::new(t));
	let repo =
		SkillRepository::with_backends(Some(rest), Arc::new(NeverBackend));

	let snap = repo.resolve(&github_source(), None).unwrap();
	assert_eq!(snap.commit_oid, COMMIT_A);
	assert_eq!(snap.tree_oid, TREE_A);

	// Branch advances between resolve and fetch.
	advanced.store(true, Ordering::SeqCst);
	recorded.lock().unwrap().clear();

	let music = SkillPath::parse("skills/music").unwrap();
	let fetched = repo.fetch(&snap, FetchSelection::Skills(&[music])).unwrap();

	// The PINNED commit is what was fetched + recorded — never the moved tip.
	assert_eq!(
		fetched.snapshot.commit_oid, COMMIT_A,
		"fetch must record the pinned commit, not the moved branch tip"
	);
	let body =
		fs::read_to_string(fetched.root.join("skills/music/SKILL.md")).unwrap();
	assert_eq!(
		body, "A version",
		"materialized the pinned commit's content"
	);

	let reqs = recorded.lock().unwrap();
	assert!(
		reqs.iter().all(|r| !is_commit_resolve(&r.url)),
		"fetch must NOT re-resolve the moving ref"
	);
	assert!(
		reqs.iter().any(|r| r.url.contains(TREE_A)),
		"fetch must read the pinned tree oid"
	);
	assert!(
		reqs.iter().all(|r| !r.url.contains(TREE_B)),
		"fetch must never read the moved tip's tree"
	);
}

// ═══════════════ Test 3: the single central fallback owner ═══════════════════

#[test]
fn rest_fallback_routes_to_gix_once_inside_the_repository() {
	// A fixture the gix slot will serve.
	let fixture = tempfile::tempdir().unwrap();
	let music = fixture.path().join("skills/music");
	fs::create_dir_all(&music).unwrap();
	fs::write(music.join("SKILL.md"), b"gix-served body\n").unwrap();

	let rest = Arc::new(AlwaysFallbackRest::default());
	let gix = Arc::new(LocalDirBackend::new(fixture.path()));
	let repo = SkillRepository::with_backends(
		Some(rest.clone() as Arc<dyn RepoFetchBackend>),
		gix.clone() as Arc<dyn RepoFetchBackend>,
	);

	// github host => REST is TRIED first; its RestFallback must route to gix,
	// decided ONCE inside the repository (never re-decided per surface).
	let snap = repo.resolve(&github_source(), None).unwrap();
	let path = SkillPath::parse("skills/music").unwrap();
	let fetched = repo.fetch(&snap, FetchSelection::Skills(&[path])).unwrap();

	assert_eq!(
		rest.resolve_calls.load(Ordering::SeqCst),
		1,
		"REST resolve attempted exactly once"
	);
	assert_eq!(
		gix.resolve_calls.load(Ordering::SeqCst),
		1,
		"gix fallback resolve taken exactly once"
	);
	assert_eq!(
		rest.materialize_calls.load(Ordering::SeqCst),
		0,
		"a REST that already fell back must not also materialize"
	);
	assert!(
		gix.materialize_calls.load(Ordering::SeqCst) >= 1,
		"the fetch must route to the gix backend"
	);
	assert!(
		fetched.root.join("skills/music/SKILL.md").exists(),
		"the gix fallback served the selected skill"
	);
}

#[test]
fn post_resolve_rest_fallback_returns_clean_error_without_staging() {
	let rest = Arc::new(TruncatedTreeRest::default());
	let repo = SkillRepository::with_backends(
		Some(rest.clone() as Arc<dyn RepoFetchBackend>),
		Arc::new(NeverBackend),
	);
	let snapshot = repo.resolve(&github_source(), None).unwrap();
	let error = repo.list(&snapshot).unwrap_err();

	assert!(matches!(error, SkillRepoError::Network(_)));
	assert_eq!(rest.materialize_calls.load(Ordering::SeqCst), 0);
}

struct EnvRestore(Vec<(&'static str, Option<std::ffi::OsString>)>);

impl EnvRestore {
	fn set(name: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
		let old = std::env::var_os(name);
		std::env::set_var(name, value);
		Self(vec![(name, old)])
	}

	fn set_more(
		&mut self,
		name: &'static str,
		value: impl AsRef<std::ffi::OsStr>,
	) {
		self.0.push((name, std::env::var_os(name)));
		std::env::set_var(name, value);
	}
}

impl Drop for EnvRestore {
	fn drop(&mut self) {
		for (name, old) in self.0.drain(..).rev() {
			match old {
				Some(value) => std::env::set_var(name, value),
				None => std::env::remove_var(name),
			}
		}
	}
}

fn run_git(git: &Path, cwd: &Path, args: &[&str]) {
	let output = Command::new(git)
		.current_dir(cwd)
		.args(args)
		.env("GIT_CONFIG_GLOBAL", "/dev/null")
		.env("GIT_CONFIG_SYSTEM", "/dev/null")
		.env("GIT_AUTHOR_NAME", "t")
		.env("GIT_AUTHOR_EMAIL", "t@t")
		.env("GIT_COMMITTER_NAME", "t")
		.env("GIT_COMMITTER_EMAIL", "t@t")
		.output()
		.unwrap();
	assert!(
		output.status.success(),
		"git {args:?} failed: {}",
		String::from_utf8_lossy(&output.stderr)
	);
}

#[test]
fn non_github_private_host_reaches_system_git_through_skill_repository() {
	use std::os::unix::fs::PermissionsExt;

	static ENV_LOCK: Mutex<()> = Mutex::new(());
	let _lock = ENV_LOCK.lock().unwrap();
	let tmp = tempfile::tempdir().unwrap();
	let real_git = String::from_utf8(
		Command::new("sh")
			.args(["-c", "command -v git"])
			.output()
			.unwrap()
			.stdout,
	)
	.unwrap();
	let real_git = real_git.trim();
	assert!(!real_git.is_empty(), "system git is required for this test");

	let origin = tmp.path().join("origin");
	let skill = origin.join("skills/private");
	fs::create_dir_all(&skill).unwrap();
	fs::write(
		skill.join("SKILL.md"),
		b"---\nname: private\ndescription: helper-backed\n---\n",
	)
	.unwrap();
	run_git(Path::new(real_git), &origin, &["init", "-q", "-b", "main"]);
	run_git(Path::new(real_git), &origin, &["add", "-A"]);
	run_git(
		Path::new(real_git),
		&origin,
		&["commit", "-q", "-m", "init"],
	);

	let bin = tmp.path().join("bin");
	fs::create_dir(&bin).unwrap();
	let shim = bin.join("git");
	fs::write(
		&shim,
		br#"#!/bin/sh
case " $* " in
  *" https://127.0.0.1:1/acme/skills.git "*)
    last=
    for arg in "$@"; do last="$arg"; done
    : > "$AGHUB_FAKE_GIT_MARKER"
    exec "$AGHUB_REAL_GIT" clone --depth 1 \
      --branch main -- "$AGHUB_FAKE_GIT_ORIGIN" "$last"
    ;;
  *) exec "$AGHUB_REAL_GIT" "$@" ;;
esac
"#,
	)
	.unwrap();
	fs::set_permissions(&shim, fs::Permissions::from_mode(0o755)).unwrap();

	let marker = tmp.path().join("system-git-used");
	let old_path = std::env::var_os("PATH").unwrap_or_default();
	let path = std::env::join_paths(
		std::iter::once(bin.clone()).chain(std::env::split_paths(&old_path)),
	)
	.unwrap();
	let mut env = EnvRestore::set("PATH", path);
	env.set_more("AGHUB_REAL_GIT", real_git);
	env.set_more("AGHUB_FAKE_GIT_ORIGIN", &origin);
	env.set_more("AGHUB_FAKE_GIT_MARKER", &marker);

	let repo = SkillRepository::new();
	let source = SourceRef {
		source: "https://127.0.0.1:1/acme/skills.git".to_string(),
		ref_: Some("main".to_string()),
	};
	let snapshot = repo.resolve(&source, None).unwrap();
	let path = SkillPath::parse("skills/private").unwrap();
	let fetched = repo
		.fetch(&snapshot, FetchSelection::Skills(&[path]))
		.unwrap();

	assert!(marker.exists(), "system-git fallback was not reached");
	assert!(
		fetched.root.join("skills/private/SKILL.md").exists(),
		"system-git content must continue through SkillRepository::fetch"
	);
}

#[test]
fn production_repository_threads_deadline_to_first_rest_request() {
	let saw_timeout = Arc::new(AtomicBool::new(false));
	let timeout_seen = Arc::clone(&saw_timeout);
	let commit = commit_json();
	let (transport, _) = transport(move |request| {
		let timeout = request
			.timeout
			.expect("production REST requests must carry a deadline budget");
		assert!(timeout > Duration::ZERO);
		assert!(timeout <= Duration::from_secs(30));
		timeout_seen.store(true, Ordering::SeqCst);
		Ok(json_ok(commit.clone().into_bytes()))
	});
	let repo = SkillRepository::with_http_transport(transport);

	let snapshot = repo.resolve(&github_source(), None).unwrap();

	assert_eq!(snapshot.commit_oid, COMMIT_OID);
	assert!(saw_timeout.load(Ordering::SeqCst));
}

// ═══════════════ Test 4: root-level skill preflight ═════════════════════════

fn root_repo_transport(
	tree_body: String,
	blobs: HashMap<String, Vec<u8>>,
) -> (Arc<dyn HttpTransport>, Arc<Mutex<Vec<HttpRequest>>>) {
	let commit = commit_json();
	transport(move |req: &HttpRequest| {
		let u = req.url.as_str();
		if let Some(oid) = blob_oid(u) {
			return match blobs.get(&oid) {
				Some(b) => Ok(raw_ok(b.clone())),
				None => Ok(status(404, &[])),
			};
		}
		if is_tree(u) {
			return Ok(json_ok(tree_body.clone().into_bytes()));
		}
		if is_commit_resolve(u) {
			return Ok(json_ok(commit.clone().into_bytes()));
		}
		Ok(status(404, &[]))
	})
}

#[test]
fn root_skill_within_bounds_fetches_the_whole_root_folder() {
	const OID_ROOT_SKILL: &str = "cccc1111cccc1111cccc1111cccc1111cccc1111";
	const OID_ROOT_REF: &str = "cccc2222cccc2222cccc2222cccc2222cccc2222";
	let tree = format!(
		r#"{{"sha":"{TREE_OID}","truncated":false,"tree":[
{{"path":"SKILL.md","mode":"100644","type":"blob","sha":"{OID_ROOT_SKILL}","size":50}},
{{"path":"references","mode":"040000","type":"tree","sha":"deadbeef00000000000000000000000000000009"}},
{{"path":"references/guide.md","mode":"100644","type":"blob","sha":"{OID_ROOT_REF}","size":12}}
]}}"#
	);
	let mut blobs = HashMap::new();
	blobs.insert(
		OID_ROOT_SKILL.to_string(),
		b"---\nname: root-skill\ndescription: root fixture\n---\n# hi\n"
			.to_vec(),
	);
	blobs.insert(OID_ROOT_REF.to_string(), b"guide bytes\n".to_vec());

	let (t, _r) = root_repo_transport(tree, blobs);
	let rest: Arc<dyn RepoFetchBackend> = Arc::new(GithubRest::new(t));
	let repo =
		SkillRepository::with_backends(Some(rest), Arc::new(NeverBackend));

	let snap = repo.resolve(&github_source(), None).unwrap();
	let root = SkillPath::parse("").unwrap();
	assert!(root.is_root());
	let fetched = repo.fetch(&snap, FetchSelection::Skills(&[root])).unwrap();

	// The whole root folder is materialized, not just SKILL.md.
	assert!(fetched.root.join("SKILL.md").exists());
	assert!(fetched.root.join("references/guide.md").exists());
	let hash = skill::compute_skill_folder_hash(&fetched.root).unwrap();
	assert_ne!(hash, skill::hash::EMPTY_SKILLS_LOCK_DIGEST);
}

#[test]
fn oversized_root_tree_is_refused_and_downloads_no_blobs() {
	const OID_BIG: &str = "dddd1111dddd1111dddd1111dddd1111dddd1111";
	// One blob whose declared size exceeds MAX_TOTAL_BYTES (256 MiB).
	let over = skill::hash::MAX_TOTAL_BYTES + 1;
	let tree = format!(
		r#"{{"sha":"{TREE_OID}","truncated":false,"tree":[
{{"path":"SKILL.md","mode":"100644","type":"blob","sha":"3333333333333333333333333333333333333333","size":50}},
{{"path":"huge.bin","mode":"100644","type":"blob","sha":"{OID_BIG}","size":{over}}}
]}}"#
	);
	let mut blobs = HashMap::new();
	blobs.insert(
		"3333333333333333333333333333333333333333".to_string(),
		b"---\nname: root\ndescription: d\n---\n".to_vec(),
	);
	blobs.insert(OID_BIG.to_string(), vec![b'x'; 8]);

	let (t, recorded) = root_repo_transport(tree, blobs);
	let rest: Arc<dyn RepoFetchBackend> = Arc::new(GithubRest::new(t));
	let repo =
		SkillRepository::with_backends(Some(rest), Arc::new(NeverBackend));

	let snap = repo.resolve(&github_source(), None).unwrap();
	let root = SkillPath::parse("").unwrap();
	let err = repo
		.fetch(&snap, FetchSelection::Skills(&[root]))
		.unwrap_err();

	assert!(
		matches!(err, SkillRepoError::RootSkillTooLarge),
		"an over-bound root tree must be refused with ROOT_SKILL_TOO_LARGE, got {err:?}"
	);
	assert_eq!(err.code(), "ROOT_SKILL_TOO_LARGE");

	// The refusal happens BEFORE any blob download — the whole point of the
	// preflight is to never pull the pathological repo.
	let downloaded: Vec<String> = recorded
		.lock()
		.unwrap()
		.iter()
		.filter_map(|r| blob_oid(&r.url))
		.collect();
	assert!(
		downloaded.is_empty(),
		"an over-bound root must download NO blobs, downloaded: {downloaded:?}"
	);
}

struct MaterializeCountingGix {
	inner: GixShallow,
	materialize_calls: Arc<AtomicUsize>,
}

impl RepoFetchBackend for MaterializeCountingGix {
	fn resolve(
		&self,
		source: &GitSourceRef,
		auth: Option<&Credentials>,
	) -> aghub_git::Result<RepoSnapshot> {
		self.inner.resolve(source, auth)
	}

	fn read_tree(
		&self,
		snapshot: &RepoSnapshot,
	) -> aghub_git::Result<RepoTree> {
		self.inner.read_tree(snapshot)
	}

	fn read_blobs(
		&self,
		snapshot: &RepoSnapshot,
		oids: &[String],
	) -> aghub_git::Result<Vec<Blob>> {
		self.inner.read_blobs(snapshot, oids)
	}

	fn materialize(
		&self,
		snapshot: &RepoSnapshot,
		paths: &[&str],
		dest: &Path,
	) -> aghub_git::Result<()> {
		self.materialize_calls.fetch_add(1, Ordering::SeqCst);
		self.inner.materialize(snapshot, paths, dest)
	}
}

struct ChildGuard(Child);

impl Drop for ChildGuard {
	fn drop(&mut self) {
		let _ = self.0.kill();
		let _ = self.0.wait();
	}
}

/// HEAD commit of a local repo (for daemon readiness verification).
fn git_head(repo: &Path) -> String {
	let out = Command::new("git")
		.current_dir(repo)
		.args(["rev-parse", "HEAD"])
		.env("GIT_CONFIG_GLOBAL", "/dev/null")
		.env("GIT_CONFIG_SYSTEM", "/dev/null")
		.output()
		.unwrap();
	assert!(out.status.success(), "git rev-parse HEAD failed");
	String::from_utf8(out.stdout).unwrap().trim().to_string()
}

/// Bounded `git ls-remote <url> HEAD` probe: hard-killed at `timeout` (an
/// impostor that accepts TCP but stalls the git protocol cannot hang the
/// caller — git:// has no client-side timeout of its own), succeeding only
/// when the served HEAD equals `expected_head` (an impostor serving a
/// different repo cannot fake readiness).
fn probe_ls_remote(url: &str, expected_head: &str, timeout: Duration) -> bool {
	let mut probe = Command::new("git")
		.args(["ls-remote", url, "HEAD"])
		.env("GIT_CONFIG_GLOBAL", "/dev/null")
		.env("GIT_CONFIG_SYSTEM", "/dev/null")
		.stdout(Stdio::piped())
		.stderr(Stdio::null())
		.spawn()
		.unwrap();
	let deadline = Instant::now() + timeout;
	loop {
		match probe.try_wait().unwrap() {
			Some(status) => {
				if !status.success() {
					return false;
				}
				let mut out = String::new();
				use std::io::Read;
				probe
					.stdout
					.take()
					.unwrap()
					.read_to_string(&mut out)
					.unwrap();
				return out.starts_with(expected_head);
			}
			None if Instant::now() >= deadline => {
				let _ = probe.kill();
				let _ = probe.wait();
				return false;
			}
			None => std::thread::sleep(Duration::from_millis(10)),
		}
	}
}

/// Spawn a loopback `git daemon` serving `base_path` and wait until it
/// answers the git protocol for `probe_repo` with `expected_head`. The
/// bind-port-0-then-drop trick is inherently TOCTOU — another process can
/// take the port before the daemon rebinds, and a bare TCP connect could
/// reach that impostor while our child is still pre-bind — so readiness is a
/// bounded, HEAD-verified `git ls-remote` (protocol-level proof it is OUR
/// daemon serving OUR repo), and a child that lost the port race (bind
/// failure → immediate exit) is retried on a fresh port. Shared by every
/// daemon-backed test.
///
/// Execs `git-daemon` from `git --exec-path` rather than `git daemon`: the
/// latter is a dashed external, so the `git` wrapper fork+execs the daemon
/// and waits on it. `ChildGuard` would then hold the WRAPPER's pid, and its
/// SIGKILL cannot be forwarded — every clean run orphaned one daemon to init,
/// still holding its listen port forever.
fn spawn_git_daemon(
	base_path: &Path,
	probe_repo: &str,
	expected_head: &str,
) -> (ChildGuard, std::net::SocketAddr) {
	let exec_path = Command::new("git").arg("--exec-path").output().unwrap();
	assert!(exec_path.status.success(), "git --exec-path failed");
	let daemon_bin =
		Path::new(String::from_utf8(exec_path.stdout).unwrap().trim())
			.join("git-daemon");
	for _attempt in 0..5 {
		let listener = TcpListener::bind("127.0.0.1:0").unwrap();
		let address = listener.local_addr().unwrap();
		drop(listener);
		let child = Command::new(&daemon_bin)
			.args([
				"--reuseaddr".to_string(),
				"--export-all".to_string(),
				"--listen=127.0.0.1".to_string(),
				format!("--port={}", address.port()),
				format!("--base-path={}", base_path.display()),
			])
			.env("GIT_CONFIG_GLOBAL", "/dev/null")
			.env("GIT_CONFIG_SYSTEM", "/dev/null")
			.stdout(Stdio::null())
			.stderr(Stdio::null())
			.spawn()
			.unwrap();
		let mut guard = ChildGuard(child);
		let probe_url = format!("git://{address}/{probe_repo}");
		let ready_by = Instant::now() + Duration::from_secs(10);
		loop {
			if guard.0.try_wait().unwrap().is_some() {
				break; // died on startup (port stolen) → retry on a new port
			}
			if probe_ls_remote(
				&probe_url,
				expected_head,
				Duration::from_secs(2),
			) {
				return (guard, address);
			}
			if Instant::now() >= ready_by {
				panic!("git daemon did not become ready on {address}");
			}
			std::thread::sleep(Duration::from_millis(50));
		}
	}
	panic!("git daemon lost the port race 5 times in a row");
}

#[test]
fn gix_root_skill_over_limit_is_refused_before_materialization() {
	let tmp = tempfile::tempdir().unwrap();
	let origin = tmp.path().join("large-root-origin");
	fs::create_dir_all(origin.join("bulk")).unwrap();
	fs::write(
		origin.join("SKILL.md"),
		b"---\nname: root\ndescription: large root\n---\n",
	)
	.unwrap();
	for index in 0..skill::hash::MAX_FILES {
		fs::write(origin.join(format!("bulk/{index:05}")), []).unwrap();
	}
	let git = Path::new("git");
	run_git(git, &origin, &["init", "-q", "-b", "main"]);
	run_git(git, &origin, &["add", "-A"]);
	run_git(git, &origin, &["commit", "-q", "-m", "large root"]);
	let head = git_head(&origin);
	let (_daemon, address) =
		spawn_git_daemon(tmp.path(), "large-root-origin", &head);

	let materialize_calls = Arc::new(AtomicUsize::new(0));
	let backend: Arc<dyn RepoFetchBackend> = Arc::new(MaterializeCountingGix {
		inner: GixShallow::new(),
		materialize_calls: Arc::clone(&materialize_calls),
	});
	let repo = SkillRepository::with_backends(None, backend);
	let source = SourceRef {
		source: format!("git://{address}/large-root-origin"),
		ref_: Some("main".to_string()),
	};
	let snapshot = repo.resolve(&source, None).unwrap();
	let root = SkillPath::parse("").unwrap();

	let error = repo
		.fetch(&snapshot, FetchSelection::Skills(&[root]))
		.unwrap_err();

	assert_eq!(error.code(), "ROOT_SKILL_TOO_LARGE");
	assert_eq!(
		materialize_calls.load(Ordering::SeqCst),
		0,
		"the over-bound gix root must be refused before any write"
	);
}

// ═══ gix over a REAL git transport: content round-trip + upstream advance ════
//
// The one GixShallow SUCCESS-path test over a real TCP transport; the other
// gix-slot tests fake the backend or exercise a refusal.
#[test]
fn gix_daemon_roundtrip_fetches_content_and_sees_upstream_advance() {
	let tmp = tempfile::tempdir().unwrap();
	let origin = tmp.path().join("daemon-origin");
	let skill_dir = origin.join("skills/hello");
	fs::create_dir_all(&skill_dir).unwrap();
	fs::write(
		skill_dir.join("SKILL.md"),
		b"---\nname: hello\ndescription: roundtrip\n---\n\nv1 body\n",
	)
	.unwrap();
	let git = Path::new("git");
	run_git(git, &origin, &["init", "-q", "-b", "main"]);
	run_git(git, &origin, &["add", "-A"]);
	run_git(git, &origin, &["commit", "-q", "-m", "v1"]);

	let head = git_head(&origin);
	let (_daemon, address) =
		spawn_git_daemon(tmp.path(), "daemon-origin", &head);

	let repo =
		SkillRepository::with_backends(None, Arc::new(GixShallow::new()));
	let source = SourceRef {
		source: format!("git://{address}/daemon-origin"),
		ref_: Some("main".to_string()),
	};
	let hello = SkillPath::parse("skills/hello").unwrap();

	// v1: resolve pins the tip, fetch materializes the skill's content.
	let v1 = repo.resolve(&source, None).unwrap();
	let fetched = repo
		.fetch(&v1, FetchSelection::Skills(std::slice::from_ref(&hello)))
		.unwrap();
	let content =
		fs::read_to_string(fetched.root.join("skills/hello/SKILL.md")).unwrap();
	assert!(content.contains("v1 body"), "v1 content must materialize");

	// Upstream advances: same source must re-resolve to the NEW tip and
	// fetch the NEW content (the apply-update chain's substrate).
	fs::write(
		skill_dir.join("SKILL.md"),
		b"---\nname: hello\ndescription: roundtrip\n---\n\nv2 body\n",
	)
	.unwrap();
	run_git(git, &origin, &["commit", "-q", "-a", "-m", "v2"]);

	let v2 = repo.resolve(&source, None).unwrap();
	assert_ne!(
		v2.commit_oid, v1.commit_oid,
		"re-resolve must see the advanced upstream tip, not a cached one"
	);
	let fetched2 = repo
		.fetch(&v2, FetchSelection::Skills(std::slice::from_ref(&hello)))
		.unwrap();
	let content2 =
		fs::read_to_string(fetched2.root.join("skills/hello/SKILL.md"))
			.unwrap();
	assert!(
		content2.contains("v2 body"),
		"the new snapshot must materialize the updated content"
	);
}

#[test]
fn list_filters_over_depth_skill_metadata_before_blob_requests() {
	const OID_SHALLOW: &str = "eeee1111eeee1111eeee1111eeee1111eeee1111";
	const OID_DEEP: &str = "eeee2222eeee2222eeee2222eeee2222eeee2222";
	let deep_folder = "a/b/c/d/e/f/g/h/i/j/k";
	let tree = format!(
		r#"{{"sha":"{TREE_OID}","truncated":false,"tree":[
{{"path":"skills/ok/SKILL.md","mode":"100644",
"type":"blob","sha":"{OID_SHALLOW}","size":45}},
{{"path":"{deep_folder}/SKILL.md","mode":"100644",
"type":"blob","sha":"{OID_DEEP}","size":45}}
]}}"#
	);
	let commit = commit_json();
	let (transport, recorded) = transport(move |request| {
		if let Some(oid) = blob_oid(&request.url) {
			let name = if oid == OID_SHALLOW { "ok" } else { "deep" };
			return Ok(raw_ok(
				format!("---\nname: {name}\ndescription: test\n---\n")
					.into_bytes(),
			));
		}
		if is_tree(&request.url) {
			return Ok(json_ok(tree.clone().into_bytes()));
		}
		if is_commit_resolve(&request.url) {
			return Ok(json_ok(commit.clone().into_bytes()));
		}
		Ok(status(404, &[]))
	});
	let rest: Arc<dyn RepoFetchBackend> = Arc::new(GithubRest::new(transport));
	let repo =
		SkillRepository::with_backends(Some(rest), Arc::new(NeverBackend));
	let snapshot = repo.resolve(&github_source(), None).unwrap();

	let catalog = repo.list(&snapshot).unwrap();

	assert_eq!(catalog.skills.len(), 1);
	assert_eq!(catalog.skills[0].name, "ok");
	let requested: Vec<String> = recorded
		.lock()
		.unwrap()
		.iter()
		.filter_map(|request| blob_oid(&request.url))
		.collect();
	assert_eq!(
		requested,
		vec![OID_SHALLOW.to_string()],
		"an over-depth SKILL.md must never enter the blob request set"
	);
}

#[test]
fn catalog_snapshot_fetches_only_discovered_skills_and_changelog() {
	const OID_A_SKILL: &str = "ffff1111ffff1111ffff1111ffff1111ffff1111";
	const OID_A_SUPPORT: &str = "ffff2222ffff2222ffff2222ffff2222ffff2222";
	const OID_B_SKILL: &str = "ffff3333ffff3333ffff3333ffff3333ffff3333";
	const OID_CHANGELOG: &str = "ffff4444ffff4444ffff4444ffff4444ffff4444";
	const OID_HUGE: &str = "ffff5555ffff5555ffff5555ffff5555ffff5555";
	let huge = skill::hash::MAX_TOTAL_BYTES + 1;
	let tree = format!(
		r#"{{"sha":"{TREE_OID}","truncated":false,"tree":[
{{"path":"skills/a/SKILL.md","mode":"100644",
"type":"blob","sha":"{OID_A_SKILL}","size":43}},
{{"path":"skills/a/reference.md","mode":"100644",
"type":"blob","sha":"{OID_A_SUPPORT}","size":9}},
{{"path":"skills/b/SKILL.md","mode":"100644",
"type":"blob","sha":"{OID_B_SKILL}","size":43}},
{{"path":"CHANGELOG.md","mode":"100644",
"type":"blob","sha":"{OID_CHANGELOG}","size":10}},
{{"path":"unrelated/huge.bin","mode":"100644",
"type":"blob","sha":"{OID_HUGE}","size":{huge}}}
]}}"#
	);
	let mut blobs = HashMap::new();
	blobs.insert(
		OID_A_SKILL.to_string(),
		b"---\nname: a\ndescription: skill a\n---\n".to_vec(),
	);
	blobs.insert(OID_A_SUPPORT.to_string(), b"reference".to_vec());
	blobs.insert(
		OID_B_SKILL.to_string(),
		b"---\nname: b\ndescription: skill b\n---\n".to_vec(),
	);
	blobs.insert(OID_CHANGELOG.to_string(), b"changelog\n".to_vec());
	blobs.insert(OID_HUGE.to_string(), b"must not fetch".to_vec());
	let commit = commit_json();
	let (transport, recorded) = transport(move |request| {
		if let Some(oid) = blob_oid(&request.url) {
			return blobs
				.get(&oid)
				.cloned()
				.map(raw_ok)
				.map(Ok)
				.unwrap_or_else(|| Ok(status(404, &[])));
		}
		if is_tree(&request.url) {
			return Ok(json_ok(tree.clone().into_bytes()));
		}
		if is_commit_resolve(&request.url) {
			return Ok(json_ok(commit.clone().into_bytes()));
		}
		Ok(status(404, &[]))
	});
	let rest: Arc<dyn RepoFetchBackend> = Arc::new(GithubRest::new(transport));
	let repo =
		SkillRepository::with_backends(Some(rest), Arc::new(NeverBackend));
	let snapshot = repo.resolve(&github_source(), None).unwrap();

	let fetched = repo
		.fetch(&snapshot, FetchSelection::CatalogSnapshot)
		.unwrap();

	assert!(fetched.root.join("skills/a/SKILL.md").exists());
	assert!(fetched.root.join("skills/a/reference.md").exists());
	assert!(fetched.root.join("skills/b/SKILL.md").exists());
	assert!(fetched.root.join("CHANGELOG.md").exists());
	assert!(!fetched.root.join("unrelated").exists());
	let requested: Vec<String> = recorded
		.lock()
		.unwrap()
		.iter()
		.filter_map(|request| blob_oid(&request.url))
		.collect();
	let expected = vec![
		OID_A_SKILL.to_string(),
		OID_B_SKILL.to_string(),
		OID_A_SUPPORT.to_string(),
		OID_CHANGELOG.to_string(),
	];
	assert_eq!(
		requested.len(),
		expected.len(),
		"catalog blobs must not be requested twice: {requested:?}"
	);
	assert_eq!(
		requested.into_iter().collect::<BTreeSet<_>>(),
		expected.into_iter().collect(),
		"catalog fetch must request exactly skill-folder + CHANGELOG blobs"
	);
	// One pinned snapshot = one tree listing, shared by the catalog scan and
	// the materialize that follows it.
	let trees = recorded
		.lock()
		.unwrap()
		.iter()
		.filter(|request| is_tree(&request.url))
		.count();
	assert_eq!(
		trees, 1,
		"catalog fetch must read the pinned tree exactly once"
	);
}

#[test]
#[ignore = "network"]
fn real_github_rest_catalog_and_install_are_pinned_and_hashed() {
	const FIXTURE_COMMIT: &str = "777599e1159e401b11ce4c8a57c20f09a8f1596e";
	const SKILL_FOLDER: &str = "skills/find-skills";
	const SKILL_HASH: &str =
		"913b9d37d0d54047dd65222bb8c67b2bf04e3cb87dcad1729068d7a8b2c8c396";

	let rest: Arc<dyn RepoFetchBackend> = Arc::new(
		GithubRest::new(Arc::new(ReqwestTransport::new()))
			.with_timeout(Duration::from_secs(30)),
	);
	// This E2E requires a token. The anonymous GitHub API (60/hr) is routinely
	// exhausted, which (correctly) routes to the gix fallback — so an anonymous
	// run would panic the NeverBackend slot instead of proving the REST path.
	// With a token the REST path has 5000/hr. No token → skip (not fail).
	let token = std::env::var("GITHUB_TOKEN")
		.ok()
		.or_else(|| std::env::var("GH_TOKEN").ok())
		.filter(|t| !t.trim().is_empty());
	let Some(token) = token else {
		eprintln!(
			"skipping real_github_rest E2E: set GITHUB_TOKEN/GH_TOKEN to run \
			 (anonymous GitHub API rate-limits and would route to fallback)"
		);
		return;
	};

	let repo =
		SkillRepository::with_backends(Some(rest), Arc::new(NeverBackend));
	let source = SourceRef {
		source: "https://github.com/vercel-labs/skills.git".to_string(),
		ref_: Some(FIXTURE_COMMIT.to_string()),
	};

	let snapshot = repo.resolve(&source, Some(token.as_str())).unwrap();
	assert_eq!(snapshot.commit_oid, FIXTURE_COMMIT);
	let catalog = repo.list(&snapshot).unwrap();
	let skill = catalog
		.skills
		.iter()
		.find(|skill| skill.skill_path.as_str() == SKILL_FOLDER)
		.expect("the pinned fixture must expose find-skills");
	let selected = SkillPath::parse(&skill.skill_path).unwrap();
	let fetched = repo
		.fetch(
			&snapshot,
			FetchSelection::Skills(std::slice::from_ref(&selected)),
		)
		.unwrap();

	assert_eq!(fetched.snapshot.commit_oid, FIXTURE_COMMIT);
	let folder = fetched.root.join(SKILL_FOLDER);
	let content = fs::read_to_string(folder.join("SKILL.md")).unwrap();
	assert!(content.contains("name: find-skills"));
	let hash = skill::compute_skill_folder_hash(&folder).unwrap();
	assert_eq!(hash, SKILL_HASH);
}

// ══════════ The update-check preflight: a tip must not cost a fetch ══════════

/// The whole point of the preflight is that asking "has upstream moved?" costs
/// less than the fetch it may avoid. On github hosts that is ONE REST request —
/// no tree, no blobs, and no git ref advertisement (a fresh TCP+TLS connection
/// plus the full heads+tags listing, measured at ~0.6s per source, is what this
/// path exists to escape).
#[test]
fn resolve_tip_costs_one_rest_request_and_downloads_no_objects() {
	let (t, recorded) = transport(happy_responder());
	let rest: Arc<dyn RepoFetchBackend> = Arc::new(GithubRest::new(t));
	let repo =
		SkillRepository::with_backends(Some(rest), Arc::new(NeverBackend));

	let (tip, pinned) = repo.resolve_tip(&github_source(), None).unwrap();

	assert_eq!(
		tip, COMMIT_OID,
		"the tip is the COMMIT oid the lock records"
	);
	assert_eq!(
		pinned.as_ref().map(|p| p.commit_oid()),
		Some(COMMIT_OID),
		"the REST path must hand back a claim naming that same tip"
	);
	let urls: Vec<String> = recorded
		.lock()
		.unwrap()
		.iter()
		.map(|r| r.url.clone())
		.collect();
	assert_eq!(
		urls.len(),
		1,
		"a tip must cost exactly one request: {urls:?}"
	);
	assert!(
		is_commit_resolve(&urls[0]),
		"the one request must be the commit resolve: {urls:?}"
	);
	assert!(
		!urls.iter().any(|u| is_tree(u) || blob_oid(u).is_some()),
		"the preflight must not download tree or blob objects: {urls:?}"
	);
}

/// A gix-slot backend that counts `resolve` instead of forbidding it, so a test
/// can prove the preflight never reaches it (and say so with a count, not a
/// panic in another thread's stack).
#[derive(Default)]
struct ResolveCountingGix {
	resolve_calls: AtomicUsize,
}

impl RepoFetchBackend for ResolveCountingGix {
	fn resolve(
		&self,
		_source: &GitSourceRef,
		_auth: Option<&Credentials>,
	) -> aghub_git::Result<RepoSnapshot> {
		self.resolve_calls.fetch_add(1, Ordering::SeqCst);
		Ok(RepoSnapshot {
			commit_oid: COMMIT_OID.into(),
			tree_oid: TREE_OID.into(),
			commit_time: None,
		})
	}
	fn read_tree(&self, _s: &RepoSnapshot) -> aghub_git::Result<RepoTree> {
		Ok(RepoTree {
			entries: Vec::new(),
		})
	}
	fn read_blobs(
		&self,
		_s: &RepoSnapshot,
		_o: &[String],
	) -> aghub_git::Result<Vec<Blob>> {
		Ok(Vec::new())
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

/// Off the REST path the tip must come from a ref advertisement, NOT from the gix
/// backend's `resolve` — that one answers by performing the depth-1 object fetch,
/// so routing the preflight through it would pay the exact cost the preflight
/// exists to avoid, on every non-github host.
///
/// The remote here is a closed port, so the advertisement attempt fails fast with
/// no network dependency. Two assertions together pin the path: the gix slot was
/// never consulted, AND the error is the one our own advertisement wrapper
/// produces — which a `resolve_tip` that skipped the advertisement and returned a
/// bare Network error could not produce.
#[test]
fn resolve_tip_off_the_rest_path_advertises_and_never_fetches_through_gix() {
	let gix = Arc::new(ResolveCountingGix::default());
	let repo = SkillRepository::with_backends(
		None,
		gix.clone() as Arc<dyn RepoFetchBackend>,
	);
	let source = SourceRef {
		source: "https://127.0.0.1:1/acme/skills.git".to_string(),
		ref_: Some("main".to_string()),
	};

	let result = repo.resolve_tip(&source, None);

	assert_eq!(
		gix.resolve_calls.load(Ordering::SeqCst),
		0,
		"the preflight must not resolve through the object-fetching gix backend"
	);
	let detail = match &result {
		Err(SkillRepoError::Network(detail)) => detail.clone(),
		other => panic!("expected a soft network failure, got {other:?}"),
	};
	assert!(
		detail.contains("Git clone failed"),
		"the failure must come from the ref advertisement actually being \
		 attempted (our `discover_remote_refs` wrapper's wording), not from \
		 short-circuiting to a generic error: {detail}"
	);
}

/// A non-https remote is refused BY the advertisement layer, so it never reaches
/// the gix backend's object-fetching resolve either. Pins that the "no objects"
/// property does not quietly depend on the remote being reachable.
#[test]
fn resolve_tip_refuses_a_non_https_remote_without_touching_gix() {
	let gix = Arc::new(ResolveCountingGix::default());
	let repo = SkillRepository::with_backends(
		None,
		gix.clone() as Arc<dyn RepoFetchBackend>,
	);
	let source = SourceRef {
		source: "git://127.0.0.1:1/acme".to_string(),
		ref_: Some("main".to_string()),
	};

	let result = repo.resolve_tip(&source, None);

	assert_eq!(gix.resolve_calls.load(Ordering::SeqCst), 0);
	let detail = match &result {
		Err(SkillRepoError::Network(detail)) => detail.clone(),
		other => panic!("expected a soft network failure, got {other:?}"),
	};
	assert!(
		detail.contains("Not an HTTPS URL"),
		"the advertisement layer's own https guard must be what refuses it: \
		 {detail}"
	);
}

/// The preflight and the fetch are one logical question, and on the REST path the
/// tip the preflight paid for is the tip the fetch needs. Buying it twice doubled
/// this flow's spend against GitHub's 60/hour anonymous budget — the difference
/// between a working check and `uncheckable/network`.
#[test]
fn a_pinned_claim_fetches_without_buying_the_tip_again() {
	let (t, recorded) = transport(happy_responder());
	let rest: Arc<dyn RepoFetchBackend> = Arc::new(GithubRest::new(t));
	let repo =
		SkillRepository::with_backends(Some(rest), Arc::new(NeverBackend));

	let (tip, pinned) = repo.resolve_tip(&github_source(), None).unwrap();
	let pinned = pinned.expect("the REST path yields a claim");
	let music = SkillPath::parse("skills/music").unwrap();
	let fetched = repo
		.fetch_pinned(&pinned, FetchSelection::Skills(&[music]))
		.unwrap();

	assert_eq!(fetched.snapshot.commit_oid, tip);
	assert!(fetched.root.join("skills/music/SKILL.md").exists());
	let commits = recorded
		.lock()
		.unwrap()
		.iter()
		.filter(|r| is_commit_resolve(&r.url))
		.count();
	assert_eq!(
		commits, 1,
		"the tip is bought once by the preflight and never again"
	);
}

/// The composite half of the same-commit collision: `GithubRest` declining a
/// colliding identity is only safe because THIS routes it to gix, so the second
/// source still gets its content under its own credentials.
#[test]
fn a_same_commit_identity_collision_falls_back_to_gix() {
	let (t, _recorded) = transport(happy_responder());
	let rest: Arc<dyn RepoFetchBackend> = Arc::new(GithubRest::new(t));
	let gix = Arc::new(ResolveCountingGix::default());
	let repo = SkillRepository::with_backends(
		Some(rest),
		gix.clone() as Arc<dyn RepoFetchBackend>,
	);

	// First source takes the REST slot for this commit.
	repo.resolve(&github_source(), None).unwrap();
	assert_eq!(gix.resolve_calls.load(Ordering::SeqCst), 0);

	// A different repo on the SAME commit must not inherit that context.
	let fork = SourceRef {
		source: "https://github.com/forkco/skills.git".to_string(),
		ref_: Some("main".to_string()),
	};
	let snapshot = repo.resolve(&fork, Some("ghp_FORK")).unwrap();

	assert_eq!(
		gix.resolve_calls.load(Ordering::SeqCst),
		1,
		"the declined source must be served by gix, not refused outright"
	);
	assert_eq!(snapshot.commit_oid, COMMIT_OID);
}

/// A commit responder whose tip MOVES on demand, so a test can interleave two
/// observations of one coordinate the way two concurrent groups do.
fn moving_tip_responder(
	moved: Arc<AtomicBool>,
) -> impl Fn(&HttpRequest) -> Result<HttpResponse, GitError> + Send + Sync + 'static
{
	const COMMIT_TWO: &str = "9999999999999999999999999999999999999999";
	const TREE_TWO: &str = "8888888888888888888888888888888888888888";
	let first = commit_json();
	let second = format!(
		r#"{{"sha":"{COMMIT_TWO}","commit":{{"tree":{{"sha":"{TREE_TWO}"}}}}}}"#
	);
	move |req: &HttpRequest| {
		if is_commit_resolve(&req.url) {
			let body = if moved.load(Ordering::SeqCst) {
				second.clone()
			} else {
				first.clone()
			};
			return Ok(json_ok(body.into_bytes()));
		}
		Ok(status(404, &[]))
	}
}

/// THE reason the handoff is a value and not a coordinate-keyed cache.
///
/// Two concurrent groups can share one coordinate — `acme/skills`,
/// `github:acme/skills` and `https://github.com/acme/skills.git` all normalize to
/// the same URL while `check_updates` groups by the RAW source string. With a
/// `coordinate -> snapshot` map, a slow observation of an older tip overwrote a
/// newer one, and the newer group's fetch then materialized the tip nobody had
/// judged and reported `UpToDate` for a source that had moved on.
///
/// A claim cannot be overwritten by anyone: it names its own snapshot.
#[test]
fn a_claim_survives_a_later_observation_of_a_different_tip() {
	let moved = Arc::new(AtomicBool::new(false));
	let (t, _recorded) = transport(moving_tip_responder(moved.clone()));
	let rest: Arc<dyn RepoFetchBackend> = Arc::new(GithubRest::new(t));
	let repo =
		SkillRepository::with_backends(Some(rest), Arc::new(NeverBackend));

	// Group A observes the tip and keeps its claim.
	let (first_tip, pinned) = repo.resolve_tip(&github_source(), None).unwrap();
	let pinned = pinned.expect("the REST path yields a claim");

	// Upstream moves; group B observes the NEW tip on the same coordinate.
	moved.store(true, Ordering::SeqCst);
	let (second_tip, _) = repo.resolve_tip(&github_source(), None).unwrap();
	assert_ne!(second_tip, first_tip, "fixture premise: the tip moved");

	assert_eq!(
		pinned.commit_oid(),
		first_tip,
		"A's claim must still name the tip A observed, not B's"
	);
	assert_eq!(
		pinned.snapshot().commit_oid,
		first_tip,
		"and the snapshot it pins must be that same commit"
	);
}
