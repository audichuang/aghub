//! Integration tests that never write process env, linked as one binary.
//! A test that sets env or takes an env lock gets its own `tests/*.rs`.
//! docs/history/tooling.md#integration-tests-share-one-binary

mod grok_agent;
mod hermes_agent;
mod integration_tests;
mod mcp_dialect_decisions;
mod mcp_dialect_golden;
mod mcp_dialect_roundtrip;
mod mcp_tests;
mod mutation_lock_flows;
mod npx_skill_path_ownership;
mod registry_bijection;
mod sub_agent_tests;
