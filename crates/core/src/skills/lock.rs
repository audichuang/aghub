//! Lock re-stamp after a content rewrite. The single home shared by the CLI
//! apply-update path and the API apply-update / git-sync routes (and, in turn,
//! the resync flow). Lives here rather than in `update` (which is pure compare,
//! no I/O) because this mutates the on-disk lock.

use crate::models::ResourceScope;
use std::path::Path;

/// Re-stamp an installed skill's lock hash after a content rewrite. `ref_commit`
/// is authoritative: `Some(oid)` records the tip; `None` CLEARS any recorded tip
/// (the content changed, so a stale OID would let an ls-refs preflight falsely
/// skip the next fetch).
pub fn update_lock_hash(
	name: &str,
	scope: ResourceScope,
	project_root: Option<&Path>,
	hash: &str,
	ref_commit: Option<&str>,
) -> Result<(), String> {
	match scope {
		ResourceScope::GlobalOnly => {
			skill::lock::global::modify_skill_lock(|lock| {
				let Some(entry) = lock.skills.get_mut(name) else {
					return Err("skill is not in global lock".to_string());
				};
				entry
					.apply_content_hash(hash, &chrono::Utc::now().to_rfc3339());
				entry.ref_commit = ref_commit.map(str::to_string);
				Ok(())
			})
			.map_err(|e| format!("Failed to update global lock: {e}"))?
		}
		ResourceScope::ProjectOnly => {
			let Some(root) = project_root else {
				return Err("project_root is required when scope is project"
					.to_string());
			};
			skill::lock::local::modify_local_lock(Some(root), |lock| {
				let Some(entry) = lock.skills.get_mut(name) else {
					return Err("skill is not in project lock".to_string());
				};
				entry.apply_computed_hash(hash);
				entry.ref_commit = ref_commit.map(str::to_string);
				Ok(())
			})
			.map_err(|e| format!("Failed to update project lock: {e}"))?
		}
		ResourceScope::Both => {
			Err("update requires global or project scope, not both".to_string())
		}
	}
}
