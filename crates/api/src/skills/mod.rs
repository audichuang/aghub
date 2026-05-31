//! Skill-specific server-side orchestration (not the HTTP routes, which live in
//! `routes::skills`). Houses the F1 update-check pipeline.

// The orchestration is consumed by the `GET /skills/check-updates` route (Task
// F1.7). Until that route lands, allow dead code so the blocking
// `clippy -D warnings` lane stays green.
#[allow(dead_code)]
pub(crate) mod update_check;
