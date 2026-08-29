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
	let mut skills = Vec::new();
	collect_skills(skills_dir, &mut skills)?;
	skills.sort_by(|a, b| a.name.cmp(&b.name));
	Ok(skills)
}

/// Load skills from multiple directories. `Err` as for [`load_skills_from_dir`].
pub fn load_skills_from_dirs(dirs: &[PathBuf]) -> std::io::Result<Vec<Skill>> {
	let mut all_skills = Vec::new();
	let mut seen_names = std::collections::HashSet::new();

	for dir in dirs {
		let mut skills = Vec::new();
		collect_skills(dir, &mut skills)?;

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

fn collect_skills(dir: &Path, skills: &mut Vec<Skill>) -> std::io::Result<()> {
	let entries = match fs::read_dir(dir) {
		Ok(entries) => entries,
		// A directory that is not there holds nothing — that IS the answer,
		// and it is the ordinary state for most agents.
		Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
			return Ok(());
		}
		Err(error) => return Err(at_path(dir, error)),
	};

	for entry in entries {
		// A per-entry error is "could not read", not "not there" — the same
		// distinction the `read_dir` arm above makes, and `flatten()` +
		// `is_dir()` both answered "no" to it. With mode 0400 on the skills
		// dir `read_dir` SUCCEEDS and every stat under it then fails, so a
		// directory full of skills read as empty and a genuine holder went
		// invisible to `transfer::skill_holders`.
		let entry = entry.map_err(|error| at_path(dir, error))?;
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
			Err(error) => return Err(at_path(&path, error)),
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
			Err(skill::SkillError::Io(error)) => {
				return Err(at_path(&path, error));
			}
			Err(_) => collect_skills(&path, skills)?,
		}
	}
	Ok(())
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
