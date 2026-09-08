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
/// This is migration's `grant_to`: D6 says an agent that reads the skill today
/// is owed an explicit Referrer once the Master moves, and "today" means
/// against the layout as it currently stands — so this must be called BEFORE
/// anything is moved. Read paths, not write dirs: an agent can read the shared
/// slot while writing to its own directory, and it is exactly those agents that
/// would silently lose the skill if migration only linked the writers.
///
/// A path counts when the skill is actually present there, so an agent that
/// merely COULD read the slot does not get a grant for a skill nobody put in it.
///
/// "Present" means [`has_skill_marker`] returning [`SkillMarker::Present`] — a
/// root `SKILL.md`, not just SOME entry at the path. A same-named category
/// directory a name collision happens to share (`SkillShape::ForeignDir`'s
/// whole reason for existing) used to count on bare existence, which put
/// every agent sharing that compat dir into `grant_to` and turned an absent
/// shared row into `Create` — a managed skill silently granted to a reader
/// that was never actually reading it.
///
/// [`SkillMarker::Unknown`] does not count either, and that is a SEPARATE,
/// later fix: an unreadable same-named directory (`chmod 000`) is not
/// evidence of a read, so it must not seed `grant_to` and, downstream, an
/// implicit `Create` in the shared slot — see [`SkillMarker`]'s doc for why
/// this caller's safe direction is the opposite of `classify_shape`'s.
///
/// A DANGLING link still counts, even though its `SKILL.md` probe cannot —
/// `has_skill_marker` asks about `<entry>/SKILL.md`, which fails `NotFound`
/// the instant the link's own target is gone, folding a stranded Referrer
/// into the same `Absent` bucket as an agent that was never granted the
/// skill at all. That is the regression this OR-clause exists to undo: a
/// migration or a hand-deleted shared slot can leave exactly this shape
/// behind (`.agent/skills/<name>` still pointing at a target that no longer
/// resolves), and the agent WAS reading through it right up until the
/// target vanished — repair is the only way back for it
/// (`crates/agents/src/agents/antigravity.rs`'s descriptor comment documents
/// this exact family of stranded skills). `Linker::is_link` asks about the
/// entry itself, not what it resolves to, so it stays `true` for a broken
/// link while staying
/// `false` for every real-directory shape this function must keep
/// excluding — a same-named regular file, a category dir with no root
/// `SKILL.md`, and (the one that matters) an unreadable directory: its own
/// `symlink_metadata` still succeeds (permission bits on an entry don't
/// gate `lstat`-ing it from its parent), but its file type is a directory,
/// never a symlink, so this OR-clause cannot make it count. `Linker::is_link`
/// is deliberately the LOSSY form (folds any I/O error to `false`) rather
/// than `is_link_checked`: an unreadable PARENT then reads as `false`, i.e.
/// "not a reader" — the same safe direction this function already takes for
/// `Unknown` — where `is_link_checked`'s fail-CLOSED direction is what
/// `compat_unlink_permitted` needs for the opposite reason (a refusal, not a
/// silent non-grant). Do not swap it for `is_link_checked` here.
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
						|| Linker::is_link(&entry)
				})
		})
		.map(|descriptor| descriptor.id)
		.collect()
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
/// Deliberately observation only, with no policy in it. An earlier version had a
/// `Legacy` variant, but the spec defines legacy as "the LOCK names it, a real
/// directory serves it, and no Master exists" — and this function cannot see the
/// lock. It classified a user's hand-placed `.cursor/skills/<n>`, and even a
/// regular file, as legacy and offered it up for adoption as the Master, which
/// D5 forbids outright. Whether an [`Self::UnmigratedCopy`] may be adopted is
/// [`plan_repair`]'s call, because that is where the lock and the slot identity
/// are known.
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
	/// strictly alone. Quarantining it would move somebody's entire skill
	/// collection aside — and before this existed, `repair` did exactly that in
	/// its `fix:` line, telling the user to "keep the one you want".
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
	// A directory with no root `SKILL.md` is not a skill, so it cannot be a
	// COPY of this one — it only shares the name. This is [`has_skill_marker`]
	// (see its doc for the three-state ABSENT/PRESENT/UNKNOWN rule — and for
	// why `prune::top_level_skill_dirs` asks the same question with a THIRD,
	// differently-folded spelling rather than this one), and `metadata`
	// follows links, so a link to a real skill still passes.
	//
	// Asked BEFORE the fork/unmigrated split because both of those lead
	// somewhere destructive: a fork gets hashed and quarantined, and an
	// unmigrated copy in the shared slot gets ADOPTED as the Master. Neither is
	// a thing to do to a directory that belongs to another tool.
	//
	// `Unknown` is treated as `Present` here — the OPPOSITE of `readers_of`'s
	// direction, and deliberately so (see [`SkillMarker`]'s doc). Staying loud
	// on an unreadable directory routes it into `ForkedCopy` ->
	// `CompareThenQuarantine` -> `compare`'s `Undecidable` arm -> a `Refused`
	// outcome a human sees, instead of downgrading it to `ForeignDir` ->
	// `LeaveForeign`, which silently passes an unreadable directory by as if
	// it belonged to somebody else.
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
/// `classify_shape` treats [`Self::Unknown`] as [`Self::Present`]: staying
/// loud on an unreadable directory routes it into `ForkedCopy` ->
/// `CompareThenQuarantine` -> a refusal a human sees, rather than
/// downgrading it to `ForeignDir`, which is left silently alone.
/// `readers_of` treats [`Self::Unknown`] as NOT [`Self::Present`] — the
/// mirror image, and for the same reason bools cannot serve both: a probe
/// that could not be answered is not evidence "this agent already reads the
/// skill", and counting it as one seeds `grant_to` with a reader that was
/// never established, which downstream turns an absent shared row into an
/// implicit `Create` — a managed skill silently granted to an agent nobody
/// asked to have it.
///
/// A single bool folded both directions into ONE answer, which was the
/// defect: `chmod 000` on a same-named collision directory made the probe
/// unreadable, the bool said "yes, a marker" (fail-open, correct for
/// `classify_shape`), and `readers_of` read that same "yes" as "this agent
/// reads the skill" and silently linked it into the shared `.agents/skills`
/// slot — reachable by `an_unreadable_same_named_dir_never_seeds_an_implicit_create`
/// below, and originally found by running `repair` against exactly this
/// state on the CLI.
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
/// NOT the only spelling in the crate: `prune::top_level_skill_dirs` asks the
/// same "is this a skill dir" question with `entry.path().join("SKILL.md")
/// .is_file()` — a bare bool that folds an I/O error (most often EACCES) into
/// `false` the same way it folds a genuine absence, i.e. the OPPOSITE
/// direction from this function's `Unknown` (which `classify_shape` treats as
/// `Present`, staying loud rather than silently calling an unreadable
/// directory "not a skill"). That third spelling drops an unreadable skill
/// dir's entry from the list `prune` prunes lock keys against — left alone
/// this round; do not assume unifying it onto `has_skill_marker` is a safe,
/// mechanical change.
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

	/// Codex 5.6 blocker 1 (DO-NOT-SHIP review): the compat-dir sweep used to
	/// compare raw `PathBuf` spellings. `.agent/skills` (antigravity's
	/// read-only compat dir) aliased AT THE DIRECTORY LEVEL onto
	/// `.agents/skills` (its own write slot, and up to eight other agents'
	/// only slot) is the layout `stow`, or a user hand-fixing their setup,
	/// actually produces — not a hypothetical. `.agent/skills/demo` then
	/// lstats as a symlink resolving to the Master exactly like the write
	/// slot's own entry, but the literal `PathBuf` differs, so the old guard
	/// missed it and scheduled the PHYSICAL shared slot for `Unlink` —
	/// deleting the referrer every other agent reads through.
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
		let aliased_entry = root.join(".agent").join("skills").join("foo");
		assert!(
			!p.actions.iter().any(|a| a.path == aliased_entry
				&& a.action == ReferrerAction::Unlink),
			"an entry reached only through a symlinked ancestor must never be \
			 scheduled to unlink the directory it aliases: {:?}",
			p.actions
		);
	}

	/// Codex 5.6 blocker 3: a REGRESSION introduced by the `ForeignDir`
	/// change. Before it, a same-named category directory classified as
	/// `ForkedCopy` and the whole plan refused loudly. After it, `readers_of`
	/// still counted the category dir as "reads this skill" on bare
	/// existence — no root `SKILL.md` required — so an agent that has never
	/// actually read the skill landed in `grant_to`, and the absent shared
	/// row turned into `Create`: a managed skill silently granted through a
	/// name collision.
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

	/// Round-2 blocker: the same fixture as the test above, but the category
	/// dir is made UNREADABLE first — reproduced against the CLI with
	/// `chmod 000` on exactly this directory before `repair research -p`
	/// silently planned `link: .agents/skills/research`. A shared
	/// `has_skill_marker` bool answered "yes, a marker" for BOTH callers
	/// (correct fail-open for `classify_shape`, wrong for this one), so
	/// `readers_of` counted an unreadable foreign directory as a read of the
	/// managed skill. Pins the `SkillMarker::Unknown` direction THROUGH
	/// `readers_of` specifically — `an_unreadable_directory_is_not_mistaken_for_a_foreign_one`
	/// above only pins `classify_shape`'s (opposite) direction.
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
		let compat = root.join(".agent").join("skills");
		fs::create_dir_all(&compat).unwrap();
		unix_fs::symlink(root.join("nonexistent-target"), compat.join(name))
			.unwrap();

		let readers = readers_of(ResourceScope::ProjectOnly, Some(&root), name);
		assert!(
			readers.contains(&"antigravity"),
			"a dangling compat referrer is still evidence this agent was \
			 granted the skill, got {readers:?}"
		);

		let write_slot = root.join(".agents").join("skills").join(name);
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

	/// The mirror of the test above, pinning that the round-2 blocker stays
	/// closed: an unreadable real DIRECTORY sitting in a compat read dir must
	/// still not count as a reader. The two pre-existing tests above already
	/// pin the end-to-end outcome for the category-dir fixture
	/// (`readers_of_treats_an_unreadable_same_named_dir_as_not_a_reader`,
	/// `an_unreadable_same_named_dir_never_seeds_an_implicit_create`); this
	/// one is the load-bearing check specifically for the OR-clause just
	/// added — swapping `Linker::is_link(&entry)` for the naive
	/// `entry.symlink_metadata().is_ok()` (the pre-round-2 bare-existence
	/// rule this whole change must not resurrect) makes THIS test fail
	/// exactly as it makes those two fail: `symlink_metadata` on a directory
	/// entry succeeds regardless of the directory's OWN permission bits
	/// (those gate reading what is inside it, not `lstat`-ing the entry from
	/// its parent), so the naive rule reads an unreadable directory as
	/// "present" every bit as much as a dangling link is.
	#[test]
	fn readers_of_still_excludes_an_unreadable_compat_directory() {
		use std::os::unix::fs::PermissionsExt;
		let (_tmp, root) = project_fixture();
		let name = "demo";
		write_skill(&root.join(".aghub").join(name), "---\nname: demo\n---\n");

		// antigravity's read-only compat dir, but this time occupied by a
		// real, unreadable directory rather than a dangling link.
		let compat = root.join(".agent").join("skills").join(name);
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
			!readers.contains(&"antigravity"),
			"an unreadable real directory must not count as a reader, got \
			 {readers:?}"
		);
	}

	/// Codex 5.6 should-fix 4 / the design decision behind finding 4: an
	/// unreadable compat parent must REFUSE the plan, never silently pass the
	/// sweep by. `symlink_metadata` on an entry under an unreadable directory
	/// fails with something other than `NotFound`, and the old sweep folded
	/// that into "nothing here" exactly like `Linker::is_link` does — so the
	/// skill read as fully conformant while a stale link sat right there.
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
		let compat_dir = root.join(".agent").join("skills");
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
	/// the permission — exactly what a refusal promises and `Failed` does not.
	UnreadableCompatDir { path: PathBuf },
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

