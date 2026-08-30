//! Lock → orchestrator-input projection: the ONE place that decides which
//! entries a check looks at, which folders it hashes, and in what order it
//! reads the two.
//!
//! Both surfaces consume it. The API route (`GET /skills/check-updates`) also
//! consumes the [`Identities`] half — it is the only surface that heals the
//! lock afterwards — while the CLI (`aghub-cli check`) ignores it. The rules
//! that used to live in the route file and were absent from the CLI's private
//! copies: the `wanted` filter, the per-root hash memo, the offline skip, and
//! the lock-before-disk read order.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use aghub_core::models::ResourceScope;
use aghub_core::skills::lock::EntryIdentity;
use aghub_core::skills::removal::skill_root;

use crate::{EntryInput, SourceRef};

/// Folder hashes for the installed copies of the locked names, plus the two
/// counters that make the sweep's cost assertable in a test (a timing-based
/// assertion cannot tell a memo hit from a fast disk).
struct LocalHashes {
	/// Hash per skill name. A name whose copies disagree across agents is
	/// dropped as ambiguous rather than reported with an arbitrary one.
	pub hashes: HashMap<String, String>,
	/// Folders actually read off disk.
	pub folders_hashed: usize,
	/// Copies served from the per-root memo instead of a fresh tree read.
	pub roots_reused: usize,
}

/// Folder hashes for the installed copies of `wanted`, keyed by skill name.
///
/// `wanted` is the lock's key set, and restricting the sweep to it is what keeps
/// this off every unlocked skill on the machine: folder-hashing reads every file
/// of every skill folder, and a real host measured 464 folders hashed to answer
/// 34 locked names — 10.4s of the check's 18.6s, ~93% of it discarded.
///
/// The filter is by NAME only, so every agent's copy of a wanted name is still
/// seen and the ambiguity detection below is unchanged.
///
/// `offline` returns empty without touching disk. A check that did not go to the
/// network has nothing to compare a local hash against, and hashing anyway is
/// what made the CLI's default-offline `check` pay for a full sweep it discarded.
fn local_hashes_for_scope(
	offline: bool,
	resource_scope: ResourceScope,
	project_root: Option<&Path>,
	wanted: &HashSet<String>,
) -> LocalHashes {
	let mut out = LocalHashes {
		hashes: HashMap::new(),
		folders_hashed: 0,
		roots_reused: 0,
	};
	let mut ambiguous = HashSet::new();
	let started = std::time::Instant::now();
	// Agents that link to the same universal master resolve to the SAME root,
	// and a folder hash is a pure function of that folder — so the second agent
	// carrying a linked skill costs a map lookup instead of a full tree read.
	// One master was observed being re-hashed 19 times without this.
	let mut hash_by_root: HashMap<std::path::PathBuf, String> = HashMap::new();
	// The sweep — the agent scan AND every folder hash — is what `offline` skips.
	// The log line below is emitted either way, so `folders_hashed=0` is the
	// observable a caller's offline flag can be pinned by: it is otherwise
	// invisible, because the offline gate upstream never reads these hashes.
	if !offline && !wanted.is_empty() {
		for agent in aghub_core::load_all_agents(resource_scope, project_root) {
			for skill in agent.skills {
				if !wanted.contains(&skill.name)
					|| ambiguous.contains(&skill.name)
				{
					continue;
				}
				let Some(root) = skill_root(&skill) else {
					continue;
				};
				let hash = if let Some(known) = hash_by_root.get(&root) {
					out.roots_reused += 1;
					known.clone()
				} else {
					out.folders_hashed += 1;
					let Ok(fresh) = skill::compute_skill_folder_hash(&root)
					else {
						continue;
					};
					hash_by_root.insert(root.clone(), fresh.clone());
					fresh
				};
				match out.hashes.get(&skill.name) {
					Some(existing) if existing != &hash => {
						out.hashes.remove(&skill.name);
						ambiguous.insert(skill.name);
					}
					Some(_) => {}
					None => {
						out.hashes.insert(skill.name, hash);
					}
				}
			}
		}
	}
	log::info!(
		"check-updates: local hashes wanted={} folders_hashed={} \
		 root_reused={} distinct_names={} ambiguous={} took={:?}",
		wanted.len(),
		out.folders_hashed,
		out.roots_reused,
		out.hashes.len(),
		ambiguous.len(),
		started.elapsed()
	);
	out
}

