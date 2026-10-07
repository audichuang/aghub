//! Typed skill removal verdict.
//!
//! Replaces the ad-hoc `blocks` boolean and independently folded `shared_master_kept`.
//! Computed once from `read_effect_after` and plan facts.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// A skill holder location with its managed / unmanaged status.
///
/// `managed` is false when the path is in a directory that is only read by
/// disabled or unselected agents (`unmanaged_skill_dirs`), and true when it is
/// managed by aghub.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Holder {
	pub path: PathBuf,
	pub managed: bool,
}

/// Typed removal verdict, computed once from `read_effect_after` and plan facts.
///
/// Single owner of "what was taken away" across surfaces. Expresses at least
/// five states:
/// - `Removed`: the skill (or planned paths) was removed.
/// - `Kept`: deliberately kept because other readers still need it (or git-tracked).
///   Carries `still_read_from` holders classified as managed or unmanaged.
/// - `Refused`: could not proceed (e.g. `--all-agents` with surviving readers,
///   or taking nothing away while Master continues serving). Carries `reason`.
/// - `Partial`: some paths were removed, but others failed to delete.
/// - `LockOnly`: the skill had no files on disk, but was pruned from the lock.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Verdict {
	#[default]
	Removed,
	Kept {
		still_read_from: Vec<Holder>,
	},
	Refused {
		reason: String,
	},
	Partial,
	LockOnly,
}

/// Borrowed inputs for computing a [`Verdict`].
#[derive(Debug, Clone)]
pub struct VerdictInputs<'a> {
	pub plan_paths: &'a [PathBuf],
	pub plan_skipped: &'a [PathBuf],
	pub initial_shared_master_kept: bool,
	pub effect: &'a crate::skills::removal::ReadEffect,
	pub all_agents: bool,
	pub unmanaged_dirs: &'a [PathBuf],
	pub failed_paths: &'a [PathBuf],
	pub has_lock_entry: bool,
	pub git_refusal: Option<String>,
	pub readers_outside: &'a [&'static str],
}

impl Verdict {
	/// True when the removal took nothing away and kept the skill
	/// (shared master / referrer kept, or refused).
	pub fn shared_master_kept(&self) -> bool {
		matches!(self, Verdict::Kept { .. } | Verdict::Refused { .. })
	}

	/// Returns holders if the verdict is `Kept`.
	pub fn still_read_from(&self) -> Option<&[Holder]> {
		match self {
			Verdict::Kept { still_read_from } => Some(still_read_from),
			_ => None,
		}
	}

	/// Return the list of paths for wire/plan compatibility.
	pub fn still_read_from_paths(&self) -> Vec<PathBuf> {
		match self {
			Verdict::Kept { still_read_from } => {
				still_read_from.iter().map(|h| h.path.clone()).collect()
			}
			_ => Vec::new(),
		}
	}

	pub fn is_removed(&self) -> bool {
		matches!(self, Verdict::Removed)
	}

	pub fn is_refused(&self) -> bool {
		matches!(self, Verdict::Refused { .. })
	}

	pub fn is_kept(&self) -> bool {
		matches!(self, Verdict::Kept { .. })
	}

	pub fn is_partial(&self) -> bool {
		matches!(self, Verdict::Partial)
	}

	pub fn is_lock_only(&self) -> bool {
		matches!(self, Verdict::LockOnly)
	}

