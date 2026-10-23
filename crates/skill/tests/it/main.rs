//! Integration tests that never write process env, linked as one binary.
//! A test that sets env or takes an env lock gets its own `tests/*.rs`.
//! docs/history/tooling.md#integration-tests-share-one-binary

mod discovery_policy;
mod fixture_validation;
mod hash_parity_golden;
mod mutation_lock;
mod npx_interop;
mod skill_path;
