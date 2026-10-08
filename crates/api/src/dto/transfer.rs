use aghub_core::scope::WriteScope;
use aghub_core::transfer::{
	InstallTarget, OperationAction, OperationBatchResult, OperationResult,
	ResourceLocator,
};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::error::ApiError;

#[derive(Debug, Clone, Copy, Deserialize, Serialize, TS)]
#[ts(export)]
#[serde(rename_all = "lowercase")]
pub enum InstallScopeDto {
	Global,
	Project,
}

impl InstallScopeDto {
	pub fn to_write_scope(
		self,
		project_root: Option<&str>,
	) -> Result<WriteScope, ApiError> {
		let scope_str = match self {
			Self::Global => "global",
			Self::Project => "project",
		};
		crate::extractors::resolve_write_scope(scope_str, project_root)
	}
}

impl From<&WriteScope> for InstallScopeDto {
	fn from(value: &WriteScope) -> Self {
		match value {
			WriteScope::Global => InstallScopeDto::Global,
			WriteScope::Project { .. } => InstallScopeDto::Project,
		}
	}
}

impl From<WriteScope> for InstallScopeDto {
	fn from(value: WriteScope) -> Self {
		InstallScopeDto::from(&value)
	}
}

#[derive(Debug, Clone, Deserialize, TS)]
#[ts(export)]
pub struct TargetDto {
	pub agent: String,
	pub scope: InstallScopeDto,
	pub project_root: Option<String>,
}

impl TargetDto {
	pub fn to_core(&self) -> Result<InstallTarget, ApiError> {
		let agent =
			crate::extractors::resolve_agent_strings(&[&self.agent])?.remove(0);
		let scope = self.scope.to_write_scope(self.project_root.as_deref())?;

		Ok(InstallTarget { agent, scope })
	}
}

#[derive(Debug, Clone, Deserialize, TS)]
#[ts(export)]
pub struct ResourceLocatorDto {
	pub agent: String,
	pub scope: InstallScopeDto,
	pub project_root: Option<String>,
	pub name: String,
}

impl ResourceLocatorDto {
	pub fn to_core(&self) -> Result<ResourceLocator, ApiError> {
		let agent =
			crate::extractors::resolve_agent_strings(&[&self.agent])?.remove(0);
		let scope = self.scope.to_write_scope(self.project_root.as_deref())?;

		Ok(ResourceLocator {
			agent,
			scope,
			name: self.name.clone(),
		})
	}
}

#[derive(Debug, Clone, Deserialize, TS)]
#[ts(export)]
pub struct TransferRequest {
	pub source: ResourceLocatorDto,
	pub destinations: Vec<TargetDto>,
}

#[derive(Debug, Clone, Deserialize, TS)]
#[ts(export)]
pub struct ReconcileRequest {
	pub source: ResourceLocatorDto,
	pub added: Option<Vec<String>>,
	pub removed: Option<Vec<String>>,
	/// Required to execute a reconcile that REMOVES — the API-side half of the
	/// CLI's `--yes`. Adds alone ignore it. Defaults to false, so a client that
	/// never heard of the field cannot delete by omission.
	#[ts(optional)]
	pub confirm: Option<bool>,
}

