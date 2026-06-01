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
	/// Tilde-prefixed `source_path` values of every installation to replace.
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
}

#[derive(Debug, TS, rocket::FromForm)]
#[ts(export)]
pub struct SkillTreeQuery {
	pub path: String,
}

#[derive(Debug, TS, rocket::FromForm)]
#[ts(export)]
pub struct ProjectLockQuery {
	pub project_path: Option<String>,
}

/// Per-skill update status surfaced by `GET /skills/check-updates`.
///
/// Tagged by `status` (camelCase): `upToDate`, `updateAvailable`,
/// `uncheckable`. The `reason` on `uncheckable` is already redacted of any URL
/// userinfo at the orchestration boundary.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum SkillUpdateStatusResponse {
	UpToDate,
	UpdateAvailable { current: String, available: String },
	Uncheckable { reason: String },
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
	fn up_to_date_serializes_status_only() {
		let resp = SkillUpdateResponse {
			name: "s".to_string(),
			scope: "global".to_string(),
			status: SkillUpdateStatusResponse::UpToDate,
		};
		let json = serde_json::to_value(&resp).unwrap();
		assert_eq!(json["status"], "upToDate");
	}
}
