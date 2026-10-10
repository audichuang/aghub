//! The ONE place that decides a locked skill's local baseline and the verdict
//! against an upstream folder.
//!
//! Surfaces only map the verdict into their own vocabulary: `check` maps
//! [`Verdict`] onto `SkillUpdateStatus` (`Ambiguous` and `Uncheckable` both
//! become `Uncheckable{local}`), and `source diff` maps it onto its own states
//! the same way. The baseline discovery, the withheld-Master fallback, the single
//! folder walk per copy, the placeholder rule and the precedence all live here.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use aghub_core::models::ResourceScope;
use aghub_core::skills::removal::skill_root;
use aghub_core::skills::resync::stored_master_root;

/// Folder hashes for the installed copies of the locked names, plus the two
/// counters that make the sweep's cost assertable in a test (a timing-based
/// assertion cannot tell a memo hit from a fast disk).
#[derive(Default)]
pub struct LocalHashes {
	/// Hash per skill name. A name whose copies disagree across agents is
	/// dropped as ambiguous rather than reported with an arbitrary one.
	pub hashes: HashMap<String, String>,
	pub comparison_hashes: HashMap<String, String>,
	/// Folders actually read off disk.
	pub folders_hashed: usize,
	/// Copies served from the per-root memo instead of a fresh tree read.
	pub roots_reused: usize,
	/// Names whose managed copies DISAGREE (comparison hash), or one of which
	/// could not be hashed. They carry no hash; the verdict reports them as
	/// ambiguous.
	pub ambiguous: HashSet<String>,
}

/// Folder hashes for the installed copies of `wanted`, keyed by skill name.
///
/// `wanted` is the lock's key set: restricting the sweep to it keeps this off
/// every unlocked skill on the machine (folder-hashing reads every file).
/// The filter is by NAME only, so every agent's copy of a wanted name is still
/// seen for the ambiguity detection
/// (`local_hashes_cover_exactly_the_locked_names`).
/// See docs/history/skill-update.md#update-check-hashes-only-locked-names
///
/// `offline` returns empty without touching disk: an offline check has nothing
/// to compare a local hash against (`offline_does_not_hash_anything`).
pub(crate) fn local_hashes_for_scope(
	offline: bool,
	resource_scope: ResourceScope,
	project_root: Option<&Path>,
	wanted: &HashSet<String>,
) -> LocalHashes {
	// Decision 2 (docs/adr/0003-update-verdict-local-baseline.md): only agents
	// the user manages count; a copy only a disabled agent reads is neither the
	// baseline nor a disagreement. A Master no managed agent links is still
	// hashed below.
	local_hashes_with(offline, resource_scope, project_root, wanted, || {
		aghub_core::load_managed_agents(resource_scope, project_root)
	})
}

