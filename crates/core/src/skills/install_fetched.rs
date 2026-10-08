//! No-network install of an ALREADY-FETCHED skill.
//!
//! Shared by the API git-install route and CLI `source sync`: install a skill
//! already fetched into a local tree (Master in `.aghub` + per-agent
//! Referrers) and write the install lock. NO network, NO credential
//! resolution — fetch + auth live in the caller.
//!
//! Returns PER-AGENT results. An `Unsupported` agent is a PREDICTABLE failure:
//! the shared multi-target preflight rejects the whole batch before any write.
//! Soft failures (`installed: false`, `error: Some(..)`) are runtime link
//! failures on targets that passed preflight.
//!
//! "Decision N" below refers to the decision list in
//! `docs/specs/2026-06-19-symlink-only-skill-install.md`.

use std::path::{Path, PathBuf};

use crate::models::ResourceScope;
use crate::skills::linker::classify::{classify_agent, LinkNeed};
use crate::skills::linker::{install_universal, master_store_dir, LinkTarget};
use crate::skills::skill_source_root;
use crate::skills::update::{detect_rename, skill_renamed_message};
use aghub_agents::models::AgentType;

/// What happened for one target agent.
#[derive(Clone, Debug)]
pub struct AgentInstallResult {
	pub agent: AgentType,
	/// `true` when this agent received the skill on this call (a fresh copy or
	/// a fresh universal link). `false` for a soft skip or runtime link failure
	/// (see `error`). Predictably unsupported targets fail preflight before a
	/// report is created.
	pub installed: bool,
	pub error: Option<String>,
}

/// Inputs for [`install_fetched_skill_and_lock`].
pub struct FetchedSkillInstallRequest<'a> {
	/// `SKILL.md` inside the already-fetched tree (or its parent dir).
	pub skill_file: &'a Path,
	/// Fetched source provenance. The source field is the normalized ownership
	/// key.
	pub source: &'a skill::InstallLockSource,
	/// npx-form lock path, e.g. `"<dir>/SKILL.md"`.
	pub lock_skill_path: String,
	/// Repo tip OID for the lock `refCommit` heal (best-effort).
	pub ref_commit: Option<String>,
	/// Install scope. Only `GlobalOnly` / `ProjectOnly` are supported.
	pub scope: ResourceScope,
	pub project_root: Option<&'a Path>,
	pub target_agents: &'a [AgentType],
	/// Rename guard: when `Some(n)`, the fetched frontmatter name MUST equal `n`
	/// or the install is refused before any write.
	pub expected_name: Option<&'a str>,
	/// Link style: relative links (project scope, portable) vs absolute
	/// (global scope). Junctions always resolve absolute regardless.
	pub target: LinkTarget,
}

/// Result of [`install_fetched_skill_and_lock`].
#[derive(Clone, Debug)]
pub struct FetchedSkillInstallReport {
	/// Parsed (canonical) skill name.
	pub name: String,
	/// The lock entry was written — either created or rewritten in place.
	pub wrote_lock: bool,
	/// The lock ENTRY did not exist before this call, established by the write
	/// itself (nothing was replaced) rather than by an earlier observation. A
	/// rewrite of a pre-existing entry is not creation: rolling back must restore
	/// that entry, never delete it.
	pub created_lock: bool,
	/// The global lock entry this call REPLACED, for a rollback to restore.
	pub replaced_global_entry: Option<skill::SkillLockEntry>,
	/// The project lock entry this call REPLACED, for a rollback to restore.
	pub replaced_project_entry: Option<skill::LocalSkillLockEntry>,
	/// `true` only when this call atomically claimed and wrote the canonical
	/// Master. A caller rolling its own install back must not remove a Master it
	/// merely found and verified — that copy belongs to whoever wrote it.
	pub wrote_master: bool,
	/// Agent skills-dirs where this call created a FRESH referrer, straight from
	/// the linker. The sound attribution for a rollback.
	pub created_referrer_dirs: Vec<PathBuf>,
	/// Content hash of the fetched source folder.
	pub installed_hash: String,
	pub agent_results: Vec<AgentInstallResult>,
}

pub use crate::skills::adoption::{
	ensure_link_free_master, hash_master, remote_owner_from_url,
	same_source_owner, skill_lock_source, AdoptionCheck, LockedSourceOwner,
};

