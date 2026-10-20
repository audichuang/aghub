//! The Fetched Source install flow shared by every surface: agent precheck with
//! per-row attribution, and the one lock-source spelling an install records.

use crate::mutation::{
	install_fetched_source, FetchedInstallRequest, FetchedSource,
	InstallMutationError,
};
use crate::{SkillRepoError, SkillRepository, SourceRef};
use aghub_core::models::AgentType;
use aghub_core::WriteScope;

/// One per-(skill, agent) row of an install result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallRow {
	pub name: String,
	pub agent: String,
	pub success: bool,
	pub error: Option<String>,
}

/// Requested agent ids in request order, all known. Duplicates are kept. Scope
/// support is NOT filtered: core's all-target preflight must see every target.
#[derive(Debug, Clone)]
pub struct InstallAgents {
	ids: Vec<String>,
	types: Vec<AgentType>,
}

/// At least one requested id is unknown: one (raw id, message) per requested
/// agent, in request order.
#[derive(Debug, Clone)]
pub struct AgentRejection {
	per_agent: Vec<(String, String)>,
}

impl InstallAgents {
	pub fn parse(raw: &[String]) -> Result<Self, AgentRejection> {
		let parsed: Vec<_> =
			raw.iter().map(|id| (id, id.parse::<AgentType>())).collect();
		if parsed.iter().any(|(_, t)| t.is_err()) {
			let per_agent = parsed
				.into_iter()
				.map(|(id, t)| {
					let msg = match t {
						Err(_) => format!("Unknown agent '{id}'"),
						Ok(_) => "Another requested agent is invalid; nothing was written"
							.to_string(),
					};
					(id.clone(), msg)
				})
				.collect();
			return Err(AgentRejection { per_agent });
		}
		Ok(Self {
			ids: raw.to_vec(),
			types: parsed.into_iter().filter_map(|(_, t)| t.ok()).collect(),
		})
	}

	pub fn types(&self) -> &[AgentType] {
		&self.types
	}

	pub fn ids(&self) -> &[String] {
		&self.ids
	}
}

impl AgentRejection {
	/// Label-major rows, agents in request order within each label.
	pub fn rows(&self, labels: &[String]) -> Vec<InstallRow> {
		labels
			.iter()
			.flat_map(|label| {
				self.per_agent
					.iter()
					.map(move |(agent, message)| InstallRow {
						name: label.clone(),
						agent: agent.clone(),
						success: false,
						error: Some(message.clone()),
					})
			})
			.collect()
	}
}

/// THE lock coordinate an install records (both API routes and CLI source sync).
pub fn install_lock_source(
	resolved: &aghub_git::ResolvedRemoteSource,
	ref_name: Option<String>,
) -> skill::InstallLockSource {
	skill::InstallLockSource {
		source: resolved.lock_source(),
		source_type: resolved.source_type.as_str().to_string(),
		source_url: resolved.source_url.clone(),
		ref_name,
	}
}

/// Errors from the resolve/list/select/fetch stage of a remote install.
#[derive(Debug)]
pub enum InstallFetchError {
	Repo(SkillRepoError),
	SkillsNotFound { missing: String, available: String },
	NoSkillsFound,
	InvalidPath,
}

/// Map shared-policy selections to `(catalog_name, SkillPath)` pairs.
fn select_catalog_paths(
	catalog: &[crate::CatalogSkill],
	requested: &[String],
	install_all: bool,
) -> Result<Vec<(String, skill::SkillPath)>, InstallFetchError> {
	let selected =
		skill::select_repo_skills(catalog, requested, install_all, |skill| {
			skill.name.as_str()
		})
		.map_err(|error| match error {
			skill::RepoSkillSelectionError::NoSkillsFound => {
				InstallFetchError::NoSkillsFound
			}
			skill::RepoSkillSelectionError::SkillsNotFound {
				missing,
				available,
			} => InstallFetchError::SkillsNotFound { missing, available },
		})?;

	selected
		.into_iter()
		.map(|skill| {
			let path = skill::SkillPath::parse(&skill.skill_path)
				.map_err(|_| InstallFetchError::InvalidPath)?;
			Ok((skill.name.clone(), path))
		})
		.collect()
}

/// Owned input of [`fetch_remote_install`], so a caller can move it into
/// `spawn_blocking`.
pub struct RemoteInstallRequest {
	/// The RAW request source: the `recorded_refs` key and fetch coordinate.
	pub source: String,
	pub resolved: aghub_git::ResolvedRemoteSource,
	pub scope: WriteScope,
	pub token: Option<String>,
	pub skills: Vec<String>,
	pub install_all: bool,
}

