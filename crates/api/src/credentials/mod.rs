//! Credential helpers for skill source fetching.
//!
//! Resolution lives here (in `crates/api`) so that `crates/core` stays pure:
//! core receives an already-resolved `Option<token>` and never touches the
//! keyring or the network.

// The resolver and binding store are consumed by the update-check orchestration
// (Task F1.5) via the `routes::skills_update` route.
pub(crate) mod resolve;
pub(crate) mod source_auth;

use std::collections::HashMap;

// Normalized `(scheme, host, port)` origin used to pin a credential/token to
// the exact place it came from. Reused by the skills git-scan host guard.
pub(crate) mod origin;

// Controller-side public resolution API re-exported from `lib.rs` for the
// desktop `src-tauri` layer (remote git-credential forwarding).
pub(crate) mod public;

// Forwarded git-credential primitives (resolver + request guard + chain).
// Wired into the sources/diff, check-updates, apply-update, and git-scan
// routes (remote git-credential forwarding).
pub(crate) mod forwarding;

use serde::de::DeserializeOwned;
use serde::Serialize;
use std::marker::PhantomData;

/// Classifies a github-credential/source-binding keyring failure so callers
/// can distinguish "the OS backend itself isn't reachable" (retryable, no
/// mutation should be assumed to have happened) from every other failure
/// (corrupt JSON, bad encoding, ...). Delegates to
/// `aghub_inference::keyring_backend_unavailable` so this and
/// `InferenceProviderError`'s equivalent `From<keyring::Error>` never diverge
/// on the classification. Both funnel into the same `KEYCHAIN_UNAVAILABLE`
/// status/code/message in `crate::error::ApiError`.
pub(crate) enum CredentialStoreError {
	/// The backend itself could not be reached.
	Unavailable(String),
	/// Any other failure (corrupt JSON, bad encoding, ...).
	Other(String),
}

impl std::fmt::Display for CredentialStoreError {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			CredentialStoreError::Unavailable(msg)
			| CredentialStoreError::Other(msg) => write!(f, "{msg}"),
		}
	}
}

impl From<keyring::Error> for CredentialStoreError {
	fn from(error: keyring::Error) -> Self {
		if aghub_inference::keyring_backend_unavailable(&error) {
			CredentialStoreError::Unavailable(error.to_string())
		} else {
			CredentialStoreError::Other(error.to_string())
		}
	}
}

impl From<serde_json::Error> for CredentialStoreError {
	fn from(error: serde_json::Error) -> Self {
		CredentialStoreError::Other(error.to_string())
	}
}

/// Distinguishes the "empty" (delete-worthy) state of a keyring-backed JSON
/// payload. This is the only difference between the github-credentials store
/// and the source-bindings store besides the `(service, user)` pair each
/// closes over — see [`KeyringJson`].
pub(crate) trait KeyringPayload:
	Default + Serialize + DeserializeOwned
{
	fn is_empty(&self) -> bool;
}

impl KeyringPayload for Vec<crate::routes::credentials::StoredCredential> {
	fn is_empty(&self) -> bool {
		self.is_empty()
	}
}

impl KeyringPayload for resolve::SourceBindings {
	fn is_empty(&self) -> bool {
		self.0.is_empty()
	}
}

/// A single JSON-blob-in-one-keyring-entry seam: round-trips a
/// [`KeyringPayload`] `T` through one `(service, user)` keyring entry as a
/// JSON string, deleting the entry when `T::is_empty()`. Shared by the
/// github-credentials store and the source-bindings store (see
/// `credentials_store`/`source_bindings_store` below) — the only difference
/// between the two is the `(service, user)` pair.
///
/// Deliberately does NOT lock — locking stays at route level (see
/// `routes::credentials::lock_credential_store`), since some flows (e.g.
/// credential delete) need ONE guard held across a `load`+`store` pair on
/// BOTH this store and another `KeyringJson` instance.
pub(crate) struct KeyringJson<T> {
	service: &'static str,
	user: &'static str,
	_payload: PhantomData<T>,
}

impl<T: KeyringPayload> KeyringJson<T> {
	const fn new(service: &'static str, user: &'static str) -> Self {
		Self {
			service,
			user,
			_payload: PhantomData,
		}
	}

	fn entry(&self) -> Result<keyring::Entry, CredentialStoreError> {
		Ok(keyring::Entry::new(self.service, self.user)?)
	}

	pub(crate) fn load(&self) -> Result<T, CredentialStoreError> {
		if let Some(cached) = cache_get(self.service, self.user) {
			return match cached {
				Some(json) => Ok(serde_json::from_str(&json)?),
				None => Ok(T::default()),
			};
		}
		let entry = self.entry()?;
		let started = std::time::Instant::now();
		let read = entry.get_password();
		log::info!(
			"keyring read: entry={} hit=false took={:?}",
			self.user,
			started.elapsed()
		);
		match read {
			Ok(json) => {
				let parsed = serde_json::from_str(&json)?;
				cache_put(self.service, self.user, Some(json));
				Ok(parsed)
			}
			Err(keyring::Error::NoEntry) => {
				cache_put(self.service, self.user, None);
				Ok(T::default())
			}
			// A failing backend is NOT cached: the next caller must be free to
			// get a real answer once the credential store comes back.
			Err(e) => Err(e.into()),
		}
	}

