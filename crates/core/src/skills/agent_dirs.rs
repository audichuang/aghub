//! One agent's skills directories at a scope: its write slot and its ordered
//! read dirs (write first). The ONE place the test skills-path override is
//! applied, so every consumer (adapter, shape, classify, doctor, API) sees it.

use aghub_agents::{AgentDescriptor, ResourceScope};
use std::cell::RefCell;
use std::path::{Path, PathBuf};

thread_local! {
	static SKILLS_PATH_OVERRIDE: RefCell<Option<(String, PathBuf)>> = const { RefCell::new(None) };
}

/// Override the skills dir of one agent (for testing). The single dir becomes
/// both its write slot and its only read dir, at every scope.
pub fn set_skills_path_override(agent_id: &str, path: Option<PathBuf>) {
	SKILLS_PATH_OVERRIDE.with(|p| {
		*p.borrow_mut() = path.map(|path| (agent_id.to_string(), path));
	});
}

#[derive(Debug, PartialEq, Eq)]
pub struct AgentSkillDirs {
	/// `None` when the agent cannot hold a skill at this scope.
	pub write: Option<PathBuf>,
	/// Every dir the agent reads at this scope; the write slot comes first.
	pub read: Vec<PathBuf>,
}

impl AgentSkillDirs {
	pub fn of(
		descriptor: &AgentDescriptor,
		scope: ResourceScope,
		project_root: Option<&Path>,
	) -> Self {
		if let Some((id, path)) =
			SKILLS_PATH_OVERRIDE.with(|p| p.borrow().clone())
		{
			if id == descriptor.id {
				return Self {
					write: Some(path.clone()),
					read: vec![path],
				};
			}
		}
		Self {
			write: descriptor.skill_write_path(project_root, scope),
			read: descriptor.skill_read_paths(project_root, scope),
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::models::AgentType;
	use crate::skills::prune::test_lock::env_lock;
	use crate::skills::shape::compat_roster;

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

	#[test]
	fn override_reaches_every_dir_consumer() {
		let _env = env_lock().lock().unwrap_or_else(|e| e.into_inner());
		let tmp = tempfile::tempdir().unwrap();
		let root = std::fs::canonicalize(tmp.path()).unwrap();
		let home = root.join("home");
		let data = root.join("data");
		std::fs::create_dir_all(&home).unwrap();
		std::fs::create_dir_all(&data).unwrap();
		let _home = EnvVarGuard::set("HOME", &home);
		let _data = EnvVarGuard::set("AGHUB_DATA_DIR", &data);

		let cline = crate::registry::get(AgentType::Cline);
		let scope = ResourceScope::ProjectOnly;
		let name = "d2-override-probe";

		// Baseline without override: the private compat dir IS a compat dir, so
		// the empty answer below is not vacuous.
		assert!(compat_roster(scope, Some(&root))
			.iter()
			.find(|a| a.id == "cline")
			.unwrap()
			.compat_dirs()
			.any(|d| *d == root.join(".clinerules/skills")));

		let over = root.join("override-skills");
		std::fs::create_dir_all(over.join(name)).unwrap();
		std::fs::write(
			over.join(name).join("SKILL.md"),
			format!("---\nname: {name}\ndescription: test\n---\n"),
		)
		.unwrap();
		set_skills_path_override("cline", Some(over.clone()));

		// the seam itself
		assert_eq!(
			AgentSkillDirs::of(cline, scope, Some(&root)),
			AgentSkillDirs {
				write: Some(over.clone()),
				read: vec![over.clone()]
			}
		);
		// readers_of
		assert_eq!(
			crate::skills::shape::readers_of(scope, Some(&root), name),
			vec!["cline"]
		);
		// candidate_referrers (still write-dir derived)
		let cand =
			crate::skills::shape::candidate_referrers(scope, Some(&root), name);
		assert_eq!(
			cand.iter()
				.find(|c| c.agent_id == "cline")
				.map(|c| c.path.clone()),
			Some(over.join(name))
		);
		// compat roster
		let roster = crate::skills::shape::compat_roster(scope, Some(&root));
		let row = roster.iter().find(|a| a.id == "cline").unwrap();
		assert_eq!(row.write, Some(over.clone()));
		assert_eq!(row.read, vec![over.clone()]);
		// compat dirs: write == only read dir, so nothing else is left
		assert_eq!(row.compat_dirs().count(), 0);

		set_skills_path_override("cline", None);
	}
}
