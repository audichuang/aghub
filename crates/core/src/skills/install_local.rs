use std::path::{Path, PathBuf};

use aghub_agents::models::AgentType;
use skill::sanitize::sanitize_name;

use crate::errors::ConfigError;
use crate::models::{ResourceScope, Skill};
use crate::scope::WriteScope;
use crate::skills::install_fetched::{
	materialize_universal_master, AgentInstallResult, MaterializedMaster,
};
use crate::skills::linker::LinkTarget;

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
	scope: ResourceScope,
	project_root: Option<&Path>,
	source: &skill::InstallLockSource,
	source_dir: &Path,
) -> Result<(), ConfigError> {
	match scope {
		ResourceScope::GlobalOnly => skill::write_global_install_lock(
			skill_name, source, None, source_dir, None,
		)
		.map(|_| ())
		.map_err(ConfigError::Io),
		ResourceScope::ProjectOnly => {
			let cwd = project_root.ok_or_else(|| {
				ConfigError::InvalidConfig(
					"project root is required for project skill installs"
						.to_string(),
				)
			})?;
			skill::write_project_install_lock(
				skill_name, source, None, source_dir, cwd, None,
			)
			.map(|_| ())
			.map_err(ConfigError::Io)
		}
		ResourceScope::Both => Err(ConfigError::InvalidConfig(
			"Combined skill scope is not supported for installs".to_string(),
		)),
	}
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

	let adoption = crate::skills::adoption::adoption_guard(
		effective_name,
		&source_root,
		resource_scope,
		project_root,
		&lock_source,
	)?;

	if let Some(canonical) = adoption.canonical.as_ref() {
		if canonical.exists()
			&& req.install_name.is_some()
			&& effective_name != source_name
		{
			return Err(ConfigError::resource_exists("skill", effective_name));
		}
	}

	skill::lock::ensure_locks_writable(
		resource_scope != ResourceScope::ProjectOnly,
		match resource_scope {
			ResourceScope::GlobalOnly => None,
			_ => project_root,
		},
	)
	.map_err(ConfigError::Io)?;

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

	let is_adoption = adoption.existing_owner.is_none() && !wrote_master;
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
			resource_scope,
			project_root,
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

	let already_installed = !wrote_master && created_referrer_dirs.is_empty();

	Ok(LocalSkillInstallReport {
		skill: installed_skill,
		already_installed,
		wrote_master,
		wrote_lock,
		created_referrer_dirs,
		agent_results,
	})
}
