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
		if let Err(e) = write_atomic(&path, bytes) {
			log::debug!("blob store: skipped writing {oid}: {e}");
		}
	}
}

/// On-disk ETag cache for GitHub JSON GETs (`/commits/<ref>`, `/git/trees/<sha>`).
///
/// A conditional request answered `304 Not Modified` while authorized does not
/// count against the REST rate limit, so a repeated tip check of an unmoved
/// source costs nothing. Keyed by the full URL (no token): the SERVER decides
/// the 304 for this request's own credential — one that cannot read the repo
/// gets a 404, never the cached body. Each entry stores its URL and is ignored
/// unless it matches, so a key-hash collision is a miss. Failures are misses.
// ponytail: unbounded, like `BlobStore`; one small file per URL ever fetched.
#[derive(Clone, Debug)]
pub struct EtagStore {
	dir: PathBuf,
}

impl EtagStore {
	/// A store rooted at `dir` (created lazily on first write).
	pub fn new(dir: impl Into<PathBuf>) -> Self {
		Self { dir: dir.into() }
	}

	fn path(&self, url: &str) -> Option<PathBuf> {
		let key = blob_oid(url.as_bytes())?;
		Some(self.dir.join(&key[..2]).join(&key[2..]))
	}

	/// `(etag, body)` last stored for exactly `url`.
	pub fn get(&self, url: &str) -> Option<(String, Vec<u8>)> {
		let raw = std::fs::read(self.path(url)?).ok()?;
		let mut parts = raw.splitn(3, |b| *b == b'\n');
		let (stored_url, etag, body) =
			(parts.next()?, parts.next()?, parts.next()?);
		if stored_url != url.as_bytes() || etag.is_empty() {
			return None;
		}
		Some((String::from_utf8(etag.to_vec()).ok()?, body.to_vec()))
	}

	/// Remember `body` as the representation `etag` names for `url`.
	pub fn put(&self, url: &str, etag: &str, body: &[u8]) {
		if url.contains('\n') || etag.contains('\n') || etag.is_empty() {
			return;
		}
		let Some(path) = self.path(url) else { return };
		let mut raw = format!("{url}\n{etag}\n").into_bytes();
		raw.extend_from_slice(body);
		if let Err(e) = write_atomic(&path, &raw) {
			log::debug!("etag store: skipped writing {url}: {e}");
		}
	}
}

/// Temp file in the target dir, then rename: concurrent CLI / desktop /
/// scheduled runs never see a half-written entry.
fn write_atomic(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
	let parent = path
		.parent()
		.ok_or_else(|| std::io::Error::other("entry path has no parent"))?;
	std::fs::create_dir_all(parent)?;
	let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
	std::io::Write::write_all(&mut tmp, bytes)?;
	tmp.persist(path).map_err(|e| e.error)?;
	Ok(())
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
	fn an_etag_entry_serves_only_its_own_url() {
		let dir = tempfile::tempdir().unwrap();
		let store = EtagStore::new(dir.path());
		store.put("https://h/a", "W/\"1\"", b"{\"sha\":\"x\"}\n");
		assert_eq!(
			store.get("https://h/a"),
			Some(("W/\"1\"".to_string(), b"{\"sha\":\"x\"}\n".to_vec()))
		);
		assert_eq!(store.get("https://h/b"), None);
		// A colliding key on disk still names its own URL: a miss, never b's body.
		let b_path = store.path("https://h/b").unwrap();
		std::fs::create_dir_all(b_path.parent().unwrap()).unwrap();
		std::fs::copy(store.path("https://h/a").unwrap(), b_path).unwrap();
		assert_eq!(store.get("https://h/b"), None);
	}

	#[test]
	fn a_non_oid_never_becomes_a_path() {
		let store = BlobStore::new("/nonexistent");
		assert!(store.path("../../etc/passwd").is_none());
		assert!(store.path(&"A".repeat(40)).is_none());
	}
}
