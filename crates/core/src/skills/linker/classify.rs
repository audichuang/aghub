//! Agent classification for symlink-only skill install.
//!
//! Derives — purely from each agent's RESOLVED write skills-dir, never from
//! `capabilities.skills.universal` and never from a hardcoded agent list —
//! where an agent's Referrer for a skill goes, or that it cannot hold skills at
//! this scope at all.
//!
//! **There is no longer a "reads the master directly" case.** The Master lives
//! in the `.aghub` store, which no agent reads, so every supported agent needs a
//! link. The old `NativeReader` variant became unreachable the moment the store
//! moved; it was deleted rather than left in place, because a variant that is
//! still constructed but never produced draws no dead-code warning and silently
//! kills every `matches!` arm that tests for it.
//!
//! What replaced it is **slot sharing**: several agents resolve to the SAME
//! Referrer directory (up to eight at project scope, all of them
//! `.agents/skills`). Granting to one grants to all of them, so that fact is
//! computed once here and carried on the plan rather than rediscovered by each
//! consumer.

use crate::AgentType;
use aghub_agents::{AgentDescriptor, ResourceScope};
use std::path::{Path, PathBuf};
use std::str::FromStr;

/// Where an agent's Referrer for a skill goes at a scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkNeed {
	/// The directory this agent reads skills from. **Not necessarily private:**
	/// ten agent/scope combinations resolve to the shared `.agents/skills`, and
	/// eight of those have no alternative. Consumers that key on this path alone
	/// conflate every sharer into one identity — read `shared_with` on the plan.
	NeedsLink { referrer_dir: PathBuf },
	/// Agent's skills-dir cannot be resolved for this scope.
	Unsupported,
}

/// One agent's classification result for a given scope.
///
/// `reads_master` / `writes_master` used to live here. Against a store no agent
/// reads they are constant `false`, so keeping them would have shipped three
/// hard-coded booleans to the UI dressed as facts. `shared_with` replaces them
/// with the fact that now matters: who else this grant would reach.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentLinkPlan {
	pub agent_id: &'static str,
	pub need: LinkNeed,
	pub installed: bool,
	/// Other agents resolving to the SAME Referrer directory at this scope.
	/// Empty for a private dir. Non-empty means granting here grants to all of
	/// them, and revoking here revokes from all of them.
	pub shared_with: Vec<&'static str>,
}

/// Serializable wire view of an [`AgentLinkPlan`] for the skills-coverage
/// surface.
///
/// `AgentLinkPlan`/`LinkNeed` are domain types (one carries a filesystem path),
/// so this view is the SINGLE place the coverage wire shape is defined. The API
/// derives a `ts-rs` DTO that mirrors it, and the CLI serializes it directly, so
/// neither hand-rolls a second mapping. `needs_link`/`supported` project the
/// `LinkNeed` 2-state; the agent is keyed as `id` and `scope` is the lowercase
/// scope label. `shared_with` is what stops the UI presenting a shared slot as
/// if it were a per-agent choice.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AgentSkillCoverageView {
	pub id: String,
	pub scope: String,
	pub needs_link: bool,
	pub supported: bool,
	pub shared_with: Vec<String>,
}

impl AgentSkillCoverageView {
	/// Project a classified plan into the coverage wire view for `scope`.
	pub fn from_plan(plan: &AgentLinkPlan, scope: &str) -> Self {
		AgentSkillCoverageView {
			id: plan.agent_id.to_string(),
			scope: scope.to_string(),
			needs_link: matches!(plan.need, LinkNeed::NeedsLink { .. }),
			supported: !matches!(plan.need, LinkNeed::Unsupported),
			shared_with: plan
				.shared_with
				.iter()
				.map(|s| (*s).to_string())
				.collect(),
		}
	}
}

