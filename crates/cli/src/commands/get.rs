use crate::{eprintln_verbose, ResourceType};
use aghub_core::dto::SkillView;
use aghub_core::manager::ConfigManager;
use anyhow::{Context, Result};
use serde::Serialize;
use tabled::builder::Builder;
use tabled::settings::Style;

#[derive(Serialize)]
pub(crate) struct McpView {
	name: String,
	enabled: bool,
	#[serde(rename = "type")]
	transport_type: String,
	/// Agent identifier (only set when using --agent all)
	#[serde(skip_serializing_if = "Option::is_none")]
	agent: Option<&'static str>,
}

pub(crate) fn mcp_to_view(
	m: &aghub_core::models::McpServer,
	agent: Option<&'static str>,
) -> McpView {
	McpView {
		name: m.name.clone(),
		enabled: m.enabled,
		transport_type: match &m.transport {
			aghub_core::models::McpTransport::Stdio { .. } => {
				"stdio".to_string()
			}
			aghub_core::models::McpTransport::Sse { .. } => "sse".to_string(),
			aghub_core::models::McpTransport::StreamableHttp { .. } => {
				"streamable-http".to_string()
			}
		},
		agent,
	}
}

/// Render skills as a table, or the exact `SkillView` array under `--json`.
///
/// The table drops `source_path`/`canonical_path` — they are absolute and blow
/// the width past any terminal. `describe <name>` shows them for one skill, and
/// `--json` keeps every field for scripts.
fn print_skills(views: &[SkillView], json: bool) -> Result<()> {
	if json {
		println!("{}", serde_json::to_string_pretty(views)?);
		return Ok(());
	}
	if views.is_empty() {
		println!("No skills.");
		return Ok(());
	}
	let with_agent = views.iter().any(|v| v.agent.is_some());
	let mut builder = Builder::default();
	let mut header = vec!["NAME".to_string()];
	if with_agent {
		header.push("AGENT".to_string());
	}
	header.push("ENABLED".to_string());
	header.push("DESCRIPTION".to_string());
	builder.push_record(header);
	for v in views {
		let mut row = vec![v.name.clone()];
		if with_agent {
			row.push(v.agent.clone().unwrap_or_else(|| "—".to_string()));
		}
		row.push(if v.enabled { "yes" } else { "no" }.to_string());
		row.push(truncate(v.description.as_deref().unwrap_or(""), 60));
		builder.push_record(row);
	}
	let mut table = builder.build();
	table.with(Style::sharp());
	println!("{table}");
	Ok(())
}

/// Render MCP servers as a table, or the exact `McpView` array under `--json`.
fn print_mcps(views: &[McpView], json: bool) -> Result<()> {
	if json {
		println!("{}", serde_json::to_string_pretty(views)?);
		return Ok(());
	}
	if views.is_empty() {
		println!("No MCP servers.");
		return Ok(());
	}
	let with_agent = views.iter().any(|v| v.agent.is_some());
	let mut builder = Builder::default();
	let mut header = vec!["NAME".to_string()];
	if with_agent {
		header.push("AGENT".to_string());
	}
	header.push("ENABLED".to_string());
	header.push("TRANSPORT".to_string());
	builder.push_record(header);
	for v in views {
		let mut row = vec![v.name.clone()];
		if with_agent {
			row.push(v.agent.unwrap_or("—").to_string());
		}
		row.push(if v.enabled { "yes" } else { "no" }.to_string());
		row.push(v.transport_type.clone());
		builder.push_record(row);
	}
	let mut table = builder.build();
	table.with(Style::sharp());
	println!("{table}");
	Ok(())
}

/// Clip a cell to `max` chars so one long description cannot widen the table
/// past the terminal. Counts CHARS, not bytes — slicing a multi-byte
/// description at a byte offset would panic.
fn truncate(text: &str, max: usize) -> String {
	let mut chars = text.chars();
	let head: String = chars.by_ref().take(max).collect();
	if chars.next().is_some() {
		format!("{head}…")
	} else {
		head
	}
}

pub fn execute(
	manager: &ConfigManager,
	resource: ResourceType,
	json: bool,
) -> Result<()> {
	let config = manager.config().context("No configuration loaded")?;

	match resource {
		ResourceType::Skills => {
			let views: Vec<SkillView> =
				config.skills.iter().map(SkillView::from).collect();
			eprintln_verbose!("Found {} skills", views.len());
			print_skills(&views, json)?;
		}
		ResourceType::Mcps => {
			let views: Vec<McpView> =
				config.mcps.iter().map(|m| mcp_to_view(m, None)).collect();
			eprintln_verbose!("Found {} MCP servers", views.len());
			print_mcps(&views, json)?;
		}
	}

	Ok(())
}

pub fn execute_all(
	resources: Vec<aghub_core::all_agents::AgentResources>,
	resource: ResourceType,
	json: bool,
) -> Result<()> {
	// Flatten output: each resource has an `agent` field indicating which agent it belongs to
	match resource {
		ResourceType::Skills => {
			let views: Vec<SkillView> = resources
				.into_iter()
				.flat_map(|r| {
					let agent_id = r.agent_id;
					r.skills
						.into_iter()
						.map(move |s| SkillView::from(&s).with_agent(agent_id))
				})
				.collect();
			eprintln_verbose!("Found {} skills across all agents", views.len());
			print_skills(&views, json)?;
		}
		ResourceType::Mcps => {
			let views: Vec<McpView> = resources
				.into_iter()
				.flat_map(|r| {
					let agent_id = r.agent_id;
					r.mcps
						.into_iter()
						.map(move |m| mcp_to_view(&m, Some(agent_id)))
				})
				.collect();
			eprintln_verbose!(
				"Found {} MCP servers across all agents",
				views.len()
			);
			print_mcps(&views, json)?;
		}
	}
	Ok(())
}