/// A commit-pinned Fetched Source bound to the scope its ref was decided in.
pub struct PreparedInstall {
	fetched: FetchedSource,
	lock_source: skill::InstallLockSource,
	scope: WriteScope,
	expected_ref: Option<Option<String>>,
	/// (row label, npx-form lock skill path)
	items: Vec<(String, String)>,
}

/// Blocking and UNLOCKED: resolve, list, select and fetch a remote install.
///
/// The ref fetched is the one the lock will record: an existing cohort's ref
/// (a recorded `None` = the default branch), else the default branch
/// (`sources::import_ref`). It is NAMED first and then exactly that ref is
/// resolved: resolving HEAD and asking its name in a second call races a
/// default branch switch (bytes from one branch, lock naming another). One
/// resolve, one list, one fetch.
pub fn fetch_remote_install(
	repo: &SkillRepository,
	req: RemoteInstallRequest,
) -> Result<PreparedInstall, InstallFetchError> {
	let RemoteInstallRequest {
		source,
		resolved,
		scope,
		token,
		skills,
		install_all,
	} = req;
	let recorded = crate::sources::recorded_refs(&scope, &source);
	let fetch_ref = crate::sources::import_ref(None, &recorded, || {
		repo.default_branch(
			&SourceRef {
				source: source.clone(),
				ref_: None,
			},
			token.as_deref(),
		)
	});
	let claim = repo
		.resolve_pinned(
			&SourceRef {
				source: source.clone(),
				ref_: fetch_ref.clone(),
			},
			token.as_deref(),
		)
		.map_err(InstallFetchError::Repo)?;
	let catalog = repo.list_pinned(&claim).map_err(InstallFetchError::Repo)?;
	let selected = select_catalog_paths(&catalog.skills, &skills, install_all)?;
	let paths: Vec<skill::SkillPath> =
		selected.iter().map(|(_, p)| p.clone()).collect();
	let fetched = repo
		.fetch_pinned(&claim, crate::FetchSelection::Skills(&paths))
		.map_err(InstallFetchError::Repo)?;

	let lock_source = install_lock_source(&resolved, fetch_ref);
	let expected_ref = Some(lock_source.ref_name.clone());
	let items = selected
		.into_iter()
		.map(|(name, path)| (name, skill::lock_skill_file_path(path.as_str())))
		.collect();
	Ok(PreparedInstall {
		fetched: FetchedSource::from_repo(fetched),
		lock_source,
		scope,
		expected_ref,
		items,
	})
}

/// Pre-fetch half of `/skills/git/install`, decided against a scanned session.
pub struct ClaimInstallRequest<'a> {
	/// `session.url()`: the `recorded_refs` key.
	pub url: &'a str,
	pub resolved: &'a aghub_git::ResolvedRemoteSource,
	pub scope: WriteScope,
	pub requested_branch: Option<&'a str>,
	/// The branch the scan fetched; the session's bytes come from it.
	pub current_branch: &'a str,
	pub skill_paths: &'a [String],
}

#[derive(Debug, PartialEq, Eq)]
pub enum ClaimInstallRefusal {
	/// The scope's cohort ref differs from the scanned branch.
	CohortMismatch {
		cohort: String,
	},
	InvalidSkillPath,
}

/// A validated plan; becomes a [`PreparedInstall`] once the bytes are fetched.
pub struct ClaimInstallPlan {
	lock_source: skill::InstallLockSource,
	scope: WriteScope,
	expected_ref: Option<Option<String>>,
	paths: Vec<skill::SkillPath>,
	items: Vec<(String, String)>,
}

/// Order is the route's precedence: ref decision, cohort mismatch, then path
/// validation (no I/O before the paths are known to be in-tree).
pub fn plan_claim_install(
	req: ClaimInstallRequest<'_>,
) -> Result<ClaimInstallPlan, ClaimInstallRefusal> {
	let ref_name = crate::sources::import_ref(
		req.requested_branch,
		&crate::sources::recorded_refs(&req.scope, req.url),
		// The scan already paid for the default branch's name.
		|| Some(req.current_branch.to_string()).filter(|b| !b.is_empty()),
	);
	// The session holds the scanned branch's commit; recording another ref would
	// pin these bytes to a branch they did not come from.
	if let Some(cohort) =
		ref_name.as_deref().filter(|r| *r != req.current_branch)
	{
		return Err(ClaimInstallRefusal::CohortMismatch {
			cohort: cohort.to_string(),
		});
	}
	let paths: Vec<skill::SkillPath> = req
		.skill_paths
		.iter()
		.map(|p| skill::SkillPath::parse(p))
		.collect::<Result<_, _>>()
		.map_err(|_| ClaimInstallRefusal::InvalidSkillPath)?;
	let lock_source = install_lock_source(req.resolved, ref_name);
	// Only a ref taken from the scope's cohort is re-checked under the lock.
	let expected_ref = req
		.requested_branch
		.is_none()
		.then(|| lock_source.ref_name.clone());
	let items = req
		.skill_paths
		.iter()
		.zip(&paths)
		.map(|(raw, p)| (raw.clone(), skill::lock_skill_file_path(p.as_str())))
		.collect();
	Ok(ClaimInstallPlan {
		lock_source,
		scope: req.scope,
		expected_ref,
		paths,
		items,
	})
}

