use serde::Serialize;
use ts_rs::TS;

/// Per-agent skill-coverage projection for GET /api/v1/skills/coverage.
/// needs_link/auto_covered/supported are the FE-partitioning projection of
/// the LinkNeed 3-state; reads_master/writes_master are the REAL
/// classifier facts (whether the agent's resolved read/write skills-dir
/// resolves to the .agents/skills master), carried verbatim from
/// AgentLinkPlan (P2-G) rather than guessed. No raw paths are exposed. The
/// frontend partitions on auto_covered/needs_link; the master booleans are
/// honest diagnostics.
#[derive(Debug, Serialize, TS)]
#[ts(export)]
pub struct AgentSkillCoverageDto {
	pub id: String,
	pub scope: String,
	pub reads_master: bool,
	pub writes_master: bool,
	pub needs_link: bool,
	pub auto_covered: bool,
	pub supported: bool,
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn serializes_with_expected_keys() {
		let dto = AgentSkillCoverageDto {
			id: "claude".to_string(),
			scope: "global".to_string(),
			reads_master: false,
			writes_master: false,
			needs_link: true,
			auto_covered: false,
			supported: true,
		};
		let json = serde_json::to_string(&dto).unwrap();
		assert_eq!(
			json,
			r#"{"id":"claude","scope":"global","reads_master":false,"writes_master":false,"needs_link":true,"auto_covered":false,"supported":true}"#
		);
	}

	/// Finding #4: this ts-rs DTO and the shared core `AgentSkillCoverageView`
	/// (which the CLI serializes) must emit BYTE-IDENTICAL JSON — the
	/// single-source contract. The core view's field order/names are the
	/// authority; if the two drift, this fails.
	#[test]
	fn dto_matches_shared_core_view_byte_for_byte() {
		use aghub_core::skills::linker::classify::{AgentLinkPlan, LinkNeed};
		let plan = AgentLinkPlan {
			agent_id: "claude",
			need: LinkNeed::NeedsLink {
				agent_skills_dir: std::path::PathBuf::from("/x"),
			},
			installed: false,
			reads_master: false,
			writes_master: true,
		};
		let view =
			aghub_core::skills::linker::AgentSkillCoverageView::from_plan(
				&plan, "global",
			);
		let dto = AgentSkillCoverageDto {
			id: view.id.clone(),
			scope: view.scope.clone(),
			reads_master: view.reads_master,
			writes_master: view.writes_master,
			needs_link: view.needs_link,
			auto_covered: view.auto_covered,
			supported: view.supported,
		};
		assert_eq!(
			serde_json::to_string(&dto).unwrap(),
			serde_json::to_string(&view).unwrap(),
			"API DTO and shared core view must serialize identically"
		);
	}
}
