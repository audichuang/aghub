use aghub_core::models::Skill;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::dto::common::ConfigSource;

#[derive(Debug, Deserialize, TS)]
#[ts(export)]
pub struct CreateSkillRequest {
	pub name: String,
	pub description: Option<String>,
	pub author: Option<String>,
	pub version: Option<String>,
	pub content: Option<String>,
	pub tools: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, TS)]
#[ts(export)]
pub struct ImportSkillRequest {
	pub path: String,
}

impl From<CreateSkillRequest> for Skill {
	fn from(req: CreateSkillRequest) -> Self {
		Skill {
			name: req.name,
			enabled: true,
			description: req.description,
			author: req.author,
			version: req.version,
			content: req.content,
			tools: req.tools.unwrap_or_default(),
			source_path: None,
			canonical_path: None,
			config_source: None,
		}
	}
}

#[derive(Debug, Deserialize, TS)]
#[ts(export)]
pub struct UpdateSkillRequest {
	pub name: Option<String>,
	pub description: Option<String>,
	pub author: Option<String>,
	pub version: Option<String>,
	pub content: Option<String>,
	pub tools: Option<Vec<String>>,
	pub enabled: Option<bool>,
}

impl UpdateSkillRequest {
	pub fn apply_to(self, existing: Skill) -> Skill {
		Skill {
			name: self.name.unwrap_or(existing.name),
			enabled: self.enabled.unwrap_or(existing.enabled),
			description: self.description.or(existing.description),
			author: self.author.or(existing.author),
			version: self.version.or(existing.version),
			content: self.content.or(existing.content),
			tools: self.tools.unwrap_or(existing.tools),
			source_path: existing.source_path,
			canonical_path: existing.canonical_path,
			config_source: existing.config_source,
		}
	}
}