/// What one entry looked like BEFORE the check fetched: its coordinates, and
/// the metadata the check's heals are computed relative to.
///
/// A check decides WHAT to fetch from an unlocked read and only takes the
/// mutation lock afterwards to write its heals, so it owes the same
/// compare-and-set every other post-fetch writer does — see [`EntryIdentity`].
/// The identity alone is not enough: `apply-update` on this very entry leaves
/// the coordinates IDENTICAL while advancing `contentHash`/`refCommit`, so a
/// heal that only checked the identity would roll those newer values back to
/// what the stale check saw.
#[derive(PartialEq, Eq)]
pub struct HealPrecondition {
	identity: EntryIdentity,
	content_hash: Option<String>,
	ref_commit: Option<String>,
	/// npx's own baseline. Not just another field to compare: `apply_content_hash`
	/// CLEARS it, so a stale heal that ignored it would destroy a newer `npx
	/// skills update`'s record — and npx skips its update check outright when it
	/// is empty, leaving that skill silently frozen on both sides.
	skill_folder_hash: String,
}

impl HealPrecondition {
	pub fn of_global_entry(entry: &skill::SkillLockEntry) -> Self {
		Self {
			identity: EntryIdentity::of_global_entry(entry),
			content_hash: entry.content_hash.clone(),
			ref_commit: entry.ref_commit.clone(),
			skill_folder_hash: entry.skill_folder_hash.clone(),
		}
	}

	/// The project lock has no folder-hash field, so there is nothing to compare
	/// — `computed_hash` is its whole content baseline.
	pub fn of_project_entry(entry: &skill::LocalSkillLockEntry) -> Self {
		Self {
			identity: EntryIdentity::of_project_entry(entry),
			content_hash: Some(entry.computed_hash.clone()),
			ref_commit: entry.ref_commit.clone(),
			skill_folder_hash: String::new(),
		}
	}
}

/// Pre-fetch preconditions keyed by skill name within a scope.
pub type Identities = HashMap<String, HealPrecondition>;

/// Project the global skill lock into the orchestrator's per-entry inputs, plus
/// the identity of each entry AS READ HERE (the read that decides what to fetch).
///
/// The lock read and the disk read are both closures so the ORDER of the two
/// cannot be got wrong by a caller. The lock snapshot must not be NEWER than the
/// disk hashes paired with it: hashing disk first lets a concurrent `npx skills
/// update` land in between, and the check then pairs the OLD disk hash with a
/// lock snapshot that already reflects npx's write. The heal derived from that
/// stale hash matches the live lock, passes the precondition, and overwrites
/// npx's newer state. Snapshotting the lock first inverts that: any interleaved
/// write leaves the live lock ahead of the snapshot, so the precondition
/// rejects the heal instead.
///
/// `read_lock` is a closure rather than a fixed read because the two surfaces
/// disagree on purpose: the API reads the lock fail-open, while the CLI hands in
/// a snapshot it already probed fail-closed (an unreadable lock must fail
/// `check`, not read as "no skills installed").
///
/// It is also the seam the order above is testable through: a test supplies a
/// `read_hashes` that performs an npx-style write before returning, which is
/// exactly the interleaving the ordering defends against — deterministically,
/// with no sleep.
pub fn global_lock_entries_with(
	read_lock: impl FnOnce() -> skill::SkillLockFile,
	read_hashes: impl FnOnce(&HashSet<String>) -> HashMap<String, String>,
) -> (Vec<EntryInput>, Identities) {
	let lock = read_lock();
	// The lock snapshot decides which names are worth hashing. Deriving the set
	// HERE rather than inside `read_hashes` keeps the documented order intact:
	// the lock is still read first, and the hashes still come from a disk read
	// that happens after it.
	let wanted: HashSet<String> = lock.skills.keys().cloned().collect();
	let local_hashes = &read_hashes(&wanted);
	let mut identities = Identities::new();
	let entries = lock
		.skills
		.into_iter()
		.map(|(name, entry)| {
			identities.insert(
				name.clone(),
				HealPrecondition::of_global_entry(&entry),
			);
			EntryInput {
				local_hash: local_hashes.get(&name).cloned(),
				name,
				scope: "global".to_string(),
				source_ref: SourceRef {
					source: crate::sources::entry_clone_source(
						&entry.source,
						Some(&entry.source_url),
						&entry.source_type,
					),
					ref_: entry.ref_name,
				},
				source_type: entry.source_type,
				skill_path: entry.skill_path,
				stored_hash: entry.content_hash,
				ref_commit: entry.ref_commit,
			}
		})
		.collect();
	(entries, identities)
}

