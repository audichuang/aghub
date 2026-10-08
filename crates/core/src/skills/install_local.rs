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
		.map_err(ConfigError::Io),
		WriteScope::Project { root } => skill::write_project_install_lock(
			skill_name, source, None, source_dir, root, None,
		)
		.map(|_| ())
		.map_err(ConfigError::Io),
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
/// and rolls back on failure.
pub fn install_local_skill(
	req: LocalSkillInstallRequest<'_>,
) -> Result<LocalSkillInstallReport, ConfigError> {
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

	if let Some(canonical) = adoption.canonical.as_ref() {
		if canonical.exists()
			&& req.install_name.is_some()
			&& effective_name != source_name
		{
			return Err(ConfigError::resource_exists("skill", effective_name));
		}
	}

	let canonical_dir = adoption.canonical.as_ref().ok_or_else(|| {
		ConfigError::ValidationFailed(format!(
			"Master for skill '{effective_name}' could not be resolved"
		))
	})?;
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
		let canonical_dir = adoption.canonical.as_ref().ok_or_else(|| {
			ConfigError::ValidationFailed(format!(
				"Master for skill '{effective_name}' could not be resolved"
			))
		})?;
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

	let covered_any = agent_results.iter().any(|r| r.error.is_none());
	let wrote_lock = wrote_master || (is_adoption && covered_any);

	if wrote_lock {
		let canonical = adoption.canonical.as_ref().ok_or_else(|| {
			ConfigError::ValidationFailed(format!(
				"Master for skill '{effective_name}' could not be resolved before the \
				 source lock write; the lock was not written"
			))
		})?;
		crate::skills::adoption::ensure_link_free_master(
			effective_name,
			canonical,
		)?;
		let master_hash =
			crate::skills::adoption::hash_master(effective_name, canonical)?;
		if effective_name == source_name
			&& master_hash != adoption.installed_hash
		{
			crate::skills::rename::rollback_materialized_install(
				effective_name,
				resource_scope,
				project_root,
				&created_referrer_dirs,
				wrote_master,
			);
			return Err(ConfigError::ValidationFailed(format!(
				"Master for skill '{effective_name}' does not match the source \
				 content before the source lock write; the lock was not written"
			)));
		}
		let lock_source_dir = if effective_name != source_name {
			canonical.as_path()
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

	let canonical_dir = adoption.canonical.as_ref().ok_or_else(|| {
		ConfigError::ValidationFailed(format!(
			"Master for skill '{effective_name}' could not be resolved"
		))
	})?;
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
