use std::path::{Path, PathBuf};

use crate::errors::ConfigError;
use crate::models::ResourceScope;
use crate::skills::linker::{master_store_dir, Linker};
use skill::sanitize::sanitize_name;

#[derive(Clone, Debug)]
pub struct LockedSourceOwner {
	pub source: String,
	pub source_type: String,
	pub source_url: Option<String>,
	pub ref_name: Option<String>,
	pub skill_path: Option<String>,
	pub ref_commit: Option<String>,
}

pub fn skill_lock_source(
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

pub fn same_source_owner(
	existing: &LockedSourceOwner,
	requested: &skill::InstallLockSource,
) -> bool {
	if !existing
		.source_type
		.eq_ignore_ascii_case(&requested.source_type)
	{
		return false;
	}
	if existing.source_type.eq_ignore_ascii_case("local") {
		return true;
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
		None => {
			matches!(
				existing.source_type.to_ascii_lowercase().as_str(),
				"github"
			) && existing.source == requested.source
		}
	}
}

pub fn hash_master(
	skill_name: &str,
	canonical: &Path,
) -> Result<String, ConfigError> {
	skill::compute_skill_folder_hash(canonical).map_err(|error| {
		ConfigError::ValidationFailed(format!(
			"Master for skill '{skill_name}' could not be verified: {error}",
		))
	})
}

pub fn ensure_link_free_master(
	skill_name: &str,
	canonical: &Path,
) -> Result<(), ConfigError> {
	let mut pending = vec![canonical.to_path_buf()];
	while let Some(path) = pending.pop() {
		let metadata = std::fs::symlink_metadata(&path).map_err(|error| {
			ConfigError::ValidationFailed(format!(
				"Master for skill '{skill_name}' could not be inspected for links: \
				 {error}",
			))
		})?;
		if metadata.file_type().is_symlink() || Linker::is_link(&path) {
			return Err(ConfigError::ValidationFailed(format!(
				"Master for skill '{skill_name}' contains a link or junction; \
				 refusing to adopt it",
			)));
		}
		if metadata.is_dir() {
			let entries = std::fs::read_dir(&path).map_err(|error| {
				ConfigError::ValidationFailed(format!(
					"Master for skill '{skill_name}' could not be inspected for \
					 links: {error}",
				))
			})?;
			for entry in entries {
				let entry = entry.map_err(|error| {
					ConfigError::ValidationFailed(format!(
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

pub struct AdoptionCheck {
	pub source_root: PathBuf,
	pub safe_name: String,
	pub installed_hash: String,
	pub canonical: Option<PathBuf>,
	pub existing_owner: Option<LockedSourceOwner>,
}

pub fn adoption_guard(
	name: &str,
	source_root: &Path,
	scope: ResourceScope,
	project_root: Option<&Path>,
	source: &skill::InstallLockSource,
) -> Result<AdoptionCheck, ConfigError> {
	let safe_name = sanitize_name(name);
	let installed_hash = skill::compute_skill_folder_hash(source_root)
		.map_err(|e| {
			ConfigError::InvalidConfig(format!("Failed to hash skill: {e}"))
		})?;
	let canonical_root = if matches!(scope, ResourceScope::ProjectOnly) {
		project_root
	} else {
		None
	};
	let canonical = master_store_dir(canonical_root)
		.map(|skills_dir| skills_dir.join(&safe_name));
	let existing_owner = skill_lock_source(name, scope, project_root);
	if let Some(existing_owner) = existing_owner.as_ref() {
		let is_regranting_canonical = canonical
			.as_ref()
			.map(|c| c.as_path() == source_root)
			.unwrap_or(false);
		if !is_regranting_canonical
			&& !same_source_owner(existing_owner, source)
		{
			return Err(ConfigError::ValidationFailed(format!(
				"Skill '{name}' is already owned by source '{}:{}'; its \
				 canonical source owner differs from requested source '{}:{}', \
				 so reassignment was refused",
				existing_owner.source_type,
				existing_owner.source,
				source.source_type,
				source.source,
			)));
		}
	}
	if let Some(canonical) = canonical.as_ref() {
		if Linker::is_link(canonical) {
			return Err(ConfigError::ValidationFailed(format!(
				"Master slot for skill '{name}' is a link; refusing to follow or \
				 adopt it",
			)));
		}
		if canonical.exists() {
			ensure_link_free_master(name, canonical)?;
			if skill::parser::parse(canonical).is_err() {
				return Err(ConfigError::InvalidConfig(format!(
					"the master at '{}' does not parse",
					canonical.display()
				)));
			}
			let is_adoption = existing_owner.is_none();
			let is_fetched = source.source_type != "local";
			if is_adoption || is_fetched {
				let master_hash = hash_master(name, canonical)?;
				if master_hash != installed_hash {
					let kind = if is_fetched { "fetched " } else { "" };
					return Err(ConfigError::ValidationFailed(format!(
						"Pre-existing Master for skill '{name}' has different content; \
						 refusing to adopt it for {kind}source '{}'",
						source.source,
					)));
				}
			}
		}
	}
	Ok(AdoptionCheck {
		source_root: source_root.to_path_buf(),
		safe_name,
		installed_hash,
		canonical,
		existing_owner,
	})
}
