use crate::models::Skill;
use crate::skills::linker::Linker;
use std::fs;
use std::path::{Path, PathBuf};

/// Load skills from a directory using skill parser.
///
/// `Err` when a directory EXISTS but cannot be read. "Absent" and "unreadable"
/// are different answers and this used to return the same empty list for both:
/// `chmod 000` on an agent's skills dir made `get skills` print `[]` on exit 0
/// with no warning, and — because `load_all_agents` only marks `load_failed`
/// when the load returns `Err` — made that agent invisible to
/// `transfer::skill_holders`, whose whole job is to notice a reader before the
/// shared master is deleted. Same silent data loss, with less signal than the
/// malformed-config case it sits next to.
pub fn load_skills_from_dir(skills_dir: &Path) -> std::io::Result<Vec<Skill>> {
	let (skills, failure, _) = walk_dir(skills_dir);
	match failure {
		Some(error) => Err(error),
		None => Ok(skills),
	}
}

/// [`load_skills_from_dir`], but keeping what it COULD read alongside the fact
/// that something was missed.
///
/// The flag means ENTRIES may be missing from the list — the directory could
/// not be listed, or one of its entries could not be classified. A SKILL.md
/// this walk could not parse does NOT set it: that entry was still enumerated,
/// so a caller probing paths can see it.
///
/// For the guards that must not turn "cannot tell" into "nothing is there":
/// they need the entries they can see AND the warning that the list is short.
/// `Err`-or-nothing forces them to pick one, and both choices are wrong for a
/// destructive decision — dropping the partial list hides a live Referrer,
/// while refusing outright makes one odd sibling block every deletion.
pub fn load_skills_from_dir_partial(skills_dir: &Path) -> (Vec<Skill>, bool) {
	let (skills, _, unlisted) = walk_dir(skills_dir);
	(skills, unlisted)
}

fn walk_dir(skills_dir: &Path) -> (Vec<Skill>, Option<std::io::Error>, bool) {
	let mut skills = Vec::new();
	let mut failure = None;
	let mut unlisted = false;
	collect_skills(skills_dir, &mut skills, &mut failure, &mut unlisted);
	skills.sort_by(|a, b| a.name.cmp(&b.name));
	(skills, failure, unlisted)
}

/// Load skills from multiple directories. `Err` as for [`load_skills_from_dir`].
pub fn load_skills_from_dirs(dirs: &[PathBuf]) -> std::io::Result<Vec<Skill>> {
	let mut all_skills = Vec::new();
	let mut seen_names = std::collections::HashSet::new();

	for dir in dirs {
		let mut skills = Vec::new();
		let mut failure = None;
		let mut unlisted = false;
		collect_skills(dir, &mut skills, &mut failure, &mut unlisted);
		if let Some(error) = failure {
			return Err(error);
		}

		for skill in skills {
			if seen_names.insert(skill.name.clone()) {
				all_skills.push(skill);
			}
		}
	}

	all_skills.sort_by(|a, b| a.name.cmp(&b.name));
	Ok(all_skills)
}

/// Name the path in an I/O error.
///
/// `std::io::Error` out of `fs` carries no path, so an unreadable skills
/// directory surfaced as a bare `Permission denied (os error 13)` — on a
/// command that may not even have been about skills.
fn at_path(path: &Path, error: std::io::Error) -> std::io::Error {
	std::io::Error::new(error.kind(), format!("{}: {error}", path.display()))
}

