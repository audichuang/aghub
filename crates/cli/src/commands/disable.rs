use crate::{eprintln_verbose, ResourceType};
use aghub_core::manager::ConfigManager;
use anyhow::Result;
use serde_json::json;

pub fn execute(
	manager: &mut ConfigManager,
	resource: ResourceType,
	name: String,
) -> Result<serde_json::Value> {
	// The caller prints the payload (single-agent) or wraps it in the batch
	// envelope (multi-agent) — command logic stays print-free.
	let payload = match resource {
		ResourceType::Skills => {
			eprintln_verbose!("Disabling skill: {}", name);
			manager.disable_skill(&name)?;
			eprintln_verbose!("Skill disabled successfully");
			json!({"enabled": false, "name": name, "type": "skill" })
		}
		ResourceType::Mcps => {
			eprintln_verbose!("Disabling MCP server: {}", name);
			manager.disable_mcp(&name)?;
			eprintln_verbose!("MCP server disabled successfully");
			json!({"enabled": false, "name": name, "type": "mcp" })
		}
	};

	Ok(payload)
}
