use std::path::PathBuf;
use std::sync::Arc;

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
