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

/// Two NativeReaders sharing one Master: removing it for ONE of them must not
/// take the Master away from the other.
///
/// The existing reference-count guard only sweeps for symlink Referrers, and a
/// NativeReader leaves none — so with two of them the sweep found nothing and
/// `remove_dir_all`'d the shared Master while reporting success. The other
/// agent silently lost the skill. Every agent here reads `.agents/skills`
/// directly, so no symlink exists anywhere in the fixture: that is the point.
#[test]
fn single_agent_delete_keeps_master_shared_by_another_native_reader() {
	let tmp = tempfile::tempdir().unwrap();
	let root = tmp.path();
	let master = root.join(".agents/skills/shared");
	write_skill_md(&master, "shared");

	// Both Cursor and OpenCode read `<project>/.agents/skills` directly.
	for agent in [AgentType::Cursor, AgentType::OpenCode] {
		let mut mgr =
			ConfigManager::new(create_adapter(agent), false, Some(root));
		mgr.load().unwrap();
		assert!(
			mgr.get_skill("shared").is_some(),
			"{agent:?} should discover the shared master"
		);
	}

	let mut mgr = ConfigManager::new(
		create_adapter(AgentType::Cursor),
		false,
		Some(root),
	);
	mgr.load().unwrap();
	// Dry-run: previews the plan (Master kept + reported). An EXECUTING call
	// refuses instead — pinned by
	// `confirmed_single_agent_delete_of_a_shared_master_errors`.
	let outcome = mgr
		.remove_skill_planned("shared", false, true, false)
		.unwrap();

	assert!(
		!outcome.plan.paths.contains(&master),
		"the shared master must never be scheduled for a single-agent removal"
	);
	assert!(
		outcome.plan.shared_master_kept,
		"the plan must record WHY nothing would be removed, so a caller can \
		 refuse instead of reporting a removal that did not happen: {:?}",
		outcome.plan
	);
	assert!(
		master.join("SKILL.md").exists(),
		"the master must survive — the other native reader still uses it"
	);

	// And the other agent still sees it.
	let mut other = ConfigManager::new(
		create_adapter(AgentType::OpenCode),
		false,
		Some(root),
	);
	other.load().unwrap();
	assert!(
		other.get_skill("shared").is_some(),
		"OpenCode must not lose a skill because Cursor asked to drop it"
	);
}

/// A CONFIRMED single-agent delete that can remove nothing must fail, not
/// report success.
///
/// `RemovalView` hardcodes `success: true`, so an executing call that removed
/// nothing exited 0 saying it "removed no installed files" while the skill was
/// still there. The dry-run keeps its preview contract (see the test above);
/// only the executing call refuses.
#[test]
fn confirmed_single_agent_delete_of_a_shared_master_errors() {
	let tmp = tempfile::tempdir().unwrap();
	let root = tmp.path();
	let master = root.join(".agents/skills/shared");
	write_skill_md(&master, "shared");

	let mut mgr = ConfigManager::new(
		create_adapter(AgentType::Cursor),
		false,
		Some(root),
	);
	mgr.load().unwrap();

	// dry-run still previews
	let preview = mgr
		.remove_skill_planned("shared", false, true, false)
		.expect("a dry-run must still preview the plan");
	assert!(preview.plan.skipped.iter().any(|p| p == &master));

	// executing refuses
	let error = mgr
		.remove_skill_planned("shared", false, false, true)
		.expect_err("an executing delete that removes nothing must fail");
	assert!(
		error.to_string().contains("shared master"),
		"the error must say why, got: {error}"
	);
	assert!(master.join("SKILL.md").exists());
}