	pub(crate) fn store(
		&self,
		payload: &T,
	) -> Result<(), CredentialStoreError> {
		let entry = self.entry()?;
		if payload.is_empty() {
			match entry.delete_credential() {
				Ok(()) | Err(keyring::Error::NoEntry) => {
					cache_put(self.service, self.user, None);
					Ok(())
				}
				Err(e) => Err(e.into()),
			}
		} else {
			let json = serde_json::to_string(payload)?;
			entry.set_password(&json)?;
			// Refresh rather than invalidate: this process just wrote the
			// authoritative value, so the next read must see it without paying
			// for another round trip.
			cache_put(self.service, self.user, Some(json));
			Ok(())
		}
	}
}

/// How long a keyring entry read stays reusable.
///
/// The OS credential store is not a cheap local read: on macOS it serializes
/// concurrent access from one process and can block for SECONDS on the first
/// touch after the keychain locks. Startup alone issues several credential
/// reads (the credentials route, the update check, a source diff), and paying
/// that price once per read made a single check-updates spend 22.9s in
/// credential resolution.
///
/// The window is short on purpose. Writes THROUGH this type refresh the entry
/// immediately, so the only staleness left is another process (`aghub-cli`, or
/// `npx skills`) changing a credential behind our back — bounded by this TTL
/// rather than lasting until the app restarts.
const CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(30);

type CacheKey = (&'static str, &'static str);
/// `None` payload = the entry is known-absent (`NoEntry`), which is a real
/// answer worth caching, not a miss.
type CacheEntry = (std::time::Instant, Option<String>);

fn cache() -> &'static std::sync::Mutex<HashMap<CacheKey, CacheEntry>> {
	static CACHE: std::sync::OnceLock<
		std::sync::Mutex<HashMap<CacheKey, CacheEntry>>,
	> = std::sync::OnceLock::new();
	CACHE.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}

fn cache_get(
	service: &'static str,
	user: &'static str,
) -> Option<Option<String>> {
	let map = cache().lock().unwrap_or_else(|e| e.into_inner());
	let (stored_at, payload) = map.get(&(service, user))?;
	if stored_at.elapsed() >= CACHE_TTL {
		return None;
	}
	Some(payload.clone())
}

fn cache_put(
	service: &'static str,
	user: &'static str,
	payload: Option<String>,
) {
	let mut map = cache().lock().unwrap_or_else(|e| e.into_inner());
	map.insert((service, user), (std::time::Instant::now(), payload));
}

/// Drop every cached keyring entry. Tests that install a faulty keyring backend
/// mid-run must call this, or a value cached by an earlier test answers the
/// read the new backend was supposed to fail.
#[cfg(test)]
pub(crate) fn clear_cache_for_test() {
	cache().lock().unwrap_or_else(|e| e.into_inner()).clear();
}

/// The github-credentials keyring entry: `service = "aghub"`,
/// `user = "github_credentials"`.
pub(crate) fn credentials_store(
) -> KeyringJson<Vec<crate::routes::credentials::StoredCredential>> {
	KeyringJson::new("aghub", "github_credentials")
}

/// The source→credential-id bindings keyring entry: `service = "aghub"`,
/// `user = "skill_source_bindings"`.
pub(crate) fn source_bindings_store() -> KeyringJson<resolve::SourceBindings> {
	KeyringJson::new("aghub", "skill_source_bindings")
}

/// Test-only injection for "the credential backend is unreachable", used by
/// `routes::skills`/`routes::skills_update` 503/fail-closed regression tests.
///
/// Rather than a hook the production code checks (the previous
/// `test_hooks::credential_backend_forced_unavailable`), this installs a REAL
/// faulty `keyring` credential builder via
/// `keyring::set_default_credential_builder` — so a test using this guard
/// exercises the actual `From<keyring::Error>` classification end to end,
/// exactly like a real unreachable secret-service backend would.
#[cfg(test)]
pub(crate) mod test_hooks {
	use keyring::credential::{CredentialBuilderApi, CredentialPersistence};
	use keyring::Credential;

	/// A credential builder whose `build()` always fails with
	/// `NoStorageAccess`, simulating "the OS keyring backend is unreachable"
	/// without ever constructing a real credential or touching any real
	/// backend.
	struct FaultyCredentialBuilder;

	impl CredentialBuilderApi for FaultyCredentialBuilder {
		fn build(
			&self,
			_target: Option<&str>,
			_service: &str,
			_user: &str,
		) -> keyring::Result<Box<Credential>> {
			Err(keyring::Error::NoStorageAccess(Box::new(
				std::io::Error::other("forced unavailable (test)"),
			)))
		}

