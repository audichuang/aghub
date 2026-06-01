//! Remote SSH management: tauri-free transport + bring-up logic.
//!
//! This crate holds the pure, unit-testable core of the remote SSH feature:
//! the [`ssh::Connection`] model, the [`ssh::CommandRunner`] abstraction (with a
//! real [`ssh::SystemRunner`] and a test `MockRunner`), the argv builders /
//! output parsers, and (in a later work item) the remote `aghub-api` bring-up
//! state machine. The Tauri command layer in `crates/desktop/src-tauri` is a
//! thin wrapper over this crate.
//!
//! Modules are added by the implementation work items W2 (ssh foundation) and
//! W3 (bring-up logic).

pub mod bringup;
pub mod ssh;

#[cfg(test)]
pub(crate) mod test_support;
