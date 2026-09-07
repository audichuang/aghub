//! Error types for inference provider storage.

/// Error type for inference provider operations.
#[derive(Debug, thiserror::Error)]
pub enum InferenceProviderError {
	/// I/O error.
	#[error("IO error: {0}")]
	Io(#[from] std::io::Error),

	/// SQLite database error.
	#[error("database error: {0}")]
	Database(String),

	/// Platform keyring error.
	#[error("keyring error: {0}")]
	Keyring(String),

	/// The credential backend itself could not be reached (Linux
	/// secret-service with no D-Bus session, a locked/inaccessible platform
	/// keychain, ...). Distinct from `Keyring` so callers can return a
	/// stable, retryable error instead of a generic one — the request didn't
	/// fail because of bad data, the backend just isn't reachable right now.
	#[error("credential backend unavailable: {0}")]
	KeyringUnavailable(String),

	/// Provider latin names must not be empty.
	#[error("provider latin name cannot be empty")]
	EmptyName,

	/// Provider latin names must be lowercase a-z.
	#[error("provider latin name must contain only lowercase a-z: {0}")]
	InvalidLatinName(String),

	/// Model names must not be empty.
	#[error("model name cannot be empty")]
	EmptyModelName,

	/// API keys must not be empty.
	#[error("provider API key cannot be empty")]
	EmptyApiKey,

	/// API base URLs must not be empty.
	#[error("provider API base URL cannot be empty")]
	EmptyApiBaseUrl,

	/// Provider latin name is already in use.
	#[error("provider already exists: {0}")]
	AlreadyExists(String),

	/// Model name is already in use for the provider.
	#[error("inference model already exists: {0}")]
	ModelAlreadyExists(String),

	/// Provider format is not supported.
	#[error("unsupported inference provider format: {0}")]
	InvalidFormat(String),

	/// Provider ID was not found.
	#[error("provider not found: {0}")]
	NotFound(String),

	/// Agent-local provider IDs must not be empty.
	#[error("agent provider id cannot be empty")]
	EmptyAgentProviderId,

	/// Agent does not support the requested provider-management capability.
	#[error("{agent_id} does not support {capability}")]
	UnsupportedAgentProviderCapability {
		/// Stable agent id.
		agent_id: String,
		/// Unsupported capability label.
		capability: String,
	},

	/// Agent provider config could not be parsed.
	#[error("invalid {agent_id} provider config at {path}: {message}")]
	InvalidAgentProviderConfig {
		/// Stable agent id.
		agent_id: String,
		/// Config path.
		path: String,
		/// Parse or validation message.
		message: String,
	},

	/// Agent credential store could not be parsed.
	#[error("invalid {agent_id} credential store at {path}: {message}")]
	InvalidAgentCredentialStore {
		/// Stable agent id.
		agent_id: String,
		/// Credential store path.
		path: String,
		/// Parse or validation message.
		message: String,
	},
}

/// `true` iff `err` means the credential backend itself is unreachable
/// (e.g. no D-Bus session on Linux secret-service, a locked/inaccessible
/// platform keychain) rather than a normal, data-shaped outcome (no entry,
/// bad encoding, ...). Shared by every keyring-touching crate (this crate's
/// `From<keyring::Error>` below, and `aghub-api`'s
/// `credentials::CredentialStoreError`) so they classify the same platform
/// error identically.
pub fn keyring_backend_unavailable(err: &keyring::Error) -> bool {
	matches!(
		err,
		keyring::Error::PlatformFailure(_) | keyring::Error::NoStorageAccess(_)
	)
}

impl From<keyring::Error> for InferenceProviderError {
	fn from(error: keyring::Error) -> Self {
		if keyring_backend_unavailable(&error) {
			Self::KeyringUnavailable(error.to_string())
		} else {
			Self::Keyring(error.to_string())
		}
	}
}

impl From<sqlx::Error> for InferenceProviderError {
	fn from(error: sqlx::Error) -> Self {
		Self::Database(error.to_string())
	}
}

impl From<sqlx::migrate::MigrateError> for InferenceProviderError {
	fn from(error: sqlx::migrate::MigrateError) -> Self {
		Self::Database(error.to_string())
	}
}

/// Result type alias for inference provider operations.
pub type Result<T> = std::result::Result<T, InferenceProviderError>;

#[cfg(test)]
mod tests {
	use super::keyring_backend_unavailable;

	#[test]
	fn platform_failure_and_no_storage_access_are_unavailable() {
		let io_err = || Box::new(std::io::Error::other("boom"));
		assert!(keyring_backend_unavailable(
			&keyring::Error::PlatformFailure(io_err())
		));
		assert!(keyring_backend_unavailable(
			&keyring::Error::NoStorageAccess(io_err())
		));
	}

	#[test]
	fn other_variants_are_not_unavailable() {
		assert!(!keyring_backend_unavailable(&keyring::Error::NoEntry));
		assert!(!keyring_backend_unavailable(&keyring::Error::BadEncoding(
			vec![0xff]
		)));
		assert!(!keyring_backend_unavailable(&keyring::Error::TooLong(
			"attr".to_string(),
			10
		)));
	}
}
