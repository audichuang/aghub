//! Credential helpers for skill source fetching.
//!
//! Resolution lives here (in `crates/api`) so that `crates/core` stays pure:
//! core receives an already-resolved `Option<token>` and never touches the
//! keyring or the network.

// The resolver and binding store are consumed by the update-check
// orchestration via the `routes::skills_update` route.
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

/// Classifies a credential/binding keyring failure: "the OS backend is
/// unreachable" (retryable, assume no mutation) vs everything else (corrupt
/// JSON, bad encoding, ...). Classified by
/// `aghub_inference::keyring_backend_unavailable`, the same predicate
/// `InferenceProviderError` uses; both map to `KEYCHAIN_UNAVAILABLE`.
/// `Clone` so a remembered failure keeps its variant — `Unavailable` fails
/// closed while `Other` may degrade to anonymous.
#[derive(Clone)]
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

impl From<keyring_core::Error> for CredentialStoreError {
	fn from(error: keyring_core::Error) -> Self {
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

/// The "empty" (delete-worthy) state of a keyring-backed JSON payload — see
/// [`KeyringJson`].
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

/// Round-trips a [`KeyringPayload`] `T` through one `(service, user)` keyring
/// entry as JSON, deleting the entry when `T::is_empty()`. Backs the bundle and
/// the two legacy entries it migrates from.
///
/// Deliberately does NOT lock: `update_bundle`/`read_bundle` serialize the
/// flows that span a load+store pair (and the three-entry migration).
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

	fn entry(&self) -> Result<keyring_core::Entry, CredentialStoreError> {
		Ok(aghub_inference::keyring_entry(self.service, self.user)?)
	}

	/// Like [`load`](Self::load) but keeps "the entry does not exist" distinct
	/// from "the entry holds an empty value".
	///
	/// Needed by the legacy migration: an absent bundle means "look for the old
	/// entries", an empty one means "migration ran, no credentials".
	pub(crate) fn load_optional(
		&self,
	) -> Result<Option<T>, CredentialStoreError> {
		match self.read_state()? {
			ReadState::Value(json) => Ok(Some(serde_json::from_str(&json)?)),
			ReadState::Absent => Ok(None),
		}
	}

	pub(crate) fn load(&self) -> Result<T, CredentialStoreError> {
		Ok(self.load_optional()?.unwrap_or_default())
	}

	/// The single keyring read, cache in front of it.
	///
	/// One primitive for both accessors above, so caching and logging exist
	/// once.
	fn read_state(&self) -> Result<ReadState, CredentialStoreError> {
		if let Some(cached) = cache_get(self.service, self.user) {
			return match cached {
				// Replay a remembered failure VERBATIM (see FAILURE_TTL):
				// `Unavailable` vs `Other` drive fail-closed vs degrade.
				CachedRead::Failed(error) => Err(error),
				CachedRead::Value(json) => Ok(ReadState::Value(json)),
				CachedRead::Absent => Ok(ReadState::Absent),
			};
		}
		// Record EVERY outcome, failures included (unreachable backend, a
		// dismissed dialog) — an uncached failure re-prompts each caller.
		match self.read_backend() {
			Ok(state) => {
				cache_put(self.service, self.user, CachedRead::from(&state));
				Ok(state)
			}
			Err(error) => {
				cache_put(
					self.service,
					self.user,
					CachedRead::Failed(error.clone()),
				);
				Err(error)
			}
		}
	}

	/// The uncached read. Separated so `read_state` has exactly one place to
	/// record the outcome.
	fn read_backend(&self) -> Result<ReadState, CredentialStoreError> {
		let entry = self.entry()?;
		let started = std::time::Instant::now();
		let read = entry.get_password();
		log::info!(
			"keyring read: entry={} hit=false took={:?}",
			self.user,
			started.elapsed()
		);
		match read {
			Ok(json) => Ok(ReadState::Value(json)),
			Err(keyring_core::Error::NoEntry) => Ok(ReadState::Absent),
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
				Ok(()) | Err(keyring_core::Error::NoEntry) => {
					cache_put(self.service, self.user, CachedRead::Absent);
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
			cache_put(self.service, self.user, CachedRead::Value(json));
			Ok(())
		}
	}
}

/// How long a keyring entry read stays reusable.
///
/// A keyring read is not cheap (macOS serializes access and can block for
/// seconds after the keychain locks), and startup issues several.
/// Short on purpose: writes through this type refresh the entry at once, so
/// the TTL only bounds staleness from ANOTHER process changing a credential.
/// See docs/history/api.md#keyring-read-cache
const CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(30);

/// How long a FAILED read is remembered.
///
/// Exists so one dismissed macOS keychain dialog (`User canceled the
/// operation`) is not re-opened by each of startup's independent reads. Short
/// so a transient failure, or the user authorizing after all, recovers in
/// seconds. See docs/history/api.md#keyring-read-cache
const FAILURE_TTL: std::time::Duration = std::time::Duration::from_secs(5);

type CacheKey = (&'static str, &'static str);

/// A SUCCESSFUL read's outcome. No failure variant: a failed read is an `Err`.
enum ReadState {
	/// The entry holds this JSON payload.
	Value(String),
	/// The entry does not exist (`NoEntry`) — a real answer, not a miss.
	Absent,
}

/// What a previous read of this entry concluded — including that it failed,
/// which [`ReadState`] cannot express.
#[derive(Clone)]
enum CachedRead {
	Value(String),
	Absent,
	/// Carries the WHOLE error, not just its text, so a replay reproduces the
	/// original classification rather than a guess at it.
	Failed(CredentialStoreError),
}

impl From<&ReadState> for CachedRead {
	fn from(state: &ReadState) -> Self {
		match state {
			ReadState::Value(json) => CachedRead::Value(json.clone()),
			ReadState::Absent => CachedRead::Absent,
		}
	}
}

type CacheEntry = (std::time::Instant, CachedRead);

fn cache() -> &'static std::sync::Mutex<HashMap<CacheKey, CacheEntry>> {
	static CACHE: std::sync::OnceLock<
		std::sync::Mutex<HashMap<CacheKey, CacheEntry>>,
	> = std::sync::OnceLock::new();
	CACHE.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}

fn cache_get(service: &'static str, user: &'static str) -> Option<CachedRead> {
	let map = cache().lock().unwrap_or_else(|e| e.into_inner());
	let (stored_at, entry) = map.get(&(service, user))?;
	let ttl = match entry {
		CachedRead::Failed(_) => FAILURE_TTL,
		_ => CACHE_TTL,
	};
	if stored_at.elapsed() >= ttl {
		return None;
	}
	Some(entry.clone())
}

fn cache_put(service: &'static str, user: &'static str, entry: CachedRead) {
	let mut map = cache().lock().unwrap_or_else(|e| e.into_inner());
	map.insert((service, user), (std::time::Instant::now(), entry));
}

/// Drop every cached keyring entry. Tests that install a faulty keyring backend
/// mid-run must call this, or a value cached by an earlier test answers the
/// read the new backend was supposed to fail.
#[cfg(test)]
pub(crate) fn clear_cache_for_test() {
	cache().lock().unwrap_or_else(|e| e.into_inner()).clear();
}

/// Everything credential-related in ONE keyring entry.
///
/// macOS authorizes per keychain ITEM, and the ad-hoc-signed app's ACL breaks
/// on every release, so each item re-prompts after an upgrade. One item = one
/// prompt (and half the round trips elsewhere).
#[derive(Default, Serialize, serde::Deserialize)]
pub(crate) struct CredentialBundle {
	#[serde(default)]
	pub(crate) credentials: Vec<crate::routes::credentials::StoredCredential>,
	#[serde(default)]
	pub(crate) bindings: resolve::SourceBindings,
}

impl KeyringPayload for CredentialBundle {
	/// NEVER "empty" — the bundle entry must survive becoming empty.
	///
	/// `KeyringJson::store` deletes an empty payload's entry, but a missing
	/// bundle re-triggers the legacy migration and could resurrect a stale
	/// legacy copy of the user's last deleted credential. An empty bundle is a
	/// tombstone: "migration ran, the user has nothing".
	fn is_empty(&self) -> bool {
		false
	}
}

/// The single combined entry: `service = "aghub"`, `user = "credentials"`.
pub(crate) fn bundle_store() -> KeyringJson<CredentialBundle> {
	KeyringJson::new("aghub", "credentials")
}

/// Serializes every bundle read-modify-write, INCLUDING the first-read
/// migration.
///
/// The migration writes from an unlocked read path (`SourceAuth::load`); if
/// this did not cover it, a check's migration could overwrite a credential a
/// concurrent route just created with the older legacy snapshot.
/// Cross-process races remain a known limitation.
static BUNDLE_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn lock_bundle() -> std::sync::MutexGuard<'static, ()> {
	BUNDLE_MUTEX
		.lock()
		.unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Read the bundle, migrating from the legacy entries on first use.
pub(crate) fn read_bundle() -> Result<CredentialBundle, CredentialStoreError> {
	let _guard = lock_bundle();
	read_bundle_locked()
}

/// Read-modify-write under one guard.
///
/// This is the ONLY way to mutate stored credentials or bindings. Callers that
/// change both (credential delete prunes its bindings) do it in a single
/// closure, so no reader can observe a binding pointing at a credential that is
/// already gone.
pub(crate) fn update_bundle<T, E>(
	mutate: impl FnOnce(&mut CredentialBundle) -> Result<T, E>,
) -> Result<Result<T, E>, CredentialStoreError> {
	let _guard = lock_bundle();
	let mut bundle = read_bundle_locked()?;
	match mutate(&mut bundle) {
		Ok(value) => {
			store_bundle(&bundle)?;
			Ok(Ok(value))
		}
		// The mutation rejected the request (duplicate name, unknown
		// credential, ...) — nothing is persisted.
		Err(rejected) => Ok(Err(rejected)),
	}
}

/// Migration is idempotent and convergent: it runs only while the bundle is
/// ABSENT. A failed (best-effort) legacy clear is harmless — once the bundle
/// exists nothing consults the legacy entries.
fn read_bundle_locked() -> Result<CredentialBundle, CredentialStoreError> {
	if let Some(bundle) = bundle_store().load_optional()? {
		return Ok(bundle);
	}
	let legacy = CredentialBundle {
		credentials: legacy_credentials_store().load()?,
		bindings: legacy_bindings_store().load()?,
	};
	// Checked field-wise on purpose: `CredentialBundle::is_empty()` answers the
	// store's delete-when-empty question and is hardwired to false (see its
	// doc), so it cannot be used to ask "did the legacy entries hold anything".
	if legacy.credentials.is_empty() && legacy.bindings.0.is_empty() {
		// Nothing to carry over. Do NOT write a tombstone for this case: a user
		// who never had a credential should not get a keychain item created for
		// them, and the absent-bundle path costs exactly the same on every read.
		return Ok(legacy);
	}
	bundle_store().store(&legacy)?;
	// Best effort, never retried (a retry can re-prompt for authorization); a
	// stale legacy copy cannot make a later read wrong. Do not log the error
	// contents: they can carry entry identifiers.
	if legacy_credentials_store().store(&Vec::new()).is_err()
		|| legacy_bindings_store()
			.store(&resolve::SourceBindings::default())
			.is_err()
	{
		log::warn!(
			"migrated credentials into the combined keychain entry, but could \
			 not clear the legacy entries; they are now ignored but still \
			 present"
		);
	}
	Ok(legacy)
}

/// Persist the bundle.
pub(crate) fn store_bundle(
	bundle: &CredentialBundle,
) -> Result<(), CredentialStoreError> {
	bundle_store().store(bundle)
}

/// Pre-merge github-credentials entry. Read ONLY by the migration above.
fn legacy_credentials_store(
) -> KeyringJson<Vec<crate::routes::credentials::StoredCredential>> {
	KeyringJson::new("aghub", "github_credentials")
}

/// Pre-merge source-bindings entry. Read ONLY by the migration above.
fn legacy_bindings_store() -> KeyringJson<resolve::SourceBindings> {
	KeyringJson::new("aghub", "skill_source_bindings")
}

/// Test keyring backends. Both guards swap keyring-core's process-global
/// default store, so callers must hold `crate::routes::test_env_lock()`.
#[cfg(test)]
pub(crate) mod test_hooks {
	use std::collections::HashMap;
	use std::sync::Arc;