#[derive(Debug, Serialize, TS)]
#[ts(export)]
pub struct SkillResponse {
	pub name: String,
	pub enabled: bool,
	pub source_path: Option<String>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub canonical_path: Option<String>,
	pub description: Option<String>,
	pub author: Option<String>,
	pub version: Option<String>,
	pub tools: Vec<String>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub source: Option<ConfigSource>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub agent: Option<String>,
	/// Advisory: the target agent reads the `.agents` master directly
	/// (NativeReader), so a universal install writes only the master with no
	/// per-agent link. Always serialized (default false) so the wire matches the
	/// generated `native_reader: boolean` ts-rs type — no DTO drift.
	pub native_reader: bool,
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum SkillTreeNodeKind {
	File,
	Directory,
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
pub struct SkillTreeNodeResponse {
	pub name: String,
	pub path: String,
	pub kind: SkillTreeNodeKind,
	pub children: Vec<SkillTreeNodeResponse>,
}

impl From<Skill> for SkillResponse {
	fn from(s: Skill) -> Self {
		SkillResponse::from(&s)
	}
}

impl SkillResponse {
	pub fn from_agent_skill(skill: Skill, agent_id: &str) -> Self {
		SkillResponse::from(
			&aghub_core::dto::SkillView::from(&skill).with_agent(agent_id),
		)
	}
}

impl From<&Skill> for SkillResponse {
	fn from(s: &Skill) -> Self {
		// Delegate the field mapping to the one core SkillView seam; this
		// wrapper only maps ConfigSource and the native_reader advisory.
		SkillResponse::from(&aghub_core::dto::SkillView::from(s))
	}
}

/// Thin ts-rs wrapper over the core [`SkillView`]: the field list lives once in
/// `aghub_core::dto`, so this only maps `ConfigSource` and carries the
/// `native_reader` advisory across.
impl From<&aghub_core::dto::SkillView> for SkillResponse {
	fn from(v: &aghub_core::dto::SkillView) -> Self {
		SkillResponse {
			name: v.name.clone(),
			enabled: v.enabled,
			source_path: v.source_path.clone(),
			canonical_path: v.canonical_path.clone(),
			description: v.description.clone(),
			author: v.author.clone(),
			version: v.version.clone(),
			tools: v.tools.clone(),
			source: v.source.map(Into::into),
			agent: v.agent.clone(),
			native_reader: v.native_reader,
		}
	}
}

/// Thin ts-rs wrapper over the core [`RemovalView`]: the 7 shared
/// removal-outcome fields copy across. `error`/`validation_errors` and the
/// lock-prune fields are api-only (core does not own them) and default to None.
impl From<&aghub_core::dto::RemovalView> for DeleteSkillByPathResponse {
	fn from(v: &aghub_core::dto::RemovalView) -> Self {
		DeleteSkillByPathResponse {
			success: v.success,
			dry_run: v.dry_run,
			executed: v.executed,
			needs_confirm: v.needs_confirm,
			paths: v.paths.clone(),
			skipped: v.skipped.clone(),
			deleted_path: v.deleted_path.clone(),
			pruned_lock_entries: None,
			prune_error: None,
			error: None,
			validation_errors: None,
		}
	}
}

#[derive(Debug, Deserialize, TS)]
#[ts(export)]
pub struct InstallSkillRequest {
	pub source: String,
	pub agents: Vec<String>,
	pub skills: Vec<String>,
	pub scope: String,
	pub project_path: Option<String>,
	pub install_all: Option<bool>,
}

#[derive(Debug, Serialize, TS)]
#[ts(export)]
pub struct InstallSkillResponse {
	pub success: bool,
	/// Per-agent install outcome rows (Decision 10: link failures are per-agent
	/// soft-fails, so an aggregate boolean cannot say WHICH agent failed).
	/// Reuses the git-install row shape for parity with `/skills/git/install`.
	pub agents: Vec<GitInstallResultEntry>,
}

/// Response for a single global skill lock entry
#[derive(Debug, Serialize, TS)]
#[ts(export)]
pub struct SkillLockEntryResponse {
	pub name: String,
	pub source: String,
	#[serde(rename = "sourceType")]
	pub source_type: String,
	#[serde(rename = "sourceUrl")]
	pub source_url: String,
	#[serde(rename = "skillPath", skip_serializing_if = "Option::is_none")]
	pub skill_path: Option<String>,
	#[serde(rename = "skillFolderHash")]
	pub skill_folder_hash: String,
	#[serde(rename = "contentHash", skip_serializing_if = "Option::is_none")]
	#[ts(optional)]
	pub content_hash: Option<String>,
	#[serde(rename = "installedAt")]
	pub installed_at: String,
	#[serde(rename = "updatedAt")]
	pub updated_at: String,
	#[serde(rename = "pluginName", skip_serializing_if = "Option::is_none")]
	pub plugin_name: Option<String>,
}

/// Response for the global skill lock file
#[derive(Debug, Serialize, TS)]
#[ts(export)]
pub struct GlobalSkillLockResponse {
	pub version: u32,
	pub skills: Vec<SkillLockEntryResponse>,
	#[serde(
		rename = "lastSelectedAgents",
		skip_serializing_if = "Option::is_none"
	)]
	pub last_selected_agents: Option<Vec<String>>,
}

/// Response for a single project skill lock entry
#[derive(Debug, Serialize, TS)]
#[ts(export)]
pub struct LocalSkillLockEntryResponse {
	pub name: String,
	pub source: String,
	#[serde(rename = "sourceType")]
	pub source_type: String,
	#[serde(rename = "computedHash")]
	pub computed_hash: String,
}

/// Response for the project skill lock file
#[derive(Debug, Serialize, TS)]
#[ts(export)]
pub struct ProjectSkillLockResponse {
	pub version: u32,
	pub skills: Vec<LocalSkillLockEntryResponse>,
}

#[derive(Debug, Deserialize, TS)]
#[ts(export)]
pub struct DeleteSkillByPathRequest {
	pub source_path: String,
	pub agents: Vec<String>,
	pub scope: String,
	pub project_root: Option<String>,
	/// Reserved for future copy-layout multi-agent removal; currently the route
	/// removes the single targeted path.
	#[serde(default)]
	#[ts(optional)]
	pub all_agents: Option<bool>,
	/// Must be `true` to actually delete. Absent/false → dry-run (lists paths,
	/// removes nothing).
	#[serde(default)]
	#[ts(optional)]
	pub confirm: Option<bool>,
}

