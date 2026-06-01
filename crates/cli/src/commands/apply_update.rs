use crate::ResourceType;
use aghub_core::models::{ResourceScope, Skill};
use anyhow::{anyhow, bail, Context, Result};
use gix::bstr::ByteSlice;
use serde_json::json;
use std::path::{Path, PathBuf};

struct ApplySource {
	source: String,
	ref_name: Option<String>,
	skill_path: String,
}

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
	let source = apply_source_from_lock(&name, scope, project_root)?;
	let targets = installed_skill_roots(&name, scope, project_root);
	if targets.is_empty() {
		bail!("skill '{name}' is locked but no installed copy was found");
	}

	let repo = fetch_source(&source)?;
	let skill_file = aghub_core::skills::update::sanitize_skill_path(
		&repo,
		&source.skill_path,
	)
	.ok_or_else(|| anyhow!("locked skillPath was not found in source"))?;
	let source_dir = skill_file.parent().unwrap_or(&repo);
	let updated_hash = skill::compute_skill_folder_hash(source_dir)
		.context("failed to hash fetched skill")?;

	let mut paths = Vec::new();
	for target in &targets {
		if target.exists() {
			if target.is_dir() {
				std::fs::remove_dir_all(target)
			} else {
				std::fs::remove_file(target)
			}
			.with_context(|| {
				format!("failed to remove old skill at {}", target.display())
			})?;
		}
		copy_dir_recursive(source_dir, target).with_context(|| {
			format!("failed to copy updated skill to {}", target.display())
		})?;
		paths.push(target.display().to_string());
	}

	update_lock_hash(&name, scope, project_root, &updated_hash)?;
	println!(
		"{}",
		serde_json::to_string_pretty(&json!({
			"success": true,
			"name": name,
			"scope": scope_name(scope),
			"updatedHash": updated_hash,
			"paths": paths,
			"error": null,
		}))?
	);
	Ok(())
}

fn apply_source_from_lock(
	name: &str,
	scope: ResourceScope,
	project_root: Option<&Path>,
) -> Result<ApplySource> {
	match scope {
		ResourceScope::GlobalOnly => {
			let entry = skill::lock::global::get_skill_from_lock(name)
				.ok_or_else(|| {
					anyhow!("skill '{name}' is not in global lock")
				})?;
			let skill_path = entry
				.skill_path
				.ok_or_else(|| anyhow!("locked skill has no skillPath"))?;
			Ok(ApplySource {
				source: entry.source_url,
				ref_name: entry.ref_name,
				skill_path,
			})
		}
		ResourceScope::ProjectOnly => {
			let root = project_root
				.ok_or_else(|| anyhow!("project root is required"))?;
			let lock = skill::lock::local::read_local_lock(Some(root));
			let entry = lock.skills.get(name).cloned().ok_or_else(|| {
				anyhow!("skill '{name}' is not in project lock")
			})?;
			let skill_path = entry
				.skill_path
				.ok_or_else(|| anyhow!("locked skill has no skillPath"))?;
			Ok(ApplySource {
				source: entry.source,
				ref_name: entry.ref_name,
				skill_path,
			})
		}
		ResourceScope::Both => {
			bail!("apply-update requires --global or --project, not --all")
		}
	}
}

fn fetch_source(source: &ApplySource) -> Result<PathBuf> {
	let resolved = aghub_git::resolve_remote_source(&source.source)
		.context("failed to resolve remote source")?;
	let creds = aghub_git::read_credentials();
	let (bare, oid) = aghub_git::fetch_ref_to_temp(
		&resolved.clone_url,
		source.ref_name.as_deref(),
		creds.as_ref(),
	)
	.context("failed to fetch source repository")?;
	let repo = gix::open(bare.path()).context("failed to open fetched repo")?;
	let object = repo
		.find_object(oid)
		.context("failed to read fetched HEAD")?;
	let tree = object
		.peel_to_tree()
		.context("failed to peel fetched tree")?;
	let materialized = tempfile::TempDir::new()?;
	materialize_tree(&repo, tree.id, materialized.path())?;
	Ok(materialized.keep())
}

