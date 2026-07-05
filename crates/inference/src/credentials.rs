//! API key storage for inference providers.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::error::Result;

const KEYRING_SERVICE: &str = "aghub.inference_provider";

/// Stores provider API keys outside of `inference_providers.json`.
pub trait CredentialStore {
	/// Read a provider API key.
	fn get_api_key(&self, provider_id: &str) -> Result<Option<String>>;

	/// Store a provider API key.
	fn set_api_key(&self, provider_id: &str, api_key: &str) -> Result<()>;

	/// Delete a provider API key.
	fn delete_api_key(&self, provider_id: &str) -> Result<()>;
}

/// Platform-native keyring implementation.
///
/// OS keyring backends may not support concurrent writes reliably. Callers
/// sharing this store across threads should serialize access themselves.
#[derive(Debug, Clone, Copy, Default)]
pub struct NativeCredentialStore;

impl NativeCredentialStore {
	fn entry(provider_id: &str) -> Result<keyring::Entry> {
		let user = format!("provider:{provider_id}:api_key");
		Ok(keyring::Entry::new(KEYRING_SERVICE, &user)?)
	}
}

impl CredentialStore for NativeCredentialStore {
	fn get_api_key(&self, provider_id: &str) -> Result<Option<String>> {
		let entry = Self::entry(provider_id)?;
		match entry.get_password() {
			Ok(api_key) => Ok(Some(api_key)),
			Err(keyring::Error::NoEntry) => Ok(None),
			Err(error) => Err(error.into()),
		}
	}

	fn set_api_key(&self, provider_id: &str, api_key: &str) -> Result<()> {
		let entry = Self::entry(provider_id)?;
		entry.set_password(api_key)?;
		Ok(())
	}

	/// Delete is **idempotent and best-effort across platforms**.
	///
	/// Removing a provider must succeed even if its keychain entry is absent or
	/// the backend is unreachable — the inventory removal is the real
	/// operation; the key is being discarded either way. `NoEntry` is the
	/// "already gone" case (Linux secret-service); other backends (e.g. the
	/// macOS keychain) report an absent/locked entry as a *different* error, so
	/// we log and swallow any error rather than fail the delete. `get`/`set`
	/// keep surfacing errors — only delete is best-effort.
	fn delete_api_key(&self, provider_id: &str) -> Result<()> {
		let entry = Self::entry(provider_id)?;
		match entry.delete_credential() {
			Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
			Err(error) => {
				log::warn!(
					"ignoring keyring delete error for provider {provider_id}: \
					 {error}"
				);
				Ok(())
			}
		}
	}
}

/// Plaintext-JSON credential store for headless environments (tests, CI) where
/// no OS keyring is available.
///
/// This is NOT a security backend — keys are stored unencrypted in a file the
/// caller names. It exists so the CLI/API inference paths can be exercised
/// end-to-end without a real keyring (where `linux-native` needs a running
/// secret-service / dbus session). Selected at runtime by the CLI via
/// `$AGHUB_TEST_CREDENTIAL_FILE`; never the default.
#[derive(Debug, Clone)]
pub struct FileCredentialStore {
	path: PathBuf,
}

impl FileCredentialStore {
	/// Back the store with the JSON file at `path` (created on first write).
	pub fn new(path: impl Into<PathBuf>) -> Self {
		Self { path: path.into() }
	}

	fn load(&self) -> Result<BTreeMap<String, String>> {
		match std::fs::read(&self.path) {
			Ok(bytes) => {
				let map = serde_json::from_slice(&bytes).map_err(|e| {
					crate::error::InferenceProviderError::Keyring(format!(
						"corrupt credential file {}: {e}",
						self.path.display()
					))
				})?;
				Ok(map)
			}
			Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
				Ok(BTreeMap::new())
			}
			Err(e) => Err(e.into()),
		}
	}

	fn store(&self, map: &BTreeMap<String, String>) -> Result<()> {
		if let Some(parent) = self.path.parent() {
			std::fs::create_dir_all(parent)?;
		}
		let bytes = serde_json::to_vec(map).map_err(|e| {
			crate::error::InferenceProviderError::Keyring(e.to_string())
		})?;
		atomic_write(&self.path, &bytes)
	}
}

/// Write `bytes` to `path` atomically (temp file + rename) so a crash mid-write
/// can never truncate the existing key file.
fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
	let tmp = path.with_extension("tmp");
	std::fs::write(&tmp, bytes)?;
	std::fs::rename(&tmp, path)?;
	Ok(())
}

impl CredentialStore for FileCredentialStore {
	fn get_api_key(&self, provider_id: &str) -> Result<Option<String>> {
		Ok(self.load()?.get(provider_id).cloned())
	}

	fn set_api_key(&self, provider_id: &str, api_key: &str) -> Result<()> {
		let mut map = self.load()?;
		map.insert(provider_id.to_string(), api_key.to_string());
		self.store(&map)
	}

	fn delete_api_key(&self, provider_id: &str) -> Result<()> {
		let mut map = self.load()?;
		map.remove(provider_id);
		self.store(&map)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn file_store_roundtrips_set_get_delete() {
		let dir = tempfile::tempdir().unwrap();
		let store = FileCredentialStore::new(dir.path().join("creds.json"));

		assert_eq!(store.get_api_key("p1").unwrap(), None);
		store.set_api_key("p1", "secret").unwrap();
		assert_eq!(
			store.get_api_key("p1").unwrap(),
			Some("secret".to_string())
		);
		store.delete_api_key("p1").unwrap();
		assert_eq!(store.get_api_key("p1").unwrap(), None);
		// Deleting a missing key is a no-op, not an error.
		store.delete_api_key("p1").unwrap();
	}

	#[test]
	fn file_store_persists_across_instances() {
		// A second store over the same path must see the first store's writes —
		// this is what lets a CLI subprocess and an in-test store share one key
		// file (the headless replacement for a shared keyring namespace).
		let dir = tempfile::tempdir().unwrap();
		let path = dir.path().join("creds.json");
		FileCredentialStore::new(&path)
			.set_api_key("p1", "secret")
			.unwrap();
		assert_eq!(
			FileCredentialStore::new(&path).get_api_key("p1").unwrap(),
			Some("secret".to_string())
		);
	}
}
