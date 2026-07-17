//! HASH-PARITY GOLDEN (ticket 04, the crux): the Source-staging materializer
//! must produce a skill folder byte-identical to — and hashing identically
//! under `compute_skill_folder_hash` to — a REAL gix clone/checkout of the same
//! committed content. The ground truth is gix's OWN worktree checkout, NOT the
//! materializer's own output, so a dropped/mangled file or a diverging hash
//! FAILS. Unix-gated: exec bit + symlink are part of the byte-identity claim.
#![cfg(unix)]

use std::collections::BTreeSet;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

use aghub_git::{stage_tree_entries, StagedEntry, StagedEntryMode};

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

/// Build a fixture repo with a nested dir, an executable file, and an in-root
/// symlink, then commit it. Returns the origin worktree path.
fn build_fixture(root: &Path) -> std::path::PathBuf {
	let origin = root.join("origin");
	std::fs::create_dir_all(origin.join("scripts")).unwrap();
	std::fs::create_dir_all(origin.join("references")).unwrap();
	std::fs::write(
		origin.join("SKILL.md"),
		b"---\nname: fixture\ndescription: golden materializer parity fixture\n---\n# body\n",
	)
	.unwrap();
	std::fs::write(origin.join("references/notes.md"), b"reference notes\n")
		.unwrap();
	let sh = origin.join("scripts/run.sh");
	std::fs::write(&sh, b"#!/bin/sh\necho hi\n").unwrap();
	std::fs::set_permissions(&sh, std::fs::Permissions::from_mode(0o755))
		.unwrap();
	// In-root symlink → must be recreated as a symlink, matching the clone.
	std::os::unix::fs::symlink("SKILL.md", origin.join("link.md")).unwrap();

	git(&["init", "-q", "-b", "main"], &origin);
	git(&["add", "-A"], &origin);
	git(&["commit", "-q", "-m", "init"], &origin);
	origin
}

/// A REAL gix clone + worktree checkout of `origin` → ground truth.
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

/// Read the tip tree of `origin` into `(path, bytes, mode)` staging entries.
fn read_tree_entries(origin: &Path) -> Vec<StagedEntry> {
	let repo = gix::open(origin).unwrap();
	let tree = repo.head_tree().unwrap();
	let mut out = Vec::new();
	walk(&repo, &tree, "", &mut out);
	out
}

fn walk(
	repo: &gix::Repository,
	tree: &gix::Tree,
	prefix: &str,
	out: &mut Vec<StagedEntry>,
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
			walk(repo, &sub, &path, out);
			continue;
		}
		let staged_mode = if mode.is_link() {
			StagedEntryMode::Symlink
		} else if format!("{:o}", mode.value()) == "100755" {
			StagedEntryMode::Executable
		} else {
			StagedEntryMode::Regular
		};
		let bytes = e.object().unwrap().data.clone();
		out.push(StagedEntry {
			path,
			bytes,
			mode: staged_mode,
		});
	}
}

/// Recursively collect (relative-path, kind, payload) for every entry under
/// `root`, excluding `.git`. kind+payload capture what byte-identity means:
/// a regular file's bytes + exec bit, or a symlink's target.
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

#[test]
fn materializer_output_is_byte_identical_and_hash_identical_to_gix_clone() {
	let tmp = tempfile::tempdir().unwrap();
	let origin = build_fixture(tmp.path());

	// Ground truth: gix's own checkout.
	let ground = tmp.path().join("ground");
	gix_checkout(&origin, &ground);

	// Under test: our materializer fed the tip tree entries.
	let staged = tmp.path().join("staged");
	stage_tree_entries(read_tree_entries(&origin), &[""], &staged).unwrap();

	// Byte-identical (file set, contents, exec bits, symlink targets).
	assert_eq!(
		snapshot(&staged),
		snapshot(&ground),
		"staged materialization must be byte-identical to the gix clone"
	);

	// And identical Source hash — the round-trip contract's real anchor.
	let h_staged = skill::compute_skill_folder_hash(&staged).unwrap();
	let h_ground = skill::compute_skill_folder_hash(&ground).unwrap();
	assert_eq!(
		h_staged, h_ground,
		"Source hash of staged folder must equal the clone's"
	);
	// Sanity: a real, non-empty hash (not the empty-input placeholder).
	assert_ne!(h_staged, skill::hash::EMPTY_SKILLS_LOCK_DIGEST);
}
