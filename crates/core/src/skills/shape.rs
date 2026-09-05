//! Referrer shape classification — the single decision procedure behind
//! `repair`, `doctor --verify-links`, the pre-mutation guard, and migration.
//!
//! A skill's bytes live in exactly ONE place, the Master
//! (`<store>/.aghub/<sanitized-name>`, [`linker::master_store_dir`]). Every
//! agent that may read the skill holds a **Referrer**: a symlink at that agent's
//! skills dir resolving to the Master. This module answers one question about
//! one `(referrer, master)` pair — what shape is it in — and nothing else. It
//! never writes, and it never decides what to DO about a shape.
//!
//! Three traps are load-bearing here; each cost a review round to find, and each
//! has a test below that goes red if the guard is removed.
//!
//! 1. **`symlink_metadata` alone cannot decide conformance.** It is `lstat`: it
//!    reports a file type and nothing about the target. A healthy Referrer, a
//!    two-hop chain and a dangling link are indistinguishable by it.
//! 2. **Two `Err`s must never compare equal.** Folding both canonicalize results
//!    into `Option` and comparing makes `None == None`, so a dangling Referrer
//!    beside a missing Master certifies as healthy — reachable by deleting the
//!    store after migrating, and it makes `apply-update` write into a store it
//!    believes is fine.
//! 3. **Identity comes before content.** When a PARENT of the Referrer is a
//!    symlink into the store, `lstat` on the leaf says "real directory" — the
//!    violation shape — while the two paths are the same inode. Any repair that
//!    trusts that verdict and removes the "duplicate" deletes the only copy.

use std::path::{Path, PathBuf};

use aghub_agents::ResourceScope;

use std::str::FromStr;

use crate::skills::linker::{master_store_dir, Linker};

/// One agent's candidate Referrer for a skill at a scope.
pub struct CandidateReferrer {
	pub agent_id: &'static str,
	/// `<that agent's skills dir>/<sanitized-name>`. Present whether or not
	/// anything is there — the whole point is that a MISSING or BROKEN Referrer
	/// is reported rather than filtered out of existence.
	pub path: PathBuf,
}

/// Every agent's candidate Referrer path for `name` at a scope.
///
/// **One derivation, and it is path-derived rather than shape-derived.** An
/// earlier design took the union of "links that already resolve to the Master"
/// and "agents that natively read `.agents/skills`"; both halves were wrong. The
/// first admits only paths that are ALREADY conformant, so a dangling link, a
/// foreign target and npx's real directory were filtered out before anything
/// could report them. The second returns a variant carrying no path at all, so
/// cursor / codex / opencode lost their private dirs — the exact agents the
/// per-agent-Referrer decision exists to serve.
///
/// Taking each agent's own WRITE dir answers both at once: a private one where
/// the agent has one, the shared `.agents/skills` slot where it does not.
pub fn candidate_referrers(
	scope: ResourceScope,
	project_root: Option<&Path>,
	name: &str,
) -> Vec<CandidateReferrer> {
	let safe = skill::sanitize_name(name);
	crate::registry::ALL_AGENTS
		.iter()
		.filter_map(|descriptor| {
			skill_write_dir(descriptor, scope, project_root).map(|dir| {
				CandidateReferrer {
					agent_id: descriptor.id,
					path: dir.join(&safe),
				}
			})
		})
		.collect()
}

/// One agent's skills WRITE dir for a scope, or `None` when it cannot hold a
/// skill there at all.
///
/// Deliberately NOT routed through `agent_link_need`. That returns a 3-state
/// whose `NativeReader` arm carries no path, and it reaches that arm whenever
/// the agent's dir resolves to the store — which happens for real: with
/// `.agents/skills` symlinked into `.aghub` (stow, or a user hand-fixing their
/// layout) every shared-slot agent classifies as a native reader and drops out
/// of the candidate set entirely, so a repair silently does nothing about the
/// eight agents that most need it. Pinned by
/// `an_aliased_master_refuses_the_whole_plan`.
fn skill_write_dir(
	descriptor: &aghub_agents::AgentDescriptor,
	scope: ResourceScope,
	project_root: Option<&Path>,
) -> Option<PathBuf> {
	match crate::AgentType::from_str(descriptor.id) {
		Ok(agent_type) => crate::create_adapter(agent_type)
			.target_skills_dir(project_root, scope),
		// Defensive: every registry id resolves today. Falling back to the
		// descriptor bypasses the test path override, same as `classify_paths`.
		Err(_) => descriptor.skill_write_path(project_root, scope),
	}
}

