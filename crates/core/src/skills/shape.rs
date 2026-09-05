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

use crate::skills::linker::{master_store_dir, shared_referrer_dir, Linker};

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

/// Which root the STORE resolves against for a scope.
///
/// A project root is only the store's root under `ProjectOnly`; under
/// `GlobalOnly` the store is `~/.aghub` even when a project root is in hand.
/// Same gate as `install_fetched.rs:417` — without it `repair -g` run inside a
/// project pairs a PROJECT Master with a set of GLOBAL Referrers, and every one
/// of them reads as broken.
fn store_root(
	scope: ResourceScope,
	project_root: Option<&Path>,
) -> Option<&Path> {
	if matches!(scope, ResourceScope::ProjectOnly) {
		project_root
	} else {
		None
	}
}

/// The Master path for `name` at a scope.
pub fn master_path(
	scope: ResourceScope,
	project_root: Option<&Path>,
	name: &str,
) -> Option<PathBuf> {
	master_store_dir(store_root(scope, project_root))
		.map(|s| s.join(skill::sanitize_name(name)))
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
	/// Something that is not a directory occupies the Master path. Without this
	/// check a regular file at the Master certifies every Referrer pointing at
	/// it as `Conformant`.
	MasterIsNotADir,
	/// Something that is neither a link nor a directory occupies the Referrer
	/// path. Not adoptable, not comparable — a human must look.
	ReferrerIsNotADir,
}

/// The **observed** shape of one `(referrer, master)` pair.
///
/// Deliberately observation only, with no policy in it. An earlier version had a
/// `Legacy` variant, but the spec defines legacy as "the LOCK names it, a real
/// directory serves it, and no Master exists" — and this function cannot see the
/// lock. It classified a user's hand-placed `.cursor/skills/<n>`, and even a
/// regular file, as legacy and offered it up for adoption as the Master, which
/// D5 forbids outright. Whether an [`Self::UnmigratedCopy`] may be adopted is
/// [`plan_repair`]'s call, because that is where the lock and the slot identity
/// are known.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillShape {
	/// Referrer is a link resolving to the Master in exactly one hop.
	Conformant,
	/// Nothing at the Referrer path. Legal — the agent was not granted this
	/// skill. Under D8 (no persisted authorization) this is indistinguishable
	/// from a grant the user removed by hand, and that is accepted.
	Absent,
	/// A real DIRECTORY serves the skill and no Master exists. Either the
	/// un-migrated layout or content aghub never installed; only the caller can
	/// tell those apart.
	UnmigratedCopy,
	/// Referrer and Master are the SAME object reached by different paths,
	/// because a parent of the Referrer is a symlink. Refuse: the "duplicate"
	/// is the original.
	AliasedMaster,
	Violation(ViolationKind),
}