	/// Pure constructor for `Verdict`.
	///
	/// Computes the verdict from the plan facts, `read_effect_after` result,
	/// scope options, and lock/disk status.
	pub fn compute(inputs: VerdictInputs<'_>) -> Self {
		if !inputs.failed_paths.is_empty() {
			return Verdict::Partial;
		}

		let mut still: Vec<PathBuf> = Vec::new();
		for path in inputs
			.effect
			.survivors
			.iter()
			.chain(inputs.plan_skipped.iter())
		{
			if !still.contains(path) {
				still.push(path.clone());
			}
		}
		let still_holders: Vec<Holder> = still
			.iter()
			.map(|p| {
				let managed =
					!inputs.unmanaged_dirs.iter().any(|u| p.starts_with(u));
				Holder {
					path: p.clone(),
					managed,
				}
			})
			.collect();

		let accounted_for = |survivor: &Path| {
			let survivor =
				crate::skills::linker::classify::canonicalize_lenient(survivor);
			inputs.plan_skipped.iter().any(|kept| {
				crate::skills::linker::classify::canonicalize_lenient(kept)
					== survivor
			})
		};
		let all_survivors_reported = !inputs.initial_shared_master_kept
			&& inputs
				.effect
				.survivors
				.iter()
				.all(|survivor| accounted_for(survivor));
		let spared_everything = all_survivors_reported
			&& inputs.plan_paths.is_empty()
			&& !inputs.effect.survivors.is_empty();

		let blocks = if inputs.all_agents {
			!inputs.effect.survivors.is_empty() || inputs.effect.incomplete
		} else {
			!inputs.effect.survivors.is_empty()
				&& !inputs.effect.changed
				&& !spared_everything
		};

		if blocks {
			let where_ = still
				.iter()
				.map(|p| p.display().to_string())
				.collect::<Vec<_>>()
				.join(", ");

			let reason = if inputs.all_agents {
				let held_by_disabled = still
					.iter()
					.filter(|path| {
						inputs
							.unmanaged_dirs
							.iter()
							.any(|dir| path.starts_with(dir))
					})
					.map(|path| path.display().to_string())
					.collect::<Vec<_>>();
				let mut r = match inputs.git_refusal {
					Some(hint) => hint,
					None => format!(
						"skill still discoverable afterwards in: {where_}"
					),
				};
				if !held_by_disabled.is_empty() {
					r.push_str(&format!(
						". Read only by disabled agent(s), which --all-agents never touches: {}. \
						 Re-enable those agents or unlink these entries yourself, then retry",
						held_by_disabled.join(", ")
					));
				}
				r
			} else {
				let mut r = if let Some(hint) = inputs.git_refusal {
					hint
				} else if where_.is_empty() {
					"skill it reads from a location shared with other agents"
						.to_string()
				} else {
					format!(
						"skill it reads from a location shared with other agents; it is still \
						 served to this agent from: {where_}"
					)
				};
				if inputs.initial_shared_master_kept
					&& !inputs.readers_outside.is_empty()
				{
					let formatted = inputs.readers_outside.join(", ");
					r.push_str(&format!(
						". Also read there by agents not in this request: {formatted}. Include them in the same request, or delete for every agent (--all-agents, which also unlinks it for them)"
					));
				}
				r
			};
			return Verdict::Refused { reason };
		}

		let is_kept = spared_everything
			|| (inputs.all_agents
				&& inputs.initial_shared_master_kept
				&& inputs.plan_paths.is_empty());
		if is_kept {
			return Verdict::Kept {
				still_read_from: still_holders,
			};
		}

		if inputs.plan_paths.is_empty()
			&& inputs.effect.survivors.is_empty()
			&& inputs.has_lock_entry
		{
			return Verdict::LockOnly;
		}

		Verdict::Removed
	}
}

