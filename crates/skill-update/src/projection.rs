//! Lock → orchestrator-input projection: the ONE place that decides which
//! entries a check looks at, which folders it hashes, and in what order it
//! reads the two.
//!
//! Both surfaces consume it: the `wanted` filter, the per-root hash memo, the
//! offline skip, and the lock-before-disk read order live only here. The API
//! route (`GET /skills/check-updates`) also consumes the [`Identities`] half
//! and the heal writer [`write_auto_healed_hashes`] — it is the only surface
//! that heals the lock afterwards — while the CLI (`aghub-cli check`) ignores
//! both.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use aghub_core::errors::ConfigError;
use aghub_core::models::ResourceScope;
use aghub_core::skills::lock::EntryIdentity;
use chrono::Utc;

use crate::verdict::local_hashes_for_scope;
use crate::{CheckOutput, EntryInput, SourceRef};

pub use crate::verdict::LocalHashes;

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

/// Heal `name` only if the live entry is still EXACTLY the one the check read.
///
/// A check reads the lock unlocked, spends seconds on the network, and only then
/// takes the mutation lock. By write time another process may have
///
/// - repointed this name at a different source/ref/skillPath and written ITS
///   correct hash, or
/// - run `apply-update` on this very entry — same coordinates, but the content
///   hash and refCommit have moved on.
///
/// Both make the stale heal wrong, and both look fine to a name lookup. Writing
/// it anyway leaves a baseline the entry never came from, so every later check
/// reports a phantom "update available" until something rewrites the entry.
fn heal_precondition_holds(
	identities: &Identities,
	name: &str,
	live: HealPrecondition,
) -> bool {
	identities.get(name) == Some(&live)
}

/// The post-check compare-and-set heal. The API calls it after a check; the CLI
/// `check` never does (see `crates/skill-update/AGENTS.md`, "Deliberate surface
/// differences").
///
/// A busy mutation lock returns `ConfigError::Io(WouldBlock)` with NOTHING
/// written, so the caller can report it as retryable beside the check results.
pub fn write_auto_healed_hashes(
	outputs: &[CheckOutput],
	project_root: Option<&Path>,
	global_identities: &Identities,
	project_identities: &Identities,
) -> Result<(), ConfigError> {
	// One record per global name: hash and OID apply under a SINGLE
	// precondition check (writing the hash first would invalidate the
	// precondition a second OID pass re-checks, so the OID never lands).
	let mut global_heals: HashMap<String, (Option<&String>, Option<&String>)> =
		HashMap::new();
	let mut project_heals = HashMap::new();
	for output in outputs {
		match output.key.scope.as_str() {
			"global" => {
				// refCommit heal is GLOBAL-only (the project lock is
				// VCS-tracked) and independent of heal_hash (a known-stored
				// entry has no heal_hash).
				if output.heal_hash.is_some() || output.heal_oid.is_some() {
					let slot = global_heals
						.entry(output.key.name.clone())
						.or_insert((None, None));
					slot.0 = slot.0.or(output.heal_hash.as_ref());
					slot.1 = slot.1.or(output.heal_oid.as_ref());
				}
			}
			"project" => {
				if let Some(hash) = &output.heal_hash {
					project_heals.insert(output.key.name.clone(), hash.clone());
				}
			}
			_ => {}
		}
	}

	if global_heals.is_empty() && project_heals.is_empty() {
		return Ok(());
	}

	// One guard over every scope this heal writes, taken before the first write,
	// so a busy lock on either file makes the whole heal all-or-nothing. The
	// inner `modify_*_lock_changed` calls re-enter it for free.
	let scope = match (global_heals.is_empty(), project_heals.is_empty()) {
		(false, false) => ResourceScope::Both,
		(false, true) => ResourceScope::GlobalOnly,
		_ => ResourceScope::ProjectOnly,
	};
	let _guard = aghub_core::skills::lock::mutation_guard(
		"auto-heal skill lock",
		scope,
		project_root,
	)?;

	if !global_heals.is_empty() {
		skill::lock::global::modify_skill_lock_changed(|lock| {
			let now = Utc::now().to_rfc3339();
			let mut changed = false;
			for (name, (hash, oid)) in &global_heals {
				let Some(entry) = lock.skills.get_mut(name) else {
					continue;
				};
				if !heal_precondition_holds(
					global_identities,
					name,
					HealPrecondition::of_global_entry(entry),
				) {
					continue;
				}
				if let Some(hash) = hash {
					changed |= entry.apply_content_hash(hash, &now);
				}
				if let Some(oid) = oid {
					if entry.ref_commit.as_deref() != Some(oid.as_str()) {
						entry.ref_commit = Some((*oid).clone());
						entry.updated_at = now.clone();
						changed = true;
					}
				}
			}
			((), changed)
		})?;
	}

	// Project outputs only exist when a root was given, so `None` here means
	// there is nothing project-side to heal.
	if let Some(root) = project_root {
		if !project_heals.is_empty() {
			skill::lock::local::modify_local_lock_changed(
				Some(root),
				|lock| {
					let mut changed = false;
					for (name, hash) in &project_heals {
						if let Some(entry) = lock.skills.get_mut(name) {
							if !heal_precondition_holds(
								project_identities,
								name,
								HealPrecondition::of_project_entry(entry),
							) {
								continue;
							}
							changed |= entry.apply_computed_hash(hash);
						}
					}
					((), changed)
				},
			)?;
		}
	}

	Ok(())
}

