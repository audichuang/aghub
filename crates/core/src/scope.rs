use crate::models::ResourceScope;
use std::path::{Path, PathBuf};

/// Exactly one scope a mutation targets. Unlike [`ResourceScope`] this makes
/// the illegal states unrepresentable: there is no `Both`, and a project
/// mutation always carries its root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteScope {
	Global,
	Project { root: PathBuf },
}

impl WriteScope {
	pub fn global() -> Self {
		Self::Global
	}

	pub fn project(root: impl Into<PathBuf>) -> Self {
		Self::Project { root: root.into() }
	}

	pub fn resource_scope(&self) -> ResourceScope {
		match self {
			Self::Global => ResourceScope::GlobalOnly,
			Self::Project { .. } => ResourceScope::ProjectOnly,
		}
	}

	pub fn project_root(&self) -> Option<&Path> {
		match self {
			Self::Global => None,
			Self::Project { root } => Some(root),
		}
	}
}