/// The Master path for `name` at a scope.
pub fn master_path(project_root: Option<&Path>, name: &str) -> Option<PathBuf> {
	master_store_dir(project_root).map(|s| s.join(skill::sanitize_name(name)))
}

/// Why a `(referrer, master)` pair is not usable as-is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViolationKind {
	/// The Referrer is a link whose target is ANOTHER link — the chain npx's
	/// `createSymlink` leaves when it repoints an agent Referrer at the shared
	/// `.agents/skills/<name>` slot instead of at the Master.
	Chain { via: PathBuf },
	/// The Referrer is a link, but it does not resolve to this Master.
	ForeignTarget,
	/// The Referrer is a link that resolves to nothing.
	Dangling,
	/// A real directory sits where the Referrer belongs while the Master also
	/// exists — the shape every npx write verb leaves behind. Its bytes may
	/// exist NOWHERE else, so it is never safe to delete without comparing.
	ForkedCopy,
	/// The Master itself is a link. The store must hold real directories: a
	/// linked Master makes every Referrer a chain and puts the real bytes
	/// somewhere aghub does not manage.
	MasterIsLink,
}

/// The shape of one `(referrer, master)` pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillShape {
	/// Referrer is a link resolving to the Master in exactly one hop.
	Conformant,
	/// Nothing at the Referrer path. Legal — the agent was not granted this
	/// skill. Under D8 (no persisted authorization) this is indistinguishable
	/// from a grant the user removed by hand, and that is accepted.
	Absent,
	/// Un-migrated: a real directory serves the skill and no Master exists yet.
	/// **Never a violation** — read paths must tolerate it on a host that has
	/// not run `repair`/`migrate`.
	Legacy,
	/// Referrer and Master are the SAME object reached by different paths,
	/// because a parent of the Referrer is a symlink. Refuse: the "duplicate"
	/// is the original.
	AliasedMaster,
	Violation(ViolationKind),
}

impl SkillShape {
	/// Whether a mutating flow may proceed against this pair without repairing
	/// first. `Legacy` is deliberately included: it is the pre-migration state,
	/// and refusing it would refuse the migration that fixes it.
	pub fn is_actionable(&self) -> bool {
		matches!(self, Self::Conformant | Self::Absent | Self::Legacy)
	}
}

/// Resolve `path` to a real location, or `None` when it does not exist.
///
/// Distinct from `linker::canonicalize_lenient`, which invents a path for a
/// missing leaf so two absent paths can compare equal. Here a missing path must
/// stay unknowable — see trap 2 in the module docs.
fn resolved(path: &Path) -> Option<PathBuf> {
	std::fs::canonicalize(path).ok()
}

/// Whether two existing paths are the same filesystem object.
///
/// `None` unless BOTH resolve; an unresolvable path is never "the same" as
/// anything, including another unresolvable path.
fn same_object(a: &Path, b: &Path) -> bool {
	match (resolved(a), resolved(b)) {
		(Some(a), Some(b)) => a == b,
		_ => false,
	}
}

/// The one-hop target of a link, resolved against the link's own directory.
///
/// The OS resolves any `..` inside the join, so a symlinked parent is handled by
/// the filesystem rather than by a lexical walk — which is what keeps this out
/// of the `parent()`/`file_name()` trap the repo bans hand-rolled normalizers
/// for. An absolute `read_link` result replaces the base, which `Path::join`
/// already does.
fn one_hop_target(link: &Path) -> Option<PathBuf> {
	let target = std::fs::read_link(link).ok()?;
	let base = link.parent()?;
	Some(base.join(target))
}

