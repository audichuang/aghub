//! No-network install of an ALREADY-FETCHED skill.
//!
//! This is the shared primitive behind both the API git-install route and the
//! CLI `source sync` command: given a skill that has already been fetched into a
//! local tree, install it into the resolved per-agent skills directories — in the
//! universal `.agents/skills` layout — and write the install lock. It performs NO
//! network and NO credential resolution; fetch + auth live in the caller.
//!
//! It returns PER-AGENT results so the API can rebuild its current per-agent
//! success / invalid-agent response and the CLI can report which agents got the
//! skill. An agent whose target skills dir cannot be resolved (`Unsupported`)
//! is a PREDICTABLE failure: the shared multi-target preflight rejects the
//! whole batch as a hard error and nothing is written for ANY agent. Soft
//! failures (`installed: false`, `error: Some(..)`) are reserved for runtime
//! link failures on targets that passed preflight.

use std::path::{Path, PathBuf};

use crate::models::ResourceScope;
use crate::skills::linker::classify::{classify_agent, LinkNeed};
use crate::skills::linker::{
	install_universal, master_store_dir, LinkTarget, Linker,
};
use crate::skills::skill_source_root;
use crate::skills::update::{detect_rename, skill_renamed_message};
use aghub_agents::models::AgentType;
use skill::sanitize::sanitize_name;

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

#[derive(Clone, Debug)]
struct LockedSourceOwner {
	source: String,
	source_type: String,
	source_url: Option<String>,
	/// Update coordinates already recorded for this skill. Compared against the
	/// request so a same-owner re-install with a different ref / path / commit
	/// heals them instead of leaving the lock pinned to the old ones.
	ref_name: Option<String>,
	skill_path: Option<String>,
	ref_commit: Option<String>,
}

fn skill_lock_source(
	skill_name: &str,
	scope: ResourceScope,
	project_root: Option<&Path>,
) -> Option<LockedSourceOwner> {
	match scope {
		ResourceScope::GlobalOnly => {
			skill::lock::global::get_skill_from_lock(skill_name).map(|entry| {
				LockedSourceOwner {
					source: entry.source,
					source_type: entry.source_type,
					source_url: Some(entry.source_url),
					ref_name: entry.ref_name,
					skill_path: entry.skill_path,
					ref_commit: entry.ref_commit,
				}
			})
		}
		ResourceScope::ProjectOnly => project_root.and_then(|root| {
			skill::lock::local::read_local_lock(Some(root))
				.skills
				.get(skill_name)
				.map(|entry| LockedSourceOwner {
					source: entry.source.clone(),
					source_type: entry.source_type.clone(),
					source_url: entry.source_url.clone(),
					ref_name: entry.ref_name.clone(),
					skill_path: entry.skill_path.clone(),
					ref_commit: entry.ref_commit.clone(),
				})
		}),
		ResourceScope::Both => None,
	}
}

/// Canonical host + repo-path identity for the common remote URL forms. The
/// transport and optional `.git` suffix do not define ownership; the host does.
///
/// This is the low-level normalizer shared by three callers, not their complete
/// identity policy. The install-time owner check passes recorded clone URLs and
/// falls back to literal equality when either side cannot be normalized.
/// [`crate::skills::lock::EntryIdentity`] wraps it in `comparable_remote`, which
/// additionally recognizes GitHub `owner/repo` shorthand but deliberately
/// returns `None` for a TFS collection path and for a bare origin-shaped string.
/// `skill_update::sources` first uses the recorded provider or restores a
/// fetchable URL, so its `source_origin` can resolve those origin-shaped strings.
/// Keeping the transport parser here prevents those caller-specific fallbacks
/// from silently widening install ownership or `EntryIdentity` comparisons.
pub fn remote_owner_from_url(source_url: &str) -> Option<String> {
	let source_url = source_url.trim();
	if source_url.is_empty() || source_url.starts_with("file:") {
		return None;
	}

	let (authority, path) =
		if let Some((scheme, rest)) = source_url.split_once("://") {
			if !matches!(
				scheme.to_ascii_lowercase().as_str(),
				"http" | "https" | "ssh" | "git"
			) {
				return None;
			}
			let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
			(authority, path)
		} else {
			// SCP-like Git URL: `[user@]host:path/to/repo.git`.
			let host_and_path = source_url
				.rsplit_once('@')
				.map_or(source_url, |(_, value)| value);
			host_and_path.split_once(':')?
		};
	let authority = authority
		.rsplit_once('@')
		.map_or(authority, |(_, value)| value)
		.to_ascii_lowercase();
	let path = path
		.split(['?', '#'])
		.next()
		.unwrap_or(path)
		.trim_matches('/');
	let path = path
		.strip_suffix(".git")
		.unwrap_or(path)
		.trim_end_matches('/');
	(!authority.is_empty() && !path.is_empty())
		.then(|| format!("{authority}/{path}"))
}

