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
//! D5–D8 are rows of the decision table in `.scratch/aghub-skill-store/spec.md`.
//!
//! Three traps are load-bearing here; each has a test below that goes red if
//! the guard is removed.
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
use crate::skills::removal::entry_identity;

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
/// Path-derived, never shape-derived: each agent's own WRITE dir (private where
/// it has one, the shared `.agents/skills` slot where it does not), so a broken
/// Referrer is reported instead of filtered out.
/// See docs/history/core-skills-shape.md#candidate-referrers-were-once-shape-derived
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
/// Asks the adapter directly, never a link-need classification: an aliased
/// store must not drop shared-slot agents out of the candidate set.
/// See docs/history/core-skills-shape.md#aliased-store-dropped-shared-slot-agents
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

/// One agent's skills READ dirs for a scope: its write dir plus every
/// compat/legacy dir the descriptor still reads.
///
/// Routed through the adapter for the same reason [`skill_write_dir`] is —
/// otherwise the test path override is bypassed and a fixture's dirs are
/// invisible.
fn skill_read_dirs(
	descriptor: &aghub_agents::AgentDescriptor,
	scope: ResourceScope,
	project_root: Option<&Path>,
) -> Vec<PathBuf> {
	match crate::AgentType::from_str(descriptor.id) {
		Ok(agent_type) => crate::create_adapter(agent_type)
			.get_skills_paths(project_root, scope),
		Err(_) => match scope {
			ResourceScope::GlobalOnly => descriptor.global_skill_read_paths(),
			ResourceScope::ProjectOnly => project_root
				.map(|root| descriptor.project_skill_read_paths(root))
				.unwrap_or_default(),
			ResourceScope::Both => Vec::new(),
		},
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

/// Which agents can READ `name` at this scope right now.
///
/// This is migration's `grant_to` (D6: an agent that reads the skill today is
/// owed an explicit Referrer once the Master moves), so call it BEFORE anything
/// is moved. Read paths, not write dirs: an agent reading the shared slot while
/// writing its own dir would otherwise lose the skill.
///
/// A path counts only when it holds a root `SKILL.md`
/// ([`SkillMarker::Present`]); a same-named category dir or an unreadable dir
/// ([`SkillMarker::Unknown`]) is no evidence of a read and must not seed an
/// implicit `Create`. A definitively DANGLING link also counts: it is a
/// stranded Referrer repair must mend, and the marker probe alone reads it as
/// `Absent`.
///
/// Do not swap [`is_dangling_link`]'s `Linker::is_link` for `is_link_checked`:
/// the lossy form reads an unreadable parent as "not a reader" (this function's
/// safe direction); fail-closed is right only for `compat_unlink_permitted`,
/// where the outcome is a refusal rather than a silent non-grant.
/// See docs/history/core-skills-shape.md#readers-of-counted-bare-existence and docs/history/core-skills-shape.md#dangling-referrer-rescue
pub fn readers_of(
	scope: ResourceScope,
	project_root: Option<&Path>,
	name: &str,
) -> Vec<&'static str> {
	let safe = skill::sanitize_name(name);
	crate::registry::ALL_AGENTS
		.iter()
		.filter(|descriptor| {
			descriptor
				.skill_read_paths(project_root, scope)
				.iter()
				.any(|dir| {
					let entry = dir.join(&safe);
					has_skill_marker(&entry) == SkillMarker::Present
						|| is_dangling_link(&entry)
				})
		})
		.map(|descriptor| descriptor.id)
		.collect()
}

/// A link whose target is DEFINITIVELY gone (`NotFound`).
///
/// `is_link` alone is too wide: a link to a dir with no `SKILL.md` would count
/// as a prior grant and hand the skill to every shared-slot agent. Any other
/// unresolvable target (EACCES, ELOOP, a dead mount) is unknown and must not
/// seed a grant. See docs/history/core-skills-shape.md#dangling-referrer-rescue
fn is_dangling_link(entry: &Path) -> bool {
	Linker::is_link(entry)
		&& std::fs::metadata(entry)
			.err()
			.is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound)
}

