use std::path::{Path, PathBuf};

use aghub_agents::models::AgentType;
use skill::sanitize::sanitize_name;

use crate::errors::ConfigError;
use crate::models::{ResourceScope, Skill};
use crate::scope::WriteScope;
use crate::skills::install_fetched::{
	materialize_universal_master, AgentInstallResult, MaterializedMaster,
};
use crate::skills::linker::{LinkTarget, Linker};

/// Request to install a local skill into the .aghub Master store and link target agents.
pub struct LocalSkillInstallRequest<'a> {
	pub source_path: &'a Path,
	pub scope: WriteScope,
	pub target_agents: &'a [AgentType],
	pub install_name: Option<&'a str>,
}

/// Report of a local skill install.
#[derive(Clone, Debug)]
pub struct LocalSkillInstallReport {
	pub skill: Skill,
	pub already_installed: bool,
	pub wrote_master: bool,
	pub wrote_lock: bool,
	pub created_referrer_dirs: Vec<PathBuf>,
	pub agent_results: Vec<AgentInstallResult>,
}

fn write_local_install_lock(
	skill_name: &str,
	scope: &WriteScope,
	source: &skill::InstallLockSource,
	source_dir: &Path,
) -> Result<(), ConfigError> {
	match scope {
		WriteScope::Global => skill::write_global_install_lock(
			skill_name, source, None, source_dir, None,
		)
		.map(|_| ())
		.map_err(ConfigError::SkillLock),
		WriteScope::Project { root } => skill::write_project_install_lock(
			skill_name, source, None, source_dir, root, None,
		)
		.map(|_| ())
		.map_err(ConfigError::SkillLock),
	}
}

fn all_targets_already_linked(
	canonical: &Path,
	safe_name: &str,
	scope: &WriteScope,
	target_agents: &[AgentType],
) -> bool {
	if target_agents.is_empty() || !canonical.exists() {
		return false;
	}
	let master_real = match std::fs::canonicalize(canonical) {
		Ok(p) => p,
		Err(_) => return false,
	};
	for &agent in target_agents {
		let link_need = crate::skills::linker::agent_link_need(
			agent.descriptor(),
			scope.resource_scope(),
			scope.project_root(),
		);
		match link_need {
			crate::skills::linker::LinkNeed::NeedsLink { referrer_dir } => {
				let slot = referrer_dir.join(safe_name);
				if !Linker::is_link(&slot) {
					return false;
				}
				match std::fs::canonicalize(&slot) {
					Ok(target) if target == master_real => {}
					_ => return false,
				}
			}
			crate::skills::linker::LinkNeed::Unsupported => return false,
		}
	}
	true
}

