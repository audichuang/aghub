//! Integration tests that never write process env, linked as one binary.
//! A test that sets env or takes an env lock gets its own `tests/*.rs`.
//! docs/history/tooling.md#integration-tests-share-one-binary

mod apply_skill_updates_contract;
mod async_client_init;