pub(crate) fn local_hashes_with(
	offline: bool,
	resource_scope: ResourceScope,
	project_root: Option<&Path>,
	wanted: &HashSet<String>,
	load_agents: impl FnOnce() -> Vec<aghub_core::AgentResources>,
) -> LocalHashes {
	let mut out = LocalHashes::default();
	let mut raw_ambiguous = HashSet::new();
	// Names some agent reported at all, hashable or not: only a name NO agent
	// carries may fall back to its stored Master below.
	let mut seen = HashSet::new();
	let started = std::time::Instant::now();
	// Agents that link to the same universal master resolve to the SAME root,
	// and a folder hash is a pure function of that folder — so the second agent
	// carrying a linked skill costs a map lookup instead of a full tree read.
	// One master was observed being re-hashed 19 times without this.
	let mut hash_by_root: HashMap<std::path::PathBuf, (String, String)> =
		HashMap::new();
	// The sweep — the agent scan AND every folder hash — is what `offline` skips.
	// The log line below is emitted either way, so `folders_hashed=0` is the
	// observable a caller's offline flag can be pinned by: it is otherwise
	// invisible, because the offline gate upstream never reads these hashes.
	if !offline && !wanted.is_empty() {
		for agent in load_agents() {
			for skill in agent.skills {
				if !wanted.contains(&skill.name)
					|| out.ambiguous.contains(&skill.name)
				{
					continue;
				}
				seen.insert(skill.name.clone());
				let Some(root) = skill_root(&skill) else {
					continue;
				};
				let (hash, comparison_hash) = if let Some(known) =
					hash_by_root.get(&root)
				{
					out.roots_reused += 1;
					known.clone()
				} else {
					out.folders_hashed += 1;
					let Ok(pair) = skill::compute_skill_folder_hashes(&root)
					else {
						// A managed copy we could not read is local evidence we
						// cannot vouch for: it vetoes "current" instead of letting
						// a healthy sibling stand in as the baseline.
						out.hashes.remove(&skill.name);
						out.comparison_hashes.remove(&skill.name);
						out.ambiguous.insert(skill.name);
						continue;
					};
					hash_by_root.insert(root.clone(), pair.clone());
					pair
				};
				if let Some(existing) = out.hashes.get(&skill.name) {
					if existing != &hash {
						out.hashes.remove(&skill.name);
						raw_ambiguous.insert(skill.name.clone());
					}
				} else if !raw_ambiguous.contains(&skill.name) {
					out.hashes.insert(skill.name.clone(), hash);
				}
				match out.comparison_hashes.get(&skill.name) {
					Some(existing) if existing != &comparison_hash => {
						out.comparison_hashes.remove(&skill.name);
						out.ambiguous.insert(skill.name);
					}
					Some(_) => {}
					None if !out.ambiguous.contains(&skill.name) => {
						out.comparison_hashes
							.insert(skill.name.clone(), comparison_hash);
					}
					None => {}
				}
			}
		}
		// A Master with zero Referrers (every agent unticked) is invisible to
		// the agent scan, yet `apply-update` still replaces it. Hash the same
		// folder resync would touch, or the check reads "no local copy" and
		// reports it uncheckable forever.
		// Exactly one store, as resync resolves it: never let a project check
		// with no root fall through to the GLOBAL store.
		let store_root = match resource_scope {
			ResourceScope::GlobalOnly => Some(None),
			ResourceScope::ProjectOnly => project_root.map(Some),
			_ => None,
		};
		let fallback = store_root
			.map(|root| wanted.difference(&seen).map(move |name| (name, root)))
			.into_iter()
			.flatten();
		for (name, store_root) in fallback {
			let Ok(Some(root)) = stored_master_root(name, store_root) else {
				continue;
			};
			out.folders_hashed += 1;
			let Ok((hash, comparison)) =
				skill::compute_skill_folder_hashes(&root)
			else {
				continue;
			};
			out.hashes.insert(name.clone(), hash);
			out.comparison_hashes.insert(name.clone(), comparison);
		}
	}
	log::info!(
		"check-updates: local hashes wanted={} folders_hashed={} \
		 root_reused={} distinct_names={} ambiguous={} took={:?}",
		wanted.len(),
		out.folders_hashed,
		out.roots_reused,
		out.hashes.len(),
		out.ambiguous.len(),
		started.elapsed()
	);
	out
}

/// Placeholder rule: a lock digest that is missing, empty, or the empty-input
/// SHA-256 says nothing about the installed content.
pub(crate) fn lock_hash_unknown(stored_hash: Option<&str>) -> bool {
	match stored_hash {
		None => true,
		Some("") => true,
		Some(hash) if skill::is_placeholder_digest(hash) => true,
		Some(_) => false,
	}
}

/// Surface-neutral answer to "is the installed copy current with upstream?".
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
	UpToDate,
	/// `current` is the local baseline digest, `available` the upstream one
	/// (comparison digests when a local copy was read, raw when the lock's
	/// hash stood in).
	UpdateAvailable {
		current: String,
		available: String,
	},
	/// No local evidence: no readable copy and nothing to say it is current.
	Uncheckable,
	/// The managed agents' copies disagree, or one could not be read.
	Ambiguous,
}

