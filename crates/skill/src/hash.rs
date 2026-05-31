//! SHA-256 folder hashing, byte-for-byte compatible with npx `computeSkillFolderHash`
//! (vercel-labs/skills `local-lock.ts:108-147`). Hash the SOURCE folder, never the
//! post-copy installed dir.

use sha2::{Digest, Sha256};
use std::io;
use std::path::Path;

/// Bounds guard (F1 hashes untrusted fetched content).
pub const MAX_FILES: usize = 10_000;
pub const MAX_TOTAL_BYTES: u64 = 256 * 1024 * 1024; // 256 MiB
pub const MAX_DEPTH: usize = 10; // mirrors install.rs scan max_depth

/// SHA-256 of the empty input — the legacy aghub placeholder.
pub const EMPTY_SKILLS_LOCK_DIGEST: &str =
	"e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

#[derive(Debug, thiserror::Error)]
pub enum HashError {
	#[error("io error hashing skill folder: {0}")]
	Io(#[from] io::Error),
	#[error("skill folder exceeds bounds: {0}")]
	Bounds(String),
}

/// True if `hash` is the empty-input placeholder (treat as "unknown" → recompute).
pub fn is_placeholder_digest(hash: &str) -> bool {
	hash == EMPTY_SKILLS_LOCK_DIGEST
}

/// Reimplements npx `computeSkillFolderHash` byte-for-byte. Returns lowercase hex.
///
/// Algorithm (local-lock.ts:108-147): collect files recursively, skip dirs named
/// exactly `.git`/`node_modules`, lstat to skip symlinks (no descend), relative
/// path with `\` → `/`, sort by Unicode code point, then for each file in order
/// `update(relative_path_bytes)` + `update(file_bytes)` with no delimiter.
pub fn compute_skill_folder_hash(dir: &Path) -> Result<String, HashError> {
	let mut files: Vec<(String, std::path::PathBuf)> = Vec::new();
	let mut total_bytes: u64 = 0;
	collect(dir, dir, 0, &mut files, &mut total_bytes)?;

	// Code-point sort == JS localeCompare for ASCII (the common case); non-ASCII
	// may diverge — accepted, guarded by recompute-on-mismatch in F1.
	files.sort_by(|a, b| a.0.cmp(&b.0));

	let mut hasher = Sha256::new();
	for (rel, abs) in &files {
		let bytes = std::fs::read(abs)?;
		hasher.update(rel.as_bytes());
		hasher.update(&bytes);
	}
	Ok(format!("{:x}", hasher.finalize()))
}

fn collect(
	root: &Path,
	dir: &Path,
	depth: usize,
	out: &mut Vec<(String, std::path::PathBuf)>,
	total_bytes: &mut u64,
) -> Result<(), HashError> {
	if depth > MAX_DEPTH {
		return Err(HashError::Bounds(format!(
			"max depth {MAX_DEPTH} exceeded"
		)));
	}
	for entry in std::fs::read_dir(dir)? {
		let entry = entry?;
		let path = entry.path();
		// lstat — do not follow symlinks.
		let meta = std::fs::symlink_metadata(&path)?;
		let ft = meta.file_type();
		if ft.is_symlink() {
			continue; // skip symlinks; never descend into symlinked dirs
		}
		if ft.is_dir() {
			let name = entry.file_name();
			if name == ".git" || name == "node_modules" {
				continue;
			}
			collect(root, &path, depth + 1, out, total_bytes)?;
		} else if ft.is_file() {
			if out.len() + 1 > MAX_FILES {
				return Err(HashError::Bounds(format!(
					"max files {MAX_FILES} exceeded"
				)));
			}
			*total_bytes += meta.len();
			if *total_bytes > MAX_TOTAL_BYTES {
				return Err(HashError::Bounds(format!(
					"max total bytes {MAX_TOTAL_BYTES} exceeded"
				)));
			}
			let rel = path
				.strip_prefix(root)
				.unwrap_or(&path)
				.to_string_lossy()
				.replace('\\', "/");
			out.push((rel, path));
		}
	}
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;
	use sha2::{Digest, Sha256};
	use std::fs;
	use tempfile::tempdir;

	fn hex(bytes: &[u8]) -> String {
		let mut h = Sha256::new();
		h.update(bytes);
		format!("{:x}", h.finalize())
	}

	#[test]
	fn empty_folder_hashes_to_empty_sha256() {
		let dir = tempdir().unwrap();
		assert_eq!(
			compute_skill_folder_hash(dir.path()).unwrap(),
			EMPTY_SKILLS_LOCK_DIGEST
		);
	}

	#[test]
	fn single_file_path_then_content_no_delimiter() {
		let dir = tempdir().unwrap();
		fs::write(dir.path().join("README.md"), b"hello world").unwrap();
		let mut expected = Sha256::new();
		expected.update(b"README.md");
		expected.update(b"hello world");
		assert_eq!(
			compute_skill_folder_hash(dir.path()).unwrap(),
			format!("{:x}", expected.finalize())
		);
	}

	#[test]
	fn files_sorted_by_codepoint() {
		let dir = tempdir().unwrap();
		fs::write(dir.path().join("zebra.txt"), b"z").unwrap();
		fs::write(dir.path().join("apple.txt"), b"a").unwrap();
		fs::write(dir.path().join("middle.txt"), b"m").unwrap();
		let mut e = Sha256::new();
		for (p, c) in
			[("apple.txt", "a"), ("middle.txt", "m"), ("zebra.txt", "z")]
		{
			e.update(p.as_bytes());
			e.update(c.as_bytes());
		}
		assert_eq!(
			compute_skill_folder_hash(dir.path()).unwrap(),
			format!("{:x}", e.finalize())
		);
	}

	#[test]
	fn nested_relative_paths_use_forward_slash() {
		let dir = tempdir().unwrap();
		fs::create_dir_all(dir.path().join("a")).unwrap();
		fs::create_dir_all(dir.path().join("b/c")).unwrap();
		fs::write(dir.path().join("file0.txt"), b"0").unwrap();
		fs::write(dir.path().join("a/file1.txt"), b"1").unwrap();
		fs::write(dir.path().join("b/c/file2.txt"), b"2").unwrap();
		let mut e = Sha256::new();
		for (p, c) in [
			("a/file1.txt", "1"),
			("b/c/file2.txt", "2"),
			("file0.txt", "0"),
		] {
			e.update(p.as_bytes());
			e.update(c.as_bytes());
		}
		assert_eq!(
			compute_skill_folder_hash(dir.path()).unwrap(),
			format!("{:x}", e.finalize())
		);
	}

	#[test]
	fn skips_dot_git_and_node_modules_only() {
		let dir = tempdir().unwrap();
		fs::write(dir.path().join("SKILL.md"), b"x").unwrap();
		fs::create_dir_all(dir.path().join(".git/objects")).unwrap();
		fs::write(dir.path().join(".git/objects/abc"), b"junk").unwrap();
		fs::create_dir_all(dir.path().join("node_modules/pkg")).unwrap();
		fs::write(dir.path().join("node_modules/pkg/index.js"), b"junk")
			.unwrap();
		// dist/__pycache__ must NOT be skipped
		fs::create_dir_all(dir.path().join("dist")).unwrap();
		fs::write(dir.path().join("dist/out.js"), b"keep").unwrap();
		let mut e = Sha256::new();
		e.update(b"SKILL.md");
		e.update(b"x");
		e.update(b"dist/out.js");
		e.update(b"keep");
		assert_eq!(
			compute_skill_folder_hash(dir.path()).unwrap(),
			format!("{:x}", e.finalize())
		);
	}

	#[test]
	fn returns_lowercase_hex_64() {
		let dir = tempdir().unwrap();
		fs::write(dir.path().join("x"), b"y").unwrap();
		let h = compute_skill_folder_hash(dir.path()).unwrap();
		assert_eq!(h.len(), 64);
		assert!(h
			.chars()
			.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()));
	}

