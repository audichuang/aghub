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

use crate::skills::linker::Linker;

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