/// Whether a stale compat Referrer at `entry` may be detached — the ONE
/// fallible, identity-based test behind the compat-Referrer sweep.
/// `plan_repair` calls it to decide the row; `execute_repair` step 6 calls it
/// AGAIN immediately before `Linker::unlink`, because the disk it was decided
/// against can move in between (npx rewrites these very directories, and a
/// long preview gives a user plenty of time to retarget a link by hand).
///
/// Fails CLOSED: any I/O error other than `NotFound` while probing `entry` is
/// returned rather than folded into `false` the way `Linker::is_link` does —
/// that folding is exactly what let a `PermissionDenied` probe read as
/// "nothing here", so a row that was never actually removed still reported
/// itself removed.
///
/// Compares by [`entry_identity`], never by `==` on the constructed
/// `PathBuf`s: a compat dir reached through a symlinked ANCESTOR
/// (`.agent/skills` -> `.agents/skills`, which stow or a hand-fixed layout
/// both produce) is a different STRING from the write slot it aliases, and
/// `==` cannot see they are the same directory. Deliberately does NOT
/// canonicalize `entry`'s own leaf — that is [`same_object`]'s job below, and
/// doing it here would fold every Referrer into its Master and make an
/// ordinary, correctly-linked Referrer look like a write slot.
///
/// `planned` is the write-dir-derived candidate set — `Unlink` rows (this
/// sweep's own conclusions, whether from an earlier iteration at plan time or
/// the full action list at execute time) are filtered OUT before comparing:
/// they are not a write slot in their own right, and comparing against them
/// would make every compat entry match itself and never detach anything.
pub(crate) fn compat_unlink_permitted(
	entry: &Path,
	master: &Path,
	adopt_source: Option<&Path>,
	planned: &[PlannedReferrer],
) -> std::io::Result<bool> {
	// NOBODY'S WRITE SLOT. `.agents/skills` is codex's second read dir and
	// eight other agents' only write dir; detaching it revokes the skill for
	// all eight — this is the guard that check was missing.
	//
	// This asks "is this a WRITE slot", but what it must actually protect is
	// every agent still READING this dir — the two coincide for every shared
	// dir in today's roster only by accident, not by design: a shared dir
	// with no writer at all would pass this guard and still get unlinked out
	// from under its read-only co-readers. Root `AGENTS.md` "Adding /
	// Removing an Agent" records the constraint a new roster entry (or a
	// scope change) must not break; read it before adding a read-only-only
	// shared dir. NOT fixed this round — deliberately deferred, not missed.
	let entry_id = entry_identity(entry);
	if planned
		.iter()
		.filter(|row| row.action != ReferrerAction::Unlink)
		.any(|row| entry_identity(&row.path) == entry_id)
	{
		return Ok(false);
	}

	// LINK ONLY. A real directory here may hold the only copy of bytes aghub
	// never installed (`CompareThenQuarantine` is the verb for those, never
	// this). `NotFound` means nothing to detach at all — both read `Ok(false)`
	// below, but only `NotFound` may; every other error fails CLOSED via
	// `Linker::is_link_checked`, the fallible probe `Linker::is_link` itself
	// wraps and folds to a bare `false` — this is the ONE caller that must NOT
	// take that lossy wrapper (root `AGENTS.md`: never hand-mirror a flow and
	// keep it in sync by hand; a second inline copy of the Windows
	// reparse-point test is exactly that).
	if !Linker::is_link_checked(entry)? {
		return Ok(false);
	}

	// RESOLVES TO THIS MASTER, or to the directory this run is about to ADOPT
	// as one. The second half is not a loosening, it is what makes the sweep
	// single-pass: during a MIGRATION the store does not exist yet, so
	// `same_object` against `master` alone is false for everything and the
	// detach would wait for a second `repair` run — leaving the agent reading
	// the skill from two places, and its toggle refusing, after a run that
	// reported `migrated`. The adopted directory becomes a link to the Master
	// in step 5, so an entry resolving to it resolves to the Master by the
	// time step 6 detaches anything. `same_object` folds two unresolvable
	// paths to `false`, never to equal, so a link pointing anywhere ELSE is
	// somebody else's — D5 says report, never move.
	let serves_this_master = same_object(entry, master)
		|| adopt_source.is_some_and(|src| same_object(entry, src));
	Ok(serves_this_master)
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

	// TWO passes, and the split is load-bearing. Shapes first, because whether
	// a Referrer is owed depends on whether a Master will EXIST — and during a
	// migration it does not exist yet, it is about to be adopted out of the
	// shared slot. Deciding actions in the same pass that observes the shapes
	// gated `Create` on `master_exists` alone, so a migration created no
	// per-agent Referrer at all: the Master moved into the store and every
	// agent that used to read the slot silently kept reading it through the one
	// shared link. That is D6 ("relink every agent that can read the skill
	// today") failing closed, and it is the entire point of moving the store.
	let shaped: Vec<(PathBuf, bool, SkillShape, Vec<&'static str>)> = order
		.into_iter()
		.map(|path| {
			let shared = shared_slot.as_deref() == Some(path.as_path());
			let shape = classify_shape(&path, &master);
			let agents = by_path.remove(&path).unwrap_or_default();
			(path, shared, shape, agents)
		})
		.collect();

	// Exactly one adoption, and only from the shared slot. Registry order must
	// never decide this: it once let an agent's PRIVATE copy win over the shared
	// slot and become the Master, while the real Master-to-be was planned for
	// Relink — the one action that destroys a directory.
	let adopting = shaped.iter().any(|(_, shared, shape, _)| {
		*shared && in_lock && *shape == SkillShape::UnmigratedCopy
	});
	// "Is there something to point at by the time Referrers are written?"
	let will_have_master = master_exists || adopting;

	let mut planned: Vec<PlannedReferrer> = shaped
		.into_iter()
		.map(|(path, shared, shape, agents)| {
			let action = action_for(
				&shape,
				shared,
				in_lock,
				will_have_master,
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

	// Stale Referrers in dirs this agent only READS.
	//
	// `candidate_referrers` is WRITE-dir derived by design, so a link an older
	// release left in a compat dir is invisible to every other part of this
	// plan — and it is not harmless: the agent goes on reading the skill from
	// it, so "remove for this agent alone" can never take anything away and
	// refuses forever. Observed on antigravity, whose global write slot moved to
	// `.gemini/config/skills` while `.gemini/antigravity/skills` kept the link.
	//
	// FOUR guards decide whether a compat entry may be detached, and three of
	// them — link-only, resolves to this Master (or the adopt source), and
	// "is nobody's write slot" — live in [`compat_unlink_permitted`], the ONE
	// fallible test `execute_repair` step 6 re-runs immediately before it
	// unlinks anything. Compared by [`entry_identity`], never by `==` on the
	// constructed `PathBuf`s: a compat dir reached through a symlinked
	// ANCESTOR (`.agent/skills` -> `.agents/skills`, which stow or a
	// hand-fixed layout both produce) is a different STRING from the write
	// slot it aliases, and `==` cannot see they are the same directory —
	// which is how an earlier version of this sweep planned an `Unlink` for
	// the PHYSICAL shared slot itself, reached through the alias.
	//
	// THE WRITE SLOT COVERS IT AFTERWARDS is the fourth guard and lives here,
	// not in the predicate: it decides whether a descriptor's read dirs get
	// swept AT ALL, and it reads the PLAN rather than the disk — a slot this
	// run is about to Create or Relink covers the agent just as well as one
	// that already resolves, and demanding disk state would make the cleanup
	// need a SECOND repair run. Without it the "cleanup" silently REVOKES the
	// skill for an agent whose only Referrer was the compat one — the
	// pre-2.18 shape `repair` exists to rescue.
	let adopt_source = planned
		.iter()
		.find(|row| row.action == ReferrerAction::AdoptAsMaster)
		.map(|row| row.path.clone());
	let mut unlink: Vec<PlannedReferrer> = Vec::new();
	for descriptor in crate::registry::ALL_AGENTS.iter() {
		let Some(write_dir) = skill_write_dir(descriptor, scope, project_root)
		else {
			continue;
		};
		let slot = write_dir.join(&safe);
		let covered = planned.iter().any(|row| {
			row.path == slot
				&& match row.action {
					ReferrerAction::Create | ReferrerAction::Relink => true,
					// Step 5 swaps the adopted directory for a link to the
					// Master, so an agent whose write slot IS the adopt source
					// ends up covered exactly like one being linked. Missing
					// this still cost a second run at PROJECT scope, where
					// antigravity's write dir and the shared slot are the same
					// directory.
					ReferrerAction::AdoptAsMaster => true,
					ReferrerAction::Leave => {
						row.shape == SkillShape::Conformant
					}
					_ => false,
				}
		});
		if !covered {
			continue;
		}
		for read_dir in skill_read_dirs(descriptor, scope, project_root) {
			// Identity, not spelling — see the guards note above.
			if entry_identity(&read_dir) == entry_identity(&write_dir) {
				continue;
			}
			let entry = read_dir.join(&safe);
			match compat_unlink_permitted(
				&entry,
				&master,
				adopt_source.as_deref(),
				&planned,
			) {
				Ok(true) => {}
				Ok(false) => continue,
				// Fail CLOSED: an unreadable compat dir is a DECISION for a
				// human (fix the permission), not a transient write failure —
				// see `RefuseReason::UnreadableCompatDir`. This blocks the
				// WHOLE plan (`RepairPlan::refusals`), same as every other
				// refusal, rather than silently reporting the skill
				// conformant while a stale link sits right there.
				Err(_) => {
					unlink.push(PlannedReferrer {
						agents: vec![descriptor.id],
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
			// Collapse by IDENTITY like the guards above: one compat dir
			// several agents read — including two whose read paths spell it
			// differently through a symlinked ancestor — is one row naming
			// all of them, never one row each.
			if let Some(row) = unlink
				.iter_mut()
				.find(|r| entry_identity(&r.path) == entry_identity(&entry))
			{
				if !row.agents.contains(&descriptor.id) {
					row.agents.push(descriptor.id);
				}
				continue;
			}
			unlink.push(PlannedReferrer {
				agents: vec![descriptor.id],
				shape: classify_shape(&entry, &master),
				path: entry,
				action: ReferrerAction::Unlink,
				shared: false,
			});
		}
	}
	planned.extend(unlink);

	// Execution order: adopt the Master, then private Referrers, then the shared
	// slot, then the compat-dir detaches. Reversing this is the difference
	// between a crash that leaves the old directory serving the skill and one
	// that leaves it readable from nowhere — and the detaches go LAST for the
	// same reason: until the write slot is really on disk, the compat link is
	// the only thing still handing the agent the skill.
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
