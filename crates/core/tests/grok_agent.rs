use aghub_core::models::AgentType;
use aghub_core::registry;

#[test]
fn registry_resolves_grok_not_fallback() {
	// `registry::get` takes an `AgentType`, not a string id. Passing
	// `AgentType::Grok` proves the descriptor is registered rather than
	// silently resolving to the Claude fallback.
	let d = registry::get(AgentType::Grok);
	assert_eq!(d.id, "grok");
	// Symmetric global + project for skills and MCP
	assert!(d.mcp_global_path.is_some());
	assert!(d.mcp_project_path.is_some());
	assert!(d.capabilities.skills.scopes.global);
	assert!(d.capabilities.skills.scopes.project);
	assert!(d.capabilities.mcp.scopes.global);
	assert!(d.capabilities.mcp.scopes.project);
	assert!(d.capabilities.mcp.enable_disable);
	// Symmetric global + project for sub-agents
	assert!(d.capabilities.sub_agents.scopes.global);
	assert!(d.capabilities.sub_agents.scopes.project);
}
