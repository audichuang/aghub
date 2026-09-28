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

/// Credential store backing `routes::inference`'s provider store:
/// `NativeCredentialStore` in production, an injected in-memory store in tests
/// so they need no reachable keyring (GitHub #15). Stateless per call, so a
/// shared `Arc<dyn Trait>` suffices (unlike [`SkillRepositoryFactory`]).
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