/// Judge one locked entry against the upstream folder's two digests.
/// Precedence: a local comparison hash is the baseline; without one, a known
/// lock hash stands in. A difference is `UpdateAvailable` regardless of local
/// evidence (applying it restores the folder). Equality is only `UpToDate`
/// with local evidence; otherwise `Ambiguous` / `Uncheckable`.
pub fn judge(
	stored_hash: Option<&str>,
	local_comparison_hash: Option<&str>,
	local_ambiguous: bool,
	upstream_raw: &str,
	upstream_comparison: &str,
) -> Verdict {
	let no_local = || {
		if local_ambiguous {
			Verdict::Ambiguous
		} else {
			Verdict::Uncheckable
		}
	};
	// Compare actual content when readable; stored lock digests remain raw.
	let (baseline, upstream) = match (local_comparison_hash, stored_hash) {
		(Some(local), _) => (local, upstream_comparison),
		(None, Some(stored)) if !lock_hash_unknown(Some(stored)) => {
			(stored, upstream_raw)
		}
		(None, _) => return no_local(),
	};
	if baseline != upstream {
		return Verdict::UpdateAvailable {
			current: baseline.to_string(),
			available: upstream.to_string(),
		};
	}
	// The lock agreeing with upstream does NOT mean the installed copy is
	// current — it means upstream has not moved since we recorded it. With
	// no readable local copy (folder deleted, unreadable, or two agent
	// copies disagreeing) that came out as `UpToDate` for a skill that is
	// not on disk, which is the one answer a user acts on by doing
	// nothing. `UpdateAvailable` stays as-is: an update really does exist,
	// and applying it restores the folder.
	if local_comparison_hash.is_none() {
		return no_local();
	}
	Verdict::UpToDate
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn copies_with_different_caches_share_comparison_but_not_raw_hash() {
		let project = tempfile::tempdir().unwrap();
		for agent in [".claude", ".cursor", ".opencode"] {
			let root = project.path().join(agent).join("skills/shared");
			write_skill(&root, "shared");
			std::fs::write(root.join("run.py"), "print('same')").unwrap();
			std::fs::create_dir(root.join("__pycache__")).unwrap();
			std::fs::write(root.join("__pycache__/run.cpython-312.pyc"), agent)
				.unwrap();
		}
		let wanted = HashSet::from(["shared".to_string()]);
		let hashes = local_hashes_with(
			false,
			ResourceScope::ProjectOnly,
			Some(project.path()),
			&wanted,
			|| {
				aghub_core::load_all_agents(
					ResourceScope::ProjectOnly,
					Some(project.path()),
				)
			},
		);
		assert!(!hashes.hashes.contains_key("shared"));
		assert!(hashes.comparison_hashes.contains_key("shared"));
		std::fs::write(
			project.path().join(".cursor/skills/shared/run.py"),
			"print('edited')",
		)
		.unwrap();
		let hashes = local_hashes_with(
			false,
			ResourceScope::ProjectOnly,
			Some(project.path()),
			&wanted,
			|| {
				aghub_core::load_all_agents(
					ResourceScope::ProjectOnly,
					Some(project.path()),
				)
			},
		);
		assert!(!hashes.comparison_hashes.contains_key("shared"));
		assert!(hashes.ambiguous.contains("shared"));
	}

	#[cfg(unix)]
	#[test]
	fn an_unreadable_copy_vetoes_current_instead_of_being_skipped() {
		use std::os::unix::fs::PermissionsExt;

		let project = tempfile::tempdir().unwrap();
		for agent in [".claude", ".cursor", ".opencode"] {
			let root = project.path().join(agent).join("skills/shared");
			write_skill(&root, "shared");
			std::fs::write(root.join("run.py"), "print('same')").unwrap();
			std::fs::create_dir(root.join("__pycache__")).unwrap();
			std::fs::write(root.join("__pycache__/run.cpython-312.pyc"), agent)
				.unwrap();
		}
		std::fs::write(
			project.path().join(".cursor/skills/shared/run.py"),
			"print('edited')",
		)
		.unwrap();
		let pyc = project
			.path()
			.join(".cursor/skills/shared/__pycache__/run.cpython-312.pyc");
		std::fs::set_permissions(&pyc, std::fs::Permissions::from_mode(0o000))
			.unwrap();
		if std::fs::read(&pyc).is_ok() {
			eprintln!("skip: perms not enforced (root)");
			return;
		}

		let wanted = HashSet::from(["shared".to_string()]);
		let out = local_hashes_with(
			false,
			ResourceScope::ProjectOnly,
			Some(project.path()),
			&wanted,
			|| {
				aghub_core::load_all_agents(
					ResourceScope::ProjectOnly,
					Some(project.path()),
				)
			},
		);
		assert!(out.ambiguous.contains("shared"));
		assert!(!out.comparison_hashes.contains_key("shared"));

		// The lock agrees with upstream, which used to read as UpToDate.
		let claude_copy = project.path().join(".claude/skills/shared");
		let upstream_raw =
			skill::compute_skill_folder_hash(&claude_copy).unwrap();
		let upstream =
			skill::compute_skill_folder_comparison_hash(&claude_copy).unwrap();
		assert_eq!(
			judge(
				Some(upstream_raw.as_str()),
				out.comparison_hashes.get("shared").map(String::as_str),
				out.ambiguous.contains("shared"),
				&upstream_raw,
				&upstream,
			),
			Verdict::Ambiguous
		);
	}

	/// Write `name` as a plain skill folder under `dir`.
	fn write_skill(dir: &Path, name: &str) {
		std::fs::create_dir_all(dir).unwrap();
		std::fs::write(
			dir.join("SKILL.md"),
			format!("---\nname: {name}\ndescription: d\n---\nbody {name}\n"),
		)
		.unwrap();
	}

	/// Unticking every agent leaves the Master and its lock entry behind with
	/// zero Referrers. `apply-update` still replaces that Master, so the check
	/// must hash it too — otherwise the row reads "no local copy" and is
	/// reported uncheckable with no way out. A locked name with no Master at all
	/// must still carry nothing, so the "folder gone" downgrade keeps working.
	#[test]
	fn a_master_with_zero_referrers_is_hashed_from_the_store() {
		let project = tempfile::tempdir().unwrap();
		let master = project.path().join(".aghub/withheld");
		write_skill(&master, "withheld");

		let wanted: HashSet<String> =
			["withheld", "gone"].into_iter().map(String::from).collect();
		let out = local_hashes_with(
			false,
			ResourceScope::ProjectOnly,
			Some(project.path()),
			&wanted,
			|| {
				aghub_core::load_all_agents(
					ResourceScope::ProjectOnly,
					Some(project.path()),
				)
			},
		);

		assert_eq!(
			out.hashes.get("withheld"),
			Some(&skill::compute_skill_folder_hash(&master).unwrap()),
			"a withheld Master must carry its own folder hash"
		);
		assert_eq!(
			out.comparison_hashes.get("withheld"),
			Some(
				&skill::compute_skill_folder_comparison_hash(&master).unwrap()
			),
		);
		assert!(
			!out.hashes.contains_key("gone")
				&& !out.comparison_hashes.contains_key("gone"),
			"a locked name with no copy anywhere must stay unhashed"
		);
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
		let out = local_hashes_with(
			false,
			ResourceScope::ProjectOnly,
			Some(project.path()),
			&wanted,
			|| {
				aghub_core::load_all_agents(
					ResourceScope::ProjectOnly,
					Some(project.path()),
				)
			},
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
		let out = local_hashes_with(
			false,
			ResourceScope::ProjectOnly,
			Some(project.path()),
			&wanted,
			|| {
				aghub_core::load_all_agents(
					ResourceScope::ProjectOnly,
					Some(project.path()),
				)
			},
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
		let out = local_hashes_with(
			true,
			ResourceScope::ProjectOnly,
			Some(project.path()),
			&wanted,
			|| {
				aghub_core::load_all_agents(
					ResourceScope::ProjectOnly,
					Some(project.path()),
				)
			},
		);

		assert_eq!(out.folders_hashed, 0);
		assert!(out.hashes.is_empty());
	}
}
