use crate::{
	lock::{global, local},
	parser, scan, SkillLockEntry,
};
use std::path::{Path, PathBuf};
use thiserror::Error;

pub const EMPTY_SKILLS_LOCK_DIGEST: &str =
	"e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoDiscoveredSkill {
	pub name: String,
	pub full_path: PathBuf,
	pub relative_dir: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RepoSkillSelectionError {
	#[error("No skills found in source repository")]
	NoSkillsFound,
	#[error(
		"Requested skills not found: {missing}. Available skills: {available}"
	)]
	SkillsNotFound { missing: String, available: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallLockSource {
	pub source: String,
	pub source_type: String,
	pub source_url: String,
	pub ref_name: Option<String>,
}

#[derive(Debug, Error)]
pub enum RepoDiscoveryError {
	#[error("No skills found in source repository")]
	NoSkillsFound,
	#[error(
		"Requested skills not found: {missing}. Available skills: {available}"
	)]
	SkillsNotFound { missing: String, available: String },
	#[error("Failed to scan repository for skills: {0:?}")]
	Scan(#[from] scan::ScanError),
	#[error(
		"Failed to determine repo-relative skill path '{path}' from '{root}'"
	)]
	RelativePath { path: PathBuf, root: PathBuf },
}

impl From<RepoSkillSelectionError> for RepoDiscoveryError {
	fn from(error: RepoSkillSelectionError) -> Self {
		match error {
			RepoSkillSelectionError::NoSkillsFound => Self::NoSkillsFound,
			RepoSkillSelectionError::SkillsNotFound { missing, available } => {
				Self::SkillsNotFound { missing, available }
			}
		}
	}
}

fn normalize_relative_repo_dir(
	repo_root: &Path,
	skill_path: &Path,
) -> Result<String, RepoDiscoveryError> {
	let relative = skill_path.strip_prefix(repo_root).map_err(|_| {
		RepoDiscoveryError::RelativePath {
			path: skill_path.to_path_buf(),
			root: repo_root.to_path_buf(),
		}
	})?;
	let value = relative.to_string_lossy().replace('\\', "/");
	if value == "." {
		Ok(String::new())
	} else {
		Ok(value)
	}
}

pub fn lock_skill_file_path(relative_dir: &str) -> String {
	if relative_dir.is_empty() {
		"SKILL.md".to_string()
	} else {
		format!("{relative_dir}/SKILL.md")
	}
}

pub fn discover_repo_skills(
	repo_root: &Path,
	requested_skills: &[String],
	install_all: bool,
) -> Result<Vec<RepoDiscoveredSkill>, RepoDiscoveryError> {
	let scan_options = scan::ScanOptions {
		max_depth: 10,
		full_depth: true,
		respect_gitignore: true,
	};
	let paths = scan::scan_skills(repo_root, scan_options, vec![])?;

	let mut discovered = Vec::new();
	for path in paths {
		let parsed = match parser::parse(&path) {
			Ok(parsed) => parsed,
			Err(_) => continue,
		};
		discovered.push(RepoDiscoveredSkill {
			name: parsed.name,
			relative_dir: normalize_relative_repo_dir(repo_root, &path)?,
			full_path: path,
		});
	}

	Ok(select_repo_skills(
		&discovered,
		requested_skills,
		install_all,
		|skill| skill.name.as_str(),
	)?
	.into_iter()
	.cloned()
	.collect())
}

pub fn select_repo_skills<'a, T, F>(
	discovered: &'a [T],
	requested_skills: &[String],
	install_all: bool,
	name: F,
) -> Result<Vec<&'a T>, RepoSkillSelectionError>
where
	F: for<'b> Fn(&'b T) -> &'b str,
{
	if discovered.is_empty() {
		return Err(RepoSkillSelectionError::NoSkillsFound);
	}

	if install_all || requested_skills.is_empty() {
		return Ok(discovered.iter().collect());
	}

	let mut selected = Vec::new();
	let mut missing = Vec::new();
	for requested in requested_skills {
		let requested_lower = requested.to_lowercase();
		match discovered
			.iter()
			.find(|skill| name(skill).to_lowercase() == requested_lower)
		{
			Some(skill) => selected.push(skill),
			None => missing.push(requested.clone()),
		}
	}

	if !missing.is_empty() {
		return Err(RepoSkillSelectionError::SkillsNotFound {
			missing: missing.join(", "),
			available: discovered
				.iter()
				.map(|skill| name(skill).to_string())
				.collect::<Vec<_>>()
				.join(", "),
		});
	}

	Ok(selected)
}

