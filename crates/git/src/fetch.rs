//! Treeless/bare ref fetch (no worktree checkout) + default-branch resolution via
//! gix HEAD symref. Never shells out to the `git` binary.
//!
//! # Chosen gix 0.83 API (spike result)
//!
//! gix 0.83 does NOT expose a partial (`blob:none`) fetch filter on the
//! blocking clone path, so per the plan's documented FALLBACK we perform a
//! **bare fetch with no worktree checkout**:
//!
//! 1. `gix::clone::PrepareFetch::new(url, tmp, gix::create::Kind::Bare, ...)`
//!    creates a bare repository (object DB lives at the temp-dir root, not under
//!    `.git`).
//! 2. `PrepareFetch::fetch_only(progress, should_interrupt)` fetches all objects
//!    and packed refs WITHOUT calling `main_worktree()` — so there is never a
//!    worktree checkout. This is the blocking variant (the crate enables
//!    `blocking-network-client`, so `maybe_async` resolves to a blocking call).
//! 3. During `fetch_only`, gix's `update_head` writes the local `HEAD` as a
//!    symbolic ref to the remote default branch (e.g. `refs/heads/main`) and
//!    materializes that branch ref at the fetched commit. We therefore resolve
//!    the default branch purely from the local `HEAD` symref via
//!    `repo.head_name()` — **no `std::process::Command`**.
//! 4. The skill subtree is read from the populated object DB in Task F1.3
//!    (no checkout required).

use tempfile::TempDir;

use crate::credentials::{
	inject_credentials, noninteractive_credentials, Credentials,
};
use crate::error::{GitError, Result};
use std::time::Duration;

/// The classification of a requested ref.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefKind {
	/// Branch name, or `None` to mean "the remote default branch".
	Branch(Option<String>),
	/// A pin (40-hex commit SHA) — intentional; callers report `UpToDate`.
	Pinned(String),
}

/// Classify a ref string.
///
/// A 40-character all-hex string is a pinned commit SHA. Any other `Some` value
/// is treated as a branch name; `None` means the remote default branch.
///
/// Tag-vs-branch disambiguation is intentionally left to the caller (F1.3),
/// which decides "tag-as-pin" from the lock's recorded source metadata.
pub fn classify_ref(r: Option<&str>) -> RefKind {
	match r {
		Some(s) if is_sha(s) => RefKind::Pinned(s.to_string()),
		Some(s) => RefKind::Branch(Some(s.to_string())),
		None => RefKind::Branch(None),
	}
}

