//! Skill-specific server-side helpers (not the HTTP routes, which live in
//! `routes::skills`).
//!
//! The F1 update-check orchestrator was extracted into the dedicated
//! `skill-update` crate (shared by the api and the CLI); only the rename-guard
//! re-export remains here.
pub(crate) mod rename;
pub(crate) mod resync;
