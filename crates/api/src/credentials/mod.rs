//! Credential helpers for skill source fetching.
//!
//! Resolution lives here (in `crates/api`) so that `crates/core` stays pure:
//! core receives an already-resolved `Option<token>` and never touches the
//! keyring or the network.

// The resolver and binding store are consumed by the update-check orchestration
// (Task F1.5) via the `routes::skills_update` route.
pub(crate) mod resolve;

// Normalized `(scheme, host, port)` origin used to pin a credential/token to
// the exact place it came from. Reused by the skills git-scan host guard.
pub(crate) mod origin;

// Controller-side public resolution API re-exported from `lib.rs` for the
// desktop `src-tauri` layer (remote git-credential forwarding).
pub(crate) mod public;

// Forwarded git-credential primitives (resolver + request guard + chain).
// Wired into the sources/diff, check-updates, apply-update, and git-scan
// routes (remote git-credential forwarding).
pub(crate) mod forwarding;
