//! Shared skill wire DTO.
//!
//! [`SkillView`] is the single source of truth for the field list both the CLI
//! (`describe`/`add` output) and the API (`SkillResponse`) serialize from a
//! [`Skill`]. The `shared_with` advisory (this grant also reaches these other
//! agents, because they read the SAME Referrer directory) is a DTO field both
//! surfaces emit instead of only the CLI's stderr note. It replaces the old
//! `native_reader` boolean, which answered "does this agent see the Master
//! without a link" — a question with no answer once the Master moved to a store
//! nothing reads.
//!
//! Serde defaults to snake_case. No ts-rs here — the ts-rs `SkillResponse`
//! stays in `crates/api` as a thin wrapper over this view.

use crate::models::{ConfigSource, Skill};
use serde::Serialize;

/// Wire view of a [`Skill`] plus the `shared_with` install advisory.
///
/// The `skip_serializing_if` attributes here must mirror the api
/// `SkillResponse` exactly (`canonical_path`/`source`/`agent` skipped when
/// `None`; `source_path`/`description`/`author`/`version` serialize as null) so
/// the CLI (which serializes this view directly) and the API wrapper are one
/// wire shape. The `skill_view_and_response_serialize_the_same_wire_shape`
/// test in `crates/api` pins that parity.
#[derive(Debug, Clone, Serialize)]
pub struct SkillView {
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
	/// Advisory: other agents that receive this skill through the SAME Referrer
	/// directory. Empty for a private dir. Non-empty is the disclosure a user
	/// needs BEFORE granting — and before a later removal takes it from all of
	/// them at once.
	pub shared_with: Vec<String>,
	/// Advisory: the install that produced this view was a no-op — the skill
	/// was already present, so NOTHING was written and every field above
	/// describes the EXISTING master, not the source that was submitted.
	/// Only the install paths set it; a plain read leaves it false.
	pub already_installed: bool,
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
			shared_with: Vec::new(),
			already_installed: false,
		}
	}
}

impl SkillView {
	/// Tag the view with the agent it was resolved for.
	pub fn with_agent(mut self, agent_id: &str) -> Self {
		self.agent = Some(agent_id.to_string());
		self
	}

	/// Set the `shared_with` install advisory.
	pub fn with_shared_with(mut self, v: Vec<String>) -> Self {
		self.shared_with = v;
		self
	}

	/// Set the `already_installed` advisory (the install was a no-op).
	pub fn with_already_installed(mut self, v: bool) -> Self {
		self.already_installed = v;
		self
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn skill_view_serializes_snake_case_and_shared_with_field() {
		let mut skill = Skill::new("foo");
		skill.source_path = Some("~/.claude/skills/foo/SKILL.md".to_string());
		skill.config_source = Some(ConfigSource::Global);
		let view = SkillView::from(&skill);
		let json = serde_json::to_value(&view).unwrap();
		assert!(json.get("source_path").is_some(), "snake_case key present");
		assert_eq!(json["shared_with"], serde_json::json!([]));
		assert_eq!(json["source"], serde_json::json!("global"));
		assert!(json.get("content").is_none(), "content not on the view");
	}

	#[test]
	fn with_agent_and_shared_with_builders_set_fields() {
		let skill = Skill::new("foo");
		let view = SkillView::from(&skill)
			.with_agent("claude")
			.with_shared_with(vec!["warp".to_string()]);
		assert_eq!(view.agent.as_deref(), Some("claude"));
		assert_eq!(view.shared_with, vec!["warp".to_string()]);
		let json = serde_json::to_value(&view).unwrap();
		assert_eq!(json["agent"], serde_json::json!("claude"));
		assert_eq!(json["shared_with"], serde_json::json!(["warp"]));
	}
}