/// [`global_lock_entries_with`] for the project lock — same read-order rule,
/// same reason both reads are closures.
fn project_lock_entries_with(
	read_lock: impl FnOnce() -> skill::lock::local::LocalSkillLockFile,
	read_hashes: impl FnOnce(&HashSet<String>) -> HashMap<String, String>,
) -> (Vec<EntryInput>, Identities) {
	let lock = read_lock();
	let wanted: HashSet<String> = lock.skills.keys().cloned().collect();
	let local_hashes = &read_hashes(&wanted);
	let mut identities = Identities::new();
	let entries = lock
		.skills
		.into_iter()
		.map(|(name, entry)| {
			identities.insert(
				name.clone(),
				HealPrecondition::of_project_entry(&entry),
			);
			EntryInput {
				local_hash: local_hashes.get(&name).cloned(),
				name,
				scope: "project".to_string(),
				source_ref: SourceRef {
					// The shared coordinate — NOT a local `source_url.unwrap_or(
					// source)`, which reads a legacy GitLab entry's `group/repo` as
					// GitHub shorthand and checks it against the wrong repository.
					source: crate::sources::entry_clone_source(
						&entry.source,
						entry.source_url.as_deref(),
						&entry.source_type,
					),
					ref_: entry.ref_name,
				},
				source_type: entry.source_type,
				skill_path: entry.skill_path,
				stored_hash: Some(entry.computed_hash),
				ref_commit: entry.ref_commit,
			}
		})
		.collect();
	(entries, identities)
}

/// [`global_lock_entries_with`] wired to the real sweep.
///
/// `offline` reaches [`local_hashes_for_scope`] HERE, once — not through a
/// closure each surface writes beside its own call. A surface that passed the
/// wrong flag would still return the right statuses (the orchestrator's offline
/// gate never reads `local_hash`), so the mistake is invisible everywhere
/// downstream; the only defence is having one place to get it right.
pub fn global_lock_entries(
	offline: bool,
	read_lock: impl FnOnce() -> skill::SkillLockFile,
) -> (Vec<EntryInput>, Identities) {
	global_lock_entries_with(read_lock, |wanted| {
		local_hashes_for_scope(offline, ResourceScope::GlobalOnly, None, wanted)
			.hashes
	})
}

/// [`global_lock_entries`] for the project lock.
pub fn project_lock_entries(
	offline: bool,
	project_root: Option<&Path>,
	read_lock: impl FnOnce() -> skill::lock::local::LocalSkillLockFile,
) -> (Vec<EntryInput>, Identities) {
	project_lock_entries_with(read_lock, |wanted| {
		local_hashes_for_scope(
			offline,
			ResourceScope::ProjectOnly,
			project_root,
			wanted,
		)
		.hashes
	})
}

#[cfg(test)]
mod tests {
	use super::*;

	/// Write `name` as a plain skill folder under `dir`.
	fn write_skill(dir: &Path, name: &str) {
		std::fs::create_dir_all(dir).unwrap();
		std::fs::write(
			dir.join("SKILL.md"),
			format!("---\nname: {name}\ndescription: d\n---\nbody {name}\n"),
		)
		.unwrap();
	}

