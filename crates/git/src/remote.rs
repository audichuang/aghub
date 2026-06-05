//! Remote git URL resolution and ref discovery.

use crate::credentials::{
	inject_credentials, noninteractive_credentials, read_credentials,
	Credentials,
};
use crate::error::{GitError, Result};

/// Options shared by remote git operations.
#[derive(Debug, Clone)]
pub struct RemoteOptions<'a> {
	/// HTTPS URL of the git repository.
	pub url: &'a str,
	/// Explicit credentials for the operation.
	pub credentials: Option<Credentials>,
}

impl<'a> RemoteOptions<'a> {
	/// Create options for a repository URL.
	pub fn new(url: &'a str) -> Self {
		Self {
			url,
			credentials: None,
		}
	}

	/// Attach explicit credentials to the operation.
	pub fn with_credentials(
		mut self,
		username: impl Into<String>,
		password: impl Into<String>,
	) -> Self {
		self.credentials = Some(Credentials::new(username, password));
		self
	}

	/// Attach an existing credentials value to the operation.
	pub fn with_auth(mut self, credentials: Credentials) -> Self {
		self.credentials = Some(credentials);
		self
	}
}

pub fn list_remote_branches(options: RemoteOptions<'_>) -> Result<Vec<String>> {
	let url = resolve_remote_url(&options, false)?;
	let remote_refs = discover_remote_refs(url.as_str())?;
	Ok(branches_from_remote_refs(&remote_refs))
}

pub(crate) fn resolve_remote_url(
	options: &RemoteOptions<'_>,
	use_env_credentials: bool,
) -> Result<String> {
	let env_credentials = if use_env_credentials {
		read_credentials()
	} else {
		None
	};
	let credentials = options.credentials.as_ref().or(env_credentials.as_ref());

	match credentials {
		Some(credentials) => inject_credentials(options.url, credentials),
		None => {
			validate_https_url(options.url).map(|()| options.url.to_string())
		}
	}
}

fn validate_https_url(url: &str) -> Result<()> {
	let parsed = url::Url::parse(url).map_err(GitError::from)?;
	if parsed.scheme() != "https" {
		return Err(GitError::not_https(url));
	}
	Ok(())
}

fn discover_remote_refs(
	url: &str,
) -> Result<Vec<gix::protocol::handshake::Ref>> {
	let temp_dir = tempfile::TempDir::new()
		.map_err(|e| GitError::TempDirFailed(e.to_string()))?;
	let repo = gix::init(temp_dir.path())
		.map_err(|e| GitError::clone_failed(e.to_string()))?;
	let remote = repo
		.remote_at(url)
		.map_err(|e| GitError::clone_failed(e.to_string()))?;
	let remote = remote
		.with_refspecs(
			[
				"+refs/heads/*:refs/remotes/origin/*",
				"+refs/tags/*:refs/remotes/origin/tags/*",
				"+HEAD:refs/remotes/origin/HEAD",
			],
			gix::remote::Direction::Fetch,
		)
		.map_err(|e| GitError::clone_failed(e.to_string()))?;
	let mut connection = remote
		.connect(gix::remote::Direction::Fetch)
		.map_err(|e| GitError::clone_failed(e.to_string()))?;
	connection.set_credentials(noninteractive_credentials);
	let (ref_map, _) = connection
		.ref_map(
			gix::progress::Discard,
			gix::remote::ref_map::Options::default(),
		)
		.map_err(|e| GitError::clone_failed(e.to_string()))?;

	Ok(ref_map.remote_refs)
}