fn materialize_tree(
	repo: &gix::Repository,
	tree_id: gix::ObjectId,
	dest: &Path,
) -> std::io::Result<()> {
	std::fs::create_dir_all(dest)?;
	let tree = repo
		.find_tree(tree_id)
		.map_err(|e| std::io::Error::other(e.to_string()))?;
	for entry in tree.iter() {
		let entry = entry.map_err(|e| std::io::Error::other(e.to_string()))?;
		let name = entry.filename().to_str_lossy();
		let target = dest.join(name.as_ref());
		if entry.mode().is_tree() {
			materialize_tree(repo, entry.object_id(), &target)?;
		} else if entry.mode().is_blob() {
			let object = entry
				.object()
				.map_err(|e| std::io::Error::other(e.to_string()))?;
			if let Some(parent) = target.parent() {
				std::fs::create_dir_all(parent)?;
			}
			std::fs::write(target, &object.data)?;
		}
	}
	Ok(())
}

fn skill_root(skill: &Skill) -> Option<PathBuf> {
	let raw = skill
		.canonical_path
		.as_deref()
		.or(skill.source_path.as_deref())?;
	let path = if let Some(stripped) = raw.strip_prefix("~/") {
		dirs::home_dir().map(|home| home.join(stripped))?
	} else {
		PathBuf::from(raw)
	};
	Some(if path.is_dir() {
		path
	} else {
		path.parent().map(Path::to_path_buf).unwrap_or(path)
	})
}

fn installed_skill_roots(
	name: &str,
	scope: ResourceScope,
	project_root: Option<&Path>,
) -> Vec<PathBuf> {
	let mut roots = Vec::new();
	for agent in aghub_core::load_all_agents(scope, project_root) {
		for skill in agent.skills {
			if skill.name != name {
				continue;
			}
			let Some(root) = skill_root(&skill) else {
				continue;
			};
			if !roots.contains(&root) {
				roots.push(root);
			}
		}
	}
	roots
}

fn copy_dir_recursive(from: &Path, to: &Path) -> std::io::Result<()> {
	std::fs::create_dir_all(to)?;
	for entry in std::fs::read_dir(from)? {
		let entry = entry?;
		let file_type = entry.file_type()?;
		if file_type.is_symlink() {
			continue;
		}
		let from_path = entry.path();
		let to_path = to.join(entry.file_name());
		if file_type.is_dir() {
			copy_dir_recursive(&from_path, &to_path)?;
		} else if file_type.is_file() {
			std::fs::copy(&from_path, &to_path)?;
		}
	}
	Ok(())
}

fn update_lock_hash(
	name: &str,
	scope: ResourceScope,
	project_root: Option<&Path>,
	hash: &str,
) -> Result<()> {
	match scope {
		ResourceScope::GlobalOnly => {
			let mut entry = skill::lock::global::get_skill_from_lock(name)
				.ok_or_else(|| {
					anyhow!("skill '{name}' is not in global lock")
				})?;
			entry.content_hash = Some(hash.to_string());
			entry.skill_folder_hash.clear();
			skill::lock::global::add_skill_to_lock(name, entry)?;
		}
		ResourceScope::ProjectOnly => {
			let root = project_root
				.ok_or_else(|| anyhow!("project root is required"))?;
			let mut lock = skill::lock::local::read_local_lock(Some(root));
			let entry = lock.skills.get_mut(name).ok_or_else(|| {
				anyhow!("skill '{name}' is not in project lock")
			})?;
			entry.computed_hash = hash.to_string();
			skill::lock::local::write_local_lock(&lock, Some(root))?;
		}
		ResourceScope::Both => {
			bail!("apply-update requires --global or --project, not --all")
		}
	}
	Ok(())
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
		)
		.unwrap();

		let lock = skill::lock::global::read_skill_lock();
		let entry = &lock.skills["legacy"];
		assert_eq!(entry.content_hash.as_deref(), Some("content-v2"));
		assert_eq!(entry.skill_folder_hash, "");
	}
}
