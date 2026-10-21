use std::path::{Path, PathBuf};

use crate::errors::ConfigError;
use crate::scope::WriteScope;
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
	/// The entry's stored Source hash (`contentHash` / `computedHash`), as
	/// recorded; `None` when the entry has none.
	pub stored_hash: Option<String>,
}

pub fn skill_lock_source(
	skill_name: &str,
	scope: &WriteScope,
) -> Option<LockedSourceOwner> {
	match scope {
		WriteScope::Global => {
			skill::lock::global::get_skill_from_lock(skill_name).map(|entry| {
				LockedSourceOwner {
					source: entry.source,
					source_type: entry.source_type,
					source_url: Some(entry.source_url),
					ref_name: entry.ref_name,
					skill_path: entry.skill_path,
					ref_commit: entry.ref_commit,
					stored_hash: entry.content_hash,
				}
			})
		}
		WriteScope::Project { root } => {
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
					stored_hash: Some(entry.computed_hash.clone()),
				})
		}
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
	scope: &WriteScope,
	source: &skill::InstallLockSource,
	is_already_installed: bool,
	keep_owner: bool,
	fetched: bool,
) -> Result<AdoptionCheck, ConfigError> {
	let safe_name = sanitize_name(name);
	let installed_hash = skill::compute_skill_folder_hash(source_root)
		.map_err(|e| {
			ConfigError::InvalidConfig(format!("Failed to hash skill: {e}"))
		})?;
	let canonical_root = scope.project_root();
	let canonical = master_store_dir(canonical_root)
		.map(|skills_dir| skills_dir.join(&safe_name));
	let existing_owner = skill_lock_source(name, scope);
	if let Some(existing_owner) = existing_owner.as_ref() {
		let is_regranting_canonical = !fetched
			&& canonical
				.as_ref()
				.map(|c| c.as_path() == source_root)
				.unwrap_or(false);
		// `keep_owner`: a same-scope regrant reassigns nothing and writes no
		// lock, so the recorded owner is not compared against `source`.
		if !keep_owner
			&& !is_regranting_canonical
			&& !same_source_owner(existing_owner, source)
		{
			return Err(ConfigError::ValidationFailed(format!(
				"Skill '{name}' is already owned by source '{}:{}'; its \
				 canonical source owner differs from requested source '{}:{}', \
				 so reassignment was refused",
				existing_owner.source_type,
				aghub_git::redact_source_credentials(&existing_owner.source),
				source.source_type,
				aghub_git::redact_source_credentials(&source.source),
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
			if !fetched && skill::parser::parse(canonical).is_err() {
				return Err(ConfigError::InvalidConfig(format!(
					"the master at '{}' does not parse",
					canonical.display()
				)));
			}
			let is_adoption = existing_owner.is_none();
			// `fetched`: the content came from a remote checkout, so an existing
			// Master must match it byte for byte. Local-disk content (an import,
			// or a transfer carrying a remote OWNER) is only compared on
			// adoption: the copier drops caches the raw hash still counts.
			let is_fetched = fetched;
			if is_fetched || (is_adoption && !is_already_installed) {
				let master_hash = hash_master(name, canonical)?;
				if master_hash != installed_hash {
					let kind = if is_fetched { "fetched " } else { "" };
					return Err(ConfigError::ValidationFailed(format!(
						"Pre-existing Master for skill '{name}' has different content; \
						 refusing to adopt it for {kind}source '{}'",
						aghub_git::redact_source_credentials(&source.source),
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

#[cfg(test)]
mod tests {
	use super::*;

	fn owner(
		source: &str,
		source_type: &str,
		url: Option<&str>,
	) -> LockedSourceOwner {
		LockedSourceOwner {
			source: source.to_string(),
			source_type: source_type.to_string(),
			source_url: url.map(str::to_string),
			ref_name: None,
			skill_path: None,
			ref_commit: None,
			stored_hash: None,
		}
	}

	fn requested(
		source: &str,
		source_type: &str,
		url: &str,
	) -> skill::InstallLockSource {
		skill::InstallLockSource {
			source: source.to_string(),
			source_type: source_type.to_string(),
			source_url: url.to_string(),
			ref_name: None,
		}
	}

	/// A URL-less `gitlab` entry does not name gitlab.com: npx 1.5.11 records
	/// any self-hosted `/-/tree/` host as `gitlab` with the host stripped. So a
	/// gitlab.com claimant is refused, as is a URL-less entry against a full
	/// URL in either direction.
	#[test]
	fn a_url_less_gitlab_entry_refuses_a_full_gitlab_com_url() {
		assert!(!same_source_owner(
			&owner("group/repo", "gitlab", None),
			&requested(
				"group/repo",
				"gitlab",
				"https://gitlab.com/group/repo.git"
			),
		));
		assert!(!same_source_owner(
			&owner("o/r", "gitlab", Some("https://gitlab.com/o/r.git")),
			&requested("o/r", "gitlab", ""),
		));
	}

	/// Two URL-less non-github entries with the same spelling may still be two
	/// self-hosted hosts: refused. Transfer asks this of an ambiguous owner it
	/// copies untracked, so such a copy onto a same-spelled entry is refused.
	#[test]
	fn url_less_gitlab_entries_with_one_spelling_are_not_one_owner() {
		assert!(!same_source_owner(
			&owner("o/r", "gitlab", None),
			&requested("o/r", "gitlab", ""),
		));
	}

	/// `gitlab.com/team/repo` is a namespace literally named `gitlab.com`
	/// (https://gitlab.com/gitlab.com/team/repo.git), not `team/repo` with its
	/// host spelled out — rebuilding coordinates conflated the two.
	#[test]
	fn a_dotted_gitlab_namespace_is_not_its_host_stripped_twin() {
		let dotted = owner("gitlab.com/team/repo", "gitlab", None);
		assert!(!same_source_owner(
			&dotted,
			&requested(
				"team/repo",
				"gitlab",
				"https://gitlab.com/team/repo.git"
			),
		));
		assert!(!same_source_owner(
			&dotted,
			&requested("team/repo", "gitlab", "")
		));
	}

	/// ...but never a different repository.
	#[test]
	fn a_url_less_gitlab_entry_refuses_another_repo() {
		assert!(!same_source_owner(
			&owner("o/r", "gitlab", None),
			&requested("o/other", "gitlab", "https://gitlab.com/o/other.git"),
		));
	}

	/// A hostless custom remote without a URL has lost its host: fail closed,
	/// even against the same spelling or a claimant naming a host.
	#[test]
	fn a_url_less_custom_remote_still_fails_closed() {
		assert!(!same_source_owner(
			&owner("o/r", "git", None),
			&requested("o/r", "git", ""),
		));
		assert!(!same_source_owner(
			&owner("o/r", "git", None),
			&requested("o/r", "git", "https://git.example.com/o/r.git"),
		));
	}
}