/// Normalize a path for comparison, tolerating a leaf that does not exist yet.
pub(crate) fn canonicalize_lenient(p: &Path) -> PathBuf {
	if let Ok(c) = std::fs::canonicalize(p) {
		return c;
	}
	// `p` (or its leaf) may not exist yet — e.g. `<root>/.agents/skills` before
	// any install. Plain canonicalize() then fails and leaves the raw path,
	// which won't match a canonicalized counterpart (macOS `/var`->`/private`,
	// Windows 8.3 short names / `\\?\` UNC). Canonicalize the longest EXISTING
	// ancestor and re-append the non-existent remainder so both sides normalize
	// identically.
	let mut ancestor = p;
	let mut tail: Vec<std::ffi::OsString> = Vec::new();
	loop {
		match std::fs::canonicalize(ancestor) {
			Ok(mut out) => {
				for part in tail.iter().rev() {
					out.push(part);
				}
				return out;
			}
			Err(_) => match (ancestor.parent(), ancestor.file_name()) {
				(Some(parent), Some(name)) => {
					tail.push(name.to_os_string());
					ancestor = parent;
				}
				_ => return p.to_path_buf(),
			},
		}
	}
}

/// One agent's Referrer directory for a scope, WITHOUT the availability probe.
///
/// The `master_skills_dir` parameter is gone. It existed to answer "does this
/// agent already read the master", and the answer is now structurally always no
/// — keeping the parameter would have let a caller pass the OLD master path and
/// silently resurrect the deleted behaviour.
pub fn agent_link_need(
	descriptor: &AgentDescriptor,
	scope: ResourceScope,
	project_root: Option<&Path>,
) -> LinkNeed {
	let write_dir = match AgentType::from_str(descriptor.id) {
		Ok(agent_type) => crate::create_adapter(agent_type)
			.target_skills_dir(project_root, scope),
		// Defensive fallback for unknown ids (all registry ids resolve via
		// from_str today); uses descriptor paths directly, intentionally
		// bypassing the SKILLS_PATH_OVERRIDE test hook.
		Err(_) => descriptor.skill_write_path(project_root, scope),
	};
	match write_dir {
		Some(referrer_dir) => LinkNeed::NeedsLink { referrer_dir },
		None => LinkNeed::Unsupported,
	}
}

/// Every OTHER agent whose Referrer directory is the same one at this scope.
///
/// Comparison is on the resolved directory, so a symlinked `.agents` still folds
/// its sharers together. Computed once here because every consumer needs it and
/// none of them should re-derive it: the install-result attribution, the doctor
/// rows, `transfer`'s protect set and the desktop checkbox group all key on the
/// Referrer path, and each of them conflated the sharers before this existed.
pub fn shared_with(
	agent_id: &str,
	dir: &Path,
	scope: ResourceScope,
	project_root: Option<&Path>,
) -> Vec<&'static str> {
	let canon = canonicalize_lenient(dir);
	crate::registry::ALL_AGENTS
		.iter()
		.filter(|d| d.id != agent_id)
		.filter(|d| {
			matches!(
				agent_link_need(d, scope, project_root),
				LinkNeed::NeedsLink { ref referrer_dir }
					if canonicalize_lenient(referrer_dir) == canon
			)
		})
		.map(|d| d.id)
		.collect()
}

pub fn classify_agent(
	descriptor: &AgentDescriptor,
	scope: ResourceScope,
	project_root: Option<&Path>,
) -> AgentLinkPlan {
	let need = agent_link_need(descriptor, scope, project_root);
	let shared = match &need {
		LinkNeed::NeedsLink { referrer_dir } => {
			shared_with(descriptor.id, referrer_dir, scope, project_root)
		}
		LinkNeed::Unsupported => Vec::new(),
	};

	AgentLinkPlan {
		agent_id: descriptor.id,
		need,
		installed: crate::availability::check_agent_availability(descriptor)
			.is_available,
		shared_with: shared,
	}
}

