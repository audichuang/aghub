//! Where skills are on disk — the ONE walker. Two questions:
//! [`entries`] (by identity, no parsing, links never followed) and [`skills`]
//! (by frontmatter name, links followed, group dirs recursed). One marker rule
//! ([`skill_marker`]: root `SKILL.md` or `skill.md`, links followed) and the
//! store-bookkeeping skip live here only. Each caller still decides which way
//! [`SkillMarker::Unknown`] leans.

use crate::models::Skill;
use crate::skills::linker::Linker;
use std::fs;
use std::path::{Path, PathBuf};

/// The answer to "does `entry` hold a root skill marker" — three states, not a
/// bool. Each caller picks its own conservative answer for [`SkillMarker::Unknown`]:
/// shape's `classify_shape` treats it as present, `readers_of` as not present,
/// and prune keeps the key. Never collapse this to a bool. See docs/history/core-skills-shape.md#one-bool-marker-served-two-callers
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SkillMarker {
	/// A root marker file is really there (`metadata` follows links, so a
	/// healthy Referrer still passes through to the Master's own file).
	Present,
	/// A definite absence: either `NotFound`, or one path segment up,
	/// `NotADirectory` — a same-named REGULAR FILE at `entry` makes
	/// `metadata("<entry>/SKILL.md")` fail that way instead of `NotFound`,
	/// but it is just as definite. Nothing can live under a non-directory
	/// path, so there is no ambiguity to fail open about.
	Absent,
	/// The probe could not tell — most often a permission fault. Never
	/// "present" and never "absent"; see the type doc for how each caller
	/// resolves it.
	Unknown,
}

/// Whether `entry` serves a skill, by ONE rule: a root `SKILL.md` or `skill.md`,
/// links followed. `skill.md` too, as the skills-ref parser accepts
/// (`find_skill_md`). `Present` wins over `Unknown`, which wins over `Absent`.
pub(crate) fn skill_marker(entry: &Path) -> SkillMarker {
	let mut answer = SkillMarker::Absent;
	for file in ["SKILL.md", "skill.md"] {
		match fs::metadata(entry.join(file)) {
			Ok(meta) if meta.is_file() => return SkillMarker::Present,
			Ok(_) => {}
			Err(e)
				if matches!(
					e.kind(),
					std::io::ErrorKind::NotFound
						| std::io::ErrorKind::NotADirectory
				) => {}
			Err(_) => answer = SkillMarker::Unknown,
		}
	}
	answer
}

/// Every entry path under `dir`, WITHOUT parsing any of them, plus whether the
/// listing may be short.
///
/// For questions answered by IDENTITY, not name ("does a link here resolve to
/// that directory?"), so an unparsable Referrer is still seen. `recurse: false`
/// is one level (prune); `true` descends into every REAL directory, reporting
/// links but never following them (removal's identity question).
pub(crate) fn entries(dir: &Path, recurse: bool) -> (Vec<PathBuf>, bool) {
	let mut out = Vec::new();
	let mut unlisted = false;
	collect_entry_paths(dir, recurse, &mut out, &mut unlisted);
	(out, unlisted)
}

fn collect_entry_paths(
	dir: &Path,
	recurse: bool,
	out: &mut Vec<PathBuf>,
	unlisted: &mut bool,
) {
	let entries = match fs::read_dir(dir) {
		Ok(entries) => entries,
		// Absent, or not a directory at all: both hold no entries.
		Err(error)
			if matches!(
				error.kind(),
				std::io::ErrorKind::NotFound
					| std::io::ErrorKind::NotADirectory
			) =>
		{
			return;
		}
		Err(_) => {
			*unlisted = true;
			return;
		}
	};
	for entry in entries {
		let Ok(entry) = entry else {
			*unlisted = true;
			continue;
		};
		let path = entry.path();
		out.push(path.clone());
		if !recurse {
			continue;
		}
		// Recurse only into a REAL directory. A link is an answer, not a door:
		// following one would leave the dir being swept and could cycle.
		match fs::symlink_metadata(&path) {
			Ok(meta) if meta.is_dir() => {
				collect_entry_paths(&path, recurse, out, unlisted)
			}
			Ok(_) => {}
			Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
			Err(_) => *unlisted = true,
		}
	}
}

