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
	Kept {
		still_read_from: Vec<Holder>,
	},
	Refused {
		reason: String,
		kind: String,
		path: Option<PathBuf>,
	},
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
	pub git_refusal: &'a dyn Fn() -> Option<(String, PathBuf)>,
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
			let (kind, path) = match git_refusal {
				Some((_, ref p)) => ("git".to_string(), Some(p.clone())),
				None => ("shared".to_string(), still.first().cloned()),
			};
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
					Some((hint, _)) => hint,
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
				let mut r = if let Some((hint, _)) = git_refusal {
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
			return Verdict::Refused { reason, kind, path };
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
mod tests;
