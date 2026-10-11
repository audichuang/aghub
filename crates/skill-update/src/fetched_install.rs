//! The Fetched Source install flow shared by every surface: agent precheck with
//! per-row attribution, and the one lock-source spelling an install records.

use aghub_core::models::AgentType;

/// One per-(skill, agent) row of an install result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallRow {
	pub name: String,
	pub agent: String,
	pub success: bool,
	pub error: Option<String>,
}

/// Requested agent ids in request order, all known. Duplicates are kept. Scope
/// support is NOT filtered: core's all-target preflight must see every target.
#[derive(Debug, Clone)]
pub struct InstallAgents {
	ids: Vec<String>,
	types: Vec<AgentType>,
}

/// At least one requested id is unknown: one (raw id, message) per requested
/// agent, in request order.
#[derive(Debug, Clone)]
pub struct AgentRejection {
	per_agent: Vec<(String, String)>,
}

impl InstallAgents {
	pub fn parse(raw: &[String]) -> Result<Self, AgentRejection> {
		let parsed: Vec<_> =
			raw.iter().map(|id| (id, id.parse::<AgentType>())).collect();
		if parsed.iter().any(|(_, t)| t.is_err()) {
			let per_agent = parsed
				.into_iter()
				.map(|(id, t)| {
					let msg = match t {
						Err(_) => format!("Unknown agent '{id}'"),
						Ok(_) => "Another requested agent is invalid; nothing was written"
							.to_string(),
					};
					(id.clone(), msg)
				})
				.collect();
			return Err(AgentRejection { per_agent });
		}
		Ok(Self {
			ids: raw.to_vec(),
			types: parsed.into_iter().filter_map(|(_, t)| t.ok()).collect(),
		})
	}

	pub fn types(&self) -> &[AgentType] {
		&self.types
	}

	pub fn ids(&self) -> &[String] {
		&self.ids
	}
}

impl AgentRejection {
	/// Label-major rows, agents in request order within each label.
	pub fn rows(&self, labels: &[String]) -> Vec<InstallRow> {
		labels
			.iter()
			.flat_map(|label| {
				self.per_agent
					.iter()
					.map(move |(agent, message)| InstallRow {
						name: label.clone(),
						agent: agent.clone(),
						success: false,
						error: Some(message.clone()),
					})
			})
			.collect()
	}
}

/// THE lock coordinate an install records (both API routes and CLI source sync).
pub fn install_lock_source(
	resolved: &aghub_git::ResolvedRemoteSource,
	ref_name: Option<String>,
) -> skill::InstallLockSource {
	skill::InstallLockSource {
		source: resolved.lock_source(),
		source_type: resolved.source_type.as_str().to_string(),
		source_url: resolved.source_url.clone(),
		ref_name,
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn s(v: &[&str]) -> Vec<String> {
		v.iter().map(|x| x.to_string()).collect()
	}

	#[test]
	fn parse_keeps_request_order_and_duplicates() {
		let a = InstallAgents::parse(&s(&["opencode", "claude", "opencode"]))
			.unwrap();
		assert_eq!(a.ids(), &s(&["opencode", "claude", "opencode"])[..]);
		assert_eq!(
			a.types(),
			&[AgentType::OpenCode, AgentType::Claude, AgentType::OpenCode]
		);
	}

	#[test]
	fn unknown_id_rejects_every_requested_agent_with_its_own_message() {
		let r = InstallAgents::parse(&s(&["claude", "nope"])).unwrap_err();
		assert_eq!(
			r.per_agent,
			vec![
				(
					"claude".to_string(),
					"Another requested agent is invalid; nothing was written"
						.to_string()
				),
				("nope".to_string(), "Unknown agent 'nope'".to_string()),
			]
		);
	}

	#[test]
	fn rejection_rows_are_label_major() {
		let r = InstallAgents::parse(&s(&["claude", "nope"])).unwrap_err();
		let one = r.rows(&[String::new()]);
		assert_eq!(one.len(), 2);
		assert!(one.iter().all(|x| x.name.is_empty() && !x.success));
		assert_eq!(one[0].agent, "claude");
		assert_eq!(one[1].agent, "nope");

		let rows = r.rows(&s(&["a", "b"]));
		let got: Vec<_> = rows
			.iter()
			.map(|x| (x.name.as_str(), x.agent.as_str()))
			.collect();
		assert_eq!(
			got,
			vec![
				("a", "claude"),
				("a", "nope"),
				("b", "claude"),
				("b", "nope")
			]
		);
		assert!(rows[1].error.as_deref().unwrap().contains("Unknown agent"));
	}

	#[test]
	fn lock_source_carries_resolved_coordinates_and_ref() {
		let resolved = aghub_git::resolve_remote_source("owner/repo").unwrap();
		let ls = install_lock_source(&resolved, Some("dev".to_string()));
		assert_eq!(ls.source, resolved.lock_source());
		assert_eq!(ls.source_type, resolved.source_type.as_str());
		assert_eq!(ls.source_url, resolved.source_url);
		assert_eq!(ls.ref_name.as_deref(), Some("dev"));
		assert_eq!(install_lock_source(&resolved, None).ref_name, None);
	}
}