/// An agent that owns a Referrer AND reads the Master loses NOTHING by giving
/// up the Referrer, so the delete must refuse instead of reporting `removed`.
///
/// This shape has a real path to unlink, so the plan looks effective and the
/// old `paths.is_empty() && shared_master_kept` guard let it straight through:
/// `delete skills mover -a opencode --yes` unlinked `.opencode/skills/mover`,
/// exited 0 with `outcome: "removed"`, and opencode went on reading the very
/// same skill from `.agents/skills`. `transfer::reconcile_skill` refused the
/// identical shape, so the two surfaces answered the same question opposite
/// ways — this pins them to the one answer.
///
/// aghub does not create that layout today (a NativeReader gets no Referrer),
/// but `npx skills` and older aghub releases did.
#[cfg(unix)]
#[test]
fn delete_refuses_when_the_agent_keeps_reading_it_from_the_master() {
	let tmp = tempfile::tempdir().unwrap();
	let root = tmp.path();
	let master = root.join(".agents/skills/mover");
	write_skill_md(&master, "mover");
	// opencode reads `.opencode/skills` FIRST and `.agents/skills` second.
	let referrer = root.join(".opencode/skills/mover");
	std::fs::create_dir_all(referrer.parent().unwrap()).unwrap();
	symlink(&master, &referrer);

	let mut mgr = ConfigManager::new(
		create_adapter(AgentType::OpenCode),
		false,
		Some(root),
	);
	mgr.load().unwrap();

	// Disk state is asserted BEFORE the return value: the regression is a
	// removal reported as done, not a different `Result` shape.
	let outcome = mgr.remove_skill_planned("mover", false, false, true);

	assert!(
		std::fs::symlink_metadata(&referrer).is_ok(),
		"nothing may be unlinked for a removal that takes nothing away"
	);
	assert!(master.join("SKILL.md").exists(), "the Master must survive");
	let error = outcome.expect_err(
		"opencode still reads 'mover' from the Master, so removing its \
		 Referrer alone must be refused",
	);
	assert!(
		error.to_string().contains("shared master"),
		"the error must say why, got: {error}"
	);
}

/// A skill whose FOLDER name differs from its frontmatter `name` must not be
/// reported as removed when the agent still reads it afterwards.
///
/// The planner used to match folders by `sanitize_name(skill.name)` ALONE, so
/// for a Master at `.agents/skills/dirname` declaring `name: realname` it found
/// no folder to touch at all: `paths: []`, `shared_master_kept: false`. The old
/// guard needed BOTH halves, so it did not fire, and `delete --yes` returned
/// `outcome: "removed"` with every file still on disk. `npx skills` and older
/// aghub releases both wrote that layout. The planner now also asks discovery
/// (see `candidate_entries`), so the Referrer IS planned — and this stays a
/// refusal for the other reason: unlinking it leaves opencode reading the very
/// same skill from the Master.
#[cfg(unix)]
#[test]
fn delete_refuses_a_plan_that_would_touch_nothing_at_all() {
	let tmp = tempfile::tempdir().unwrap();
	let root = tmp.path();
	let master = root.join(".agents/skills/dirname");
	write_skill_md(&master, "realname");
	let referrer = root.join(".opencode/skills/dirname");
	std::fs::create_dir_all(referrer.parent().unwrap()).unwrap();
	symlink(&master, &referrer);

	let mut mgr = ConfigManager::new(
		create_adapter(AgentType::OpenCode),
		false,
		Some(root),
	);
	mgr.load().unwrap();
	assert!(
		mgr.get_skill("realname").is_some(),
		"fixture: the skill must be discoverable by its frontmatter name"
	);

	let outcome = mgr.remove_skill_planned("realname", false, false, true);

	assert!(
		master.join("SKILL.md").exists(),
		"nothing was planned, so nothing may be gone"
	);
	outcome.expect_err(
		"a removal that touches no path at all is not a removal and must not \
		 be reported as one",
	);
}

