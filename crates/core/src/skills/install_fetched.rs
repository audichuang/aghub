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

use std::path::Path;

use crate::models::ResourceScope;
use crate::skills::linker::classify::{classify_agent, LinkNeed};
use crate::skills::linker::{
	install_universal, universal_canonical_dir, LinkTarget, Linker,
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
	pub wrote_lock: bool,
	/// Content hash of the fetched source folder.
	pub installed_hash: String,
	pub agent_results: Vec<AgentInstallResult>,
}

#[derive(Clone, Debug)]
struct LockedSourceOwner {
	source: String,
	source_type: String,
	source_url: Option<String>,
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
				})
		}),
		ResourceScope::Both => None,
	}
}

/// Canonical host + repo-path identity for the common remote URL forms. The
/// transport and optional `.git` suffix do not define ownership; the host does.
fn remote_owner_from_url(source_url: &str) -> Option<String> {
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

fn write_install_lock(
	skill_name: &str,
	scope: ResourceScope,
	project_root: Option<&Path>,
	source: &skill::InstallLockSource,
	lock_skill_path: String,
	source_dir: &Path,
	ref_commit: Option<String>,
) -> Result<(), crate::ConfigError> {
	match scope {
		ResourceScope::GlobalOnly => skill::write_global_install_lock(
			skill_name,
			source,
			Some(lock_skill_path),
			source_dir,
			ref_commit,
		)
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
	let canonical = universal_canonical_dir(canonical_root)
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

	let (agent_results, wrote_master) = materialize_universal_master(
		&source_root,
		&safe_name,
		req.scope,
		req.project_root,
		req.target_agents,
		req.target,
	)?;

	// Gate ordinary lock rewrites on a fresh Master or fresh Referrer. One
	// additional case is safe: an untracked, byte-identical Master with at least
	// one successfully covered target (including an already-correct Referrer)
	// may be adopted without manufacturing a filesystem change.
	let installed_any = agent_results.iter().any(|r| r.installed);
	let covered_any = agent_results.iter().any(|r| r.error.is_none());
	let wrote_lock = wrote_master
		|| installed_any
		|| (existing_owner.is_none() && covered_any);
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
		write_install_lock(
			&name,
			req.scope,
			req.project_root,
			req.source,
			req.lock_skill_path.clone(),
			&source_root,
			req.ref_commit.clone(),
		)?;
	}

	Ok(FetchedSkillInstallReport {
		name,
		wrote_lock,
		installed_hash,
		agent_results,
	})
}

/// The ONE universal-install materializer shared by every install path: the
/// fetched/desktop install ([`install_fetched_skill_and_lock`]) AND the CLI
/// `aghub add skill` path (`ConfigManager::add_skill_universal` /
/// `add_skill_from_path_universal`). Materializes the `.agents/skills/<name>`
/// Master from `source_root` (copy-free linker; copied only when absent) and
/// links each `NeedsLink` agent.
///
/// Returns the per-agent results plus `wrote_master` — `true` only when the
/// canonical master was NEWLY written on this run. NativeReader agents are
/// reported installed with NO link; NeedsLink agents are linked via the
/// copy-free linker. Unsupported agents reject the whole request before the
/// shared Master write. A per-agent LinkError is folded into that agent's row
/// (Decision 10), never aborting later runtime attempts.
pub fn materialize_universal_master(
	source_root: &Path,
	safe_name: &str,
	scope: ResourceScope,
	project_root: Option<&Path>,
	target_agents: &[AgentType],
	target: LinkTarget,
) -> Result<(Vec<AgentInstallResult>, bool), crate::ConfigError> {
	let canonical_root = if matches!(scope, ResourceScope::ProjectOnly) {
		project_root
	} else {
		None
	};
	let Some(canonical_skills_dir) = universal_canonical_dir(canonical_root)
	else {
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
		return Ok((results, false));
	};
	let canonical = canonical_skills_dir.join(safe_name);
	let master_was_absent = !canonical.exists();

	// Classify every target agent against the canonical SKILLS-DIR (not the
	// SKILL-DIR). `plans[i]` pairs 1:1 with `target_agents[i]`.
	let plans: Vec<(AgentType, LinkNeed)> = target_agents
		.iter()
		.map(|&agent| {
			let descriptor = crate::registry::get(agent);
			(
				agent,
				classify_agent(
					descriptor,
					scope,
					project_root,
					&canonical_skills_dir,
				)
				.need,
			)
		})
		.collect();
	if plans.is_empty() {
		return Ok((Vec::new(), false));
	}

	// One shared setup materializes the Master and performs every needed link.
	// Unsupported is a predictable preflight failure: mixing one with supported
	// targets must never let the shared setup create a Master or an earlier link.
	let mut materialized = false;
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
			let symlink_dirs = plans
				.iter()
				.filter_map(|(_, need)| match need {
					LinkNeed::NeedsLink { agent_skills_dir } => {
						Some(agent_skills_dir.clone())
					}
					_ => None,
				})
				.collect::<Vec<_>>();
			let install = install_universal(
				source_root,
				&canonical,
				&symlink_dirs,
				target,
			)
			.map_err(|error| error.to_string())?;
			materialized = true;

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
			Ok((failed_by_dir, conflict_dirs, linked_dirs))
		},
		|&(agent, ref need), prepared| {
			let result = match need {
				LinkNeed::NativeReader => AgentInstallResult {
					agent,
					installed: true,
					error: None,
				},
				LinkNeed::NeedsLink { agent_skills_dir } => {
					let (failed_by_dir, conflict_dirs, linked_dirs) = prepared;
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
							installed: linked_dirs.contains(agent_skills_dir),
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
	Ok((results, master_was_absent && materialized))
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

		let canonical = root.join(".agents/skills/my-skill");
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