/// Whether a same-owner lock's update coordinates disagree with this request.
/// Healing them keeps `source sync --update` on the requested ref; an
/// idempotent re-install writes no file, so stale coordinates would survive.
///
/// Only a PRESENT requested value can differ: an omitted coordinate is carried
/// over from the recorded entry at the write site, never erased.
fn coordinates_need_heal(
	existing: &LockedSourceOwner,
	requested_ref: Option<&str>,
	requested_skill_path: &str,
	requested_commit: Option<&str>,
) -> bool {
	let differs = |req: Option<&str>, locked: &Option<String>| {
		req.is_some_and(|r| locked.as_deref() != Some(r))
	};
	differs(requested_ref, &existing.ref_name)
		|| differs(requested_commit, &existing.ref_commit)
		|| existing.skill_path.as_deref() != Some(requested_skill_path)
}

/// What a lock write actually replaced, straight from the map insert. `None` in
/// both fields after a write means the entry was CREATED, so a rollback owns it;
/// a `Some` is the previous entry a rollback must RESTORE rather than delete.
#[derive(Clone, Debug, Default)]
struct LockWriteReceipt {
	replaced_global: Option<skill::SkillLockEntry>,
	replaced_project: Option<skill::LocalSkillLockEntry>,
}

fn write_install_lock(
	skill_name: &str,
	scope: ResourceScope,
	project_root: Option<&Path>,
	source: &skill::InstallLockSource,
	lock_skill_path: String,
	source_dir: &Path,
	ref_commit: Option<String>,
) -> Result<LockWriteReceipt, crate::ConfigError> {
	match scope {
		ResourceScope::GlobalOnly => skill::write_global_install_lock(
			skill_name,
			source,
			Some(lock_skill_path),
			source_dir,
			ref_commit,
		)
		.map(|replaced_global| LockWriteReceipt {
			replaced_global,
			replaced_project: None,
		})
		.map_err(crate::ConfigError::Io),
		ResourceScope::ProjectOnly => {
			let cwd = project_root.ok_or_else(|| {
				crate::ConfigError::InvalidConfig(
					"project root is required for project skill installs"
						.to_string(),
				)
			})?;
			skill::write_project_install_lock(
				skill_name,
				source,
				Some(lock_skill_path),
				source_dir,
				cwd,
				ref_commit,
			)
			.map(|replaced_project| LockWriteReceipt {
				replaced_global: None,
				replaced_project,
			})
			.map_err(crate::ConfigError::Io)
		}
		ResourceScope::Both => Err(crate::ConfigError::InvalidConfig(
			"Combined skill scope is not supported for installs".to_string(),
		)),
	}
}

/// Parse + rename guard + scope guard. Pure reads; runs BEFORE the mutation
/// lock so an unsupported request creates nothing. Returns the skill name.
fn precheck_request(
	req: &FetchedSkillInstallRequest<'_>,
) -> Result<String, crate::ConfigError> {
	let parsed = skill::parser::parse(req.skill_file).map_err(|e| {
		crate::ConfigError::InvalidConfig(format!("Failed to parse skill: {e}"))
	})?;
	let name = parsed.name;

	// Rename guard: refuse before any write if the fetched name diverged.
	if let Some(expected) = req.expected_name {
		if let Some(found) = detect_rename(&name, expected) {
			return Err(crate::ConfigError::ValidationFailed(
				skill_renamed_message(expected, &found),
			));
		}
	}

	// Scope guard: only Global / Project installs are supported. Reject BEFORE
	// any source-root resolution / copy / link / lock work so an unsupported
	// scope can never leave a partial side effect (e.g. a written universal
	// master). The API rejects `Both` at the same point via `resource_scope`.
	if !matches!(
		req.scope,
		ResourceScope::GlobalOnly | ResourceScope::ProjectOnly
	) {
		return Err(crate::ConfigError::InvalidConfig(
			"Combined skill scope is not supported for installs".to_string(),
		));
	}
	Ok(name)
}

/// Owner / Master-link / Master-hash guard. Pure reads; the install runs it
/// under the mutation lock, [`preflight_fetched_install`] without one.
fn adoption_guard(
	name: &str,
	req: &FetchedSkillInstallRequest<'_>,
) -> Result<AdoptionCheck, crate::ConfigError> {
	let source_root = skill_source_root(req.skill_file);
	crate::skills::adoption::adoption_guard(
		name,
		&source_root,
		req.scope,
		req.project_root,
		req.source,
	)
}