/// The refusal must not become a way to make a real removal impossible.
///
/// The check it replaced probed `read_dir.join(sanitize_name(name))` for mere
/// EXISTENCE, so an empty folder that happens to carry the name — no `SKILL.md`,
/// invisible to every agent — read as "still installed" and would have blocked
/// the removal of the actual skill forever. Asking discovery instead makes the
/// empty folder exactly what it is: nothing.
#[cfg(unix)]
#[test]
fn delete_still_works_with_an_empty_lookalike_folder_in_a_read_dir() {
	let tmp = tempfile::tempdir().unwrap();
	let root = tmp.path();
	// opencode reads BOTH of these dirs — that is what makes the lookalike
	// reachable by the check at all.
	let owned = root.join(".opencode/skills/solo");
	write_skill_md(&owned, "solo");
	// A same-named folder with no SKILL.md in the shared Master store.
	std::fs::create_dir_all(root.join(".agents/skills/solo")).unwrap();

	let mut mgr = ConfigManager::new(
		create_adapter(AgentType::OpenCode),
		false,
		Some(root),
	);
	mgr.load().unwrap();

	mgr.remove_skill_planned("solo", false, false, true)
		.expect("an empty lookalike folder holds no skill and must not veto");
	assert!(
		!owned.exists(),
		"opencode's real copy must actually be gone"
	);
}

/// Removing the skill from EVERY agent that holds it must take the Master too.
///
/// Per-agent removal refuses for a NativeReader, so an exhaustive reconcile
/// would fail on every target and orphan the Master. The desktop's
/// manage-agents dialog allows exactly this shape (deselect all, no adds).
#[test]
fn reconcile_removing_every_holder_takes_the_master() {
	use aghub_core::transfer::{
		reconcile_skill, InstallScope, ResourceLocator,
	};

	let tmp = tempfile::tempdir().unwrap();
	let root = tmp.path();
	let master = root.join(".agents/skills/shared");
	write_skill_md(&master, "shared");

	// EVERY native reader of `<project>/.agents/skills` holds this skill, not
	// just the two obvious ones — computed rather than hardcoded so a change to
	// the agent roster cannot silently make this test non-exhaustive (and thus
	// stop covering the branch it exists for).
	let holders: Vec<AgentType> = aghub_core::load_all_agents(
		aghub_core::models::ResourceScope::ProjectOnly,
		Some(root),
	)
	.into_iter()
	.filter(|a| a.skills.iter().any(|s| s.name == "shared"))
	.filter_map(|a| a.agent_id.parse::<AgentType>().ok())
	.collect();
	assert!(
		holders.len() > 1,
		"fixture must have several native readers, got {holders:?}"
	);

	let result = reconcile_skill(
		ResourceLocator {
			agent: AgentType::Cursor,
			scope: InstallScope::Project,
			project_root: Some(root.to_path_buf()),
			name: "shared".to_string(),
		},
		vec![],
		holders.clone(),
		true,
	)
	.expect("an exhaustive removal must be allowed");

	assert_eq!(
		result.failed_count(),
		0,
		"no target may fail: {:?}",
		result.results.iter().map(|r| &r.error).collect::<Vec<_>>()
	);
	assert!(
		!master.exists(),
		"the last reader is gone, so the Master must go with it"
	);
}

/// `--all-agents` means EVERY agent, including one whose copy is not at
/// `<skills-root>/<sanitized-name>`.
///
/// Both planners swept that slot alone — the only place aghub itself installs.
/// A grouped/renamed layout (`npx skills` and older aghub releases wrote them;
/// discovery recurses into them) sits somewhere else, so the sweep never saw
/// it: `delete skills foo --all-agents --yes` deleted Claude's copy, reported
/// `executed: true` with `outcome: "removed"`, and OpenCode went on discovering
/// `foo` from `.opencode/skills/team/legacy-dir` — the round-2 blocker's shape
/// (both rows successful, the target state not reached) on the all-agents path.
#[cfg(unix)]
#[test]
fn all_agents_delete_reaches_a_nested_copy_in_another_agents_dir() {
	let tmp = tempfile::tempdir().unwrap();
	let root = tmp.path();
	// Claude reads ONLY `.claude/skills`, so it cannot see the other copy —
	// that is what made the initiator-only guard answer "nothing left".
	let claude = root.join(".claude/skills/foo");
	write_skill_md(&claude, "foo");
	// Folder name differs from the frontmatter `name`, AND it is nested: the
	// slot probe misses on both counts.
	let nested = root.join(".opencode/skills/team/legacy-dir");
	write_skill_md(&nested, "foo");

	let mut mgr = ConfigManager::new(
		create_adapter(AgentType::Claude),
		false,
		Some(root),
	);
	mgr.load().unwrap();
	mgr.remove_skill_planned("foo", true, false, true)
		.expect("an all-agents delete of a skill that exists must run");

	assert!(!claude.exists(), "the initiating agent's copy must be gone");
	assert!(
		!nested.exists(),
		"'every agent' includes the one whose copy is not at the \
		 <root>/<name> slot: {}",
		nested.display()
	);
	// The end state, not just the paths: ask the other agent what it can see.
	let mut other = ConfigManager::new(
		create_adapter(AgentType::OpenCode),
		false,
		Some(root),
	);
	other.load().unwrap();
	assert!(
		other.get_skill("foo").is_none(),
		"opencode must not still discover a skill that was deleted for every \
		 agent"
	);
}

