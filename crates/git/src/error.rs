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

	/// A GitHub REST fast-path attempt could not complete. The caller decides
	/// what to do based on WHEN it surfaces: a `RestFallback` at **resolve**
	/// re-routes to the gix transport; a `RestFallback` **after** a successful
	/// resolve (chiefly a `truncated` tree at `read_tree`) is turned into a clean
	/// error by `SkillRepository`, NOT re-routed (gix 0.84 cannot re-fetch a
	/// pinned commit by OID). This is the ONLY error a REST backend raises for
	/// transient / unsupported-capability / not-GitHub conditions (truncated
	/// tree, rate limit, 401/403/404, network, unexpected shape).
	/// A security-validation failure must NOT be reported as `RestFallback` —
	/// it is a hard error so it is never masked by a silent fallback.
	#[error("GitHub REST unavailable, fall back to git: {0}")]
	RestFallback(String),
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

	/// Create a REST-fallback error. The message is redacted so any URL
	/// userinfo never leaks, mirroring [`Self::clone_failed`].
	pub fn rest_fallback(msg: impl Into<String>) -> Self {
		Self::RestFallback(crate::redact::redact_url_userinfo(&msg.into()))
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

/// Result type alias for git operations.
pub type Result<T> = std::result::Result<T, GitError>;

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
