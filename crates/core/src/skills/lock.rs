//! Lock re-stamp after a content rewrite. The single home shared by the CLI
//! apply-update path and the API apply-update / git-sync routes (and, in turn,
//! the resync flow). Lives here rather than in `update` (which is pure compare,
//! no I/O) because this mutates the on-disk lock.

use crate::models::ResourceScope;
use std::path::Path;

/// A remote identity two source spellings can be compared on, or `None` when this
/// spelling is not one of the forms we can canonicalize.
///
/// Wraps [`crate::skills::install_fetched::remote_owner_from_url`] with the
/// `owner/repo` shorthand, which that function deliberately does not accept (it
/// guards Master adoption, where treating a hostless string as GitHub would widen
/// ownership). Here the shorthand MUST resolve, because it is exactly what an
/// npx-written project entry records — leaving it unresolvable would make every
/// such entry unprovable and therefore unguarded. `owner/repo` means GitHub in
/// this codebase; `precheck_source` accepts no other hostless form.
fn comparable_remote(source: &str) -> Option<String> {
	use crate::skills::install_fetched::remote_owner_from_url;
	let source = source.trim();
	let hostless_shorthand = !source.contains("://")
		&& !source.contains(':')
		&& source.matches('/').count() == 1;
	if hostless_shorthand {
		return remote_owner_from_url(&format!("https://github.com/{source}"));
	}
	remote_owner_from_url(source)
}

/// Wire code for mutation-lock contention: another aghub process held the lock,
/// nothing was written, and the SAME request will succeed once it finishes. The
/// one thing a surface must convey is that it is retryable — a generic failure
/// code makes callers give up on a transient condition.
///
/// Defined here so every surface that can hit contention (manager mutations via
/// `ConfigError::Io`, resync, rename) projects it identically instead of each
/// spelling the string itself.
pub const MUTATION_LOCK_BUSY_CODE: &str = "SKILL_MUTATION_LOCK_BUSY";

/// Wire code for [`EntryIdentity`] refusing: the lock entry no longer matches the
/// one this operation fetched, so nothing was written. Also retryable, but only
/// after the caller re-reads — the coordinates themselves moved.
pub const SOURCE_CHANGED_DURING_FETCH_CODE: &str =
	"SKILL_SOURCE_CHANGED_DURING_FETCH";

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
/// **Only an entry aghub actually read can produce one** — via
/// [`of_global_entry`](Self::of_global_entry) /
/// [`of_project_entry`](Self::of_project_entry) when the caller already holds the
/// entry, or [`capture`](Self::capture) when it does not. Never by hand: every
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
///
/// A caller that reads the entry to decide WHAT to fetch must build the identity
/// from THAT read — `of_*_entry` — not from a second [`Self::capture`]. Two reads can
/// straddle another process's repoint: the first yields the coordinates that get
/// fetched, the second an identity that matches the live entry, so the
/// compare-after-fetch passes while the bytes came from the OTHER coordinates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryIdentity {
	/// Effective source: the global entry's `source_url`, or the project entry's
	/// `source_url` falling back to `source`.
	source: String,
	skill_path: Option<String>,
	ref_name: Option<String>,
}

impl EntryIdentity {
	/// The identity of a GLOBAL entry the caller already holds, so it comes from
	/// the SAME read that produced the coordinates being fetched. Prefer this over
	/// [`capture`](Self::capture) whenever the entry is in hand — see the type
	/// docs for the two-read interleaving it rules out.
	pub fn of_global_entry(entry: &skill::SkillLockEntry) -> Self {
		Self {
			source: entry.source_url.clone(),
			skill_path: entry.skill_path.clone(),
			ref_name: entry.ref_name.clone(),
		}
	}

	/// [`of_global_entry`](Self::of_global_entry) for a PROJECT entry, whose
	/// `sourceUrl` is optional (npx writes none) so the effective source falls
	/// back to `source`.
	pub fn of_project_entry(entry: &skill::LocalSkillLockEntry) -> Self {
		Self {
			source: entry
				.source_url
				.clone()
				.unwrap_or_else(|| entry.source.clone()),
			skill_path: entry.skill_path.clone(),
			ref_name: entry.ref_name.clone(),
		}
	}