fn same_source_owner(
	existing: &LockedSourceOwner,
	requested: &skill::InstallLockSource,
) -> bool {
	if !existing
		.source_type
		.eq_ignore_ascii_case(&requested.source_type)
	{
		return false;
	}
	match existing
		.source_url
		.as_deref()
		.filter(|source_url| !source_url.trim().is_empty())
	{
		Some(source_url) => match (
			remote_owner_from_url(source_url),
			remote_owner_from_url(&requested.source_url),
		) {
			(Some(existing), Some(requested)) => existing == requested,
			_ => source_url.trim() == requested.source_url.trim(),
		},
		// Project GitHub and local locks intentionally omit a reconstructable
		// sourceUrl, and their provider gives the missing identity. A legacy
		// non-GitHub remote without sourceUrl has lost its host; fail closed rather
		// than let an arbitrary host with the same owner/repo claim it.
		None => {
			matches!(
				existing.source_type.to_ascii_lowercase().as_str(),
				"github" | "local"
			) && existing.source == requested.source
		}
	}
}

/// Whether a same-owner lock's update coordinates disagree with what this
/// install was asked for. Healing them keeps a later `source sync --update`
/// following the ref the caller actually requested, instead of whatever the
/// lock happened to be written with first — an idempotent re-install writes no
/// file, so without this the stale coordinates would survive silently.
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

fn hash_master(
	skill_name: &str,
	canonical: &Path,
) -> Result<String, crate::ConfigError> {
	skill::compute_skill_folder_hash(canonical).map_err(|error| {
		crate::ConfigError::ValidationFailed(format!(
			"Master for skill '{skill_name}' could not be verified: {error}",
		))
	})
}