/// Skills under `dir` by frontmatter name, links followed, group dirs recursed.
///
/// `store: true` is the Master-store mode: top level only, and aghub's own
/// bookkeeping entries are skipped. Sorted by name.
pub(crate) fn skills(
	dir: &Path,
	store: bool,
) -> (Vec<Skill>, Option<std::io::Error>, bool) {
	let mut skills = Vec::new();
	let mut failure = None;
	let mut unlisted = false;
	collect_skills(dir, &mut skills, &mut failure, &mut unlisted, store);
	skills.sort_by(|a, b| a.name.cmp(&b.name));
	(skills, failure, unlisted)
}

/// Name the path in an I/O error.
///
/// `std::io::Error` out of `fs` carries no path.
fn at_path(path: &Path, error: std::io::Error) -> std::io::Error {
	std::io::Error::new(error.kind(), format!("{}: {error}", path.display()))
}

/// Walk `dir`, pushing every skill it can read and recording the FIRST failure
/// instead of stopping at it.
///
/// Walking on is what makes a destructive caller correct: aborting made one
/// unreadable sibling hide the whole dir, so a planner concluded nobody else
/// held the skill. The error is still returned.
fn collect_skills(
	dir: &Path,
	skills: &mut Vec<Skill>,
	failure: &mut Option<std::io::Error>,
	unlisted: &mut bool,
	store: bool,
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
		// A non-directory holds no ENTRIES — a complete answer. "Cannot tell"
		// means a directory whose CONTENTS are hidden.
		Err(error) if error.kind() == std::io::ErrorKind::NotADirectory => {
			return;
		}
		Err(error) => {
			*unlisted = true;
			note(failure, at_path(dir, error));
			return;
		}
	};

	for entry in entries {
		// A per-entry error is "could not read", not "not there" (mode 0400:
		// `read_dir` succeeds, every stat under it fails).
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
		// Store TOP level only. `to_string_lossy`, not `to_str`: a non-UTF8
		// name must still be tested for the leading dot.
		if store
			&& path.file_name().is_some_and(|name| {
				crate::skills::linker::is_store_bookkeeping(
					&name.to_string_lossy(),
				)
			}) {
			continue;
		}
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

		// No marker at all: a GROUP directory — recurse. An UNREADABLE marker
		// is not a group: it falls through to the parse below, whose `Io` arm
		// records it as a failure, as before.
		if skill_marker(&path) == SkillMarker::Absent {
			collect_skills(&path, skills, failure, unlisted, false);
			continue;
		}

		match skill::parser::parse_skill_dir(&path) {
			Ok(skill_pkg) => {
				let mut skill = crate::convert_skill(skill_pkg);
				// `Linker::is_link` also sees junctions (is_symlink() does not).
				if Linker::is_link(&path) {
					if let Ok(resolved) = fs::canonicalize(&path) {
						let canonical = resolved.join("SKILL.md");
						skill.canonical_path =
							crate::format_path_with_tilde(&canonical);
					}
				}
				skills.push(skill);
			}
			// No SKILL.md: a GROUP directory — recurse. An unreadable SKILL.md
			// is a failure, NOT a group (recursing hid a real reader and the
			// shared master was deleted). A malformed one keeps recursing
			// (`doctor`'s `invalid-skill`), and `InvalidData` / `IsADirectory`
			// count as malformed: the bytes were read.
			// See docs/history/core-repair-rename.md#discovery-read-unreadable-as-empty
			Err(skill::SkillError::Io(error))
				if !matches!(
					error.kind(),
					std::io::ErrorKind::InvalidData
						| std::io::ErrorKind::IsADirectory
				) =>
			{
				// NOT `unlisted`: the entry WAS enumerated; only its name is
				// unknown. Flagging it kept every OTHER agent's dir alive.
				note(failure, at_path(&path, error));
			}
			Err(_) => collect_skills(&path, skills, failure, unlisted, false),
		}
	}
}

#[cfg(all(test, unix))]
mod tests {
	use super::*;
	use crate::skills::prune::test_lock::env_lock;
	use std::collections::BTreeSet;
	use std::os::unix::fs::{symlink, PermissionsExt};