impl SkillShape {
	/// Whether a mutating flow may proceed against this pair without repairing
	/// first. `UnmigratedCopy` is deliberately included: it is the pre-migration
	/// state, and refusing it would refuse the migration that fixes it. D7 then
	/// requires that flow to migrate the skill inside its own transaction —
	/// "may proceed" is not "may ignore".
	pub fn is_actionable(&self) -> bool {
		matches!(self, Self::Conformant | Self::Absent | Self::UnmigratedCopy)
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
	// A file at the Master path resolves fine, so every Referrer pointing at it
	// compares equal and certifies Conformant. `is_dir` follows links, but the
	// link case already returned above.
	if master_exists && !master.is_dir() {
		return SkillShape::Violation(ViolationKind::MasterIsNotADir);
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

	// Not a link, not absent, not the Master by another name. It must be a real
	// DIRECTORY to be either adoptable or comparable — a regular file is
	// neither, and calling it either is how a file got offered up as a Master.
	if !referrer.is_dir() {
		return SkillShape::Violation(ViolationKind::ReferrerIsNotADir);
	}
	if master_exists {
		SkillShape::Violation(ViolationKind::ForkedCopy)
	} else {
		SkillShape::UnmigratedCopy
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
	fn real_directory_with_no_master_is_an_unmigrated_copy_not_a_violation() {
		let tmp = tempfile::tempdir().unwrap();
		let root = fs::canonicalize(tmp.path()).unwrap();
		let legacy = root.join(".agents").join("skills").join("foo");
		fs::create_dir_all(&legacy).unwrap();
		fs::write(legacy.join("SKILL.md"), "---\nname: foo\n---\n").unwrap();
		let master = root.join(".aghub").join("foo"); // not migrated yet

		let shape = classify_shape(&legacy, &master);
		assert_eq!(shape, SkillShape::UnmigratedCopy);
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

	fn plan(root: &Path, in_lock: bool, grant: &[&str]) -> RepairPlan {
		plan_repair(
			ResourceScope::ProjectOnly,
			Some(root),
			"foo",
			in_lock,
			grant,
		)
		.expect("project scope always yields a plan")
	}

	fn action_at<'a>(p: &'a RepairPlan, path: &Path) -> &'a ReferrerAction {
		&p.actions
			.iter()
			.find(|a| a.path == path)
			.unwrap_or_else(|| panic!("no action for {}", path.display()))
			.action
	}

	fn write_skill(dir: &Path, body: &str) {
		fs::create_dir_all(dir).unwrap();
		fs::write(dir.join("SKILL.md"), body).unwrap();
	}

	fn shared_slot(root: &Path) -> PathBuf {
		root.join(".agents").join("skills").join("foo")
	}

	#[test]
	fn a_lock_named_shared_slot_is_adopted_as_the_master() {
		let (_tmp, root) = project_fixture();
		let slot = shared_slot(&root);
		write_skill(&slot, "---\nname: foo\n---\n");

		let p = plan(&root, true, &[]);
		assert_eq!(p.adopts(), Some(slot.as_path()));
		assert_eq!(action_at(&p, &slot), &ReferrerAction::AdoptAsMaster);
		assert!(!p.is_noop());
		assert!(p.refusals().is_empty(), "adoption is not a refusal");
		assert_eq!(
			p.actions.first().map(|a| &a.action),
			Some(&ReferrerAction::AdoptAsMaster),
			"the Master must be planned FIRST — a Referrer may never precede it"
		);
	}

	/// D5: aghub must not relocate content it did not install. A directory the
	/// lock does not name is reported, never adopted and never rewritten.
	#[test]
	fn an_unlocked_directory_is_never_adopted() {
		let (_tmp, root) = project_fixture();
		let slot = shared_slot(&root);
		write_skill(&slot, "---\nname: foo\n---\n");

		let p = plan(&root, false, &[]);
		assert_eq!(p.adopts(), None, "not in the lock, not aghub's to move");
		assert_eq!(action_at(&p, &slot), &ReferrerAction::LeaveForeign);
	}

	/// The blocker this rewrite exists for: a hand-placed private copy must not
	/// beat the shared slot to become the Master. Selection is by SLOT, never by
	/// registry order.
	#[test]
	fn a_private_copy_never_wins_adoption_over_the_shared_slot() {
		let (_tmp, root) = project_fixture();
		let private = root.join(".claude").join("skills").join("foo");
		write_skill(&private, "hand-placed");
		let slot = shared_slot(&root);
		write_skill(&slot, "---\nname: foo\n---\n");

		let p = plan(&root, true, &[]);
		assert_eq!(
			p.adopts(),
			Some(slot.as_path()),
			"the shared slot is the only adoptable source"
		);
		assert_eq!(
			action_at(&p, &private),
			&ReferrerAction::LeaveForeign,
			"a second real directory must never be planned for an action that \
			 destroys it"
		);
	}

	/// A regular file is neither adoptable nor comparable.
	#[test]
	fn a_regular_file_at_the_slot_is_refused_not_adopted() {
		let (_tmp, root) = project_fixture();
		let slot = shared_slot(&root);
		fs::create_dir_all(slot.parent().unwrap()).unwrap();
		fs::write(&slot, "not a skill").unwrap();

		let p = plan(&root, true, &[]);
		assert_eq!(p.adopts(), None);
		assert!(matches!(
			action_at(&p, &slot),
			ReferrerAction::Refuse {
				reason: RefuseReason::ReferrerIsNotADir
			}
		));
	}

	#[test]
	fn a_broken_link_is_planned_for_relink_once_a_master_exists() {
		let (_tmp, root) = project_fixture();
		write_skill(&root.join(".aghub").join("foo"), "master");
		let private = root.join(".claude").join("skills");
		fs::create_dir_all(&private).unwrap();
		unix_fs::symlink(root.join("nowhere"), private.join("foo")).unwrap();

		let p = plan(&root, true, &[]);
		assert_eq!(
			action_at(&p, &private.join("foo")),
			&ReferrerAction::Relink,
			"a dangling Referrer is exactly what repair exists to fix"
		);
		assert!(!p.is_noop());
	}

	/// Never create a Referrer before its Master exists.
	#[test]
	fn a_broken_link_with_no_master_refuses_instead_of_linking_to_nothing() {
		let (_tmp, root) = project_fixture();
		let private = root.join(".claude").join("skills");
		fs::create_dir_all(&private).unwrap();
		unix_fs::symlink(root.join("nowhere"), private.join("foo")).unwrap();

		let p = plan(&root, true, &[]);
		assert!(
			matches!(
				action_at(&p, &private.join("foo")),
				ReferrerAction::Refuse {
					reason: RefuseReason::MasterMissing
				}
			),
			"nothing to point at and nothing to adopt: {:?}",
			p.actions
		);
	}

	#[test]
	fn a_master_that_is_a_link_refuses_the_whole_plan() {
		let (_tmp, root) = project_fixture();
		let real = root.join("elsewhere");
		write_skill(&real, "real");
		fs::create_dir_all(root.join(".aghub")).unwrap();
		unix_fs::symlink(&real, root.join(".aghub").join("foo")).unwrap();

		let p = plan(&root, true, &[]);
		let refusals = p.refusals();
		assert!(!refusals.is_empty());
		assert!(refusals
			.iter()
			.all(|(_, r)| **r == RefuseReason::MasterIsLink));
	}

	#[test]
	fn a_file_at_the_master_path_refuses_rather_than_certifying_healthy() {
		let (_tmp, root) = project_fixture();
		fs::create_dir_all(root.join(".aghub")).unwrap();
		fs::write(root.join(".aghub").join("foo"), "not a dir").unwrap();
		let private = root.join(".claude").join("skills");
		fs::create_dir_all(&private).unwrap();
		unix_fs::symlink(root.join(".aghub").join("foo"), private.join("foo"))
			.unwrap();

		let p = plan(&root, true, &[]);
		assert!(!p.is_noop(), "a file Master must never read as healthy");
		assert!(p
			.refusals()
			.iter()
			.all(|(_, r)| **r == RefuseReason::MasterIsNotADir));
	}

	/// Migration step 2 — the reason the whole change is worth doing. An agent
	/// that reads the skill today gets an explicit, individually revocable link.
	#[test]
	fn an_agent_that_reads_it_today_is_granted_an_explicit_referrer() {
		let (_tmp, root) = project_fixture();
		write_skill(&root.join(".aghub").join("foo"), "master");
		let claude = root.join(".claude").join("skills").join("foo");

		let ungranted = plan(&root, true, &[]);
		assert_eq!(
			action_at(&ungranted, &claude),
			&ReferrerAction::Leave,
			"repair must not hand a skill to an agent nobody asked for"
		);

		let granted = plan(&root, true, &["claude"]);
		assert_eq!(
			action_at(&granted, &claude),
			&ReferrerAction::Create,
			"an implicit read must become an explicit grant"
		);
		assert!(!granted.is_noop());
	}

	#[test]
	fn a_healthy_link_beside_a_live_master_plans_nothing() {
		let (_tmp, root) = project_fixture();
		let master = root.join(".aghub").join("foo");
		write_skill(&master, "master");
		let private = root.join(".claude").join("skills");
		fs::create_dir_all(&private).unwrap();
		unix_fs::symlink(&master, private.join("foo")).unwrap();

		let p = plan(&root, true, &[]);
		assert_eq!(
			action_at(&p, &private.join("foo")),
			&ReferrerAction::Leave,
			"an already-correct Referrer must never be rewritten"
		);
		assert!(p.is_noop(), "{:?}", p.actions);
	}

	#[test]
	fn an_aliased_master_refuses_the_whole_plan() {
		let (_tmp, root) = project_fixture();
		let store = root.join(".aghub");
		write_skill(&store.join("foo"), "---\nname: foo\n---\n");
		fs::create_dir_all(root.join(".agents")).unwrap();
		unix_fs::symlink(&store, root.join(".agents").join("skills")).unwrap();

		let p = plan(&root, true, &[]);
		let refusals = p.refusals();
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
		write_skill(&root.join(".aghub").join("foo"), "master");
		let slot = shared_slot(&root);
		write_skill(&slot, "npx wrote this");

		let p = plan(&root, true, &[]);
		assert_eq!(
			action_at(&p, &slot),
			&ReferrerAction::CompareThenQuarantine,
			"bytes that may exist nowhere else are never planned for deletion"
		);
	}

	/// The shared slot is ONE directory that many agents resolve to. Reporting
	/// it once per agent would give a user eight identical rows and eight
	/// identical refusals for a single problem.
	#[test]
	fn the_shared_slot_is_one_action_carrying_every_agent_that_reads_it() {
		let (_tmp, root) = project_fixture();
		write_skill(&shared_slot(&root), "---\nname: foo\n---\n");

		let p = plan(&root, true, &[]);
		let rows: Vec<_> = p.actions.iter().filter(|a| a.shared).collect();
		assert_eq!(rows.len(), 1, "one directory, one row: {:?}", rows);
		assert!(
			rows[0].agents.len() > 1,
			"and it must name every agent that shares it, got {:?}",
			rows[0].agents
		);
	}

	/// `Both` names no single store. Answering with an empty plan reported
	/// `is_noop` for a host that badly needed migrating — and `Both` is the
	/// DEFAULT scope of doctor / check / source list.
	#[test]
	fn scope_both_refuses_to_plan_rather_than_reporting_nothing_to_do() {
		let (_tmp, root) = project_fixture();
		write_skill(&shared_slot(&root), "---\nname: foo\n---\n");
		assert!(plan_repair(
			ResourceScope::Both,
			Some(&root),
			"foo",
			true,
			&[]
		)
		.is_none());
	}

	/// A global plan must resolve its Master under HOME even when a project root
	/// is in hand, or it pairs a PROJECT Master with GLOBAL Referrers and every
	/// one of them reads as broken.
	#[test]
	fn a_global_plan_ignores_the_project_root_for_the_store() {
		let _env = crate::skills::prune::test_lock::env_lock();
		let (_tmp, root) = project_fixture();
		let home = dirs::home_dir().expect("home");
		let master =
			master_path(ResourceScope::GlobalOnly, Some(&root), "foo").unwrap();
		assert!(
			master.starts_with(&home),
			"global store must live under HOME, got {}",
			master.display()
		);
		assert!(!master.starts_with(&root));
	}

	#[test]
	fn only_conformant_absent_and_unmigrated_are_actionable() {
		assert!(SkillShape::Conformant.is_actionable());
		assert!(SkillShape::Absent.is_actionable());
		assert!(SkillShape::UnmigratedCopy.is_actionable());
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
	/// Already correct, or deliberately not granted and not owed one.
	Leave,
	/// This real directory becomes the Master. Exactly ONE action per plan may
	/// be this, and it must run before every other action — a Referrer must
	/// never precede its Master.
	AdoptAsMaster,
	/// Grant an agent that reads the skill today but holds no link of its own.
	/// This is migration step 2, and it is what turns an implicit read into an
	/// individually revocable grant. Without it, moving the Master buys nothing:
	/// codex / cursor / opencode stay fused to the shared slot.
	Create,
	/// Point an existing but wrong link at the Master. Covers a chain, a foreign
	/// target and a dangling link — all three are "the link is wrong", one write
	/// fixes each. Never applied to a real directory: that would mean deleting
	/// bytes, which is `CompareThenQuarantine`'s job.
	Relink,
	/// A real directory holding possibly-unique bytes. Compare against the
	/// Master before touching it; never delete.
	CompareThenQuarantine,
	/// Content aghub did not install and must not move (D5). Report only.
	LeaveForeign,
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
	/// Something that is not a directory occupies the Master path.
	MasterIsNotADir,
	/// Something that is neither a link nor a directory occupies the Referrer.
	ReferrerIsNotADir,
	/// There is no Master and nothing to adopt as one, yet Referrers are owed.
	/// Writing them would create links pointing at nothing.
	MasterMissing,
}

/// One planned change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedReferrer {
	/// Every agent that resolves to this path. The shared `.agents/skills` slot
	/// is ONE directory read by up to eight agents, so it appears once with all
	/// of their ids — not eight times. Callers disclose "granting to one grants
	/// to all of these" straight from this field.
	pub agents: Vec<&'static str>,
	pub path: PathBuf,
	pub shape: SkillShape,
	pub action: ReferrerAction,
	/// True for the shared `.agents/skills` slot.
	pub shared: bool,
}

/// The plan for one skill at one scope.
///
/// `actions` is ORDERED as execution must apply it: adopt the Master, then the
/// private Referrers, then the shared slot last. The shared slot goes last
/// because swapping it is the only destructive step, and every crash before it
/// leaves the old directory still serving the skill.
#[derive(Debug, Clone)]
pub struct RepairPlan {
	pub name: String,
	pub master: PathBuf,
	pub master_exists: bool,
	pub actions: Vec<PlannedReferrer>,
}

impl RepairPlan {
	pub fn is_noop(&self) -> bool {
		self.actions.iter().all(|a| {
			matches!(
				a.action,
				ReferrerAction::Leave | ReferrerAction::LeaveForeign
			)
		})
	}

	/// Refusals block the WHOLE plan: a partially applied repair is how a skill
	/// ends up readable from nowhere.
	pub fn refusals(&self) -> Vec<(&PlannedReferrer, &RefuseReason)> {
		self.actions
			.iter()
			.filter_map(|a| match &a.action {
				ReferrerAction::Refuse { reason } => Some((a, reason)),
				_ => None,
			})
			.collect()
	}

	/// The directory this plan will adopt as the Master, if any.
	pub fn adopts(&self) -> Option<&Path> {
		self.actions
			.iter()
			.find(|a| a.action == ReferrerAction::AdoptAsMaster)
			.map(|a| a.path.as_path())
	}
}

/// Compute the repair plan for one skill. Pure: reads the filesystem, writes
/// nothing.
///
/// `in_lock` is the caller's answer to "does a lock entry name this skill". It
/// is a parameter rather than a lock read here because D5 hangs on it — only a
/// lock-named skill may be adopted as a Master; anything else is content aghub
/// did not install and must not move — and because the lock read must fail
/// CLOSED at the surface that reports it (root AGENTS.md), which is a decision
/// this pure function has no business making.
///
/// `grant_to` is the migration-time question, also the caller's: which agents
/// read this skill TODAY and are therefore owed an explicit Referrer once the
/// Master moves. Empty means "grant nobody new".
pub fn plan_repair(
	scope: ResourceScope,
	project_root: Option<&Path>,
	name: &str,
	in_lock: bool,
	grant_to: &[&str],
) -> Option<RepairPlan> {
	// `Both` is the default scope of doctor / check / source list, and it names
	// no single store. Answering with an empty plan would report `is_noop` for a
	// host that badly needs migrating.
	if matches!(scope, ResourceScope::Both) {
		return None;
	}
	let master = master_path(scope, project_root, name)?;
	let shared_slot = shared_referrer_dir(store_root(scope, project_root))
		.map(|d| d.join(skill::sanitize_name(name)));

	// Collapse the candidates by PATH: the shared slot is one directory that up
	// to eight agents resolve to. Compare the constructed paths, never resolved
	// ones — an Absent candidate does not canonicalize, so resolving would fold
	// every ungranted agent into one bucket.
	let mut order: Vec<PathBuf> = Vec::new();
	let mut by_path: std::collections::HashMap<PathBuf, Vec<&'static str>> =
		std::collections::HashMap::new();
	for candidate in candidate_referrers(scope, project_root, name) {
		by_path
			.entry(candidate.path.clone())
			.or_insert_with(|| {
				order.push(candidate.path.clone());
				Vec::new()
			})
			.push(candidate.agent_id);
	}

	let master_exists = referrer_or_master_exists(&master);
	let mut planned: Vec<PlannedReferrer> = order
		.into_iter()
		.map(|path| {
			let shared = shared_slot.as_deref() == Some(path.as_path());
			let shape = classify_shape(&path, &master);
			let agents = by_path.remove(&path).unwrap_or_default();
			let action = action_for(
				&shape,
				shared,
				in_lock,
				master_exists,
				&agents,
				grant_to,
			);
			PlannedReferrer {
				agents,
				path,
				shape,
				action,
				shared,
			}
		})
		.collect();

	// Exactly one adoption, and only from the shared slot. Registry order must
	// never decide this: it once let an agent's PRIVATE copy win over the shared
	// slot and become the Master, while the real Master-to-be was planned for
	// Relink — the one action that destroys a directory.
	let adopting = planned
		.iter()
		.any(|p| p.action == ReferrerAction::AdoptAsMaster);

	// Nothing to point at and nothing to adopt: writing Referrers now would
	// create links to nothing. Refuse the whole plan rather than half-build it.
	if !master_exists && !adopting {
		for entry in &mut planned {
			if matches!(
				entry.action,
				ReferrerAction::Create | ReferrerAction::Relink
			) {
				entry.action = ReferrerAction::Refuse {
					reason: RefuseReason::MasterMissing,
				};
			}
		}
	}

	// Execution order: adopt the Master, then private Referrers, then the shared
	// slot. Reversing this is the difference between a crash that leaves the old
	// directory serving the skill and one that leaves it readable from nowhere.
	planned.sort_by_key(|p| match p.action {
		ReferrerAction::AdoptAsMaster => 0,
		_ if p.shared => 2,
		_ => 1,
	});

	Some(RepairPlan {
		name: name.to_string(),
		master,
		master_exists,
		actions: planned,
	})
}

fn action_for(
	shape: &SkillShape,
	shared: bool,
	in_lock: bool,
	master_exists: bool,
	agents: &[&'static str],
	grant_to: &[&str],
) -> ReferrerAction {
	match shape {
		SkillShape::Conformant => ReferrerAction::Leave,
		SkillShape::Absent => {
			// Migration step 2: an agent that reads the skill today is owed an
			// explicit link once the Master moves. Anything else stays ungranted
			// — repair must not hand a skill to an agent nobody asked for.
			if master_exists && agents.iter().any(|a| grant_to.contains(a)) {
				ReferrerAction::Create
			} else {
				ReferrerAction::Leave
			}
		}
		// Only the SHARED slot of a LOCK-NAMED skill may become the Master.
		// Every other real directory is content aghub did not install (D5).
		SkillShape::UnmigratedCopy => {
			if shared && in_lock {
				ReferrerAction::AdoptAsMaster
			} else {
				ReferrerAction::LeaveForeign
			}
		}
		SkillShape::AliasedMaster => ReferrerAction::Refuse {
			reason: RefuseReason::AliasedMaster,
		},
		SkillShape::Violation(kind) => match kind {
			ViolationKind::MasterIsLink => ReferrerAction::Refuse {
				reason: RefuseReason::MasterIsLink,
			},
			ViolationKind::MasterIsNotADir => ReferrerAction::Refuse {
				reason: RefuseReason::MasterIsNotADir,
			},
			ViolationKind::ReferrerIsNotADir => ReferrerAction::Refuse {
				reason: RefuseReason::ReferrerIsNotADir,
			},
			ViolationKind::ForkedCopy => ReferrerAction::CompareThenQuarantine,
			ViolationKind::Chain { .. }
			| ViolationKind::ForeignTarget
			| ViolationKind::Dangling => ReferrerAction::Relink,
		},
	}
}

/// Refuse a removal that would destroy bytes existing nowhere else.
///
/// **Shares [`plan_repair`]'s observation, NOT its policy.** It reuses the
/// collapsed candidate set (one entry per directory, with the `shared` flag)
/// because deriving that twice is how two answers drift apart. It deliberately
/// ignores the action column, because repair and removal refuse different
/// things: repair refuses what it cannot FIX (a link pointing at nothing is
/// `Refuse { MasterMissing }` — unfixable without a Master), while removal
/// refuses only what it cannot UNDO. Unlinking a dangling link destroys
/// nothing, so mapping repair's refusals onto delete refused every
/// pre-migration user's delete outright.
///
/// Exactly two shapes block:
///
/// - [`ViolationKind::ForkedCopy`] **at the shared slot**. That directory is
///   supposed to be a link; real content there was written by something else
///   (every npx write verb calls `cleanAndCreateDirectory`), so its bytes may
///   exist nowhere else.
/// - [`SkillShape::AliasedMaster`] — the "duplicate" is the original, reached
///   through a symlinked parent. Removing it removes the only copy.
///
/// Everything else is allowed, and two exclusions are load-bearing:
///
/// - A forked copy in an agent's PRIVATE directory stays legal. Removing a
///   private copy that shadows a Master is documented, tested behaviour (root
///   `AGENTS.md`: it "DOES take something away, and stays legal — the Master it
///   falls back to is disclosed in `skipped`"). A guard must not quietly
///   relitigate a spec decision.
/// - Link shapes (`Dangling`, `ForeignTarget`, `Chain`) and the master-side
///   violations are repair problems, not delete hazards.
///
/// `in_lock: false` is passed deliberately and cannot change the verdict — it
/// only picks between `AdoptAsMaster` and `LeaveForeign`, neither of which this
/// function reads. A lock read here would fail open at a layer whose job is to
/// fail closed.
///
/// `Ok(())` for [`ResourceScope::Both`] and for a scope with no store root:
/// `plan_repair` names no single store there, and refusing every removal a
/// scopeless caller makes would be a guess, not a guard.
pub fn verify_shape(
	scope: ResourceScope,
	project_root: Option<&Path>,
	name: &str,
) -> crate::errors::Result<()> {
	let Some(plan) = plan_repair(scope, project_root, name, false, &[]) else {
		return Ok(());
	};
	// Exhaustive on the blocking shapes ON PURPOSE. An earlier version matched
	// the action and fell through a `_` arm for the detail text, which printed
	// "a foreign link target … something that is neither a link nor a
	// directory" — two different shapes in one sentence. With the blocking set
	// enumerated here, a wrong detail is unreachable.
	let blocker = plan.actions.iter().find_map(|a| match &a.shape {
		SkillShape::Violation(ViolationKind::ForkedCopy) if a.shared => Some((
			a,
			"a real directory sits in the shared slot where a link to the \
			 store belongs, and its content may exist nowhere else",
		)),
		SkillShape::AliasedMaster => Some((
			a,
			"it IS the master reached through a symlinked parent, so the \
			 \"duplicate\" is the only copy",
		)),
		_ => None,
	});
	let Some((blocker, detail)) = blocker else {
		return Ok(());
	};
	// Built directly rather than through `ConfigError::unsupported_operation`,
	// whose "Cannot {op} {noun} for {agent} agent" template has no room for an
	// explanation and would name the target agent — misleading here, because the
	// blocking directory is the SHARED slot, read by agents the command never
	// mentioned. The error CODE is what the wire contract pins
	// (`UNSUPPORTED_OPERATION` / HTTP 422), not the prose.
	let writers = if blocker.agents.is_empty() {
		String::new()
	} else {
		// Their WRITE dir, which is what `PlannedReferrer::agents` holds.
		// Deliberately not "read by": more agents than these read a shared
		// slot, and naming only the writers as readers would under-report.
		format!(" (the skills directory of {})", blocker.agents.join(", "))
	};
	Err(crate::errors::ConfigError::UnsupportedOperation(format!(
		"Cannot remove skill '{name}': {} at {}{writers} — {detail}. Run \
		 `aghub skills repair {name}` first; deleting now could destroy content \
		 aghub cannot recover.",
		blocker.shape_label(),
		blocker.path.display(),
	)))
}

impl PlannedReferrer {
	/// Short human label for the observed shape, for error text.
	fn shape_label(&self) -> &'static str {
		match &self.shape {
			SkillShape::Conformant => "a conformant referrer",
			SkillShape::Absent => "nothing",
			SkillShape::UnmigratedCopy => "an un-migrated copy",
			SkillShape::AliasedMaster => "an aliased master",
			SkillShape::Violation(kind) => match kind {
				ViolationKind::Chain { .. } => "a link chain",
				ViolationKind::ForeignTarget => "a foreign link target",
				ViolationKind::Dangling => "a dangling link",
				ViolationKind::ForkedCopy => "a forked copy",
				ViolationKind::MasterIsLink => "a linked master",
				ViolationKind::MasterIsNotADir => "a non-directory master",
				ViolationKind::ReferrerIsNotADir => "a non-directory referrer",
			},
		}
	}
}
