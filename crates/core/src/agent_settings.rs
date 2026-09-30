//! Which agents the user has turned ON — the ONE home of that choice.
//!
//! Stored as an ALLOW-list: an agent aghub has never been told to manage —
//! one a later release adds, or one that only appeared on this machine after
//! the choice was saved — stays off until the user turns it on. Everything
//! outside the list is "disabled": no flow that picks its own agent set
//! (repair, update, git sync, rename, delete-from-all) writes into its
//! directories. A disabled agent is unmanaged, so it is not counted as an
//! unselected reader of a shared slot for a single-agent / comma-list delete
//! ([`crate::skills::removal::skill_dir_readers_outside`] skips it). The full
//! roster still applies to Master retention (a disabled agent's Referrer keeps
//! a Master alive on `--all-agents`, see `unmanaged_skill_dirs`/`plan_removal`),
//! to the "is anyone besides the initiator here" shortcut in
//! `unselected_reader_needs_referrer` (the initiator may itself be disabled),
//! to transfer's delete-ordering key
//! (`removal::slot_reader_count`; slot sharing is structural),
//! and to `load_all_agents` style "does anyone else hold this" questions. A
//! real (non-link) directory in a universal store is still refused by a
//! single-agent delete whoever reads it (the API's delete-by-path is the one
//! documented exception).
//!
//! Persisted under [`crate::paths::app_data_dir`] so every surface (desktop,
//! CLI, a remote's aghub-api) reads the same answer. A missing file means
//! nobody chose yet, and then every agent is managed (the CLI-only default;
//! the desktop seeds a real choice on first run).

use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};

const FILE_NAME: &str = "agents.json";

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct AgentSettingsFile {
	#[serde(default, skip_serializing_if = "Option::is_none")]
	enabled: Option<BTreeSet<String>>,
	/// v2.34.0 stored the complement. Read once, never written again.
	#[serde(default, skip_serializing)]
	disabled: Option<BTreeSet<String>>,
}

fn settings_path(data_dir: &Path) -> PathBuf {
	data_dir.join(FILE_NAME)
}

fn all_ids() -> impl Iterator<Item = String> {
	crate::AgentType::ALL.iter().map(|a| a.as_str().to_string())
}

/// The stored selection as the agents that are OFF, or `None` when the user
/// never saved one. Every agent outside the stored allow-list counts, so an
/// agent the choice predates is off.
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
	Ok(Some(match file.enabled {
		Some(enabled) => all_ids().filter(|id| !enabled.contains(id)).collect(),
		// A v2.34.0 file: its complement is what the user had on, so reading
		// it this way keeps their state; the next save writes an allow-list.
		None => file.disabled.unwrap_or_default(),
	}))
}

/// Replace the stored selection: every known agent NOT in `disabled` is
/// stored as on. Unknown ids simply do not appear.
pub fn write_disabled_agents_in(
	data_dir: &Path,
	disabled: &BTreeSet<String>,
) -> io::Result<()> {
	let file = AgentSettingsFile {
		enabled: Some(all_ids().filter(|id| !disabled.contains(id)).collect()),
		disabled: None,
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

/// Turn the given agents on (managed=true) or off, keeping every other agent's
/// state. The API PUT keeps its own full-replace route.
pub fn set_agents_managed_in(
	data_dir: &Path,
	agents: &[crate::AgentType],
	managed: bool,
) -> io::Result<BTreeSet<String>> {
	let mut disabled = read_disabled_agents_in(data_dir)?.unwrap_or_default();
	for agent in agents {
		let id = agent.as_str();
		if managed {
			disabled.remove(id);
		} else {
			disabled.insert(id.to_string());
		}
	}
	write_disabled_agents_in(data_dir, &disabled)?;
	Ok(disabled)
}

/// [`set_agents_managed_in`] at [`crate::paths::app_data_dir`]. The API PUT
/// keeps its own full-replace route.
pub fn set_agents_managed(
	agents: &[crate::AgentType],
	managed: bool,
) -> io::Result<BTreeSet<String>> {
	set_agents_managed_in(&crate::paths::app_data_dir(), agents, managed)
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
	use crate::AgentType;

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

	/// THE property of the allow-list: an agent the saved choice never named
	/// (a later release's, simulated by a file that omits it) is off.
	#[test]
	fn an_agent_the_choice_never_named_is_disabled() {
		let dir = tempfile::tempdir().unwrap();
		std::fs::write(dir.path().join(FILE_NAME), r#"{"enabled":["claude"]}"#)
			.unwrap();
		let disabled = read_disabled_agents_in(dir.path()).unwrap().unwrap();
		assert!(disabled.contains("augmentcode"));
		assert!(!disabled.contains("claude"));
	}

	/// A v2.34.0 file keeps meaning what it meant, and is rewritten as an
	/// allow-list on the next save.
	#[test]
	fn a_legacy_disabled_file_keeps_its_meaning() {
		let dir = tempfile::tempdir().unwrap();
		std::fs::write(
			dir.path().join(FILE_NAME),
			r#"{"disabled":["copilot"]}"#,
		)
		.unwrap();
		let disabled = read_disabled_agents_in(dir.path()).unwrap().unwrap();
		assert_eq!(disabled, BTreeSet::from(["copilot".to_string()]));

		write_disabled_agents_in(dir.path(), &disabled).unwrap();
		let text = std::fs::read_to_string(dir.path().join(FILE_NAME)).unwrap();
		assert!(text.contains("\"enabled\"") && !text.contains("\"disabled\""));
		assert_eq!(
			read_disabled_agents_in(dir.path()).unwrap(),
			Some(disabled)
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

	#[test]
	fn set_agents_managed_in_toggles_only_named_agents() {
		let dir = tempfile::tempdir().unwrap();
		let disabled =
			set_agents_managed_in(dir.path(), &[AgentType::Claude], false)
				.unwrap();
		let claude_set = BTreeSet::from(["claude".to_string()]);
		assert_eq!(disabled, claude_set);
		assert_eq!(
			read_disabled_agents_in(dir.path()).unwrap(),
			Some(claude_set)
		);

		let disabled =
			set_agents_managed_in(dir.path(), &[AgentType::Claude], true)
				.unwrap();
		let empty_set = BTreeSet::new();
		assert_eq!(disabled, empty_set);
		assert_eq!(
			read_disabled_agents_in(dir.path()).unwrap(),
			Some(empty_set)
		);
	}

	#[test]
	fn set_agents_managed_in_refuses_corrupt_file() {
		let dir = tempfile::tempdir().unwrap();
		let file = dir.path().join(FILE_NAME);
		std::fs::write(&file, b"{not json").unwrap();

		let res =
			set_agents_managed_in(dir.path(), &[AgentType::Claude], false);
		assert!(res.is_err());
		assert_eq!(std::fs::read(&file).unwrap(), b"{not json");
	}
}
