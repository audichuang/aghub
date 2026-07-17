//! Ticket 05: the `RepoFetchBackend` trait + `GixShallow` backend.
//!
//! - `resolve` returns a `RepoSnapshot` whose `commit_oid` is the branch tip and
//!   whose `tree_oid` is DISTINCT from the commit oid (OID separation, ticket 01).
//! - `read_tree` lists the tip's file entries (repo-relative, mode-tagged).
//! - `read_blobs` returns the exact stored bytes for requested blob oids.
//! - `materialize` writes selected sub-trees through the ticket-04
//!   `stage_tree_entries` materializer, producing a folder BYTE-IDENTICAL to a
//!   real gix clone — including recreating an in-folder symlink as a symlink
//!   (which is the observable proof it routed through `stage_tree_entries` and
//!   not a naive blob-dump that would write the symlink as a regular file).
//!
//! Unix-gated: the exec bit + symlink are part of the byte-identity claim.
#![cfg(unix)]

use std::collections::BTreeSet;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

use aghub_git::{GixShallow, RepoFetchBackend, SourceRef, StagedEntryMode};

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

/// Origin repo with a sub-folder skill `skills/music/` containing a SKILL.md, a
/// nested executable, and an in-folder symlink. Returns the origin path.
fn build_origin(root: &Path) -> std::path::PathBuf {
	let origin = root.join("origin");
	let skill = origin.join("skills/music");
	std::fs::create_dir_all(skill.join("scripts")).unwrap();
	std::fs::write(
		skill.join("SKILL.md"),
		b"---\nname: music\ndescription: a sub-folder skill fixture\n---\n# body\n",
	)
	.unwrap();
	let sh = skill.join("scripts/run.sh");
	std::fs::write(&sh, b"#!/bin/sh\necho hi\n").unwrap();
	std::fs::set_permissions(&sh, std::fs::Permissions::from_mode(0o755))
		.unwrap();
	// In-folder symlink → must be recreated AS a symlink (clone parity).
	std::os::unix::fs::symlink("SKILL.md", skill.join("link.md")).unwrap();
	// An unrelated top-level file, to confirm materialize takes ONLY the folder.
	std::fs::write(origin.join("UNRELATED.txt"), b"noise\n").unwrap();

	git(&["init", "-q", "-b", "main"], &origin);
	git(&["add", "-A"], &origin);
	git(&["commit", "-q", "-m", "init"], &origin);
	origin
}

/// A real gix clone + worktree checkout → ground truth for byte-identity.
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

/// (relative-path, kind:payload) for every entry under `root`, excluding `.git`.
/// A symlink records its target; a file records exec-bit + content hex — so a
/// symlink written as a regular file would NOT match a symlink target entry.
fn snapshot(root: &Path) -> BTreeSet<(String, String)> {
	let mut set = BTreeSet::new();
	collect(root, root, &mut set);
	set
}

fn collect(root: &Path, dir: &Path, set: &mut BTreeSet<(String, String)>) {
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

fn source_ref(origin: &Path) -> SourceRef {
	SourceRef {
		url: format!("file://{}", origin.display()),
		ref_: Some("main".to_string()),
	}
}

#[test]
fn resolve_returns_tip_commit_with_distinct_tree_oid() {
	let tmp = tempfile::tempdir().unwrap();
	let origin = build_origin(tmp.path());

	let backend = GixShallow::new();
	let snap = backend.resolve(&source_ref(&origin), None).unwrap();

	// commit_oid must be the branch tip; tree_oid is a DIFFERENT object id (the
	// tip's root tree) — never conflated (ticket 01 OID separation).
	let repo = gix::open(&origin).unwrap();
	let tip = repo.head_id().unwrap().detach().to_string();
	let tip_tree = repo.head_tree().unwrap().id.to_string();
	assert_eq!(snap.commit_oid, tip, "snapshot commit_oid must be the tip");
	assert_eq!(
		snap.tree_oid, tip_tree,
		"snapshot tree_oid must be the root tree"
	);
	assert_ne!(
		snap.commit_oid, snap.tree_oid,
		"commit and tree oids must stay distinct"
	);
}

#[test]
fn read_tree_lists_file_entries_with_modes() {
	let tmp = tempfile::tempdir().unwrap();
	let origin = build_origin(tmp.path());
	let backend = GixShallow::new();
	let snap = backend.resolve(&source_ref(&origin), None).unwrap();

	let tree = backend.read_tree(&snap).unwrap();
	let find = |path: &str| tree.entries.iter().find(|e| e.path == path);

	assert!(
		matches!(find("skills/music/SKILL.md"), Some(e) if e.mode == StagedEntryMode::Regular),
		"SKILL.md must be listed as a regular file"
	);
	assert!(
		matches!(find("skills/music/scripts/run.sh"), Some(e) if e.mode == StagedEntryMode::Executable),
		"run.sh must be listed as executable (mode 100755)"
	);
	assert!(
		matches!(find("skills/music/link.md"), Some(e) if e.mode == StagedEntryMode::Symlink),
		"link.md must be listed as a symlink (mode 120000)"
	);
	assert!(
		find("UNRELATED.txt").is_some(),
		"read_tree lists the whole tip"
	);
}

#[test]
fn read_blobs_returns_exact_stored_bytes() {
	let tmp = tempfile::tempdir().unwrap();
	let origin = build_origin(tmp.path());
	let backend = GixShallow::new();
	let snap = backend.resolve(&source_ref(&origin), None).unwrap();

	let tree = backend.read_tree(&snap).unwrap();
	let entry = tree
		.entries
		.iter()
		.find(|e| e.path == "skills/music/SKILL.md")
		.unwrap();

	let blobs = backend
		.read_blobs(&snap, std::slice::from_ref(&entry.oid))
		.unwrap();
	assert_eq!(blobs.len(), 1);
	assert_eq!(blobs[0].oid, entry.oid);
	assert_eq!(
		blobs[0].bytes,
		b"---\nname: music\ndescription: a sub-folder skill fixture\n---\n# body\n"
	);
}

#[test]
fn materialize_selected_folder_is_byte_identical_to_clone() {
	let tmp = tempfile::tempdir().unwrap();
	let origin = build_origin(tmp.path());

	// Ground truth: a real gix clone + checkout of the whole repo.
	let ground = tmp.path().join("ground");
	gix_checkout(&origin, &ground);

	// Under test: resolve + materialize ONLY the skills/music sub-tree.
	let backend = GixShallow::new();
	let snap = backend.resolve(&source_ref(&origin), None).unwrap();
	let dest = tmp.path().join("staged");
	backend
		.materialize(&snap, &["skills/music"], &dest)
		.unwrap();

	// The materialized folder must be byte-identical to the clone's folder —
	// exec bit preserved and the symlink recreated AS a symlink (proof it went
	// through stage_tree_entries, not a naive blob dump).
	assert_eq!(
		snapshot(&dest.join("skills/music")),
		snapshot(&ground.join("skills/music")),
		"materialized skill folder must be byte-identical to the gix clone"
	);

	// And it must hash identically under the Source-hash — the round-trip anchor.
	let h_staged =
		skill::compute_skill_folder_hash(&dest.join("skills/music")).unwrap();
	let h_ground =
		skill::compute_skill_folder_hash(&ground.join("skills/music")).unwrap();
	assert_eq!(h_staged, h_ground, "Source hash must equal the clone's");

	// ONLY the selected folder was written — the unrelated top-level file is not.
	assert!(
		!dest.join("UNRELATED.txt").exists(),
		"materialize must write only the selected sub-tree"
	);
}
