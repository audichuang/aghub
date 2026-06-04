//! CI-BLOCKING: aghub hash must byte-match npx `computeSkillFolderHash` on a
//! committed fixture. Re-capture GOLDEN via the bun command in the plan if the
//! fixture changes. The test also injects .git/node_modules at runtime to prove
//! they are skipped (we cannot commit a literal `.git` dir).

use skill::compute_skill_folder_hash;
use std::fs;

const GOLDEN: &str =
	"38a71af3e6146b33484d22a5ebd8fc9df2368d7da7eac1bd661baadcf60acad9";
const FIXTURE: &str = concat!(
	env!("CARGO_MANIFEST_DIR"),
	"/tests/fixtures/hash-parity-skill"
);

#[test]
fn hash_parity_fixture_skill_golden() {
	let got = compute_skill_folder_hash(std::path::Path::new(FIXTURE)).unwrap();
	assert_eq!(got, GOLDEN, "aghub hash differs from npx golden");
}

#[test]
fn hash_parity_skip_git_and_modules() {
	// Copy the fixture into a temp dir, then add .git + node_modules; the hash
	// must be unchanged (they are skipped), i.e. still equal to GOLDEN.
	let tmp = tempfile::tempdir().unwrap();
	let dst = tmp.path().join("skill");
	copy_dir(std::path::Path::new(FIXTURE), &dst);
	fs::create_dir_all(dst.join(".git/objects")).unwrap();
	fs::write(dst.join(".git/objects/abc"), b"junk").unwrap();
	fs::create_dir_all(dst.join("node_modules/pkg")).unwrap();
	fs::write(dst.join("node_modules/pkg/index.js"), b"junk").unwrap();
	assert_eq!(compute_skill_folder_hash(&dst).unwrap(), GOLDEN);
}

fn copy_dir(from: &std::path::Path, to: &std::path::Path) {
	fs::create_dir_all(to).unwrap();
	for e in fs::read_dir(from).unwrap() {
		let e = e.unwrap();
		let dst = to.join(e.file_name());
		if e.file_type().unwrap().is_dir() {
			copy_dir(&e.path(), &dst);
		} else {
			fs::copy(e.path(), dst).unwrap();
		}
	}
}

/// Golden captured from the REAL upstream `computeSkillFolderHash` (skills
/// v1.5.x, run via `node --experimental-strip-types`) on a folder whose names
/// are exactly the case-collision + numeric cases where feruca's default
/// ("shifted") order diverged from npx `localeCompare`. Pins that aghub now
/// orders these like npx (non-ignorable punctuation), not just simple names.
const GOLDEN_EXOTIC: &str =
	"fee6325b514e7168672298bbac49237ec3a99350c0741c1cbc18accd038f7f9f";

#[test]
fn hash_parity_exotic_filenames_match_npx() {
	let tmp = tempfile::tempdir().unwrap();
	let d = tmp.path();
	for (name, body) in [
		("1.md", "1"),
		("2.md", "2"),
		("10.md", "10"),
		("z.md", "z"),
		("ZEBRA.md", "zebra"),
	] {
		fs::write(d.join(name), body).unwrap();
	}
	assert_eq!(
		compute_skill_folder_hash(d).unwrap(),
		GOLDEN_EXOTIC,
		"aghub hash must match npx localeCompare order for case/numeric names"
	);
}
