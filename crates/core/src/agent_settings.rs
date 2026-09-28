//! Which agents the user has turned OFF — the ONE home of that choice.
//!
//! A disabled agent is one aghub does not manage: no flow that picks its own
//! agent set (repair, update, git sync, rename, delete-from-all) writes into a
//! disabled agent's directories. It still counts as a READER wherever a flow
//! asks "would anyone lose this skill" — not managing an agent must never mean
//! breaking it.
//!
//! Persisted under [`crate::paths::app_data_dir`] so every surface (desktop,
//! CLI, a remote's aghub-api) reads the same answer. Default: every agent is
//! managed, which is also what a missing file means.

use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};

const FILE_NAME: &str = "agents.json";

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct AgentSettingsFile {
	#[serde(default)]
	disabled: BTreeSet<String>,
}

fn settings_path(data_dir: &Path) -> PathBuf {
	data_dir.join(FILE_NAME)
}

/// The stored selection, or `None` when the user never saved one.
///
/// `Err` only for a file that exists and cannot be read or parsed.
pub fn read_disabled_agents_in(
	data_dir: &Path,
) -> io::Result<Option<BTreeSet<String>>> {
	let text = match std::fs::read_to_string(settings_path(data_dir)) {
		Ok(text) => text,
		Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
		Err(e) => return Err(e),
	};
	let file: AgentSettingsFile = serde_json::from_str(&text)
		.map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
	Ok(Some(file.disabled))
}

/// Replace the stored selection. Unknown ids are dropped so a stale or typo'd
/// id cannot linger in the file.
pub fn write_disabled_agents_in(
	data_dir: &Path,
	disabled: &BTreeSet<String>,
) -> io::Result<()> {
	let file = AgentSettingsFile {
		disabled: disabled
			.iter()
			.filter(|id| id.parse::<crate::AgentType>().is_ok())
			.cloned()
			.collect(),
	};
	std::fs::create_dir_all(data_dir)?;
	let path = settings_path(data_dir);
	// Write-then-rename so a crash never leaves a half-written file that
	// would read as "manage everything".
	let tmp = path.with_extension("json.tmp");
	std::fs::write(&tmp, serde_json::to_vec_pretty(&file)?)?;
	std::fs::rename(&tmp, &path)
}

/// Disabled agent ids at the shared app data root.
///
/// Fails OPEN on an unreadable file (logged): the alternative blocks every
/// mutation on a machine until someone hand-fixes a settings file.
/// ponytail: fail-open on a corrupt file, thread the error to the mutation
/// surfaces if that ever bites.
pub fn disabled_agents() -> BTreeSet<String> {
	// The lib's unit tests never read the developer's real settings: a
	// per-thread set (empty unless a test opts in) keeps them deterministic
	// without touching the env.
	#[cfg(test)]
	{
		test_override::DISABLED.with(|d| d.borrow().clone())
	}
	#[cfg(not(test))]
	{
		let dir = crate::paths::app_data_dir();
		match read_disabled_agents_in(&dir) {
			Ok(set) => set.unwrap_or_default(),
			Err(e) => {
				log::warn!(
					"ignoring unreadable agent settings in {dir:?}: {e}"
				);
				BTreeSet::new()
			}
		}
	}
}

/// [`read_disabled_agents_in`] at the shared app data root — for the surface
/// that shows the setting, which must report an unreadable file, not hide it.
pub fn read_disabled_agents() -> io::Result<Option<BTreeSet<String>>> {
	read_disabled_agents_in(&crate::paths::app_data_dir())
}

/// [`write_disabled_agents_in`] at the shared app data root.
pub fn write_disabled_agents(disabled: &BTreeSet<String>) -> io::Result<()> {
	write_disabled_agents_in(&crate::paths::app_data_dir(), disabled)
}

/// Does aghub manage this agent (i.e. may a server-chosen fan-out write to it)?
pub fn is_managed(agent_id: &str) -> bool {
	!disabled_agents().contains(agent_id)
}

#[cfg(test)]
pub(crate) mod test_override {
	use std::cell::RefCell;
	use std::collections::BTreeSet;

	thread_local! {
		pub(super) static DISABLED: RefCell<BTreeSet<String>> =
			const { RefCell::new(BTreeSet::new()) };
	}

	/// Disable `ids` on this test thread until the guard drops.
	pub(crate) fn disable(ids: &[&str]) -> impl Drop {
		struct Reset;
		impl Drop for Reset {
			fn drop(&mut self) {
				DISABLED.with(|d| d.borrow_mut().clear());
			}
		}
		DISABLED.with(|d| {
			*d.borrow_mut() = ids.iter().map(|id| id.to_string()).collect();
		});
		Reset
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn missing_file_means_never_configured() {
		let dir = tempfile::tempdir().unwrap();
		assert_eq!(read_disabled_agents_in(dir.path()).unwrap(), None);
	}

	#[test]
	fn round_trips_and_drops_unknown_ids() {
		let dir = tempfile::tempdir().unwrap();
		let wanted: BTreeSet<String> =
			["copilot", "not-an-agent"].map(String::from).into();
		write_disabled_agents_in(dir.path(), &wanted).unwrap();
		assert_eq!(
			read_disabled_agents_in(dir.path()).unwrap(),
			Some(BTreeSet::from(["copilot".to_string()]))
		);
	}

	#[test]
	fn an_empty_selection_is_still_configured() {
		let dir = tempfile::tempdir().unwrap();
		write_disabled_agents_in(dir.path(), &BTreeSet::new()).unwrap();
		assert_eq!(
			read_disabled_agents_in(dir.path()).unwrap(),
			Some(BTreeSet::new())
		);
	}
}
