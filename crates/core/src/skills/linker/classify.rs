//! Agent auto-classification for symlink-only skill install.
//!
//! Derives — purely from each agent's RESOLVED read/write skills-dir paths,
//! never from `capabilities.skills.universal` and never from a hardcoded
//! agent list — whether an agent already reads the `.agents/skills` master
//! (NativeReader, no link needed), needs a per-agent link (NeedsLink), or
//! cannot hold skills at this scope (Unsupported).
//!
//! All comparisons are SKILLS-DIR vs SKILLS-DIR: `master_skills_dir` is the
//! `.agents/skills` store, NOT the `.agents/skills/<name>` skill-dir.

use crate::AgentType;
use aghub_agents::{AgentDescriptor, ResourceScope};
use std::path::{Path, PathBuf};
use std::str::FromStr;

/// Whether an agent needs a per-agent link to the `.agents/skills` master.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkNeed {
	/// Agent's own skills-dir at this scope IS or already READS
	/// `.agents/skills`: sees the master directly, NO link required.
	NativeReader,
	/// Agent has a private skills-dir not mapped to the master: needs a link.
	NeedsLink { agent_skills_dir: PathBuf },
	/// Agent's skills-dir cannot be resolved for this scope.
	Unsupported,
}

/// One agent's classification result for a given scope.
///
/// `reads_master` / `writes_master` are the REAL facts computed by the
/// classifier (does this agent's resolved read/write skills-dir resolve to the
/// `.agents/skills` master?), surfaced so the coverage DTO can report accurate
/// diagnostics (P2-G) instead of guessing. `need` is the 3-state derived from
/// them; the FE partitions on `need`, but the booleans are honest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentLinkPlan {
	pub agent_id: &'static str,
	pub need: LinkNeed,
	pub installed: bool,
	pub reads_master: bool,
	pub writes_master: bool,
}

/// Classify ONE agent against a scope + project_root + the canonical master
/// SKILLS-DIR (`.agents/skills`).
fn canonicalize_lenient(p: &Path) -> PathBuf {
	std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

pub fn classify_agent(
	descriptor: &AgentDescriptor,
	scope: ResourceScope,
	project_root: Option<&Path>,
	master_skills_dir: &Path,
) -> AgentLinkPlan {
	let (read_paths, write_dir) = match AgentType::from_str(descriptor.id) {
		Ok(agent_type) => {
			let adapter = crate::create_adapter(agent_type);
			(
				adapter.get_skills_paths(project_root, scope),
				adapter.target_skills_dir(project_root, scope),
			)
		}
		Err(_) => (
			descriptor.skill_read_paths(project_root, scope),
			descriptor.skill_write_path(project_root, scope),
		),
	};

	let canon = canonicalize_lenient(master_skills_dir);
	let reads_master =
		read_paths.iter().any(|p| canonicalize_lenient(p) == canon);
	let writes_master =
		write_dir.as_ref().map(|p| canonicalize_lenient(p)) == Some(canon);

	let need = if reads_master || writes_master {
		LinkNeed::NativeReader
	} else if let Some(dir) = write_dir {
		LinkNeed::NeedsLink {
			agent_skills_dir: dir,
		}
	} else {
		LinkNeed::Unsupported
	};

	AgentLinkPlan {
		agent_id: descriptor.id,
		need,
		installed: crate::availability::check_agent_availability(descriptor)
			.is_available,
		reads_master,
		writes_master,
	}
}

/// Classify ALL registered agents (`registry::ALL_AGENTS`).
pub fn classify_all(
	scope: ResourceScope,
	project_root: Option<&Path>,
	master_skills_dir: &Path,
) -> Vec<AgentLinkPlan> {
	let _ = (scope, project_root, master_skills_dir);
	unimplemented!()
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::registry;
	use crate::skills::linker::universal_canonical_dir;

	fn plan_for(
		id: &str,
		scope: ResourceScope,
		project_root: Option<&Path>,
	) -> AgentLinkPlan {
		let master = universal_canonical_dir(project_root).unwrap();
		let descriptor = registry::ALL_AGENTS
			.iter()
			.find(|d| d.id == id)
			.unwrap_or_else(|| panic!("no descriptor for {id}"));
		classify_agent(descriptor, scope, project_root, &master)
	}

	#[test]
	fn codex_global_is_native_reader() {
		let plan = plan_for("codex", ResourceScope::GlobalOnly, None);
		assert_eq!(
			plan.need,
			LinkNeed::NativeReader,
			"codex reads ~/.agents/skills at global"
		);
		assert_eq!(plan.agent_id, "codex");
	}

	#[test]
	fn global_native_reader_set_matches_descriptors() {
		// Oracle (the AGENTS.md-documented global native set). This list is
		// the TEST expectation only — the impl derives it from descriptors.
		let expected_native = ["codex", "opencode", "cursor", "cline", "warp"];
		for id in expected_native {
			let plan = plan_for(id, ResourceScope::GlobalOnly, None);
			assert_eq!(
				plan.need,
				LinkNeed::NativeReader,
				"{id} should be a global NativeReader"
			);
		}
		// A clear non-native agent at global: Claude reads only
		// ~/.claude/skills.
		let claude = plan_for("claude", ResourceScope::GlobalOnly, None);
		assert!(
			matches!(claude.need, LinkNeed::NeedsLink { .. }),
			"claude @global should NeedsLink, got {:?}",
			claude.need
		);
	}
}
