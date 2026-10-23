use aghub_core::models::{AgentType, ResourceScope};
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
	assert!(d.supports_skill_scope(ResourceScope::GlobalOnly));
	assert!(d.supports_skill_scope(ResourceScope::ProjectOnly));
	assert!(d.supports_mcp_scope(ResourceScope::GlobalOnly));
	assert!(d.supports_mcp_scope(ResourceScope::ProjectOnly));
	assert!(d.capabilities.mcp.enable_disable);
	// Symmetric global + project for sub-agents
	assert!(d.supports_sub_agent_scope(ResourceScope::GlobalOnly));
	assert!(d.supports_sub_agent_scope(ResourceScope::ProjectOnly));
}