impl ReconcileRequest {
	/// The ONE request-to-core confirmation conversion.
	///
	/// All three reconcile routes go through this rather than each spelling
	/// `unwrap_or(false)`: three copies is three places to flip to `true` and
	/// restore the unconfirmed-removal bug, and only one of them has an
	/// end-to-end route test.
	pub fn confirmed(&self) -> bool {
		self.confirm.unwrap_or(false)
	}
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum OperationActionDto {
	Copy,
	Delete,
}

impl From<OperationAction> for OperationActionDto {
	fn from(value: OperationAction) -> Self {
		match value {
			OperationAction::Copy => OperationActionDto::Copy,
			OperationAction::Delete => OperationActionDto::Delete,
		}
	}
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
pub struct OperationResultDto {
	pub agent: String,
	pub scope: InstallScopeDto,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub project_root: Option<String>,
	pub action: OperationActionDto,
	pub success: bool,
	/// Duplicate of `success` under the name `core::batch`'s envelope rows use
	/// (`AgentOpResultView.ok`). Both families serialize into an envelope with
	/// identical top-level keys, so a client written against `row.ok` read
	/// `undefined` here and scored every SUCCESS as a failure. Mirrors
	/// `aghub_core::transfer::OperationResultView`, which
	/// `dto_matches_shared_core_view_byte_for_byte` pins byte-for-byte.
	pub ok: bool,
	/// The target already held this resource; nothing was written. Still a
	/// success row. Always `false` on a Delete row.
	///
	/// Emitted unconditionally, and positioned between `ok` and `error` to
	/// match `OperationResultView` field-for-field —
	/// `dto_matches_shared_core_view_byte_for_byte` compares the SERIALIZED
	/// strings, so a correct field in the wrong slot fails and reads like a
	/// mapping bug.
	pub already_present: bool,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub error: Option<String>,
	#[serde(skip_serializing_if = "Option::is_none")]
	#[ts(optional)]
	pub outcome: Option<crate::dto::skill::RemovalOutcomeKind>,
	#[serde(skip_serializing_if = "Option::is_none")]
	#[ts(optional)]
	pub still_read_by: Option<Vec<String>>,
	#[serde(skip_serializing_if = "Option::is_none")]
	#[ts(optional)]
	pub still_read_by_managed: Option<Vec<String>>,
	#[serde(skip_serializing_if = "Option::is_none")]
	#[ts(optional)]
	pub still_read_by_unmanaged: Option<Vec<String>>,
}

impl From<OperationResult> for OperationResultDto {
	fn from(value: OperationResult) -> Self {
		OperationResultDto {
			agent: value.target.agent.as_str().to_string(),
			scope: (&value.target.scope).into(),
			project_root: value
				.target
				.scope
				.project_root()
				.map(|path| path.to_string_lossy().to_string()),
			action: value.action.into(),
			success: value.success,
			ok: value.success,
			already_present: value.already_present,
			error: value.error,
			outcome: value.outcome.map(Into::into),
			still_read_by: value.still_read_by,
			still_read_by_managed: value.still_read_by_managed,
			still_read_by_unmanaged: value.still_read_by_unmanaged,
		}
	}
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
pub struct OperationBatchResponse {
	pub success_count: usize,
	pub failed_count: usize,
	pub results: Vec<OperationResultDto>,
}

impl From<OperationBatchResult> for OperationBatchResponse {
	fn from(value: OperationBatchResult) -> Self {
		OperationBatchResponse {
			success_count: value.success_count(),
			failed_count: value.failed_count(),
			results: value.results.into_iter().map(Into::into).collect(),
		}
	}
}

#[cfg(test)]
mod tests {
	use aghub_core::transfer::{OperationBatchView, OperationResultView};

	use super::*;

	/// Finding #4: the API DTO (ts-rs) and the shared core `OperationBatchView`
	/// (which the CLI serializes) must emit BYTE-IDENTICAL JSON. This is the
	/// single-source contract — if the two mappings ever drift, this fails.
	#[test]
	fn dto_matches_shared_core_view_byte_for_byte() {
		use std::path::PathBuf;
		let batch = OperationBatchResult {
			results: vec![
				OperationResult {
					target: InstallTarget {
						agent: "claude".parse().unwrap(),
						scope: WriteScope::project(PathBuf::from("/tmp/proj")),
					},
					action: OperationAction::Copy,
					success: true,
					// Non-default on purpose: a parity test that only ever sees
					// `false` cannot catch a mapper that hard-codes it.
					already_present: true,
					error: None,
					outcome: None,
					still_read_by: None,
					still_read_by_managed: None,
					still_read_by_unmanaged: None,
				},
				OperationResult {
					target: InstallTarget {
						agent: "opencode".parse().unwrap(),
						scope: WriteScope::Global,
					},
					action: OperationAction::Delete,
					success: false,
					already_present: false,
					error: Some("nope".to_string()),
					outcome: Some(aghub_core::dto::RemovalKind::Removed),
					still_read_by: None,
					still_read_by_managed: None,
					still_read_by_unmanaged: None,
				},
			],
		};

		let dto_json =
			serde_json::to_string(&OperationBatchResponse::from(batch.clone()))
				.unwrap();
		let view_json =
			serde_json::to_string(&OperationBatchView::from(&batch)).unwrap();
		assert_eq!(
			dto_json, view_json,
			"API DTO and shared core view must serialize identically"
		);
	}

	#[test]
	fn result_dto_matches_result_view() {
		use std::path::PathBuf;
		let result = OperationResult {
			target: InstallTarget {
				agent: "cursor".parse().unwrap(),
				scope: WriteScope::project(PathBuf::from("/x")),
			},
			action: OperationAction::Copy,
			success: true,
			error: None,
			already_present: false,
			outcome: None,
			still_read_by: None,
			still_read_by_managed: None,
			still_read_by_unmanaged: None,
		};
		let dto =
			serde_json::to_string(&OperationResultDto::from(result.clone()))
				.unwrap();
		let view =
			serde_json::to_string(&OperationResultView::from(&result)).unwrap();
		assert_eq!(dto, view);
	}

	#[test]
	fn target_dto_to_core_rejects_missing_project_root() {
		let dto = TargetDto {
			agent: "claude".into(),
			scope: InstallScopeDto::Project,
			project_root: None,
		};
		let err = dto.to_core().unwrap_err();
		assert_eq!(err.body.code, "PROJECT_ROOT_REQUIRED");
	}
}
