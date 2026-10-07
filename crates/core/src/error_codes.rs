//! Stable wire codes for [`ConfigError`], shared by every surface.
//!
//! The code + retryability half lives here and both CLI and API read it; HTTP
//! status stays in the API (the only transport-specific part). Callers branch
//! on these codes, never on error prose. Why: knowledge page
//! `CLI 與 API 的共用錯誤契約`.

use crate::errors::ConfigError;

/// The stable machine code for this error.
///
/// Same strings the API has always sent — never rename one.
pub fn wire_code(error: &ConfigError) -> &'static str {
	match error {
		ConfigError::ResourceNotFound { .. } => "RESOURCE_NOT_FOUND",
		ConfigError::ResourceExists { .. } => "RESOURCE_EXISTS",
		ConfigError::NotFound { .. } => "CONFIG_NOT_FOUND",
		ConfigError::UnsupportedOperation { .. } => "UNSUPPORTED_OPERATION",
		ConfigError::ValidationFailed(_) => "VALIDATION_FAILED",
		ConfigError::InvalidConfig(_) => "INVALID_CONFIG",
		ConfigError::Json(_) => "JSON_PARSE_ERROR",
		// Mutation-lock contention arrives as `Io(WouldBlock)` — `skill::lock::
		// guard` is its only producer. It is a RETRYABLE conflict, not a fault:
		// another aghub process simply held the lock and nothing was written.
		ConfigError::Io(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
			crate::skills::lock::MUTATION_LOCK_BUSY_CODE
		}
		ConfigError::Io(_) => "IO_ERROR",
	}
}

/// Is the SAME request worth retrying unchanged?
///
/// Only lock contention is: the operation wrote nothing and will succeed once
/// the other process finishes. Everything else needs the caller to change
/// something first — including
/// [`SOURCE_CHANGED_DURING_FETCH`](crate::skills::lock::SOURCE_CHANGED_DURING_FETCH_CODE),
/// which is retryable only AFTER a re-read, so it is not `true` here.
pub fn retryable(error: &ConfigError) -> bool {
	matches!(
		error,
		ConfigError::Io(e) if e.kind() == std::io::ErrorKind::WouldBlock
	)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn lock_contention_is_the_only_retryable_code() {
		let busy = ConfigError::Io(std::io::Error::new(
			std::io::ErrorKind::WouldBlock,
			"held",
		));
		assert_eq!(
			wire_code(&busy),
			crate::skills::lock::MUTATION_LOCK_BUSY_CODE
		);
		assert!(retryable(&busy));

		// A real IO fault is NOT retryable and must not borrow the busy code.
		let broken = ConfigError::Io(std::io::Error::new(
			std::io::ErrorKind::PermissionDenied,
			"nope",
		));
		assert_eq!(wire_code(&broken), "IO_ERROR");
		assert!(!retryable(&broken));

		// A missing resource is a stable, non-retryable code regardless of the
		// wording the surface happens to use for it.
		let missing = ConfigError::resource_not_found("skill", "ghost");
		assert_eq!(wire_code(&missing), "RESOURCE_NOT_FOUND");
		assert!(!retryable(&missing));
	}
}
