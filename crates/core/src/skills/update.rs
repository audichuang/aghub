//! PURE update comparison. No network, no keyring — callers pass a resolved token
//! and a fetched source folder. (Fetch + creds live in crates/api.)

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UncheckableReason {
	Auth,
	Network,
	Local,
	Ssh,
	UnsupportedScheme,
	NoPath,
	Timeout,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillUpdateStatus {
	UpToDate,
	UpdateAvailable { current: String, available: String },
	Uncheckable { reason: UncheckableReason },
}

/// Compare two known content hashes.
///
/// Legacy/missing hashes are intentionally not handled here: callers must first
/// compute a local baseline hash when the lock has no trustworthy value.
pub fn compare_known_hashes(stored: &str, fresh: &str) -> SkillUpdateStatus {
	if stored == fresh {
		SkillUpdateStatus::UpToDate
	} else {
		SkillUpdateStatus::UpdateAvailable {
			current: stored.to_string(),
			available: fresh.to_string(),
		}
	}
}

/// Reject absolute paths and any `..`; join under `root`; canonicalize; verify the
/// result stays under `root`. Returns the safe absolute skill dir, or None to reject.
pub fn sanitize_skill_path(root: &Path, skill_path: &str) -> Option<PathBuf> {
	if skill_path.is_empty() {
		return None;
	}
	let p = Path::new(skill_path);
	if p.is_absolute() {
		return None;
	}
	if p.components()
		.any(|c| matches!(c, std::path::Component::ParentDir))
	{
		return None;
	}
	let joined = root.join(p);
	let canon_root = root.canonicalize().ok()?;
	let canon = joined.canonicalize().ok()?; // also resolves symlinks
	if canon.starts_with(&canon_root) {
		Some(canon)
	} else {
		None
	}
}

/// Compare `stored` (content_hash/computed_hash) against the freshly recomputed
/// `fetched_dir` hash. `stored == None` or placeholder → recompute-only (auto-heal):
/// returns UpToDate when stored is unknown and recompute succeeds (no false positive).
pub fn compare_hashes(
	stored: Option<&str>,
	fetched_dir: &Path,
) -> Result<SkillUpdateStatus, std::io::Error> {
	let fresh = skill::compute_skill_folder_hash(fetched_dir)
		.map_err(|e| std::io::Error::other(e.to_string()))?;
	match stored {
		// unknown or placeholder → auto-heal: no false UpdateAvailable
		None => Ok(SkillUpdateStatus::UpToDate),
		Some(h) if skill::is_placeholder_digest(h) => {
			Ok(SkillUpdateStatus::UpToDate)
		}
		Some(h) => Ok(compare_known_hashes(h, &fresh)),
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::fs;
	use tempfile::tempdir;

	#[test]
	fn rejects_absolute_skill_path() {
		let root = tempdir().unwrap();
		assert_eq!(sanitize_skill_path(root.path(), "/etc/passwd"), None);
	}
	#[test]
	fn rejects_dotdot_skill_path() {
		let root = tempdir().unwrap();
		assert_eq!(
			sanitize_skill_path(root.path(), "../../secret/SKILL.md"),
			None
		);
	}
	#[test]
	fn accepts_contained_skill_path() {
		let root = tempdir().unwrap();
		fs::create_dir_all(root.path().join("skills/a")).unwrap();
		fs::write(root.path().join("skills/a/SKILL.md"), b"x").unwrap();
		let got =
			sanitize_skill_path(root.path(), "skills/a/SKILL.md").unwrap();
		assert!(got.starts_with(root.path().canonicalize().unwrap()));
	}
	#[cfg(unix)]
	#[test]
	fn rejects_symlink_escape() {
		use std::os::unix::fs::symlink;
		let root = tempdir().unwrap();
		let outside = tempdir().unwrap();
		fs::write(outside.path().join("SKILL.md"), b"x").unwrap();
		symlink(outside.path(), root.path().join("escape")).unwrap();
		assert_eq!(sanitize_skill_path(root.path(), "escape/SKILL.md"), None);
	}

	#[test]
	fn same_content_is_up_to_date() {
		let d = tempdir().unwrap();
		fs::write(d.path().join("SKILL.md"), b"x").unwrap();
		let h = skill::compute_skill_folder_hash(d.path()).unwrap();
		assert_eq!(
			compare_hashes(Some(&h), d.path()).unwrap(),
			SkillUpdateStatus::UpToDate
		);
	}
	#[test]
	fn changed_content_is_update_available() {
		let d = tempdir().unwrap();
		fs::write(d.path().join("SKILL.md"), b"NEW").unwrap();
		let st = compare_hashes(Some("oldhash"), d.path()).unwrap();
		assert!(matches!(st, SkillUpdateStatus::UpdateAvailable { .. }));
	}
	#[test]
	fn missing_hash_recomputes_no_false_positive() {
		let d = tempdir().unwrap();
		fs::write(d.path().join("SKILL.md"), b"x").unwrap();
		assert_eq!(
			compare_hashes(None, d.path()).unwrap(),
			SkillUpdateStatus::UpToDate
		);
	}
	#[test]
	fn placeholder_hash_auto_heals() {
		let d = tempdir().unwrap();
		fs::write(d.path().join("SKILL.md"), b"x").unwrap();
		let st =
			compare_hashes(Some(skill::EMPTY_SKILLS_LOCK_DIGEST), d.path())
				.unwrap();
		assert_eq!(st, SkillUpdateStatus::UpToDate);
	}
}