/// Inspect a Master with lstat semantics and never descend through a link or
/// Windows reparse point. Hashing intentionally skips links for npx parity, so
/// provenance adoption needs this separate invariant: every byte reachable
/// through the adopted Master must come from its real directory tree.
fn ensure_link_free_master(
	skill_name: &str,
	canonical: &Path,
) -> Result<(), crate::ConfigError> {
	let mut pending = vec![canonical.to_path_buf()];
	while let Some(path) = pending.pop() {
		let metadata = std::fs::symlink_metadata(&path).map_err(|error| {
			crate::ConfigError::ValidationFailed(format!(
				"Master for skill '{skill_name}' could not be inspected for links: \
				 {error}",
			))
		})?;
		if metadata.file_type().is_symlink() || Linker::is_link(&path) {
			return Err(crate::ConfigError::ValidationFailed(format!(
				"Master for skill '{skill_name}' contains a link or junction; \
				 refusing to adopt it",
			)));
		}
		if metadata.is_dir() {
			let entries = std::fs::read_dir(&path).map_err(|error| {
				crate::ConfigError::ValidationFailed(format!(
					"Master for skill '{skill_name}' could not be inspected for \
					 links: {error}",
				))
			})?;
			for entry in entries {
				let entry = entry.map_err(|error| {
					crate::ConfigError::ValidationFailed(format!(
						"Master for skill '{skill_name}' could not be inspected for \
						 links: {error}",
					))
				})?;
				pending.push(entry.path());
			}
		}
	}
	Ok(())
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

	// Hold the interprocess mutation lock from the FIRST state read (the
	// ownership / Master-hash guards below) through the lock write, so no other
	// aghub process can invalidate a guard between checking it and acting on it.
	let _mutation_guard = crate::skills::lock::mutation_guard(
		"install skill",
		req.scope,
		req.project_root,
	)
	.map_err(crate::ConfigError::Io)?;

	let source_root = skill_source_root(req.skill_file);
	let safe_name = sanitize_name(&name);
	let installed_hash = skill::compute_skill_folder_hash(&source_root)
		.map_err(|e| {
			crate::ConfigError::InvalidConfig(format!(
				"Failed to hash fetched skill: {e}"
			))
		})?;
	let canonical_root = if matches!(req.scope, ResourceScope::ProjectOnly) {
		req.project_root
	} else {
		None
	};
	let canonical = master_store_dir(canonical_root)
		.map(|skills_dir| skills_dir.join(&safe_name));
	let existing_owner = skill_lock_source(&name, req.scope, req.project_root);
	if let Some(existing_owner) = existing_owner.as_ref() {
		if !same_source_owner(existing_owner, req.source) {
			return Err(crate::ConfigError::ValidationFailed(format!(
				"Skill '{name}' is already owned by source '{}:{}'; its \
				 canonical source owner differs from requested source '{}:{}', \
				 so reassignment was refused",
				existing_owner.source_type,
				existing_owner.source,
				req.source.source_type,
				req.source.source,
			)));
		}
	}
	if let Some(canonical) = canonical.as_ref() {
		if Linker::is_link(canonical) {
			return Err(crate::ConfigError::ValidationFailed(format!(
				"Master slot for skill '{name}' is a link; refusing to follow or \
				 adopt it",
			)));
		}
		if canonical.exists() {
			ensure_link_free_master(&name, canonical)?;
			let master_hash = hash_master(&name, canonical)?;
			if master_hash != installed_hash {
				return Err(crate::ConfigError::ValidationFailed(format!(
					"Pre-existing Master for skill '{name}' has different content; \
					 refusing to adopt it for fetched source '{}'",
					req.source.source,
				)));
			}
		}
	}

	// LAST preflight before the first write. The lock is written at the END of
	// this flow, but a modify funnel that refuses on an unreadable lock would
	// do so AFTER the Master and Referrers exist — and `?` there returns
	// without consuming the materialization receipt, leaving an untracked
	// partial install. Refuse now, while there is still nothing to roll back.
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

	// Gate ordinary lock rewrites on a fresh Master or fresh Referrer. One
	// additional case is safe: an untracked, byte-identical Master with at least
	// one successfully covered target (including an already-correct Referrer)
	// may be adopted without manufacturing a filesystem change.
	// The lock-write signal must come from the CREATION receipt, not from
	// "can this agent read it". Those diverged the moment `installed` started
	// folding in `already_linked` — which it had to, or the second through
	// eighth agent of a shared slot reported `installed: false` with no error on
	// a first install. Keying the lock write on readability instead made an
	// idempotent re-run rewrite the lock, because every already-correct link
	// reports readable.
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
		// Rewriting an existing entry must not DROP a coordinate this request
		// omits (a relink rewrites the entry with no commit of its own, and
		// erasing `ref_commit` changes update preflight). But a recorded commit
		// certifies ONE (ref, skillPath) pair: carry it over only while both
		// still match, else leave it None so preflight cannot treat coordinates
		// nothing has verified as already proven.
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
		// Roll back from THIS call's receipt when the lock write fails. The
		// preflight above rejects a lock we cannot parse, but it cannot rule
		// out a late I/O failure (unwritable parent, full disk) or a foreign
		// writer corrupting the file in between — the mutation lock serializes
		// aghub against aghub only. Returning `?` here instead would leave the
		// Master and Referrers this call just created with no lock entry
		// pointing at them: an untracked install the caller was told failed.
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
	/// from the linker's own `linked` set -- never reconstructed from
	/// `installed` or from read-path order, which would wrongly attribute a
	/// NativeReader row (installed, no link, first read path IS the Master).
	pub created_referrer_dirs: Vec<PathBuf>,
}

/// The ONE universal-install materializer shared by every install path: the
/// fetched/desktop install ([`install_fetched_skill_and_lock`]) AND the CLI
/// `aghub add skill` path (`ConfigManager::add_skill_universal` /
/// `add_skill_from_path_universal`). Materializes the `.agents/skills/<name>`
/// Master from `source_root` (copy-free linker; copied only when absent) and
/// links each `NeedsLink` agent.
///
/// NativeReader agents are reported installed with NO link; NeedsLink agents are
/// linked via the copy-free linker. Unsupported agents reject the whole request
/// before the shared Master write. A per-agent LinkError is folded into that
/// agent's row (Decision 10), never aborting later runtime attempts.
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
					"Cannot resolve .agents canonical directory".to_string(),
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
			// Dedup: up to eight agents resolve to the SAME shared
			// `.agents/skills`, and asking the linker to create one link eight
			// times makes seven of them report `AlreadyLinked` against work this
			// very call just did.
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
			// Attribution is a different question from rollback. An agent whose
			// slot was ALREADY correctly linked is installed — it can read the
			// skill. Folding these in became load-bearing the moment
			// `NativeReader` was deleted: eight agents now share one
			// `.agents/skills` slot, so one is `Linked` and seven are
			// `AlreadyLinked`, and reporting those seven as
			// `installed: false, error: None` is a first-install failure with no
			// error attached.
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
