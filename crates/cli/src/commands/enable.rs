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
			eprintln_verbose!("Enabling skill: {}", name);
			manager.enable_skill(&name)?;
			eprintln_verbose!("Skill enabled successfully");
			json!({"enabled": true, "name": name, "type": "skill" })
		}
		ResourceType::Mcps => {
			eprintln_verbose!("Enabling MCP server: {}", name);
			manager.enable_mcp(&name)?;
			eprintln_verbose!("MCP server enabled successfully");
			json!({"enabled": true, "name": name, "type": "mcp" })
		}
	};

	Ok(payload)
}