	/// The hash sweep must cover EXACTLY the locked names — and carry the right
	/// value for them.
	///
	/// Both halves are load-bearing. Hashing beyond the lock is pure waste (a
	/// real host hashed 464 folders to answer 34 names, 10.4s of an 18.6s
	/// check), but a filter that drops a name the lock DOES ask about is worse
	/// than slow: `local_hash: None` makes the check compare against nothing and
	/// report a locally-modified skill as up to date. The value assertion is
	/// what separates "filtered correctly" from "filtered everything out".
	#[test]
	fn local_hashes_cover_exactly_the_locked_names() {
		let project = tempfile::tempdir().unwrap();
		for name in ["locked", "unlocked"] {
			write_skill(
				&project.path().join(".claude/skills").join(name),
				name,
			);
		}

		let wanted: HashSet<String> =
			std::iter::once("locked".to_string()).collect();
		let out = local_hashes_for_scope(
			false,
			ResourceScope::ProjectOnly,
			Some(project.path()),
			&wanted,
		);

		let expected = skill::compute_skill_folder_hash(
			&project.path().join(".claude/skills/locked"),
		)
		.expect("the locked skill folder hashes");
		assert_eq!(
			out.hashes.get("locked"),
			Some(&expected),
			"a locked name must carry its real folder hash"
		);
		assert!(
			!out.hashes.contains_key("unlocked"),
			"a skill absent from the lock must not be hashed"
		);
		assert_eq!(
			out.folders_hashed, 1,
			"only the locked folder may be read off disk"
		);
	}

	/// The universal Master is on the read path of many agents at once, so the
	/// SAME folder comes back once per agent. Hashing reads every file in the
	/// tree, so re-reading it per agent is the sweep's other
	/// waste — one master was observed hashed 19 times.
	///
	/// Counted, not timed: a memo hit and a warm page cache are
	/// indistinguishable on a clock.
	#[test]
	fn one_master_read_by_many_agents_is_hashed_once() {
		let project = tempfile::tempdir().unwrap();
		// `.agents/skills` is on the project read path of every universal-master
		// agent (cursor, amp, cline, gemini, …), so one folder yields many rows.
		write_skill(&project.path().join(".agents/skills/shared"), "shared");

		let wanted: HashSet<String> =
			std::iter::once("shared".to_string()).collect();
		let out = local_hashes_for_scope(
			false,
			ResourceScope::ProjectOnly,
			Some(project.path()),
			&wanted,
		);

		assert!(
			out.hashes.contains_key("shared"),
			"the master must still be hashed once"
		);
		// Non-vacuity, measured WITHOUT the memo counter: every row that reaches
		// the hash step lands in exactly one of the two counters, so their sum
		// is the number of agent rows carrying this master. Asserting on
		// `roots_reused` alone would go vacuous the moment the memo is removed —
		// which is the very regression this test exists to catch.
		assert!(
			out.folders_hashed + out.roots_reused > 1,
			"more than one agent must have reported this master, or the test \
			 proves nothing (hashed={} reused={})",
			out.folders_hashed,
			out.roots_reused
		);
		assert_eq!(
			out.folders_hashed,
			1,
			"every agent resolves to the same root, so the tree may be read \
			 exactly once ({} rows observed)",
			out.folders_hashed + out.roots_reused
		);
	}

	/// Offline never touches disk. The CLI's default `check` is offline, so a
	/// sweep here is paid on every run and discarded — no local hash can be
	/// compared against an upstream nobody fetched.
	#[test]
	fn offline_does_not_hash_anything() {
		let project = tempfile::tempdir().unwrap();
		write_skill(&project.path().join(".claude/skills/locked"), "locked");

		let wanted: HashSet<String> =
			std::iter::once("locked".to_string()).collect();
		let out = local_hashes_for_scope(
			true,
			ResourceScope::ProjectOnly,
			Some(project.path()),
			&wanted,
		);

		assert_eq!(out.folders_hashed, 0);
		assert!(out.hashes.is_empty());
	}

