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
pub mod fs;
pub mod ssh;
pub mod ssh_config;

/// Windows `CREATE_NO_WINDOW` process-creation flag. Applied to every external
/// process this crate spawns (ssh/scp) so the windowless desktop GUI does not
/// flash a console window on each remote operation. No-op off Windows.
#[cfg(windows)]
pub(crate) const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[cfg(test)]
pub(crate) mod test_support;