/// `true` when `s` is a 40-character lowercase/uppercase hex string (a commit SHA).
fn is_sha(s: &str) -> bool {
	s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Fetch `ref_` of `url` into a fresh temp dir WITHOUT a worktree checkout.
///
/// The returned [`TempDir`] holds a **bare** git object store. The resolved
/// commit [`gix::ObjectId`] is the tip of the requested ref (or of the remote
/// default branch when `ref_` is `None`).
///
/// Credentials, when present, are injected into the URL so PAT auth works.
/// Every gix error is funneled through [`GitError::clone_failed`], which
/// redacts URL userinfo so embedded tokens never leak.
pub fn fetch_ref_to_temp(
	url: &str,
	ref_: Option<&str>,
	creds: Option<&Credentials>,
	total_timeout: Option<Duration>,
) -> Result<(TempDir, gix::ObjectId)> {
	let temp_dir =
		TempDir::new().map_err(|e| GitError::TempDirFailed(e.to_string()))?;

	// Inject credentials so private-repo PAT auth works. The fetch URL is only
	// ever passed to gix; gix errors are redacted via GitError::clone_failed.
	let fetch_url = match creds {
		Some(creds) => inject_credentials(url, creds)?,
		None => url.to_string(),
	};

	let oid =
		fetch_into_bare(&fetch_url, temp_dir.path(), ref_, total_timeout)?;
	// A bare repo keeps its config at the dir root (not under .git). gix wrote
	// the token-bearing fetch URL there; strip it (no-op when creds were None).
	if creds.is_some() {
		crate::redact::scrub_config_userinfo(&temp_dir.path().join("config"));
	}
	Ok((temp_dir, oid))
}

/// Resolve the remote default branch of an already-fetched repo via its `HEAD`
/// symref, stripping the `refs/heads/` prefix. No subprocess.
pub fn resolve_default_branch(repo: &gix::Repository) -> Result<String> {
	let head_name = repo
		.head_name()
		.map_err(|e| {
			GitError::clone_failed(format!("Reading HEAD failed: {e}"))
		})?
		.ok_or_else(|| {
			GitError::clone_failed(
				"Remote HEAD is detached; cannot resolve default branch"
					.to_string(),
			)
		})?;

	let full = head_name.as_bstr().to_string();
	let branch = full.strip_prefix("refs/heads/").unwrap_or(&full);
	Ok(branch.to_string())
}

/// Resolve the checked-out branch of a repository on disk via its `HEAD`
/// symref. Opens the repo with gix (no subprocess) and returns the branch name
/// with the `refs/heads/` prefix stripped. Returns `None` if the path is not a
/// repository or `HEAD` is detached/empty.
pub fn current_branch_at_path(repo_path: &std::path::Path) -> Option<String> {
	let repo = gix::open(repo_path).ok()?;
	let branch = resolve_default_branch(&repo).ok()?;
	if branch.is_empty() {
		None
	} else {
		Some(branch)
	}
}

fn fetch_into_bare(
	url: &str,
	dest: &std::path::Path,
	ref_: Option<&str>,
	total_timeout: Option<Duration>,
) -> Result<gix::ObjectId> {
	let mut prep = gix::clone::PrepareFetch::new(
		url,
		dest,
		gix::create::Kind::Bare,
		Default::default(),
		Default::default(),
	)
	.map_err(|e| GitError::clone_failed(e.to_string()))?;

	prep = prep.configure_connection(move |connection| {
		connection.set_credentials(noninteractive_credentials);
		if let Some(timeout) = total_timeout {
			let options = gix::protocol::transport::client::blocking_io::http::reqwest::Options {
				configure_request: Some(Box::new(move |request| {
					*request.timeout_mut() = Some(timeout);
					Ok(())
				})),
			};
			connection.set_transport_options(Box::new(options));
		}
		Ok(())
	});

	if let Some(branch) = ref_ {
		prep = prep.with_ref_name(Some(branch)).map_err(
			|e: gix::refs::name::Error| {
				GitError::clone_failed(format!(
					"Invalid ref name '{branch}': {e}"
				))
			},
		)?;
	}

	// Fetch only — NO `main_worktree()` checkout.
	let (repo, _outcome) = prep
		.fetch_only(gix::progress::Discard, &gix::interrupt::IS_INTERRUPTED)
		.map_err(|e| GitError::clone_failed(format!("Fetch failed: {e}")))?;

	// gix's `update_head` set local HEAD to the wanted ref (or the remote default
	// branch when `ref_` is None) and materialized that branch at the tip.
	let oid = repo
		.head_id()
		.map_err(|e| {
			GitError::clone_failed(format!(
				"Resolving fetched HEAD failed: {e}"
			))
		})?
		.detach();

	Ok(oid)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn classify_sha_is_pinned() {
		assert_eq!(
			classify_ref(Some("0123456789abcdef0123456789abcdef01234567")),
			RefKind::Pinned("0123456789abcdef0123456789abcdef01234567".into())
		);
	}

	#[test]
	fn classify_uppercase_sha_is_pinned() {
		assert_eq!(
			classify_ref(Some("0123456789ABCDEF0123456789ABCDEF01234567")),
			RefKind::Pinned("0123456789ABCDEF0123456789ABCDEF01234567".into())
		);
	}

	#[test]
	fn classify_branch_is_branch() {
		assert_eq!(
			classify_ref(Some("main")),
			RefKind::Branch(Some("main".into()))
		);
	}

	#[test]
	fn classify_short_hex_is_branch() {
		// 39 chars — not a full SHA, so it's a branch.
		assert_eq!(
			classify_ref(Some("0123456789abcdef0123456789abcdef0123456")),
			RefKind::Branch(Some(
				"0123456789abcdef0123456789abcdef0123456".into()
			))
		);
	}

	#[test]
	fn classify_non_hex_40_is_branch() {
		// 40 chars but contains a non-hex char → branch, not a pin.
		let name = "zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz";
		assert_eq!(name.len(), 40);
		assert_eq!(
			classify_ref(Some(name)),
			RefKind::Branch(Some(name.into()))
		);
	}

	#[test]
	fn classify_none_is_default_branch() {
		assert_eq!(classify_ref(None), RefKind::Branch(None));
	}

	#[ignore = "network"]
	#[test]
	fn fetch_public_repo_default_branch_no_binary() {
		let (tmp, oid) = fetch_ref_to_temp(
			"https://github.com/octocat/Hello-World.git",
			None,
			None,
			None,
		)
		.unwrap();
		assert!(tmp.path().exists());
		assert!(!oid.is_null());

		let repo = gix::open(tmp.path()).unwrap();
		let branch = resolve_default_branch(&repo).unwrap();
		assert!(!branch.is_empty());
		assert!(!branch.starts_with("refs/"));
	}

	#[test]
	fn current_branch_at_path_reads_head_symref_no_binary() {
		// Build a repo on branch "main" via gix (no `git` binary), then assert
		// the on-disk HEAD symref resolves to "main".
		let tmp = TempDir::new().unwrap();
		let repo = gix::init(tmp.path()).unwrap();

		// A freshly-initialized repo has an unborn HEAD pointing at the default
		// branch name; resolve_default_branch reads that symref directly.
		let head = repo.head_name().unwrap().unwrap();
		let full = head.as_bstr().to_string();
		let expected = full.strip_prefix("refs/heads/").unwrap_or(&full);

		let detected = current_branch_at_path(tmp.path()).unwrap();
		assert_eq!(detected, expected);
		assert!(!detected.starts_with("refs/"));
	}

	#[test]
	fn current_branch_at_path_non_repo_returns_none() {
		let tmp = TempDir::new().unwrap();
		assert_eq!(current_branch_at_path(tmp.path()), None);
	}

	#[ignore = "network"]
	#[test]
	fn fetch_public_repo_named_branch_no_binary() {
		let (tmp, oid) = fetch_ref_to_temp(
			"https://github.com/octocat/Hello-World.git",
			Some("master"),
			None,
			None,
		)
		.unwrap();
		assert!(tmp.path().exists());
		assert!(!oid.is_null());
	}
}
