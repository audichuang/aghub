use crate::models::Skill;
use std::path::{Path, PathBuf};

/// Load skills from a directory using skill parser.
///
/// `Err` when a directory EXISTS but cannot be read: "absent" and
/// "unreadable" are different answers, and `load_failed` (which
/// `transfer::skill_holders` relies on) is set only from an `Err`.
/// See docs/history/core-repair-rename.md#discovery-read-unreadable-as-empty
pub fn load_skills_from_dir(skills_dir: &Path) -> std::io::Result<Vec<Skill>> {
	let (skills, failure, _) = super::enumerate::skills(skills_dir, false);
	match failure {
		Some(error) => Err(error),
		None => Ok(skills),
	}
}

/// Discover live Masters, skipping aghub's own bookkeeping in the store.
///
/// The skip is applied by `enumerate::skills` in store mode, through
/// [`crate::skills::linker::is_store_bookkeeping`], not a named path:
/// `.quarantine/`, `.aghub-stage-*`, `.aghub-backup-*` and
/// `.<name>.aghub-migrating` all hold a full `SKILL.md`, and without the skip
/// one name enumerates twice (resync refuses a multi-Master conflict, removal
/// picks by `read_dir` order).
///
/// `Err`-or-nothing on purpose for a DESTRUCTIVE caller: an unopenable
/// `SKILL.md` may be the very skill under another folder name, so a partial
/// list cannot answer "not there". Do not add a `_partial` twin here.
pub(crate) fn load_master_skills(store: &Path) -> std::io::Result<Vec<Skill>> {
	let (skills, failure, _) = super::enumerate::skills(store, true);
	match failure {
		Some(error) => Err(error),
		None => Ok(skills),
	}
}

/// Masters in ONE scope's store that no agent reads — every agent unticked,
/// so the skill is stored (and still updated) but granted to nobody.
///
/// Invisible to every per-agent listing, so it needs its own question.
/// "Reads" is by frontmatter name, as in the update check's Master fallback.
///
/// Advisory only: `load_all_agents` fails OPEN, so a Master read only by an
/// unloadable agent lists here too. Deleting is still safe — the removal plan
/// re-derives readers and fails closed.
pub fn withheld_masters(
	scope: crate::models::ResourceScope,
	project_root: Option<&Path>,
) -> std::io::Result<Vec<Skill>> {
	use crate::models::ResourceScope;
	let store_root = match scope {
		ResourceScope::GlobalOnly => None,
		ResourceScope::ProjectOnly if project_root.is_some() => project_root,
		_ => return Ok(Vec::new()),
	};
	let Some(store) = crate::skills::linker::master_store_dir(store_root)
	else {
		return Ok(Vec::new());
	};
	let masters = load_master_skills(&store)?;
	if masters.is_empty() {
		return Ok(masters);
	}
	let read: std::collections::HashSet<String> =
		crate::load_all_agents(scope, project_root)
			.into_iter()
			.flat_map(|agent| agent.skills)
			.map(|skill| skill.name)
			.collect();
	Ok(masters
		.into_iter()
		.filter(|master| !read.contains(&master.name))
		.collect())
}

/// [`load_skills_from_dir`], but keeping what it COULD read alongside the fact
/// that something was missed.
///
/// The flag means ENTRIES may be missing from the list — the directory could
/// not be listed, or one of its entries could not be classified. A SKILL.md
/// this walk could not parse does NOT set it: that entry was still enumerated,
/// so a caller probing paths can see it.
///
/// For guards that need the visible entries AND the warning: dropping the
/// partial list hides a live Referrer, refusing outright lets one odd sibling
/// block every deletion.
pub fn load_skills_from_dir_partial(skills_dir: &Path) -> (Vec<Skill>, bool) {
	let (skills, _, unlisted) = super::enumerate::skills(skills_dir, false);
	(skills, unlisted)
}

/// Every entry path under `dir`, WITHOUT parsing any of them, plus whether the
/// listing may be short.
///
/// For questions answered by IDENTITY, not name ("does a link here resolve to
/// that directory?"), so an unparsable Referrer is still seen. Recurses into
/// real directories only; links are reported, never followed.
pub fn entry_paths(dir: &Path) -> (Vec<PathBuf>, bool) {
	super::enumerate::entries(dir, true)
}

/// Load skills from multiple directories. `Err` as for [`load_skills_from_dir`].
pub fn load_skills_from_dirs(dirs: &[PathBuf]) -> std::io::Result<Vec<Skill>> {
	let mut all_skills = Vec::new();
	let mut seen_names = std::collections::HashSet::new();

	for dir in dirs {
		let (skills, failure, _) = super::enumerate::skills(dir, false);
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

#[cfg(test)]
mod tests {
	use super::*;
	use std::fs;

	/// A Master every agent was unticked from is listed; one an agent still
	/// links to is not.
	#[cfg(unix)]
	#[test]
	fn withheld_masters_lists_only_the_ungranted_master() {
		use crate::models::ResourceScope;
		let tmp = tempfile::tempdir().unwrap();
		let root = tmp.path();
		for name in ["granted", "withheld"] {
			let master = root.join(".aghub").join(name);
			fs::create_dir_all(&master).unwrap();
			fs::write(
				master.join("SKILL.md"),
				format!("---\nname: {name}\ndescription: d\n---\n"),
			)
			.unwrap();
		}
		fs::create_dir_all(root.join(".claude/skills")).unwrap();
		std::os::unix::fs::symlink(
			root.join(".aghub/granted"),
			root.join(".claude/skills/granted"),
		)
		.unwrap();

		let names: Vec<String> =
			withheld_masters(ResourceScope::ProjectOnly, Some(root))
				.unwrap()
				.into_iter()
				.map(|skill| skill.name)
				.collect();
		assert_eq!(names, vec!["withheld".to_string()]);
	}

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
