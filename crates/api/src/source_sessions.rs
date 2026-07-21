use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use skill_update::{FetchedRepo, SkillRepoError, SkillRepository};
use tokio::time::timeout;

const SESSION_TTL: Duration = Duration::from_secs(10 * 60);
const FETCH_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Debug)]
pub(crate) enum PinnedSourceFetchError {
	Repository(SkillRepoError),
	Task(tokio::task::JoinError),
	Timeout,
}

#[derive(Clone)]
pub(crate) struct PinnedSourceSession {
	/// The skill-aware repository that resolved `snapshot` (the single REST→gix
	/// fallback owner). The repository and immutable snapshot stay paired so a
	/// later selective fetch cannot accidentally re-resolve a moving branch.
	repository: Arc<SkillRepository>,
	snapshot: aghub_git::RepoSnapshot,
	created_at: Instant,
	/// The original clone URL (without credentials).
	url: String,
	/// Resolved credential token, origin-pinned to the clone-URL origin.
	credential_token: Option<String>,
	branches: Vec<String>,
	current_branch: String,
}

impl PinnedSourceSession {
	pub(crate) fn new(
		repository: Arc<SkillRepository>,
		snapshot: aghub_git::RepoSnapshot,
		url: String,
		credential_token: Option<String>,
		branches: Vec<String>,
		current_branch: String,
	) -> Self {
		Self {
			repository,
			snapshot,
			created_at: Instant::now(),
			url,
			credential_token,
			branches,
			current_branch,
		}
	}

	pub(crate) fn url(&self) -> &str {
		&self.url
	}

	pub(crate) fn credential_token(&self) -> Option<&str> {
		self.credential_token.as_deref()
	}

	pub(crate) fn branches(&self) -> &[String] {
		&self.branches
	}

	pub(crate) fn current_branch(&self) -> &str {
		&self.current_branch
	}

	#[cfg(test)]
	pub(crate) fn commit_oid(&self) -> &str {
		&self.snapshot.commit_oid
	}

	pub(crate) async fn fetch_skills(
		&self,
		skill_paths: &[skill::SkillPath],
	) -> Result<FetchedRepo, PinnedSourceFetchError> {
		let repository = Arc::clone(&self.repository);
		let snapshot = self.snapshot.clone();
		let skill_paths = skill_paths.to_vec();
		match timeout(
			FETCH_TIMEOUT,
			tokio::task::spawn_blocking(move || {
				repository.fetch(
					&snapshot,
					skill_update::FetchSelection::Skills(&skill_paths),
				)
			}),
		)
		.await
		{
			Ok(Ok(Ok(fetched))) => Ok(fetched),
			Ok(Ok(Err(error))) => {
				Err(PinnedSourceFetchError::Repository(error))
			}
			Ok(Err(error)) => Err(PinnedSourceFetchError::Task(error)),
			Err(_) => Err(PinnedSourceFetchError::Timeout),
		}
	}

	#[cfg(test)]
	pub(crate) fn set_created_at(&mut self, created_at: Instant) {
		self.created_at = created_at;
	}
}

#[derive(Default)]
pub struct PinnedSourceSessions {
	sessions: Arc<Mutex<HashMap<String, PinnedSourceSession>>>,
}

/// Exclusive lease for one pinned Source session.
///
/// Claiming removes the session from the active map, so a concurrent request
/// or replay cannot use the same commit-pinned repository. Failed operations
/// restore the session on drop; successful operations call [`Self::consume`]
/// to make the removal permanent.
pub(crate) struct PinnedSourceClaim {
	sessions: Arc<Mutex<HashMap<String, PinnedSourceSession>>>,
	session_id: String,
	session: Option<PinnedSourceSession>,
}

impl PinnedSourceClaim {
	pub(crate) fn consume(mut self) {
		self.session = None;
	}
}

impl std::ops::Deref for PinnedSourceClaim {
	type Target = PinnedSourceSession;

	fn deref(&self) -> &Self::Target {
		self.session
			.as_ref()
			.expect("a live claim always contains its session")
	}
}

impl Drop for PinnedSourceClaim {
	fn drop(&mut self) {
		let Some(session) = self.session.take() else {
			return;
		};
		let mut sessions = self.sessions.lock().unwrap();
		sessions.entry(self.session_id.clone()).or_insert(session);
	}
}

impl PinnedSourceSessions {
	pub(crate) fn active(
		&self,
		session_id: &str,
	) -> Option<PinnedSourceSession> {
		let mut sessions = self.sessions.lock().unwrap();
		Self::evict_expired(&mut sessions);
		sessions.get(session_id).cloned()
	}

	pub(crate) fn claim(&self, session_id: &str) -> Option<PinnedSourceClaim> {
		let session = {
			let mut sessions = self.sessions.lock().unwrap();
			Self::evict_expired(&mut sessions);
			sessions.remove(session_id)?
		};
		Some(PinnedSourceClaim {
			sessions: Arc::clone(&self.sessions),
			session_id: session_id.to_string(),
			session: Some(session),
		})
	}

	pub(crate) fn insert(
		&self,
		session_id: String,
		session: PinnedSourceSession,
	) {
		let mut sessions = self.sessions.lock().unwrap();
		Self::evict_expired(&mut sessions);
		sessions.insert(session_id, session);
	}

	pub(crate) fn replace(
		&self,
		old_session_id: &str,
		session_id: String,
		session: PinnedSourceSession,
	) {
		let mut sessions = self.sessions.lock().unwrap();
		Self::evict_expired(&mut sessions);
		sessions.remove(old_session_id);
		sessions.insert(session_id, session);
	}

	fn evict_expired(sessions: &mut HashMap<String, PinnedSourceSession>) {
		sessions
			.retain(|_, session| session.created_at.elapsed() < SESSION_TTL);
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn session() -> PinnedSourceSession {
		PinnedSourceSession::new(
			Arc::new(SkillRepository::new()),
			aghub_git::RepoSnapshot {
				commit_oid: "commit".to_string(),
				tree_oid: "tree".to_string(),
				commit_time: None,
			},
			"https://github.com/owner/repo.git".to_string(),
			None,
			vec!["main".to_string()],
			"main".to_string(),
		)
	}

	#[test]
	fn claimed_session_is_exclusive_and_failure_restores_it() {
		let sessions = PinnedSourceSessions::default();
		sessions.insert("id".to_string(), session());

		let claim = sessions.claim("id").expect("first claim");
		assert!(sessions.claim("id").is_none(), "claim must be exclusive");
		drop(claim);

		assert!(
			sessions.active("id").is_some(),
			"dropping a failed operation must retain the session for retry",
		);
	}

	#[test]
	fn successful_claim_consumes_session() {
		let sessions = PinnedSourceSessions::default();
		sessions.insert("id".to_string(), session());

		let claim = sessions.claim("id").expect("claim");
		claim.consume();

		assert!(sessions.active("id").is_none());
	}
}