/// Classify ALL registered agents (`registry::ALL_AGENTS`).
pub fn classify_all(
	scope: ResourceScope,
	project_root: Option<&Path>,
) -> Vec<AgentLinkPlan> {
	crate::registry::ALL_AGENTS
		.iter()
		.map(|descriptor| classify_agent(descriptor, scope, project_root))
		.collect()
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::registry;

	fn plan_for(
		id: &str,
		scope: ResourceScope,
		project_root: Option<&Path>,
	) -> AgentLinkPlan {
		let descriptor = registry::ALL_AGENTS
			.iter()
			.find(|d| d.id == id)
			.unwrap_or_else(|| panic!("no descriptor for {id}"));
		classify_agent(descriptor, scope, project_root)
	}

	fn dir_of(plan: &AgentLinkPlan) -> &Path {
		match &plan.need {
			LinkNeed::NeedsLink { referrer_dir } => referrer_dir,
			other => panic!("expected NeedsLink, got {other:?}"),
		}
	}

	/// The five agents that used to be `NativeReader` at global scope now each
	/// get a Referrer directory — and for three of them it is a PRIVATE one.
	/// That is the feature: codex, cursor and opencode become individually
	/// revocable instead of sharing one all-or-nothing slot.
	///
	/// Restore the `reads_master` short-circuit and these three collapse back to
	/// a variant carrying no path at all.
	#[test]
	fn the_former_native_readers_now_get_their_own_referrer_dirs() {
		let _env = crate::skills::prune::test_lock::env_lock();
		let home = dirs::home_dir().expect("home");
		let shared = home.join(".agents").join("skills");

		for (id, private) in [
			("codex", home.join(".codex").join("skills")),
			("cursor", home.join(".cursor").join("skills")),
		] {
			let plan = plan_for(id, ResourceScope::GlobalOnly, None);
			assert_eq!(
				dir_of(&plan),
				private,
				"{id} must be linked into its OWN dir, not the shared slot"
			);
			assert!(
				!plan.shared_with.contains(&"cline"),
				"{id} no longer shares a slot with cline"
			);
		}

		// cline and warp have no private skills dir at any scope: the shared
		// slot IS their directory. This is the documented floor, not a defect —
		// granting to either grants to both.
		for id in ["cline", "warp"] {
			let plan = plan_for(id, ResourceScope::GlobalOnly, None);
			assert_eq!(dir_of(&plan), shared, "{id} has no private dir");
		}
		let cline = plan_for("cline", ResourceScope::GlobalOnly, None);
		assert!(
			cline.shared_with.contains(&"warp"),
			"cline must disclose warp as a co-grantee, got {:?}",
			cline.shared_with
		);
	}

	/// Slot sharing is computed from RESOLVED directories and must be symmetric:
	/// if A discloses B, B discloses A. An asymmetric answer means one of them
	/// silently loses a skill on removal.
	#[test]
	fn slot_sharing_is_symmetric_and_excludes_self() {
		let tmp = tempfile::tempdir().unwrap();
		let root = std::fs::canonicalize(tmp.path()).unwrap();
		let cline = plan_for("cline", ResourceScope::ProjectOnly, Some(&root));
		let warp = plan_for("warp", ResourceScope::ProjectOnly, Some(&root));

		assert!(cline.shared_with.contains(&"warp"));
		assert!(warp.shared_with.contains(&"cline"));
		assert!(
			!cline.shared_with.contains(&"cline"),
			"an agent never shares with itself"
		);
	}

	/// At PROJECT scope amp and kimi resolve to `<root>/.agents/skills`, so they
	/// join the shared slot with codex, cline, warp, antigravity, copilot and
	/// gemini — eight agents, one directory. At GLOBAL they share a DIFFERENT
	/// directory with each other (`~/.config/agents/skills`), which is a second
	/// shared slot with the same property.
	#[test]
	fn amp_and_kimi_share_the_project_slot_and_a_second_one_at_global() {
		let tmp = tempfile::tempdir().unwrap();
		let root = std::fs::canonicalize(tmp.path()).unwrap();
		let shared = root.join(".agents").join("skills");
		for id in ["amp", "kimi"] {
			let plan = plan_for(id, ResourceScope::ProjectOnly, Some(&root));
			assert_eq!(dir_of(&plan), shared, "{id} @project");
		}
		let amp = plan_for("amp", ResourceScope::ProjectOnly, Some(&root));
		assert!(
			amp.shared_with.len() >= 7,
			"the project slot is read by eight agents, got {:?}",
			amp.shared_with
		);

		let _env = crate::skills::prune::test_lock::env_lock();
		let amp_g = plan_for("amp", ResourceScope::GlobalOnly, None);
		assert!(
			amp_g.shared_with.contains(&"kimi"),
			"amp and kimi share ~/.config/agents/skills at global, got {:?}",
			amp_g.shared_with
		);
		assert!(
			!amp_g.shared_with.contains(&"cline"),
			"but NOT the .agents/skills slot"
		);
	}

	#[test]
	fn augmentcode_gets_its_own_private_dir() {
		let _env = crate::skills::prune::test_lock::env_lock();
		let global = plan_for("augmentcode", ResourceScope::GlobalOnly, None);
		assert!(dir_of(&global).ends_with(".augment/skills"));
		assert!(
			global.shared_with.is_empty(),
			"a private dir is shared with nobody, got {:?}",
			global.shared_with
		);
	}

	#[test]
	fn agent_without_skill_support_is_unsupported() {
		let plan = plan_for("jetbrains-ai", ResourceScope::GlobalOnly, None);
		assert_eq!(plan.need, LinkNeed::Unsupported);
		let tmp = tempfile::tempdir().unwrap();
		let root = std::fs::canonicalize(tmp.path()).unwrap();
		let plan_p =
			plan_for("jetbrains-ai", ResourceScope::ProjectOnly, Some(&root));
		assert_eq!(plan_p.need, LinkNeed::Unsupported);
	}

	fn assert_totality(plans: &[AgentLinkPlan]) {
		assert_eq!(
			plans.len(),
			registry::ALL_AGENTS.len(),
			"classify_all must cover every registered agent"
		);
		for plan in plans {
			let needs_link = matches!(plan.need, LinkNeed::NeedsLink { .. });
			let unsupported = matches!(plan.need, LinkNeed::Unsupported);
			assert!(
				needs_link ^ unsupported,
				"agent {} must be in exactly one bucket, got {:?}",
				plan.agent_id,
				plan.need
			);
		}
	}

	#[test]
	fn classify_all_is_total_at_global() {
		let _env = crate::skills::prune::test_lock::env_lock();
		assert_totality(&classify_all(ResourceScope::GlobalOnly, None));
	}

	#[test]
	fn classify_all_is_total_at_project() {
		let tmp = tempfile::tempdir().unwrap();
		let root = std::fs::canonicalize(tmp.path()).unwrap();
		assert_totality(&classify_all(
			ResourceScope::ProjectOnly,
			Some(root.as_path()),
		));
	}

	/// The coverage wire must not carry a constant dressed as a fact. The three
	/// booleans that used to ride here (`reads_master`, `writes_master`,
	/// `auto_covered`) are all permanently false against a store nothing reads;
	/// the desktop partitioned on `auto_covered` and would render an empty
	/// bucket forever.
	#[test]
	fn the_coverage_view_carries_sharing_not_dead_booleans() {
		let tmp = tempfile::tempdir().unwrap();
		let root = std::fs::canonicalize(tmp.path()).unwrap();
		let plan = plan_for("cline", ResourceScope::ProjectOnly, Some(&root));
		let view = AgentSkillCoverageView::from_plan(&plan, "project");

		assert!(view.needs_link && view.supported);
		assert!(
			view.shared_with.contains(&"warp".to_string()),
			"the UI cannot present a shared slot honestly without this: {:?}",
			view.shared_with
		);
	}
}