/// Classify one `(referrer, master)` pair.
///
/// `master` is the skill dir inside the store (`<store>/.aghub/<name>`), NOT the
/// store itself. `referrer` is the candidate path in one agent's skills dir —
/// derived from that agent's descriptor, never from what happens to be on disk,
/// so a broken Referrer is reported rather than filtered out of existence.
pub fn classify_shape(referrer: &Path, master: &Path) -> SkillShape {
	let master_exists = referrer_or_master_exists(master);

	// The Master must be a real directory. Checked first: a linked Master makes
	// every Referrer look like a chain, and reporting that per-agent would send
	// the user chasing five symptoms of one cause.
	if master_exists && Linker::is_link(master) {
		return SkillShape::Violation(ViolationKind::MasterIsLink);
	}

	let referrer_is_link = Linker::is_link(referrer);
	if !referrer_or_master_exists(referrer) && !referrer_is_link {
		return SkillShape::Absent;
	}

	// Trap 3: identity before anything that could lead to a deletion. A parent
	// symlink makes the leaf lstat as a real directory while being the Master.
	if !referrer_is_link && same_object(referrer, master) {
		return SkillShape::AliasedMaster;
	}

	if referrer_is_link {
		let Some(target) = resolved(referrer) else {
			return SkillShape::Violation(ViolationKind::Dangling);
		};
		// Trap 2: `master` must resolve on its own. Comparing two failures is
		// how a dangling Referrer beside a missing Master certified as healthy.
		let Some(master_real) = resolved(master) else {
			return SkillShape::Violation(ViolationKind::ForeignTarget);
		};
		if target != master_real {
			return SkillShape::Violation(ViolationKind::ForeignTarget);
		}
		// Trap 1: endpoints agreeing is not enough — npx leaves a chain whose
		// endpoint is still the Master. A Windows junction may not be readable
		// as a link; when the hop cannot be read, endpoint equality stands.
		if let Some(hop) = one_hop_target(referrer) {
			if Linker::is_link(&hop) {
				return SkillShape::Violation(ViolationKind::Chain {
					via: hop,
				});
			}
		}
		return SkillShape::Conformant;
	}

	// A real directory sits at the Referrer path.
	if master_exists {
		SkillShape::Violation(ViolationKind::ForkedCopy)
	} else {
		SkillShape::Legacy
	}
}

/// `Path::exists` follows links, so a dangling link reads as absent. Callers
/// here need "is there an entry at this path at all", link-ness included.
fn referrer_or_master_exists(path: &Path) -> bool {
	path.symlink_metadata().is_ok()
}

#[cfg(all(test, unix))]
mod tests {
	use super::*;
	use std::fs;
	use std::os::unix::fs as unix_fs;

