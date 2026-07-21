//! Credential helpers for skill source fetching.
//!
//! Resolution lives here (in `crates/api`) so that `crates/core` stays pure:
//! core receives an already-resolved `Option<token>` and never touches the
//! keyring or the network.

// The resolver and binding store are consumed by the update-check orchestration
// (Task F1.5) via the `routes::skills_update` route.
pub(crate) mod resolve;
pub(crate) mod source_auth;

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
		let entry = self.entry()?;
		match entry.get_password() {
			Ok(json) => Ok(serde_json::from_str(&json)?),
			Err(keyring::Error::NoEntry) => Ok(T::default()),
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
				Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
				Err(e) => Err(e.into()),
			}
		} else {
			entry.set_password(&serde_json::to_string(payload)?)?;
			Ok(())
		}
	}
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
		}
	}
}
