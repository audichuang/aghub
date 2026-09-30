//! Shared sub-agent wire DTO.
//!
//! [`SubAgent`]'s own serde skips `instruction` (which lives in the file body,
//! not YAML frontmatter), so [`SubAgentView`] is the wire view that carries it
//! for both CLI and API surfaces.

use crate::models::{ConfigSource, SubAgent};
use serde::Serialize;

/// Wire view of a [`SubAgent`].
#[derive(Debug, Clone, Serialize)]
pub struct SubAgentView {
	pub name: String,
	pub description: Option<String>,
	pub instruction: Option<String>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub source_path: Option<String>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub source: Option<ConfigSource>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub agent: Option<String>,
}

impl From<&SubAgent> for SubAgentView {
	fn from(s: &SubAgent) -> Self {
		Self {
			name: s.name.clone(),
			description: s.description.clone(),
			instruction: s.instruction.clone(),
			source_path: s.source_path.clone(),
			source: s.config_source,
			agent: None,
		}
	}
}

impl SubAgentView {
	/// Tag the view with the agent it was resolved for.
	pub fn with_agent(mut self, agent_id: &str) -> Self {
		self.agent = Some(agent_id.to_string());
		self
	}
}
