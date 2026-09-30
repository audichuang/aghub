//! Wire DTO builders shared by the CLI and API surfaces.
//!
//! These views own the `RemovalOutcome -> wire`, `Skill -> wire`, and
//! `McpServer -> wire` field mapping once, so the CLI `delete`/`add`/`describe`
//! output and the API response DTOs stay in lockstep. Core carries no ts-rs
//! dependency; the ts-rs structs live in `crates/api` and wrap these views.

pub mod mcp;
pub mod removal;
pub mod skill;

pub use mcp::McpView;
pub use removal::{RemovalKind, RemovalView};
pub use skill::SkillView;
