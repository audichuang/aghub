pub mod amp;
pub mod antigravity;
pub mod augmentcode;
pub mod claude;
pub mod cline;
pub mod codex;
pub mod copilot;
pub mod cursor;
pub mod factory;
pub mod gemini;
pub mod grok;
pub mod hermes;
pub mod jetbrains_ai;
pub mod kilocode;
pub mod kimi;
pub mod kiro;
pub mod mistral;
pub mod omp;
pub mod openclaw;
pub mod opencode;
pub mod pi;
pub mod roocode;
pub mod trae;
pub mod warp;
pub mod windsurf;
pub mod zed;

use crate::AgentDescriptor;

/// Every agent descriptor that ships, in the order the registry used to list
/// them. THE list — `aghub_core::registry::ALL_AGENTS` points here and the
/// descriptor matrix test derives from it, so "add an agent" cannot mean
/// "update three hand-written lists and hope". A `const` (not a `static`) so a
/// downstream `static` can initialise from it.
///
/// Membership is asserted bijective against `AgentType::ALL` in
/// `aghub-core/tests/registry_bijection.rs`; without that, an entry missing
/// here makes `registry::get` fall back to Claude's descriptor SILENTLY and
/// the new agent's MCPs land in `~/.claude.json`.
pub const ALL_DESCRIPTORS: &[&AgentDescriptor] = &[
	&claude::DESCRIPTOR,
	&codex::DESCRIPTOR,
	&openclaw::DESCRIPTOR,
	&opencode::DESCRIPTOR,
	&gemini::DESCRIPTOR,
	&cline::DESCRIPTOR,
	&copilot::DESCRIPTOR,
	&cursor::DESCRIPTOR,
	&antigravity::DESCRIPTOR,
	&kiro::DESCRIPTOR,
	&windsurf::DESCRIPTOR,
	&trae::DESCRIPTOR,
	&zed::DESCRIPTOR,
	&jetbrains_ai::DESCRIPTOR,
	&roocode::DESCRIPTOR,
	&kimi::DESCRIPTOR,
	&mistral::DESCRIPTOR,
	&pi::DESCRIPTOR,
	&augmentcode::DESCRIPTOR,
	&kilocode::DESCRIPTOR,
	&amp::DESCRIPTOR,
	&factory::DESCRIPTOR,
	&warp::DESCRIPTOR,
	&hermes::DESCRIPTOR,
	&grok::DESCRIPTOR,
	&omp::DESCRIPTOR,
];
