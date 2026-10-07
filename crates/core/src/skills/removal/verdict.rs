//! Typed skill removal verdict.
//!
//! Replaces the ad-hoc `blocks` boolean and independently folded `shared_master_kept`.
//! Computed once from `read_effect_after` and plan facts.

use std::path::{Path, PathBuf};

/// A skill holder location with its managed / unmanaged status.
///
/// `managed` is false when the path is in a directory that is only read by
/// disabled or unselected agents (`unmanaged_skill_dirs`), and true when it is
/// managed by aghub.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Holder {
	pub path: PathBuf,
	pub managed: bool,
}

/// Typed removal verdict, computed once from `read_effect_after` and plan facts.
///
/// Single owner of "what was taken away" across surfaces. Expresses at least
/// six states:
/// - `Removed`: the skill (or planned paths) was removed.
/// - `Kept`: deliberately kept because other readers still need it (or git-tracked).
///   Carries `still_read_from` holders classified as managed or unmanaged.
/// - `Refused`: could not proceed (e.g. `--all-agents` with surviving readers,
///   or taking nothing away while Master continues serving). Carries `reason`.
/// - `Partial`: some paths were removed, but others failed to delete. Downgraded
///   exclusively by `RemovalOutcome::commit`.
/// - `LockOnly`: no files on disk, an in-scope lock entry remains (not pruned).
/// - `Absent`: the skill had no files on disk and no lock entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
	Removed,
	Kept { still_read_from: Vec<Holder> },
	Refused { reason: String },
	Partial,
	LockOnly,
	Absent,
}

/// Order-preserving union of survivors and skipped paths.
pub fn still_read_from(
	survivors: &[PathBuf],
	skipped: &[PathBuf],
) -> Vec<PathBuf> {
	let mut still: Vec<PathBuf> = Vec::new();
	for path in survivors.iter().chain(skipped.iter()) {
		if !still.contains(path) {
			still.push(path.clone());
		}
	}
	still
}

/// Borrowed inputs for computing a [`Verdict`].
pub struct VerdictInputs<'a> {
	pub plan_paths: &'a [PathBuf],
	pub plan_skipped: &'a [PathBuf],
	pub initial_shared_master_kept: bool,
	pub effect: &'a crate::skills::removal::ReadEffect,
	pub all_agents: bool,
	pub unmanaged_dirs: &'a [PathBuf],
	pub git_refusal: &'a dyn Fn() -> Option<String>,
	pub readers_outside: &'a dyn Fn() -> Vec<&'static str>,
}

impl Verdict {
	/// Convenience constructor for a single managed holder kept.
	pub fn kept_managed(path: PathBuf) -> Self {
		Verdict::Kept {
			still_read_from: vec![Holder {
				path,
				managed: true,
			}],
		}
	}

	/// True when the removal took nothing away and kept the skill
	/// (shared master / referrer kept, or refused).
	pub fn shared_master_kept(&self) -> bool {
		matches!(self, Verdict::Kept { .. } | Verdict::Refused { .. })
	}

	/// Returns the paths the skill is still served from or read by, if any.
	pub fn still_read_from_paths(&self) -> Vec<PathBuf> {
		match self {
			Verdict::Kept { still_read_from } => {
				still_read_from.iter().map(|h| h.path.clone()).collect()
			}
			Verdict::Refused { reason } => {
				if let Some(idx) =
					reason.find("; it is still served to this agent from: ")
				{
					let tail = &reason[idx
						+ "; it is still served to this agent from: ".len()..];
					let paths_str = tail
						.split(". Also read there")
						.next()
						.unwrap_or(tail)
						.trim_end_matches('.');
					paths_str
						.split(", ")
						.map(|s| PathBuf::from(s.trim()))
						.filter(|p| !p.as_os_str().is_empty())
						.collect()
				} else if let Some(idx) =
					reason.find("skill still discoverable afterwards in: ")
				{
					let tail = &reason[idx
						+ "skill still discoverable afterwards in: ".len()..];
					let paths_str = tail
						.split(". Read only")
						.next()
						.unwrap_or(tail)
						.trim_end_matches('.');
					paths_str
						.split(", ")
						.map(|s| PathBuf::from(s.trim()))
						.filter(|p| !p.as_os_str().is_empty())
						.collect()
				} else {
					Vec::new()
				}
			}
			_ => Vec::new(),
		}
	}