/// Project the global skill lock into the orchestrator's per-entry inputs, plus
/// the identity of each entry AS READ HERE (the read that decides what to fetch).
///
/// Both reads are closures so a caller cannot get their ORDER wrong: lock
/// FIRST, then disk. Hashing disk first lets a concurrent `npx skills update`
/// land in between, and the stale-hash heal would then pass its precondition
/// and overwrite npx's newer state; lock-first leaves any interleaved write
/// ahead of the snapshot, so the precondition rejects the heal
/// (`the_lock_is_read_before_the_hashes_and_names_them`).
///
/// `read_lock` is a closure because the surfaces differ on purpose: the API
/// reads fail-open, the CLI hands in a snapshot it probed fail-closed.
pub fn global_lock_entries_with(
	read_lock: impl FnOnce() -> skill::SkillLockFile,
	read_hashes: impl FnOnce(&HashSet<String>) -> LocalHashes,
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
				local_hash: local_hashes.hashes.get(&name).cloned(),
				local_comparison_hash: local_hashes
					.comparison_hashes
					.get(&name)
					.cloned(),
				local_ambiguous: local_hashes.ambiguous.contains(&name),
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
	read_hashes: impl FnOnce(&HashSet<String>) -> LocalHashes,
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
				local_hash: local_hashes.hashes.get(&name).cloned(),
				local_comparison_hash: local_hashes
					.comparison_hashes
					.get(&name)
					.cloned(),
				local_ambiguous: local_hashes.ambiguous.contains(&name),
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
/// `offline` reaches [`local_hashes_for_scope`] HERE, once: a wrong flag would
/// still yield the right statuses, so the mistake would be invisible downstream
/// (`offline_is_wired_to_the_sweep_by_the_projection`).
pub fn global_lock_entries(
	offline: bool,
	read_lock: impl FnOnce() -> skill::SkillLockFile,
) -> (Vec<EntryInput>, Identities) {
	global_lock_entries_with(read_lock, |wanted| {
		local_hashes_for_scope(offline, ResourceScope::GlobalOnly, None, wanted)
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
				LocalHashes::default()
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
	/// its own half. Both matter and for the SAME reason: the heal writer
	/// (`write_auto_healed_hashes`, called only by the API) also heals the
	/// project lock, and a heal computed from
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
				LocalHashes::default()
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