/// Select the commit OID (40-hex) for `wanted` from an advertised ref list.
///
/// `wanted` is matched against `refs/heads/<wanted>` first, then
/// `refs/tags/<wanted>` (annotated tags resolve to the peeled commit). `None`
/// follows the remote `HEAD` symref to the tip of its target branch. Returns
/// `None` when the ref is not advertised.
pub fn select_ref_oid(
	refs: &[gix::protocol::handshake::Ref],
	wanted: Option<&str>,
) -> Option<String> {
	use gix::bstr::ByteSlice;
	use gix::protocol::handshake::Ref;

	// The commit OID a fully-qualified ref resolves to: for annotated tags
	// (`Peeled`) this is the peeled `object` (the commit), never the tag object.
	let oid_for = |full: &str| -> Option<String> {
		refs.iter().find_map(|r| match r {
			Ref::Direct {
				full_ref_name,
				object,
			}
			| Ref::Peeled {
				full_ref_name,
				object,
				..
			} if full_ref_name.to_str_lossy().as_ref() == full => {
				Some(object.to_string())
			}
			_ => None,
		})
	};

	match wanted {
		Some(name) => oid_for(&format!("refs/heads/{name}"))
			.or_else(|| oid_for(&format!("refs/tags/{name}"))),
		None => {
			// The remote default branch is whatever HEAD points at. HEAD may be
			// advertised as Symbolic (symref capability) OR as a plain Direct
			// ref; match it by name in any resolved variant.
			for r in refs {
				let (full_ref_name, object) = match r {
					Ref::Direct {
						full_ref_name,
						object,
					}
					| Ref::Peeled {
						full_ref_name,
						object,
						..
					}
					| Ref::Symbolic {
						full_ref_name,
						object,
						..
					} => (full_ref_name, object),
					Ref::Unborn { .. } => continue,
				};
				if full_ref_name.to_str_lossy().as_ref() == "HEAD" {
					return Some(object.to_string());
				}
			}
			None
		}
	}
}

/// Resolve the tip commit OID (40-hex) of `wanted` on a remote via a ref
/// advertisement (no object download). `wanted` is a branch or tag name, or
/// `None` for the remote default branch. Credentials in `options` are injected
/// so private repos work. Returns `Ok(None)` when the ref is not advertised.
pub fn resolve_ref_oid(
	options: RemoteOptions<'_>,
	wanted: Option<&str>,
) -> Result<Option<String>> {
	let url = resolve_remote_url(&options, false)?;
	let refs = discover_remote_refs(url.as_str())?;
	Ok(select_ref_oid(&refs, wanted))
}

