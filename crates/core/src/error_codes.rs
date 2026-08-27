//! Stable wire codes for [`ConfigError`], shared by every surface.
//!
//! The HTTP API projected `ConfigError` onto a `(Status, code)` pair inside its
//! own `From<ConfigError> for ApiError`, and that was the only place the code
//! vocabulary existed. The CLI had none: under `--json` a failure printed
//! NOTHING on stdout and one line of English prose on stderr, and policy
//! refusals, missing resources, invalid agent ids and genuine write failures
//! were all exit 1. A caller could only tell them apart by pattern-matching the
//! prose — which is not stable, and already differs in wording for the same
//! condition (`Skill 'x' not found` from `describe` vs `Resource not found:
//! skill 'x'` from `disable`).
//!
//! So the code + retryability half lives here, and both surfaces read it.
//! HTTP status stays in the API, where it belongs — it is the only part that is
//! genuinely transport-specific.

use crate::errors::ConfigError;

/// The stable machine code for this error.
///
/// Same strings the API has always sent, so a client that already branches on
/// `ApiError::code` needs no change and the CLI's `--json` errors speak the
/// same vocabulary.
pub fn wire_code(error: &ConfigError) -> &'static str {
	match error {
		ConfigError::ResourceNotFound { .. } => "RESOURCE_NOT_FOUND",
		ConfigError::ResourceExists { .. } => "RESOURCE_EXISTS",
		ConfigError::NotFound { .. } => "CONFIG_NOT_FOUND",
		ConfigError::UnsupportedOperation(_) => "UNSUPPORTED_OPERATION",
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
///
/// This exists because "retry" is the single most consequential thing an
/// automating caller decides from an error, and it was previously undecidable:
/// every CLI failure was exit 1 with prose.
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
