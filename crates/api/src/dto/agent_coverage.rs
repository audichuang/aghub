use serde::Serialize;
use ts_rs::TS;

/// Per-agent skill-coverage projection for GET /api/v1/skills/coverage.
///
/// `reads_master` / `writes_master` / `auto_covered` were removed with
/// `LinkNeed::NativeReader`. Against the `.aghub` store — which no agent reads —
/// all three are permanently false, and the frontend partitioned on
/// `auto_covered`, so it would have rendered an empty bucket forever while
/// presenting three constants as classifier facts.
///
/// `shared_with` is what replaced them: the other agents that receive a skill
/// through the SAME Referrer directory. It is the only thing that lets the UI
/// present a shared slot honestly — checking one checks the group, and
/// unchecking one unchecks the group. No raw paths are exposed.
#[derive(Debug, Serialize, TS)]
#[ts(export)]
pub struct AgentSkillCoverageDto {
	pub id: String,
	pub scope: String,
	pub needs_link: bool,
	pub supported: bool,
	pub shared_with: Vec<String>,
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn serializes_with_expected_keys() {
		let dto = AgentSkillCoverageDto {
			id: "claude".to_string(),
			scope: "global".to_string(),
			needs_link: true,
			supported: true,
			shared_with: vec!["warp".to_string()],
		};
		let json = serde_json::to_string(&dto).unwrap();
		assert_eq!(
			json,
			r#"{"id":"claude","scope":"global","needs_link":true,"supported":true,"shared_with":["warp"]}"#
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
				referrer_dir: std::path::PathBuf::from("/x"),
			},
			installed: false,
			shared_with: vec!["warp"],
		};
		let view =
			aghub_core::skills::linker::AgentSkillCoverageView::from_plan(
				&plan, "global",
			);
		let dto = AgentSkillCoverageDto {
			id: view.id.clone(),
			scope: view.scope.clone(),
			needs_link: view.needs_link,
			supported: view.supported,
			shared_with: view.shared_with.clone(),
		};
		assert_eq!(
			serde_json::to_string(&dto).unwrap(),
			serde_json::to_string(&view).unwrap(),
			"API DTO and shared core view must serialize identically"
		);
	}
}
