//! Lock re-stamp after a content rewrite. The single home shared by the CLI
//! apply-update path and the API apply-update / git-sync routes (and, in turn,
//! the resync flow). Lives here rather than in `update` (which is pure compare,
//! no I/O) because this mutates the on-disk lock.

use crate::models::ResourceScope;
use std::path::Path;

/// Take the interprocess skill mutation lock for `scope`, to be held across a
/// whole transaction (check → write → rollback) rather than one lock write.
///
/// This is what makes a flow's own receipts (`created_master`, `created_lock`,
/// the replaced entry) trustworthy: without it two aghub processes both observe
/// an absent entry and are both told they created it. Reentrant, so the
/// `modify_*_lock` calls underneath take it again for free. `op` names the
/// operation in the timeout error. See `skill::lock::MutationScope`.
///
/// A `ProjectOnly` scope with no root locks nothing but the process — those
/// flows reject the missing root on their own.
pub fn mutation_guard(
	op: &str,
	scope: ResourceScope,
	project_root: Option<&Path>,
) -> std::io::Result<skill::lock::MutationGuard> {
	let mut scopes = Vec::new();
	if matches!(scope, ResourceScope::GlobalOnly | ResourceScope::Both) {
		scopes.push(skill::lock::MutationScope::Global);
	}
	if matches!(scope, ResourceScope::ProjectOnly | ResourceScope::Both) {
		if let Some(root) = project_root {
			scopes
				.push(skill::lock::MutationScope::Project(root.to_path_buf()));
		}
	}
	skill::lock::mutation_guard(op, &scopes)
}

/// The lock coordinates a flow FETCHED from, carried through the fetch so the
/// mutation can prove — under the mutation lock — that it is still acting on the
/// same entry.
///
/// The mutation lock cannot close this window on its own: a fetch is a NETWORK
/// operation and must not run while holding the lock, so the read that decides
/// what to fetch is necessarily unlocked. That makes it the WIDEST window in the
/// subsystem (seconds, not microseconds) — everything else the lock covers is a
/// local filesystem step. Compare-after-fetch is the other half.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchedFrom {
	/// The source the pre-fetch read produced, VERBATIM — the global entry's
	/// `source_url`, or the project entry's `source_url` falling back to `source`.
	/// Carry the value that read returned, never a re-derived one: an npx-written
	/// entry has no `sourceUrl` at all (it is an aghub-only field), so its
	/// effective source is the `owner/repo` shorthand and comparing a
	/// reconstructed clone URL against it would reject every such entry.
	pub source: String,
	/// The repo-relative `skillPath` the fetch used, when the flow requires it to
	/// be unchanged. `None` for a flow that legitimately RESOLVES a moved path —
	/// `accept_rename` does exactly that, so comparing the path there would reject
	/// the very case it exists to handle.
	pub skill_path: Option<String>,
}

impl FetchedFrom {
	/// Refuse unless the CURRENT lock entry for `name` is still the one
	/// [`FetchedFrom`] describes. Call under the mutation lock, immediately
	/// before mutating.
	///
	/// Compares the source, and `skillPath` when the caller supplied one — the
	/// fields that identify WHOSE skill this is and WHERE in the repo it lives.
	/// Deliberately not `ref_name`: an adapter may legitimately override the ref
	/// for this very operation, so comparing it would reject valid work.
	pub fn ensure_unchanged(
		&self,
		name: &str,
		scope: ResourceScope,
		project_root: Option<&Path>,
	) -> Result<(), String> {
		let current = match scope {
			ResourceScope::GlobalOnly => skill::lock::global::read_skill_lock()
				.skills
				.get(name)
				.map(|e| (e.source_url.clone(), e.skill_path.clone())),
			ResourceScope::ProjectOnly => project_root.and_then(|root| {
				skill::lock::local::read_local_lock(Some(root))
					.skills
					.get(name)
					.map(|e| {
						(
							e.source_url
								.clone()
								.unwrap_or_else(|| e.source.clone()),
							e.skill_path.clone(),
						)
					})
			}),
			ResourceScope::Both => None,
		};
		let Some((source, skill_path)) = current else {
			return Err(format!(
				"'{name}' is no longer in the lock; another aghub process changed \
				 it while this operation was fetching, so nothing was written"
			));
		};
		let path_ok = match &self.skill_path {
			Some(expected) => skill_path.as_deref() == Some(expected.as_str()),
			// The caller resolves a moved path on purpose; only the source binds.
			None => true,
		};
		if source == self.source && path_ok {
			return Ok(());
		}
		// Names only, no paths/URLs: a surface may forward this verbatim.
		Err(format!(
			"'{name}' now points at a different source or skill path than the one \
			 this operation fetched; another aghub process changed it in the \
			 meantime, so nothing was written. Re-run to use the current source."
		))
	}
}

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