	/// Pure constructor for `Verdict`.
	///
	/// Computes the verdict from the plan facts, `read_effect_after` result,
	/// scope options, and disk status.
	pub fn compute(inputs: VerdictInputs<'_>) -> Self {
		let still =
			still_read_from(&inputs.effect.survivors, inputs.plan_skipped);
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

		// `--all-agents` asserts a POSTCONDITION, so an unreadable read dir
		// blocks too. The single-agent branch ignores `incomplete`: it decides
		// whether to REFUSE, and one odd sibling must not make a skill undeletable.
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

			// Note: git_refusal and readers_outside closures run eagerly on blocked
			// preview so preview and commit yield identical Verdict. Dry-run and
			// transfer preflight pay this probe cost deliberately.
			let git_refusal = (inputs.git_refusal)();
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
				let mut r = match git_refusal {
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
				// Name the paths, not just the other agents: a leftover
				// Referrer in this agent's own second read dir is what the user
				// can act on.
				// See docs/history/core-skills-shape.md#antigravity-write-slot-moved-and-left-a-compat-link
				let mut r = if let Some(hint) = git_refusal {
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
				// The gate keeps npx-era/compat leftover refusals (shared_referrer_kept=false)
				// on their original message instead of listing unrelated readers;
				// see docs/history/core-skills-shape.md#antigravity-write-slot-moved-and-left-a-compat-link
				let readers_outside = (inputs.readers_outside)();
				if inputs.initial_shared_master_kept
					&& !readers_outside.is_empty()
				{
					let formatted = readers_outside.join(", ");
					r.push_str(&format!(
						". Also read there by agents not in this request: {formatted}. Include them in the same request, or delete for every agent (--all-agents, which also unlinks it for them)"
					));
				}
				r
			};
			return Verdict::Refused { reason };
		}

		// The second disjunct is the planner's OWN keep, which `blocks` cannot
		// always see (discovery stops at a parsing `SKILL.md`, the planner's
		// sweep recurses into it). Both single-agent and --all-agents treat an
		// empty plan with an initial keep as Kept: previously single-agent
		// reached commit which would falsely report `executed: true` and prune
		// the lock for a kept master; now both return preview with `Verdict::Kept`,
		// `executed: false`, and no lock prune.
		// See docs/history/core-manager.md#nested-broken-link-reached-commit
		let is_kept = spared_everything
			|| (inputs.initial_shared_master_kept
				&& inputs.plan_paths.is_empty());
		if is_kept {
			return Verdict::Kept {
				still_read_from: still_holders,
			};
		}

		Verdict::Removed
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
		let no_refusal: &dyn Fn() -> Option<String> = &|| None;
		let no_readers: &dyn Fn() -> Vec<&'static str> = &|| Vec::new();

		// Row 1: Removed (single-agent planned paths)
		{
			let p = PathBuf::from("/home/user/.claude/skills/demo");
			let effect = crate::skills::removal::ReadEffect {
				survivors: vec![],
				changed: true,
				incomplete: false,
			};
			let verdict = Verdict::compute(VerdictInputs {
				plan_paths: &[p],
				plan_skipped: &[],
				initial_shared_master_kept: false,
				effect: &effect,
				all_agents: false,
				unmanaged_dirs: &[],
				git_refusal: no_refusal,
				readers_outside: no_readers,
			});
			assert_eq!(verdict, Verdict::Removed);
		}

		// Row 2: Removed (private copy shadows Master - disclosing Master)
		{
			let private_p = PathBuf::from("/project/.cursor/skills/demo");
			let master_p = PathBuf::from("/project/.agents/skills/demo");
			let effect = crate::skills::removal::ReadEffect {
				survivors: vec![master_p.clone()],
				changed: true,
				incomplete: false,
			};
			let verdict = Verdict::compute(VerdictInputs {
				plan_paths: &[private_p],
				plan_skipped: &[master_p],
				initial_shared_master_kept: false,
				effect: &effect,
				all_agents: false,
				unmanaged_dirs: &[],
				git_refusal: no_refusal,
				readers_outside: no_readers,
			});
			assert_eq!(verdict, Verdict::Removed);
		}

		// Row 3: Kept (with managed and unmanaged holders)
		{
			let managed_p = PathBuf::from("/home/user/.agents/skills/demo");
			let unmanaged_p = PathBuf::from("/home/user/.cursor/skills/demo");
			let unmanaged_dirs =
				vec![PathBuf::from("/home/user/.cursor/skills")];
			let effect = crate::skills::removal::ReadEffect {
				survivors: vec![managed_p.clone(), unmanaged_p.clone()],
				changed: false,
				incomplete: false,
			};
			let verdict = Verdict::compute(VerdictInputs {
				plan_paths: &[],
				plan_skipped: &[managed_p.clone(), unmanaged_p.clone()],
				initial_shared_master_kept: false,
				effect: &effect,
				all_agents: false,
				unmanaged_dirs: &unmanaged_dirs,
				git_refusal: no_refusal,
				readers_outside: no_readers,
			});
			match verdict {
				Verdict::Kept { still_read_from } => {
					assert_eq!(still_read_from.len(), 2);
					let m = still_read_from
						.iter()
						.find(|h| h.path == managed_p)
						.unwrap();
					assert!(m.managed);
					let u = still_read_from
						.iter()
						.find(|h| h.path == unmanaged_p)
						.unwrap();
					assert!(!u.managed);
				}
				other => panic!("expected Kept, got {other:?}"),
			}
		}

		// Row 4: Refused (all_agents encountered survivors)
		{
			let survivor_p = PathBuf::from("/home/user/.cursor/skills/demo");
			let effect = crate::skills::removal::ReadEffect {
				survivors: vec![survivor_p.clone()],
				changed: false,
				incomplete: false,
			};
			let verdict = Verdict::compute(VerdictInputs {
				plan_paths: &[],
				plan_skipped: &[],
				initial_shared_master_kept: false,
				effect: &effect,
				all_agents: true,
				unmanaged_dirs: &[],
				git_refusal: no_refusal,
				readers_outside: no_readers,
			});
			match verdict {
				Verdict::Refused { reason } => {
					assert!(reason
						.contains("skill still discoverable afterwards in:"));
					assert!(reason.contains(&survivor_p.display().to_string()));
				}
				other => panic!("expected Refused, got {other:?}"),
			}
		}

		// Row 5: Kept (single agent planner keep corner, MINOR 8)
		{
			let effect = crate::skills::removal::ReadEffect {
				survivors: vec![],
				changed: false,
				incomplete: false,
			};
			let verdict = Verdict::compute(VerdictInputs {
				plan_paths: &[],
				plan_skipped: &[],
				initial_shared_master_kept: true,
				effect: &effect,
				all_agents: false,
				unmanaged_dirs: &[],
				git_refusal: no_refusal,
				readers_outside: no_readers,
			});
			assert!(matches!(verdict, Verdict::Kept { .. }));
		}

		// Row 6: Removed (empty plan and no survivors falls through to Removed)
		{
			let effect = crate::skills::removal::ReadEffect {
				survivors: vec![],
				changed: false,
				incomplete: false,
			};
			let verdict = Verdict::compute(VerdictInputs {
				plan_paths: &[],
				plan_skipped: &[],
				initial_shared_master_kept: false,
				effect: &effect,
				all_agents: false,
				unmanaged_dirs: &[],
				git_refusal: no_refusal,
				readers_outside: no_readers,
			});
			assert_eq!(verdict, Verdict::Removed);
		}

		// Row 7: LockOnly and Absent are exclusive to no-op outcomes
		{
			assert_eq!(
				crate::skills::removal::RemovalOutcome::noop(true).verdict,
				Verdict::LockOnly
			);
			assert_eq!(
				crate::skills::removal::RemovalOutcome::noop(false).verdict,
				Verdict::Absent
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
			&[dir1.join("multi")],
		);

		let verdict = Verdict::compute(VerdictInputs {
			plan_paths: &[dir1.join("multi")],
			plan_skipped: &[],
			initial_shared_master_kept: false,
			effect: &effect,
			all_agents: true,
			unmanaged_dirs: &[],
			git_refusal: &|| None,
			readers_outside: &|| Vec::new(),
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
	fn test_verdict_compute_kept_unmanaged_holder_real_fs() {
		let tmp = tempfile::tempdir().unwrap();
		let _env = TestEnv::new(tmp.path());

		let unmanaged_dir = tmp.path().join("home/.cursor/skills");
		let unmanaged_skill = unmanaged_dir.join("demo");
		write_test_skill(&unmanaged_skill, "demo");

		let effect = crate::skills::removal::read_effect_after(
			std::slice::from_ref(&unmanaged_dir),
			"demo",
			&[],
		);

		let verdict = Verdict::compute(VerdictInputs {
			plan_paths: &[],
			plan_skipped: std::slice::from_ref(&unmanaged_skill),
			initial_shared_master_kept: false,
			effect: &effect,
			all_agents: false,
			unmanaged_dirs: &[unmanaged_dir],
			git_refusal: &|| None,
			readers_outside: &|| Vec::new(),
		});

		match verdict {
			Verdict::Kept { still_read_from } => {
				assert_eq!(still_read_from.len(), 1);
				assert_eq!(still_read_from[0].path, unmanaged_skill);
				assert!(
					!still_read_from[0].managed,
					"holder in unmanaged directory must have managed == false"
				);
			}
			other => panic!("expected Kept, got {other:?}"),
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

	#[test]
	fn test_remove_skill_planned_shadowing_copy() {
		let tmp = tempfile::tempdir().unwrap();
		let _env = TestEnv::new(tmp.path());

		let root = tmp.path().join("project");
		std::fs::create_dir_all(&root).unwrap();

		let private = root.join(".cursor/skills/shadow");
		write_test_skill(&private, "shadow");
		std::fs::write(private.join("extra.md"), "private v1").unwrap();

		let master = root.join(".agents/skills/shadow");
		write_test_skill(&master, "shadow");
		write_test_lock_entry("shadow");

		let mut mgr = crate::manager::ConfigManager::new(
			crate::create_adapter(crate::models::AgentType::Cursor),
			false,
			Some(&root),
		);
		mgr.load().unwrap();

		let preview = mgr
			.remove_skill_planned("shadow", false, true, false)
			.unwrap();
		assert_eq!(preview.verdict, Verdict::Removed);
		assert!(
			preview.plan.skipped.contains(&master),
			"preview must disclose surviving master in skipped: {:?}",
			preview.plan.skipped
		);
		assert!(private.exists(), "preview must not delete private copy");
		assert!(master.exists(), "preview must not delete master");
		assert!(
			skill::read_skill_lock().skills.contains_key("shadow"),
			"preview must not prune lock"
		);

		let commit = mgr
			.remove_skill_planned("shadow", false, false, true)
			.unwrap();
		assert_eq!(commit.verdict, Verdict::Removed);
		assert_eq!(preview.verdict, commit.verdict);
		assert!(
			commit.plan.skipped.contains(&master),
			"commit must disclose surviving master in skipped: {:?}",
			commit.plan.skipped
		);
		assert!(
			!private.exists(),
			"commit must delete shadowed private copy"
		);
		assert!(master.exists(), "commit must preserve Master on disk");
		assert!(
			master.join("SKILL.md").exists(),
			"Master SKILL.md must remain intact"
		);
		assert!(
			skill::read_skill_lock().skills.contains_key("shadow"),
			"lock entry must remain because Master still exists"
		);
	}

	#[cfg(unix)]
	fn perms_enforced(under: &Path) -> bool {
		use std::os::unix::fs::PermissionsExt;
		let probe = under.join(".perm-probe");
		std::fs::create_dir(&probe).unwrap();
		std::fs::set_permissions(
			&probe,
			std::fs::Permissions::from_mode(0o555),
		)
		.unwrap();
		let blocked = std::fs::write(probe.join("x"), b"x").is_err();
		std::fs::set_permissions(
			&probe,
			std::fs::Permissions::from_mode(0o755),
		)
		.unwrap();
		std::fs::remove_dir_all(&probe).ok();
		blocked
	}

	#[cfg(unix)]
	#[test]
	fn test_remove_skill_planned_kept() {
		let tmp = tempfile::tempdir().unwrap();
		let _env = TestEnv::new(tmp.path());

		let home = tmp.path().join("home");
		let aghub_dir = home.join(".aghub/kept-skill");
		write_test_skill(&aghub_dir, "kept-skill");
		write_test_lock_entry("kept-skill");

		// An unrelated skill in an in-scope agent dir with a nested link
		// whose canonicalize fails with ENOTDIR, causing the planner to
		// spare the Master.
		let healthy = home.join(".claude/skills/healthy");
		write_test_skill(&healthy, "healthy");
		let plain = home.join("regular-file");
		std::fs::write(&plain, "f").unwrap();
		std::os::unix::fs::symlink(plain.join("y"), healthy.join("broken"))
			.unwrap();

		let mut mgr = crate::manager::ConfigManager::new(
			crate::create_adapter(crate::models::AgentType::Claude),
			true,
			None,
		);
		mgr.load().unwrap();

		let preview = mgr
			.remove_skill_planned("kept-skill", true, true, false)
			.unwrap();
		match &preview.verdict {
			Verdict::Kept { still_read_from } => {
				assert_eq!(still_read_from.len(), 1);
				assert_eq!(still_read_from[0].path, aghub_dir);
				assert!(
					still_read_from[0].managed,
					"master in .aghub must be classified as managed: true"
				);
			}
			other => panic!("expected Kept, got {other:?}"),
		}
		assert!(preview.plan.paths.is_empty());

		let commit = mgr
			.remove_skill_planned("kept-skill", true, false, true)
			.unwrap();
		assert_eq!(preview.verdict, commit.verdict);
		assert!(!commit.executed, "commit executed must be false when kept");
		assert!(
			aghub_dir.exists(),
			"commit must not delete kept shared skill"
		);
		assert!(
			skill::read_skill_lock().skills.contains_key("kept-skill"),
			"lock entry must remain intact when kept"
		);
	}

	#[test]
	fn test_remove_skill_planned_refused_shared_slot() {
		let tmp = tempfile::tempdir().unwrap();
		let _env = TestEnv::new(tmp.path());

		let home = tmp.path().join("home");
		let shared_path = home.join(".agents/skills/shared-skill");
		write_test_skill(&shared_path, "shared-skill");
		write_test_lock_entry("shared-skill");

		let mut mgr = crate::manager::ConfigManager::new(
			crate::create_adapter(crate::models::AgentType::Cursor),
			true,
			None,
		);
		mgr.load().unwrap();

		let preview = mgr
			.remove_skill_planned("shared-skill", false, true, false)
			.unwrap();
		match &preview.verdict {
			Verdict::Refused { reason } => {
				assert!(
					reason.contains(
						"skill it reads from a location shared with other agents"
					),
					"refusal reason must explain shared slot: {reason}"
				);
			}
			other => panic!("expected Refused, got {other:?}"),
		}

		let err = mgr
			.remove_skill_planned("shared-skill", false, false, true)
			.unwrap_err();
		assert!(
			matches!(err, crate::errors::ConfigError::UnsupportedOperation(_)),
			"commit on refused shared slot must return UnsupportedOperation: {err:?}"
		);
		assert!(
			shared_path.exists(),
			"refused removal must preserve shared skill on disk"
		);
		assert!(
			skill::read_skill_lock().skills.contains_key("shared-skill"),
			"refused removal must preserve lock entry"
		);
	}

	/// Real-fs test verifying that survivors in unmanaged directories (held by
	/// disabled agents) cause `--all-agents` removal to be refused.
	///
	/// Note on `Holder.managed == false`:
	/// The unmanaged (`managed == false`) classification is pinned by synthetic
	/// Row 3 of `test_verdict_table` and direct `Verdict::compute` tests; through
	/// `remove_skill_planned` on a real filesystem it is structurally unreachable
	/// for Kept. In `--all-agents`, any unmanaged holder survives and triggers
	/// `blocks = true`, resulting in `Verdict::Refused` (which carries a string
	/// `reason` rather than structured `Holder` items). In single-agent removal,
	/// an unmanaged peer holder triggers a keep reason with
	/// `initial_shared_master_kept = true`, which also forces `blocks = true` and
	/// `Verdict::Refused`. Thus, `Verdict::Kept` containing a holder with
	/// `managed == false` cannot be produced through `remove_skill_planned`.
	#[cfg(unix)]
	#[test]
	fn test_remove_skill_planned_refused_disabled_agent() {
		let tmp = tempfile::tempdir().unwrap();
		let _env = TestEnv::new(tmp.path());

		// Override disabled-agent selection: disable cursor.
		let _disabled_guard =
			crate::agent_settings::test_override::disable(&["cursor"]);
		let data_dir = tmp.path().join("data");
		let mut disabled = std::collections::BTreeSet::new();
		disabled.insert("cursor".to_string());
		crate::agent_settings::write_disabled_agents_in(&data_dir, &disabled)
			.unwrap();

		let home = tmp.path().join("home");
		let master = home.join(".aghub/disabled-holder");
		write_test_skill(&master, "disabled-holder");
		write_test_lock_entry("disabled-holder");

		let claude_dir = home.join(".claude/skills");
		let cursor_dir = home.join(".cursor/skills");
		std::fs::create_dir_all(&claude_dir).unwrap();
		std::fs::create_dir_all(&cursor_dir).unwrap();

		let claude_skill = claude_dir.join("disabled-holder");
		let cursor_skill = cursor_dir.join("disabled-holder");
		std::os::unix::fs::symlink(&master, &claude_skill).unwrap();
		std::os::unix::fs::symlink(&master, &cursor_skill).unwrap();

		let mut mgr = crate::manager::ConfigManager::new(
			crate::create_adapter(crate::models::AgentType::Claude),
			true,
			None,
		);
		mgr.load().unwrap();

		let preview = mgr
			.remove_skill_planned("disabled-holder", true, true, false)
			.unwrap();
		match &preview.verdict {
			Verdict::Refused { reason } => {
				assert!(
					reason.contains(
						"Read only by disabled agent(s), which --all-agents never touches:"
					),
					"must identify survivor held by disabled agent: {reason}"
				);
				assert!(
					reason.contains(&cursor_skill.display().to_string()),
					"must report exact unmanaged holder path: {reason}"
				);
			}
			other => panic!("expected Refused, got {other:?}"),
		}

		let err = mgr
			.remove_skill_planned("disabled-holder", true, false, true)
			.unwrap_err();
		assert!(
			matches!(err, crate::errors::ConfigError::UnsupportedOperation(_)),
			"commit on refused removal must error with UnsupportedOperation: {err:?}"
		);
		assert!(
			claude_skill.exists(),
			"refused removal must preserve files on disk"
		);
		assert!(
			cursor_skill.exists(),
			"refused removal must preserve unmanaged files on disk"
		);
		assert!(
			master.exists(),
			"refused removal must preserve master on disk"
		);
		assert!(
			skill::read_skill_lock()
				.skills
				.contains_key("disabled-holder"),
			"refused removal must preserve lock entry"
		);
	}

	#[cfg(unix)]
	#[test]
	fn test_remove_skill_planned_partial() {
		use std::os::unix::fs::PermissionsExt;

		let tmp = tempfile::tempdir().unwrap();
		let _env = TestEnv::new(tmp.path());

		if !perms_enforced(tmp.path()) {
			eprintln!("skipping: 0o555 not enforced (running as root)");
			return;
		}

		let home = tmp.path().join("home");
		let claude_dir = home.join(".claude/skills/partial-skill");
		write_test_skill(&claude_dir, "partial-skill");
		write_test_lock_entry("partial-skill");

		let mut mgr = crate::manager::ConfigManager::new(
			crate::create_adapter(crate::models::AgentType::Claude),
			true,
			None,
		);
		mgr.load().unwrap();

		let parent = claude_dir.parent().unwrap();
		let orig_perms = std::fs::metadata(parent).unwrap().permissions();
		std::fs::set_permissions(
			parent,
			std::fs::Permissions::from_mode(0o555),
		)
		.unwrap();
		let commit_res =
			mgr.remove_skill_planned("partial-skill", false, false, true);
		std::fs::set_permissions(parent, orig_perms).unwrap();

		let outcome = commit_res.unwrap();
		assert_eq!(outcome.verdict, Verdict::Partial);
		assert!(outcome.executed, "executed must be true for commit attempt");
		assert!(
			!outcome.failed_paths.is_empty(),
			"failed_paths must record the failed deletion"
		);
		assert_eq!(outcome.failed_paths, vec![claude_dir.clone()]);
		assert_eq!(outcome.plan.skipped, outcome.failed_paths);
		assert!(
			claude_dir.exists(),
			"path that failed to delete must remain on disk"
		);
		assert!(
			!outcome.verdict.shared_master_kept(),
			"partial is not shared_master_kept"
		);
		// The directory itself remains on disk because rmdir failed (parent was 0o555),
		// but remove_dir_all already unlinked SKILL.md inside it. Consequently, the
		// post-removal prune scan does not recognize the gutted directory as a valid skill
		// (top_level_skill_dirs requires SKILL.md), treating the lock entry as orphaned.
		assert!(
			!claude_dir.join("SKILL.md").exists(),
			"remove_dir_all unlinked contents before rmdir failed"
		);
		assert_eq!(
			outcome.prune,
			crate::skills::removal::PruneStatus::Pruned(vec![
				"partial-skill".to_string()
			]),
			"prune dropped lock entry because SKILL.md was deleted before rmdir failed"
		);
		assert!(
			!skill::read_skill_lock()
				.skills
				.contains_key("partial-skill"),
			"lock entry is pruned because surviving dir without SKILL.md is not recognized as a skill"
		);
	}

	#[test]
	fn test_remove_skill_planned_lock_only() {
		let tmp = tempfile::tempdir().unwrap();
		let _env = TestEnv::new(tmp.path());

		let skill_name = "lockonly-skill";
		write_test_lock_entry(skill_name);
		assert!(
			skill::read_skill_lock().skills.contains_key(skill_name),
			"lock entry must exist initially"
		);

		let mut mgr = crate::manager::ConfigManager::new(
			crate::create_adapter(crate::models::AgentType::Claude),
			true,
			None,
		);
		mgr.load().unwrap();

		// Skill absent from disk: core must return ResourceNotFound
		let preview_err = mgr
			.remove_skill_planned(skill_name, false, true, false)
			.unwrap_err();
		assert!(
			matches!(
				preview_err,
				crate::errors::ConfigError::ResourceNotFound { .. }
			),
			"absent skill must return ResourceNotFound: {preview_err:?}"
		);

		let commit_err = mgr
			.remove_skill_planned(skill_name, false, false, true)
			.unwrap_err();
		assert!(
			matches!(
				commit_err,
				crate::errors::ConfigError::ResourceNotFound { .. }
			),
			"absent skill must return ResourceNotFound: {commit_err:?}"
		);

		// Producer for surfaces: skill_noop_outcome yields Verdict::LockOnly.
		let noop = mgr.skill_noop_outcome(skill_name);
		assert_eq!(noop.verdict, Verdict::LockOnly);
		assert!(!noop.executed, "no-op must not report execution");
		assert!(noop.absent, "no-op must have absent set");
		assert!(noop.plan.paths.is_empty());

		// Wire view mapping must report absent, executed: false.
		let view = crate::dto::RemovalView::from_outcome(&noop, false);
		assert_eq!(
			view.outcome,
			crate::dto::RemovalKind::Absent,
			"wire outcome must be absent"
		);
		assert!(!view.executed, "wire view executed must be false");
		assert!(view.paths.is_empty(), "wire view paths must be empty");
		assert!(view.success, "wire view success must be true");

		// Lock entry must NOT be pruned.
		assert!(
			skill::read_skill_lock().skills.contains_key(skill_name),
			"lock entry must remain intact"
		);
	}

	#[test]
	fn test_remove_skill_planned_absent() {
		let tmp = tempfile::tempdir().unwrap();
		let _env = TestEnv::new(tmp.path());

		let skill_name = "absent-skill";
		assert!(
			!skill::read_skill_lock().skills.contains_key(skill_name),
			"lock entry must not exist initially"
		);

		let mut mgr = crate::manager::ConfigManager::new(
			crate::create_adapter(crate::models::AgentType::Claude),
			true,
			None,
		);
		mgr.load().unwrap();

		let preview_err = mgr
			.remove_skill_planned(skill_name, false, true, false)
			.unwrap_err();
		assert!(
			matches!(
				preview_err,
				crate::errors::ConfigError::ResourceNotFound { .. }
			),
			"absent skill must return ResourceNotFound: {preview_err:?}"
		);

		let noop = mgr.skill_noop_outcome(skill_name);
		assert_eq!(noop.verdict, Verdict::Absent);
		assert!(!noop.executed);
		assert!(noop.absent);
		assert!(noop.plan.paths.is_empty());

		let view = crate::dto::RemovalView::from_outcome(&noop, false);
		assert_eq!(view.outcome, crate::dto::RemovalKind::Absent);
		assert!(!view.executed);
		assert!(view.paths.is_empty());
		assert!(view.success);

		assert!(
			!skill::read_skill_lock().skills.contains_key(skill_name),
			"lock entry must remain absent"
		);
	}

	/// A single-agent removal where the planner takes nothing and spares the
	/// Master (`spared_everything`) previews and commits with identical `Verdict::Kept`,
	/// `executed: false`, without pruning the lock.
	///
	/// Fixture rationale:
	/// On a real filesystem, single-agent removal through `remove_skill_planned`
	/// requires the skill to exist in `read_dirs` (otherwise `skill_for_planned_removal`
	/// bails with `ResourceNotFound`). When `plan.paths` is empty, `read_effect_after`
	/// inevitably rediscovers the skill, so `effect.survivors` is non-empty.
	/// If `initial_shared_master_kept` were true, `all_survivors_reported` would be false
	/// (it requires `!initial_shared_master_kept`), making `spared_everything` false, which
	/// triggers `blocks = true` and produces `Verdict::Refused` (refusing to let a single
	/// agent delete a shared slot). Therefore, any real-fs single-agent `Verdict::Kept`
	/// necessarily resolves via `spared_everything`.
	///
	/// The second disjunct in `is_kept` (`(initial_shared_master_kept && plan_paths.is_empty())`)
	/// covers the planner-keep corner where discovery cannot see survivors; that disjunct
	/// is directly pinned by the synthetic table test `test_verdict_table` (Row 5).
	#[cfg(unix)]
	#[test]
	fn test_single_agent_spared_master_previews_and_does_not_prune() {
		let tmp = tempfile::tempdir().unwrap();
		let _env = TestEnv::new(tmp.path());

		let home = tmp.path().join("home");
		let aghub_master = home.join(".aghub/shared-kept");
		write_test_skill(&aghub_master, "shared-kept");
		write_test_lock_entry("shared-kept");

		let agents_skills = home.join(".agents/skills");
		std::fs::create_dir_all(&agents_skills).unwrap();
		let referrer = agents_skills.join("shared-kept");
		std::os::unix::fs::symlink(&aghub_master, &referrer).unwrap();

		let mut mgr = crate::manager::ConfigManager::new(
			crate::create_adapter(crate::models::AgentType::Cursor),
			true,
			None,
		);
		mgr.load().unwrap();

		// Preview
		let preview = mgr
			.remove_skill_planned("shared-kept", false, true, false)
			.unwrap();
		assert!(
			matches!(preview.verdict, Verdict::Kept { .. }),
			"preview must yield Verdict::Kept: {:?}",
			preview.verdict
		);
		assert!(!preview.executed, "preview executed must be false");
		assert!(preview.plan.paths.is_empty(), "plan paths must be empty");

		// Commit (with confirm: true) must return the same Kept outcome,
		// executed: false, and must NOT prune the lock.
		let commit = mgr
			.remove_skill_planned("shared-kept", false, false, true)
			.unwrap();
		assert_eq!(
			preview.verdict, commit.verdict,
			"preview and commit must yield identical Verdict::Kept"
		);
		assert!(!commit.executed, "commit executed must be false");
		assert!(aghub_master.exists(), "kept Master must remain on disk");
		assert!(referrer.exists(), "kept Referrer must remain on disk");
		assert!(
			skill::read_skill_lock().skills.contains_key("shared-kept"),
			"lock entry must NOT be pruned"
		);
	}

	#[test]
	fn test_verdict_still_read_from_paths() {
		let p1 = PathBuf::from("/project/.cline/skills/demo");
		let p2 = PathBuf::from("/project/.clinerules/skills/demo");

		let kept = Verdict::Kept {
			still_read_from: vec![Holder {
				path: p1.clone(),
				managed: true,
			}],
		};
		assert_eq!(kept.still_read_from_paths(), vec![p1.clone()]);

		let refused_single = Verdict::Refused {
			reason: format!(
				"skill it reads from a location shared with other agents; it is still served to this agent from: {}",
				p2.display()
			),
		};
		assert_eq!(refused_single.still_read_from_paths(), vec![p2.clone()]);

		let refused_with_peers = Verdict::Refused {
			reason: format!(
				"skill it reads from a location shared with other agents; it is still served to this agent from: {}. Also read there by agents not in this request: claude",
				p2.display()
			),
		};
		assert_eq!(
			refused_with_peers.still_read_from_paths(),
			vec![p2.clone()]
		);

		let refused_all = Verdict::Refused {
			reason: format!(
				"skill still discoverable afterwards in: {}. Read only by disabled agent(s)...",
				p1.display()
			),
		};
		assert_eq!(refused_all.still_read_from_paths(), vec![p1]);

		assert!(Verdict::Removed.still_read_from_paths().is_empty());
		assert!(Verdict::Absent.still_read_from_paths().is_empty());
	}
}