/// The same slot-probe blind spot, seen from the other direction: it made a
/// legitimate `--all-agents` delete IMPOSSIBLE.
///
/// For the npx layout (Master `.agents/skills/dirname` declaring
/// `name: realname`, Referrer of the same folder name) the symlink planner
/// probed `<dir>/realname`, found nothing anywhere, and planned an empty
/// removal — which the round-2 guard then refused as
/// `unsupported_operation`, with a message about removing "for this agent
/// alone" that does not even describe the request. There was no verb left that
/// could delete it.
#[cfg(unix)]
#[test]
fn all_agents_delete_works_for_an_npx_folder_name_mismatch() {
	let tmp = tempfile::tempdir().unwrap();
	let root = tmp.path();
	let master = root.join(".agents/skills/dirname");
	write_skill_md(&master, "realname");
	let referrer = root.join(".opencode/skills/dirname");
	std::fs::create_dir_all(referrer.parent().unwrap()).unwrap();
	symlink(&master, &referrer);

	let mut mgr = ConfigManager::new(
		create_adapter(AgentType::OpenCode),
		false,
		Some(root),
	);
	mgr.load().unwrap();
	mgr.remove_skill_planned("realname", true, false, true)
		.expect("--all-agents must be able to remove an npx-layout skill");

	assert!(
		!master.exists(),
		"the Master must go: --all-agents leaves no reader behind"
	);
	assert!(
		std::fs::symlink_metadata(&referrer).is_err(),
		"the Referrer must go with it, not dangle"
	);
}

/// The "did that removal take anything away" guard has to widen with
/// `--all-agents`, because the promise widens with it.
///
/// Asking only the INITIATING agent's read dirs cannot see a leftover in
/// SOMEONE ELSE's dir, and the planner can legitimately leave one: a link that
/// escapes the allow-listed skills roots is refused by the containment check
/// and lands in `skipped`, never `paths`. With the narrow question the run
/// reported a clean `removed` while the other agent still discovered the skill
/// — the round-2 blocker's shape again, one layer further in than the planner
/// fix reaches.
#[cfg(unix)]
#[test]
fn all_agents_delete_refuses_when_an_out_of_tree_entry_survives() {
	let tmp = tempfile::tempdir().unwrap();
	let root = tmp.path();
	// Somewhere no skills root covers, so containment refuses to delete it.
	let outside = tempfile::tempdir().unwrap();
	let stray = outside.path().join("foo");
	write_skill_md(&stray, "foo");

	// Claude reads only `.claude/skills`: the leftover is invisible to it.
	let claude = root.join(".claude/skills/foo");
	write_skill_md(&claude, "foo");
	let escaping = root.join(".opencode/skills/foo");
	std::fs::create_dir_all(escaping.parent().unwrap()).unwrap();
	symlink(&stray, &escaping);

	let mut mgr = ConfigManager::new(
		create_adapter(AgentType::Claude),
		false,
		Some(root),
	);
	mgr.load().unwrap();
	let result = mgr.remove_skill_planned("foo", true, false, true);

	// Disk first: the regression is a removal reported as complete.
	assert!(
		claude.exists(),
		"nothing may be deleted for a removal that cannot finish"
	);
	let error = result.expect_err(
		"an all-agents delete that provably leaves the skill readable must \
		 not report success",
	);
	assert!(
		error.to_string().contains("every agent"),
		"the refusal must name the request the caller actually made, got: \
		 {error}"
	);

	let mut other = ConfigManager::new(
		create_adapter(AgentType::OpenCode),
		false,
		Some(root),
	);
	other.load().unwrap();
	assert!(
		other.get_skill("foo").is_some(),
		"fixture: opencode must still reach the stray skill, or this test \
		 proves nothing"
	);
}

