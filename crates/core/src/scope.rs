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

	pub fn label(&self) -> &'static str {
		match self {
			Self::Global => "global",
			Self::Project { .. } => "project",
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn write_scope_properties() {
		let global = WriteScope::global();
		assert_eq!(global.resource_scope(), ResourceScope::GlobalOnly);
		assert_eq!(global.project_root(), None);
		assert_eq!(global.label(), "global");

		let project = WriteScope::project("/test/path");
		assert_eq!(project.resource_scope(), ResourceScope::ProjectOnly);
		assert_eq!(project.project_root(), Some(Path::new("/test/path")));
		assert_eq!(project.label(), "project");
	}
}
