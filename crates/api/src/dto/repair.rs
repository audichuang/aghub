//! Wire DTOs for `POST /api/v1/skills/repair`.
//!
//! Mirrors `aghub_core::skills::repair::RepairReport`. It is a separate ts-rs
//! type only because ts-rs lives in this crate; the FIELD LIST is core's, and
//! `report_dto_matches_the_core_report` pins that so the two cannot drift.
//!
//! The desktop needs all three of the spec's preview facts from ONE response:
//! which Master moves (`master`), which per-agent Referrers appear
//! (`referrers`), and which agents stay fused to the shared slot (`fused`). A
//! user who cannot see that codex remains fused does not know what the
//! migration bought them.

use serde::Serialize;
use ts_rs::TS;

/// What repair did to one skill.
#[derive(Debug, Serialize, TS)]
#[ts(export)]
pub struct RepairReportDto {
	pub name: String,
	/// The shape found, snake_case (`unmigrated_copy`, `conformant`, …), or
	/// null when the scope named no candidate directory at all.
	pub shape: Option<String>,
	/// `conformant` | `migrated` | `relinked` | `reconciled` | `refused`.
	pub outcome: String,
	/// Set only for `refused`: why, and the literal next step. Kept as prose
	/// the UI can show verbatim — a refusal must read as an instruction.
	pub reason: Option<String>,
	pub fix: Option<String>,
	pub master: String,
	pub referrers: Vec<String>,
	pub quarantined: Option<String>,
	pub fused: Vec<String>,
}

/// The whole run.
#[derive(Debug, Serialize, TS)]
#[ts(export)]
pub struct RepairResponse {
	/// True when nothing was written. The desktop shows the preview from this
	/// exact shape and then re-posts with `dry_run: false`.
	pub dry_run: bool,
	pub scope: String,
	pub skills: Vec<RepairReportDto>,
	/// True when any row is `refused`, so the UI does not have to re-derive it.
	pub refused: bool,
}

impl From<&aghub_core::skills::repair::RepairReport> for RepairReportDto {
	fn from(r: &aghub_core::skills::repair::RepairReport) -> Self {
		use aghub_core::skills::repair::RepairOutcome;
		let (outcome, reason, fix) = match &r.outcome {
			RepairOutcome::Conformant => ("conformant", None, None),
			RepairOutcome::Migrated => ("migrated", None, None),
			RepairOutcome::Relinked => ("relinked", None, None),
			RepairOutcome::Reconciled => ("reconciled", None, None),
			RepairOutcome::Refused { reason, fix } => {
				("refused", Some(reason.clone()), Some(fix.clone()))
			}
		};
		Self {
			name: r.name.clone(),
			// Through serde so the wire spelling is the ONE snake_case
			// vocabulary core defines, not a second hand-written mapping that
			// can drift from it.
			shape: r.shape.as_ref().and_then(|s| {
				serde_json::to_value(s).ok().and_then(|v| match v {
					serde_json::Value::String(s) => Some(s),
					// A violation serializes as `{"violation": {...}}`; name
					// the variant rather than dropping it to null.
					serde_json::Value::Object(map) => {
						map.keys().next().cloned()
					}
					_ => None,
				})
			}),
			outcome: outcome.to_string(),
			reason,
			fix,
			master: r.master.display().to_string(),
			referrers: r
				.referrers
				.iter()
				.map(|p| p.display().to_string())
				.collect(),
			quarantined: r
				.quarantined
				.as_ref()
				.map(|p| p.display().to_string()),
			fused: r.fused.clone(),
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use aghub_core::skills::repair::{RepairOutcome, RepairReport};
	use aghub_core::skills::shape::{SkillShape, ViolationKind};

	fn report(
		outcome: RepairOutcome,
		shape: Option<SkillShape>,
	) -> RepairReport {
		RepairReport {
			name: "demo".to_string(),
			shape,
			outcome,
			master: std::path::PathBuf::from("/store/demo"),
			referrers: vec![std::path::PathBuf::from("/a/demo")],
			quarantined: None,
			fused: vec!["codex".to_string()],
			dry_run: false,
		}
	}

	#[test]
	fn a_refusal_carries_both_the_reason_and_the_literal_fix() {
		let dto = RepairReportDto::from(&report(
			RepairOutcome::Refused {
				reason: "differs".to_string(),
				fix: "diff -r a b".to_string(),
			},
			Some(SkillShape::Violation(ViolationKind::ForkedCopy)),
		));
		assert_eq!(dto.outcome, "refused");
		assert_eq!(dto.reason.as_deref(), Some("differs"));
		assert_eq!(
			dto.fix.as_deref(),
			Some("diff -r a b"),
			"the UI shows this verbatim; dropping it leaves a dead end"
		);
		assert_eq!(dto.shape.as_deref(), Some("violation"));
	}

	/// The three facts the spec's preview requires must all survive the DTO.
	#[test]
	fn a_migration_carries_master_referrers_and_who_stays_fused() {
		let dto = RepairReportDto::from(&report(
			RepairOutcome::Migrated,
			Some(SkillShape::UnmigratedCopy),
		));
		assert_eq!(dto.outcome, "migrated");
		assert_eq!(dto.shape.as_deref(), Some("unmigrated_copy"));
		assert_eq!(dto.master, "/store/demo");
		assert_eq!(dto.referrers, vec!["/a/demo".to_string()]);
		assert_eq!(
			dto.fused,
			vec!["codex".to_string()],
			"without this the user cannot see what the migration did NOT buy"
		);
		assert!(dto.reason.is_none() && dto.fix.is_none());
	}
}