impl From<bool> for Verdict {
	fn from(blocks: bool) -> Self {
		if blocks {
			Verdict::Refused {
				reason: String::new(),
			}
		} else {
			Verdict::Removed
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::ffi::OsString;
	use std::sync::MutexGuard;

	struct TestEnv {
		_guard: MutexGuard<'static, ()>,
		prev_vars: Vec<(&'static str, Option<OsString>)>,
	}

	impl TestEnv {
		fn new(tmp: &Path) -> Self {
			let guard = crate::skills::prune::test_lock::env_lock()
				.lock()
				.unwrap_or_else(|e| e.into_inner());
			let home = tmp.join("home");
			let data = tmp.join("data");
			std::fs::create_dir_all(&home).unwrap();
			std::fs::create_dir_all(&data).unwrap();

			let mut vars = vec![
				("HOME", std::env::var_os("HOME")),
				("AGHUB_DATA_DIR", std::env::var_os("AGHUB_DATA_DIR")),
				("XDG_CONFIG_HOME", std::env::var_os("XDG_CONFIG_HOME")),
				("XDG_STATE_HOME", std::env::var_os("XDG_STATE_HOME")),
				("XDG_DATA_HOME", std::env::var_os("XDG_DATA_HOME")),
			];
			for var in crate::PATH_OVERRIDE_VARS {
				vars.push((*var, std::env::var_os(var)));
				std::env::remove_var(var);
			}

			std::env::set_var("HOME", &home);
			std::env::set_var("AGHUB_DATA_DIR", &data);
			std::env::set_var("XDG_CONFIG_HOME", home.join(".config"));
			std::env::set_var("XDG_STATE_HOME", home.join(".local/state"));
			std::env::set_var("XDG_DATA_HOME", home.join(".local/share"));

			Self {
				_guard: guard,
				prev_vars: vars,
			}
		}
	}

	impl Drop for TestEnv {
		fn drop(&mut self) {
			for (key, val) in &self.prev_vars {
				match val {
					Some(v) => std::env::set_var(key, v),
					None => std::env::remove_var(key),
				}
			}
		}
	}

	fn write_test_skill(dir: &Path, name: &str) {
		std::fs::create_dir_all(dir).unwrap();
		std::fs::write(
			dir.join("SKILL.md"),
			format!("---\nname: {name}\ndescription: test\n---\n# {name}\n"),
		)
		.unwrap();
	}

	fn write_test_lock_entry(name: &str) {
		let entry = skill::SkillLockEntry::new(
			"owner/repo".to_string(),
			"github".to_string(),
			"https://github.com/owner/repo".to_string(),
			None,
			None,
			"hash123".to_string(),
			None,
		);
		skill::lock::add_skill_to_lock(name, entry).unwrap();
	}

	#[test]
	fn test_verdict_table() {
		let tmp = tempfile::tempdir().unwrap();
		let _env = TestEnv::new(tmp.path());

		let claude_dir = tmp.path().join("home/.claude/skills");
		let cursor_dir = tmp.path().join("home/.cursor/skills");
		let shared_dir = tmp.path().join("home/.agents/skills");
		let aghub_dir = tmp.path().join("home/.aghub");

		// --- Row 1: Removed (clean removal of single-agent copy) ---
		{
			let skill_name = "row1-removed";
			let path = claude_dir.join(skill_name);
			write_test_skill(&path, skill_name);
			write_test_lock_entry(skill_name);

			let read_dirs = vec![claude_dir.clone()];
			let plan_paths = vec![path.clone()];
			let effect = crate::skills::removal::read_effect_after(
				&read_dirs,
				skill_name,
				&plan_paths,
			);

			// Execution: file deleted from disk, lock entry pruned
			std::fs::remove_dir_all(&path).unwrap();
			skill::lock::remove_skill_from_lock(skill_name).unwrap();

			let verdict = Verdict::compute(VerdictInputs {
				plan_paths: &plan_paths,
				plan_skipped: &[],
				initial_shared_master_kept: false,
				effect: &effect,
				all_agents: false,
				unmanaged_dirs: &[],
				failed_paths: &[],
				has_lock_entry: false,
				git_refusal: None,
				readers_outside: &[],
			});

			assert_eq!(verdict, Verdict::Removed, "Row 1 must be Removed");
			assert!(!path.exists(), "Row 1: disk directory must be removed");
			assert!(
				!skill::read_skill_lock().skills.contains_key(skill_name),
				"Row 1: lock entry must be pruned"
			);
		}

		// --- Row 2: Removed (private copy shadows Master - disclosing Master) ---
		{
			let skill_name = "row2-shadow";
			let master_path = aghub_dir.join(skill_name);
			let private_path = claude_dir.join(skill_name);
			write_test_skill(&master_path, skill_name);
			write_test_skill(&private_path, skill_name);
			write_test_lock_entry(skill_name);

			let read_dirs = vec![claude_dir.clone(), aghub_dir.clone()];
			let plan_paths = vec![private_path.clone()];
			let plan_skipped = vec![master_path.clone()];
			let effect = crate::skills::removal::read_effect_after(
				&read_dirs,
				skill_name,
				&plan_paths,
			);

			assert!(
				!effect.survivors.is_empty(),
				"Master must survive in effect"
			);
			assert!(
				effect.changed,
				"Shrinking read set must set effect.changed = true"
			);

			// Execution: only private copy is deleted; Master is preserved
			std::fs::remove_dir_all(&private_path).unwrap();

			let verdict = Verdict::compute(VerdictInputs {
				plan_paths: &plan_paths,
				plan_skipped: &plan_skipped,
				initial_shared_master_kept: false,
				effect: &effect,
				all_agents: false,
				unmanaged_dirs: &[],
				failed_paths: &[],
				has_lock_entry: true,
				git_refusal: None,
				readers_outside: &[],
			});

			assert_eq!(
				verdict,
				Verdict::Removed,
				"Row 2 must be Removed because effect.changed is true (private copy removed)"
			);
			assert!(
				!private_path.exists(),
				"Row 2: private copy must be deleted"
			);
			assert!(master_path.exists(), "Row 2: Master must survive on disk");
		}

		// --- Row 3: Kept (with managed and unmanaged holders) ---
		{
			let skill_name = "row3-kept";
			let managed_path = shared_dir.join(skill_name);
			let unmanaged_path = cursor_dir.join(skill_name);
			write_test_skill(&managed_path, skill_name);
			write_test_skill(&unmanaged_path, skill_name);
			write_test_lock_entry(skill_name);

			// Cursor is unmanaged
			let unmanaged_dirs = vec![cursor_dir.clone()];
			let read_dirs = vec![shared_dir.clone(), cursor_dir.clone()];
			let plan_paths: Vec<PathBuf> = vec![];
			let plan_skipped =
				vec![managed_path.clone(), unmanaged_path.clone()];
			let effect = crate::skills::removal::read_effect_after(
				&read_dirs,
				skill_name,
				&plan_paths,
			);

			let verdict = Verdict::compute(VerdictInputs {
				plan_paths: &plan_paths,
				plan_skipped: &plan_skipped,
				initial_shared_master_kept: false, // initial_shared_master_kept: false -> spared_everything
				effect: &effect,
				all_agents: false,
				unmanaged_dirs: &unmanaged_dirs,
				failed_paths: &[],
				has_lock_entry: true,
				git_refusal: None,
				readers_outside: &[],
			});

			match &verdict {
				Verdict::Kept { still_read_from } => {
					assert_eq!(
						still_read_from.len(),
						2,
						"Row 3 must record 2 holders"
					);
					let managed_holder = still_read_from
						.iter()
						.find(|h| h.path == managed_path)
						.expect("managed holder must exist");
					assert!(
						managed_holder.managed,
						"shared dir must be categorized as managed"
					);

					let unmanaged_holder = still_read_from
						.iter()
						.find(|h| h.path == unmanaged_path)
						.expect("unmanaged holder must exist");
					assert!(
						!unmanaged_holder.managed,
						"cursor dir must be categorized as unmanaged"
					);
				}
				other => panic!("Row 3: expected Kept, got {other:?}"),
			}

			// Disk and lock assertions: files and lock entry remain untouched
			assert!(managed_path.exists(), "Row 3: managed path must remain");
			assert!(
				unmanaged_path.exists(),
				"Row 3: unmanaged path must remain"
			);
			assert!(
				skill::read_skill_lock().skills.contains_key(skill_name),
				"Row 3: lock entry must remain"
			);
		}

		// --- Row 4: Refused (all_agents encountered survivors) ---
		{
			let skill_name = "row4-refused";
			let path = cursor_dir.join(skill_name);
			write_test_skill(&path, skill_name);
			write_test_lock_entry(skill_name);

			let read_dirs = vec![cursor_dir.clone()];
			let plan_paths: Vec<PathBuf> = vec![];
			let effect = crate::skills::removal::read_effect_after(
				&read_dirs,
				skill_name,
				&plan_paths,
			);

			let verdict = Verdict::compute(VerdictInputs {
				plan_paths: &plan_paths,
				plan_skipped: &[],
				initial_shared_master_kept: false,
				effect: &effect,
				all_agents: true, // all_agents = true with survivors
				unmanaged_dirs: &[],
				failed_paths: &[],
				has_lock_entry: true,
				git_refusal: None,
				readers_outside: &[],
			});

			match &verdict {
				Verdict::Refused { reason } => {
					assert!(
						reason.contains("skill still discoverable afterwards in:"),
						"Row 4: reason must report remaining locations: {reason}"
					);
					assert!(
						reason.contains(&path.display().to_string()),
						"Row 4: reason must list the survivor path: {reason}"
					);
				}
				other => panic!("Row 4: expected Refused, got {other:?}"),
			}

			// Disk and lock assertions: files and lock entry remain untouched
			assert!(path.exists(), "Row 4: file must remain on disk");
			assert!(
				skill::read_skill_lock().skills.contains_key(skill_name),
				"Row 4: lock entry must remain"
			);
		}

		// --- Row 5: Partial (failed paths during deletion) ---
		{
			let skill_name = "row5-partial";
			let p1 = claude_dir.join(skill_name);
			let p2 = cursor_dir.join(skill_name);
			write_test_skill(&p1, skill_name);
			write_test_skill(&p2, skill_name);
			write_test_lock_entry(skill_name);

			// p1 deleted, p2 failed
			std::fs::remove_dir_all(&p1).unwrap();
			let failed_paths = vec![p2.clone()];

			let effect = crate::skills::removal::read_effect_after(
				&[claude_dir.clone(), cursor_dir.clone()],
				skill_name,
				&[p1.clone(), p2.clone()],
			);

			let verdict = Verdict::compute(VerdictInputs {
				plan_paths: &[p1.clone(), p2.clone()],
				plan_skipped: &[],
				initial_shared_master_kept: false,
				effect: &effect,
				all_agents: false,
				unmanaged_dirs: &[],
				failed_paths: &failed_paths,
				has_lock_entry: true,
				git_refusal: None,
				readers_outside: &[],
			});

			assert_eq!(verdict, Verdict::Partial, "Row 5 must be Partial");
			assert!(!p1.exists(), "Row 5: p1 was deleted");
			assert!(p2.exists(), "Row 5: p2 failed and remains on disk");
		}

		// --- Row 6: LockOnly (skill not on disk, pruned from lock) ---
		{
			let skill_name = "row6-lockonly";
			write_test_lock_entry(skill_name);

			let read_dirs = vec![claude_dir.clone()];
			let plan_paths: Vec<PathBuf> = vec![];
			let effect = crate::skills::removal::read_effect_after(
				&read_dirs,
				skill_name,
				&plan_paths,
			);

			// Lock pruned
			skill::lock::remove_skill_from_lock(skill_name).unwrap();

			let verdict = Verdict::compute(VerdictInputs {
				plan_paths: &plan_paths,
				plan_skipped: &[],
				initial_shared_master_kept: false,
				effect: &effect,
				all_agents: false,
				unmanaged_dirs: &[],
				failed_paths: &[],
				has_lock_entry: true, // had lock entry
				git_refusal: None,
				readers_outside: &[],
			});

			assert_eq!(verdict, Verdict::LockOnly, "Row 6 must be LockOnly");
			assert!(
				!claude_dir.join(skill_name).exists(),
				"Row 6: no disk path"
			);
			assert!(
				!skill::read_skill_lock().skills.contains_key(skill_name),
				"Row 6: lock entry was pruned"
			);
		}
	}

	#[test]
	fn test_all_agents_refuses_any_survivor_with_locations() {
		let tmp = tempfile::tempdir().unwrap();
		let _env = TestEnv::new(tmp.path());

		let dir1 = tmp.path().join("home/.claude/skills");
		let dir2 = tmp.path().join("home/.cursor/skills");
		write_test_skill(&dir1.join("multi"), "multi");
		write_test_skill(&dir2.join("multi"), "multi");

		let effect = crate::skills::removal::read_effect_after(
			&[dir1.clone(), dir2.clone()],
			"multi",
			&[dir1.join("multi")], // only dir1 planned
		);

		let verdict = Verdict::compute(VerdictInputs {
			plan_paths: &[dir1.join("multi")],
			plan_skipped: &[],
			initial_shared_master_kept: false,
			effect: &effect,
			all_agents: true,
			unmanaged_dirs: &[],
			failed_paths: &[],
			has_lock_entry: false,
			git_refusal: None,
			readers_outside: &[],
		});

		match verdict {
			Verdict::Refused { reason } => {
				assert!(
					reason.contains("skill still discoverable afterwards in:"),
					"Must report discoverable locations"
				);
				assert!(
					reason.contains(&dir2.join("multi").display().to_string()),
					"Must list surviving location: {reason}"
				);
			}
			other => panic!(
				"Expected Refused for all_agents with survivor, got {other:?}"
			),
		}
	}

	#[test]
	fn test_preview_and_commit_yield_identical_verdict() {
		let tmp = tempfile::tempdir().unwrap();
		let _env = TestEnv::new(tmp.path());

		let root = tmp.path().join("proj");
		let skill_dir = root.join(".claude/skills/demo");
		write_test_skill(&skill_dir, "demo");

		let mut mgr_preview = crate::manager::ConfigManager::new(
			crate::create_adapter(crate::models::AgentType::Claude),
			false,
			Some(&root),
		);
		mgr_preview.load().unwrap();

		let preview_outcome = mgr_preview
			.remove_skill_planned("demo", false, true, false)
			.unwrap();

		let mut mgr_commit = crate::manager::ConfigManager::new(
			crate::create_adapter(crate::models::AgentType::Claude),
			false,
			Some(&root),
		);
		mgr_commit.load().unwrap();

		let commit_outcome = mgr_commit
			.remove_skill_planned("demo", false, false, true)
			.unwrap();

		assert_eq!(
			preview_outcome.verdict,
			commit_outcome.verdict,
			"Preview and commit on the same fixture must yield the identical Verdict"
		);
		assert_eq!(preview_outcome.verdict, Verdict::Removed);
	}
}
