use std::path::PathBuf;
use thiserror::Error;

/// Errors that can occur in the core library
#[derive(Error, Debug)]
pub enum ConfigError {
	#[error("IO error: {0}")]
	Io(#[from] std::io::Error),

	#[error("JSON parsing error: {0}")]
	Json(#[from] serde_json::Error),

	#[error("Configuration file not found: {path}")]
	NotFound { path: PathBuf },

	#[error("Resource not found: {resource_type} '{name}'")]
	ResourceNotFound { resource_type: String, name: String },

	#[error("Resource already exists: {resource_type} '{name}'")]
	ResourceExists { resource_type: String, name: String },

	#[error("Agent validation failed: {0}")]
	ValidationFailed(String),

	#[error("Unsupported operation for agent: {0}")]
	UnsupportedOperation(String),

	#[error("Invalid configuration: {0}")]
	InvalidConfig(String),
}

impl ConfigError {
	pub fn not_found(path: impl Into<PathBuf>) -> Self {
		Self::NotFound { path: path.into() }
	}

	pub fn resource_not_found(
		resource_type: impl Into<String>,
		name: impl Into<String>,
	) -> Self {
		Self::ResourceNotFound {
			resource_type: resource_type.into(),
			name: name.into(),
		}
	}

	pub fn resource_exists(
		resource_type: impl Into<String>,
		name: impl Into<String>,
	) -> Self {
		Self::ResourceExists {
			resource_type: resource_type.into(),
			name: name.into(),
		}
	}

	pub fn unsupported_operation(
		operation: impl Into<String>,
		resource_type: impl Into<String>,
		agent: impl Into<String>,
	) -> Self {
		Self::UnsupportedOperation(format!(
			"Cannot {} {} for {} agent",
			operation.into(),
			resource_type.into(),
			agent.into()
		))
	}
}

/// Note: this [`Clone`] implementation is lossy for [`ConfigError::Io`] and
/// [`ConfigError::Json`]. Because neither underlying error type implements `Clone`,
/// `Io` is reconstructed from its `ErrorKind` and formatted message (losing raw OS
/// error codes / inner source errors) and `Json` is reconstructed from its formatted
/// message (losing parsed line/column numbers).
impl Clone for ConfigError {
	fn clone(&self) -> Self {
		match self {
			Self::Io(e) => {
				Self::Io(std::io::Error::new(e.kind(), e.to_string()))
			}
			Self::Json(e) => {
				use serde::de::Error;
				Self::Json(serde_json::Error::custom(e.to_string()))
			}
			Self::NotFound { path } => Self::NotFound { path: path.clone() },
			Self::ResourceNotFound {
				resource_type,
				name,
			} => Self::ResourceNotFound {
				resource_type: resource_type.clone(),
				name: name.clone(),
			},
			Self::ResourceExists {
				resource_type,
				name,
			} => Self::ResourceExists {
				resource_type: resource_type.clone(),
				name: name.clone(),
			},
			Self::ValidationFailed(s) => Self::ValidationFailed(s.clone()),
			Self::UnsupportedOperation(s) => {
				Self::UnsupportedOperation(s.clone())
			}
			Self::InvalidConfig(s) => Self::InvalidConfig(s.clone()),
		}
	}
}

pub type Result<T> = std::result::Result<T, ConfigError>;