/// Advisory dry run of the install's refusals: the same [`precheck_request`]
/// and [`adoption_guard`] the install runs, but WITHOUT the mutation lock and
/// with no write. The install re-runs the guard under the lock, so this
/// answer can go stale between the call and a later install.
pub fn preflight_fetched_install(
	req: &FetchedSkillInstallRequest<'_>,
) -> Result<(), crate::ConfigError> {
	let name = precheck_request(req)?;
	adoption_guard(&name, req).map(|_| ())
}

/// Install an already-fetched skill into the resolved agent dirs and write the
/// install lock. See module docs. Performs no network / credential work.
///
/// Before any Master, Referrer, or lock mutation, an existing Master must hash
/// identically to the fetched content and an existing lock must have the same
/// normalized source owner. Exact-byte untracked Masters may be adopted. The
/// Master is hashed again immediately before a source lock is persisted.
pub fn install_fetched_skill_and_lock(
	req: FetchedSkillInstallRequest<'_>,
) -> Result<FetchedSkillInstallReport, crate::ConfigError> {
	let name = precheck_request(&req)?;

	// Hold the interprocess mutation lock from the FIRST state read (the
	// ownership / Master-hash guards below) through the lock write, so no other
	// aghub process can invalidate a guard between checking it and acting on it.
	let _mutation_guard = crate::skills::lock::mutation_guard(
		"install skill",
		req.scope,
		req.project_root,
	)
	.map_err(crate::ConfigError::Io)?;

	let AdoptionCheck {
		source_root,
		safe_name,
		installed_hash,
		canonical,
		existing_owner,
	} = adoption_guard(&name, &req)?;

	// LAST preflight before the first write: an unreadable lock refused at
	// the END would leave an untracked partial install. Refuse while there is
	// nothing to roll back.
	skill::lock::ensure_locks_writable(
		req.scope != ResourceScope::ProjectOnly,
		match req.scope {
			ResourceScope::GlobalOnly => None,
			_ => req.project_root,
		},
	)
	.map_err(crate::ConfigError::Io)?;

	let materialized = materialize_universal_master(
		&source_root,
		&safe_name,
		req.scope,
		req.project_root,
		req.target_agents,
		req.target,
	)?;
	let MaterializedMaster {
		agent_results,
		created_master: wrote_master,
		created_referrer_dirs,
	} = materialized;

	// Gate lock rewrites on a fresh Master or fresh Referrer — the CREATION
	// receipt, never readability (`installed` folds in `already_linked`, so an
	// idempotent re-run would rewrite the lock). One extra safe case: adopt an
	// untracked byte-identical Master with at least one covered target.
	// See docs/history/core-install-linker.md#install-attribution-vs-rollback-receipts
	let linked_any = !created_referrer_dirs.is_empty();
	let covered_any = agent_results.iter().any(|r| r.error.is_none());
	// A same-owner re-install that changed nothing on disk still has to correct
	// stale update coordinates; ownership and Master content are already proven
	// identical at this point, and the write below re-verifies the hash.
	let heal_coordinates = existing_owner.as_ref().is_some_and(|owner| {
		coordinates_need_heal(
			owner,
			req.source.ref_name.as_deref(),
			&req.lock_skill_path,
			req.ref_commit.as_deref(),
		)
	});
	let wrote_lock = wrote_master
		|| linked_any
		|| (existing_owner.is_none() && covered_any)
		|| heal_coordinates;
	// Filled from the write below, never from `existing_owner`: an observation
	// taken before the write cannot prove what the write actually replaced.
	let mut receipt = LockWriteReceipt::default();
	if wrote_lock {
		let canonical = canonical.as_ref().ok_or_else(|| {
			crate::ConfigError::ValidationFailed(format!(
				"Master for skill '{name}' could not be resolved before the \
				 source lock write; the lock was not written",
			))
		})?;
		ensure_link_free_master(&name, canonical)?;
		let master_hash = hash_master(&name, canonical)?;
		if master_hash != installed_hash {
			return Err(crate::ConfigError::ValidationFailed(format!(
				"Master for skill '{name}' does not match the fetched content \
				 before the source lock write; the lock was not written",
			)));
		}
		// Never DROP a coordinate this request omits. But a recorded commit
		// certifies ONE (ref, skillPath) pair: carry it over only while both
		// still match, or preflight treats unverified coordinates as proven.
		let mut effective_source = req.source.clone();
		let mut effective_commit = req.ref_commit.clone();
		if let Some(owner) = existing_owner.as_ref() {
			if effective_source.ref_name.is_none() {
				effective_source.ref_name = owner.ref_name.clone();
			}
			let same_context = owner.skill_path.as_deref()
				== Some(req.lock_skill_path.as_str())
				&& owner.ref_name.as_deref()
					== effective_source.ref_name.as_deref();
			if effective_commit.is_none() && same_context {
				effective_commit = owner.ref_commit.clone();
			}
		}
		// Roll back from THIS call's receipt when the lock write fails (late
		// I/O, or a foreign non-aghub writer): a bare `?` would leave an
		// untracked install the caller was told failed.
		receipt = match write_install_lock(
			&name,
			req.scope,
			req.project_root,
			&effective_source,
			req.lock_skill_path.clone(),
			&source_root,
			effective_commit,
		) {
			Ok(receipt) => receipt,
			Err(error) => {
				crate::skills::rename::rollback_materialized_install(
					&name,
					req.scope,
					req.project_root,
					&created_referrer_dirs,
					wrote_master,
				);
				return Err(error);
			}
		};
	}
	let created_lock = wrote_lock
		&& receipt.replaced_global.is_none()
		&& receipt.replaced_project.is_none();

	Ok(FetchedSkillInstallReport {
		name,
		wrote_lock,
		created_lock,
		replaced_global_entry: receipt.replaced_global,
		replaced_project_entry: receipt.replaced_project,
		wrote_master,
		created_referrer_dirs,
		installed_hash,
		agent_results,
	})
}

