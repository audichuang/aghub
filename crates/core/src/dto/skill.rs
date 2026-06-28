//! Shared skill wire DTO.
//!
//! [`SkillView`] is the single source of truth for the field list both the CLI
//! (`describe`/`add` output) and the API (`SkillResponse`) serialize from a
//! [`Skill`]. The `native_reader` advisory (target agent reads the `.agents`
//! Master directly, so no per-agent symlink is created) is a DTO field both
//! surfaces can emit instead of only the CLI's stderr note.
//!
//! Serde defaults to snake_case. No ts-rs here — the ts-rs `SkillResponse`
//! stays in `crates/api` as a thin wrapper over this view.

use crate::models::{ConfigSource, Skill};
use serde::Serialize;

/// Wire view of a [`Skill`] plus the `native_reader` install advisory.
#[derive(Debug, Clone, Serialize)]
pub struct SkillView {
	pub name: String,
	pub enabled: bool,
	pub source_path: Option<String>,
	pub canonical_path: Option<String>,
	pub description: Option<String>,
	pub author: Option<String>,
	pub version: Option<String>,
	pub tools: Vec<String>,
	pub source: Option<ConfigSource>,
	pub agent: Option<String>,
	/// Advisory: the target agent is a NativeReader (reads the `.agents`
	/// master directly), so a universal install writes only the master with
	/// no per-agent link.
	pub native_reader: bool,
}

impl From<&Skill> for SkillView {
	fn from(skill: &Skill) -> Self {
		Self {
			name: skill.name.clone(),
			enabled: skill.enabled,
			source_path: skill.source_path.clone(),
			canonical_path: skill.canonical_path.clone(),
			description: skill.description.clone(),
			author: skill.author.clone(),
			version: skill.version.clone(),
			tools: skill.tools.clone(),
			source: skill.config_source,
			agent: None,
			native_reader: false,
		}
	}
}

impl SkillView {
	/// Tag the view with the agent it was resolved for.
	pub fn with_agent(mut self, agent_id: &str) -> Self {
		self.agent = Some(agent_id.to_string());
		self
	}

	/// Set the `native_reader` install advisory.
	pub fn with_native_reader(mut self, v: bool) -> Self {
		self.native_reader = v;
		self
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn skill_view_serializes_snake_case_and_native_reader_field() {
		let mut skill = Skill::new("foo");
		skill.source_path = Some("~/.claude/skills/foo/SKILL.md".to_string());
		skill.config_source = Some(ConfigSource::Global);
		let view = SkillView::from(&skill);
		let json = serde_json::to_value(&view).unwrap();
		assert!(json.get("source_path").is_some(), "snake_case key present");
		assert_eq!(json["native_reader"], serde_json::json!(false));
		assert_eq!(json["source"], serde_json::json!("global"));
		assert!(json.get("content").is_none(), "content not on the view");
	}

	#[test]
	fn with_agent_and_native_reader_builders_set_fields() {
		let skill = Skill::new("foo");
		let view = SkillView::from(&skill)
			.with_agent("claude")
			.with_native_reader(true);
		assert_eq!(view.agent.as_deref(), Some("claude"));
		assert!(view.native_reader);
		let json = serde_json::to_value(&view).unwrap();
		assert_eq!(json["agent"], serde_json::json!("claude"));
		assert_eq!(json["native_reader"], serde_json::json!(true));
	}
}
