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
	if let Ok(c) = std::fs::canonicalize(p) {
		return c;
	}
	// `p` (or its leaf) may not exist yet — e.g. `<root>/.agents/skills` before
	// any install. Plain canonicalize() then fails and leaves the raw path,
	// which won't match a canonicalized counterpart (macOS `/var`->`/private`,
	// Windows 8.3 short names / `\\?\` UNC). Canonicalize the longest EXISTING
	// ancestor and re-append the non-existent remainder so both sides normalize
	// identically.
	let mut ancestor = p;
	let mut tail: Vec<std::ffi::OsString> = Vec::new();
	loop {
		match std::fs::canonicalize(ancestor) {
			Ok(mut out) => {
				for part in tail.iter().rev() {
					out.push(part);
				}
				return out;
			}
			Err(_) => match (ancestor.parent(), ancestor.file_name()) {
				(Some(parent), Some(name)) => {
					tail.push(name.to_os_string());
					ancestor = parent;
				}
				_ => return p.to_path_buf(),
			},
		}
	}
}

/// Path-only classification: the `(reads_master, writes_master, need)` facts for
/// one agent against a scope, WITHOUT the agent-availability subprocess probe.
/// Shared by [`classify_agent`] (which adds `installed`) and [`agent_link_need`]
/// (the install path, which only needs the `need` and must stay cheap).
fn classify_paths(
	descriptor: &AgentDescriptor,
	scope: ResourceScope,
	project_root: Option<&Path>,
	master_skills_dir: &Path,
) -> (bool, bool, LinkNeed) {
	let (read_paths, write_dir) = match AgentType::from_str(descriptor.id) {
		Ok(agent_type) => {
			let adapter = crate::create_adapter(agent_type);
			(
				adapter.get_skills_paths(project_root, scope),
				adapter.target_skills_dir(project_root, scope),
			)
		}
		// Defensive fallback for unknown ids (all 23 registry ids resolve via
		// from_str today); uses descriptor paths directly, intentionally
		// bypassing the SKILLS_PATH_OVERRIDE test hook.
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

	(reads_master, writes_master, need)
}

/// The [`LinkNeed`] for one agent at a scope, WITHOUT the availability probe.
/// Use this on the install path where only the need (link vs skip) matters; use
/// [`classify_agent`] when the `installed`/diagnostic booleans are also needed.
pub fn agent_link_need(
	descriptor: &AgentDescriptor,
	scope: ResourceScope,
	project_root: Option<&Path>,
	master_skills_dir: &Path,
) -> LinkNeed {
	classify_paths(descriptor, scope, project_root, master_skills_dir).2
}

pub fn classify_agent(
	descriptor: &AgentDescriptor,
	scope: ResourceScope,
	project_root: Option<&Path>,
	master_skills_dir: &Path,
) -> AgentLinkPlan {
	let (reads_master, writes_master, need) =
		classify_paths(descriptor, scope, project_root, master_skills_dir);

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
	crate::registry::ALL_AGENTS
		.iter()
		.map(|descriptor| {
			classify_agent(descriptor, scope, project_root, master_skills_dir)
		})
		.collect()
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

	#[test]
	fn amp_kimi_global_needs_link_but_project_is_native() {
		// At GLOBAL scope the universal flag appends the XDG dir
		// (~/.config/agents/skills), NOT ~/.agents/skills, so Amp/Kimi do NOT
		// read the global master and must be linked.
		for id in ["amp", "kimi"] {
			let plan = plan_for(id, ResourceScope::GlobalOnly, None);
			assert!(
				matches!(plan.need, LinkNeed::NeedsLink { .. }),
				"{id} @global should NeedsLink (XDG != ~/.agents/skills), \
				 got {:?}",
				plan.need
			);
		}

		// At PROJECT scope the universal flag appends
		// project_root/.agents/skills == the canonical master, so they ARE
		// NativeReaders.
		let tmp = tempfile::tempdir().unwrap();
		let root = std::fs::canonicalize(tmp.path()).unwrap();
		for id in ["amp", "kimi"] {
			let plan =
				plan_for(id, ResourceScope::ProjectOnly, Some(root.as_path()));
			assert_eq!(
				plan.need,
				LinkNeed::NativeReader,
				"{id} @project should be NativeReader (project .agents/skills \
				 == canonical)"
			);
		}
	}

	#[test]
	fn agent_without_skill_support_is_unsupported() {
		// jetbrains-ai has no skills scopes => no write dir, no master read.
		let plan = plan_for("jetbrains-ai", ResourceScope::GlobalOnly, None);
		assert_eq!(
			plan.need,
			LinkNeed::Unsupported,
			"jetbrains-ai @global should be Unsupported (no skills dir)"
		);
		let plan_p = {
			let tmp = tempfile::tempdir().unwrap();
			let root = std::fs::canonicalize(tmp.path()).unwrap();
			plan_for(
				"jetbrains-ai",
				ResourceScope::ProjectOnly,
				Some(root.as_path()),
			)
		};
		assert_eq!(
			plan_p.need,
			LinkNeed::Unsupported,
			"jetbrains-ai @project should be Unsupported"
		);
	}

	fn assert_totality(plans: &[AgentLinkPlan]) {
		assert_eq!(
			plans.len(),
			registry::ALL_AGENTS.len(),
			"classify_all must cover every registered agent"
		);
		for plan in plans {
			let auto_covered = matches!(plan.need, LinkNeed::NativeReader);
			let needs_link = matches!(plan.need, LinkNeed::NeedsLink { .. });
			let unsupported = matches!(plan.need, LinkNeed::Unsupported);
			let count = [auto_covered, needs_link, unsupported]
				.iter()
				.filter(|b| **b)
				.count();
			assert_eq!(
				count, 1,
				"agent {} must be in exactly one bucket, got {:?}",
				plan.agent_id, plan.need
			);
		}
	}

	#[test]
	fn classify_all_is_total_at_global() {
		let master = universal_canonical_dir(None).unwrap();
		let plans = classify_all(ResourceScope::GlobalOnly, None, &master);
		assert_totality(&plans);
	}

	#[test]
	fn classify_all_is_total_at_project() {
		let tmp = tempfile::tempdir().unwrap();
		let root = std::fs::canonicalize(tmp.path()).unwrap();
		let master = universal_canonical_dir(Some(root.as_path())).unwrap();
		let plans = classify_all(
			ResourceScope::ProjectOnly,
			Some(root.as_path()),
			&master,
		);
		assert_totality(&plans);
	}

	#[test]
	fn classify_canonicalizes_both_sides() {
		let tmp = tempfile::tempdir().unwrap();
		// Deliberately use the RAW (possibly /var-symlinked) temp path as the
		// project_root, but a CANONICALIZED master skills-dir.
		let raw_root = tmp.path();
		let canon_root = std::fs::canonicalize(raw_root).unwrap();
		let master =
			universal_canonical_dir(Some(canon_root.as_path())).unwrap();
		let codex = registry::ALL_AGENTS
			.iter()
			.find(|d| d.id == "codex")
			.unwrap();
		let plan = classify_agent(
			codex,
			ResourceScope::ProjectOnly,
			Some(raw_root),
			&master,
		);
		assert_eq!(
			plan.need,
			LinkNeed::NativeReader,
			"codex @project must be NativeReader even when project_root is the \
			 raw (un-canonicalized) temp path and master is canonicalized"
		);
	}
}
