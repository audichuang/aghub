//! Credential DTO mappers for skill source fetching.
//!
//! Storage + resolution moved down to [`skill_update::credentials`] so the
//! CLI shares one credential store. What remains here is the API-only
//! projection of a source→credential binding into its ts-rs DTO
//! (`skill-update` has no ts-rs wiring), consumed by `routes::credentials`.

pub(crate) mod resolve;