		fn as_any(&self) -> &dyn std::any::Any {
			self
		}

		fn persistence(&self) -> CredentialPersistence {
			CredentialPersistence::EntryOnly
		}
	}

	/// RAII guard: installs [`FaultyCredentialBuilder`] as the process-global
	/// default keyring credential builder for its lifetime, restoring the
	/// platform default builder (the true pre-guard state at this guard's
	/// call sites, which use `with_isolated_state`/`test_env_lock`, not
	/// `IsolatedApiTest`) on drop.
	///
	/// Process-global (`keyring::set_default_credential_builder` has no
	/// thread-local variant and the actual keyring read runs inside
	/// `tokio::task::spawn_blocking`, on a different OS thread than the one
	/// that installs this guard) — the caller MUST hold
	/// `crate::routes::test_env_lock()` for this guard's entire lifetime, the
	/// same requirement `IsolatedApiTest` carries for the identical
	/// process-global builder swap, or it can race a concurrent test that
	/// expects the mock/real backend.
	pub(crate) struct ForceCredentialBackendUnavailable;

	impl ForceCredentialBackendUnavailable {
		pub(crate) fn new() -> Self {
			// Swapping the builder is not enough on its own: a value cached
			// from the PREVIOUS backend would answer the read this guard exists
			// to make fail, and the fail-closed assertion would pass for the
			// wrong reason. Clear on the way in and on the way out, so neither
			// backend can answer for the other.
			super::clear_cache_for_test();
			keyring::set_default_credential_builder(Box::new(
				FaultyCredentialBuilder,
			));
			Self
		}
	}

	impl Drop for ForceCredentialBackendUnavailable {
		fn drop(&mut self) {
			keyring::set_default_credential_builder(
				keyring::default::default_credential_builder(),
			);
			super::clear_cache_for_test();
		}
	}

	/// RAII guard: installs the in-memory mock keyring builder so credential
	/// reads deterministically SUCCEED with an empty store — independent of
	/// whether the host has a reachable secret-service/keychain (CI runners
	/// do not; a developer machine may even hold real aghub credentials).
	/// Same process-global contract as [`ForceCredentialBackendUnavailable`]:
	/// the caller MUST hold `crate::routes::test_env_lock()` for this guard's
	/// entire lifetime. Restores the platform default builder on drop.
	pub(crate) struct MockKeyringBackend;

	impl MockKeyringBackend {
		pub(crate) fn new() -> Self {
			keyring::set_default_credential_builder(
				keyring::mock::default_credential_builder(),
			);
			super::clear_cache_for_test();
			Self
		}
	}

	impl Drop for MockKeyringBackend {
		fn drop(&mut self) {
			keyring::set_default_credential_builder(
				keyring::default::default_credential_builder(),
			);
			super::clear_cache_for_test();
		}
	}
}

#[cfg(test)]
mod cache_tests {
	use super::*;
	use crate::routes::credentials::StoredCredential;

	fn cred(id: &str) -> StoredCredential {
		StoredCredential {
			id: id.to_string(),
			name: format!("name-{id}"),
			token: format!("token-{id}"),
		}
	}

	/// A write must be visible to the very next read.
	///
	/// The read path caches, so this is the failure mode that matters: if a
	/// write did not refresh the cached entry, the token a user just added
	/// stays invisible for the whole TTL and their private-source fetch fails
	/// with "no credential" while the UI shows the credential present.
	///
	/// The leading `load()` is what gives the test teeth — it seeds the cache
	/// with the EMPTY store, so a `store()` that forgets to refresh leaves that
	/// stale empty value behind for the second `load()` to return.
	#[test]
	fn a_write_is_visible_to_the_next_read() {
		let _env = crate::routes::test_env_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		let _mock = test_hooks::MockKeyringBackend::new();

		let store = credentials_store();
		assert!(
			store.load().ok().expect("empty store loads").is_empty(),
			"the mock backend starts empty"
		);

		store.store(&vec![cred("a")]).ok().expect("store writes");

		let after = store.load().ok().expect("store reloads");
		assert_eq!(
			after.len(),
			1,
			"a write must invalidate or refresh the cached entry"
		);
		assert_eq!(after[0].id, "a");
	}

	/// Deleting (storing an empty payload) must be visible too — the delete
	/// branch takes a different path than `set_password`, so it needs its own
	/// cache refresh.
	#[test]
	fn a_delete_is_visible_to_the_next_read() {
		let _env = crate::routes::test_env_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		let _mock = test_hooks::MockKeyringBackend::new();

		let store = credentials_store();
		store.store(&vec![cred("b")]).ok().expect("store writes");
		assert_eq!(store.load().ok().expect("reload").len(), 1);

		store.store(&Vec::new()).ok().expect("delete writes");

		assert!(
			store.load().ok().expect("reload after delete").is_empty(),
			"a delete must not leave the pre-delete value cached"
		);
	}
}