/// What [`materialize_universal_master`] actually did, as attribution a caller
/// can roll back safely.
pub struct MaterializedMaster {
	pub agent_results: Vec<AgentInstallResult>,
	/// `true` only when this call atomically claimed and wrote the Master.
	pub created_master: bool,
	/// The agent skills-dirs where this call created a FRESH referrer, taken
	/// from the linker's own `linked` set — never reconstructed from
	/// `installed` (which includes already-linked slots).
	pub created_referrer_dirs: Vec<PathBuf>,
}

/// The ONE universal-install materializer shared by every install path: the
/// fetched/desktop install ([`install_fetched_skill_and_lock`]) AND the CLI
/// `aghub add skill` path (`ConfigManager::add_skill_universal` /
/// `add_skill_from_path_universal`). Materializes the `.aghub` Master from
/// `source_root` (copied only when absent) and links each `NeedsLink` agent.
///
/// Unsupported agents reject the whole request before the shared Master
/// write. A per-agent LinkError is folded into that agent's row (Decision 10).
pub fn materialize_universal_master(
	source_root: &Path,
	safe_name: &str,
	scope: ResourceScope,
	project_root: Option<&Path>,
	target_agents: &[AgentType],
	target: LinkTarget,
) -> Result<MaterializedMaster, crate::ConfigError> {
	let canonical_root = if matches!(scope, ResourceScope::ProjectOnly) {
		project_root
	} else {
		None
	};
	let Some(canonical_skills_dir) = master_store_dir(canonical_root) else {
		let results = target_agents
			.iter()
			.map(|&agent| AgentInstallResult {
				agent,
				installed: false,
				error: Some(
					"Cannot resolve the .aghub Master store directory"
						.to_string(),
				),
			})
			.collect();
		return Ok(MaterializedMaster {
			agent_results: results,
			created_master: false,
			created_referrer_dirs: Vec::new(),
		});
	};
	let canonical = canonical_skills_dir.join(safe_name);

	// Classify every target agent against the canonical SKILLS-DIR (not the
	// SKILL-DIR). `plans[i]` pairs 1:1 with `target_agents[i]`.
	let plans: Vec<(AgentType, LinkNeed)> = target_agents
		.iter()
		.map(|&agent| {
			let descriptor = crate::registry::get(agent);
			(agent, classify_agent(descriptor, scope, project_root).need)
		})
		.collect();
	if plans.is_empty() {
		return Ok(MaterializedMaster {
			agent_results: Vec::new(),
			created_master: false,
			created_referrer_dirs: Vec::new(),
		});
	}

	// One shared setup materializes the Master and performs every needed link.
	// Unsupported is a predictable preflight failure: mixing one with supported
	// targets must never let the shared setup create a Master or an earlier link.
	let mut created_master = false;
	let mut created_referrer_dirs: Vec<PathBuf> = Vec::new();
	let report = crate::batch::run_shared_multi_target_mutation(
		&plans,
		|&(agent, ref need)| match need {
			LinkNeed::Unsupported => Err(format!(
				"agent '{}' does not support persistent skill creation in this scope",
				agent.as_str()
			)),
			_ => Ok(()),
		},
		|plans| {
			// Dedup: sharers of one slot would otherwise report
			// `AlreadyLinked` against work this very call just did.
			let mut seen = std::collections::HashSet::new();
			let symlink_dirs = plans
				.iter()
				.filter_map(|(_, need)| match need {
					LinkNeed::NeedsLink { referrer_dir } => {
						Some(referrer_dir.clone())
					}
					_ => None,
				})
				.filter(|dir| seen.insert(dir.clone()))
				.collect::<Vec<_>>();
			let install = install_universal(
				source_root,
				&canonical,
				&symlink_dirs,
				target,
			)
			.map_err(|error| error.to_string())?;
			created_master = install.created_master;

			let failed_by_dir = install
				.failed
				.iter()
				.filter_map(|(link, error)| {
					link.parent()
						.map(|parent| (parent.to_path_buf(), error.to_string()))
				})
				.collect::<std::collections::HashMap<_, _>>();
			let conflict_dirs = install
				.conflicts
				.iter()
				.filter_map(|link| link.parent().map(Path::to_path_buf))
				.collect::<std::collections::HashSet<_>>();
			let linked_dirs = install
				.linked
				.iter()
				.filter_map(|link| link.parent().map(Path::to_path_buf))
				.collect::<std::collections::HashSet<_>>();
			// The linker's own record of what it CREATED -- the only sound
			// attribution for a rollback. Deliberately excludes
			// `already_linked`: rolling back a link this call did not create
			// would remove a grant that was already there.
			created_referrer_dirs = linked_dirs.iter().cloned().collect();
			// Attribution differs from rollback: an ALREADY correctly linked
			// slot is installed (it can read the skill), or a shared slot's
			// other sharers report a silent failure on a first install.
			let mut present_dirs = linked_dirs.clone();
			present_dirs.extend(
				install
					.already_linked
					.iter()
					.filter_map(|link| link.parent().map(Path::to_path_buf)),
			);
			Ok((failed_by_dir, conflict_dirs, present_dirs))
		},
		|&(agent, ref need), prepared| {
			let result = match need {
				LinkNeed::NeedsLink { referrer_dir } => {
					let (failed_by_dir, conflict_dirs, present_dirs) = prepared;
					let agent_skills_dir = referrer_dir;
					if let Some(message) = failed_by_dir.get(agent_skills_dir) {
						AgentInstallResult {
							agent,
							installed: false,
							error: Some(message.clone()),
						}
					} else if conflict_dirs.contains(agent_skills_dir) {
						AgentInstallResult {
							agent,
							installed: false,
							error: Some(
								"A real directory or a foreign link already \
								 occupies this skill slot; it was not overwritten"
									.to_string(),
							),
						}
					} else {
						AgentInstallResult {
							agent,
							installed: present_dirs.contains(agent_skills_dir),
							error: None,
						}
					}
				}
				LinkNeed::Unsupported => AgentInstallResult {
					agent,
					installed: false,
					error: Some(
						"Agent does not support persistent skill creation in \
						 this scope"
							.to_string(),
					),
				},
			};
			Ok(result)
		},
	)
	.map_err(|error| {
		crate::ConfigError::InvalidConfig(format!(
			"skill install preflight failed: {}; nothing was written",
			error
				.failures
				.into_iter()
				.map(|failure| failure.reason)
				.collect::<Vec<_>>()
				.join("; ")
		))
	})?;

	let results = report
		.results
		.into_iter()
		.map(|row| match row.result {
			Ok(result) => result,
			Err(error) => AgentInstallResult {
				agent: row.target.0,
				installed: false,
				error: Some(error),
			},
		})
		.collect();
	Ok(MaterializedMaster {
		agent_results: results,
		created_master,
		created_referrer_dirs,
	})
}

