use aghub_core::models::SubAgent;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::dto::common::ConfigSource;

#[derive(Debug, Deserialize, TS)]
#[ts(export)]
pub struct CreateSubAgentRequest {
	pub name: String,
	pub description: String,
	pub instruction: String,
}

impl From<CreateSubAgentRequest> for SubAgent {
	fn from(req: CreateSubAgentRequest) -> Self {
		SubAgent {
			name: req.name,
			description: Some(req.description),
			instruction: Some(req.instruction),
			source_path: None,
			config_source: None,
			extra_frontmatter: Default::default(),
		}
	}
}

#[derive(Debug, Deserialize, TS)]
#[ts(export)]
pub struct UpdateSubAgentRequest {
	pub name: Option<String>,
	pub description: String,
	pub instruction: String,
}

impl From<UpdateSubAgentRequest>
	for aghub_core::manager::sub_agent::SubAgentPatch
{
	fn from(req: UpdateSubAgentRequest) -> Self {
		Self {
			name: req.name,
			description: Some(req.description),
			instruction: Some(req.instruction),
		}
	}
}

#[derive(Debug, Serialize, TS)]
#[ts(export)]
pub struct SubAgentResponse {
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

impl From<SubAgent> for SubAgentResponse {
	fn from(s: SubAgent) -> Self {
		SubAgentResponse::from(&s)
	}
}

impl From<&SubAgent> for SubAgentResponse {
	fn from(s: &SubAgent) -> Self {
		let view = aghub_core::dto::SubAgentView::from(s);
		SubAgentResponse {
			name: view.name,
			description: view.description,
			instruction: view.instruction,
			source_path: view.source_path,
			source: view.source.map(Into::into),
			agent: view.agent,
		}
	}
}

impl From<(SubAgent, &str)> for SubAgentResponse {
	fn from((s, agent_id): (SubAgent, &str)) -> Self {
		SubAgentResponse {
			agent: Some(agent_id.to_string()),
			..SubAgentResponse::from(s)
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use aghub_core::dto::SubAgentView;
	use aghub_core::models::{ConfigSource, SubAgent};

	#[test]
	fn sub_agent_response_matches_core_view() {
		let s = SubAgent {
			name: "reviewer".to_string(),
			description: Some("Code reviewer".to_string()),
			instruction: Some("Review code carefully".to_string()),
			source_path: Some("/x/rev.md".to_string()),
			config_source: Some(ConfigSource::Global),
			extra_frontmatter: Default::default(),
		};
		assert_eq!(
			serde_json::to_value(SubAgentResponse::from(&s)).unwrap(),
			serde_json::to_value(SubAgentView::from(&s)).unwrap(),
		);

		let s_none = SubAgent {
			name: "minimal".to_string(),
			description: None,
			instruction: None,
			source_path: None,
			config_source: None,
			extra_frontmatter: Default::default(),
		};
		assert_eq!(
			serde_json::to_value(SubAgentResponse::from(&s_none)).unwrap(),
			serde_json::to_value(SubAgentView::from(&s_none)).unwrap(),
		);
	}
}