	#[test]
	fn deterministic() {
		let dir = tempdir().unwrap();
		fs::write(dir.path().join("a"), b"1").unwrap();
		fs::write(dir.path().join("b"), b"2").unwrap();
		assert_eq!(
			compute_skill_folder_hash(dir.path()).unwrap(),
			compute_skill_folder_hash(dir.path()).unwrap()
		);
	}

	#[cfg(unix)]
	#[test]
	fn symlinks_are_skipped_not_followed() {
		use std::os::unix::fs::symlink;
		let dir = tempdir().unwrap();
		fs::write(dir.path().join("real.txt"), b"r").unwrap();
		symlink(dir.path().join("real.txt"), dir.path().join("link.txt"))
			.unwrap();
		// symlinked directory must not be descended
		let outside = tempdir().unwrap();
		fs::write(outside.path().join("secret"), b"s").unwrap();
		symlink(outside.path(), dir.path().join("linkdir")).unwrap();
		let mut e = Sha256::new();
		e.update(b"real.txt");
		e.update(b"r");
		assert_eq!(
			compute_skill_folder_hash(dir.path()).unwrap(),
			format!("{:x}", e.finalize())
		);
	}

	#[test]
	fn nonexistent_dir_is_io_error() {
		let dir = tempdir().unwrap();
		let missing = dir.path().join("does-not-exist");
		assert!(matches!(
			compute_skill_folder_hash(&missing),
			Err(HashError::Io(_))
		));
	}

	#[test]
	fn exceeding_max_files_is_bounds_error() {
		let dir = tempdir().unwrap();
		for i in 0..(MAX_FILES + 1) {
			fs::write(dir.path().join(format!("f{i}")), b"").unwrap();
		}
		assert!(matches!(
			compute_skill_folder_hash(dir.path()),
			Err(HashError::Bounds(_))
		));
	}

	#[allow(dead_code)]
	fn _hex_used(_: &[u8]) {
		let _ = hex;
	}
}
