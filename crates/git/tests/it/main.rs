//! Integration tests that never write process env, linked as one binary.
//! A test that sets env or takes an env lock gets its own `tests/*.rs`.
//! docs/history/tooling.md#integration-tests-share-one-binary

mod gix_shallow_backend;
mod shallow_fetch;
mod stage_hash_parity;
mod staging_materializer;
