//! npx `add.ts:1568-1575` skill_path form: POSIX `<repo-relative-dir>/SKILL.md`,
//! case-preserving; root-level skill → `SKILL.md`.

use std::path::Path;

/// `repo_root` and `skill_dir` are absolute. Returns the npx skill_path or None
/// if `skill_dir` is not inside `repo_root`.
pub fn skill_path_from_repo_dir(
	repo_root: &Path,
	skill_dir: &Path,
) -> Option<String> {
	let rel = skill_dir.strip_prefix(repo_root).ok()?;
	let rel = rel.to_string_lossy().replace('\\', "/");
	let rel = rel.trim_matches('/');
	if rel.is_empty() {
		Some("SKILL.md".to_string())
	} else {
		Some(format!("{rel}/SKILL.md"))
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::path::Path;

	#[test]
	fn root_level_skill() {
		assert_eq!(
			skill_path_from_repo_dir(
				Path::new("/tmp/repo"),
				Path::new("/tmp/repo")
			),
			Some("SKILL.md".to_string())
		);
	}

	#[test]
	fn nested_preserves_case_and_uses_forward_slash() {
		assert_eq!(
			skill_path_from_repo_dir(
				Path::new("/tmp/repo"),
				Path::new("/tmp/repo/skills/MySkill")
			),
			Some("skills/MySkill/SKILL.md".to_string())
		);
	}

	#[test]
	fn outside_repo_is_none() {
		assert_eq!(
			skill_path_from_repo_dir(
				Path::new("/tmp/repo"),
				Path::new("/other/x")
			),
			None
		);
	}
}