/// Walk `dir`, pushing every skill it can read and recording the FIRST failure
/// instead of stopping at it.
///
/// Walking on is not a relaxation, it is what makes a destructive caller
/// correct. Aborting discarded every skill already found in the same tree, so
/// one unreadable sibling made a whole agent dir look empty — and an empty read
/// dir is how `candidate_entries` loses a nested Referrer and how a planner
/// concludes nobody else holds the skill. The error is still returned to
/// callers that want it; what changes is that "some of it" is no longer thrown
/// away with it.
fn collect_skills(
	dir: &Path,
	skills: &mut Vec<Skill>,
	failure: &mut Option<std::io::Error>,
	unlisted: &mut bool,
) {
	/// Keep the FIRST failure: it is the one nearest the caller's own path,
	/// and a later one adds nothing a caller can act on.
	fn note(slot: &mut Option<std::io::Error>, error: std::io::Error) {
		if slot.is_none() {
			*slot = Some(error);
		}
	}
	let entries = match fs::read_dir(dir) {
		Ok(entries) => entries,
		// A directory that is not there holds nothing — that IS the answer,
		// and it is the ordinary state for most agents.
		Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
			return;
		}
		Err(error) => {
			// A path that is not a directory holds no entries — nothing CAN be
			// missing from the list, so this is not an unfinished enumeration.
			// The error is still recorded: `skill_holders` must fail CLOSED on
			// a peer whose skills dir it cannot even open.
			*unlisted |= error.kind() != std::io::ErrorKind::NotADirectory;
			note(failure, at_path(dir, error));
			return;
		}
	};

	for entry in entries {
		// A per-entry error is "could not read", not "not there" — the same
		// distinction the `read_dir` arm above makes, and `flatten()` +
		// `is_dir()` both answered "no" to it. With mode 0400 on the skills
		// dir `read_dir` SUCCEEDS and every stat under it then fails, so a
		// directory full of skills read as empty and a genuine holder went
		// invisible to `transfer::skill_holders`.
		let entry = match entry {
			Ok(entry) => entry,
			Err(error) => {
				// An entry that failed mid-enumeration is one this list does
				// not have — the caller's candidate set is short by it.
				*unlisted = true;
				note(failure, at_path(dir, error));
				continue;
			}
		};
		let path = entry.path();
		match fs::metadata(&path) {
			Ok(meta) if meta.is_dir() => {}
			// A file, or a referrer pointing at nothing: both are real
			// answers about what is installed, not read failures. A dangling
			// link is `doctor`'s to report.
			Ok(_) => continue,
			Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
				continue;
			}
			Err(error) => {
				// Listed but unclassifiable: it could be the Referrer, and
				// nothing here can rule that out.
				*unlisted = true;
				note(failure, at_path(&path, error));
				continue;
			}
		}

		match skill::parser::parse_skill_dir(&path) {
			Ok(skill_pkg) => {
				let mut skill = crate::convert_skill(skill_pkg);
				// Detect a link (unix symlink OR windows junction) and record the
				// canonical path. A junction reports is_symlink()==false, so the
				// bare file-type check missed it; Linker::is_link sees both.
				if Linker::is_link(&path) {
					if let Ok(resolved) = fs::canonicalize(&path) {
						let canonical = resolved.join("SKILL.md");
						skill.canonical_path =
							crate::format_path_with_tilde(&canonical);
					}
				}
				skills.push(skill);
			}
			// A directory with no SKILL.md is a GROUP directory — recurse.
			// An I/O error is not: `parse_skill_dir` raises `SkillError::Io`
			// when SKILL.md is there and cannot be read, and the old blanket
			// `Err(_)` recursed into it, found only files, and returned `Ok`.
			// The agent then held the skill while reporting it held nothing:
			// `load_failed` stayed false, `transfer::skill_holders` counted a
			// real reader as a non-reader, and the shared master was deleted —
			// exit 0, "N succeeded, 0 failed", no warning. In a copy layout the
			// same truncation widened a single-agent removal into a sweep that
			// deleted an UNTARGETED agent's skill directory.
			//
			// A malformed SKILL.md keeps recursing, as it always has: that is
			// `doctor`'s `invalid-skill`, and changing it here would be a
			// separate, wider behaviour change.
			// ...but `SkillError::Io` is not a synonym for "unreadable".
			// `read_to_string` also raises `InvalidData` for a SKILL.md that is
			// not UTF-8 (one latin-1 byte, a cp1252 smart quote) and
			// `IsADirectory` for a SKILL.md that is a directory. Those bytes
			// WERE read; the content is malformed. Propagating them made a
			// single bad file exit-1 every command for that agent — including
			// the `delete` that would have removed the offender, so it could
			// not be cleaned up through aghub at all.
			Err(skill::SkillError::Io(error))
				if !matches!(
					error.kind(),
					std::io::ErrorKind::InvalidData
						| std::io::ErrorKind::IsADirectory
				) =>
			{
				// NOT `unlisted`: the entry WAS enumerated, so the caller's
				// per-entry probes can still see it. Only its frontmatter name
				// is unknown, and treating that as a hidden entry made one
				// broken skill keep every OTHER agent's directory alive.
				note(failure, at_path(&path, error));
			}
			Err(_) => collect_skills(&path, skills, failure, unlisted),
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::fs;

	#[test]
	fn test_recursive_skills_discovery() {
		let tmp = tempfile::tempdir().unwrap();
		let root = tmp.path();
		let skill_a = root.join("skill-a");
		fs::create_dir_all(&skill_a).unwrap();
		fs::write(
			skill_a.join("SKILL.md"),
			"---\nname: skill-a\ndescription: Direct skill\n---\n",
		)
		.unwrap();
		let group = root.join("group");
		fs::create_dir_all(&group).unwrap();
		let skill_b = group.join("skill-b");
		fs::create_dir_all(&skill_b).unwrap();
		fs::write(
			skill_b.join("SKILL.md"),
			"---\nname: skill-b\ndescription: Nested skill\n---\n",
		)
		.unwrap();
		let skills = load_skills_from_dir(root).unwrap();
		let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
		assert!(names.contains(&"skill-a"));
		assert!(names.contains(&"skill-b"));
		assert_eq!(skills.len(), 2);
	}

	// T-DISCOVERY-JUNCTION-CANONICAL: a junction install is recognized as a
	// referrer (canonical_path set), not rediscovered as a plain copy.
	// windows-latest.
	#[cfg(windows)]
	#[test]
	fn discovery_sets_canonical_path_for_junction() {
		use crate::skills::linker::create_junction;
		let tmp = tempfile::tempdir().unwrap();
		let master = tmp.path().join(".agents/skills/foo");
		std::fs::create_dir_all(&master).unwrap();
		std::fs::write(
			master.join("SKILL.md"),
			"---\nname: foo\ndescription: d\n---\n",
		)
		.unwrap();
		let claude = tmp.path().join(".claude/skills");
		std::fs::create_dir_all(&claude).unwrap();
		create_junction(&master.canonicalize().unwrap(), &claude.join("foo"))
			.unwrap();

		let skills = load_skills_from_dir(&claude).unwrap();
		let foo = skills
			.iter()
			.find(|s| s.name == "foo")
			.expect("junction install must be discovered");
		assert!(
			foo.canonical_path.is_some(),
			"a junction must set canonical_path (recognized as a referrer)"
		);
	}
}
