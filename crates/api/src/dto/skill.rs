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
		let mut response = Self::from(&skill);
		response.agent = Some(agent_id.to_string());
		response
	}
}

impl From<&Skill> for SkillResponse {
	fn from(s: &Skill) -> Self {
		SkillResponse {
			name: s.name.clone(),
			enabled: s.enabled,
			source_path: s.source_path.clone(),
			canonical_path: s.canonical_path.clone(),
			description: s.description.clone(),
			author: s.author.clone(),
			version: s.version.clone(),
			tools: s.tools.clone(),
			source: s.config_source.map(Into::into),
			agent: None,
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
	#[serde(skip_serializing_if = "Option::is_none")]
	pub deleted_path: Option<String>,
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

/// Request to atomically rename an installed skill:
/// install upstream-current under `new_name` + delete `old_name`.
#[derive(Debug, Clone, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct AcceptRenameRequest {
	/// Locked name of the installed skill to replace.
	pub old_name: String,
	/// New upstream name (from the `renamed.newName` field).
	pub new_name: String,
	pub scope: String,
	pub project_root: Option<String>,
	/// Must be `true` to execute.  Absent / false → dry-run description only.
	pub confirm: Option<bool>,
}

/// Response from `POST /skills/accept-rename`.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct AcceptRenameResponse {
	pub success: bool,
	pub old_name: String,
	pub new_name: String,
	pub scope: String,
	#[serde(skip_serializing_if = "Option::is_none")]
	#[ts(optional)]
	pub installed_hash: Option<String>,
	pub paths: Vec<String>,
	#[serde(skip_serializing_if = "Option::is_none")]
	#[ts(optional)]
	pub error: Option<String>,
	#[serde(skip_serializing_if = "Option::is_none")]
	#[ts(optional)]
	pub code: Option<String>,
}

#[cfg(test)]
mod tests {
	use super::*;

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

	#[test]
	fn accept_rename_request_deserializes() {
		let json =
			r#"{"oldName":"a","newName":"b","scope":"global","confirm":true}"#;
		let req: AcceptRenameRequest =
			serde_json::from_str(json).expect("must deserialise");
		assert_eq!(req.old_name, "a");
		assert_eq!(req.new_name, "b");
		assert_eq!(req.scope, "global");
		assert_eq!(req.confirm, Some(true));
	}

	#[test]
	fn accept_rename_response_serializes_success() {
		let resp = AcceptRenameResponse {
			success: true,
			old_name: "a".to_string(),
			new_name: "b".to_string(),
			scope: "global".to_string(),
			installed_hash: Some("abc123".to_string()),
			paths: vec!["/some/path".to_string()],
			error: None,
			code: None,
		};
		let val = serde_json::to_value(&resp).unwrap();
		assert_eq!(val["success"], true);
		assert_eq!(val["oldName"], "a");
		assert_eq!(val["newName"], "b");
		assert!(val.get("error").is_none() || val["error"].is_null());
	}
}
