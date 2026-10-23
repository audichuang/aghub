use gix::bstr::ByteSlice;
use std::path::Path;

const MAX_TREE_DEPTH: usize = 64;

/// Streaming git blob object id (SHA-1 over `blob <len>\0<bytes>`), the oid a
/// tree lists for a file with these bytes.
pub struct BlobHasher {
	hasher: gix::hash::Hasher,
	declared: u64,
	seen: u64,
}

impl BlobHasher {
	/// `declared_len` goes into the object header, so it must be the byte
	/// count that [`Self::update`] will be fed; [`Self::finish`] checks it.
	pub fn new(declared_len: u64) -> Self {
		let mut hasher = gix::hash::hasher(gix::hash::Kind::Sha1);
		hasher.update(format!("blob {declared_len}\0").as_bytes());
		Self {
			hasher,
			declared: declared_len,
			seen: 0,
		}
	}

	pub fn update(&mut self, bytes: &[u8]) {
		self.seen = self.seen.saturating_add(bytes.len() as u64);
		self.hasher.update(bytes);
	}

	/// Lowercase hex oid, or `None` when the fed byte count differs from the
	/// declared length (the file changed under the read) or SHA-1 collision
	/// detection fired.
	pub fn finish(self) -> Option<String> {
		if self.seen != self.declared {
			return None;
		}
		self.hasher.try_finalize().ok().map(|oid| oid.to_string())
	}
}

pub fn is_safe_tree_entry_name(name: &[u8]) -> bool {
	if name.is_empty() || name == b"." || name == b".." {
		return false;
	}
	if name.iter().any(|b| matches!(b, b'\0' | b'/' | b'\\')) {
		return false;
	}
	if cfg!(windows) && name.contains(&b':') {
		return false;
	}
	true
}

pub fn materialize_tree(
	repo: &gix::Repository,
	tree_id: gix::ObjectId,
	dest: &Path,
) -> std::io::Result<()> {
	materialize_tree_at_depth(repo, tree_id, dest, 0)
}

fn materialize_tree_at_depth(
	repo: &gix::Repository,
	tree_id: gix::ObjectId,
	dest: &Path,
	depth: usize,
) -> std::io::Result<()> {
	check_tree_depth(depth)?;
	std::fs::create_dir_all(dest)?;
	let tree = repo
		.find_tree(tree_id)
		.map_err(|e| std::io::Error::other(e.to_string()))?;
	for entry in tree.iter() {
		let entry = entry.map_err(|e| std::io::Error::other(e.to_string()))?;
		let name = entry.filename();
		if !is_safe_tree_entry_name(name.as_ref()) {
			return Err(std::io::Error::new(
				std::io::ErrorKind::InvalidData,
				format!("unsafe git tree entry name: {}", name.to_str_lossy()),
			));
		}
		let target = dest.join(name.to_str_lossy().as_ref());
		if entry.mode().is_tree() {
			materialize_tree_at_depth(
				repo,
				entry.object_id(),
				&target,
				depth + 1,
			)?;
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

fn check_tree_depth(depth: usize) -> std::io::Result<()> {
	if depth > MAX_TREE_DEPTH {
		return Err(std::io::Error::new(
			std::io::ErrorKind::InvalidData,
			format!("max git tree depth {MAX_TREE_DEPTH} exceeded"),
		));
	}
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;

	/// Goldens from `git hash-object --stdin`.
	#[test]
	fn blob_hasher_matches_git_hash_object() {
		let mut hello = BlobHasher::new(6);
		hello.update(b"hel");
		hello.update(b"lo\n");
		assert_eq!(
			hello.finish().as_deref(),
			Some("ce013625030ba8dba906f756967f9e9ca394464a")
		);
		assert_eq!(
			BlobHasher::new(0).finish().as_deref(),
			Some("e69de29bb2d1d6434b8b29ae775ad8c2e48c5391")
		);
		let mut short = BlobHasher::new(7);
		short.update(b"hello\n");
		assert_eq!(short.finish(), None, "length mismatch yields no oid");
	}

	#[test]
	fn safe_tree_entry_name_accepts_plain_names() {
		assert!(is_safe_tree_entry_name(b"SKILL.md"));
		assert!(is_safe_tree_entry_name(b"skill_name-1.0"));
	}

	#[test]
	fn safe_tree_entry_name_rejects_special_components() {
		assert!(!is_safe_tree_entry_name(b""));
		assert!(!is_safe_tree_entry_name(b"."));
		assert!(!is_safe_tree_entry_name(b".."));
	}

	#[test]
	fn safe_tree_entry_name_rejects_separators_and_nul() {
		assert!(!is_safe_tree_entry_name(b"a/b"));
		assert!(!is_safe_tree_entry_name(b"a\\b"));
		assert!(!is_safe_tree_entry_name(b"a\0b"));
	}

	#[cfg(windows)]
	#[test]
	fn safe_tree_entry_name_rejects_windows_colon_names() {
		assert!(!is_safe_tree_entry_name(b"C:"));
		assert!(!is_safe_tree_entry_name(b"c:secret"));
		assert!(!is_safe_tree_entry_name(b"a:b"));
	}

	#[cfg(not(windows))]
	#[test]
	fn safe_tree_entry_name_accepts_unix_colon_names() {
		assert!(is_safe_tree_entry_name(b"C:"));
		assert!(is_safe_tree_entry_name(b"c:secret"));
		assert!(is_safe_tree_entry_name(b"a:b"));
	}

	#[test]
	fn tree_depth_limit_rejects_excessive_recursion() {
		assert!(check_tree_depth(MAX_TREE_DEPTH).is_ok());
		assert!(check_tree_depth(MAX_TREE_DEPTH + 1).is_err());
	}
}
