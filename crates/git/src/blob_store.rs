//! On-disk, content-addressed blob store shared across processes.
//!
//! A git blob oid IS the hash of its bytes, so an entry can never go stale and
//! needs no invalidation: every read re-hashes and treats a mismatch (torn
//! write, disk corruption, tampering) as a miss. That makes it safe to share
//! between repositories and credentials too — a hit only returns bytes whose
//! hash the caller already holds from a tree it was allowed to read.
//!
//! Writes go to a temp file in the target dir and are renamed into place, so
//! concurrent CLI / desktop / scheduled runs never see a half-written entry.
//! Every failure is a miss or a skipped write, never an error: this is a cache.

use std::path::PathBuf;

use crate::tree::BlobHasher;

/// Directory of blobs keyed by lowercase SHA-1 oid (`<oid[..2]>/<oid[2..]>`).
// ponytail: unbounded. Skill repos are small and an entry is never wrong; add
// an mtime-based sweep if a user's store ever grows large enough to matter.
#[derive(Clone, Debug)]
pub struct BlobStore {
	dir: PathBuf,
}

impl BlobStore {
	/// A store rooted at `dir` (created lazily on first write).
	pub fn new(dir: impl Into<PathBuf>) -> Self {
		Self { dir: dir.into() }
	}

	/// The entry path, or `None` for anything that is not a 40-hex oid — the
	/// oid comes from a server response and must never steer a path.
	fn path(&self, oid: &str) -> Option<PathBuf> {
		let valid = oid.len() == 40
			&& oid.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
		valid.then(|| self.dir.join(&oid[..2]).join(&oid[2..]))
	}

	/// Whether an entry exists. Cheap; does not verify — [`Self::get`] does.
	pub fn contains(&self, oid: &str) -> bool {
		self.path(oid).is_some_and(|p| p.is_file())
	}

	/// The bytes for `oid`, only if they still hash to it.
	pub fn get(&self, oid: &str) -> Option<Vec<u8>> {
		let bytes = std::fs::read(self.path(oid)?).ok()?;
		(blob_oid(&bytes).as_deref() == Some(oid)).then_some(bytes)
	}

	/// Store `bytes` under `oid` if they hash to it; otherwise do nothing.
	/// Overwrites: callers only `put` after a verified miss, and a corrupted
	/// entry must be replaced or it would miss forever.
	pub fn put(&self, oid: &str, bytes: &[u8]) {
		let Some(path) = self.path(oid) else { return };
		if blob_oid(bytes).as_deref() != Some(oid) {
			return;
		}
		let Some(parent) = path.parent() else { return };
		let written = std::fs::create_dir_all(parent)
			.and_then(|()| tempfile::NamedTempFile::new_in(parent))
			.and_then(|mut tmp| {
				std::io::Write::write_all(&mut tmp, bytes)?;
				tmp.persist(&path).map_err(|e| e.error)?;
				Ok(())
			});
		if let Err(e) = written {
			log::debug!("blob store: skipped writing {oid}: {e}");
		}
	}
}

fn blob_oid(bytes: &[u8]) -> Option<String> {
	let mut hasher = BlobHasher::new(bytes.len() as u64);
	hasher.update(bytes);
	hasher.finish()
}

#[cfg(test)]
mod tests {
	use super::*;

	// `git hash-object` of "hello\n".
	const HELLO: &str = "ce013625030ba8dba906f756967f9e9ca394464a";

	#[test]
	fn round_trips_and_rejects_what_does_not_hash() {
		let dir = tempfile::tempdir().unwrap();
		let store = BlobStore::new(dir.path());
		store.put(HELLO, b"not hello\n");
		assert!(
			!store.contains(HELLO),
			"mismatched bytes must not be stored"
		);
		store.put(HELLO, b"hello\n");
		assert_eq!(store.get(HELLO).as_deref(), Some(&b"hello\n"[..]));
	}

	#[test]
	fn a_corrupted_entry_reads_as_a_miss() {
		let dir = tempfile::tempdir().unwrap();
		let store = BlobStore::new(dir.path());
		store.put(HELLO, b"hello\n");
		std::fs::write(store.path(HELLO).unwrap(), b"tampered\n").unwrap();
		assert_eq!(store.get(HELLO), None);
	}

	#[test]
	fn a_non_oid_never_becomes_a_path() {
		let store = BlobStore::new("/nonexistent");
		assert!(store.path("../../etc/passwd").is_none());
		assert!(store.path(&"A".repeat(40)).is_none());
	}
}
