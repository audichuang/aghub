//! Contract for the Source-staging materializer (ticket 04): mode-aware writes
//! with symlink containment. Unix-gated: exec bit and symlinks are the
//! semantics under test (Windows filename normalization is out of scope).
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use aghub_git::{stage_tree_entries, StagedEntry, StagedEntryMode};

fn entry(path: &str, bytes: &[u8], mode: StagedEntryMode) -> StagedEntry {
	StagedEntry {
		path: path.to_string(),
		bytes: bytes.to_vec(),
		mode,
	}
}

fn exec_bit(p: &Path) -> u32 {
	std::fs::metadata(p).unwrap().permissions().mode() & 0o111
}

// ── exec bit: 100755 → executable, 100644 → not (Unix) ──────────────────────

#[test]
fn regular_and_executable_modes_set_exec_bit_correctly() {
	let tmp = tempfile::tempdir().unwrap();
	let dest = tmp.path().join("staged");
	stage_tree_entries(
		[
			entry("plain.txt", b"plain", StagedEntryMode::Regular),
			entry(
				"run.sh",
				b"#!/bin/sh\necho hi\n",
				StagedEntryMode::Executable,
			),
			entry("nested/dir/deep.txt", b"deep", StagedEntryMode::Regular),
		],
		&dest,
	)
	.unwrap();

	assert_eq!(std::fs::read(dest.join("plain.txt")).unwrap(), b"plain");
	assert_eq!(
		std::fs::read(dest.join("nested/dir/deep.txt")).unwrap(),
		b"deep"
	);
	assert_ne!(
		exec_bit(&dest.join("run.sh")),
		0,
		"100755 must be executable"
	);
	assert_eq!(
		exec_bit(&dest.join("plain.txt")),
		0,
		"100644 must NOT be executable"
	);
}

// ── in-root symlink recreated as a symlink pointing at the same target ──────

#[test]
fn in_root_symlink_is_recreated_as_symlink() {
	let tmp = tempfile::tempdir().unwrap();
	let dest = tmp.path().join("staged");
	stage_tree_entries(
		[
			entry("SKILL.md", b"x", StagedEntryMode::Regular),
			entry("link.md", b"SKILL.md", StagedEntryMode::Symlink),
		],
		&dest,
	)
	.unwrap();

	let link = dest.join("link.md");
	let meta = std::fs::symlink_metadata(&link).unwrap();
	assert!(
		meta.file_type().is_symlink(),
		"must be a symlink, not a copy"
	);
	assert_eq!(std::fs::read_link(&link).unwrap(), Path::new("SKILL.md"));
}

// ── SECURITY: a symlink escaping the root is REJECTED and nothing is written ─

#[test]
fn symlink_escaping_root_via_dotdot_is_rejected_and_writes_nothing() {
	let tmp = tempfile::tempdir().unwrap();
	let outside = tmp.path().join("outside");
	std::fs::create_dir_all(&outside).unwrap();
	let canary = outside.join("secret.txt");
	std::fs::write(&canary, b"top secret").unwrap();

	let dest = tmp.path().join("staged");
	let res = stage_tree_entries(
		[entry(
			"pwn",
			b"../outside/secret.txt",
			StagedEntryMode::Symlink,
		)],
		&dest,
	);

	assert!(res.is_err(), "out-of-root symlink must be rejected");
	assert!(
		std::fs::symlink_metadata(dest.join("pwn")).is_err(),
		"the escaping symlink must NOT be created"
	);
	assert_eq!(
		std::fs::read(&canary).unwrap(),
		b"top secret",
		"file outside the staging root must be untouched"
	);
}

#[test]
fn absolute_symlink_target_is_rejected_and_writes_nothing() {
	let tmp = tempfile::tempdir().unwrap();
	let dest = tmp.path().join("staged");
	let res = stage_tree_entries(
		[entry("pwn", b"/etc/passwd", StagedEntryMode::Symlink)],
		&dest,
	);
	assert!(res.is_err(), "absolute symlink target must be rejected");
	assert!(
		std::fs::symlink_metadata(dest.join("pwn")).is_err(),
		"the absolute symlink must NOT be created"
	);
}

// ── SECURITY: an entry path escaping the root is rejected before any write ──

#[test]
fn entry_path_escaping_root_is_rejected() {
	let tmp = tempfile::tempdir().unwrap();
	let outside = tmp.path().join("outside");
	std::fs::create_dir_all(&outside).unwrap();
	let canary = outside.join("victim.txt");
	std::fs::write(&canary, b"orig").unwrap();

	let dest = tmp.path().join("staged");
	let res = stage_tree_entries(
		[entry(
			"../outside/victim.txt",
			b"pwned",
			StagedEntryMode::Regular,
		)],
		&dest,
	);
	assert!(res.is_err(), "`..` in entry path must be rejected");
	assert_eq!(
		std::fs::read(&canary).unwrap(),
		b"orig",
		"file outside the staging root must be untouched"
	);
}

// ── gitlink (160000) is never written as a file ─────────────────────────────

#[test]
fn gitlink_entry_is_never_written_as_a_file() {
	let tmp = tempfile::tempdir().unwrap();
	let dest = tmp.path().join("staged");
	stage_tree_entries(
		[
			entry("SKILL.md", b"x", StagedEntryMode::Regular),
			entry("vendored", b"", StagedEntryMode::Gitlink),
		],
		&dest,
	)
	.unwrap();
	assert!(dest.join("SKILL.md").exists());
	assert!(
		std::fs::symlink_metadata(dest.join("vendored")).is_err(),
		"a gitlink/submodule must not appear on disk"
	);
}

// ── collision guard: two entries at the same destination is an error, never a
//    silent merge/overwrite ───────────────────────────────────────────────────

#[test]
fn colliding_destinations_error_instead_of_silent_merge() {
	let tmp = tempfile::tempdir().unwrap();
	let dest = tmp.path().join("staged");
	let res = stage_tree_entries(
		[
			entry("dup", b"first", StagedEntryMode::Regular),
			entry("dup", b"second", StagedEntryMode::Regular),
		],
		&dest,
	);
	assert!(
		res.is_err(),
		"a second entry at the same path must error, not silently overwrite"
	);
}
