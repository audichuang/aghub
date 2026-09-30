use crate::{eprintln_verbose, ResourceType};
use aghub_core::dto::{McpView, SkillView, SubAgentView};
use aghub_core::manager::ConfigManager;
use anyhow::{Context, Result};
use serde::Serialize;
use tabled::builder::Builder;
use tabled::settings::Style;

#[derive(Serialize)]
struct McpRow {
	#[serde(flatten)]
	view: McpView,
	#[serde(rename = "type")]
	kind: &'static str,
}

impl From<McpView> for McpRow {
	fn from(view: McpView) -> Self {
		let kind = view.transport.kind();
		Self { view, kind }
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

/// Render MCP servers as a table, or the exact `McpRow` array under `--json`.
fn print_mcps(rows: &[McpRow], json: bool) -> Result<()> {
	if json {
		println!("{}", serde_json::to_string_pretty(rows)?);
		return Ok(());
	}
	if rows.is_empty() {
		println!("No MCP servers.");
		return Ok(());
	}
	let with_agent = rows.iter().any(|r| r.view.agent.is_some());
	let mut builder = Builder::default();
	let mut header = vec!["NAME".to_string()];
	if with_agent {
		header.push("AGENT".to_string());
	}
	header.push("ENABLED".to_string());
	header.push("TRANSPORT".to_string());
	header.push("TARGET".to_string());
	builder.push_record(header);
	for r in rows {
		let mut row = vec![r.view.name.clone()];
		if with_agent {
			row.push(r.view.agent.clone().unwrap_or_else(|| "—".to_string()));
		}
		row.push(if r.view.enabled { "yes" } else { "no" }.to_string());
		row.push(r.kind.to_string());
		let target = match &r.view.transport {
			aghub_core::models::McpTransport::Stdio {
				command, args, ..
			} => {
				if args.is_empty() {
					command.clone()
				} else {
					format!("{command} {}", args.join(" "))
				}
			}
			aghub_core::models::McpTransport::Sse { url, .. }
			| aghub_core::models::McpTransport::StreamableHttp {
				url, ..
			} => url.clone(),
		};
		row.push(truncate(&target, 60));
		builder.push_record(row);
	}
	let mut table = builder.build();
	table.with(Style::sharp());
	println!("{table}");
	Ok(())
}

/// Render sub-agents as a table, or the exact `SubAgentView` array under `--json`.
fn print_sub_agents(views: &[SubAgentView], json: bool) -> Result<()> {
	if json {
		println!("{}", serde_json::to_string_pretty(views)?);
		return Ok(());
	}
	if views.is_empty() {
		println!("No sub-agents.");
		return Ok(());
	}
	let with_agent = views.iter().any(|v| v.agent.is_some());
	let mut builder = Builder::default();
	let mut header = vec!["NAME".to_string()];
	if with_agent {
		header.push("AGENT".to_string());
	}
	header.push("DESCRIPTION".to_string());
	builder.push_record(header);
	for v in views {
		let mut row = vec![v.name.clone()];
		if with_agent {
			row.push(v.agent.clone().unwrap_or_else(|| "—".to_string()));
		}
		row.push(truncate(v.description.as_deref().unwrap_or(""), 60));
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
			let rows: Vec<McpRow> = config
				.mcps
				.iter()
				.map(|m| McpRow::from(McpView::from(m)))
				.collect();
			eprintln_verbose!("Found {} MCP servers", rows.len());
			print_mcps(&rows, json)?;
		}
		ResourceType::SubAgents => {
			manager.ensure_sub_agents_readable()?;
			let views: Vec<SubAgentView> =
				config.sub_agents.iter().map(SubAgentView::from).collect();
			eprintln_verbose!("Found {} sub-agents", views.len());
			print_sub_agents(&views, json)?;
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
			let rows: Vec<McpRow> = resources
				.into_iter()
				.flat_map(|r| {
					let agent_id = r.agent_id;
					r.mcps.into_iter().map(move |m| {
						McpRow::from(McpView::from(&m).with_agent(agent_id))
					})
				})
				.collect();
			eprintln_verbose!(
				"Found {} MCP servers across all agents",
				rows.len()
			);
			print_mcps(&rows, json)?;
		}
		ResourceType::SubAgents => {
			let views: Vec<SubAgentView> = resources
				.into_iter()
				.flat_map(|r| {
					let agent_id = r.agent_id;
					r.sub_agents.into_iter().map(move |s| {
						SubAgentView::from(&s).with_agent(agent_id)
					})
				})
				.collect();
			eprintln_verbose!(
				"Found {} sub-agents across all agents",
				views.len()
			);
			print_sub_agents(&views, json)?;
		}
	}
	Ok(())
}
