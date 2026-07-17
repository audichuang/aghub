//! CRUX (ticket 05): the treeless/bare fetch used by every update/install path
//! must be SHALLOW (depth-1) — history is dropped everywhere. Proven with a
//! local-remote fixture: after fetching a repo whose tip has a parent, the
//! parent commit object must be UNREACHABLE from the fetched object DB (not
//! merely "the fetch succeeded"). FAILS if the fetch is full-history.

use std::path::Path;
use std::process::Command;

fn git(args: &[&str], cwd: &Path) -> String {
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
	String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A local origin repo on branch `main` with TWO commits. Returns
/// `(origin_path, parent_commit_hex, tip_commit_hex)`.
fn two_commit_origin(root: &Path) -> (std::path::PathBuf, String, String) {
	let origin = root.join("origin");
	std::fs::create_dir_all(&origin).unwrap();
	git(&["init", "-q", "-b", "main"], &origin);
	std::fs::write(origin.join("a.txt"), b"one\n").unwrap();
	git(&["add", "-A"], &origin);
	git(&["commit", "-q", "-m", "c1"], &origin);
	let parent = git(&["rev-parse", "HEAD"], &origin);
	std::fs::write(origin.join("a.txt"), b"two\n").unwrap();
	git(&["add", "-A"], &origin);
	git(&["commit", "-q", "-m", "c2"], &origin);
	let tip = git(&["rev-parse", "HEAD"], &origin);
	(origin, parent, tip)
}

#[test]
fn fetch_ref_to_temp_is_shallow_depth1_parent_unreachable() {
	let tmp = tempfile::tempdir().unwrap();
	let (origin, parent, tip) = two_commit_origin(tmp.path());
	let url = format!("file://{}", origin.display());

	let (bare, resolved) =
		aghub_git::fetch_ref_to_temp(&url, Some("main"), None, None).unwrap();
	let repo = gix::open(bare.path()).unwrap();

	// The tip we asked for is present and is the branch tip.
	assert_eq!(resolved.to_string(), tip, "resolved oid must be the tip");
	let tip_oid = gix::ObjectId::from_hex(tip.as_bytes()).unwrap();
	assert!(
		repo.find_object(tip_oid).is_ok(),
		"tip commit must be present after fetch"
	);

	// The crux: the PARENT commit object was never fetched (depth-1 boundary).
	// This assertion FAILS on a full-history fetch, where the parent is present.
	let parent_oid = gix::ObjectId::from_hex(parent.as_bytes()).unwrap();
	assert!(
		repo.find_object(parent_oid).is_err(),
		"parent commit MUST be unreachable under a depth-1 shallow fetch \
		 (present => the fetch is still full-history)"
	);
}