/// Why a `(referrer, master)` pair is not usable as-is.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
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
/// Observation only, no policy: this cannot see the lock, so whether an
/// [`Self::UnmigratedCopy`] may be adopted is [`plan_repair`]'s call (D5).
/// See docs/history/core-skills-shape.md#legacy-shape-variant-offered-user-dirs-for-adoption
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
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
	/// A real directory sits at the Referrer path and is NOT a skill at all —
	/// it has no root `SKILL.md`. Almost always a NAME COLLISION: several
	/// agents group their own skills under a category directory, so
	/// `~/.hermes/skills/research/` holds fourteen sub-skills and a
	/// `DESCRIPTION.md` while aghub happens to manage a skill called
	/// `research`. Distinguished from `ForkedCopy` because the remedies are
	/// opposites: a fork is compared and quarantined, whereas this must be left
	/// strictly alone. See docs/history/core-skills-shape.md#repair-quarantined-category-directories
	ForeignDir,
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
	/// `ForeignDir` is excluded with the other occupied-slot shapes: aghub
	/// cannot put a Referrer where somebody else's directory already sits, and
	/// pretending otherwise would have a mutating flow plan a write that must
	/// never happen.
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
	// A directory with no root `SKILL.md` is not a skill, only a name
	// collision. Asked BEFORE the fork/unmigrated split, because both of those
	// lead somewhere destructive (quarantine, adoption as the Master).
	// `Unknown` counts as `Present` here — the OPPOSITE of `readers_of` — so an
	// unreadable dir routes to a visible refusal instead of `LeaveForeign`
	// (see [`SkillMarker`]).
	if has_skill_marker(referrer) == SkillMarker::Absent {
		return SkillShape::ForeignDir;
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

/// The answer to "does `entry` hold a root `SKILL.md`" — three states, not a
/// bool, because the two callers below need OPPOSITE conservative answers
/// when the probe genuinely cannot tell.
///
/// `classify_shape` treats [`Self::Unknown`] as [`Self::Present`] (an
/// unreadable dir routes to a refusal a human sees, not a silent
/// `ForeignDir`). `readers_of` treats it as NOT present (an unanswered probe is
/// no evidence of a read, and counting it seeds an implicit `Create`). Never
/// collapse this to a bool. See docs/history/core-skills-shape.md#one-bool-marker-served-two-callers
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SkillMarker {
	/// A root `SKILL.md` file is really there (`metadata` follows links, so a
	/// healthy Referrer still passes through to the Master's own file).
	Present,
	/// A definite absence: either `NotFound`, or one path segment up,
	/// `NotADirectory` — a same-named REGULAR FILE at `entry` makes
	/// `metadata("<entry>/SKILL.md")` fail that way instead of `NotFound`,
	/// but it is just as definite. Nothing can live under a non-directory
	/// path, so there is no ambiguity to fail open about.
	Absent,
	/// The probe could not tell — most often a permission fault. Never
	/// "present" and never "absent"; see the type doc for how each caller
	/// resolves it.
	Unknown,
}

/// Whether `entry` is actually serving A SKILL, by ONE rule shared by every
/// caller in THIS module that asks this question ([`classify_shape`]'s
/// `ForeignDir` split and [`readers_of`]'s `Present` check) — each picks its
/// own safe direction for [`SkillMarker::Unknown`]; see that type's doc.
///
/// NOT the only spelling in the crate: `prune::top_level_skill_dirs` uses a
/// bare `is_file()` that folds EACCES into `false`. Unifying it onto this is
/// not a mechanical change — it alters which lock keys `prune` keeps.
fn has_skill_marker(entry: &Path) -> SkillMarker {
	match std::fs::metadata(entry.join("SKILL.md")) {
		Ok(meta) if meta.is_file() => SkillMarker::Present,
		Ok(_) => SkillMarker::Absent,
		Err(e)
			if matches!(
				e.kind(),
				std::io::ErrorKind::NotFound
					| std::io::ErrorKind::NotADirectory
			) =>
		{
			SkillMarker::Absent
		}
		Err(_) => SkillMarker::Unknown,
	}
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

	/// A directory sharing the NAME of a skill is not a copy of it.
	///
	/// Several agents group their own skills under a category directory:
	/// `~/.hermes/skills/research/` holds a `DESCRIPTION.md` and fourteen
	/// sub-skills, and no root `SKILL.md`. Classifying that as `ForkedCopy`
	/// sent it to `CompareThenQuarantine`, which hashed it, found it different
	/// and refused with "compare them, then keep the one you want" — advice
	/// that, followed, moves fourteen of somebody else's skills aside.
	#[test]
	fn a_directory_that_is_not_a_skill_is_foreign_not_a_fork() {
		let (_tmp, master, agent_dir) = fixture();
		let category = agent_dir.join("foo");
		fs::create_dir_all(category.join("arxiv")).unwrap();
		fs::write(category.join("DESCRIPTION.md"), "a group of skills\n")
			.unwrap();
		// The sub-skill has one; the category directory itself does not.
		fs::write(
			category.join("arxiv").join("SKILL.md"),
			"---\nname: arxiv\n---\n",
		)
		.unwrap();

		assert_eq!(
			classify_shape(&category, &master),
			SkillShape::ForeignDir,
			"no root SKILL.md means it is not this skill at all"
		);
		assert!(
			!SkillShape::ForeignDir.is_actionable(),
			"the slot is occupied, so no mutating flow may plan a write there"
		);
		assert_eq!(
			action_for(&SkillShape::ForeignDir, false, true, true, &[], &[]),
			ReferrerAction::LeaveForeign,
			"report only — never hash it, never move it"
		);
	}

	/// A link is only a prior grant when its target is DEFINITIVELY gone.
	///
	/// Asked THROUGH `readers_of`, not of the helper: the helper being right
	/// buys nothing if the call site widens again. Pins that a link to an
	/// existing directory holding no `SKILL.md` never seeds `grant_to` (else
	/// `repair --yes` hands the managed skill to every reader of the shared
	/// slot).
	/// See docs/history/core-skills-shape.md#dangling-referrer-rescue
	#[test]
	fn readers_of_counts_only_a_definitively_dangling_link() {
		let tmp = tempfile::tempdir().unwrap();
		let root = fs::canonicalize(tmp.path()).unwrap();
		fs::create_dir_all(root.join(".claude")).unwrap();
		let name = "demo";
		let write_skill_dir = |dir: &Path| {
			fs::create_dir_all(dir).unwrap();
			fs::write(dir.join("SKILL.md"), "---\nname: demo\n---\n").unwrap();
		};
		write_skill_dir(&root.join(".aghub").join(name));
		// antigravity's PROJECT compat alias; its write dir is `.agents/skills`.
		let compat = root.join(".agent").join("skills");
		fs::create_dir_all(&compat).unwrap();
		let entry = compat.join(name);
		let readers = |root: &Path| {
			readers_of(ResourceScope::ProjectOnly, Some(root), name)
		};

		// (a) the rescue: the target is gone, so the link is the only evidence
		//     this agent was ever granted the skill.
		unix_fs::symlink(root.join("gone"), &entry).unwrap();
		assert!(
			readers(&root).contains(&"antigravity"),
			"a link to a missing target is the shape repair exists to mend"
		);
		fs::remove_file(&entry).unwrap();

		// (b) a link to a real directory serving NO skill — NOT a grant.
		let not_a_skill = root.join("not-a-skill");
		fs::create_dir_all(&not_a_skill).unwrap();
		unix_fs::symlink(&not_a_skill, &entry).unwrap();
		assert!(
			!readers(&root).contains(&"antigravity"),
			"a link to content that is not this skill must not seed a grant, \
			 got {:?}",
			readers(&root)
		);
		fs::remove_file(&entry).unwrap();

		// (c) a link to a real skill dir needs no OR-clause — already Present.
		let other = root.join("other");
		write_skill_dir(&other);
		unix_fs::symlink(&other, &entry).unwrap();
		assert!(readers(&root).contains(&"antigravity"));
	}

	/// ABSENT is not UNREADABLE — folding the two is fail-OPEN.
	///
	/// A bare `is_file()` probe answers false for a `SKILL.md` inside a
	/// `chmod 000` directory just as it does for one that is not there, so a
	/// real skill aghub installed classified as somebody else's content and got
	/// `LeaveForeign`: a permission fault turned into a silent no-op instead of
	/// the `Failed` report that names it. Only a definite `NotFound` may demote
	/// the directory.
	#[test]
	fn an_unreadable_directory_is_not_mistaken_for_a_foreign_one() {
		use std::os::unix::fs::PermissionsExt;
		let (_tmp, master, agent_dir) = fixture();
		let fork = agent_dir.join("foo");
		fs::create_dir_all(&fork).unwrap();
		fs::write(fork.join("SKILL.md"), "---\nname: foo\n---\n").unwrap();
		fs::set_permissions(&fork, fs::Permissions::from_mode(0o000)).unwrap();
		// Root ignores the mode bits, so the probe would still succeed there.
		let enforced = fs::read_dir(&fork).is_err();
		let shape = classify_shape(&fork, &master);
		fs::set_permissions(&fork, fs::Permissions::from_mode(0o755)).unwrap();

		if !enforced {
			eprintln!("skip: perms not enforced (root)");
			return;
		}
		assert_eq!(
			shape,
			SkillShape::Violation(ViolationKind::ForkedCopy),
			"an undecidable probe must fall through to the conservative \
			 answer, never to ForeignDir"
		);
	}

	/// The other half of the same rule: a real directory that IS a skill beside
	/// a live Master is still a fork, and still gets compared. Without this the
	/// probe above could be widened until nothing was ever a fork again.
	#[test]
	fn a_real_skill_directory_beside_the_master_is_still_a_fork() {
		let (_tmp, master, agent_dir) = fixture();
		let fork = agent_dir.join("foo");
		fs::create_dir_all(&fork).unwrap();
		fs::write(fork.join("SKILL.md"), "---\nname: foo\n---\n").unwrap();

		assert_eq!(
			classify_shape(&fork, &master),
			SkillShape::Violation(ViolationKind::ForkedCopy)
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

	/// `git -C <root> <args>`, asserted to succeed. No commit anywhere below:
	/// `git ls-files` reads the INDEX, so `git add` alone is what "tracked"
	/// means — and it needs no `user.name`/`user.email`, which a fresh CI
	/// container does not have.
	fn git(root: &Path, args: &[&str]) {
		let status = std::process::Command::new("git")
			.arg("-C")
			.arg(root)
			.args(args)
			.stdout(std::process::Stdio::null())
			.stderr(std::process::Stdio::null())
			.status()
			.expect("this test needs the `git` binary");
		assert!(
			status.success(),
			"git {args:?} failed in {}",
			root.display()
		);
	}

	/// B6, the bug this guard exists for: `.agents/skills/<n>` written IN PLACE
	/// and committed has the very same shape as a pre-2.18 install
	/// (`UnmigratedCopy`), and the lock names it either way — so `repair -p
	/// --yes` renamed 39 tracked skill files into an ignored store and exited 0
	/// with doctor green. Git tracking is the only signal that separates
	/// authored source from an install, so the plan has to ask.
	#[test]
	fn a_git_tracked_shared_slot_refuses_instead_of_migrating() {
		let _env = crate::skills::prune::test_lock::env_lock()
			.lock()
			.unwrap_or_else(|error| error.into_inner());
		let (_tmp, root) = project_fixture();
		let slot = shared_slot(&root);
		write_skill(&slot, "---\nname: foo\n---\n");
		git(&root, &["init", "-q"]);
		git(&root, &["add", "--", ".agents/skills/foo/SKILL.md"]);

		let p = plan(&root, true, &[]);
		assert_eq!(
			action_at(&p, &slot),
			&ReferrerAction::Refuse {
				reason: RefuseReason::GitTrackedSource {
					paths: vec![slot.clone()]
				}
			},
			"a tracked real directory is authored source: renaming it away \
			 deletes the skill from version control"
		);
		assert_eq!(
			p.adopts(),
			None,
			"nothing may be adopted out of a tracked directory"
		);
		assert!(!p.refusals().is_empty(), "the whole plan must be blocked");
	}

	/// The other half, and the reason the verdict is "tracked" and not "inside
	/// a repo": migrating an UNtracked real directory is exactly what `repair`
	/// is for. Widen the guard to the repository and this goes red.
	#[test]
	fn an_untracked_shared_slot_inside_a_repo_still_migrates() {
		let _env = crate::skills::prune::test_lock::env_lock()
			.lock()
			.unwrap_or_else(|error| error.into_inner());
		let (_tmp, root) = project_fixture();
		let slot = shared_slot(&root);
		write_skill(&slot, "---\nname: foo\n---\n");
		git(&root, &["init", "-q"]);

		let p = plan(&root, true, &[]);
		assert_eq!(action_at(&p, &slot), &ReferrerAction::AdoptAsMaster);
		assert_eq!(p.adopts(), Some(slot.as_path()));
		assert!(
			p.refusals().is_empty(),
			"untracked content is aghub's to move"
		);
	}

	/// `CompareThenQuarantine` moves a real directory too — into
	/// `.aghub/.quarantine/` — so guarding only `AdoptAsMaster` would leave
	/// half of B6 in place. On the VM this was the arm that actually ran.
	#[test]
	fn a_git_tracked_fork_is_not_quarantined() {
		let _env = crate::skills::prune::test_lock::env_lock()
			.lock()
			.unwrap_or_else(|error| error.into_inner());
		let (_tmp, root) = project_fixture();
		write_skill(&root.join(".aghub").join("foo"), "---\nname: foo\n---\n");
		let fork = root.join(".claude").join("skills").join("foo");
		write_skill(&fork, "---\nname: foo\n---\n# authored here\n");
		git(&root, &["init", "-q"]);
		git(&root, &["add", "--", ".claude/skills/foo/SKILL.md"]);

		let p = plan(&root, true, &[]);
		assert_eq!(
			action_at(&p, &fork),
			&ReferrerAction::Refuse {
				reason: RefuseReason::GitTrackedSource {
					paths: vec![fork.clone()]
				}
			},
			"quarantining a tracked fork loses it from version control just \
			 like adopting one"
		);
	}

	/// Third state, not a bool: a repository is right there but `git` cannot
	/// answer. Migrating on an unanswered probe is how B6 happened, so it
	/// refuses — with its OWN reason, because the remedy is different.
	#[test]
	fn an_unanswerable_git_probe_refuses_rather_than_migrating() {
		let _env = crate::skills::prune::test_lock::env_lock()
			.lock()
			.unwrap_or_else(|error| error.into_inner());
		let (_tmp, root) = project_fixture();
		let slot = shared_slot(&root);
		write_skill(&slot, "---\nname: foo\n---\n");
		// A gitfile git refuses to parse: `.git` exists, every `git` command
		// below it exits 128. Same shape as a missing `git` binary, without
		// mutating `PATH` for the whole test process.
		fs::write(root.join(".git"), "not a gitfile\n").unwrap();

		let p = plan(&root, true, &[]);
		assert_eq!(
			action_at(&p, &slot),
			&ReferrerAction::Refuse {
				reason: RefuseReason::GitTrackingUndecided {
					path: slot.clone()
				}
			},
			"undecided is not permission to migrate"
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

	/// A hand-placed private copy must not beat the shared slot to become the
	/// Master. Selection is by SLOT, never by registry order.
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

	/// A disabled agent is not aghub's to manage: its broken Referrer is left
	/// exactly as found, while a managed agent's identical one is still mended.
	#[test]
	fn a_disabled_agents_broken_link_is_left_alone() {
		let (_tmp, root) = project_fixture();
		write_skill(&root.join(".aghub").join("foo"), "master");
		let private = root.join(".claude").join("skills");
		fs::create_dir_all(&private).unwrap();
		unix_fs::symlink(root.join("nowhere"), private.join("foo")).unwrap();

		let _off = crate::agent_settings::test_override::disable(&["claude"]);
		let p = plan(&root, true, &[]);
		assert_eq!(
			action_at(&p, &private.join("foo")),
			&ReferrerAction::Leave,
			"repair must not write into a disabled agent's skills dir"
		);
		assert!(p.is_noop(), "nothing else here needs repair");
		drop(_off);

		assert_eq!(
			action_at(&plan(&root, true, &[]), &private.join("foo")),
			&ReferrerAction::Relink,
			"re-enabled, the same link is repaired again"
		);
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
			rows[0].agents == vec!["amp"],
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

	/// `.agent/skills` (antigravity's read-only compat dir) aliased AT THE
	/// DIRECTORY LEVEL onto `.agents/skills` — the layout `stow` produces —
	/// must never schedule the physical shared slot for `Unlink`: compare by
	/// entry identity, not `PathBuf` spelling.
	/// See docs/history/core-skills-shape.md#compat-sweep-unlinked-the-physical-shared-slot
	#[test]
	fn an_aliased_compat_dir_never_schedules_the_shared_slot_for_unlink() {
		let (_tmp, root) = project_fixture();
		write_skill(&root.join(".aghub").join("foo"), "---\nname: foo\n---\n");
		let master = root.join(".aghub").join("foo");

		// `.agents/skills` is the shared write slot: healthy, one hop to the
		// Master.
		let shared_dir = root.join(".agents").join("skills");
		fs::create_dir_all(&shared_dir).unwrap();
		unix_fs::symlink(&master, shared_dir.join("foo")).unwrap();

		// `.agent/skills` (singular) is antigravity's read-only compat dir,
		// aliased to the shared slot AT THE DIRECTORY LEVEL.
		fs::create_dir_all(root.join(".agent")).unwrap();
		unix_fs::symlink(&shared_dir, root.join(".agent").join("skills"))
			.unwrap();

		let p = plan(&root, true, &[]);
		// Assert on IDENTITY, not on the alias spelling. Since antigravity's
		// project write dir became `.agent/skills` the sweep skips that read
		// dir as its own slot, so the alias path is never even constructed and
		// a `== aliased_entry` assertion passes without testing anything. What
		// must stay protected is the PHYSICAL shared slot, whichever spelling
		// reaches it.
		let shared_entry = shared_dir.join("foo");
		let aliased_entry = root.join(".agent").join("skills").join("foo");
		for entry in [&shared_entry, &aliased_entry] {
			assert!(
				!p.actions
					.iter()
					.any(|a| entry_identity(&a.path) == entry_identity(entry)
						&& a.action == ReferrerAction::Unlink),
				"an entry reached through a symlinked ancestor must never \
				 schedule the directory it aliases for unlink ({}): {:?}",
				entry.display(),
				p.actions
			);
		}
	}

	// GUARD 4, and the only place it can be made to go red.
	//
	// The sweep authorizes a detach on behalf of EVERY agent reading the entry,
	// but the real roster cannot stage disagreement: there is no directory read
	// by two agents and written by none, and `set_skills_path_override` cannot
	// invent one (it is a single thread-local pair that replaces an agent's read
	// paths AND its write path with the same dir, which the sweep then skips as
	// that agent's own slot). So the roster arrives as data — that is what the
	// `compat_unlink_authorized` seam is for. An end-to-end test here would be
	// green before and after the fix: exactly the false green root `AGENTS.md`
	// Testing warns about.
	#[test]
	fn a_shared_compat_entry_is_spared_unless_every_reader_is_covered() {
		let (_tmp, root) = project_fixture();
		let shared = root.join("shared-read-only").join("skills");
		fs::create_dir_all(&shared).unwrap();
		let entry = shared.join("demo");

		// `d` has a private slot and also reads the shared dir; `e` reads the
		// shared dir and nothing else.
		let roster = vec![
			AgentDirs {
				id: "d",
				write: Some(root.join("d").join("skills")),
				read: vec![root.join("d").join("skills"), shared.clone()],
			},
			AgentDirs {
				id: "e",
				write: Some(root.join("e").join("skills")),
				read: vec![shared.clone()],
			},
		];

		// THE DEFECT: `d` is covered and used to authorize the detach by
		// itself. `e` reads the skill from this entry alone, so detaching it
		// revokes the skill for an agent that never got a vote.
		let only_d: std::collections::HashSet<&'static str> =
			std::iter::once("d").collect();
		assert!(
			!compat_unlink_authorized(&entry, "demo", &roster, &only_d),
			"a covered reader must not authorize detaching the only entry an \
			 uncovered co-reader has"
		);

		// And the fix must not simply switch the sweep off: with every reader
		// covered the antigravity/pre-2.18 cleanup still has to fire.
		let both: std::collections::HashSet<&'static str> =
			["d", "e"].into_iter().collect();
		assert!(
			compat_unlink_authorized(&entry, "demo", &roster, &both),
			"with every reader served afterwards the detach must still happen"
		);

		// An agent with no write slot at this scope can never be served, so it
		// vetoes rather than being skipped.
		let slotless = vec![
			AgentDirs {
				id: "d",
				write: Some(root.join("d").join("skills")),
				read: vec![root.join("d").join("skills"), shared.clone()],
			},
			AgentDirs {
				id: "e",
				write: None,
				read: vec![shared.clone()],
			},
		];
		assert!(
			!compat_unlink_authorized(&entry, "demo", &slotless, &both),
			"an agent with no write slot has nothing to fall back on"
		);

		// Readers are matched by IDENTITY, not spelling. A co-reader that
		// reaches the same directory through a symlinked ANCESTOR is still a
		// co-reader; matching on `dir.join(leaf) == entry` would drop it out of
		// the quorum and reintroduce the very defect this predicate closes, one
		// level down. Without this case a naive `==` passes every other
		// assertion here unchanged.
		let aliased = root.join("aliased-skills");
		unix_fs::symlink(&shared, &aliased).unwrap();
		let through_alias = vec![
			AgentDirs {
				id: "d",
				write: Some(root.join("d").join("skills")),
				read: vec![root.join("d").join("skills"), shared.clone()],
			},
			AgentDirs {
				id: "e",
				write: Some(root.join("e").join("skills")),
				read: vec![aliased.clone()],
			},
		];
		assert!(
			!compat_unlink_authorized(&entry, "demo", &through_alias, &only_d),
			"an uncovered co-reader that spells the dir through a symlinked \
			 ancestor still vetoes"
		);
		assert!(
			compat_unlink_authorized(&entry, "demo", &through_alias, &both),
			"and the same aliased reader, once covered, must not block the \
			 cleanup"
		);

		// Vacuous truth is the one answer this must never give: `all()` over an
		// empty reader set is `true`, which would detach entries nobody claims.
		assert!(
			!compat_unlink_authorized(
				&root.join("unread").join("demo"),
				"demo",
				&roster,
				&both
			),
			"an entry no agent reads is not detachable"
		);
	}

	/// A same-named category directory with no root `SKILL.md` is not a read:
	/// counting it put an agent that never read the skill in `grant_to` and
	/// turned the absent shared row into `Create`.
	/// See docs/history/core-skills-shape.md#readers-of-counted-bare-existence
	#[test]
	fn readers_of_ignores_a_same_named_category_dir_with_no_root_skill_md() {
		let (_tmp, root) = project_fixture();
		let name = "research";
		write_skill(
			&root.join(".aghub").join(name),
			"---\nname: research\n---\n",
		);

		// antigravity's read-only compat dir: a category directory grouping
		// the agent's OWN skills under a name that happens to collide.
		let compat = root.join(".agent").join("skills").join(name);
		fs::create_dir_all(compat.join("arxiv")).unwrap();
		fs::write(compat.join("DESCRIPTION.md"), "grouped skills\n").unwrap();
		fs::write(
			compat.join("arxiv").join("SKILL.md"),
			"---\nname: arxiv\n---\n",
		)
		.unwrap();

		let write_slot = root.join(".agents").join("skills").join(name);
		assert!(!write_slot.exists(), "fixture premise: write slot absent");

		let readers = readers_of(ResourceScope::ProjectOnly, Some(&root), name);
		assert!(
			!readers.contains(&"antigravity"),
			"a directory with no root SKILL.md is not a read of this skill, \
			 got {readers:?}"
		);

		let p = plan_repair(
			ResourceScope::ProjectOnly,
			Some(&root),
			name,
			true,
			&readers,
		)
		.unwrap();
		assert_eq!(
			action_at(&p, &write_slot),
			&ReferrerAction::Leave,
			"an agent nobody granted must not get an implicit Create just \
			 because a same-named foreign directory sits in a dir it reads"
		);
	}

	/// The same fixture as the test above, but the category dir is made
	/// UNREADABLE first. Pins the `SkillMarker::Unknown` direction THROUGH
	/// `readers_of` specifically — `an_unreadable_directory_is_not_mistaken_for_a_foreign_one`
	/// above only pins `classify_shape`'s (opposite) direction.
	/// See docs/history/core-skills-shape.md#one-bool-marker-served-two-callers
	#[test]
	fn readers_of_treats_an_unreadable_same_named_dir_as_not_a_reader() {
		use std::os::unix::fs::PermissionsExt;
		let (_tmp, root) = project_fixture();
		let name = "research";
		write_skill(
			&root.join(".aghub").join(name),
			"---\nname: research\n---\n",
		);

		let compat = root.join(".agent").join("skills").join(name);
		fs::create_dir_all(compat.join("arxiv")).unwrap();
		fs::write(compat.join("DESCRIPTION.md"), "grouped skills\n").unwrap();
		fs::write(
			compat.join("arxiv").join("SKILL.md"),
			"---\nname: arxiv\n---\n",
		)
		.unwrap();

		fs::set_permissions(&compat, fs::Permissions::from_mode(0o000))
			.unwrap();
		// `compat` itself exists (created above), so — unlike probing a leaf
		// that may not exist — `read_dir` failing here really does mean the
		// mode bit is enforced, not merely that nothing is there; root
		// bypasses the mode and `read_dir` succeeds regardless.
		let enforced = fs::read_dir(&compat).is_err();
		let readers = readers_of(ResourceScope::ProjectOnly, Some(&root), name);
		fs::set_permissions(&compat, fs::Permissions::from_mode(0o755))
			.unwrap();

		if !enforced {
			eprintln!("skip: perms not enforced (root)");
			return;
		}
		assert!(
			!readers.contains(&"antigravity"),
			"an unreadable directory is not evidence of a read either way — \
			 it must not count as `Present` any more than a readable foreign \
			 one does, got {readers:?}"
		);
	}

	/// The consequence that actually matters: with the unreadable directory's
	/// `readers_of` output fed straight into `plan_repair` (exactly what
	/// `repair_skill` does), the write slot must still be `Leave`, never
	/// `Create`. Before the fix this planned `Create` — a symlink into
	/// `.agents/skills`, granting the skill to antigravity and every other
	/// agent that shares that slot, none of whom ever asked for it.
	#[test]
	fn an_unreadable_same_named_dir_never_seeds_an_implicit_create() {
		use std::os::unix::fs::PermissionsExt;
		let (_tmp, root) = project_fixture();
		let name = "research";
		write_skill(
			&root.join(".aghub").join(name),
			"---\nname: research\n---\n",
		);

		let compat = root.join(".agent").join("skills").join(name);
		fs::create_dir_all(compat.join("arxiv")).unwrap();
		fs::write(compat.join("DESCRIPTION.md"), "grouped skills\n").unwrap();
		fs::write(
			compat.join("arxiv").join("SKILL.md"),
			"---\nname: arxiv\n---\n",
		)
		.unwrap();

		fs::set_permissions(&compat, fs::Permissions::from_mode(0o000))
			.unwrap();
		// See the sibling test above for why `read_dir` on `compat` itself,
		// not `metadata` on a leaf that may not exist, is the right probe.
		let enforced = fs::read_dir(&compat).is_err();
		let readers = readers_of(ResourceScope::ProjectOnly, Some(&root), name);
		let p = plan_repair(
			ResourceScope::ProjectOnly,
			Some(&root),
			name,
			true,
			&readers,
		);
		fs::set_permissions(&compat, fs::Permissions::from_mode(0o755))
			.unwrap();

		if !enforced {
			eprintln!("skip: perms not enforced (root)");
			return;
		}
		let p = p.unwrap();
		let write_slot = root.join(".agents").join("skills").join(name);
		assert_eq!(
			action_at(&p, &write_slot),
			&ReferrerAction::Leave,
			"an unreadable same-named directory must never seed an implicit \
			 grant to the shared slot: {:?}",
			p.actions
		);
	}

	/// G-C: a same-named REGULAR FILE, not a directory at all, is a different
	/// error shape than the category-dir cases above — `metadata` on
	/// `"<file>/SKILL.md"` fails with `NotADirectory`, never `NotFound` — but
	/// it is exactly as definite an absence: nothing can live under a
	/// non-directory path, so it must not count as a marker either.
	#[test]
	fn readers_of_ignores_a_same_named_regular_file() {
		let (_tmp, root) = project_fixture();
		let name = "research";
		write_skill(
			&root.join(".aghub").join(name),
			"---\nname: research\n---\n",
		);

		let compat = root.join(".agent").join("skills").join(name);
		fs::create_dir_all(compat.parent().unwrap()).unwrap();
		fs::write(&compat, "not a skill directory at all").unwrap();

		let readers = readers_of(ResourceScope::ProjectOnly, Some(&root), name);
		assert!(
			!readers.contains(&"antigravity"),
			"a same-named regular file is not a read of this skill, got \
			 {readers:?}"
		);
	}

	/// The precise claim behind the test above, pinned directly at the
	/// probe: a regular file fails with `NotADirectory`, not `NotFound`, but
	/// it is exactly as definite an absence and must not fall through to
	/// `Unknown` — which would make `classify_shape` (the OTHER caller,
	/// unreachable on this input today only because it checks `is_dir()`
	/// first) treat it as `Present` were that ordering ever to change.
	#[test]
	fn has_skill_marker_treats_a_same_named_regular_file_as_a_definite_absence()
	{
		let (_tmp, root) = project_fixture();
		let entry = root.join("plain-file");
		fs::write(&entry, "not a skill directory at all").unwrap();

		assert_eq!(
			has_skill_marker(&entry),
			SkillMarker::Absent,
			"NotADirectory is just as definite as NotFound; it must not be \
			 folded into Unknown"
		);
	}

	/// Round-3 regression: round 2's `has_skill_marker` rewrite fixed the
	/// category-dir blocker but broke the rescue it stood next to. A DANGLING
	/// Referrer in a read-only compat dir — the exact shape a migration or a
	/// hand-deleted shared slot leaves behind — asks `has_skill_marker` about
	/// `<entry>/SKILL.md`, which fails `NotFound` the instant the link's own
	/// target is gone, so the stranded reader disappeared from `readers_of`
	/// and its empty write slot planned `Leave` instead of `Create`: the
	/// stranded skill `repair` exists to rescue (`crates/agents/src/agents/
	/// antigravity.rs`) went unrescued. `Linker::is_link` asks about the
	/// entry itself, not its target, so it restores the grant without
	/// reopening the `chmod 000` blocker below (a directory, unreadable or
	/// not, is never a symlink).
	#[test]
	fn readers_of_counts_a_dangling_compat_referrer_as_a_reader() {
		let (_tmp, root) = project_fixture();
		let name = "demo";
		write_skill(&root.join(".aghub").join(name), "---\nname: demo\n---\n");

		// antigravity's read-only compat dir: a Referrer whose target no
		// longer resolves.
		let compat = root.join(".clinerules").join("skills");
		fs::create_dir_all(&compat).unwrap();
		unix_fs::symlink(root.join("nonexistent-target"), compat.join(name))
			.unwrap();

		let readers = readers_of(ResourceScope::ProjectOnly, Some(&root), name);
		assert!(
			readers.contains(&"cline"),
			"a dangling compat referrer is still evidence this agent was \
			 granted the skill, got {readers:?}"
		);

		let write_slot = root.join(".cline").join("skills").join(name);
		// `readers` as `grant_to`, exactly like `repair_skill` wires them.
		let p = plan_repair(
			ResourceScope::ProjectOnly,
			Some(&root),
			name,
			true,
			&readers,
		)
		.unwrap();
		assert_eq!(
			action_at(&p, &write_slot),
			&ReferrerAction::Create,
			"the stranded reader must get a fresh Referrer in its own write \
			 slot: {:?}",
			p.actions
		);
	}

	/// An unreadable real DIRECTORY in a compat read dir must not count as a
	/// reader. Pins the `Linker::is_link(&entry)` clause: the naive
	/// `entry.symlink_metadata().is_ok()` fails this test, because `lstat` on
	/// a directory entry succeeds regardless of the directory's OWN
	/// permission bits.
	/// See docs/history/core-skills-shape.md#dangling-referrer-rescue
	#[test]
	fn readers_of_still_excludes_an_unreadable_compat_directory() {
		use std::os::unix::fs::PermissionsExt;
		let (_tmp, root) = project_fixture();
		let name = "demo";
		write_skill(&root.join(".aghub").join(name), "---\nname: demo\n---\n");

		// antigravity's read-only compat dir, but this time occupied by a
		// real, unreadable directory rather than a dangling link.
		let compat = root.join(".clinerules").join("skills").join(name);
		fs::create_dir_all(&compat).unwrap();
		fs::set_permissions(&compat, fs::Permissions::from_mode(0o000))
			.unwrap();
		let enforced = fs::read_dir(&compat).is_err();

		let readers = readers_of(ResourceScope::ProjectOnly, Some(&root), name);
		fs::set_permissions(&compat, fs::Permissions::from_mode(0o755))
			.unwrap();

		if !enforced {
			eprintln!("skip: perms not enforced (root)");
			return;
		}
		assert!(
			!readers.contains(&"cline"),
			"an unreadable real directory must not count as a reader, got \
			 {readers:?}"
		);
	}

	/// An unreadable compat parent must REFUSE the plan, never silently pass
	/// the sweep by: `symlink_metadata` under an unreadable directory fails
	/// with something other than `NotFound`, which must not read as "nothing
	/// here".
	/// See docs/history/core-skills-shape.md#compat-probe-folded-permission-errors
	#[test]
	fn an_unreadable_compat_dir_refuses_the_plan_instead_of_a_silent_no_op() {
		use std::os::unix::fs::PermissionsExt;
		let (_tmp, root) = project_fixture();
		let name = "demo";
		let master = root.join(".aghub").join(name);
		write_skill(&master, "---\nname: demo\n---\n");
		let write_slot = root.join(".agents").join("skills").join(name);
		fs::create_dir_all(write_slot.parent().unwrap()).unwrap();
		unix_fs::symlink(&master, &write_slot).unwrap();

		// antigravity's compat dir holds a stale referrer, then its PARENT
		// becomes unreadable — `chmod 000` denies the traversal needed to
		// `symlink_metadata` the entry itself, not just a `stat` on the dir.
		let compat_dir = root.join(".clinerules").join("skills");
		fs::create_dir_all(&compat_dir).unwrap();
		unix_fs::symlink(&master, compat_dir.join(name)).unwrap();
		fs::set_permissions(&compat_dir, fs::Permissions::from_mode(0o000))
			.unwrap();
		let enforced = fs::metadata(compat_dir.join(name)).is_err();

		let p = plan_repair(
			ResourceScope::ProjectOnly,
			Some(&root),
			name,
			true,
			&[],
		)
		.unwrap();

		fs::set_permissions(&compat_dir, fs::Permissions::from_mode(0o755))
			.unwrap();

		if !enforced {
			eprintln!("skip: perms not enforced (root)");
			return;
		}
		let refusals = p.refusals();
		assert!(
			refusals.iter().any(|(_, r)| matches!(
				r,
				RefuseReason::UnreadableCompatDir { .. }
			)),
			"an unreadable compat dir must refuse the whole plan, not \
			 silently pass it by: {:?}",
			p.actions
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
	/// A stale Referrer in a dir this agent only READS. Detach it — the write
	/// slot serves the same Master, so nothing is lost, and leaving it makes
	/// "remove for this agent alone" refuse forever. Symlink-only: execution
	/// re-checks and refuses a real directory, which may hold the only copy.
	Unlink,
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
	/// The compat-Referrer sweep could not tell whether `path` is safe to
	/// detach — `symlink_metadata` failed with something other than
	/// `NotFound` (most often a permission fault). A refusal, not a
	/// [`crate::skills::repair::RepairOutcome::Failed`]: the disk did not
	/// just glitch mid-write, the sweep never got far enough to plan a write
	/// at all, and the next run repeats the same answer until a human fixes
	/// the permission.
	UnreadableCompatDir { path: PathBuf },
	/// The real directory a moving action would rename away is TRACKED by the
	/// surrounding git repository, so it is in-place authored SOURCE, not a
	/// pre-2.18 install. Migrating it takes the skill out of version control:
	/// `git status` fills with deletions and the bytes now live only in an
	/// ignored store. Every tracked path in the plan is listed, so one
	/// `git rm -r --cached` round clears them all instead of the user
	/// discovering the next one on every re-run.
	/// See docs/history/core-skills-shape.md#repair-migrated-git-tracked-source-b6
	GitTrackedSource { paths: Vec<PathBuf> },
	/// A repository is right there (a `.git` above `path`) but `git` could not
	/// answer whether it tracks the directory — no `git` binary, an unusable
	/// gitfile, any non-`0`/`1` exit. An undecided probe refuses rather than
	/// guessing "untracked".
	GitTrackingUndecided { path: PathBuf },
}

/// One planned change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedReferrer {
	/// Every agent that resolves to this path. The shared `.agents/skills` slot
	/// is ONE directory read by every agent whose descriptor lists it, so it
	/// appears once with all of their ids — not once per agent. Callers
	/// disclose "granting to one grants to all of these" straight from this
	/// field.
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
	/// True when the skill exists nowhere at this scope: no Master and every
	/// slot `Absent`.
	pub fn finds_nothing(&self) -> bool {
		!self.master_exists
			&& self.actions.iter().all(|a| a.shape == SkillShape::Absent)
	}

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

/// Does the surrounding git repository TRACK this path — three states.
///
/// Only the two actions that RENAME a real directory away (`AdoptAsMaster`,
/// `CompareThenQuarantine`) ask. "Inside a repo" is NOT the question: an
/// untracked directory is exactly what `repair` exists to migrate.
///
/// Shells out to `git` on purpose (`aghub-core` must not grow a git dependency
/// for a yes/no question) and reads only the EXIT CODE: stdout would land in a
/// `repair --json` run and messages are localized.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GitTracked {
	Yes,
	No,
	Undecided,
}

/// Upper bound for one `git ls-files` probe. A healthy answer takes
/// milliseconds; a hung git (stalled network filesystem, a lock held by
/// something else) must not freeze a delete or repair, so past this it counts
/// as [`GitTracked::Undecided`].
/// See docs/history/core-removal.md#git-probe-had-no-deadline
const GIT_PROBE_TIMEOUT: std::time::Duration =
	std::time::Duration::from_secs(10);

/// Runs `cmd` to completion for at most `limit`. `Ok(None)` means the deadline
/// passed: the child was killed and reaped, so nothing is left running.
fn status_within(
	cmd: &mut std::process::Command,
	limit: std::time::Duration,
) -> std::io::Result<Option<std::process::ExitStatus>> {
	let mut child = cmd.spawn()?;
	let deadline = std::time::Instant::now() + limit;
	loop {
		if let Some(status) = child.try_wait()? {
			return Ok(Some(status));
		}
		if std::time::Instant::now() >= deadline {
			let _ = child.kill();
			let _ = child.wait();
			return Ok(None);
		}
		std::thread::sleep(std::time::Duration::from_millis(5));
	}
}

pub(crate) fn git_tracked(path: &Path) -> GitTracked {
	let Some(parent) = path.parent() else {
		return GitTracked::No;
	};
	// No repository above it: decided without running git, so a missing `git`
	// binary cannot refuse every repair. `exists()` follows the link, so a
	// linked worktree's `.git` FILE counts too.
	if !parent.ancestors().any(|dir| dir.join(".git").exists()) {
		return GitTracked::No;
	}
	// ponytail: one process per moving row (at most one adopt plus the forks),
	// re-asked per skill. Cache by repository if a bulk `repair` ever measures
	// slow.
	let mut cmd = std::process::Command::new("git");
	cmd.arg("-C")
		.arg(parent)
		.args(["ls-files", "--error-unmatch", "--"])
		.arg(path)
		.stdin(std::process::Stdio::null())
		.stdout(std::process::Stdio::null())
		.stderr(std::process::Stdio::null());
	// git hooks set GIT_DIR; inherited vars make git answer for another repository.
	for var in [
		"GIT_DIR",
		"GIT_WORK_TREE",
		"GIT_INDEX_FILE",
		"GIT_COMMON_DIR",
		"GIT_OBJECT_DIRECTORY",
		"GIT_NAMESPACE",
	] {
		cmd.env_remove(var);
	}
	#[cfg(windows)]
	{
		use std::os::windows::process::CommandExt;
		// CREATE_NO_WINDOW — same reason as `system_git`: no console flash per
		// skill when the desktop app runs a bulk repair.
		cmd.creation_flags(0x0800_0000);
	}
	match status_within(&mut cmd, GIT_PROBE_TIMEOUT) {
		Ok(Some(s)) if s.success() => GitTracked::Yes,
		// Exactly 1 is "no pathspec matched": the definite negative.
		Ok(Some(s)) if s.code() == Some(1) => GitTracked::No,
		// 128 (not a repository, an unusable gitfile, a path outside the repo),
		// a signal, a timeout, or no `git` binary at all. We know a repository
		// is there and cannot say — see `RefuseReason::GitTrackingUndecided`.
		Ok(_) | Err(_) => GitTracked::Undecided,
	}
}

#[cfg(all(test, unix))]
mod git_env_tests {
	use super::{git_tracked, status_within, GitTracked};

	#[test]
	fn status_within_kills_a_child_that_outlives_the_deadline() {
		let started = std::time::Instant::now();
		let mut cmd = std::process::Command::new("sleep");
		cmd.arg("30");
		let outcome =
			status_within(&mut cmd, std::time::Duration::from_millis(200))
				.unwrap();
		assert!(outcome.is_none(), "a hung child must report the timeout");
		assert!(
			started.elapsed() < std::time::Duration::from_secs(10),
			"the probe must return at the deadline, not when the child exits"
		);

		let mut quick = std::process::Command::new("true");
		let done =
			status_within(&mut quick, std::time::Duration::from_secs(10))
				.unwrap();
		assert!(done.is_some_and(|s| s.success()));
	}

	const GIT_VARS: &[&str] = &[
		"GIT_DIR",
		"GIT_WORK_TREE",
		"GIT_INDEX_FILE",
		"GIT_COMMON_DIR",
		"GIT_OBJECT_DIRECTORY",
		"GIT_NAMESPACE",
	];

	struct RestoreEnv(Vec<(&'static str, Option<std::ffi::OsString>)>);

	impl RestoreEnv {
		fn clear() -> Self {
			let saved = GIT_VARS
				.iter()
				.map(|&key| (key, std::env::var_os(key)))
				.collect();
			for key in GIT_VARS {
				std::env::remove_var(key);
			}
			Self(saved)
		}
	}

	impl Drop for RestoreEnv {
		fn drop(&mut self) {
			for (key, value) in &self.0 {
				match value {
					Some(value) => std::env::set_var(key, value),
					None => std::env::remove_var(key),
				}
			}
		}
	}

	#[test]
	fn git_tracked_ignores_inherited_repository_environment() {
		use crate::skills::removal::tests::git_fixture;

		if !git_fixture::has_git() {
			eprintln!("skipping test: git binary unavailable");
			return;
		}
		let _env = crate::skills::prune::test_lock::env_lock()
			.lock()
			.unwrap_or_else(|error| error.into_inner());
		let _restore = RestoreEnv::clear();
		let repo = tempfile::tempdir().unwrap();
		let other = tempfile::tempdir().unwrap();
		let tracked = repo.path().join("d");
		std::fs::create_dir_all(&tracked).unwrap();
		std::fs::write(tracked.join("SKILL.md"), "---\nname: d\n---\n")
			.unwrap();
		git_fixture::git(repo.path(), &["init", "-q"]);
		git_fixture::git(repo.path(), &["add", "--", "d/SKILL.md"]);
		git_fixture::git(other.path(), &["init", "-q"]);

		std::env::set_var("GIT_DIR", other.path().join(".git"));
		assert_eq!(
			git_tracked(&tracked),
			GitTracked::Yes,
			"GIT_DIR must not make git answer for the unrelated repository"
		);

		std::env::remove_var("GIT_DIR");
		std::env::set_var("GIT_WORK_TREE", other.path());
		assert_eq!(
			git_tracked(&tracked),
			GitTracked::Yes,
			"GIT_WORK_TREE must not replace the repository selected by -C"
		);
	}
}

/// Whether a stale compat Referrer at `entry` may be detached — the ONE
/// fallible, identity-based test behind the compat-Referrer sweep.
/// `plan_repair` calls it to decide the row; `execute_repair` step 6 calls it
/// AGAIN immediately before `Linker::unlink`, because the disk it was decided
/// against can move in between (npx rewrites these very directories, and a
/// long preview gives a user plenty of time to retarget a link by hand).
///
/// Fails CLOSED: any I/O error other than `NotFound` is returned, never folded
/// into `false`. See docs/history/core-skills-shape.md#compat-probe-folded-permission-errors
///
/// Compares by [`entry_identity`], never `==` on `PathBuf`s: a compat dir
/// reached through a symlinked ancestor is a different string from the slot it
/// aliases. It does NOT canonicalize `entry`'s own leaf ([`same_object`] does
/// that below), or every correctly-linked Referrer would look like a write
/// slot.
/// See docs/history/core-skills-shape.md#compat-sweep-unlinked-the-physical-shared-slot
///
/// `Unlink` rows in `planned` are filtered out before comparing: they are this
/// sweep's own conclusions, and matching them would make every compat entry
/// match itself and never detach.
pub(crate) fn compat_unlink_permitted(
	entry: &Path,
	master: &Path,
	adopt_source: Option<&Path>,
	planned: &[PlannedReferrer],
) -> std::io::Result<bool> {
	// NOBODY'S WRITE SLOT: "is this path the plan's own output?" It is the only
	// thing stopping step 6 from deleting what steps 4-5 of the same run wrote
	// (e.g. amp's project `.agents/skills/<name>`, where every other reader is
	// covered, so [`compat_unlink_authorized`] passes). The two guards protect
	// disjoint populations and are ANDed, never substituted: a writer never
	// records itself as a reader of its own slot, so the reader quorum cannot
	// see this case, and this guard cannot see an uncovered co-reader of a dir
	// nobody writes. See docs/history/core-skills-shape.md#compat-sweep-asked-only-the-first-reader
	let entry_id = entry_identity(entry);
	if planned
		.iter()
		.filter(|row| row.action != ReferrerAction::Unlink)
		.any(|row| entry_identity(&row.path) == entry_id)
	{
		return Ok(false);
	}

	// LINK ONLY. A real directory may hold the only copy of bytes aghub never
	// installed (`CompareThenQuarantine`'s job, never this). Only `NotFound`
	// may read `Ok(false)`; every other error fails CLOSED via
	// `Linker::is_link_checked` — the ONE caller that must not take the lossy
	// `Linker::is_link`, and must not inline its own reparse-point test either.
	if !Linker::is_link_checked(entry)? {
		return Ok(false);
	}

	// RESOLVES TO THIS MASTER, or to the directory this run ADOPTS as one —
	// that half keeps a migration single-pass (the store does not exist yet;
	// the adopted dir becomes a link to the Master in step 5, before step 6).
	// `same_object` never equates two unresolvable paths, so a link pointing
	// anywhere else is somebody else's (D5: report, never move).
	let serves_this_master = same_object(entry, master)
		|| adopt_source.is_some_and(|src| same_object(entry, src));
	Ok(serves_this_master)
}

/// One agent's skills directories for a scope, snapshotted once before the
/// compat sweep runs so [`compat_unlink_authorized`] can quantify over every
/// READER of an entry.
pub(crate) struct AgentDirs {
	pub(crate) id: &'static str,
	/// `None` when this scope gives the agent no write slot at all. Such an
	/// agent can never be covered — nothing at this scope can serve it — so a
	/// compat entry it reads is its only way in and it vetoes the detach.
	pub(crate) write: Option<PathBuf>,
	pub(crate) read: Vec<PathBuf>,
}

/// GUARD 4: will every agent that reads `entry` still be served by its OWN
/// write slot once the plan has run?
///
/// Pure on purpose (roster and coverage arrive as data): the real roster has
/// no dir read by two agents and written by none, so this interface is the
/// only test surface the rule has.
///
/// Universal, never existential: one reader's coverage is not consent for the
/// rest. Readers are matched by [`entry_identity`], so a co-reader spelling the
/// dir through a symlinked ancestor still counts.
/// See docs/history/core-skills-shape.md#compat-sweep-asked-only-the-first-reader
///
/// An entry nobody was observed reading is NOT detachable (never the vacuous
/// `all()` over an empty set).
///
/// Deliberate ceiling: "the agent's OWN slot serves it" is stronger than "it
/// still reads it from somewhere"; the looser form needs a fixpoint for a
/// layout nobody has, and refusing leaves only a stale link (the safe side).
pub(crate) fn compat_unlink_authorized(
	entry: &Path,
	leaf: &str,
	roster: &[AgentDirs],
	covered: &std::collections::HashSet<&'static str>,
) -> bool {
	let id = entry_identity(entry);
	let mut observed = false;
	for agent in roster.iter().filter(|agent| {
		agent
			.read
			.iter()
			.any(|dir| entry_identity(&dir.join(leaf)) == id)
	}) {
		observed = true;
		// Checked here, not trusted to `covered`: no write slot means nothing to
		// fall back on, a property of the agent, not of how the set was built.
		if agent.write.is_none() || !covered.contains(agent.id) {
			return false;
		}
	}
	observed
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
	let safe = skill::sanitize_name(name);
	let shared_slot = shared_referrer_dir(store_root(scope, project_root))
		.map(|d| d.join(&safe));

	// Collapse the candidates by PATH: the shared slot is one directory that
	// every agent whose descriptor lists it resolves to. Compare the
	// constructed paths, never resolved ones — an Absent candidate does not
	// canonicalize, so resolving would fold every ungranted agent into one
	// bucket.
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

	// TWO passes, load-bearing: whether a Referrer is owed depends on whether a
	// Master will EXIST, and during a migration it is only about to be adopted
	// out of the shared slot. See docs/history/core-skills-shape.md#migration-created-no-per-agent-referrers
	let shaped: Vec<(PathBuf, bool, SkillShape, Vec<&'static str>)> = order
		.into_iter()
		.map(|path| {
			let shared = shared_slot.as_deref() == Some(path.as_path());
			let shape = classify_shape(&path, &master);
			let agents = by_path.remove(&path).unwrap_or_default();
			(path, shared, shape, agents)
		})
		.collect();

	// A slot only disabled agents use is not aghub's to touch
	// (`crate::agent_settings`): it plans `Leave` whatever its shape. Decided
	// before adoption so an unmanaged shared slot is never adopted either; the
	// compat sweep below keeps their links too (a disabled agent is never
	// `covered`, so guard 4 vetoes).
	let disabled = crate::agent_settings::disabled_agents();
	let managed =
		|agents: &[&str]| agents.iter().any(|a| !disabled.contains(*a));

	// Exactly one adoption, and only from the shared slot — never by registry
	// order. See docs/history/core-skills-shape.md#private-copy-won-adoption-by-registry-order
	let adopting = shaped.iter().any(|(_, shared, shape, agents)| {
		*shared
			&& in_lock
			&& *shape == SkillShape::UnmigratedCopy
			&& managed(agents)
	});
	// "Is there something to point at by the time Referrers are written?"
	let will_have_master = master_exists || adopting;

	let mut planned: Vec<PlannedReferrer> = shaped
		.into_iter()
		.map(|(path, shared, shape, agents)| {
			let action = if managed(&agents) {
				action_for(
					&shape,
					shared,
					in_lock,
					will_have_master,
					&agents,
					grant_to,
				)
			} else {
				ReferrerAction::Leave
			};
			PlannedReferrer {
				agents,
				path,
				shape,
				action,
				shared,
			}
		})
		.collect();

	// Nothing to point at and nothing to adopt: writing Referrers now would
	// create links to nothing. Refuse the whole plan rather than half-build it.
	if !will_have_master {
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

	// A real directory git TRACKS is authored in place, not installed; both
	// moving actions would rename it into the ignored store and take it out of
	// version control. Neither shape nor lock can tell source from a pre-2.18
	// install, so tracking is the only signal. Deliberately NOT "is it inside a
	// repo" (that refuses repair's main job), and scope-blind (dotfiles repos).
	// All tracked paths go on ONE refusal, since `execute_repair` reports only
	// the first. See docs/history/core-skills-shape.md#repair-migrated-git-tracked-source-b6
	let mut tracked: Vec<PathBuf> = Vec::new();
	let mut undecided: Vec<PathBuf> = Vec::new();
	for entry in &planned {
		if !matches!(
			entry.action,
			ReferrerAction::AdoptAsMaster
				| ReferrerAction::CompareThenQuarantine
		) {
			continue;
		}
		match git_tracked(&entry.path) {
			GitTracked::No => {}
			GitTracked::Yes => tracked.push(entry.path.clone()),
			GitTracked::Undecided => undecided.push(entry.path.clone()),
		}
	}
	for entry in &mut planned {
		if tracked.contains(&entry.path) {
			entry.action = ReferrerAction::Refuse {
				reason: RefuseReason::GitTrackedSource {
					paths: tracked.clone(),
				},
			};
		} else if undecided.contains(&entry.path) {
			entry.action = ReferrerAction::Refuse {
				reason: RefuseReason::GitTrackingUndecided {
					path: entry.path.clone(),
				},
			};
		}
	}

	// Stale Referrers in dirs this agent only READS. `candidate_referrers` is
	// write-dir derived, so a link an older release left in a compat dir is
	// invisible elsewhere — and the agent keeps reading from it, so "remove for
	// this agent alone" refuses forever.
	// See docs/history/core-skills-shape.md#antigravity-write-slot-moved-and-left-a-compat-link
	//
	// FOUR guards decide a detach. Three — link-only, resolves to this Master
	// (or the adopt source), nobody's write slot — live in
	// [`compat_unlink_permitted`], which `execute_repair` step 6 re-runs right
	// before unlinking. The fourth, THE WRITE SLOT COVERS IT AFTERWARDS, is
	// decided here because it reads the PLAN, not the disk (a slot about to be
	// Created/Relinked covers the agent, so no second run is needed); a
	// `RepairPlan` carries no scope, so execution cannot re-ask it. It gates
	// only the destructive half, inside [`compat_unlink_authorized`], and is
	// asked of EVERY reader of the entry — otherwise the cleanup revokes the
	// skill for an agent whose only Referrer was the compat one.
	let adopt_source = planned
		.iter()
		.find(|row| row.action == ReferrerAction::AdoptAsMaster)
		.map(|row| row.path.clone());
	// Snapshot every agent's dirs BEFORE sweeping, so guard 4 sees each
	// entry's whole reader set.
	let roster: Vec<AgentDirs> = crate::registry::ALL_AGENTS
		.iter()
		.map(|descriptor| AgentDirs {
			id: descriptor.id,
			write: skill_write_dir(descriptor, scope, project_root),
			read: skill_read_dirs(descriptor, scope, project_root),
		})
		.collect();
	// An agent with no write slot at this scope is never covered, so it always
	// vetoes. So does a disabled agent: its slot may be healthy, but detaching
	// the compat link it reads would still be a write into its dirs.
	let covered: std::collections::HashSet<&'static str> = roster
		.iter()
		.filter(|agent| !disabled.contains(agent.id))
		.filter(|agent| {
			let Some(slot) = agent.write.as_ref().map(|dir| dir.join(&safe))
			else {
				return false;
			};
			planned.iter().any(|row| {
				row.path == slot
					&& match row.action {
						ReferrerAction::Create | ReferrerAction::Relink => true,
						// Step 5 swaps the adopted directory for a link to the
						// Master, so its slot's agents are covered.
						// See docs/history/core-skills-shape.md#adopt-source-coverage-cost-a-second-run
						ReferrerAction::AdoptAsMaster => true,
						ReferrerAction::Leave => {
							row.shape == SkillShape::Conformant
						}
						_ => false,
					}
			})
		})
		.map(|agent| agent.id)
		.collect();

	let mut unlink: Vec<PlannedReferrer> = Vec::new();
	for agent in &roster {
		let Some(write_dir) = agent.write.as_ref() else {
			continue;
		};
		// NOT an early `continue` on coverage: the fallible probe must run even
		// for an uncovered agent, as it is the only thing that notices an
		// unreadable compat dir. See docs/history/core-skills-shape.md#compat-sweep-skipped-the-unreadable-probe
		for read_dir in &agent.read {
			// Identity, not spelling — see the guards note above.
			if entry_identity(read_dir) == entry_identity(write_dir) {
				continue;
			}
			let entry = read_dir.join(&safe);
			match compat_unlink_permitted(
				&entry,
				&master,
				adopt_source.as_deref(),
				&planned,
			) {
				// Permitted, but detaching is authorized only when EVERY agent
				// reading this entry keeps a way in afterwards — not merely the
				// one whose iteration happened to reach it first.
				Ok(true)
					if compat_unlink_authorized(
						&entry, &safe, &roster, &covered,
					) => {}
				Ok(true) | Ok(false) => continue,
				// Fail CLOSED: an unreadable compat dir is a decision for a
				// human, and it blocks the WHOLE plan like every refusal.
				Err(_) => {
					unlink.push(PlannedReferrer {
						agents: vec![agent.id],
						shape: classify_shape(&entry, &master),
						path: entry.clone(),
						action: ReferrerAction::Refuse {
							reason: RefuseReason::UnreadableCompatDir {
								path: entry,
							},
						},
						shared: false,
					});
					continue;
				}
			}
			// Collapse by IDENTITY: one compat dir several agents read (however
			// spelled) is one row naming all of them.
			if let Some(row) = unlink
				.iter_mut()
				.find(|r| entry_identity(&r.path) == entry_identity(&entry))
			{
				if !row.agents.contains(&agent.id) {
					row.agents.push(agent.id);
				}
				continue;
			}
			unlink.push(PlannedReferrer {
				agents: vec![agent.id],
				shape: classify_shape(&entry, &master),
				path: entry,
				action: ReferrerAction::Unlink,
				shared: false,
			});
		}
	}
	planned.extend(unlink);

	// Execution order: adopt the Master, then private Referrers, then the shared
	// slot, then the compat-dir detaches — so a crash at any point leaves the
	// skill still served (until the write slot is on disk, the compat link is
	// the agent's only way in).
	planned.sort_by_key(|p| match p.action {
		ReferrerAction::AdoptAsMaster => 0,
		ReferrerAction::Unlink => 3,
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
		// Report only, never touch (D5). The slot is occupied by content aghub
		// did not install, so this agent simply does not get a Referrer — the
		// honest answer, and the one that stops `repair` demanding a decision
		// nobody can make correctly.
		SkillShape::ForeignDir => ReferrerAction::LeaveForeign,
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
/// **Shares [`plan_repair`]'s observation, NOT its policy**: it reuses the
/// collapsed candidate set but ignores the action column. Repair refuses what
/// it cannot FIX; removal refuses only what it cannot UNDO (unlinking a
/// dangling link destroys nothing).
/// See docs/history/core-skills-shape.md#verify-shape-reused-repair-refusals
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
/// - A forked copy in an agent's PRIVATE directory stays legal: removing a
///   private copy that shadows a Master is specified behaviour (the Master is
///   disclosed in `skipped`; see `crates/core/AGENTS.md` "Did that removal take
///   anything away?"). A guard must not quietly relitigate a spec decision.
/// - Link shapes (`Dangling`, `ForeignTarget`, `Chain`) and the master-side
///   violations are repair problems, not delete hazards.
///
/// `in_lock: false` cannot change the verdict (it only picks `AdoptAsMaster`
/// vs `LeaveForeign`, which this ignores); a lock read here would fail open.
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
	// Exhaustive on the blocking shapes ON PURPOSE, so a wrong detail is
	// unreachable. See docs/history/core-skills-shape.md#verify-shape-printed-two-shapes-in-one-detail
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
	// Not `ConfigError::unsupported_operation`: its template would name the
	// target agent, but the blocker is the SHARED slot. The wire contract pins
	// the CODE (`UNSUPPORTED_OPERATION` / HTTP 422), not the prose.
	let writers = if blocker.agents.is_empty() {
		String::new()
	} else {
		// `PlannedReferrer::agents` are WRITERS; more agents read a shared
		// slot, so never phrase this as "read by".
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
			SkillShape::ForeignDir => "a directory that is not a skill",
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