	use keyring_core::api::CredentialStoreApi;

	/// How often the faulty backend was reached (a cached verdict must not).
	pub(crate) static FAULTY_BUILDS: std::sync::atomic::AtomicUsize =
		std::sync::atomic::AtomicUsize::new(0);

	/// Every entry fails with `NoStorageAccess`, like an unreachable backend.
	#[derive(Debug)]
	struct FaultyCredentialStore;

	impl CredentialStoreApi for FaultyCredentialStore {
		fn vendor(&self) -> String {
			"aghub test: always unavailable".to_string()
		}

		fn id(&self) -> String {
			"faulty".to_string()
		}

		fn build(
			&self,
			_service: &str,
			_user: &str,
			_modifiers: Option<&HashMap<&str, &str>>,
		) -> keyring_core::Result<keyring_core::Entry> {
			FAULTY_BUILDS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
			Err(keyring_core::Error::NoStorageAccess(Box::new(
				std::io::Error::other("forced unavailable (test)"),
			)))
		}

		fn as_any(&self) -> &dyn std::any::Any {
			self
		}
	}

	pub(crate) struct ForceCredentialBackendUnavailable;

	impl ForceCredentialBackendUnavailable {
		pub(crate) fn new() -> Self {
			// Otherwise a value cached from the previous backend answers.
			super::clear_cache_for_test();
			keyring_core::set_default_store(Arc::new(FaultyCredentialStore));
			Self
		}
	}

