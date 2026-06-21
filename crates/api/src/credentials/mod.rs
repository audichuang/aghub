//! Credential helpers for skill source fetching.
//!
//! Resolution lives here (in `crates/api`) so that `crates/core` stays pure:
//! core receives an already-resolved `Option<token>` and never touches the
//! keyring or the network.

// The resolver and binding store are consumed by the update-check orchestration
// (Task F1.5) via the `routes::skills_update` route.
pub(crate) mod resolve;

// Forwarded git-credential primitives (resolver + request guard + chain).
// Built here as reusable pieces; route/CORS wiring lands in a later task, so
// the items are not yet referenced from any route.
#[allow(dead_code)]
pub(crate) mod forwarding;