/// A preview must never green-light what the commit refuses.
///
/// `remove_skill_planned` folds "the agent still reads it afterwards" into
/// `plan.shared_master_kept` for exactly this reason, and `RemovalView` turns
/// that into `kept` — "success, and THE ENTITY IS STILL THERE". Nothing pinned
/// either half: with the fold dropped, the npx layout below previews as
/// `{"outcome":"preview","needs_confirm":true}`, whose contract promises
/// `--yes` will change something, and `--yes` answers `unsupported_operation`.
/// That is the never-terminating hint `Kept` and `Absent` were both introduced
/// to eliminate.
#[cfg(unix)]
#[test]
fn a_preview_the_commit_will_refuse_reports_kept_not_preview() {
	let tmp = tempfile::tempdir().unwrap();
	let root = tmp.path();
	// npx layout: folder name != frontmatter name, Referrer beside the Master
	// it points at. opencode reads `.opencode/skills` AND `.agents/skills`, so
	// dropping the Referrer takes nothing away from it.
	let master = root.join(".agents/skills/dirname");
	write_skill_md(&master, "realname");
	let referrer = root.join(".opencode/skills/dirname");
	std::fs::create_dir_all(referrer.parent().unwrap()).unwrap();
	symlink(&master, &referrer);

	let mut mgr = ConfigManager::new(
		create_adapter(AgentType::OpenCode),
		false,
		Some(root),
	);
	mgr.load().unwrap();

	let preview = mgr
		.remove_skill_planned("realname", false, true, false)
		.expect("a preview never refuses");
	let view = aghub_core::dto::RemovalView::from_outcome(&preview, true);
	assert_eq!(
		view.outcome,
		aghub_core::dto::RemovalKind::Kept,
		"the commit refuses this shape, so the preview must say `kept`, not \
		 point the caller at a guaranteed error: {view:?}"
	);

	// The pairing, on the same fixture: what the preview promised is what the
	// commit does.
	mgr.remove_skill_planned("realname", false, false, true)
		.expect_err("the commit refuses — that is what `kept` disclosed");
	assert!(
		std::fs::symlink_metadata(&referrer).is_ok(),
		"and it refuses without touching anything"
	);
}

/// A private per-agent copy SHADOWING the Master is deletable, and deleting it
/// really removes it.
///
/// This is NOT the Referrer-beside-Master shape: `.cursor/skills/foo` is a real
/// directory with its own content, so dropping it changes what cursor reads (it
/// falls back to the Master's version). Refusing it — which "are there any
/// survivors?" did — left NO verb able to drop a stale private copy: `delete`,
/// both API delete routes and `reconcile --remove` all refused, and
/// `--all-agents` takes the Master with it. The behaviour that guard replaced
/// was not lying here; it was only SILENT about the fallback, which is why the
/// Master now has to show up in `skipped`.
#[cfg(unix)]
#[test]
fn delete_removes_a_private_copy_that_shadows_the_master() {
	let tmp = tempfile::tempdir().unwrap();
	let root = tmp.path();
	// Different content on purpose: that is why a user wants the copy gone.
	let private = root.join(".cursor/skills/foo");
	write_skill_md(&private, "foo");
	std::fs::write(private.join("extra.md"), "private v1").unwrap();
	let master = root.join(".agents/skills/foo");
	write_skill_md(&master, "foo");

	let mut mgr = ConfigManager::new(
		create_adapter(AgentType::Cursor),
		false,
		Some(root),
	);
	mgr.load().unwrap();
	let outcome = mgr
		.remove_skill_planned("foo", false, false, true)
		.expect("a private copy shadowing the Master must be removable");

	assert!(
		!private.exists(),
		"the private copy must be GONE from disk, not merely reported"
	);
	assert!(
		master.join("SKILL.md").exists(),
		"a single-agent removal may never take the shared Master"
	);
	assert_eq!(
		outcome.plan.paths,
		vec![private.clone()],
		"only cursor's own copy is in scope"
	);
	assert!(
		outcome.plan.skipped.contains(&master),
		"the removal has to disclose that cursor now reads `foo` from the \
		 Master instead: {:?}",
		outcome.plan.skipped
	);
	let view = aghub_core::dto::RemovalView::from_outcome(&outcome, false);
	assert_eq!(
		view.outcome,
		aghub_core::dto::RemovalKind::Removed,
		"something really was removed, so `kept` would be a second lie: \
		 {view:?}"
	);
}

