//! Credential helpers for skill source fetching.
//!
//! Resolution lives here (in `crates/api`) so that `crates/core` stays pure:
//! core receives an already-resolved `Option<token>` and never touches the
//! keyring or the network.

// The resolver and binding store are consumed by the update-check orchestration
// (Task F1.5). Until that lands, mark as allowed dead code so the blocking
// `clippy -D warnings` lane stays green.
#[allow(dead_code)]
pub(crate) mod resolve;
