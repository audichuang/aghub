use aghub_core::transfer::{
	InstallScope, InstallTarget, OperationAction, OperationBatchResult,
	OperationResult, ResourceLocator,
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use ts_rs::TS;

use crate::error::ApiError;

#[derive(Debug, Clone, Copy, Deserialize, Serialize, TS)]
#[ts(export)]
#[serde(rename_all = "lowercase")]
pub enum InstallScopeDto {
	Global,
	Project,
}

impl From<InstallScopeDto> for InstallScope {
	fn from(value: InstallScopeDto) -> Self {
		match value {
			InstallScopeDto::Global => InstallScope::Global,
			InstallScopeDto::Project => InstallScope::Project,
		}
	}
}

impl From<InstallScope> for InstallScopeDto {
	fn from(value: InstallScope) -> Self {
		match value {
			InstallScope::Global => InstallScopeDto::Global,
			InstallScope::Project => InstallScopeDto::Project,
		}
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
		let agent = self.agent.parse().map_err(|_| {
			ApiError::new(
				rocket::http::Status::BadRequest,
				format!("Unknown agent '{}'", self.agent),
				"INVALID_PARAM",
			)
		})?;

		Ok(InstallTarget {
			agent,
			scope: self.scope.into(),
			project_root: self.project_root.as_deref().map(PathBuf::from),
		})
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
		let agent = self.agent.parse().map_err(|_| {
			ApiError::new(
				rocket::http::Status::BadRequest,
				format!("Unknown agent '{}'", self.agent),
				"INVALID_PARAM",
			)
		})?;

		Ok(ResourceLocator {
			agent,
			scope: self.scope.into(),
			project_root: self.project_root.as_deref().map(PathBuf::from),
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
}

impl From<OperationResult> for OperationResultDto {
	fn from(value: OperationResult) -> Self {
		OperationResultDto {
			agent: value.target.agent.as_str().to_string(),
			scope: value.target.scope.into(),
			project_root: value
				.target
				.project_root
				.map(|path| path.to_string_lossy().to_string()),
			action: value.action.into(),
			success: value.success,
			ok: value.success,
			already_present: value.already_present,
			error: value.error,
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
						scope: InstallScope::Project,
						project_root: Some(PathBuf::from("/tmp/proj")),
					},
					action: OperationAction::Copy,
					success: true,
					// Non-default on purpose: a parity test that only ever sees
					// `false` cannot catch a mapper that hard-codes it.
					already_present: true,
					error: None,
				},
				OperationResult {
					target: InstallTarget {
						agent: "opencode".parse().unwrap(),
						scope: InstallScope::Global,
						project_root: None,
					},
					action: OperationAction::Delete,
					success: false,
					already_present: false,
					error: Some("nope".to_string()),
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
				scope: InstallScope::Project,
				project_root: Some(PathBuf::from("/x")),
			},
			action: OperationAction::Copy,
			success: true,
			error: None,
			already_present: false,
		};
		let dto =
			serde_json::to_string(&OperationResultDto::from(result.clone()))
				.unwrap();
		let view =
			serde_json::to_string(&OperationResultView::from(&result)).unwrap();
		assert_eq!(dto, view);
	}
}