/// Compute the source-folder hash for an install.
///
/// Hashes the SOURCE repo subfolder (`source_dir`), not the post-copy
/// installed dir. Maps a [`crate::hash::HashError`] into an
/// [`std::io::Error`] so the install writers share one error type.
fn compute_install_hash(source_dir: &Path) -> std::io::Result<String> {
	crate::compute_skill_folder_hash(source_dir)
		.map_err(|e| std::io::Error::other(e.to_string()))
}

pub fn write_global_install_lock(
	skill_name: &str,
	source: &InstallLockSource,
	skill_path: Option<String>,
	source_dir: &Path,
	ref_commit: Option<String>,
) -> std::io::Result<Option<crate::SkillLockEntry>> {
	let content_hash = compute_install_hash(source_dir)?;
	global::add_skill_to_lock(
		skill_name,
		SkillLockEntry {
			source: source.source.clone(),
			source_type: source.source_type.clone(),
			source_url: source.source_url.clone(),
			ref_name: source.ref_name.clone(),
			skill_path,
			// Constraint 4: `skill_folder_hash` stays empty; the real hash
			// lives in the optional per-entry `content_hash`.
			skill_folder_hash: String::new(),
			installed_at: String::new(),
			updated_at: String::new(),
			plugin_name: None,
			content_hash: Some(content_hash),
			ref_commit,
		},
	)
}

/// The `sourceUrl` to persist in a project lock entry, or `None` to omit it.
/// Only a non-github remote host carries recovery info the host-stripped
/// `source` lacks; github shorthand and local sources reconstruct fine and are
/// left out to keep the lock byte-identical and npx-invisible.
fn recordable_source_url(source: &InstallLockSource) -> Option<String> {
	let url = source.source_url.trim();
	if url.is_empty() || source.source_type.eq_ignore_ascii_case("local") {
		return None;
	}
	// Exact-host check (NOT substring): `mygithub.com` must still be recorded,
	// only real github.com / *.github.com is reconstructable from `source`.
	match host_of_url(url) {
		Some(host) if host == "github.com" || host.ends_with(".github.com") => {
			None
		}
		_ => Some(url.to_string()),
	}
}

/// Lowercased host of an `scheme://[user@]host[:port]/…` URL (userinfo/port
/// stripped); `None` when there is no `://` authority.
fn host_of_url(url: &str) -> Option<String> {
	let after_scheme = url.split_once("://")?.1;
	let authority = after_scheme.split(['/', '?', '#']).next()?;
	let authority = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
	let host = if let Some(rest) = authority.strip_prefix('[') {
		rest.split_once(']')?.0
	} else {
		authority.split(':').next()?
	};
	(!host.is_empty()).then(|| host.to_ascii_lowercase())
}

pub fn write_project_install_lock(
	skill_name: &str,
	source: &InstallLockSource,
	skill_path: Option<String>,
	source_dir: &Path,
	cwd: &Path,
	ref_commit: Option<String>,
) -> std::io::Result<Option<local::LocalSkillLockEntry>> {
	let computed_hash = compute_install_hash(source_dir)?;
	local::add_skill_to_local_lock(
		skill_name,
		local::LocalSkillLockEntry {
			ref_commit,
			source: source.source.clone(),
			ref_name: source.ref_name.clone(),
			source_type: source.source_type.clone(),
			computed_hash,
			skill_path,
			// Record the full clone URL ONLY for a non-github remote host
			// (TFS/Azure DevOps/on-prem GitLab), where the host-stripped
			// `source` can't be reconstructed. github and local sources
			// reconstruct fine from `source`, so leave them None — keeping the
			// lock byte-identical and npx-invisible for the common case.
			source_url: recordable_source_url(source),
		},
		Some(cwd),
	)
}

#[cfg(test)]
mod tests {
	use super::*;
	use tempfile::TempDir;