#[cfg(all(test, unix))]
mod nocopy_tests {
	use super::*;
	use crate::skills::linker::Linker;
	use std::fs;
	use tempfile::tempdir;

	// T-NOCOPY (install_fetched): a NeedsLink agent receives a real symlink
	// to the Master, never a copy. Writing a sentinel into the Master AFTER
	// install and reading it back THROUGH the link proves it is a link.
	#[test]
	fn install_fetched_links_master_never_copies() {
		let tmp = tempdir().unwrap();
		let src = tmp.path().join("src/my-skill");
		fs::create_dir_all(&src).unwrap();
		fs::write(
			src.join("SKILL.md"),
			"---\nname: my-skill\ndescription: d\n---\nbody",
		)
		.unwrap();
		let root = tmp.path().canonicalize().unwrap();
		let lock_source = skill::InstallLockSource {
			source: "local/test".to_string(),
			source_type: "local".to_string(),
			source_url: "file:///local/test".to_string(),
			ref_name: None,
		};
		let req = FetchedSkillInstallRequest {
			skill_file: &src.join("SKILL.md"),
			source: &lock_source,
			lock_skill_path: "my-skill/SKILL.md".to_string(),
			ref_commit: None,
			scope: ResourceScope::ProjectOnly,
			project_root: Some(&root),
			target_agents: &[AgentType::Claude],
			expected_name: None,
			target: LinkTarget::Relative,
		};
		let report = install_fetched_skill_and_lock(req).unwrap();
		assert_eq!(report.name, "my-skill");

		let canonical = root.join(".aghub/my-skill");
		let link = root.join(".claude/skills/my-skill");
		assert!(Linker::is_link(&link), "agent dir must hold a link");
		fs::write(canonical.join("sentinel.txt"), "live").unwrap();
		assert_eq!(
			fs::read_to_string(link.join("sentinel.txt")).unwrap(),
			"live",
			"reading through the link must see the Master => not a copy"
		);
	}

