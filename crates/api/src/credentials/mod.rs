//! Credential helpers for skill source fetching.
//!
//! Resolution lives here (in `crates/api`) so that `crates/core` stays pure:
//! core receives an already-resolved `Option<token>` and never touches the
//! keyring or the network.

// The resolver and binding store are consumed by the update-check orchestration
// (Task F1.5) via the `routes::skills_update` route.
pub(crate) mod resolve;

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

/// Classifies a github-credential/source-binding keyring failure so callers
/// can distinguish "the OS backend itself isn't reachable" (retryable, no
/// mutation should be assumed to have happened) from every other failure
/// (corrupt JSON, bad encoding, ...). The `From<InferenceProviderError>`
/// mapping in `crate::error` makes the equivalent distinction for
/// `aghub-inference` — both funnel into the same `KEYCHAIN_UNAVAILABLE`
/// status/code/message in `crate::error::ApiError`, so the two credential
/// domains never diverge on this.
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
		// Same classification as `aghub_inference::InferenceProviderError`
		// (see its `From<keyring::Error>` impl) — keep both in sync.
		match error {
			keyring::Error::PlatformFailure(_)
			| keyring::Error::NoStorageAccess(_) => {
				CredentialStoreError::Unavailable(error.to_string())
			}
			other => CredentialStoreError::Other(other.to_string()),
		}
	}
}

impl From<serde_json::Error> for CredentialStoreError {
	fn from(error: serde_json::Error) -> Self {
		CredentialStoreError::Other(error.to_string())
	}
}

/// Test-only injection hook for "the credential backend is unreachable",
/// used by `routes::credentials::load_credentials` and
/// `resolve::load_source_bindings` (GitHub #15 round-2 Codex finding).
///
/// The 503-path tests that predate this hook tampered with
/// `DBUS_SESSION_BUS_ADDRESS` to force a REAL secret-service failure. That
/// only affects Linux (the only OS that uses D-Bus/secret-service here) —
/// CI also runs macOS/Windows, where the same env var does nothing, so those
/// tests would get a non-503 result and fail on those runners. This hook
/// lets a test force `CredentialStoreError::Unavailable` deterministically
/// on ANY platform, without touching any real keyring or platform-specific
/// env var.
///
/// Process-global (`static`, not thread-local): `KeyringResolver::load`/
/// `load_or_unavailable` run the actual read inside
/// `tokio::task::spawn_blocking`, which executes on a DIFFERENT OS thread
/// than the one that sets the guard — a thread-local override would not be
/// visible there. Being process-global means a test using this guard MUST
/// hold `crate::routes::test_env_lock()` for the guard's entire lifetime
/// (same requirement the pre-existing `DBUS_SESSION_BUS_ADDRESS`-tampering
/// tests already had for that env var, and the same requirement
/// `keyring::set_default_credential_builder` carries in `lib.rs`'s
/// `IsolatedApiTest` — all three are process-global test state serialized by
/// that one lock), or it can race a concurrent test that expects the real
/// backend.
#[cfg(test)]
pub(crate) mod test_hooks {
	use std::sync::atomic::{AtomicBool, Ordering};

	static FORCE_UNAVAILABLE: AtomicBool = AtomicBool::new(false);

	pub(crate) fn credential_backend_forced_unavailable() -> bool {
		FORCE_UNAVAILABLE.load(Ordering::SeqCst)
	}

	/// RAII guard: forces `load_credentials`/`load_source_bindings` to
	/// report `CredentialStoreError::Unavailable` for its lifetime. Caller
	/// must hold `crate::routes::test_env_lock()` for as long as this guard
	/// is alive (see module doc).
	pub(crate) struct ForceCredentialBackendUnavailable;

	impl ForceCredentialBackendUnavailable {
		pub(crate) fn new() -> Self {
			FORCE_UNAVAILABLE.store(true, Ordering::SeqCst);
			Self
		}
	}

	impl Drop for ForceCredentialBackendUnavailable {
		fn drop(&mut self) {
			FORCE_UNAVAILABLE.store(false, Ordering::SeqCst);
		}
	}
}