#[derive(Debug, Serialize, TS)]
#[ts(export)]
pub struct ValidationError {
	pub agent: String,
	pub reason: String,
}

#[derive(Debug, Deserialize, TS)]
#[ts(export)]
pub struct GitScanRequest {
	pub url: String,
	pub credential_id: Option<String>,
	pub branch: Option<String>,
	/// When re-scanning (e.g. branch switch), pass the existing
	/// session ID so the old clone is replaced.
	pub session_id: Option<String>,
}

#[derive(Debug, Serialize, TS)]
#[ts(export)]
pub struct GitScanSkillEntry {
	pub name: String,
	pub description: String,
	pub author: Option<String>,
	pub version: Option<String>,
	pub path: String,
}

#[derive(Debug, Serialize, TS)]
#[ts(export)]
pub struct GitScanResponse {
	pub session_id: String,
	pub skills: Vec<GitScanSkillEntry>,
	pub branches: Vec<String>,
	pub current_branch: String,
}

#[derive(Debug, Deserialize, TS)]
#[ts(export)]
pub struct GitInstallRequest {
	pub session_id: String,
	pub skill_paths: Vec<String>,
	pub agents: Vec<String>,
	pub scope: String,
	pub project_root: Option<String>,
}

/// Request to sync (update in-place) an existing skill from a git session.
#[derive(Debug, Deserialize, TS)]
#[ts(export)]
pub struct GitSyncRequest {
	pub session_id: String,
	/// Current installed skill name; used to update the matching lock entry.
	pub name: String,
	/// Lock scope for the installed skill (`global` or `project`).
	pub scope: String,
	pub project_root: Option<String>,
	/// Relative path of the skill within the cloned repo (from scan result).
	pub skill_path: String,
	/// Legacy client hint. The server derives replacement targets by `name`.
	pub source_paths: Vec<String>,
}

/// Response for a git sync operation.
#[derive(Debug, Serialize, TS)]
#[ts(export)]
pub struct GitSyncResponse {
	pub success: bool,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub name: Option<String>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub updated_hash: Option<String>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub error: Option<String>,
}

#[derive(Debug, Serialize, TS)]
#[ts(export)]
pub struct GitInstallResultEntry {
	pub name: String,
	pub agent: String,
	pub success: bool,
	pub error: Option<String>,
}

#[derive(Debug, Serialize, TS)]
#[ts(export)]
pub struct GitInstallResponse {
	pub results: Vec<GitInstallResultEntry>,
}

#[derive(Debug, Default, Serialize, TS)]
#[ts(export)]
pub struct DeleteSkillByPathResponse {
	pub success: bool,
	/// True when this was a dry-run (nothing was deleted).
	pub dry_run: bool,
	/// True when deletion actually ran.
	pub executed: bool,
	/// True when this removal is destructive enough to require confirm=true.
	pub needs_confirm: bool,
	/// The exact paths that were removed (or, in a dry-run, would be removed).
	pub paths: Vec<String>,
	/// Paths intentionally NOT removed (outside the allow-listed skills roots).
	pub skipped: Vec<String>,
	/// Absolute path actually removed (only when `executed`); `null` otherwise.
	/// Always serialized (no skip) so the runtime matches both the shared
	/// `RemovalView` wire shape and the generated `deleted_path: string | null`.
	pub deleted_path: Option<String>,
	/// Lock keys (skill names — never raw paths) dropped by the post-delete
	/// disk-reconciled prune. `Some([])` means the prune ran and found nothing
	/// orphaned; `None` means no prune was attempted (dry-run/unconfirmed). On a
	/// prune failure this carries the keys dropped BEFORE the failure (partial).
	#[serde(skip_serializing_if = "Option::is_none")]
	pub pruned_lock_entries: Option<Vec<String>>,
	/// Set when the post-delete lock prune failed. The deletion still happened
	/// (prune is non-fatal); this reports why the lock could not be reconciled.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub prune_error: Option<String>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub error: Option<String>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub validation_errors: Option<Vec<ValidationError>>,
}

#[derive(Debug, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct PruneLockRequest {
	/// `"global"` or `"project"`.
	pub scope: String,
	pub project_root: Option<String>,
	/// Must be `true` to write; absent/false → dry-run (reports would-prune).
	#[serde(default)]
	#[ts(optional)]
	pub confirm: Option<bool>,
}

