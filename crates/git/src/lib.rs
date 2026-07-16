//! Git clone library with credential injection from environment variables.
//!
//! This library provides functionality to clone git repositories into
//! temporary directories with credentials automatically injected from
//! environment variables.
//!
//! # Environment Variables
//!
//! - `GIT_USERNAME`: Git username for authentication
//! - `GIT_PASSWORD`: Git password or personal access token
//!
//! # Example
//!
//! ```rust,no_run
//! use aghub_git::{clone_to_temp, CloneOptions};
//!
//! // Set credentials via environment (or set them before running)
//! std::env::set_var("GIT_USERNAME", "myuser");
//! std::env::set_var("GIT_PASSWORD", "mytoken");
//!
//! // Clone a repository
//! let temp_dir = clone_to_temp(
//!     CloneOptions::new("https://github.com/user/repo.git")
//! ).unwrap();
//! println!("Cloned to: {}", temp_dir.path().display());
//!
//! // The temp directory is cleaned up automatically when dropped
//! ```
//!
//! # Explicit Credentials
//!
//! You can also provide credentials explicitly:
//!
//! ```rust,no_run
//! use aghub_git::{clone_to_temp, CloneOptions};
//!
//! let temp_dir = clone_to_temp(
//!     CloneOptions::new("https://github.com/user/private-repo.git")
//!         .with_credentials("myuser", "my_personal_access_token")
//! ).unwrap();
//! ```

pub mod clone;
pub mod credentials;
pub mod error;
pub mod fetch;
pub mod redact;
pub mod remote;
pub mod source;
pub mod system_git;
pub mod tree;

// Re-export commonly used items
pub use clone::{clone_to_path, clone_to_temp, CloneOptions};
pub use credentials::{inject_credentials, read_credentials, Credentials};
pub use error::{GitError, Result};
pub use fetch::{
	classify_ref, current_branch_at_path, fetch_ref_to_temp,
	resolve_default_branch, RefKind,
};
pub use redact::redact_url_userinfo;
pub use remote::{
	list_remote_branches, resolve_ref_oid, select_ref_oid, RemoteOptions,
};
pub use source::{
	normalize_repo_source_from_url, normalize_tfs_clone_url,
	resolve_remote_source, RemoteSourceType, ResolvedRemoteSource, SourceError,
};
pub use system_git::{
	clone_to_temp_system_git, list_remote_branches_system_git,
	probe_credential, system_git_available,
};
pub use tree::{is_safe_tree_entry_name, materialize_tree};

/// An immutable pin of a resolved repository state: the three OIDs kept
/// deliberately distinct. `commit_oid` is the ONLY value a lock's `refCommit`
/// may record; `tree_oid` is a tree object id (e.g. the GitHub REST trees API
/// root `sha`) and must NEVER be written to a lock. `commit_time` is the
/// best-effort RFC 3339 author time of the tip commit — `None` when it cannot
/// be read (shallow fetch, old gix, parse error).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RepoSnapshot {
	pub commit_oid: String,
	pub tree_oid: String,
	pub commit_time: Option<String>,
}
