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

	/// Do the two scopes write one lock file? Use this, not `==`, which
	/// compares raw root spellings.
	pub fn shares_lock_with(&self, other: &WriteScope) -> bool {
		match (self, other) {
			(Self::Global, Self::Global) => true,
			(Self::Project { root: a }, Self::Project { root: b }) => {
				crate::skills::path_identity::resolved_location(a)
					== crate::skills::path_identity::resolved_location(b)
			}
			_ => false,
		}
	}

	pub fn label(&self) -> &'static str {
		match self {
			Self::Global => "global",
			Self::Project { .. } => "project",
		}
	}
}