#[derive(Debug, Serialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct PruneLockResponse {
	pub pruned: Vec<String>,
	pub dry_run: bool,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub error: Option<String>,
}

#[derive(Debug, TS, rocket::FromForm)]
#[ts(export)]
pub struct SkillContentQuery {
	pub path: String,
	pub scope: Option<String>,
	pub project_root: Option<String>,
}

#[derive(Debug, TS, rocket::FromForm)]
#[ts(export)]
pub struct SkillTreeQuery {
	pub path: String,
	pub scope: Option<String>,
	pub project_root: Option<String>,
}

#[derive(Debug, TS, rocket::FromForm)]
#[ts(export)]
pub struct ProjectLockQuery {
	pub project_path: Option<String>,
}

/// Per-skill update status surfaced by `GET /skills/check-updates`.
///
/// Tagged by `status` (camelCase): `upToDate`, `updateAvailable`,
/// `renamed`, `uncheckable`. The `reason` on `uncheckable` is already
/// redacted of any URL userinfo at the orchestration boundary.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum SkillUpdateStatusResponse {
	UpToDate,
	UpdateAvailable {
		current: String,
		available: String,
	},
	Renamed {
		#[serde(rename = "newName")]
		new_name: String,
	},
	Uncheckable {
		reason: String,
	},
}

impl From<aghub_core::skills::update::SkillUpdateStatus>
	for SkillUpdateStatusResponse
{
	fn from(s: aghub_core::skills::update::SkillUpdateStatus) -> Self {
		use aghub_core::skills::update::{
			SkillUpdateStatus, UncheckableReason,
		};
		match s {
			SkillUpdateStatus::UpToDate => SkillUpdateStatusResponse::UpToDate,
			SkillUpdateStatus::UpdateAvailable { current, available } => {
				SkillUpdateStatusResponse::UpdateAvailable {
					current,
					available,
				}
			}
			SkillUpdateStatus::Renamed { new_name } => {
				SkillUpdateStatusResponse::Renamed { new_name }
			}
			SkillUpdateStatus::Uncheckable { reason } => {
				let reason = match reason {
					UncheckableReason::Auth => "auth",
					UncheckableReason::Network => "network",
					UncheckableReason::Local => "local",
					UncheckableReason::Ssh => "ssh",
					UncheckableReason::UnsupportedScheme => "unsupportedScheme",
					UncheckableReason::NoPath => "noPath",
					UncheckableReason::Timeout => "timeout",
				};
				SkillUpdateStatusResponse::Uncheckable {
					reason: reason.to_string(),
				}
			}
		}
	}
}

/// One skill's name plus its flattened update status.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct SkillUpdateResponse {
	pub name: String,
	pub scope: String,
	#[serde(flatten)]
	pub status: SkillUpdateStatusResponse,
}

/// Request to re-fetch and overwrite an installed skill from its lock source.
#[derive(Debug, Clone, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ApplySkillUpdateRequest {
	pub name: String,
	pub scope: String,
	pub project_root: Option<String>,
	pub confirm: Option<bool>,
}

