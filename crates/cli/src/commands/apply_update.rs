use crate::ResourceType;
use aghub_core::models::ResourceScope;
use anyhow::{anyhow, bail, Result};
use serde_json::json;
use std::path::Path;
#[cfg(test)]
use std::path::PathBuf;

pub fn execute(
	resource: ResourceType,
	name: String,
	scope: ResourceScope,
	project_root: Option<&Path>,
	yes: bool,
	_json: bool,
) -> Result<()> {
	match resource {
		ResourceType::Skills => {}
		ResourceType::Mcps => bail!("`apply-update` only supports skills"),
	}
	if !yes {
		bail!("refusing to overwrite skill files without --yes");
	}
	let report = skill_update::mutation::resync_locked_skill(
		skill_update::mutation::LockedResyncRequest {
			name: &name,
			scope,
			project_root,
		},
		&skill_update::GitFetcher,
		&crate::commands::source::EnvTokenResolver,
	)
	.map_err(|error| locked_resync_error(&name, error))?;
	let paths = report.swapped;
	let updated_hash = report.updated_hash;
	println!(
		"{}",
		serde_json::to_string_pretty(&json!({
			"success": true,
			"name": name,
			"scope": scope_name(scope),
			"updatedHash": updated_hash,
			"paths": paths
				.iter()
				.map(|p| p.display().to_string())
				.collect::<Vec<_>>(),
			"error": null,
		}))?
	);
	Ok(())
}

/// Apply a skill update from an **already-fetched** repo working tree, then
/// update the scope's lock (content/computed hash + `refCommit`) exactly as
/// [`execute`] does. Fetch-free so callers that already materialized the source
/// (e.g. `source sync --update`) can reuse it without a second network round.
///
/// Sanitizes the locked skillPath to a source dir, then delegates the rename
/// guard → containment → best-effort swap → lock transaction to the shared core
/// resync. Returns the swapped install paths.
#[cfg(test)]
pub fn apply_skill_update_from_fetched(
	repo_root: &Path,
	skill_path: &str,
	name: &str,
	scope: ResourceScope,
	project_root: Option<&Path>,
	ref_commit: Option<&str>,
) -> Result<Vec<PathBuf>> {
	use aghub_core::skills::resync::{
		resync_installed_skill, ResyncError, ResyncRequest,
	};

	let skill_file =
		aghub_core::skills::update::sanitize_skill_path(repo_root, skill_path)
			.ok_or_else(|| {
				anyhow!("locked skillPath was not found in source")
			})?;
	let source_dir = skill_file.parent().unwrap_or(repo_root);

	let report = resync_installed_skill(ResyncRequest {
		source_dir,
		name,
		scope,
		project_root,
		ref_commit,
	})
	.map_err(|e| match e {
		ResyncError::NotInstalled => {
			anyhow!("skill '{name}' is locked but no installed copy was found")
		}
		ResyncError::Renamed { new_name } => anyhow!(
			aghub_core::skills::update::skill_renamed_message(name, &new_name)
		),
		other => anyhow!(other.to_string()),
	})?;
	Ok(report.swapped)
}

