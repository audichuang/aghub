//! Shared removal-outcome wire DTO.
//!
//! [`RemovalView`] is the single source of truth for turning a
//! [`RemovalOutcome`] into the success/dry_run/executed/needs_confirm/paths/
//! skipped/deleted_path shape both the CLI `delete` command and the API
//! `DeleteSkillByPathResponse` serialize. The `PathBuf -> String`
//! stringification and `deleted_path` derivation live here and nowhere else.
//!
//! Serde defaults to snake_case, matching the field names the desktop and CLI
//! already consume. No ts-rs here — core carries no ts-rs dependency; the
//! ts-rs struct stays in `crates/api` as a thin wrapper over this view.

use crate::skills::removal::RemovalOutcome;
use serde::Serialize;

/// Wire view of a [`RemovalOutcome`]: the post-execution plan flattened to
/// strings plus the derived `deleted_path`/`dry_run` flags.
#[derive(Debug, Clone, Serialize)]
pub struct RemovalView {
	pub success: bool,
	pub dry_run: bool,
	pub executed: bool,
	pub needs_confirm: bool,
	pub paths: Vec<String>,
	pub skipped: Vec<String>,
	pub deleted_path: Option<String>,
}

impl From<&RemovalOutcome> for RemovalView {
	fn from(outcome: &RemovalOutcome) -> Self {
		let stringify = |paths: &[std::path::PathBuf]| -> Vec<String> {
			paths.iter().map(|p| p.display().to_string()).collect()
		};
		Self {
			success: true,
			dry_run: !outcome.executed,
			executed: outcome.executed,
			needs_confirm: outcome.plan.needs_confirm,
			paths: stringify(&outcome.plan.paths),
			skipped: stringify(&outcome.plan.skipped),
			deleted_path: outcome
				.executed
				.then(|| {
					outcome.plan.paths.first().map(|p| p.display().to_string())
				})
				.flatten(),
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::skills::removal::{
		Layout, PruneStatus, RemovalOutcome, RemovalPlan,
	};
	use std::path::PathBuf;

	fn outcome(executed: bool, paths: Vec<PathBuf>) -> RemovalOutcome {
		RemovalOutcome {
			plan: RemovalPlan {
				layout: Layout::Copy,
				paths,
				skipped: vec![],
				needs_confirm: false,
			},
			executed,
			prune: PruneStatus::NotRun,
		}
	}

	#[test]
	fn removal_view_dry_run_sets_flags_and_no_deleted_path() {
		let view =
			RemovalView::from(&outcome(false, vec![PathBuf::from("/a/foo")]));
		let json = serde_json::to_value(&view).unwrap();
		assert_eq!(json["dry_run"], serde_json::json!(true));
		assert_eq!(json["executed"], serde_json::json!(false));
		assert_eq!(json["deleted_path"], serde_json::Value::Null);
	}

	#[test]
	fn executed_outcome_sets_deleted_path_to_first() {
		let p = PathBuf::from("/a/foo");
		let view = RemovalView::from(&outcome(true, vec![p.clone()]));
		assert_eq!(view.deleted_path.as_deref(), Some("/a/foo"));
		let json = serde_json::to_value(&view).unwrap();
		assert_eq!(json["executed"], serde_json::json!(true));
		assert_eq!(json["dry_run"], serde_json::json!(false));
		assert_eq!(json["deleted_path"], serde_json::json!("/a/foo"));
	}

	#[test]
	fn executed_outcome_with_no_paths_has_null_deleted_path() {
		let view = RemovalView::from(&outcome(true, vec![]));
		assert!(view.deleted_path.is_none());
	}

	#[test]
	fn needs_confirm_and_skipped_paths_map_through() {
		// Guards the single mapper against dropping/hard-coding needs_confirm
		// or skipped: a not-yet-executed all-agents delete carries
		// needs_confirm=true and a kept (skipped) shared master.
		let outcome = RemovalOutcome {
			plan: RemovalPlan {
				layout: Layout::Copy,
				paths: vec![PathBuf::from("/a/foo")],
				skipped: vec![PathBuf::from("/a/master")],
				needs_confirm: true,
			},
			executed: false,
			prune: PruneStatus::NotRun,
		};
		let view = RemovalView::from(&outcome);
		assert!(view.needs_confirm, "needs_confirm must propagate");
		assert_eq!(view.skipped, vec!["/a/master".to_string()]);
		assert!(view.deleted_path.is_none(), "nothing executed");
		let json = serde_json::to_value(&view).unwrap();
		assert_eq!(json["needs_confirm"], serde_json::json!(true));
		assert_eq!(json["skipped"], serde_json::json!(["/a/master"]));
	}
}
