//! Shared rename-detection helpers. The implementation now lives in
//! `aghub_core::skills::update` so BOTH the api routes/update-check pipeline AND
//! the CLI `apply-update` command depend on a single source of truth (the CLI
//! previously had no rename guard because the predicate was api-crate-local).
//! This module re-exports them so the existing api call sites are unchanged.

pub use aghub_core::skills::update::{
	detect_rename, skill_renamed_message, SKILL_RENAMED_CODE,
};