fn locked_resync_error(
	name: &str,
	error: skill_update::mutation::LockedResyncError,
) -> anyhow::Error {
	use aghub_core::skills::resync::ResyncError;
	use skill_update::mutation::LockedResyncError;

	match error {
		LockedResyncError::UnsupportedScope(_) => {
			anyhow!("apply-update requires --global or --project, not --all")
		}
		LockedResyncError::ProjectRootRequired => {
			anyhow!("project root is required")
		}
		LockedResyncError::LockEntryNotFound { scope } => match scope {
			ResourceScope::GlobalOnly => {
				anyhow!("skill '{name}' is not in global lock")
			}
			ResourceScope::ProjectOnly => {
				anyhow!("skill '{name}' is not in project lock")
			}
			ResourceScope::Both => anyhow!("skill '{name}' is not in lock"),
		},
		LockedResyncError::MissingSkillPath => {
			anyhow!("locked skill has no skillPath")
		}
		LockedResyncError::NotInstalled
		| LockedResyncError::Resync(ResyncError::NotInstalled) => {
			anyhow!("skill '{name}' is locked but no installed copy was found")
		}
		LockedResyncError::CredentialBackendUnavailable
		| LockedResyncError::Fetch(
			skill_update::FetchError::BackendUnavailable,
		) => anyhow!("Credential backend is unavailable; retry later."),
		LockedResyncError::InvalidSkillPath => {
			anyhow!("locked skillPath is not a valid skill folder")
		}
		LockedResyncError::SourceSkillNotFound => {
			anyhow!("locked skillPath was not found in source")
		}
		LockedResyncError::Fetch(skill_update::FetchError::Auth) => {
			anyhow!("failed to fetch source repository: authentication failed")
		}
		LockedResyncError::Fetch(skill_update::FetchError::Network) => {
			anyhow!("failed to fetch source repository")
		}
		LockedResyncError::Resync(ResyncError::Renamed { new_name }) => {
			anyhow!(aghub_core::skills::update::skill_renamed_message(
				name, &new_name
			))
		}
		LockedResyncError::Resync(other) => anyhow!(other.to_string()),
	}
}

