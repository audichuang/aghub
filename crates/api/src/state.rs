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

/// Factory for the [`aghub_inference::CredentialStore`] backing
/// `routes::inference`'s provider store, following the same
/// production-default + test-injection shape as [`SkillRepositoryFactory`]
/// above. Production always resolves to `NativeCredentialStore` (the real OS
/// keyring); tests inject an in-memory store via `fixed` so route tests are
/// deterministic across OSes/CI, regardless of whether a keyring backend is
/// actually reachable (see GitHub #15 P1a/P1c — a hardcoded
/// `InferenceProviderStore::new` left route tests coupled to a real keyring).
#[derive(Clone)]
pub struct CredentialStoreFactory {
	make: Arc<
		dyn Fn() -> Arc<dyn aghub_inference::CredentialStore + Send + Sync>
			+ Send
			+ Sync,
	>,
}

impl Default for CredentialStoreFactory {
	fn default() -> Self {
		Self {
			make: Arc::new(|| Arc::new(aghub_inference::NativeCredentialStore)),
		}
	}
}

impl CredentialStoreFactory {
	pub fn create(
		&self,
	) -> Arc<dyn aghub_inference::CredentialStore + Send + Sync> {
		(self.make)()
	}

	#[cfg(test)]
	pub fn fixed(
		store: Arc<dyn aghub_inference::CredentialStore + Send + Sync>,
	) -> Self {
		Self {
			make: Arc::new(move || Arc::clone(&store)),
		}
	}
}

pub struct InferenceProviderState {
	pub app_data_dir: PathBuf,
	pub credentials: CredentialStoreFactory,
}

impl InferenceProviderState {
	pub fn new(app_data_dir: PathBuf) -> Self {
		Self {
			app_data_dir,
			credentials: CredentialStoreFactory::default(),
		}
	}
}