	impl Drop for ForceCredentialBackendUnavailable {
		fn drop(&mut self) {
			keyring_core::unset_default_store();
			super::clear_cache_for_test();
		}
	}

	/// A fresh, empty in-memory store.
	pub(crate) struct MockKeyringBackend;

	impl MockKeyringBackend {
		pub(crate) fn new() -> Self {
			keyring_core::set_default_store(
				keyring_core::mock::Store::new().expect("mock keyring store"),
			);
			super::clear_cache_for_test();
			Self
		}
	}

	impl Drop for MockKeyringBackend {
		fn drop(&mut self) {
			keyring_core::unset_default_store();
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

		let store = bundle_store();
		assert!(
			store
				.load()
				.ok()
				.expect("empty store loads")
				.credentials
				.is_empty(),
			"the mock backend starts empty"
		);

		store
			.store(&CredentialBundle {
				credentials: vec![cred("a")],
				..Default::default()
			})
			.ok()
			.expect("store writes");

		let after = store.load().ok().expect("store reloads");
		assert_eq!(
			after.credentials.len(),
			1,
			"a write must invalidate or refresh the cached entry"
		);
		assert_eq!(after.credentials[0].id, "a");
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

		let store = bundle_store();
		store
			.store(&CredentialBundle {
				credentials: vec![cred("b")],
				..Default::default()
			})
			.ok()
			.expect("store writes");
		assert_eq!(store.load().ok().expect("reload").credentials.len(), 1);

		store
			.store(&CredentialBundle::default())
			.ok()
			.expect("delete writes");

		assert!(
			store
				.load()
				.ok()
				.expect("reload after delete")
				.credentials
				.is_empty(),
			"a delete must not leave the pre-delete value cached"
		);
	}
}

#[cfg(test)]
mod migration_tests {
	use super::*;
	use crate::routes::credentials::StoredCredential;

