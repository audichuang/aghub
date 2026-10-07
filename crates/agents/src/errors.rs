use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use thiserror::Error;

/// A target rejected during preflight validation
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RejectedTarget {
	pub agent: String,
	pub reason: String,
}

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

	#[error("Unsupported operation for agent: {message}")]
	UnsupportedOperation {
		message: String,
		rejected_targets: Option<Vec<RejectedTarget>>,
	},

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
		Self::UnsupportedOperation {
			message: format!(
				"Cannot {} {} for {} agent",
				operation.into(),
				resource_type.into(),
				agent.into()
			),
			rejected_targets: None,
		}
	}

	pub fn unsupported_op(message: impl Into<String>) -> Self {
		Self::UnsupportedOperation {
			message: message.into(),
			rejected_targets: None,
		}
	}
}

pub type Result<T> = std::result::Result<T, ConfigError>;
