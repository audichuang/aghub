//! DTOs for the unified "Sources" feature.
//!
//! A *source* is a place skills were installed from (a git repo / registry),
//! reconstructed from the skill lock files. `GET /skills/sources` lists the
//! sources in a scope (offline, lock-only). `GET /skills/sources/diff` fetches a
//! single source and reports each skill as not-installed / installed-current /
//! installed-outdated / uncheckable, so the UI can offer "install the new ones".

use serde::Serialize;
use ts_rs::TS;

/// Whether a source can be re-fetched with credentials we already hold.
///
/// - `bound`: an explicit source→credential binding exists.
/// - `hostMatch`: no explicit binding, but a stored credential matches the host.
/// - `missing`: no usable credential (a private source will be `needsCredential`).
/// - `notRequired`: the source looks public; no credential needed.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum CredentialStatus {
	Bound,
	HostMatch,
	Missing,
	NotRequired,
}

/// TypeScript-exported union mirroring
/// `skill_update::sources::SourceSkillState::as_wire()`. Declared here (in the
/// API DTO crate, which has ts-rs) rather than in `skill-update` (which does
/// not) — approach B from the Phase 3 plan §12-C4/GAP5.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum SourceSkillStateDto {
	NotInstalled,
	InstalledCurrent,
	InstalledOutdated,
	Renamed,
	Removed,
	Deprecated,
	Uncheckable,
}

impl From<&skill_update::sources::SourceSkillState> for SourceSkillStateDto {
	fn from(s: &skill_update::sources::SourceSkillState) -> Self {
		use skill_update::sources::SourceSkillState;
		match s {
			SourceSkillState::NotInstalled => Self::NotInstalled,
			SourceSkillState::InstalledCurrent => Self::InstalledCurrent,
			SourceSkillState::InstalledOutdated => Self::InstalledOutdated,
			SourceSkillState::Renamed => Self::Renamed,
			SourceSkillState::Removed => Self::Removed,
			SourceSkillState::Deprecated => Self::Deprecated,
			SourceSkillState::Uncheckable => Self::Uncheckable,
		}
	}
}

/// One aggregated source row in the overview (one repo within one scope).
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct SourceSummaryResponse {
	/// Normalized identifier (e.g. `owner/repo`).
	pub source: String,
	/// Best-effort re-fetch URL (the lock's `sourceUrl`, or reconstructed from
	/// `source` + `sourceType` for project-scope entries that omit it).
	pub source_url: String,
	/// Provider type: `github`, `mintlify`, `local`, ...
	pub source_type: String,
	/// `global` or `project`.
	pub scope: String,
	/// Number of installed skills from this source in this scope.
	pub skill_count: u32,
	/// True when the source is (likely) a private/credentialed source.
	pub is_private: bool,
	/// Credential availability for re-fetching this source.
	pub credential_status: CredentialStatus,
}

/// Response for `GET /skills/sources`.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct SourcesListResponse {
	pub sources: Vec<SourceSummaryResponse>,
}

/// Three-state (plus uncheckable) diff of a single skill within a source.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct SourceSkillDiff {
	pub name: String,
	/// Repo-relative skill path (`<dir>/SKILL.md`) — the identity key.
	pub skill_path: String,
	#[serde(skip_serializing_if = "Option::is_none")]
	#[ts(optional)]
	pub description: Option<String>,
	#[serde(skip_serializing_if = "Option::is_none")]
	#[ts(optional)]
	pub version: Option<String>,
	#[serde(skip_serializing_if = "Option::is_none")]
	#[ts(optional)]
	pub author: Option<String>,
	/// Typed state enum (ts-rs generates a TS union, not `string`).
	/// One of: `notInstalled`, `installedCurrent`, `installedOutdated`,
	/// `renamed`, `removed`, `deprecated`, `uncheckable`.
	pub state: SourceSkillStateDto,
	/// Previous installed skill name when this row is a `renamed` successor:
	/// either the upstream skill at the same `skillPath` now declares a
	/// different `name`, or a CHANGELOG entry maps a removed old name (whose
	/// `skillPath` is gone) onto this successor.
	#[serde(skip_serializing_if = "Option::is_none")]
	#[ts(optional)]
	pub previous_name: Option<String>,
	/// For `uncheckable`/`removed`: redacted reason, aligned with
	/// `SkillUpdateStatusResponse` (`auth`/`network`/`local`/`ssh`/
	/// `unsupportedScheme`/`noPath`/`timeout`).
	#[serde(skip_serializing_if = "Option::is_none")]
	#[ts(optional)]
	pub reason: Option<String>,
	/// Scopes/agents where this skill is already installed (display hint).
	pub installed_paths: Vec<String>,
	/// RFC 3339 timestamp of the upstream tip commit at diff time.
	/// Present only for `installedOutdated` rows.
	#[serde(skip_serializing_if = "Option::is_none")]
	#[ts(optional)]
	pub upstream_commit_time: Option<String>,
}

/// Response for `GET /skills/sources/diff`.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct SourceDiffResponse {
	pub source: String,
	#[serde(skip_serializing_if = "Option::is_none")]
	#[ts(optional)]
	pub git_ref: Option<String>,
	/// A fresh git-clone session id usable with `POST /skills/git/install` to
	/// install the selected not-installed skills without re-scanning.
	#[serde(skip_serializing_if = "Option::is_none")]
	#[ts(optional)]
	pub session_id: Option<String>,
	/// True when the source is private and we lack a usable credential, so the
	/// diff could not be computed and the UI should offer to bind one.
	pub needs_credential: bool,
	pub skills: Vec<SourceSkillDiff>,
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn source_skill_state_dto_serializes_to_camel_case_strings() {
		assert_eq!(
			serde_json::to_string(&SourceSkillStateDto::NotInstalled).unwrap(),
			r#""notInstalled""#
		);
		assert_eq!(
			serde_json::to_string(&SourceSkillStateDto::InstalledCurrent)
				.unwrap(),
			r#""installedCurrent""#
		);
		assert_eq!(
			serde_json::to_string(&SourceSkillStateDto::InstalledOutdated)
				.unwrap(),
			r#""installedOutdated""#
		);
		assert_eq!(
			serde_json::to_string(&SourceSkillStateDto::Renamed).unwrap(),
			r#""renamed""#
		);
		assert_eq!(
			serde_json::to_string(&SourceSkillStateDto::Removed).unwrap(),
			r#""removed""#
		);
		assert_eq!(
			serde_json::to_string(&SourceSkillStateDto::Deprecated).unwrap(),
			r#""deprecated""#
		);
		assert_eq!(
			serde_json::to_string(&SourceSkillStateDto::Uncheckable).unwrap(),
			r#""uncheckable""#
		);
	}

	#[test]
	fn source_skill_diff_state_field_is_not_plain_string() {
		let diff = SourceSkillDiff {
			name: "foo".to_string(),
			skill_path: "foo/SKILL.md".to_string(),
			description: None,
			version: None,
			author: None,
			state: SourceSkillStateDto::InstalledCurrent,
			previous_name: None,
			reason: None,
			installed_paths: vec![],
			upstream_commit_time: None,
		};
		let val = serde_json::to_value(&diff).unwrap();
		assert_eq!(val["state"], "installedCurrent");
	}
}
