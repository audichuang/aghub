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
//! skill. An agent whose target skills dir cannot be resolved is reported as a
//! soft failure (`installed: false`, `error: Some(..)`), NOT a hard error.

use std::path::Path;

use crate::models::ResourceScope;
use crate::skills::linker::classify::{classify_agent, LinkNeed};
use crate::skills::linker::{
	install_universal, universal_canonical_dir, LinkTarget,
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
	/// a fresh universal link). `false` for a soft skip (already present, or no
	/// resolvable skills dir — see `error`).
	pub installed: bool,
	pub error: Option<String>,
}

/// Inputs for [`install_fetched_skill_and_lock`].
pub struct FetchedSkillInstallRequest<'a> {
	/// `SKILL.md` inside the already-fetched tree (or its parent dir).
	pub skill_file: &'a Path,
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

/// Whether the lock should be (re)written for this scope.
///
/// Mirrors the API's `should_write_install_lock`
/// (`crates/api/src/routes/skills.rs`): write when at least one agent actually
/// received the skill on this call, OR there is no existing lock entry yet.
fn should_write_install_lock(
	skill_name: &str,
	installed_any: bool,
	scope: ResourceScope,
	project_root: Option<&Path>,
) -> bool {
	installed_any || !skill_lock_contains(skill_name, scope, project_root)
}

fn skill_lock_contains(
	skill_name: &str,
	scope: ResourceScope,
	project_root: Option<&Path>,
) -> bool {
	match scope {
		ResourceScope::GlobalOnly => {
			skill::lock::global::get_skill_from_lock(skill_name).is_some()
		}
		ResourceScope::ProjectOnly => project_root.is_some_and(|root| {
			skill::lock::local::read_local_lock(Some(root))
				.skills
				.contains_key(skill_name)
		}),
		ResourceScope::Both => false,
	}
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

	let (agent_results, wrote_master) = install_universal_layout(
		&source_root,
		&safe_name,
		req.scope,
		req.project_root,
		req.target_agents,
		req.target,
	)?;

	// Gate the lock write on the master being freshly written OR at least one
	// agent actually receiving the skill on THIS run (Decision 11).
	// NOTE: the gate passes (wrote_master||installed_any) as the helper's
	// `installed_any` arg; when the outer guard is false (both false) the
	// `&&` short-circuits before calling the helper, so
	// `skill_lock_contains` is never reached — that branch is dead here.
	let installed_any = agent_results.iter().any(|r| r.installed);
	let wrote_lock = (wrote_master || installed_any)
		&& should_write_install_lock(
			&name,
			wrote_master || installed_any,
			req.scope,
			req.project_root,
		);
	if wrote_lock {
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

/// Returns the per-agent results plus `wrote_master` — `true` only when the
/// canonical master was NEWLY written on this run. NativeReader agents are
/// reported installed with NO link; NeedsLink agents are linked via the
/// copy-free linker; Unsupported agents soft-fail. A per-agent LinkError is
/// folded into that agent's row (Decision 10), never aborting the install.
fn install_universal_layout(
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
	let wrote_master = !canonical.exists();

	// Classify every target agent against the canonical SKILLS-DIR (not the
	// SKILL-DIR). `plans[i]` pairs 1:1 with `target_agents[i]`.
	let plans: Vec<LinkNeed> = target_agents
		.iter()
		.map(|&agent| {
			let descriptor = crate::registry::get(agent);
			classify_agent(
				descriptor,
				scope,
				project_root,
				&canonical_skills_dir,
			)
			.need
		})
		.collect();
	let symlink_dirs: Vec<std::path::PathBuf> = plans
		.iter()
		.filter_map(|need| match need {
			LinkNeed::NeedsLink { agent_skills_dir } => {
				Some(agent_skills_dir.clone())
			}
			_ => None,
		})
		.collect();

	// Copy-free install: materialize the Master (if absent) and link each
	// NeedsLink agent. A hard Master-copy failure (e.g. ENOTDIR on the
	// canonical parent) is converted to per-agent failures rather than
	// propagated as Err; callers always get Ok with per-agent results.
	let report =
		match install_universal(source_root, &canonical, &symlink_dirs, target)
		{
			Ok(r) => r,
			Err(e) => {
				let msg = e.to_string();
				let results = target_agents
					.iter()
					.map(|&agent| AgentInstallResult {
						agent,
						installed: false,
						error: Some(msg.clone()),
					})
					.collect();
				return Ok((results, false));
			}
		};
	// Per-agent link errors keyed by the agent's skills-dir (the link parent).
	let failed_by_dir: std::collections::HashMap<std::path::PathBuf, String> =
		report
			.failed
			.iter()
			.filter_map(|(link, err)| {
				link.parent().map(|p| (p.to_path_buf(), err.to_string()))
			})
			.collect();
	// P1-D: a conflict (an occupied real dir, or a foreign link in the agent's
	// skills-dir) is NOT a successful install — it was never clobbered. Fold
	// report.conflicts by the agent skills-dir too, so a NeedsLink agent whose
	// slot is occupied is reported `installed:false` with an error, never a
	// silent `installed:true`.
	let conflict_dirs: std::collections::HashSet<std::path::PathBuf> = report
		.conflicts
		.iter()
		.filter_map(|link| link.parent().map(|p| p.to_path_buf()))
		.collect();
	let linked_dirs: std::collections::HashSet<std::path::PathBuf> = report
		.linked
		.iter()
		.filter_map(|link| link.parent().map(|p| p.to_path_buf()))
		.collect();

	let results = target_agents
		.iter()
		.zip(plans.iter())
		.map(|(&agent, need)| match need {
			LinkNeed::NativeReader => AgentInstallResult {
				agent,
				installed: true,
				error: None,
			},
			LinkNeed::NeedsLink { agent_skills_dir } => {
				if let Some(msg) = failed_by_dir.get(agent_skills_dir) {
					AgentInstallResult {
						agent,
						installed: false,
						error: Some(msg.clone()),
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
					// Fresh link in `report.linked` -> installed:true.
					// Correct existing link in `report.already_linked` ->
					// installed:false (idempotent, not a new install).
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
		})
		.collect();

	Ok((results, wrote_master))
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
