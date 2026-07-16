//! Contract for the validated `SkillPath` newtype (ticket 02).
//!
//! A `SkillPath` is a repo-relative, POSIX skill *folder* path. Construction
//! rejects anything that could escape the repo/clone root (`..`, absolute,
//! leading `/`, Windows drive prefix); the empty path denotes the repo-root
//! skill. Once constructed, joining it under any root is guaranteed to stay
//! inside that root — that guarantee is what closes the traversal risk at every
//! fetch/install entry point.

use skill::SkillPath;
use std::path::Path;

#[test]
fn accepts_normal_subfolder() {
	let sp = SkillPath::parse("skills/music").expect("normal path is valid");
	assert_eq!(sp.as_str(), "skills/music");
	assert!(!sp.is_root());
	assert_eq!(
		sp.resolve_under(Path::new("/srv/clone")),
		Path::new("/srv/clone/skills/music"),
	);
}

#[test]
fn accepts_empty_root_path() {
	let sp = SkillPath::parse("").expect("empty path is the repo-root skill");
	assert!(sp.is_root());
	assert_eq!(sp.as_str(), "");
	assert_eq!(
		sp.resolve_under(Path::new("/srv/clone")),
		Path::new("/srv/clone"),
	);
}

#[test]
fn normalizes_backslashes_to_posix() {
	let sp = SkillPath::parse("skills\\music").expect("valid after normalize");
	assert_eq!(sp.as_str(), "skills/music");
}

#[test]
fn rejects_parent_traversal() {
	assert!(SkillPath::parse("../evil").is_err());
	assert!(SkillPath::parse("a/../../b").is_err());
	assert!(SkillPath::parse("skills/../../etc").is_err());
	assert!(SkillPath::parse("..").is_err());
}

#[test]
fn rejects_absolute_and_leading_slash() {
	assert!(SkillPath::parse("/etc/passwd").is_err());
	assert!(SkillPath::parse("/").is_err());
}

#[test]
fn rejects_windows_drive_and_backslash_traversal() {
	// `C:\...` normalizes to `C:/...` — a drive prefix, still an escape.
	assert!(SkillPath::parse("C:\\Windows\\System32").is_err());
	// Backslash traversal normalizes to `../../evil`.
	assert!(SkillPath::parse("..\\..\\evil").is_err());
}

/// The security-critical invariant: any value that parses, joined under ANY
/// root, stays inside that root. This is the property every fetch/install join
/// relies on.
#[test]
fn resolved_path_always_stays_inside_root() {
	let root = Path::new("/srv/clone");
	for good in ["", "a", "a/b/c", "skills/music", "skills\\music"] {
		let sp = SkillPath::parse(good)
			.unwrap_or_else(|_| panic!("{good:?} should be valid"));
		let joined = sp.resolve_under(root);
		assert!(
			joined.starts_with(root),
			"{good:?} escaped the root: {joined:?}",
		);
	}
}
