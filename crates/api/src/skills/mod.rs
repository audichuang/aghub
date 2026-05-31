//! Skill-specific server-side orchestration (not the HTTP routes, which live in
//! `routes::skills`). Houses the F1 update-check pipeline.

// The orchestration is consumed by the `GET /skills/check-updates` route
// (`routes::skills_update`).
pub(crate) mod update_check;