	fn cred(id: &str) -> StoredCredential {
		StoredCredential {
			id: id.to_string(),
			name: format!("name-{id}"),
			token: format!("token-{id}"),
		}
	}

	/// Users upgrading from a pre-merge build have their data in the two legacy
	/// entries. Losing it means their stored tokens silently vanish and every
	/// private-source fetch starts failing, so the carry-over is the one part of
	/// this change that MUST be pinned.
	#[test]
	fn legacy_entries_are_carried_into_the_bundle() {
		let _env = crate::routes::test_env_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		let _mock = test_hooks::MockKeyringBackend::new();

		legacy_credentials_store()
			.store(&vec![cred("legacy")])
			.ok()
			.expect("legacy credentials written");
		let mut bindings = std::collections::BTreeMap::new();
		bindings.insert("owner/repo".to_string(), "legacy".to_string());
		legacy_bindings_store()
			.store(&resolve::SourceBindings(bindings))
			.ok()
			.expect("legacy bindings written");

		let bundle = read_bundle().ok().expect("migration runs");

		assert_eq!(
			bundle.credentials.len(),
			1,
			"the legacy credential must survive the merge"
		);
		assert_eq!(bundle.credentials[0].id, "legacy");
		assert_eq!(
			bundle.bindings.0.get("owner/repo").map(String::as_str),
			Some("legacy"),
			"the legacy binding must survive the merge"
		);

		// And it must now come from the bundle, not be re-migrated: clearing the
		// legacy entries must not change what a later read returns.
		let _ = legacy_credentials_store().store(&Vec::new());
		let _ =
			legacy_bindings_store().store(&resolve::SourceBindings::default());
		let again = read_bundle().ok().expect("bundle reload");
		assert_eq!(
			again.credentials.len(),
			1,
			"once migrated, reads must come from the bundle"
		);
	}