impl ClaimInstallPlan {
	pub fn paths(&self) -> &[skill::SkillPath] {
		&self.paths
	}

	pub fn into_prepared(self, fetched: crate::FetchedRepo) -> PreparedInstall {
		PreparedInstall {
			fetched: FetchedSource::from_repo(fetched),
			lock_source: self.lock_source,
			scope: self.scope,
			expected_ref: self.expected_ref,
			items: self.items,
		}
	}
}

impl PreparedInstall {
	/// Install every item, taking the mutation lock PER SKILL inside
	/// `install_fetched_source`. Callers run it on the mutation pool. A zip
	/// shorter than the agent list truncates rows, as before.
	pub fn install(
		self,
		agents: &InstallAgents,
		render: impl Fn(InstallMutationError) -> String,
	) -> Vec<InstallRow> {
		let mut rows = Vec::new();
		for (name, lock_skill_path) in &self.items {
			let installed = install_fetched_source(
				&self.fetched,
				FetchedInstallRequest {
					source: &self.lock_source,
					lock_skill_path,
					expected_name: None,
					scope: self.scope.clone(),
					target_agents: agents.types(),
					expected_ref: self
						.expected_ref
						.as_ref()
						.map(|r| r.as_deref()),
				},
			)
			.map_err(&render);
			match installed {
				Ok(report) => {
					for (agent, agent_result) in
						agents.ids().iter().zip(report.agent_results)
					{
						let success = agent_result.error.is_none();
						rows.push(InstallRow {
							name: if success {
								report.name.clone()
							} else {
								name.clone()
							},
							agent: agent.clone(),
							success,
							error: agent_result.error,
						});
					}
				}
				Err(message) => {
					for agent in agents.ids() {
						rows.push(InstallRow {
							name: name.clone(),
							agent: agent.clone(),
							success: false,
							error: Some(message.clone()),
						});
					}
				}
			}
		}
		rows
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn s(v: &[&str]) -> Vec<String> {
		v.iter().map(|x| x.to_string()).collect()
	}

	#[test]
	fn parse_keeps_request_order_and_duplicates() {
		let a = InstallAgents::parse(&s(&["opencode", "claude", "opencode"]))
			.unwrap();
		assert_eq!(a.ids(), &s(&["opencode", "claude", "opencode"])[..]);
		assert_eq!(
			a.types(),
			&[AgentType::OpenCode, AgentType::Claude, AgentType::OpenCode]
		);
	}

	#[test]
	fn unknown_id_rejects_every_requested_agent_with_its_own_message() {
		let r = InstallAgents::parse(&s(&["claude", "nope"])).unwrap_err();
		assert_eq!(
			r.per_agent,
			vec![
				(
					"claude".to_string(),
					"Another requested agent is invalid; nothing was written"
						.to_string()
				),
				("nope".to_string(), "Unknown agent 'nope'".to_string()),
			]
		);
	}

	#[test]
	fn rejection_rows_are_label_major() {
		let r = InstallAgents::parse(&s(&["claude", "nope"])).unwrap_err();
		let one = r.rows(&[String::new()]);
		assert_eq!(one.len(), 2);
		assert!(one.iter().all(|x| x.name.is_empty() && !x.success));
		assert_eq!(one[0].agent, "claude");
		assert_eq!(one[1].agent, "nope");

		let rows = r.rows(&s(&["a", "b"]));
		let got: Vec<_> = rows
			.iter()
			.map(|x| (x.name.as_str(), x.agent.as_str()))
			.collect();
		assert_eq!(
			got,
			vec![
				("a", "claude"),
				("a", "nope"),
				("b", "claude"),
				("b", "nope")
			]
		);
		assert!(rows[1].error.as_deref().unwrap().contains("Unknown agent"));
	}

	#[test]
	fn lock_source_carries_resolved_coordinates_and_ref() {
		let resolved = aghub_git::resolve_remote_source("owner/repo").unwrap();
		let ls = install_lock_source(&resolved, Some("dev".to_string()));
		assert_eq!(ls.source, resolved.lock_source());
		assert_eq!(ls.source_type, resolved.source_type.as_str());
		assert_eq!(ls.source_url, resolved.source_url);
		assert_eq!(ls.ref_name.as_deref(), Some("dev"));
		assert_eq!(install_lock_source(&resolved, None).ref_name, None);
	}

	fn scope_with_cohort(
		root: &std::path::Path,
		ref_name: Option<&str>,
	) -> WriteScope {
		let mut lock = skill::LocalSkillLockFile::new();
		lock.skills.insert(
			"other".to_string(),
			skill::LocalSkillLockEntry {
				extra: Default::default(),
				source: "o/r".to_string(),
				source_url: Some("https://github.com/o/r".to_string()),
				source_type: "github".to_string(),
				ref_name: ref_name.map(str::to_string),
				skill_path: Some("other/SKILL.md".to_string()),
				computed_hash: "h".to_string(),
				ref_commit: None,
			},
		);
		skill::write_local_lock(&lock, Some(root)).unwrap();
		WriteScope::project(root)
	}

	fn claim<'a>(
		resolved: &'a aghub_git::ResolvedRemoteSource,
		scope: WriteScope,
		requested_branch: Option<&'a str>,
		current_branch: &'a str,
		skill_paths: &'a [String],
	) -> Result<ClaimInstallPlan, ClaimInstallRefusal> {
		plan_claim_install(ClaimInstallRequest {
			url: "https://github.com/o/r",
			resolved,
			scope,
			requested_branch,
			current_branch,
			skill_paths,
		})
	}

