use gix::bstr::ByteSlice;
use std::path::Path;

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

#[cfg(test)]
mod tests {
	use super::*;

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
}
