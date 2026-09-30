//! `aghub-cli agents <list|enable|disable>` — show or change which agents
//! aghub manages (the selection read by `-a all`, `source sync -a all`,
//! repair, rename, and delete-from-all).

use aghub_core::agent_settings::{read_disabled_agents, set_agents_managed};
use aghub_core::models::AgentSelection;
use aghub_core::AgentType;
use anyhow::{anyhow, Result};
use clap::Subcommand;
use tabled::builder::Builder;
use tabled::settings::Style;

/// Actions for the `agents` subcommand group.
#[derive(Subcommand, Clone)]
pub enum AgentsAction {
	/// List all known agents and whether aghub manages them.
	List,
	/// Turn one or more agents on (managed=true).
	///
	/// Reversible preference toggle (no preview / `--yes` needed).
	/// `-a all`, `source sync -a all`, repair, rename, and
	/// delete-from-all skip unmanaged agents.
	Enable {
		/// Agent id, alias, comma-separated list, or 'all'
		agents: String,
	},
	/// Turn one or more agents off (managed=false).
	///
	/// Reversible preference toggle (no preview / `--yes` needed).
	/// `-a all`, `source sync -a all`, repair, rename, and
	/// delete-from-all skip unmanaged agents.
	Disable {
		/// Agent id, alias, comma-separated list, or 'all'
		agents: String,
	},
}

pub fn execute(action: &AgentsAction, json: bool) -> Result<()> {
	let (agents, managed) = match action {
		AgentsAction::List => return print_agents(json),
		AgentsAction::Enable { agents } => (agents, true),
		AgentsAction::Disable { agents } => (agents, false),
	};
	let list = match AgentSelection::parse(agents)
		.map_err(|e| anyhow!("invalid agents: {e}"))?
	{
		AgentSelection::All => AgentType::ALL.to_vec(),
		AgentSelection::List(v) => v,
	};
	set_agents_managed(&list, managed)?;
	print_agents(json)
}

fn print_agents(json: bool) -> Result<()> {
	let stored = read_disabled_agents()?;
	let configured = stored.is_some();
	let disabled = stored.unwrap_or_default();

	if json {
		let agents: Vec<_> = AgentType::ALL
			.iter()
			.map(|a| {
				let id = a.as_str();
				serde_json::json!({
					"id": id,
					"display_name": a.descriptor().display_name,
					"managed": !disabled.contains(id),
				})
			})
			.collect();
		let out = serde_json::json!({
			"configured": configured,
			"agents": agents,
		});
		println!("{}", serde_json::to_string_pretty(&out)?);
		return Ok(());
	}

	let mut builder = Builder::default();
	builder.push_record(["ID", "NAME", "MANAGED"]);
	for a in AgentType::ALL {
		let id = a.as_str();
		let managed = !disabled.contains(id);
		builder.push_record([
			id,
			a.descriptor().display_name,
			if managed { "yes" } else { "no" },
		]);
	}
	let mut table = builder.build();
	table.with(Style::sharp());
	println!("{table}");
	Ok(())
}