fn scope_name(scope: ResourceScope) -> &'static str {
	match scope {
		ResourceScope::GlobalOnly => "global",
		ResourceScope::ProjectOnly => "project",
		ResourceScope::Both => "all",
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use aghub_core::skills::lock::update_lock_hash;
	use std::sync::{Mutex, MutexGuard, OnceLock};
	use tempfile::{tempdir, TempDir};

	struct GlobalLockGuard {
		_temp: TempDir,
		old: Option<String>,
		_lock: MutexGuard<'static, ()>,
	}

	impl GlobalLockGuard {
		fn new() -> Self {
			static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
			let guard = LOCK
				.get_or_init(|| Mutex::new(()))
				.lock()
				.unwrap_or_else(|e| e.into_inner());
			let temp = tempdir().unwrap();
			let old = std::env::var("XDG_STATE_HOME").ok();
			std::env::set_var("XDG_STATE_HOME", temp.path());
			Self {
				_temp: temp,
				old,
				_lock: guard,
			}
		}
	}

	impl Drop for GlobalLockGuard {
		fn drop(&mut self) {
			match &self.old {
				Some(value) => std::env::set_var("XDG_STATE_HOME", value),
				None => std::env::remove_var("XDG_STATE_HOME"),
			}
		}
	}

	fn global_entry() -> skill::SkillLockEntry {
		skill::SkillLockEntry {
			source: "owner/repo".to_string(),
			source_type: "github".to_string(),
			source_url: "https://github.com/owner/repo".to_string(),
			ref_name: Some("main".to_string()),
			skill_path: Some("SKILL.md".to_string()),
			skill_folder_hash: "tree-v1".to_string(),
			content_hash: None,
			ref_commit: None,
			installed_at: "t".to_string(),
			updated_at: "t".to_string(),
			plugin_name: None,
		}
	}

	#[test]
	fn global_update_lock_hash_clears_npx_folder_hash() {
		let _guard = GlobalLockGuard::new();
		skill::lock::global::add_skill_to_lock("legacy", global_entry())
			.unwrap();

		update_lock_hash(
			"legacy",
			ResourceScope::GlobalOnly,
			None,
			"content-v2",
			None,
		)
		.unwrap();

		let lock = skill::lock::global::read_skill_lock();
		let entry = &lock.skills["legacy"];
		assert_eq!(entry.content_hash.as_deref(), Some("content-v2"));
		assert_eq!(entry.skill_folder_hash, "");
	}

	#[test]
	fn global_apply_update_writes_ref_commit() {
		let _guard = GlobalLockGuard::new();
		skill::lock::global::add_skill_to_lock("legacy", global_entry())
			.unwrap();

		update_lock_hash(
			"legacy",
			ResourceScope::GlobalOnly,
			None,
			"content-v2",
			Some("deadbeefcafef00d"),
		)
		.unwrap();

		let lock = skill::lock::global::read_skill_lock();
		let entry = &lock.skills["legacy"];
		assert_eq!(entry.content_hash.as_deref(), Some("content-v2"));
		assert_eq!(entry.ref_commit.as_deref(), Some("deadbeefcafef00d"));
	}

	#[test]
	fn project_apply_update_writes_ref_commit() {
		let project = tempdir().unwrap();
		skill::lock::local::add_skill_to_local_lock(
			"legacy",
			skill::lock::local::LocalSkillLockEntry {
				source_url: None,
				source: "owner/repo".to_string(),
				ref_name: Some("main".to_string()),
				source_type: "github".to_string(),
				computed_hash: "old".to_string(),
				skill_path: Some("SKILL.md".to_string()),
				ref_commit: None,
			},
			Some(project.path()),
		)
		.unwrap();

		update_lock_hash(
			"legacy",
			ResourceScope::ProjectOnly,
			Some(project.path()),
			"content-v2",
			Some("deadbeefcafef00d"),
		)
		.unwrap();

		let lock = skill::lock::local::read_local_lock(Some(project.path()));
		let entry = &lock.skills["legacy"];
		assert_eq!(entry.computed_hash, "content-v2");
		assert_eq!(entry.ref_commit.as_deref(), Some("deadbeefcafef00d"));
	}

	fn write_skill_md(dir: &std::path::Path, name: &str, body: &str) {
		std::fs::create_dir_all(dir).unwrap();
		std::fs::write(
			dir.join("SKILL.md"),
			format!("---\nname: {name}\ndescription: d\n---\n\n{body}\n"),
		)
		.unwrap();
	}

	// The CLI wrapper maps resync's NotInstalled to the documented message.
	#[test]
	fn apply_from_fetched_not_installed_errors() {
		let tmp = tempdir().unwrap();
		let project = tmp.path().join("project");
		let repo = tmp.path().join("repo");
		write_skill_md(&repo.join("ghost"), "ghost", "x");

		let err = apply_skill_update_from_fetched(
			&repo,
			"ghost/SKILL.md",
			"ghost",
			ResourceScope::ProjectOnly,
			Some(&project),
			None,
		)
		.unwrap_err();
		assert!(err.to_string().contains("no installed copy"), "err: {err}");
	}

	// The CLI wrapper maps resync's Renamed to the shared rename message and
	// leaves the installed copy untouched.
	#[test]
	fn apply_from_fetched_renamed_errors_and_keeps_install() {
		let tmp = tempdir().unwrap();
		let project = tmp.path().join("project");
		let installed = project.join(".claude/skills/keep");
		write_skill_md(&installed, "keep", "old");
		skill::add_skill_to_local_lock(
			"keep",
			skill::LocalSkillLockEntry {
				source_url: None,
				source: "owner/repo".to_string(),
				ref_name: Some("main".to_string()),
				source_type: "github".to_string(),
				computed_hash: "old".to_string(),
				skill_path: Some("keep/SKILL.md".to_string()),
				ref_commit: None,
			},
			Some(&project),
		)
		.unwrap();
		let repo = tmp.path().join("repo");
		write_skill_md(&repo.join("keep"), "keep-v2", "new");

		let err = apply_skill_update_from_fetched(
			&repo,
			"keep/SKILL.md",
			"keep",
			ResourceScope::ProjectOnly,
			Some(&project),
			None,
		)
		.unwrap_err();
		let msg = err.to_string();
		assert!(
			msg.contains("keep") && msg.contains("keep-v2"),
			"msg: {msg}"
		);
		assert!(std::fs::read_to_string(installed.join("SKILL.md"))
			.unwrap()
			.contains("old"));
	}
}