	struct EnvVarGuard(&'static str, Option<std::ffi::OsString>);

	impl EnvVarGuard {
		fn set(key: &'static str, value: &Path) -> Self {
			let previous = std::env::var_os(key);
			std::env::set_var(key, value);
			Self(key, previous)
		}
	}

	impl Drop for EnvVarGuard {
		fn drop(&mut self) {
			match self.1.take() {
				Some(value) => std::env::set_var(self.0, value),
				None => std::env::remove_var(self.0),
			}
		}
	}

	fn write_skill(dir: &Path, file: &str, name: &str) {
		std::fs::create_dir_all(dir).unwrap();
		std::fs::write(
			dir.join(file),
			format!("---\nname: {name}\ndescription: d\n---\n"),
		)
		.unwrap();
	}

	fn seed_lock(root: &Path, keys: &[&str]) {
		for &key in keys {
			skill::add_skill_to_local_lock(
				key,
				skill::LocalSkillLockEntry {
					source_url: None,
					ref_commit: None,
					source: "o/r".to_string(),
					ref_name: None,
					source_type: "github".to_string(),
					computed_hash: "h".to_string(),
					skill_path: None,
				},
				Some(root),
			)
			.unwrap();
		}
	}

	/// The production entry: real per-scope dirs, real scanner.
	fn prune_and_read(root: &Path) -> BTreeSet<String> {
		crate::skills::prune::prune_lock_scanning(&crate::WriteScope::project(
			root,
		))
		.unwrap();
		skill::read_local_lock(Some(root))
			.skills
			.keys()
			.cloned()
			.collect()
	}

	fn names(store: &Path) -> std::io::Result<Vec<String>> {
		crate::skills::discovery::load_master_skills(store)
			.map(|v| v.into_iter().map(|s| s.name).collect())
	}

	/// Each row is a fresh project root, so one row's lock cannot leak into
	/// another. Every row seeds `control-orphan` (no folder anywhere): it must
	/// be gone after the prune, which proves the prune ran and did not bail on
	/// the lock read. The expected sets therefore never list it.
	#[test]
	fn probe_table_enumerate_discovery_prune_agree() {
		let _env = env_lock().lock().unwrap_or_else(|e| e.into_inner());
		let tmp = tempfile::tempdir().unwrap();
		let outer = std::fs::canonicalize(tmp.path()).unwrap();
		let home = outer.join("home");
		let data = outer.join("data");
		std::fs::create_dir_all(&home).unwrap();
		std::fs::create_dir_all(&data).unwrap();
		let _home = EnvVarGuard::set("HOME", &home);
		let _data = EnvVarGuard::set("AGHUB_DATA_DIR", &data);

		// nested_broken_link: a group dir holding a dangling link. The group has
		// no marker, so it is not a skill; the dangling link is not one either.
		{
			let root = outer.join("nested_broken_link");
			let store = root.join(".aghub");
			std::fs::create_dir_all(store.join("group")).unwrap();
			symlink(root.join("missing"), store.join("group/inner")).unwrap();
			seed_lock(&root, &["inner", "control-orphan"]);

			assert_eq!(
				skill_marker(&store.join("group")),
				SkillMarker::Absent,
				"nested_broken_link: group must read as Absent"
			);
			assert!(
				entries(&store, true).0.contains(&store.join("group/inner")),
				"nested_broken_link: the recursive identity walk sees the link"
			);
			assert_eq!(
				names(&store).unwrap(),
				Vec::<String>::new(),
				"nested_broken_link: discovery lists no skill"
			);
			assert_eq!(
				prune_and_read(&root),
				BTreeSet::new(),
				"nested_broken_link: `inner` is dropped"
			);
		}

		// group_via_link: a link to a group dir. Links are followed by name
		// discovery but never by the identity walk. Prune is one level deep by
		// design: `team` has no marker of its own, so `alpha` is not a disk name.
		{
			let root = outer.join("group_via_link");
			let store = root.join(".aghub");
			write_skill(
				&root.join("elsewhere/team/alpha"),
				"SKILL.md",
				"alpha",
			);
			std::fs::create_dir_all(&store).unwrap();
			symlink(root.join("elsewhere/team"), store.join("team")).unwrap();
			seed_lock(&root, &["alpha", "control-orphan"]);

			assert_eq!(
				skill_marker(&store.join("team")),
				SkillMarker::Absent,
				"group_via_link: the link to a group has no marker"
			);
			assert!(
				!entries(&store, true).0.contains(&store.join("team/alpha")),
				"group_via_link: links are never followed by the identity walk"
			);
			assert_eq!(
				names(&store).unwrap(),
				vec!["alpha".to_string()],
				"group_via_link: name discovery follows the link"
			);
			assert_eq!(
				prune_and_read(&root),
				BTreeSet::new(),
				"group_via_link: `alpha` is dropped (prune is one level deep)"
			);
		}

		// lowercase: a root `skill.md` is a skill (D3: prune used to drop it).
		{
			let root = outer.join("lowercase");
			let store = root.join(".aghub");
			write_skill(&store.join("lower"), "skill.md", "lower");
			seed_lock(&root, &["lower", "control-orphan"]);

			assert_eq!(
				skill_marker(&store.join("lower")),
				SkillMarker::Present,
				"lowercase: skill.md is a marker"
			);
			assert_eq!(
				names(&store).unwrap(),
				vec!["lower".to_string()],
				"lowercase: discovery lists it"
			);
			assert_eq!(
				prune_and_read(&root),
				BTreeSet::from(["lower".to_string()]),
				"lowercase: the key is KEPT (D3: was pruned before)"
			);
		}

		// unreadable: a skill folder whose marker cannot be probed (mode 0000).
		{
			let root = outer.join("unreadable");
			let store = root.join(".aghub");
			let locked = store.join("locked");
			write_skill(&locked, "SKILL.md", "locked");
			seed_lock(&root, &["locked", "control-orphan"]);
			std::fs::set_permissions(
				&locked,
				std::fs::Permissions::from_mode(0o000),
			)
			.unwrap();
			if std::fs::read_dir(&locked).is_ok() {
				// Root reads through 0o000, so the row cannot be built here.
				std::fs::set_permissions(
					&locked,
					std::fs::Permissions::from_mode(0o755),
				)
				.unwrap();
				eprintln!("skip unreadable row: perms not enforced");
			} else {
				let marker = skill_marker(&locked);
				let discovery_failed = names(&store).is_err();
				let after = prune_and_read(&root);
				// Restore before asserting, so a failure cannot strand the tempdir.
				std::fs::set_permissions(
					&locked,
					std::fs::Permissions::from_mode(0o755),
				)
				.unwrap();
				assert_eq!(
					marker,
					SkillMarker::Unknown,
					"unreadable: the probe cannot tell"
				);
				assert!(
					discovery_failed,
					"unreadable: discovery reports the failure"
				);
				assert_eq!(
					after,
					BTreeSet::from(["locked".to_string()]),
					"unreadable: the key is KEPT (D3: was pruned before)"
				);
			}
		}

		// quarantine: aghub's bookkeeping is skipped by the store walk, and prune
		// has no root marker to see under `.quarantine`.
		{
			let root = outer.join("quarantine");
			let store = root.join(".aghub");
			write_skill(&store.join(".quarantine/q/1"), "SKILL.md", "q");
			seed_lock(&root, &["q", "control-orphan"]);

			assert_eq!(
				skill_marker(&store.join(".quarantine")),
				SkillMarker::Absent,
				"quarantine: no root marker on the bookkeeping dir"
			);
			assert!(
				skills(&store, true).0.is_empty(),
				"quarantine: the store walk skips bookkeeping"
			);
			assert_eq!(
				names(&store).unwrap(),
				Vec::<String>::new(),
				"quarantine: discovery lists nothing"
			);
			assert_eq!(
				prune_and_read(&root),
				BTreeSet::new(),
				"quarantine: `q` is dropped"
			);
		}

		// renamed_folder: prune is keyed by FOLDER name, discovery by frontmatter.
		{
			let root = outer.join("renamed_folder");
			let store = root.join(".aghub");
			write_skill(&store.join("folder-x"), "SKILL.md", "other-name");
			seed_lock(&root, &["folder-x", "other-name", "control-orphan"]);

			assert_eq!(
				skill_marker(&store.join("folder-x")),
				SkillMarker::Present,
				"renamed_folder: the folder has a marker"
			);
			assert_eq!(
				names(&store).unwrap(),
				vec!["other-name".to_string()],
				"renamed_folder: discovery names it by frontmatter"
			);
			assert_eq!(
				prune_and_read(&root),
				BTreeSet::from(["folder-x".to_string()]),
				"renamed_folder: keyed by folder name (pinned, unchanged by D3)"
			);
		}
	}
}
