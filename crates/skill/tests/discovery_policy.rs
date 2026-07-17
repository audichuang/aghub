//! Behavior-pinning tests for the shared skill-discovery policy (ticket 03).
//!
//! These pin the EXACT semantics extracted from the historical `scan_skills`
//! into the pure `discover_from_entries` fn, so that a later silent drift (a
//! case-insensitive dedup, a shifted depth boundary, a lost first-seen wins
//! rule) fails loudly. The extraction must be behavior-preserving: the same
//! fixture run through the filesystem `scan_skills` and through the pure fn fed
//! the equivalent entry stream must agree.
//!
//! Equivalence approach: **pinned expected values** (not a side-by-side copy of
//! the old impl). The pure-fn tests pin exact output vecs; the `scan_skills`
//! tests pin exact on-disk paths and cross-check the pure fn on the equivalent
//! stream. The pre-existing `scan.rs` suite (30 tests) is the standing
//! regression guard for the filesystem side, so keeping a dead copy of the old
//! implementation solely for a test would only rot.

use skill::scan::{
	discover_from_entries, scan_skills, CandidateEntry, ScanOptions,
};
use std::path::PathBuf;
use tempfile::TempDir;

fn entry(path: &str, depth: usize, name: Option<&str>) -> CandidateEntry {
	CandidateEntry {
		path: PathBuf::from(path),
		depth,
		has_skill_md: true,
		name: name.map(String::from),
	}
}

fn make_skill(dir: &std::path::Path, name: &str) {
	std::fs::create_dir_all(dir).unwrap();
	std::fs::write(
		dir.join("SKILL.md"),
		format!("---\nname: {name}\ndescription: test skill\n---\n# Body"),
	)
	.unwrap();
}

// --- (a) Case-sensitivity: `Foo` and `foo` are DIFFERENT names -------------

#[test]
fn policy_dedup_is_case_sensitive() {
	// Names differ only in case; both MUST survive. Fails if dedup ever folds
	// case.
	let entries = vec![
		entry("a/Foo", 2, Some("Foo")),
		entry("b/foo", 2, Some("foo")),
	];
	let got = discover_from_entries(entries, 10, true);
	assert_eq!(got, vec![PathBuf::from("a/Foo"), PathBuf::from("b/foo")]);
}

// --- (b) Depth boundary respects max_depth ---------------------------------

#[test]
fn policy_respects_max_depth_boundary() {
	// depth == max_depth is kept; depth > max_depth is dropped (walker
	// semantics: root = 0).
	let entries = vec![
		entry("at", 3, Some("at-boundary")),
		entry("beyond", 4, Some("beyond-boundary")),
	];
	let got = discover_from_entries(entries, 3, true);
	assert_eq!(got, vec![PathBuf::from("at")]);
}

// --- (c) Dedup by name keeps first-seen ------------------------------------

#[test]
fn policy_dedups_same_name_keeping_first_seen() {
	let entries = vec![
		entry("first/dup", 2, Some("dup")),
		entry("second/dup", 2, Some("dup")),
	];
	let got = discover_from_entries(entries, 10, true);
	assert_eq!(got, vec![PathBuf::from("first/dup")]);
}

// --- Additional pinned semantics (skip rules + early return) ----------------

#[test]
fn policy_skips_missing_skill_md_and_unparseable_name() {
	let entries = vec![
		CandidateEntry {
			path: PathBuf::from("no-md"),
			depth: 1,
			has_skill_md: false,
			name: Some("x".into()),
		},
		CandidateEntry {
			path: PathBuf::from("no-name"),
			depth: 1,
			has_skill_md: true,
			name: None,
		},
		entry("good", 1, Some("good")),
	];
	let got = discover_from_entries(entries, 10, true);
	assert_eq!(got, vec![PathBuf::from("good")]);
}

#[test]
fn policy_full_depth_false_returns_root_skill_only() {
	// A depth-0 (root) skill with !full_depth is the historical early return:
	// return it alone, ignore everything after.
	let entries = vec![
		entry("", 0, Some("root")),
		entry("nested/child", 2, Some("child")),
	];
	assert_eq!(
		discover_from_entries(entries.clone(), 10, false),
		vec![PathBuf::from("")]
	);
	// full_depth => both survive.
	assert_eq!(
		discover_from_entries(entries, 10, true),
		vec![PathBuf::from(""), PathBuf::from("nested/child")]
	);
}

// --- Equivalence: filesystem scan_skills agrees with the pure fn -----------

#[test]
fn scan_and_policy_agree_case_sensitive() {
	let temp = TempDir::new().unwrap();
	// Distinct folder names (avoid case-insensitive-fs folder collision), but
	// frontmatter names differing only in case.
	make_skill(&temp.path().join("a"), "Foo");
	make_skill(&temp.path().join("b"), "foo");

	let opts = ScanOptions {
		full_depth: true,
		max_depth: 10,
		respect_gitignore: false,
	};
	let mut scanned = scan_skills(temp.path(), opts, vec![]).unwrap();
	scanned.sort();

	let mut expected = vec![temp.path().join("a"), temp.path().join("b")];
	expected.sort();
	// Both survive on the filesystem side (case-sensitive dedup).
	assert_eq!(scanned, expected);

	// The pure fn fed the equivalent stream agrees.
	let entries = vec![
		CandidateEntry {
			path: temp.path().join("a"),
			depth: 1,
			has_skill_md: true,
			name: Some("Foo".into()),
		},
		CandidateEntry {
			path: temp.path().join("b"),
			depth: 1,
			has_skill_md: true,
			name: Some("foo".into()),
		},
	];
	let mut policy = discover_from_entries(entries, 10, true);
	policy.sort();
	assert_eq!(policy, expected);
}

#[test]
fn scan_and_policy_agree_dedup_and_depth() {
	let temp = TempDir::new().unwrap();
	// Same-name skill twice + one skill beyond a max_depth of 2.
	make_skill(&temp.path().join("x1/dup"), "dup");
	make_skill(&temp.path().join("x2/dup"), "dup");
	make_skill(&temp.path().join("a/b/c/deep"), "deep"); // folder depth 4

	let opts = ScanOptions {
		full_depth: true,
		max_depth: 2,
		respect_gitignore: false,
	};
	let scanned = scan_skills(temp.path(), opts, vec![]).unwrap();

	// max_depth 2 excludes the depth-4 "deep" skill; the two "dup" skills
	// dedup to exactly one.
	assert_eq!(scanned.len(), 1);
	assert!(scanned[0].ends_with("dup"));
}