	/// `<tmp>/store/.aghub/foo` as the Master, plus an empty agent dir.
	fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
		let tmp = tempfile::tempdir().unwrap();
		let root = fs::canonicalize(tmp.path()).unwrap();
		let master = root.join(".aghub").join("foo");
		fs::create_dir_all(&master).unwrap();
		fs::write(master.join("SKILL.md"), "---\nname: foo\n---\n").unwrap();
		let agent_dir = root.join(".claude").join("skills");
		fs::create_dir_all(&agent_dir).unwrap();
		(tmp, master, agent_dir)
	}

	#[test]
	fn healthy_symlink_is_conformant() {
		let (_tmp, master, agent_dir) = fixture();
		let referrer = agent_dir.join("foo");
		unix_fs::symlink(&master, &referrer).unwrap();
		assert_eq!(classify_shape(&referrer, &master), SkillShape::Conformant);
	}

	#[test]
	fn missing_referrer_is_absent() {
		let (_tmp, master, agent_dir) = fixture();
		assert_eq!(
			classify_shape(&agent_dir.join("foo"), &master),
			SkillShape::Absent
		);
	}

	/// Trap 1. npx's `createSymlink` repoints agent Referrers at the shared
	/// `.agents/skills/<n>` slot, so the endpoint is still the Master and only
	/// the hop reveals the chain. Deleting the `Chain` arm makes this Conformant.
	#[test]
	fn two_hop_chain_is_a_violation_even_though_the_endpoint_matches() {
		let (_tmp, master, agent_dir) = fixture();
		let shared = master.parent().unwrap().parent().unwrap().join(".agents");
		fs::create_dir_all(shared.join("skills")).unwrap();
		let slot = shared.join("skills").join("foo");
		unix_fs::symlink(&master, &slot).unwrap();

		let referrer = agent_dir.join("foo");
		unix_fs::symlink(&slot, &referrer).unwrap();

		assert_eq!(
			resolved(&referrer).unwrap(),
			resolved(&master).unwrap(),
			"precondition: the endpoints DO agree, so endpoint equality alone \
			 would pass this"
		);
		assert!(
			matches!(
				classify_shape(&referrer, &master),
				SkillShape::Violation(ViolationKind::Chain { .. })
			),
			"a chain must not certify as conformant"
		);
	}

	/// Trap 2. Both sides unresolvable. Folding into `Option` and comparing
	/// makes `None == None` and calls this healthy.
	#[test]
	fn dangling_referrer_with_missing_master_is_never_conformant() {
		let tmp = tempfile::tempdir().unwrap();
		let root = fs::canonicalize(tmp.path()).unwrap();
		let agent_dir = root.join(".claude").join("skills");
		fs::create_dir_all(&agent_dir).unwrap();
		let master = root.join(".aghub").join("foo"); // never created
		let referrer = agent_dir.join("foo");
		unix_fs::symlink(&master, &referrer).unwrap();

		let shape = classify_shape(&referrer, &master);
		assert_ne!(
			shape,
			SkillShape::Conformant,
			"a dangling link beside a missing master must not be healthy"
		);
		assert!(matches!(shape, SkillShape::Violation(_)));
	}

	/// Trap 3. `.agents/skills` is itself a symlink into the store (stow, or a
	/// user hand-fixing their layout), so the leaf lstats as a real directory
	/// while BEING the Master. Verified against a compiled probe during review:
	/// treating this as `ForkedCopy` and removing the "duplicate" deletes the
	/// Master.
	#[test]
	fn aliased_master_through_a_symlinked_parent_is_not_a_forked_copy() {
		let (_tmp, master, _agent_dir) = fixture();
		let store = master.parent().unwrap().to_path_buf();
		let agents = store.parent().unwrap().join(".agents");
		fs::create_dir_all(&agents).unwrap();
		// .agents/skills -> ../.aghub
		unix_fs::symlink(&store, agents.join("skills")).unwrap();

		let referrer = agents.join("skills").join("foo");
		assert!(
			!Linker::is_link(&referrer),
			"precondition: lstat on the leaf reports a real directory"
		);
		assert!(
			referrer.is_dir(),
			"precondition: it looks exactly like the ForkedCopy shape"
		);

		assert_eq!(
			classify_shape(&referrer, &master),
			SkillShape::AliasedMaster,
			"the 'duplicate' is the Master reached through a symlinked parent"
		);
	}

	/// The npx-clobbered shape: a real directory holding bytes that may exist
	/// nowhere else, beside a live Master.
	#[test]
	fn real_directory_beside_a_live_master_is_a_forked_copy() {
		let (_tmp, master, agent_dir) = fixture();
		let referrer = agent_dir.join("foo");
		fs::create_dir_all(&referrer).unwrap();
		fs::write(referrer.join("SKILL.md"), "npx wrote this").unwrap();
		assert_eq!(
			classify_shape(&referrer, &master),
			SkillShape::Violation(ViolationKind::ForkedCopy)
		);
	}

	/// The un-migrated host. Must NOT be a violation, or every read path breaks
	/// on day one and `doctor` tells the user to prune live lock entries.
	#[test]
	fn real_directory_with_no_master_is_legacy_not_a_violation() {
		let tmp = tempfile::tempdir().unwrap();
		let root = fs::canonicalize(tmp.path()).unwrap();
		let legacy = root.join(".agents").join("skills").join("foo");
		fs::create_dir_all(&legacy).unwrap();
		fs::write(legacy.join("SKILL.md"), "---\nname: foo\n---\n").unwrap();
		let master = root.join(".aghub").join("foo"); // not migrated yet

		let shape = classify_shape(&legacy, &master);
		assert_eq!(shape, SkillShape::Legacy);
		assert!(
			shape.is_actionable(),
			"a mutating flow must be able to proceed and migrate it, not \
			 refuse the state it exists to fix"
		);
	}

	#[test]
	fn link_to_someone_elses_skill_is_a_foreign_target() {
		let (_tmp, master, agent_dir) = fixture();
		let other = master.parent().unwrap().join("bar");
		fs::create_dir_all(&other).unwrap();
		let referrer = agent_dir.join("foo");
		unix_fs::symlink(&other, &referrer).unwrap();
		assert_eq!(
			classify_shape(&referrer, &master),
			SkillShape::Violation(ViolationKind::ForeignTarget)
		);
	}

	#[test]
	fn a_linked_master_is_reported_once_not_as_five_broken_referrers() {
		let (_tmp, master, agent_dir) = fixture();
		let real = master.parent().unwrap().join("elsewhere");
		fs::create_dir_all(&real).unwrap();
		fs::remove_dir_all(&master).unwrap();
		unix_fs::symlink(&real, &master).unwrap();

		let referrer = agent_dir.join("foo");
		unix_fs::symlink(&master, &referrer).unwrap();
		assert_eq!(
			classify_shape(&referrer, &master),
			SkillShape::Violation(ViolationKind::MasterIsLink)
		);
	}

	/// The load-bearing claim of the whole change: asking against the `.aghub`
	/// store, the three agents that today read `~/.agents/skills` *and* own a
	/// private dir resolve to the PRIVATE one — which is what makes them
	/// individually revocable. Point this at `~/.agents/skills` instead and all
	/// three collapse to `NativeReader`, carry no path, and the feature is gone.
	#[test]
	fn global_candidates_prefer_private_dirs_over_the_shared_slot() {
		// Reads HOME through `master_store_dir` / the descriptors.
		let _env = crate::skills::prune::test_lock::env_lock();
		let home = dirs::home_dir().expect("home");

		let by_id: std::collections::HashMap<_, _> =
			candidate_referrers(ResourceScope::GlobalOnly, None, "foo")
				.into_iter()
				.map(|c| (c.agent_id, c.path))
				.collect();

		for (id, private_suffix) in
			[("codex", ".codex/skills"), ("cursor", ".cursor/skills")]
		{
			let path = by_id
				.get(id)
				.unwrap_or_else(|| panic!("{id} must have a candidate"));
			assert!(
				path.starts_with(home.join(private_suffix)),
				"{id} must resolve to its PRIVATE dir, got {}",
				path.display()
			);
		}

		// cline and warp have no private skills dir anywhere — their only
		// skills path IS the shared slot, so they land there. This is the
		// documented floor, not a defect.
		let shared = home.join(".agents").join("skills");
		for id in ["cline", "warp"] {
			let path = by_id
				.get(id)
				.unwrap_or_else(|| panic!("{id} must have a candidate"));
			assert!(
				path.starts_with(&shared),
				"{id} has no private dir and must share the slot, got {}",
				path.display()
			);
		}

		assert_eq!(
			by_id.get("cline").and_then(|p| p.parent()),
			by_id.get("warp").and_then(|p| p.parent()),
			"the shared slot must be ONE directory — granting to either grants \
			 to both, and callers depend on comparing these paths"
		);
	}

	/// Project scope with a tempdir root — no real HOME involved.
	fn project_fixture() -> (tempfile::TempDir, PathBuf) {
		let tmp = tempfile::tempdir().unwrap();
		let root = fs::canonicalize(tmp.path()).unwrap();
		(tmp, root)
	}

	#[test]
	fn an_unmigrated_skill_plans_a_migration_and_names_its_source() {
		let (_tmp, root) = project_fixture();
		let legacy = root.join(".agents").join("skills").join("foo");
		fs::create_dir_all(&legacy).unwrap();
		fs::write(legacy.join("SKILL.md"), "---\nname: foo\n---\n").unwrap();

		let plan = plan_repair(ResourceScope::ProjectOnly, Some(&root), "foo")
			.unwrap();
		assert!(plan.needs_migration, "a real dir with no Master is legacy");
		assert_eq!(plan.migrate_from.as_deref(), Some(legacy.as_path()));
		assert_eq!(plan.master, root.join(".aghub").join("foo"));
		assert!(!plan.is_noop());
		assert!(
			plan.refusals().is_empty(),
			"legacy must never be a refusal — it is the state repair exists \
			 to fix"
		);
	}

	#[test]
	fn a_fully_linked_skill_plans_nothing() {
		let (_tmp, root) = project_fixture();
		let master = root.join(".aghub").join("foo");
		fs::create_dir_all(&master).unwrap();
		fs::write(master.join("SKILL.md"), "---\nname: foo\n---\n").unwrap();

		let plan = plan_repair(ResourceScope::ProjectOnly, Some(&root), "foo")
			.unwrap();
		assert!(
			plan.is_noop(),
			"every Referrer is Absent (granted to nobody) and the Master \
			 exists — that is the withheld state, not a problem: {:?}",
			plan.actions
		);
	}

	/// A refusal must surface through `refusals()` so execution can abort the
	/// WHOLE plan. A partially applied repair is how a skill ends up readable
	/// from nowhere.
	#[test]
	fn an_aliased_master_refuses_the_whole_plan() {
		let (_tmp, root) = project_fixture();
		let store = root.join(".aghub");
		let master = store.join("foo");
		fs::create_dir_all(&master).unwrap();
		fs::write(master.join("SKILL.md"), "---\nname: foo\n---\n").unwrap();
		fs::create_dir_all(root.join(".agents")).unwrap();
		unix_fs::symlink(&store, root.join(".agents").join("skills")).unwrap();

		let plan = plan_repair(ResourceScope::ProjectOnly, Some(&root), "foo")
			.unwrap();
		let refusals = plan.refusals();
		assert!(
			!refusals.is_empty(),
			"the shared slot aliases the store; repair must not proceed"
		);
		assert!(refusals
			.iter()
			.all(|(_, r)| **r == RefuseReason::AliasedMaster));
	}

	#[test]
	fn npx_forked_copy_plans_a_comparison_never_a_delete() {
		let (_tmp, root) = project_fixture();
		let master = root.join(".aghub").join("foo");
		fs::create_dir_all(&master).unwrap();
		fs::write(master.join("SKILL.md"), "master").unwrap();
		let slot = root.join(".agents").join("skills").join("foo");
		fs::create_dir_all(&slot).unwrap();
		fs::write(slot.join("SKILL.md"), "npx wrote this").unwrap();

		let plan = plan_repair(ResourceScope::ProjectOnly, Some(&root), "foo")
			.unwrap();
		let slot_actions: Vec<_> = plan
			.actions
			.iter()
			.filter(|(_, p, _)| p == &slot)
			.map(|(_, _, a)| a)
			.collect();
		assert!(!slot_actions.is_empty(), "the shared slot must be planned");
		assert!(
			slot_actions
				.iter()
				.all(|a| **a == ReferrerAction::CompareThenQuarantine),
			"bytes that may exist nowhere else are never planned for deletion"
		);
	}

	#[test]
	fn only_conformant_absent_and_legacy_are_actionable() {
		assert!(SkillShape::Conformant.is_actionable());
		assert!(SkillShape::Absent.is_actionable());
		assert!(SkillShape::Legacy.is_actionable());
		assert!(!SkillShape::AliasedMaster.is_actionable());
		assert!(
			!SkillShape::Violation(ViolationKind::ForkedCopy).is_actionable()
		);
	}
}

