use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use skill_update::SkillRepository;

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

pub struct InferenceProviderState {
	pub app_data_dir: PathBuf,
}