	// T-LOCK-PARITY-LINK-VS-COPY: the FULL install-lock entry written by
	// the symlink-only (link-era) path is byte-identical to the copy-era
	// fixture, because both eras hash the SOURCE folder and write the same
	// schema. Pins the round-trip contract (Decision 7) at the FULL-ENTRY
	// level (every field + key order), not just the folder hash.
	#[test]
	fn install_lock_entry_byte_identical_to_copy_era_fixture() {
		let tmp = tempdir().unwrap();
		let root = tmp.path().canonicalize().unwrap();
		// Fixed SKILL.md bytes -> deterministic hash.
		let src = root.join("src/my-skill");
		fs::create_dir_all(&src).unwrap();
		fs::write(
			src.join("SKILL.md"),
			"---\nname: my-skill\ndescription: d\n---\nbody",
		)
		.unwrap();

		// Compute the expected hash from the SOURCE folder (same path
		// both eras use).
		let expected_hash = skill::compute_skill_folder_hash(&src).unwrap();

		let lock_source = skill::InstallLockSource {
			source: "local/test".to_string(),
			source_type: "local".to_string(),
			source_url: "file:///local/test".to_string(),
			ref_name: None,
		};
		let req = FetchedSkillInstallRequest {
			skill_file: &src.join("SKILL.md"),
			source: &lock_source,
			lock_skill_path: "my-skill/SKILL.md".to_string(),
			ref_commit: None,
			scope: ResourceScope::ProjectOnly,
			project_root: Some(&root),
			target_agents: &[AgentType::Claude],
			expected_name: None,
			target: LinkTarget::Relative,
		};
		let report = install_fetched_skill_and_lock(req).unwrap();
		assert!(report.wrote_lock, "lock must be written");

		// Read back the written entry.
		let lock = skill::lock::local::read_local_lock(Some(&root));
		let entry = lock.skills.get("my-skill").expect("entry must exist");
		let got = serde_json::to_value(entry).unwrap();

		// The copy-era fixture: every field the project lock carries.
		// refCommit is absent (None => skip_serializing_if), so NOT in JSON.
		let want = serde_json::json!({
			"source": "local/test",
			"sourceType": "local",
			"skillPath": "my-skill/SKILL.md",
			"computedHash": expected_hash,
		});
		assert_eq!(
			got, want,
			"link-era lock entry must match copy-era byte-for-byte"
		);
	}
}
