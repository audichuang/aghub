//! The environment variables that move an agent's config off its default path.
//!
//! Every descriptor that honours a private home/config variable reads it from
//! this list's vocabulary, and those variables outrank `$HOME` — so a test that
//! resolves ANY global agent path must clear all of them first, or it silently
//! reads and writes the developer's real config. That has happened three times
//! in three different test harnesses (an ambient `OPENCODE_CONFIG_DIR` wrote MCP
//! servers into a live `opencode.json`; a real `~/.config/orca/...` turned up in
//! an api test's allow-listed roots), each time because the harness kept its own
//! hand-copied list and missed a variable.
//!
//! So there is ONE list, here, next to the descriptors that read it. Isolating
//! `$HOME` alone is NOT isolation.

/// Variables a descriptor consults ahead of `$HOME` when resolving its global
/// config or skills directory.
///
/// Adding a descriptor that reads a NEW variable means adding it here too;
/// `descriptor_regression.rs`'s `path_override_vars_covers_every_descriptor_read`
/// fails if you don't, by reading the descriptor sources.
pub const PATH_OVERRIDE_VARS: &[&str] = &[
	"OPENCODE_CONFIG",
	"OPENCODE_CONFIG_DIR",
	"XDG_CONFIG_HOME",
	"CODEX_HOME",
	"COPILOT_HOME",
	"KIMI_SHARE_DIR",
	"VIBE_HOME",
	"HERMES_HOME",
	"GROK_HOME",
	"OPENCLAW_CONFIG_PATH",
	"OPENCLAW_STATE_DIR",
];
