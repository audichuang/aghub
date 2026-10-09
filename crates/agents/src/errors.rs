use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use thiserror::Error;

/// A reader of a kept path
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RejectedTargetReader {
	pub agent: String,
	pub managed: bool,
}

/// A target rejected during preflight validation
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RejectedTarget {
	pub agent: String,
	pub reason: String,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub kind: Option<String>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub path: Option<String>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub readers: Option<Vec<RejectedTargetReader>>,
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

	#[error("Invalid configuration: {message}")]
	InvalidConfigWithTargets {
		message: String,
		rejected_targets: Option<Vec<RejectedTarget>>,
	},

	/// The resource belongs to another installer (a Claude Code plugin);
	/// aghub refuses to mutate it.
	#[error("{0}")]
	ManagedResource(String),
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

	pub fn unsupported_operation_with_targets(
		operation: impl Into<String>,
		resource_type: impl Into<String>,
		agent: impl Into<String>,
		rejected_targets: Option<Vec<RejectedTarget>>,
	) -> Self {
		Self::UnsupportedOperation {
			message: format!(
				"Cannot {} {} for {} agent",
				operation.into(),
				resource_type.into(),
				agent.into()
			),
			rejected_targets,
		}
	}

	pub fn unsupported_op(message: impl Into<String>) -> Self {
		Self::UnsupportedOperation {
			message: message.into(),
			rejected_targets: None,
		}
	}

	pub fn invalid_config_with_targets(
		message: impl Into<String>,
		rejected_targets: Option<Vec<RejectedTarget>>,
	) -> Self {
		Self::InvalidConfigWithTargets {
			message: message.into(),
			rejected_targets,
		}
	}

	pub fn rejected_targets(&self) -> Option<&[RejectedTarget]> {
		match self {
			Self::UnsupportedOperation {
				rejected_targets, ..
			}
			| Self::InvalidConfigWithTargets {
				rejected_targets, ..
			} => rejected_targets.as_deref(),
			_ => None,
		}
	}
}

pub type Result<T> = std::result::Result<T, ConfigError>;