	/// An existing bundle is authoritative. A leftover legacy entry (the delete
	/// is best-effort and can fail) must never resurrect an old credential over
	/// the current one.
	#[test]
	fn a_leftover_legacy_entry_never_overrides_the_bundle() {
		let _env = crate::routes::test_env_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		let _mock = test_hooks::MockKeyringBackend::new();

		store_bundle(&CredentialBundle {
			credentials: vec![cred("current")],
			..Default::default()
		})
		.ok()
		.expect("bundle written");
		legacy_credentials_store()
			.store(&vec![cred("stale")])
			.ok()
			.expect("stale legacy entry written");

		let bundle = read_bundle().ok().expect("bundle read");

		assert_eq!(bundle.credentials.len(), 1);
		assert_eq!(
			bundle.credentials[0].id, "current",
			"the bundle wins over a leftover legacy entry"
		);
	}
}

#[cfg(test)]
mod tombstone_tests {
	use super::*;
	use crate::routes::credentials::StoredCredential;

	/// Emptying the bundle must NOT delete it.
	///
	/// The legacy cleanup is best-effort, so a stale legacy entry may still be
	/// sitting in the keychain. If deleting the last credential also deleted the
	/// bundle, the next read would see "no bundle", re-run the migration, and
	/// resurrect the credential the user just deleted — with its token.
	#[test]
	fn emptying_the_bundle_does_not_resurrect_legacy_credentials() {
		let _env = crate::routes::test_env_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		let _mock = test_hooks::MockKeyringBackend::new();

		// A legacy entry that cleanup failed to remove.
		legacy_credentials_store()
			.store(&vec![StoredCredential {
				id: "stale".into(),
				name: "stale".into(),
				token: "stale-token".into(),
			}])
			.ok()
			.expect("stale legacy entry written");
		store_bundle(&CredentialBundle {
			credentials: vec![StoredCredential {
				id: "current".into(),
				name: "current".into(),
				token: "current-token".into(),
			}],
			..Default::default()
		})
		.ok()
		.expect("bundle written");

		// The user deletes their last credential.
		update_bundle(|bundle| {
			bundle.credentials.clear();
			Ok::<(), std::convert::Infallible>(())
		})
		.ok()
		.expect("empty write succeeds")
		.expect("closure cannot reject");

		let after = read_bundle().ok().expect("bundle reload");
		assert!(
			after.credentials.is_empty(),
			"the deleted credential must stay deleted, not be re-migrated from \
			 the leftover legacy entry"
		);
	}
}

#[cfg(test)]
mod failure_cache_tests {
	use super::*;