/// The preview of that same removal must promise a commit that CHANGES
/// something — including the lock keys it would drop.
///
/// `remove_skill_planned` gates the `would_prune_lock_entries` disclosure on
/// the very flag the refusal reads. Leave that gate on "are there survivors?"
/// while the refusal moves on, and the now-allowed removal previews as
/// `PruneStatus::NotRun` — silently dropping A9's disclosure for exactly the
/// shape this fix re-enabled.
#[cfg(unix)]
#[test]
fn preview_of_a_shadowing_copy_removal_discloses_the_lock_prune() {
	let tmp = tempfile::tempdir().unwrap();
	let root = tmp.path();
	let private = root.join(".cursor/skills/foo");
	write_skill_md(&private, "foo");
	write_skill_md(&root.join(".agents/skills/foo"), "foo");

	let mut mgr = ConfigManager::new(
		create_adapter(AgentType::Cursor),
		false,
		Some(root),
	);
	mgr.load().unwrap();
	let preview = mgr
		.remove_skill_planned("foo", false, true, false)
		.expect("a preview never refuses");

	assert_eq!(
		aghub_core::dto::RemovalView::from_outcome(&preview, true).outcome,
		aghub_core::dto::RemovalKind::Preview,
		"the commit goes ahead, so this is a preview — not `kept`"
	);
	assert!(
		matches!(
			preview.prune,
			aghub_core::skills::removal::PruneStatus::WouldPrune(_)
		),
		"a preview whose commit really runs must disclose the prune that \
		 commit would run, got: {:?}",
		preview.prune
	);
}

/// The third verb that was blocked by the same refusal: `reconcile --remove`.
///
/// It crosses `preflight_delete`, which refuses BEFORE the first write, so its
/// verdict has to be the commit's verdict exactly — it reads the flag
/// `remove_skill_planned` folded into its own dry-run rather than re-deriving
/// one. A preflight of its own that refuses on "something still serves this
/// skill" (the master now rides along in `skipped`) would look defensive and
/// silently take away cursor's last way to drop a stale private copy.
#[cfg(unix)]
#[test]
fn reconcile_removes_a_private_copy_that_shadows_the_master() {
	use aghub_core::transfer::{
		reconcile_skill, InstallScope, ResourceLocator,
	};

	let tmp = tempfile::tempdir().unwrap();
	let root = tmp.path();
	let private = root.join(".cursor/skills/foo");
	write_skill_md(&private, "foo");
	let master = root.join(".agents/skills/foo");
	write_skill_md(&master, "foo");

	// No `--add`: nothing here can hand the skill back through a new Master.
	reconcile_skill(
		ResourceLocator {
			agent: AgentType::Cursor,
			scope: InstallScope::Project,
			project_root: Some(root.to_path_buf()),
			name: "foo".to_string(),
		},
		vec![],
		vec![AgentType::Cursor],
		true,
	)
	.expect("dropping a private copy that shadows the Master is legal");

	assert!(!private.exists(), "cursor's private copy must be gone");
	assert!(
		master.join("SKILL.md").exists(),
		"and the shared Master must survive it"
	);
}