/// What `repair` would do about one Referrer.
///
/// Planning is separated from execution so preview and commit consume the SAME
/// plan: they cannot disagree, because there is only one computation. This
/// mirrors `RemovalPlan` / `RemovalOutcome`, for the same reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReferrerAction {
	/// Already correct, or deliberately not granted.
	Leave,
	/// Point this Referrer at the Master. Covers a chain, a foreign target and
	/// a dangling link alike — all three are "the link is wrong", and the fix is
	/// the same write.
	Relink,
	/// A real directory holding possibly-unique bytes. Compare against the
	/// Master before touching it; never delete.
	CompareThenQuarantine,
	/// Nothing may be written for this pair until a human intervenes.
	Refuse { reason: RefuseReason },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefuseReason {
	/// The Referrer IS the Master, reached through a symlinked parent. Removing
	/// the "duplicate" would delete the only copy.
	AliasedMaster,
	/// The store holds a link where it must hold a real directory.
	MasterIsLink,
}

/// The plan for one skill at one scope: what to do about each candidate
/// Referrer, plus whether the Master itself must be created first.
#[derive(Debug, Clone)]
pub struct RepairPlan {
	pub name: String,
	pub master: PathBuf,
	/// True when the skill is un-migrated: a real directory is serving it and
	/// no Master exists. Execution must materialize the Master BEFORE any
	/// Referrer is written — a Referrer must never precede its Master.
	pub needs_migration: bool,
	/// The legacy directory to adopt as the Master, when `needs_migration`.
	pub migrate_from: Option<PathBuf>,
	pub actions: Vec<(&'static str, PathBuf, ReferrerAction)>,
}

impl RepairPlan {
	/// Nothing to do — every Referrer is already `Leave` and no migration is
	/// pending.
	pub fn is_noop(&self) -> bool {
		!self.needs_migration
			&& self
				.actions
				.iter()
				.all(|(_, _, a)| matches!(a, ReferrerAction::Leave))
	}