	/// A refused read must not send the next caller back to the backend.
	///
	/// On macOS a locked keychain shows an authorization dialog; dismissing it
	/// surfaces as a backend failure. App startup issues several independent
	/// credential reads, so a failure that is not remembered re-opens that
	/// dialog once per read — a real session showed one cancel turning into
	/// three prompts and 10.3s of waiting.
	#[test]
	fn a_refused_read_is_not_retried_by_the_next_caller() {
		let _env = crate::routes::test_env_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());

		{
			let _faulty = test_hooks::ForceCredentialBackendUnavailable::new();
			test_hooks::FAULTY_BUILDS
				.store(0, std::sync::atomic::Ordering::SeqCst);

			assert!(
				bundle_store().load_optional().is_err(),
				"the faulty backend must fail the first read"
			);
			assert!(
				bundle_store().load_optional().is_err(),
				"the remembered failure is replayed as the same error"
			);

			// The assertion that has teeth: BOTH reads returned Err either way,
			// so only the reach count distinguishes a replayed verdict from a
			// second trip to the backend — a second trip is a second dialog.
			assert_eq!(
				test_hooks::FAULTY_BUILDS
					.load(std::sync::atomic::Ordering::SeqCst),
				1,
				"the second read must be answered from the remembered failure, \
				 not by asking the backend again"
			);
		}

		// Guard dropped: the real backend is back AND the guard cleared the
		// cache, so nothing stale outlives the test.
		assert!(
			cache_get("aghub", "credentials").is_none(),
			"the guard must leave no cached verdict behind"
		);
	}

	/// A replayed failure must carry its ORIGINAL classification.
	///
	/// `Unavailable` and `Other` are not interchangeable: `SourceAuth::load_soft`
	/// turns only `Unavailable` into `BackendUnavailable` (fail closed) and lets
	/// `Other` degrade to an anonymous fetch, and the two map to 503 vs 500 at
	/// the HTTP layer. An earlier version of this cache rebuilt every replayed
	/// error as `Unavailable`, which silently converted a fail-OPEN path into a
	/// fail-CLOSED one for the whole window.
	///
	/// The faulty-backend guard can only produce `Unavailable`, so the cache is
	/// seeded directly — that is the only way to observe the `Other` direction.
	#[test]
	fn a_replayed_failure_keeps_its_original_classification() {
		let _env = crate::routes::test_env_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		clear_cache_for_test();

		cache_put(
			"aghub",
			"credentials",
			CachedRead::Failed(CredentialStoreError::Other(
				"corrupt payload".to_string(),
			)),
		);

		match bundle_store().load_optional() {
			Err(CredentialStoreError::Other(message)) => {
				assert_eq!(message, "corrupt payload");
			}
			Err(CredentialStoreError::Unavailable(_)) => panic!(
				"a cached `Other` was replayed as `Unavailable` — that turns a \
				 degrade-to-anonymous path into a hard failure"
			),
			Ok(_) => panic!("the cached failure must be replayed, not ignored"),
		}

		clear_cache_for_test();
	}

	/// The remembered failure must not outlive its short window, or a user who
	/// dismisses the dialog once would be locked out until the app restarts.
	#[test]
	fn the_failure_window_is_far_shorter_than_the_success_window() {
		assert!(
			FAILURE_TTL < CACHE_TTL,
			"a refusal must expire sooner than a successful read"
		);
		assert!(
			FAILURE_TTL <= std::time::Duration::from_secs(10),
			"the user must be able to change their mind within seconds"
		);
	}
}