	/// Snapshot the entry's identity as it stands NOW, for a caller that does NOT
	/// already hold the entry. `None` means there is no such entry, which is
	/// itself a decision the caller must make explicitly (it has no mandate to
	/// overwrite a skill it never saw).
	pub fn capture(
		name: &str,
		scope: ResourceScope,
		project_root: Option<&Path>,
	) -> Option<Self> {
		match scope {
			ResourceScope::GlobalOnly => skill::lock::global::read_skill_lock()
				.skills
				.get(name)
				.map(Self::of_global_entry),
			ResourceScope::ProjectOnly => project_root.and_then(|root| {
				skill::lock::local::read_local_lock(Some(root))
					.skills
					.get(name)
					.map(Self::of_project_entry)
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

	/// Whether a request claiming `source` + `skill_path` is talking about THIS
	/// entry — i.e. whether the caller is allowed to overwrite it with content it
	/// fetched from those coordinates.
	///
	/// [`ensure_unchanged`](Self::ensure_unchanged) is a different question: it
	/// proves nobody else moved the entry while we fetched. It cannot catch a
	/// caller that fetched from somewhere else entirely, because the entry it
	/// compares against never changed. The API's git-sync takes both the session
	/// (a repo) and the skill name from the request, so without this a client can
	/// pair repo B's session with a skill locked to repo A: B's bytes land on disk
	/// while the lock keeps A's source/path/ref and merely gets B's hash stamped.
	/// No race needed.
	///
	/// Refuses ONLY on a provable mismatch. A side that cannot be resolved to a
	/// comparable remote (a self-hosted spelling, a form neither branch below
	/// understands) proves nothing, and refusing there would break legitimate
	/// syncs — the same false-negative trap that comparing a RECONSTRUCTED source
	/// URL fell into for npx-written project entries.
	pub fn describes(&self, source: &str, skill_path: &str) -> bool {
		if let Some(recorded) = self.skill_path.as_deref() {
			if recorded.trim() != skill_path.trim() {
				return false;
			}
		}
		match (comparable_remote(&self.source), comparable_remote(source)) {
			(Some(recorded), Some(claimed)) => recorded == claimed,
			_ => true,
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

#[cfg(test)]
mod tests {
	use super::*;

	fn identity(source: &str, skill_path: &str) -> EntryIdentity {
		EntryIdentity::unchecked_for_tests(
			source,
			Some(skill_path.to_string()),
			Some("main".to_string()),
		)
	}

	/// The mismatched-pair the API's git-sync could not see: a session for one
	/// repo alongside a skill locked to another. `ensure_unchanged` cannot catch
	/// it — the entry never moved — so this is the only thing standing between a
	/// caller and installing B's bytes under A's lock entry.
	#[test]
	fn a_different_repo_is_refused() {
		let entry = identity("https://github.com/good/repo.git", "s/SKILL.md");
		assert!(
			!entry.describes("https://github.com/evil/repo.git", "s/SKILL.md")
		);
		// Same repo NAME on another host is still another repo.
		assert!(
			!entry.describes("https://gitlab.com/good/repo.git", "s/SKILL.md")
		);
	}

	/// The false-negative trap this must NOT fall into. One repo has many legal
	/// spellings, and a project entry written by `npx skills` records the bare
	/// `owner/repo` shorthand — rejecting those would break every such sync,
	/// which is exactly the regression a reconstructed source URL caused before.
	#[test]
	fn one_repo_spelled_many_ways_is_accepted() {
		let shorthand = identity("owner/repo", "s/SKILL.md");
		for claimed in [
			"owner/repo",
			"https://github.com/owner/repo",
			"https://github.com/owner/repo.git",
			"https://GitHub.com/owner/repo.git/",
			"git@github.com:owner/repo.git",
			"ssh://git@github.com/owner/repo.git",
		] {
			assert!(
				shorthand.describes(claimed, "s/SKILL.md"),
				"'{claimed}' is the same repo as the shorthand"
			);
		}

		// And from the other direction: a full URL on the entry, shorthand claimed.
		let full = identity("https://github.com/owner/repo.git", "s/SKILL.md");
		assert!(full.describes("owner/repo", "s/SKILL.md"));
	}

	/// Same repo, different folder: the content would come from a path the entry
	/// does not name, while the lock keeps pointing at the old one.
	#[test]
	fn a_different_skill_path_is_refused() {
		let entry = identity("owner/repo", "mine/SKILL.md");
		assert!(!entry.describes("owner/repo", "theirs/SKILL.md"));
		assert!(entry.describes("owner/repo", "mine/SKILL.md"));
	}

	/// An unresolvable spelling proves nothing, so it must not refuse: a
	/// self-hosted host or a form neither branch understands is not evidence of a
	/// mismatch, and treating it as one would break working setups.
	#[test]
	fn an_unprovable_source_is_allowed_through() {
		let local = identity("file:///srv/skills", "s/SKILL.md");
		assert!(local.describes("file:///somewhere/else", "s/SKILL.md"));
		let entry = identity("owner/repo", "s/SKILL.md");
		assert!(entry.describes("not a url at all", "s/SKILL.md"));
	}

	/// A missing `skillPath` cannot be compared, and the source check still runs.
	#[test]
	fn an_entry_without_a_skill_path_still_checks_the_source() {
		let entry =
			EntryIdentity::unchecked_for_tests("owner/repo", None, None);
		assert!(entry.describes("owner/repo", "anything/SKILL.md"));
		assert!(!entry.describes("other/repo", "anything/SKILL.md"));
	}
}
