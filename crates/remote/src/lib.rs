//! Remote SSH management: tauri-free transport + bring-up logic.
//!
//! The pure, unit-testable core of the remote SSH feature: the
//! [`ssh::Connection`] model, the [`ssh::CommandRunner`] abstraction (real
//! [`ssh::SystemRunner`], test `MockRunner`), argv builders / output parsers,
//! and the remote `aghub-api` bring-up state machine ([`bringup`]). The Tauri
//! command layer in `crates/desktop/src-tauri` is a thin wrapper over it.

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
