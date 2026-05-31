//! Error types for git operations.

use std::path::PathBuf;

/// Error type for git clone operations.
#[derive(Debug, thiserror::Error)]
pub enum GitError {
	/// I/O error.
	#[error("IO error: {0}")]
	Io(#[from] std::io::Error),

	/// Git clone error from gix.
	#[error("Git clone failed: {0}")]
	CloneFailed(String),

	/// Invalid URL format.
	#[error("Invalid URL: {0}")]
	InvalidUrl(String),

	/// URL parse error.
	#[error("URL parse error: {0}")]
	UrlParse(#[from] url::ParseError),

	/// Failed to create temp directory.
	#[error("Failed to create temp directory: {0}")]
	TempDirFailed(String),

	/// Not an HTTPS URL.
	#[error("Not an HTTPS URL: {0}")]
	NotHttps(String),

	/// Clone destination error.
	#[error("Clone destination error at {path}: {reason}")]
	DestinationError {
		/// Path where the error occurred.
		path: PathBuf,
		/// Reason for the error.
		reason: String,
	},
}

impl GitError {
	/// Create a clone failed error with a message.
	///
	/// The message is run through [`crate::redact::redact_url_userinfo`] so that
	/// any credentials embedded in a URL never leak into the error string.
	pub fn clone_failed(msg: impl Into<String>) -> Self {
		Self::CloneFailed(crate::redact::redact_url_userinfo(&msg.into()))
	}

	/// Create an invalid URL error.
	pub fn invalid_url(url: impl Into<String>) -> Self {
		Self::InvalidUrl(crate::redact::redact_url_userinfo(&url.into()))
	}

	/// Create a not HTTPS error.
	pub fn not_https(url: impl Into<String>) -> Self {
		Self::NotHttps(crate::redact::redact_url_userinfo(&url.into()))
	}

	/// Create a destination error.
	pub fn destination_error(
		path: impl Into<PathBuf>,
		reason: impl Into<String>,
	) -> Self {
		Self::DestinationError {
			path: path.into(),
			reason: crate::redact::redact_url_userinfo(&reason.into()),
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn clone_failed_display_redacts_userinfo() {
		let e = GitError::clone_failed(
			"Fetch failed: could not read https://user:ghp_SECRET@github.com/o/r.git",
		);
		let shown = e.to_string();
		assert!(!shown.contains("ghp_SECRET"));
		assert!(!shown.contains("user:"));
	}

	#[test]
	fn invalid_url_and_not_https_redact_userinfo() {
		let invalid =
			GitError::invalid_url("https://user:ghp_SECRET@github.com/o/r")
				.to_string();
		assert!(!invalid.contains("ghp_SECRET") && !invalid.contains("user:"));

		let not_https =
			GitError::not_https("https://user:ghp_SECRET@github.com/o/r")
				.to_string();
		assert!(
			!not_https.contains("ghp_SECRET") && !not_https.contains("user:")
		);
	}

	#[test]
	fn destination_error_reason_redacts_userinfo() {
		let e = GitError::destination_error(
			"/tmp/x",
			"failed cloning https://user:ghp_SECRET@github.com/o/r.git",
		);
		let shown = e.to_string();
		assert!(!shown.contains("ghp_SECRET") && !shown.contains("user:"));
	}
}

/// Result type alias for git operations.
pub type Result<T> = std::result::Result<T, GitError>;