	/// The lock is read BEFORE the disk sweep, and the sweep is told exactly
	/// which names the lock asked about. Both halves are what
	/// `local_hashes_for_scope`'s `wanted` filter and the heal precondition
	/// depend on; a caller that reversed them would still compile.
	#[test]
	fn the_lock_is_read_before_the_hashes_and_names_them() {
		let order = std::cell::RefCell::new(Vec::<&'static str>::new());
		let seen = std::cell::RefCell::new(HashSet::new());

		let mut lock = skill::SkillLockFile::default();
		lock.skills.insert(
			"legacy".to_string(),
			skill::SkillLockEntry {
				source: "owner/repo".to_string(),
				source_type: "github".to_string(),
				source_url: "https://github.com/owner/repo".to_string(),
				ref_name: Some("main".to_string()),
				skill_path: Some("SKILL.md".to_string()),
				skill_folder_hash: String::new(),
				content_hash: None,
				ref_commit: None,
				installed_at: "t".to_string(),
				updated_at: "t".to_string(),
				plugin_name: None,
			},
		);

		let (entries, _identities) = global_lock_entries_with(
			|| {
				order.borrow_mut().push("lock");
				lock
			},
			|wanted| {
				order.borrow_mut().push("hashes");
				*seen.borrow_mut() = wanted.clone();
				HashMap::new()
			},
		);

		assert_eq!(
			order.into_inner(),
			vec!["lock", "hashes"],
			"hashing before snapshotting the lock lets a concurrent npx write \
			 land between the two, and the resulting heal overwrites it"
		);
		assert_eq!(
			seen.into_inner(),
			HashSet::from(["legacy".to_string()]),
			"the sweep must be scoped to the names the lock snapshot holds"
		);
		assert_eq!(entries.len(), 1);
	}

	/// The project half of the read order, which the global test above pins for
	/// its own half. Both matter and for the SAME reason: the API heals the
	/// project lock too (`write_auto_healed_hashes`), and a heal computed from
	/// disk hashes older than the lock snapshot passes its own precondition and
	/// then clears `skillFolderHash` — destroying an `npx skills update` that
	/// landed in between.
	#[test]
	fn the_project_lock_is_read_before_the_hashes_and_names_them() {
		let order = std::cell::RefCell::new(Vec::<&'static str>::new());
		let seen = std::cell::RefCell::new(HashSet::new());

		let mut lock = skill::lock::local::LocalSkillLockFile::new();
		lock.skills.insert(
			"legacy".to_string(),
			skill::LocalSkillLockEntry {
				source: "owner/repo".to_string(),
				source_type: "github".to_string(),
				source_url: None,
				ref_name: Some("main".to_string()),
				skill_path: Some("SKILL.md".to_string()),
				computed_hash: "h".to_string(),
				ref_commit: None,
			},
		);

		let (entries, _identities) = project_lock_entries_with(
			|| {
				order.borrow_mut().push("lock");
				lock
			},
			|wanted| {
				order.borrow_mut().push("hashes");
				*seen.borrow_mut() = wanted.clone();
				HashMap::new()
			},
		);

		assert_eq!(
			order.into_inner(),
			vec!["lock", "hashes"],
			"hashing before snapshotting the lock lets a concurrent npx write \
			 land between the two, and the resulting heal overwrites it"
		);
		assert_eq!(
			seen.into_inner(),
			HashSet::from(["legacy".to_string()]),
			"the sweep must be scoped to the names the lock snapshot holds"
		);
		assert_eq!(entries.len(), 1);
	}

	/// `offline` is wired to the sweep by the projection, not by each surface.
	///
	/// Asserted on the ENTRY, not on `LocalHashes`: `local_hash` is what a
	/// caller's offline flag actually reaches, and it is also why a mis-wire is
	/// otherwise undetectable — the orchestrator's offline gate answers
	/// `Uncheckable{network}` without ever reading it, so the statuses are
	/// identical either way and only the wasted sweep differs.
	#[test]
	fn offline_is_wired_to_the_sweep_by_the_projection() {
		let project = tempfile::tempdir().unwrap();
		write_skill(&project.path().join(".claude/skills/locked"), "locked");
		let entry = skill::LocalSkillLockEntry {
			source: "owner/repo".to_string(),
			source_type: "github".to_string(),
			source_url: None,
			ref_name: Some("main".to_string()),
			skill_path: Some("locked/SKILL.md".to_string()),
			computed_hash: "stale".to_string(),
			ref_commit: None,
		};
		let read_lock = || {
			let mut lock = skill::lock::local::LocalSkillLockFile::new();
			lock.skills.insert("locked".to_string(), entry.clone());
			lock
		};

		let (online, _) =
			project_lock_entries(false, Some(project.path()), read_lock);
		assert_eq!(
			online[0].local_hash,
			skill::compute_skill_folder_hash(
				&project.path().join(".claude/skills/locked")
			)
			.ok(),
			"an online check must carry the installed copy's real hash, or \
			 every locally-modified skill reads as up to date"
		);

		let (offline, _) =
			project_lock_entries(true, Some(project.path()), read_lock);
		assert_eq!(
			offline[0].local_hash, None,
			"an offline check hashes nothing — it has no upstream to compare \
			 against"
		);
	}
}
