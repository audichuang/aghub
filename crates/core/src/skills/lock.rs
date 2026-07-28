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

/// A snapshot of one lock entry's identity, taken BEFORE a fetch and re-verified
/// under the mutation lock immediately before mutating — a compare-and-set on the
/// coordinates, not on content.
///
/// The mutation lock cannot close this window on its own: a fetch is a NETWORK
/// operation and must not run while holding the lock, so the read that decides
/// what to fetch is necessarily unlocked. That makes it the WIDEST window in the
/// subsystem (seconds, not microseconds) — everything else the lock covers is a
/// local filesystem step.
///
/// **Always produce one with [`EntryIdentity::capture`], never by hand.** Every
/// field then holds the entry's own pre-fetch value verbatim, which is what makes
/// the comparison meaningful and removes two traps:
///
/// - A re-derived source is not comparable. A GLOBAL entry always carries
///   `sourceUrl`, but a PROJECT entry's is aghub-only and absent on npx-written
///   locks, where the effective source is the `owner/repo` shorthand — so
///   reconstructing an HTTPS URL and comparing it would falsely reject every such
///   entry.
/// - A flow that OVERRIDES a coordinate for the operation (a `--ref`, or a
///   rename resolving a moved `skillPath`) must still compare the value that was
///   there BEFORE. Capturing separates the two by construction: the snapshot is
///   the expectation, and whatever the flow writes is the intent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryIdentity {
	/// Effective source: the global entry's `source_url`, or the project entry's
	/// `source_url` falling back to `source`.
	source: String,
	skill_path: Option<String>,
	ref_name: Option<String>,
}

impl EntryIdentity {
	/// Snapshot the entry's identity as it stands NOW. Call before fetching.
	/// `None` means there is no such entry, which is itself a decision the caller
	/// must make explicitly (it has no mandate to overwrite a skill it never saw).
	pub fn capture(
		name: &str,
		scope: ResourceScope,
		project_root: Option<&Path>,
	) -> Option<Self> {
		match scope {
			ResourceScope::GlobalOnly => skill::lock::global::read_skill_lock()
				.skills
				.get(name)
				.map(|e| Self {
					source: e.source_url.clone(),
					skill_path: e.skill_path.clone(),
					ref_name: e.ref_name.clone(),
				}),
			ResourceScope::ProjectOnly => project_root.and_then(|root| {
				skill::lock::local::read_local_lock(Some(root))
					.skills
					.get(name)
					.map(|e| Self {
						source: e
							.source_url
							.clone()
							.unwrap_or_else(|| e.source.clone()),
						skill_path: e.skill_path.clone(),
						ref_name: e.ref_name.clone(),
					})
			}),
			ResourceScope::Both => None,
		}
	}

	/// Build one WITHOUT reading a lock, for tests that never compare it (a
	/// fetch-only fixture still has to hand a value to the type). Gated on the
	/// same `testing` feature `TestConfig` uses. Production must always
	/// [`capture`](Self::capture) — a hand-built snapshot proves nothing.
	#[cfg(feature = "testing")]
	pub fn unchecked_for_tests(
		source: impl Into<String>,
		skill_path: Option<String>,
		ref_name: Option<String>,
	) -> Self {
		Self {
			source: source.into(),
			skill_path,
			ref_name,
		}
	}

	/// Refuse unless the entry is still exactly the one this snapshot describes.
	/// Call under the mutation lock, immediately before mutating.
	///
	/// All three coordinates bind. `ref_name` included: A fetching `main` while
	/// another process repoints the entry to `stable` would otherwise overwrite
	/// that content with `main` and stamp only the hash/OID, leaving disk and lock
	/// disagreeing. `skillPath` included: for a rename, the snapshot holds the OLD
	/// path, so a repoint to a different folder in the same repo is caught while
	/// the flow's own resolved new path is unaffected.
	pub fn ensure_unchanged(
		&self,
		name: &str,
		scope: ResourceScope,
		project_root: Option<&Path>,
	) -> Result<(), String> {
		let Some(current) = Self::capture(name, scope, project_root) else {
			return Err(format!(
				"'{name}' is no longer in the lock; another aghub process changed \
				 it while this operation was fetching, so nothing was written"
			));
		};
		if &current == self {
			return Ok(());
		}
		// Names only, no paths/URLs: a surface may forward this verbatim.
		Err(format!(
			"'{name}' now points at a different source, skill path or ref than the \
			 one this operation started from; another aghub process changed it in \
			 the meantime, so nothing was written. Re-run to use the current \
			 coordinates."
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
