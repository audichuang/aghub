//! Regression tests for npx-`skills` layout path ownership.
//!
//! In the npx universal layout the Master lives at `<root>/.agents/skills/<name>`
//! and each agent owns ONLY its own per-agent skills dir (which holds symlink
//! Referrers to the Master). An agent must NEVER read another agent's private
//! skills dir — doing so makes it (a) discover skills it does not own, and (b)
//! plan destructive removals against another agent's content.
//!
//! Source of truth for the per-agent dirs: upstream `vercel-labs/skills`
//! `src/agents.ts` — cursor/opencode map to `.agents/skills` (project) + their
//! own global dir; neither reads `.claude/skills` or `.codex/skills`.

use aghub_agents::agents::{cursor, opencode};
use aghub_core::create_adapter;
use aghub_core::manager::ConfigManager;
use aghub_core::models::AgentType;
use std::path::{Path, PathBuf};

fn write_skill_md(dir: &Path, name: &str) {
	std::fs::create_dir_all(dir).unwrap();
	std::fs::write(
		dir.join("SKILL.md"),
		format!("---\nname: {name}\ndescription: d\n---\n"),
	)
	.unwrap();
}

#[cfg(unix)]
fn symlink(target: &Path, link: &Path) {
	std::os::unix::fs::symlink(target, link).unwrap();
}

fn contains_suffix(paths: &[PathBuf], suffix: &str) -> bool {
	paths.iter().any(|p| p.ends_with(suffix))
}

// ─── Path-policy tests (descriptor read paths) ──────────────────────────────

#[test]
fn cursor_project_read_paths_exclude_foreign_agent_dirs() {
	let root = Path::new("/tmp/proj");
	let paths = cursor::DESCRIPTOR.project_skill_read_paths(root);
	assert!(
		contains_suffix(&paths, ".cursor/skills"),
		"cursor must read its own dir: {paths:?}"
	);
	assert!(
		contains_suffix(&paths, ".agents/skills"),
		"cursor must read the universal master: {paths:?}"
	);
	assert!(
		!contains_suffix(&paths, ".claude/skills"),
		"cursor must NOT read Claude's private dir: {paths:?}"
	);
	assert!(
		!contains_suffix(&paths, ".codex/skills"),
		"cursor must NOT read Codex's private dir: {paths:?}"
	);
}

#[test]
fn cursor_global_read_paths_exclude_foreign_agent_dirs() {
	let paths = cursor::DESCRIPTOR.global_skill_read_paths();
	assert!(
		!contains_suffix(&paths, ".claude/skills"),
		"cursor global must NOT read Claude's private dir: {paths:?}"
	);
	assert!(
		!contains_suffix(&paths, ".codex/skills"),
		"cursor global must NOT read Codex's private dir: {paths:?}"
	);
}

#[test]
fn opencode_project_read_paths_exclude_foreign_agent_dirs() {
	let root = Path::new("/tmp/proj");
	let paths = opencode::DESCRIPTOR.project_skill_read_paths(root);
	assert!(
		contains_suffix(&paths, ".opencode/skills"),
		"opencode must read its own dir: {paths:?}"
	);
	assert!(
		contains_suffix(&paths, ".agents/skills"),
		"opencode must read the universal master: {paths:?}"
	);
	assert!(
		!contains_suffix(&paths, ".claude/skills"),
		"opencode must NOT read Claude's private dir: {paths:?}"
	);
}

#[test]
fn opencode_global_read_paths_exclude_foreign_agent_dirs() {
	let paths = opencode::DESCRIPTOR.global_skill_read_paths();
	assert!(
		!contains_suffix(&paths, ".claude/skills"),
		"opencode global must NOT read Claude's private dir: {paths:?}"
	);
}

// ─── Behavioral: discovery must not cross agent boundaries ──────────────────

