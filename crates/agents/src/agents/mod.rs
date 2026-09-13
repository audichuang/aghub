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

/// Declare the agent roster ONCE and generate everything that used to be a
/// separate hand-written list from it: the `AgentType` enum, `AgentType::ALL`,
/// `as_str`, `FromStr` (ids plus aliases), `AgentType::descriptor`, and
/// `ALL_DESCRIPTORS`.
///
/// Four lists is how "add an agent" became "update three of them and hope":
/// a variant with no descriptor entry was served **Claude's** descriptor by
/// `registry::get` — silently, so its MCP servers landed in `~/.claude.json`
/// and its skills in Claude's directory. One row per agent makes the pairing
/// structural: `AgentType::descriptor` is a total `match`, so `registry::get`
/// has no fallback left to reach.
///
/// What the macro still CANNOT see, and `registry_bijection.rs` therefore
/// still owns: only `$variant` is compiler-checked. Two copy-paste mistakes
/// build clean — a row naming ANOTHER agent's module (that agent is then
/// served the wrong descriptor: the old fallback bug, by hand), and an `$id`
/// that drifts from the `id:` field inside `agents/<module>.rs`.
///
/// A repeated STRING — one row's id or alias equal to another's — is the one
/// the compiler catches unaided: `from_str` emits the arms in row order, so
/// the loser is an `unreachable_patterns` warning, which `just preflight`'s
/// `clippy -D warnings` turns into a failure.
///
/// The order of these rows is the order of BOTH rosters — the desktop agent
/// list, `-a all` expansion, batch row order and first-error all read it.
macro_rules! agent_roster {
	($(
		$variant:ident => $id:literal, $module:ident, [$($alias:literal),* $(,)?];
	)+) => {
		/// Agent types supported by the system
		#[derive(Debug, Clone, Copy, PartialEq, Eq)]
		pub enum AgentType {
			$($variant,)+
		}

		impl AgentType {
			/// Every agent, in roster order. Paired with [`ALL_DESCRIPTORS`]
			/// index for index — both come from the same declaration.
			pub const ALL: &[AgentType] = &[$(AgentType::$variant,)+];

			pub fn as_str(&self) -> &'static str {
				match self {
					$(AgentType::$variant => $id,)+
				}
			}

			/// This agent's descriptor. Total by construction — the reason
			/// `aghub_core::registry::get` needs no fallback.
			pub fn descriptor(&self) -> &'static AgentDescriptor {
				match self {
					$(AgentType::$variant => &$module::DESCRIPTOR,)+
				}
			}
		}

		impl std::str::FromStr for AgentType {
			type Err = String;

			fn from_str(s: &str) -> Result<Self, Self::Err> {
				match s.to_lowercase().as_str() {
					$($id $(| $alias)* => Ok(AgentType::$variant),)+
					_ => Err(format!("Unknown agent type: {s}")),
				}
			}
		}

		/// Every agent descriptor that ships, in roster order. THE list —
		/// `aghub_core::registry::ALL_AGENTS` points here and the descriptor
		/// matrix test derives from it. A `const` (not a `static`) so a
		/// downstream `static` can initialise from it.
		pub const ALL_DESCRIPTORS: &[&AgentDescriptor] =
			&[$(&$module::DESCRIPTOR,)+];

		/// Every declared alias with the variant it resolves to. Generated
		/// so the alias test reads the SAME rows `from_str` was built from —
		/// a hand-copied list would have to be kept in step with these rows
		/// by hand, which is the duplication this macro exists to delete.
		#[cfg(test)]
		pub(crate) const ROSTER_ALIASES: &[(&str, AgentType)] =
			&[$($(($alias, AgentType::$variant),)*)+];
	};
}

// Variant => id, descriptor module, [`from_str` aliases beyond the id].
// Order is claude-first, matching the desktop's agent list.
agent_roster! {
	Claude      => "claude",       claude,       [];
	Codex       => "codex",        codex,        [];
	Openclaw    => "openclaw",     openclaw,     [];
	OpenCode    => "opencode",     opencode,     [];
	Gemini      => "gemini",       gemini,       [];
	Cline       => "cline",        cline,        [];
	Copilot     => "copilot",      copilot,      [];
	Cursor      => "cursor",       cursor,       [];
	Antigravity => "antigravity",  antigravity,  [];
	Kiro        => "kiro",         kiro,         [];
	Windsurf    => "windsurf",     windsurf,     [];
	Trae        => "trae",         trae,         [];
	Zed         => "zed",          zed,          [];
	JetBrainsAi => "jetbrains-ai", jetbrains_ai, ["jetbrains", "jb"];
	RooCode     => "roocode",      roocode,      ["roo"];
	Kimi        => "kimi",         kimi,         ["kimi-cli"];
	Mistral     => "mistral",      mistral,      [];
	Pi          => "pi",           pi,           [];
	AugmentCode => "augmentcode",  augmentcode,  ["augment"];
	KiloCode    => "kilocode",     kilocode,     ["kilo"];
	Amp         => "amp",          amp,          [];
	Factory     => "factory",      factory,      [];
	Warp        => "warp",         warp,         [];
	Hermes      => "hermes",       hermes,       [];
	Grok        => "grok",         grok,         [];
	Omp         => "omp",          omp,          ["oh-my-pi"];
}