/// Response from `POST /skills/apply-update`.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ApplySkillUpdateResponse {
	pub success: bool,
	pub name: String,
	pub scope: String,
	pub updated_hash: Option<String>,
	pub paths: Vec<String>,
	pub error: Option<String>,
	/// Stable machine-readable error code (e.g. `SKILL_RENAMED_IN_SOURCE`).
	/// Lets consumers distinguish a rename from a generic failure without
	/// parsing the human-readable `error` string.
	#[serde(skip_serializing_if = "Option::is_none")]
	#[ts(optional)]
	pub code: Option<String>,
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn delete_response_from_removal_view_matches_outcome() {
		// The api DeleteSkillByPathResponse must be a thin wrapper over the core
		// RemovalView: the 7 shared fields copy across, and the serde json keeps
		// the snake_case keys the desktop consumes (dry_run/needs_confirm).
		let view = aghub_core::dto::RemovalView {
			success: true,
			dry_run: false,
			executed: true,
			needs_confirm: true,
			paths: vec!["/a/foo".to_string()],
			skipped: vec!["/a/bar".to_string()],
			deleted_path: Some("/a/foo".to_string()),
		};
		let resp = DeleteSkillByPathResponse::from(&view);
		assert!(resp.success);
		assert!(!resp.dry_run);
		assert!(resp.executed);
		assert!(resp.needs_confirm);
		assert_eq!(resp.paths, vec!["/a/foo".to_string()]);
		assert_eq!(resp.skipped, vec!["/a/bar".to_string()]);
		assert_eq!(resp.deleted_path.as_deref(), Some("/a/foo"));
		// API-only error fields are not owned by core; default to None.
		assert!(resp.error.is_none());
		assert!(resp.validation_errors.is_none());
		assert!(resp.pruned_lock_entries.is_none());
		assert!(resp.prune_error.is_none());

		let json = serde_json::to_value(&resp).unwrap();
		assert_eq!(json["dry_run"], serde_json::json!(false));
		assert_eq!(json["needs_confirm"], serde_json::json!(true));
		assert!(json.get("dryRun").is_none(), "must stay snake_case");
		assert!(json.get("needsConfirm").is_none(), "must stay snake_case");
	}

	#[test]
	fn removal_view_and_response_serialize_one_deleted_path_shape() {
		// One wire shape for `deleted_path`: serializing the core RemovalView
		// directly (CLI) must produce the same `deleted_path` key as the API
		// DeleteSkillByPathResponse wrapping it — for BOTH Some and None. They
		// diverged when the API skipped `deleted_path` on None while RemovalView
		// (and the generated `deleted_path: string | null`) always emitted it.
		for deleted_path in [None, Some("/a/foo".to_string())] {
			let view = aghub_core::dto::RemovalView {
				success: true,
				dry_run: deleted_path.is_none(),
				executed: deleted_path.is_some(),
				needs_confirm: false,
				paths: vec![],
				skipped: vec![],
				deleted_path: deleted_path.clone(),
			};
			let resp = DeleteSkillByPathResponse::from(&view);
			let view_json = serde_json::to_value(&view).unwrap();
			let resp_json = serde_json::to_value(&resp).unwrap();
			// The key must be present in both, even when None (= null), to
			// match the ts-rs `deleted_path: string | null` contract.
			assert!(
				view_json.get("deleted_path").is_some(),
				"RemovalView must always emit deleted_path"
			);
			assert!(
				resp_json.get("deleted_path").is_some(),
				"response must always emit deleted_path (no skip on None)"
			);
			assert_eq!(
				view_json["deleted_path"], resp_json["deleted_path"],
				"deleted_path must serialize identically (None -> null)"
			);
		}
	}

	#[test]
	fn skill_response_serializes_native_reader_false() {
		// native_reader is always serialized so the wire matches the generated
		// `native_reader: boolean` ts-rs type (default false, no drift).
		let skill = aghub_core::models::Skill::new("foo");
		let resp = SkillResponse::from(&skill);
		assert!(!resp.native_reader);
		let json = serde_json::to_value(&resp).unwrap();
		assert_eq!(
			json["native_reader"],
			serde_json::json!(false),
			"native_reader must be present (= false)"
		);
	}

	#[test]
	fn skill_view_and_response_serialize_the_same_wire_shape() {
		// The candidate's promise is one wire shape: serializing the core
		// SkillView directly (the CLI path) must produce byte-identical JSON to
		// the API SkillResponse wrapping it. They diverged when SkillView
		// emitted explicit nulls for canonical_path/source/agent while
		// SkillResponse skipped them when None.
		let skill = aghub_core::models::Skill::new("foo");
		let view = aghub_core::dto::SkillView::from(&skill);
		let resp = SkillResponse::from(&view);
		assert_eq!(
			serde_json::to_value(&view).unwrap(),
			serde_json::to_value(&resp).unwrap(),
			"SkillView and SkillResponse must be one wire shape"
		);

		// And with the optional fields populated, they still match.
		let mut skill = aghub_core::models::Skill::new("bar");
		skill.canonical_path = Some(".agents/skills/bar".to_string());
		skill.config_source = Some(aghub_core::models::ConfigSource::Global);
		let view =
			aghub_core::dto::SkillView::from(&skill).with_agent("claude");
		let resp = SkillResponse::from(&view);
		assert_eq!(
			serde_json::to_value(&view).unwrap(),
			serde_json::to_value(&resp).unwrap(),
			"populated SkillView and SkillResponse must be one wire shape"
		);
	}

	#[test]
	fn skill_response_from_view_carries_native_reader() {
		// Building from a core SkillView with the advisory set surfaces the
		// `native_reader` key (= true).
		let skill = aghub_core::models::Skill::new("foo");
		let view =
			aghub_core::dto::SkillView::from(&skill).with_native_reader(true);
		let resp = SkillResponse::from(&view);
		assert!(resp.native_reader);
		let json = serde_json::to_value(&resp).unwrap();
		assert_eq!(json["native_reader"], serde_json::json!(true));
	}

	#[test]
	fn update_available_serializes_with_status_tag_and_fields() {
		let resp = SkillUpdateResponse {
			name: "my-skill".to_string(),
			scope: "global".to_string(),
			status: SkillUpdateStatusResponse::UpdateAvailable {
				current: "aaa".to_string(),
				available: "bbb".to_string(),
			},
		};
		let json = serde_json::to_value(&resp).unwrap();
		assert_eq!(json["status"], "updateAvailable");
		assert_eq!(json["current"], "aaa");
		assert_eq!(json["available"], "bbb");
		assert_eq!(json["name"], "my-skill");
		assert_eq!(json["scope"], "global");
	}

	#[test]
	fn uncheckable_serializes_reason_string() {
		let resp = SkillUpdateResponse {
			name: "s".to_string(),
			scope: "project".to_string(),
			status: SkillUpdateStatusResponse::Uncheckable {
				reason: "auth".to_string(),
			},
		};
		let json = serde_json::to_value(&resp).unwrap();
		assert_eq!(json["status"], "uncheckable");
		assert_eq!(json["reason"], "auth");
	}

	#[test]
	fn renamed_serializes_new_name() {
		let resp = SkillUpdateResponse {
			name: "old".to_string(),
			scope: "global".to_string(),
			status: SkillUpdateStatusResponse::Renamed {
				new_name: "new".to_string(),
			},
		};
		let json = serde_json::to_value(&resp).unwrap();
		assert_eq!(json["status"], "renamed");
		assert_eq!(json["newName"], "new");
	}

	#[test]
	fn up_to_date_serializes_status_only() {
		let resp = SkillUpdateResponse {
			name: "s".to_string(),
			scope: "global".to_string(),
			status: SkillUpdateStatusResponse::UpToDate,
		};
		let json = serde_json::to_value(&resp).unwrap();
		assert_eq!(json["status"], "upToDate");
	}

	#[test]
	fn prune_lock_request_response_use_camel_case() {
		let request: PruneLockRequest =
			serde_json::from_value(serde_json::json!({
				"scope": "project",
				"projectRoot": "/tmp/project",
				"confirm": true,
			}))
			.unwrap();
		assert_eq!(request.project_root.as_deref(), Some("/tmp/project"));

		let response = PruneLockResponse {
			pruned: vec!["s".to_string()],
			dry_run: true,
			error: None,
		};
		let json = serde_json::to_value(response).unwrap();
		assert_eq!(json["dryRun"], true);
		assert!(json.get("dry_run").is_none());
	}

	#[test]
	fn git_install_request_ignores_legacy_universal_field() {
		// No deny_unknown_fields => a legacy client sending "universal" still
		// deserializes (field dropped), and the struct no longer has it.
		let json = r#"{
			"session_id":"s",
			"skill_paths":[],
			"agents":[],
			"scope":"global",
			"project_root":null,
			"universal":true
		}"#;
		let req: GitInstallRequest =
			serde_json::from_str(json).expect("parses, ignoring universal");
		let req = GitInstallRequest {
			session_id: req.session_id,
			skill_paths: req.skill_paths,
			agents: req.agents,
			scope: req.scope,
			project_root: req.project_root,
		};
		assert_eq!(req.session_id, "s");
	}
}