	#[test]
	fn repo_skill_selection_policy_is_shared_and_complete() {
		#[derive(Debug, PartialEq, Eq)]
		struct Named(&'static str);

		fn name(skill: &Named) -> &str {
			skill.0
		}

		let catalog = [Named("Alpha"), Named("Beta")];
		let empty: [Named; 0] = [];
		assert_eq!(
			select_repo_skills(&empty, &[], false, name),
			Err(RepoSkillSelectionError::NoSkillsFound)
		);
		assert_eq!(
			select_repo_skills(&catalog, &["missing".into()], true, name)
				.unwrap(),
			vec![&catalog[0], &catalog[1]]
		);
		assert_eq!(
			select_repo_skills(&catalog, &[], false, name).unwrap(),
			vec![&catalog[0], &catalog[1]]
		);
		assert_eq!(
			select_repo_skills(&catalog, &["bEtA".into()], false, name)
				.unwrap(),
			vec![&catalog[1]]
		);
		assert_eq!(
			select_repo_skills(&catalog, &["missing".into()], false, name),
			Err(RepoSkillSelectionError::SkillsNotFound {
				missing: "missing".into(),
				available: "Alpha, Beta".into(),
			})
		);
	}

	#[test]
	fn lock_skill_file_path_handles_root_skill() {
		assert_eq!(lock_skill_file_path(""), "SKILL.md");
		assert_eq!(
			lock_skill_file_path("skills/test-skill"),
			"skills/test-skill/SKILL.md"
		);
	}

	fn sample_source() -> InstallLockSource {
		InstallLockSource {
			source: "owner/repo".to_string(),
			source_type: "github".to_string(),
			source_url: "https://github.com/owner/repo.git".to_string(),
			ref_name: Some("main".to_string()),
		}
	}

	#[test]
	fn recordable_source_url_uses_exact_host_not_substring() {
		let mk = |ty: &str, url: &str| InstallLockSource {
			source: "o/r".to_string(),
			source_type: ty.to_string(),
			source_url: url.to_string(),
			ref_name: None,
		};
		// Real github (+ subdomain) reconstructs from `source` → omitted.
		assert_eq!(
			recordable_source_url(&mk("github", "https://github.com/o/r.git")),
			None
		);
		assert_eq!(
			recordable_source_url(&mk("github", "https://raw.github.com/o/r")),
			None
		);
		// Look-alike host must NOT be treated as github (substring bug).
		assert_eq!(
			recordable_source_url(&mk("git", "https://mygithub.com/o/r.git"))
				.as_deref(),
			Some("https://mygithub.com/o/r.git")
		);
		// Genuine non-github remote → recorded.
		assert_eq!(
			recordable_source_url(&mk(
				"git",
				"https://tfs.example:8443/c/_git/r"
			))
			.as_deref(),
			Some("https://tfs.example:8443/c/_git/r")
		);
		// Local source → never recorded.
		assert_eq!(recordable_source_url(&mk("local", "file:///tmp/x")), None);
	}

	#[test]
	fn write_global_install_lock_records_ref_commit() {
		let _g = crate::lock::test_utils::TestLockGuard::new();
		let source = TempDir::new().unwrap();
		std::fs::write(source.path().join("SKILL.md"), b"name: t\n").unwrap();

		write_global_install_lock(
			"t",
			&sample_source(),
			Some(lock_skill_file_path("")),
			source.path(),
			Some("deadbeefcafef00d".to_string()),
		)
		.unwrap();

		let lock = crate::lock::global::read_skill_lock();
		let entry = lock.skills.get("t").unwrap();
		assert_eq!(entry.ref_commit.as_deref(), Some("deadbeefcafef00d"));
	}

	#[test]
	fn write_project_install_lock_records_ref_commit() {
		let _g = crate::lock::test_utils::TestLockGuard::new();
		let project = TempDir::new().unwrap();
		let source = TempDir::new().unwrap();
		std::fs::write(source.path().join("SKILL.md"), b"name: t\n").unwrap();

		write_project_install_lock(
			"t",
			&sample_source(),
			Some(lock_skill_file_path("")),
			source.path(),
			project.path(),
			Some("deadbeefcafef00d".to_string()),
		)
		.unwrap();

		let lock = local::read_local_lock(Some(project.path()));
		let entry = lock.skills.get("t").unwrap();
		assert_eq!(entry.ref_commit.as_deref(), Some("deadbeefcafef00d"));
	}

	#[test]
	fn write_project_install_lock_computes_real_hash() {
		let _g = crate::lock::test_utils::TestLockGuard::new();
		let project = TempDir::new().unwrap();
		let source = TempDir::new().unwrap(); // the SOURCE repo subfolder
		std::fs::write(source.path().join("SKILL.md"), b"name: t\n").unwrap();

		let src = sample_source();
		write_project_install_lock(
			"t",
			&src,
			Some(lock_skill_file_path("skills/my-skill")),
			source.path(),
			project.path(),
			None,
		)
		.unwrap();

		let lock = local::read_local_lock(Some(project.path()));
		let entry = lock.skills.get("t").unwrap();
		assert_ne!(entry.computed_hash, crate::hash::EMPTY_SKILLS_LOCK_DIGEST);
		assert_eq!(
			entry.computed_hash,
			crate::compute_skill_folder_hash(source.path()).unwrap()
		);
		assert_eq!(
			entry.skill_path.as_deref(),
			Some("skills/my-skill/SKILL.md")
		);
	}
}
