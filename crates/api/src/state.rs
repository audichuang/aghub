use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use skill_update::SkillRepository;

#[derive(Clone)]
pub struct SkillRepositoryFactory {
	make: Arc<dyn Fn() -> Arc<SkillRepository> + Send + Sync>,
}

impl Default for SkillRepositoryFactory {
	fn default() -> Self {
		Self {
			make: Arc::new(|| Arc::new(SkillRepository::new())),
		}
	}
}

impl SkillRepositoryFactory {
	pub fn create(&self) -> Arc<SkillRepository> {
		(self.make)()
	}

	#[cfg(test)]
	pub fn fixed(repo: Arc<SkillRepository>) -> Self {
		Self {
			make: Arc::new(move || Arc::clone(&repo)),
		}
	}
}

pub struct GitCloneSession {
	/// The skill-aware repository that resolved `snapshot` (the single REST→gix
	/// fallback owner). `fetch` routes through the SAME backend that produced the
	/// snapshot via its commit-oid memo. On the gix-fallback path the shallow
	/// clone temp dir is retained INTERNALLY by this repository's `GixShallow`
	/// cache (keyed by commit_oid) and dropped with the session; on the github
	/// REST path there is NO whole-repo temp dir. No `TempDir` is leaked here.
	pub repo: Arc<SkillRepository>,
	/// Immutable pin of the scanned commit; install/sync fetch THIS commit.
	pub snapshot: aghub_git::RepoSnapshot,
	pub created_at: Instant,
	/// The original clone URL (without credentials).
	pub url: String,
	/// Resolved credential token, origin-pinned to the resolved clone-URL origin.
	pub credential_token: Option<String>,
	/// Cached list of remote branch names.
	pub branches: Vec<String>,
	/// The branch currently checked out in this session.
	pub current_branch: String,
}

pub struct GitCloneSessions {
	pub sessions: Mutex<HashMap<String, GitCloneSession>>,
}

/// Credential store backing `routes::inference`'s provider store. Production
/// always resolves to `NativeCredentialStore` (the real OS keyring); tests
/// inject an in-memory store directly so route tests are deterministic across
/// OSes/CI, regardless of whether a keyring backend is actually reachable
/// (see GitHub #15 P1a/P1c — a hardcoded `InferenceProviderStore::new` left
/// route tests coupled to a real keyring). Unlike [`SkillRepositoryFactory`],
/// this store is stateless per call, so a plain shared `Arc<dyn Trait>`
/// suffices — no per-request factory needed.
pub struct InferenceProviderState {
	pub app_data_dir: PathBuf,
	pub credentials: Arc<dyn aghub_inference::CredentialStore + Send + Sync>,
}

impl InferenceProviderState {
	pub fn new(app_data_dir: PathBuf) -> Self {
		Self {
			app_data_dir,
			credentials: Arc::new(aghub_inference::NativeCredentialStore),
		}
	}
}