pub(crate) fn branches_from_remote_refs(
	remote_refs: &[gix::protocol::handshake::Ref],
) -> Vec<String> {
	use gix::bstr::ByteSlice;

	let mut branches: Vec<String> = remote_refs
		.iter()
		.filter_map(|remote_ref| match remote_ref {
			gix::protocol::handshake::Ref::Direct { full_ref_name, .. }
			| gix::protocol::handshake::Ref::Peeled { full_ref_name, .. } => {
				full_ref_name
					.strip_prefix(b"refs/heads/" as &[u8])
					.map(|name| name.to_str_lossy().to_string())
			}
			gix::protocol::handshake::Ref::Symbolic { target, .. }
			| gix::protocol::handshake::Ref::Unborn { target, .. } => target
				.strip_prefix(b"refs/heads/" as &[u8])
				.map(|name| name.to_str_lossy().to_string()),
		})
		.collect();
	branches.sort();
	branches.dedup();
	branches
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::sync::{Mutex, OnceLock};

	fn env_lock() -> &'static Mutex<()> {
		static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
		LOCK.get_or_init(|| Mutex::new(()))
	}

	#[test]
	fn test_list_remote_branches_public_repo() {
		let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
		let branches = list_remote_branches(RemoteOptions::new(
			"https://github.com/octocat/Hello-World.git",
		))
		.unwrap();
		assert!(!branches.is_empty());
		assert!(branches.contains(&"master".to_string()));
	}

	#[test]
	fn select_ref_oid_matches_branch_head() {
		use gix::protocol::handshake::Ref;
		let oid = gix::ObjectId::from_hex(
			b"1234567890abcdef1234567890abcdef12345678",
		)
		.unwrap();
		let refs = vec![Ref::Direct {
			full_ref_name: "refs/heads/main".into(),
			object: oid,
		}];
		assert_eq!(
			select_ref_oid(&refs, Some("main")),
			Some("1234567890abcdef1234567890abcdef12345678".to_string())
		);
	}

	#[test]
	fn select_ref_oid_matches_annotated_tag_peeled() {
		use gix::protocol::handshake::Ref;
		let tag_obj = gix::ObjectId::from_hex(
			b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
		)
		.unwrap();
		let commit = gix::ObjectId::from_hex(
			b"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
		)
		.unwrap();
		let refs = vec![Ref::Peeled {
			full_ref_name: "refs/tags/v1.0".into(),
			tag: tag_obj,
			object: commit,
		}];
		// The peeled commit, NOT the annotated tag object.
		assert_eq!(
			select_ref_oid(&refs, Some("v1.0")),
			Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string())
		);
	}

	#[test]
	fn select_ref_oid_none_follows_head_symref() {
		use gix::protocol::handshake::Ref;
		let oid = gix::ObjectId::from_hex(
			b"cccccccccccccccccccccccccccccccccccccccc",
		)
		.unwrap();
		let refs = vec![
			Ref::Symbolic {
				full_ref_name: "HEAD".into(),
				target: "refs/heads/main".into(),
				tag: None,
				object: oid,
			},
			Ref::Direct {
				full_ref_name: "refs/heads/main".into(),
				object: oid,
			},
		];
		assert_eq!(
			select_ref_oid(&refs, None),
			Some("cccccccccccccccccccccccccccccccccccccccc".to_string())
		);
	}

	#[test]
	#[ignore = "network"]
	fn resolve_ref_oid_default_branch_over_network() {
		let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
		let oid = resolve_ref_oid(
			RemoteOptions::new("https://github.com/octocat/Hello-World.git"),
			None,
		)
		.unwrap()
		.expect("default branch should resolve to a tip oid");
		assert_eq!(oid.len(), 40);
		assert!(oid.bytes().all(|b| b.is_ascii_hexdigit()));
	}

	#[test]
	fn select_ref_oid_none_matches_head_advertised_as_direct() {
		// Some servers advertise HEAD as a plain Direct ref (no symref cap),
		// not Symbolic — None must still resolve it.
		use gix::protocol::handshake::Ref;
		let oid = gix::ObjectId::from_hex(
			b"00000000000000000000000000000000deadbeef",
		)
		.unwrap();
		let refs = vec![Ref::Direct {
			full_ref_name: "HEAD".into(),
			object: oid,
		}];
		assert_eq!(
			select_ref_oid(&refs, None),
			Some("00000000000000000000000000000000deadbeef".to_string())
		);
	}

	#[test]
	fn select_ref_oid_returns_none_when_absent() {
		use gix::protocol::handshake::Ref;
		let oid = gix::ObjectId::from_hex(
			b"dddddddddddddddddddddddddddddddddddddddd",
		)
		.unwrap();
		let refs = vec![Ref::Direct {
			full_ref_name: "refs/heads/main".into(),
			object: oid,
		}];
		assert_eq!(select_ref_oid(&refs, Some("nope")), None);
	}

	#[test]
	fn test_branches_from_remote_refs() {
		use gix::protocol::handshake::Ref;

		let null_id = gix::hash::ObjectId::null(gix::hash::Kind::Sha1);
		let branches = branches_from_remote_refs(&[
			Ref::Direct {
				full_ref_name: "refs/heads/main".into(),
				object: null_id,
			},
			Ref::Symbolic {
				full_ref_name: "HEAD".into(),
				target: "refs/heads/main".into(),
				tag: None,
				object: gix::hash::ObjectId::null(gix::hash::Kind::Sha1),
			},
			Ref::Unborn {
				full_ref_name: "HEAD".into(),
				target: "refs/heads/develop".into(),
			},
			Ref::Peeled {
				full_ref_name: "refs/heads/release".into(),
				tag: gix::hash::ObjectId::null(gix::hash::Kind::Sha1),
				object: gix::hash::ObjectId::null(gix::hash::Kind::Sha1),
			},
		]);

		assert_eq!(
			branches,
			vec![
				"develop".to_string(),
				"main".to_string(),
				"release".to_string(),
			],
		);
	}
}