	fn resolved() -> aghub_git::ResolvedRemoteSource {
		aghub_git::resolve_remote_source("https://github.com/o/r").unwrap()
	}

	#[test]
	fn claim_plan_refuses_a_cohort_the_scan_did_not_fetch() {
		let dir = tempfile::tempdir().unwrap();
		let scope = scope_with_cohort(dir.path(), Some("main"));
		let err = claim(&resolved(), scope, None, "develop", &s(&["a"]))
			.err()
			.unwrap();
		assert_eq!(
			err,
			ClaimInstallRefusal::CohortMismatch {
				cohort: "main".to_string()
			}
		);
	}

	#[test]
	fn claim_plan_keeps_a_none_cohort() {
		let dir = tempfile::tempdir().unwrap();
		let scope = scope_with_cohort(dir.path(), None);
		let plan = claim(&resolved(), scope, None, "develop", &s(&["a"]))
			.ok()
			.unwrap();
		assert_eq!(plan.lock_source.ref_name, None);
		assert_eq!(plan.expected_ref, Some(None));
	}

	#[test]
	fn claim_plan_reports_cohort_mismatch_before_an_invalid_path() {
		let dir = tempfile::tempdir().unwrap();
		let scope = scope_with_cohort(dir.path(), Some("main"));
		let err = claim(&resolved(), scope, None, "develop", &s(&["/abs"]))
			.err()
			.unwrap();
		assert!(matches!(err, ClaimInstallRefusal::CohortMismatch { .. }));
		let dir = tempfile::tempdir().unwrap();
		let scope = WriteScope::project(dir.path());
		let err = claim(&resolved(), scope, None, "develop", &s(&["/abs"]))
			.err()
			.unwrap();
		assert_eq!(err, ClaimInstallRefusal::InvalidSkillPath);
	}

	#[test]
	fn explicit_branch_sets_no_expected_ref() {
		let dir = tempfile::tempdir().unwrap();
		let scope = WriteScope::project(dir.path());
		let paths = s(&["skills/a", "b"]);
		let plan =
			claim(&resolved(), scope, Some("develop"), "develop", &paths)
				.ok()
				.unwrap();
		assert_eq!(plan.expected_ref, None);
		assert_eq!(plan.lock_source.ref_name.as_deref(), Some("develop"));
		assert_eq!(plan.paths().len(), 2);
		// Labels stay the raw requested paths; lock paths are npx-form.
		assert_eq!(plan.items[0].0, "skills/a");
		assert_eq!(plan.items[0].1, "skills/a/SKILL.md");
	}

	#[test]
	fn default_closure_not_called_when_cohort_exists() {
		// The cohort ref wins over the scan's branch: the mismatch names the
		// cohort, never the default branch.
		let dir = tempfile::tempdir().unwrap();
		let scope = scope_with_cohort(dir.path(), Some("main"));
		let err = claim(&resolved(), scope, None, "develop", &s(&["a"]))
			.err()
			.unwrap();
		assert_eq!(
			err,
			ClaimInstallRefusal::CohortMismatch {
				cohort: "main".to_string()
			}
		);
		// With no cohort at all, the scan's branch is the default.
		let dir = tempfile::tempdir().unwrap();
		let scope = WriteScope::project(dir.path());
		let plan = claim(&resolved(), scope, None, "develop", &s(&["a"]))
			.ok()
			.unwrap();
		assert_eq!(plan.lock_source.ref_name.as_deref(), Some("develop"));
	}
}