	/// Refusals block the WHOLE plan: a partially applied repair is how a skill
	/// ends up readable from nowhere.
	pub fn refusals(&self) -> Vec<(&'static str, &RefuseReason)> {
		self.actions
			.iter()
			.filter_map(|(id, _, a)| match a {
				ReferrerAction::Refuse { reason } => Some((*id, reason)),
				_ => None,
			})
			.collect()
	}
}

/// Compute the repair plan for one skill. Pure: reads the filesystem, writes
/// nothing.
pub fn plan_repair(
	scope: ResourceScope,
	project_root: Option<&Path>,
	name: &str,
) -> Option<RepairPlan> {
	let master = master_path(project_root, name)?;
	let candidates = candidate_referrers(scope, project_root, name);

	let mut actions = Vec::with_capacity(candidates.len());
	let mut migrate_from = None;
	for candidate in &candidates {
		let shape = classify_shape(&candidate.path, &master);
		if matches!(shape, SkillShape::Legacy) && migrate_from.is_none() {
			migrate_from = Some(candidate.path.clone());
		}
		actions.push((
			candidate.agent_id,
			candidate.path.clone(),
			match shape {
				SkillShape::Conformant | SkillShape::Absent => {
					ReferrerAction::Leave
				}
				// The legacy directory becomes the Master; every OTHER agent that
				// was reading it needs a link once it moves. The one being adopted
				// is handled by the migration step itself.
				SkillShape::Legacy => ReferrerAction::Relink,
				SkillShape::AliasedMaster => ReferrerAction::Refuse {
					reason: RefuseReason::AliasedMaster,
				},
				SkillShape::Violation(ViolationKind::MasterIsLink) => {
					ReferrerAction::Refuse {
						reason: RefuseReason::MasterIsLink,
					}
				}
				SkillShape::Violation(ViolationKind::ForkedCopy) => {
					ReferrerAction::CompareThenQuarantine
				}
				SkillShape::Violation(
					ViolationKind::Chain { .. }
					| ViolationKind::ForeignTarget
					| ViolationKind::Dangling,
				) => ReferrerAction::Relink,
			},
		));
	}

	Some(RepairPlan {
		name: name.to_string(),
		needs_migration: migrate_from.is_some(),
		migrate_from,
		master,
		actions,
	})
}