/// Install a skill from a local filesystem path: materializes Master in `.aghub`,
/// links target agents, checks adoption against existing lock/disk, stamps lock,
/// and rolls back on failure. Refuses first if a target agent's config
/// exists but does not parse, and refuses a name a target agent already
/// discovers unless its Referrer already links to this Master.
pub fn install_local_skill(
	req: LocalSkillInstallRequest<'_>,
) -> Result<LocalSkillInstallReport, ConfigError> {
	// Same precondition as the CLI preload: a target agent config that exists but
	// does not parse refuses the install; only a missing config is tolerated.
	// See docs/history/cli.md#malformed-config-is-not-missing
	for &agent in req.target_agents {
		let mut manager = crate::ConfigManager::for_write(
			crate::create_adapter(agent),
			req.scope.clone(),
		);
		match manager.load() {
			Ok(_) | Err(ConfigError::NotFound { .. }) => {}
			Err(ConfigError::Io(e))
				if e.kind() == std::io::ErrorKind::NotFound => {}
			Err(e) => return Err(e),
		}
	}

	let expanded_path =
		crate::skills::removal::expand_tilde_path(req.source_path);
	let skill_pkg = skill::parser::parse(&expanded_path).map_err(|e| {
		ConfigError::InvalidConfig(format!("Failed to parse skill: {e}"))
	})?;

	let source_name = skill_pkg.name.clone();
	let effective_name = req.install_name.unwrap_or(&source_name);
	let safe_name = sanitize_name(effective_name);
	let source_root = crate::skills::skill_source_root(&expanded_path);
	let resource_scope = req.scope.resource_scope();
	let project_root = req.scope.project_root();

	// Request validation (config load, source parse) precedes the guard on purpose: a bad request is not retryable, so it must not report lock contention.
	let _mutation_guard = crate::skills::lock::mutation_guard(
		"install local skill",
		resource_scope,
		project_root,
	)
	.map_err(ConfigError::Io)?;

	let lock_source = skill::InstallLockSource {
		source: expanded_path.display().to_string(),
		source_type: "local".to_string(),
		source_url: expanded_path.display().to_string(),
		ref_name: None,
	};

	let canonical_root = if matches!(resource_scope, ResourceScope::ProjectOnly)
	{
		project_root
	} else {
		None
	};
	let canonical_opt = crate::skills::linker::master_store_dir(canonical_root)
		.map(|skills_dir| skills_dir.join(&safe_name));

	// Under the guard, re-read each target agent's discovered skills: a name the
	// agent already sees (in any folder) is taken. Idempotent only when THIS
	// agent's Referrer slot already links to the intended Master; an explicit
	// name always refuses. Restores the base add_skill_from_path_universal rule.
	for &agent in req.target_agents {
		let mut manager = crate::ConfigManager::for_write(
			crate::create_adapter(agent),
			req.scope.clone(),
		);
		let taken = match manager.load() {
			Ok(config) => {
				config.skills.iter().any(|s| s.name == effective_name)
			}
			Err(ConfigError::NotFound { .. }) => false,
			Err(ConfigError::Io(e))
				if e.kind() == std::io::ErrorKind::NotFound =>
			{
				false
			}
			Err(e) => return Err(e),
		};
		if !taken {
			continue;
		}
		let linked_to_master = canonical_opt.as_deref().is_some_and(|c| {
			all_targets_already_linked(
				c,
				&safe_name,
				&req.scope,
				std::slice::from_ref(&agent),
			)
		});
		if req.install_name.is_some() || !linked_to_master {
			return Err(ConfigError::resource_exists("skill", effective_name));
		}
	}

	let is_already_installed = canonical_opt
		.as_deref()
		.map(|c| {
			all_targets_already_linked(
				c,
				&safe_name,
				&req.scope,
				req.target_agents,
			)
		})
		.unwrap_or(false);

	let adoption = crate::skills::adoption::adoption_guard(
		effective_name,
		&source_root,
		&req.scope,
		&lock_source,
		is_already_installed,
	)?;

	let canonical_dir = adoption.canonical.as_ref().ok_or_else(|| {
		ConfigError::ValidationFailed(format!(
			"Master for skill '{effective_name}' could not be resolved"
		))
	})?;
	if canonical_dir.exists()
		&& req.install_name.is_some()
		&& effective_name != source_name
	{
		return Err(ConfigError::resource_exists("skill", effective_name));
	}
	let master_exists = canonical_dir.exists();
	let master_hash = if master_exists {
		Some(crate::skills::adoption::hash_master(
			effective_name,
			canonical_dir,
		)?)
	} else {
		None
	};
	let is_adoption = adoption.existing_owner.is_none()
		&& master_hash.as_deref() == Some(&adoption.installed_hash);
	let can_write = !master_exists || is_adoption;

	if can_write {
		skill::lock::ensure_locks_writable(
			resource_scope != ResourceScope::ProjectOnly,
			match resource_scope {
				ResourceScope::GlobalOnly => None,
				_ => project_root,
			},
		)
		.map_err(ConfigError::Io)?;
	}

	let link_target = match req.scope {
		WriteScope::Project { .. } => LinkTarget::Relative,
		WriteScope::Global => LinkTarget::Absolute,
	};

	let materialized = materialize_universal_master(
		&source_root,
		&safe_name,
		resource_scope,
		project_root,
		req.target_agents,
		link_target,
	)?;

	let MaterializedMaster {
		agent_results,
		created_master: wrote_master,
		created_referrer_dirs,
	} = materialized;

	for &agent in req.target_agents {
		let link_need = crate::skills::linker::agent_link_need(
			agent.descriptor(),
			resource_scope,
			project_root,
		);
		let agent_res: Vec<_> = agent_results
			.iter()
			.filter(|r| r.agent == agent)
			.cloned()
			.collect();
		if let Err(err) =
			crate::manager::ConfigManager::ensure_single_agent_installed(
				&agent_res,
				&link_need,
				effective_name,
			) {
			crate::skills::rename::rollback_materialized_install(
				effective_name,
				resource_scope,
				project_root,
				&created_referrer_dirs,
				wrote_master,
			);
			return Err(err);
		}
	}

	if wrote_master && effective_name != source_name {
		if let Err(e) = crate::manager::skill::rewrite_master_skill_name(
			canonical_dir,
			effective_name,
		) {
			crate::skills::rename::rollback_materialized_install(
				effective_name,
				resource_scope,
				project_root,
				&created_referrer_dirs,
				true,
			);
			return Err(ConfigError::Io(e));
		}
	}

	// Parse the materialized Master before the lock write, so a Master that
	// does not parse rolls back without leaving a ghost lock entry.
	let installed_skill = match crate::manager::skill::master_on_disk(
		canonical_dir,
		effective_name,
	) {
		Some(found) => found,
		None => {
			crate::skills::rename::rollback_materialized_install(
				effective_name,
				resource_scope,
				project_root,
				&created_referrer_dirs,
				wrote_master,
			);
			return Err(ConfigError::InvalidConfig(format!(
				"the master at '{}' does not parse",
				canonical_dir.display()
			)));
		}
	};

	let covered_any = agent_results.iter().any(|r| r.error.is_none());
	let wrote_lock = wrote_master || (is_adoption && covered_any);

	if wrote_lock {
		crate::skills::adoption::ensure_link_free_master(
			effective_name,
			canonical_dir,
		)?;
		// No re-hash here: a fresh Master is the copier's output of source_root (which drops npx-excluded files), and an adopted Master was already hash-matched above under the same guard.
		let lock_source_dir = if effective_name != source_name {
			canonical_dir.as_path()
		} else {
			&source_root
		};
		if let Err(error) = write_local_install_lock(
			effective_name,
			&req.scope,
			&lock_source,
			lock_source_dir,
		) {
			crate::skills::rename::rollback_materialized_install(
				effective_name,
				resource_scope,
				project_root,
				&created_referrer_dirs,
				wrote_master,
			);
			return Err(error);
		}
	}

	let all_targets_linked = !req.target_agents.is_empty()
		&& agent_results
			.iter()
			.all(|r| r.installed && r.error.is_none());
	let already_installed =
		!wrote_master && created_referrer_dirs.is_empty() && all_targets_linked;

	Ok(LocalSkillInstallReport {
		skill: installed_skill,
		already_installed,
		wrote_master,
		wrote_lock,
		created_referrer_dirs,
		agent_results,
	})
}