/// A skill that exists ONLY in `.claude/skills` (a real, Claude-owned dir, never
/// installed universally) must be invisible to Cursor.
#[test]
fn cursor_does_not_discover_claude_only_skill() {
	let tmp = tempfile::tempdir().unwrap();
	let root = tmp.path();
	write_skill_md(&root.join(".claude/skills/claude-only"), "claude-only");

	let mut mgr = ConfigManager::new(
		create_adapter(AgentType::Cursor),
		false,
		Some(root),
	);
	mgr.load().unwrap();

	assert!(
		mgr.get_skill("claude-only").is_none(),
		"Cursor must not discover a Claude-only skill living in .claude/skills"
	);
}

/// A universal skill (Master in `.agents/skills`, Claude Referrer symlink) must
/// be discovered by OpenCode via the Master — NOT misattributed through Claude's
/// symlink. The discovered `source_path` must never point into `.claude/skills`.
#[cfg(unix)]
#[test]
fn opencode_does_not_misattribute_claude_symlink() {
	let tmp = tempfile::tempdir().unwrap();
	let root = tmp.path();
	let master = root.join(".agents/skills/shared");
	write_skill_md(&master, "shared");
	std::fs::create_dir_all(root.join(".claude/skills")).unwrap();
	symlink(&master, &root.join(".claude/skills/shared"));

	let mut mgr = ConfigManager::new(
		create_adapter(AgentType::OpenCode),
		false,
		Some(root),
	);
	mgr.load().unwrap();

	let skill = mgr.get_skill("shared").expect(
		"OpenCode should discover the universal master via .agents/skills",
	);
	let source = skill.source_path.clone().unwrap_or_default();
	assert!(
		!source.contains(".claude/skills"),
		"OpenCode must not source a skill from Claude's dir, got: {source}"
	);
}

// ─── Behavioral: delete must not destroy another agent's content ────────────

/// Deleting via Cursor must never plan removal of a Claude-only skill. After the
/// fix Cursor cannot even discover it, so the planner reports not-found and the
/// Claude-owned directory survives untouched.
#[test]
fn cursor_delete_does_not_remove_claude_only_skill() {
	let tmp = tempfile::tempdir().unwrap();
	let root = tmp.path();
	let claude_skill = root.join(".claude/skills/claude-only");
	write_skill_md(&claude_skill, "claude-only");

	let mut mgr = ConfigManager::new(
		create_adapter(AgentType::Cursor),
		false,
		Some(root),
	);
	mgr.load().unwrap();

	// Cursor does not own this skill — a delete request must error, not plan a
	// destructive removal of Claude's directory.
	let res = mgr.remove_skill_planned("claude-only", false, true, false);
	assert!(
		res.is_err(),
		"Cursor must not be able to plan removal of a Claude-only skill"
	);
	assert!(
		claude_skill.join("SKILL.md").exists(),
		"the Claude-owned skill directory must survive"
	);
}

/// A single-agent delete of a shared universal Master (another agent still
/// references it) must be reference-counted: the Master is KEPT and reported in
/// `skipped` — never silently dropped from output, never placed in `paths`.
#[cfg(unix)]
#[test]
fn single_agent_delete_keeps_shared_master_and_reports_it() {
	let tmp = tempfile::tempdir().unwrap();
	let root = tmp.path();
	let master = root.join(".agents/skills/shared");
	write_skill_md(&master, "shared");
	// Claude references the master via a symlink — so it is shared.
	std::fs::create_dir_all(root.join(".claude/skills")).unwrap();
	symlink(&master, &root.join(".claude/skills/shared"));

	// Codex reads the master directly (its project skills dir IS .agents/skills).
	let mut mgr =
		ConfigManager::new(create_adapter(AgentType::Codex), false, Some(root));
	mgr.load().unwrap();
	assert!(
		mgr.get_skill("shared").is_some(),
		"Codex should discover the shared master via .agents/skills"
	);

	let outcome = mgr
		.remove_skill_planned("shared", false, true, false)
		.unwrap();
	assert!(
		!outcome.plan.paths.contains(&master),
		"a shared master must never be scheduled for single-agent removal"
	);
	assert!(
		outcome.plan.skipped.iter().any(|p| p == &master),
		"the kept master must be reported in `skipped`, not silently no-op'd: {:?}",
		outcome.plan
	);
	assert!(
		master.join("SKILL.md").exists(),
		"the shared master must survive the planned delete"
	);
}
