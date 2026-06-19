//! Backward-compatibility shim. The authoritative directory-link primitives
//! now live in `crate::skills::linker`. This module re-exports the types and
//! provides thin wrappers that preserve the legacy `bool` / `io::Result`
//! call signatures so existing `install_layout::` call sites keep compiling
//! during the symlink-only migration; it is DELETED in Task 47a once every
//! consumer references `crate::skills::linker` directly.
use std::io;
use std::path::{Path, PathBuf};

pub use crate::skills::linker::{
	universal_canonical_dir, LinkError, LinkOutcome, LinkTarget, Linker,
	UniversalInstallReport,
};

/// Compatibility wrapper: calls
/// [`crate::skills::linker::install_universal`] with a `bool` flag
/// converted to [`LinkTarget`], returning `io::Result`.
pub fn install_universal(
	source_root: &Path,
	canonical: &Path,
	agent_skills_dirs: &[PathBuf],
	use_relative_links: bool,
) -> io::Result<UniversalInstallReport> {
	let target = if use_relative_links {
		LinkTarget::Relative
	} else {
		LinkTarget::Absolute
	};
	crate::skills::linker::install_universal(
		source_root,
		canonical,
		agent_skills_dirs,
		target,
	)
	.map_err(|e| match e {
		LinkError::Io(io_err) => io_err,
		other => io::Error::other(other.to_string()),
	})
}

/// Compatibility wrapper: calls
/// [`crate::skills::linker::link_agents_to_canonical`] with a `bool`
/// flag converted to [`LinkTarget`], returning `io::Result`.
pub fn link_agents_to_canonical(
	canonical: &Path,
	agent_skills_dirs: &[PathBuf],
	use_relative_links: bool,
) -> io::Result<UniversalInstallReport> {
	let target = if use_relative_links {
		LinkTarget::Relative
	} else {
		LinkTarget::Absolute
	};
	crate::skills::linker::link_agents_to_canonical(
		canonical,
		agent_skills_dirs,
		target,
	)
	.map_err(|e| match e {
		LinkError::Io(io_err) => io_err,
		other => io::Error::other(other.to_string()),
	})
}
