use aghub_core::models::AgentType;
use aghub_core::registry;

#[test]
fn registry_resolves_hermes_not_fallback() {
	// `registry::get` takes an `AgentType`, not a string id. Passing
	// `AgentType::Hermes` proves the descriptor is registered rather than
	// silently resolving to the Claude fallback.
	let d = registry::get(AgentType::Hermes);
	assert_eq!(d.id, "hermes");
	// global-only: has a global MCP path, no project path
	assert!(d.mcp_global_path.is_some());
	assert!(d.mcp_project_path.is_none());
	assert!(d.capabilities.skills.scopes.global);
	assert!(!d.capabilities.skills.scopes.project);
	assert!(d.capabilities.mcp.enable_disable);
}
