//! Validated repo-relative skill folder paths.
//!
//! A [`SkillPath`] is the only shape allowed to reach a fetch/install join:
//! never absolute, never containing `..`, so joining under a clone root cannot
//! escape that root.

use std::path::{Path, PathBuf};

/// A validated, repo-relative POSIX skill *folder* path.
///
/// Guarantees, checked at construction: not absolute, no leading `/`, no `..`
/// component, no Windows drive prefix — so joining it under any root can never
/// escape that root. The empty path denotes the repo-root skill. This is the
/// only shape allowed to reach a fetch/install join.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillPath(String);

/// Why a candidate skill path was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillPathError {
	/// Absolute path, leading `/`, or a Windows drive / UNC prefix.
	Absolute,
	/// Contains a `..` component that could escape the root.
	ParentTraversal,
}

impl SkillPath {
	/// Validate a repo-relative skill folder path. Backslashes are normalized
	/// to `/` first; the empty string is the repo-root skill.
	pub fn parse(input: &str) -> Result<Self, SkillPathError> {
		// Platform-independent: do not use Path::is_absolute (OS-dependent).
		let norm = input.replace('\\', "/");
		if norm.is_empty() {
			return Ok(SkillPath(String::new()));
		}
		// Leading `/` covers Unix absolute paths and UNC (`//server/...`).
		if norm.starts_with('/') {
			return Err(SkillPathError::Absolute);
		}
		// Windows drive prefix: `C:/...`, `C:foo` (byte at index 1 is `:`).
		if norm.as_bytes().get(1) == Some(&b':') {
			return Err(SkillPathError::Absolute);
		}
		if norm.split('/').any(|c| c == "..") {
			return Err(SkillPathError::ParentTraversal);
		}
		Ok(SkillPath(norm))
	}

	/// The normalized POSIX relative folder string (`""` for the root skill).
	pub fn as_str(&self) -> &str {
		&self.0
	}

	/// Whether this is the repo-root skill (empty path).
	pub fn is_root(&self) -> bool {
		self.0.is_empty()
	}

	/// Join under `root`, guaranteed to stay inside it. For the root skill
	/// this returns `root` itself.
	pub fn resolve_under(&self, root: &Path) -> PathBuf {
		if self.is_root() {
			root.to_path_buf()
		} else {
			root.join(self.as_str())
		}
	}
}
