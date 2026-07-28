//! `aghub-cli doctor` — read-only skill health across scopes.
//!
//! Reconciles each scope's skill lock against the on-disk universal master
//! (`.agents/skills`): what's installed, where it came from (git repo vs local),
//! and whether disk and lock agree. One table instead of cross-referencing
//! `source list` + `check` + `prune-lock`. Never writes.

use std::collections::BTreeMap;
use std::path::Path;

use aghub_core::{
	models::{AgentSelection, AgentType, ResourceScope},
	registry,
	skills::linker::{
		classify::{agent_link_need, LinkNeed},
		universal_canonical_dir, Linker,
	},
};
use anyhow::{anyhow, Result};
use serde::Serialize;
use skill_update::sources::SourceScope;
use tabled::builder::Builder;
use tabled::settings::Style;

/// On-disk state of a skill's master directory (`.agents/skills/<name>`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum MasterState {
	/// A real directory — the expected symlink-master layout.
	Dir,
	/// A symlink where the master dir should be (unusual; target recorded).
	Link,
	/// Nothing at that path.
	Missing,
}

/// Per-agent state of one skill referrer when link verification is requested.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
enum AgentLinkState {
	AutoCovered,
	Unsupported,
	Linked,
	Missing,
	Dangling,
	ForeignLink,
	RealPathConflict,
	Inaccessible,
}

impl AgentLinkState {
	fn label(self) -> &'static str {
		match self {
			Self::AutoCovered => "auto-covered",
			Self::Unsupported => "unsupported",
			Self::Linked => "linked",
			Self::Missing => "missing",
			Self::Dangling => "dangling",
			Self::ForeignLink => "foreign-link",
			Self::RealPathConflict => "real-path-conflict",
			Self::Inaccessible => "inaccessible",
		}
	}
}

#[derive(Debug, Clone, Serialize)]
struct AgentLinkAudit {
	agent: String,
	state: AgentLinkState,
	#[serde(skip_serializing_if = "Option::is_none")]
	path: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
enum LinkAudit {
	NotRequested,
	Verified { agents: Vec<AgentLinkAudit> },
}

impl LinkAudit {
	fn label(&self) -> String {
		match self {
			Self::NotRequested => "not-requested".to_string(),
			Self::Verified { agents } => agents
				.iter()
				.map(|audit| format!("{}:{}", audit.agent, audit.state.label()))
				.collect::<Vec<_>>()
				.join(","),
		}
	}
}

impl MasterState {
	fn label(&self) -> &'static str {
		match self {
			Self::Dir => "dir",
			Self::Link => "link",
			Self::Missing => "missing",
		}
	}
}

/// One skill row in the doctor report.
#[derive(Debug, Clone, Serialize)]
struct DoctorRow {
	scope: &'static str,
	skill: String,
	/// Displayable source (`owner/repo`, `local`, or `type:source`).
	source: String,
	/// True when the source is a git repo — i.e. `check`/`apply-update` can
	/// refresh it. JSON-only hint; `check --online` is authoritative.
	updatable: bool,
	master: MasterState,
	/// `ok` | `orphan-lock` (lock entry, no master on disk) | `untracked`
	/// (master on disk, no lock entry) | `master-is-symlink`.
	health: &'static str,
	/// Explicitly distinguishes the default Master-only audit from an optional
	/// roster-aware referrer audit.
	#[serde(rename = "linkAudit")]
	link_audit: LinkAudit,
}

#[derive(Debug, Clone)]
struct LockedSkill {
	source: String,
	source_type: String,
	skill_path: Option<String>,
}

/// Inspect one NeedsLink agent slot without mutating it or following a foreign
/// occupant. The master argument is the canonical skill directory, not the
/// universal-master parent.
fn inspect_agent_link(
	master_skill: &Path,
	agent_skills_dir: &Path,
	skill_name: &str,
) -> AgentLinkState {
	let slot = agent_skills_dir.join(skill_name);
	match std::fs::symlink_metadata(&slot) {
		Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
			return AgentLinkState::Missing;
		}
		Err(_) => return AgentLinkState::Inaccessible,
		Ok(_) if !Linker::is_link(&slot) => {
			return AgentLinkState::RealPathConflict;
		}
		Ok(_) => {}
	}

	let Ok(actual) = std::fs::canonicalize(&slot) else {
		return AgentLinkState::Dangling;
	};
	let Ok(expected) = std::fs::canonicalize(master_skill) else {
		return AgentLinkState::Dangling;
	};
	if actual == expected {
		AgentLinkState::Linked
	} else {
		AgentLinkState::ForeignLink
	}
}

fn resolve_roster(agent: &str) -> Result<Vec<AgentType>> {
	match AgentSelection::parse(agent).map_err(|error| {
		anyhow!("invalid --agent for doctor link audit: {error}")
	})? {
		AgentSelection::All => Ok(AgentType::ALL.to_vec()),
		AgentSelection::List(agents) => Ok(agents),
	}
}

fn audit_agent_links(
	skill_name: &str,
	master: &Path,
	scope: ResourceScope,
	project_root: Option<&Path>,
	agents: &[AgentType],
) -> LinkAudit {
	let master_skill = master.join(skill_name);
	let agents = agents
		.iter()
		.map(|agent| {
			let need = agent_link_need(
				registry::get(*agent),
				scope,
				project_root,
				master,
			);
			let (state, path) = match need {
				LinkNeed::NativeReader => {
					let state = if matches!(
						master_state(&master_skill),
						MasterState::Dir
					) {
						AgentLinkState::AutoCovered
					} else {
						AgentLinkState::Missing
					};
					(state, Some(master_skill.to_string_lossy().into_owned()))
				}
				LinkNeed::Unsupported => (AgentLinkState::Unsupported, None),
				LinkNeed::NeedsLink { agent_skills_dir } => {
					let state = inspect_agent_link(
						&master_skill,
						&agent_skills_dir,
						skill_name,
					);
					(
						state,
						Some(
							agent_skills_dir
								.join(skill_name)
								.to_string_lossy()
								.into_owned(),
						),
					)
				}
			};
			AgentLinkAudit {
				agent: agent.as_str().to_string(),
				state,
				path,
			}
		})
		.collect();
	LinkAudit::Verified { agents }
}

/// Health verdict from lock membership + on-disk master state. Pure so it unit
/// tests without touching the filesystem.
fn health_of(
	tracked: bool,
	master: &MasterState,
	valid_skill: bool,
) -> &'static str {
	match (tracked, master, valid_skill) {
		(_, MasterState::Dir, false) => "invalid-skill",
		(true, MasterState::Dir, true) => "ok",
		(_, MasterState::Link, _) => "master-is-symlink",
		(true, MasterState::Missing, _) => "orphan-lock",
		// Untracked rows are generated from the disk scan, so they are always
		// present on disk; the missing arm cannot occur but stays exhaustive.
		(false, MasterState::Missing, _) => "orphan-lock",
		(false, _, _) => "untracked",
	}
}

/// Human source label + whether it's a git source (updatable). The label is
/// `owner/repo` for github, `local`, or `type:source` for any other provider.
/// Pure.
fn source_display(source: &str, source_type: &str) -> (String, bool) {
	let t = source_type.to_ascii_lowercase();
	if t == "local" || source.is_empty() {
		return ("local".to_string(), false);
	}
	// Git source types aghub can re-fetch — the `as_str()` values of
	// `aghub_git::RemoteSourceType` (github / gitlab / git).
	let updatable = matches!(t.as_str(), "github" | "git" | "gitlab");
	// github shorthand is already `owner/repo`; other providers keep their type
	// prefix so `mintlify:bun.com` doesn't read as a git repo.
	let label = if t == "github" {
		source.to_string()
	} else {
		format!("{source_type}:{source}")
	};
	(label, updatable)
}

/// On-disk state of the master path for one skill. Uses the canonical
/// [`Linker::is_link`] so a Windows junction is classified as a link, not a dir.
fn master_state(path: &Path) -> MasterState {
	if std::fs::symlink_metadata(path).is_err() {
		MasterState::Missing
	} else if Linker::is_link(path) {
		MasterState::Link
	} else {
		MasterState::Dir
	}
}

/// Candidate skill dir names under the Master. Real directories are included
/// even when their SKILL.md is invalid/missing, and links are included without
/// trusting their targets, so doctor can report both hazards.
fn master_skills_on_disk(master: &Path) -> Vec<String> {
	let Ok(rd) = std::fs::read_dir(master) else {
		return Vec::new();
	};
	rd.filter_map(|e| e.ok())
		.filter(|e| Linker::is_link(&e.path()) || e.path().is_dir())
		.filter_map(|e| e.file_name().into_string().ok())
		.collect()
}

/// Build the rows for one scope: every lock entry reconciled against the master,
/// plus any master skill with no lock entry (`untracked`).
fn build_rows(
	scope: &'static str,
	master: &Path,
	locked: &BTreeMap<String, LockedSkill>,
) -> Vec<DoctorRow> {
	let mut rows = Vec::new();
	for (name, locked_skill) in locked {
		let state = master_state(&master.join(name));
		let valid_skill = matches!(state, MasterState::Dir)
			&& skill::parser::parse(&master.join(name).join("SKILL.md"))
				.is_ok_and(|parsed| parsed.name == *name);
		let (label, fetchable) =
			source_display(&locked_skill.source, &locked_skill.source_type);
		let updatable =
			fetchable && locked_skill.skill_path.is_some() && valid_skill;
		let health = health_of(true, &state, valid_skill);
		rows.push(DoctorRow {
			scope,
			skill: name.clone(),
			source: label,
			updatable,
			master: state,
			health,
			link_audit: LinkAudit::NotRequested,
		});
	}
	for name in master_skills_on_disk(master) {
		if !locked.contains_key(&name) {
			let state = master_state(&master.join(&name));
			let valid_skill = matches!(state, MasterState::Dir)
				&& skill::parser::parse(&master.join(&name).join("SKILL.md"))
					.is_ok_and(|parsed| parsed.name == name);
			let health = health_of(false, &state, valid_skill);
			rows.push(DoctorRow {
				scope,
				skill: name,
				source: "—".to_string(),
				updatable: false,
				master: state,
				health,
				link_audit: LinkAudit::NotRequested,
			});
		}
	}
	rows.sort_by(|a, b| a.skill.cmp(&b.skill));
	rows
}

/// Global lock entries reduced to `(name → (source, source_type))`.
fn global_locked() -> BTreeMap<String, LockedSkill> {
	skill::get_all_locked_skills()
		.into_iter()
		.map(|(name, entry)| {
			(
				name,
				LockedSkill {
					source: entry.source,
					source_type: entry.source_type,
					skill_path: entry.skill_path,
				},
			)
		})
		.collect()
}

/// Project lock entries reduced to `(name → (source, source_type))`.
fn project_locked(root: &Path) -> BTreeMap<String, LockedSkill> {
	skill::read_local_lock(Some(root))
		.skills
		.into_iter()
		.map(|(name, entry)| {
			(
				name,
				LockedSkill {
					source: entry.source,
					source_type: entry.source_type,
					skill_path: entry.skill_path,
				},
			)
		})
		.collect()
}

/// Dispatch `doctor`, optionally auditing the selected roster's referrers.
/// Scope resolution is shared with `source` (`-g` global only, `-p` project
/// only, default = global plus the current project when a root is detected).
pub fn execute_with_options(
	global: bool,
	project: bool,
	json: bool,
	verify_links: bool,
	agent: &str,
) -> Result<()> {
	let scopes = crate::commands::source::resolve_read_scopes(global, project)?;
	let roster = verify_links.then(|| resolve_roster(agent)).transpose()?;

	let mut rows: Vec<DoctorRow> = Vec::new();
	for scope in &scopes {
		let (root, resource_scope, locked) = match scope {
			SourceScope::Global => {
				(None, ResourceScope::GlobalOnly, global_locked())
			}
			SourceScope::Project { root } => (
				Some(root.as_path()),
				ResourceScope::ProjectOnly,
				project_locked(root),
			),
		};
		if let Some(master) = universal_canonical_dir(root) {
			let mut scope_rows = build_rows(
				crate::commands::source::scope_label(scope),
				&master,
				&locked,
			);
			if let Some(agents) = &roster {
				for row in &mut scope_rows {
					row.link_audit = audit_agent_links(
						&row.skill,
						&master,
						resource_scope,
						root,
						agents,
					);
				}
			}
			rows.extend(scope_rows);
		}
	}

	if json {
		println!("{}", serde_json::to_string_pretty(&rows)?);
		return Ok(());
	}

	if rows.is_empty() {
		println!("No installed skills.");
		return Ok(());
	}

	let mut builder = Builder::default();
	builder
		.push_record(["SCOPE", "SKILL", "SOURCE", "MASTER", "HEALTH", "LINKS"]);
	for r in &rows {
		builder.push_record([
			r.scope.to_string(),
			r.skill.clone(),
			r.source.clone(),
			r.master.label().to_string(),
			r.health.to_string(),
			r.link_audit.label(),
		]);
	}
	let mut table = builder.build();
	table.with(Style::sharp());
	println!("{table}");

	// A one-line hint only when something is off, so the healthy case stays quiet.
	let orphans = rows.iter().filter(|r| r.health == "orphan-lock").count();
	let untracked = rows.iter().filter(|r| r.health == "untracked").count();
	if orphans > 0 {
		eprintln!(
			"note: {orphans} orphan lock ent(y/ies) — run `aghub-cli prune-lock` \
			 to clear"
		);
	}
	if untracked > 0 {
		eprintln!(
			"note: {untracked} untracked skill(s) on disk with no lock — compare or \
			 back up local content, then delete before reinstalling via source sync; \
			 sync never overwrites an existing Master"
		);
	}
	let broken_links = rows
		.iter()
		.filter_map(|row| match &row.link_audit {
			LinkAudit::NotRequested => None,
			LinkAudit::Verified { agents } => Some(agents),
		})
		.flatten()
		.filter(|audit| {
			matches!(
				audit.state,
				AgentLinkState::Missing
					| AgentLinkState::Dangling
					| AgentLinkState::ForeignLink
					| AgentLinkState::RealPathConflict
					| AgentLinkState::Inaccessible
			)
		})
		.count();
	if broken_links > 0 {
		eprintln!(
			"note: {broken_links} agent referrer issue(s) — repair missing/dangling \
			 links with an explicit roster and source sync --skill <name> \
			 --install-missing; foreign or real-path conflicts require inspection"
		);
	}
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn health_ok_when_tracked_and_master_is_a_dir() {
		assert_eq!(health_of(true, &MasterState::Dir, true), "ok");
	}

	#[test]
	fn health_orphan_lock_when_tracked_but_master_missing() {
		assert_eq!(
			health_of(true, &MasterState::Missing, false),
			"orphan-lock"
		);
	}

	#[test]
	fn health_untracked_when_on_disk_but_not_locked() {
		assert_eq!(health_of(false, &MasterState::Dir, true), "untracked");
	}

	#[test]
	fn health_flags_a_master_that_is_itself_a_symlink() {
		assert_eq!(
			health_of(true, &MasterState::Link, false),
			"master-is-symlink"
		);
	}

	#[cfg(unix)]
	#[test]
	fn untracked_symlink_master_is_reported_as_unsafe() {
		use std::os::unix::fs::symlink;

		let tmp = tempfile::tempdir().unwrap();
		let master = tmp.path().join("master");
		let outside = tmp.path().join("outside/loose");
		write_skill(&outside, "loose");
		std::fs::create_dir_all(&master).unwrap();
		symlink(&outside, master.join("loose")).unwrap();

		let rows = build_rows("global", &master, &BTreeMap::new());
		assert_eq!(rows.len(), 1);
		assert_eq!(rows[0].master, MasterState::Link);
		assert_eq!(rows[0].health, "master-is-symlink");
	}

	#[test]
	fn source_display_github_is_owner_repo_and_updatable() {
		let (label, updatable) = source_display("owner/repo", "github");
		assert_eq!(label, "owner/repo");
		assert!(updatable);
	}

	#[test]
	fn source_display_local_is_not_updatable() {
		let (label, updatable) = source_display("/tmp/x", "local");
		assert_eq!(label, "local");
		assert!(!updatable);
	}

	#[test]
	fn source_display_other_provider_keeps_type_prefix() {
		let (label, updatable) = source_display("bun.com", "mintlify");
		assert_eq!(label, "mintlify:bun.com");
		assert!(!updatable);
	}

	#[test]
	fn doctor_row_json_keeps_updatable_field() {
		// `doctor --json` shipped `updatable` in v2.6.2 — it is part of the
		// released schema and must stay serialized.
		let locked = BTreeMap::from([(
			"x".to_string(),
			LockedSkill {
				source: "o/r".to_string(),
				source_type: "github".to_string(),
				skill_path: Some("x/SKILL.md".to_string()),
			},
		)]);
		let rows = build_rows("global", Path::new("/nonexistent"), &locked);
		let v = serde_json::to_value(&rows[0]).unwrap();
		assert_eq!(v["updatable"], serde_json::json!(false));
	}

	#[test]
	fn build_rows_marks_untracked_master_skill() {
		let tmp = tempfile::tempdir().unwrap();
		let master = tmp.path();
		// A skill dir on disk with a SKILL.md but no lock entry.
		let d = master.join("loose");
		std::fs::create_dir_all(&d).unwrap();
		std::fs::write(
			d.join("SKILL.md"),
			"---\nname: loose\ndescription: valid\n---\n",
		)
		.unwrap();
		let rows = build_rows("global", master, &BTreeMap::new());
		assert_eq!(rows.len(), 1);
		assert_eq!(rows[0].skill, "loose");
		assert_eq!(rows[0].health, "untracked");
	}

	#[test]
	fn build_rows_flags_lock_entry_with_no_master_on_disk() {
		let tmp = tempfile::tempdir().unwrap();
		let mut locked = BTreeMap::new();
		locked.insert(
			"gone".to_string(),
			LockedSkill {
				source: "owner/repo".to_string(),
				source_type: "github".to_string(),
				skill_path: Some("gone/SKILL.md".to_string()),
			},
		);
		let rows = build_rows("global", tmp.path(), &locked);
		assert_eq!(rows.len(), 1);
		assert_eq!(rows[0].skill, "gone");
		assert_eq!(rows[0].health, "orphan-lock");
		assert_eq!(rows[0].source, "owner/repo");
		assert!(!rows[0].updatable);
	}

	#[cfg(unix)]
	fn write_skill(dir: &Path, name: &str) {
		std::fs::create_dir_all(dir).unwrap();
		std::fs::write(
			dir.join("SKILL.md"),
			format!("---\nname: {name}\ndescription: test\n---\n"),
		)
		.unwrap();
	}

	#[cfg(unix)]
	#[test]
	fn inspect_agent_link_distinguishes_every_occupant_state() {
		use std::os::unix::fs::symlink;

		let tmp = tempfile::tempdir().unwrap();
		let master = tmp.path().join("master/foo");
		write_skill(&master, "foo");

		let missing_dir = tmp.path().join("missing-agent");
		assert_eq!(
			inspect_agent_link(&master, &missing_dir, "foo"),
			AgentLinkState::Missing
		);

		let linked_dir = tmp.path().join("linked-agent");
		std::fs::create_dir_all(&linked_dir).unwrap();
		symlink(&master, linked_dir.join("foo")).unwrap();
		assert_eq!(
			inspect_agent_link(&master, &linked_dir, "foo"),
			AgentLinkState::Linked
		);

		let dangling_dir = tmp.path().join("dangling-agent");
		std::fs::create_dir_all(&dangling_dir).unwrap();
		symlink(tmp.path().join("gone"), dangling_dir.join("foo")).unwrap();
		assert_eq!(
			inspect_agent_link(&master, &dangling_dir, "foo"),
			AgentLinkState::Dangling
		);

		let foreign_master = tmp.path().join("other/foo");
		write_skill(&foreign_master, "foo");
		let foreign_dir = tmp.path().join("foreign-agent");
		std::fs::create_dir_all(&foreign_dir).unwrap();
		symlink(&foreign_master, foreign_dir.join("foo")).unwrap();
		assert_eq!(
			inspect_agent_link(&master, &foreign_dir, "foo"),
			AgentLinkState::ForeignLink
		);

		let conflict_dir = tmp.path().join("conflict-agent");
		write_skill(&conflict_dir.join("foo"), "foo");
		assert_eq!(
			inspect_agent_link(&master, &conflict_dir, "foo"),
			AgentLinkState::RealPathConflict
		);
	}

	#[test]
	fn native_reader_is_missing_when_master_skill_is_absent() {
		let tmp = tempfile::tempdir().unwrap();
		let master = tmp.path().join(".agents/skills");
		let audit = audit_agent_links(
			"gone",
			&master,
			ResourceScope::ProjectOnly,
			Some(tmp.path()),
			&[AgentType::Codex],
		);
		let LinkAudit::Verified { agents } = audit else {
			panic!("link audit should be verified")
		};
		assert_eq!(agents.len(), 1);
		assert_eq!(agents[0].state, AgentLinkState::Missing);
		assert_eq!(
			agents[0].path.as_deref(),
			Some(master.join("gone").to_string_lossy().as_ref())
		);
	}

	#[test]
	fn doctor_json_says_when_link_audit_was_not_requested() {
		let rows =
			build_rows("global", Path::new("/nonexistent"), &BTreeMap::new());
		assert!(rows.is_empty());

		let row = DoctorRow {
			scope: "global",
			skill: "x".to_string(),
			source: "local".to_string(),
			updatable: false,
			master: MasterState::Dir,
			health: "untracked",
			link_audit: LinkAudit::NotRequested,
		};
		let value = serde_json::to_value(row).unwrap();
		assert_eq!(value["linkAudit"]["state"], "notRequested");
	}

	#[test]
	fn tracked_skill_is_updatable_only_with_path_and_valid_master() {
		let tmp = tempfile::tempdir().unwrap();
		let master = tmp.path();
		let skill_dir = master.join("tracked");
		std::fs::create_dir_all(&skill_dir).unwrap();
		std::fs::write(skill_dir.join("SKILL.md"), "not frontmatter").unwrap();

		let mut locked = BTreeMap::from([(
			"tracked".to_string(),
			LockedSkill {
				source: "owner/repo".to_string(),
				source_type: "github".to_string(),
				skill_path: Some("tracked/SKILL.md".to_string()),
			},
		)]);
		let rows = build_rows("global", master, &locked);
		assert_eq!(rows[0].health, "invalid-skill");
		assert!(!rows[0].updatable);

		std::fs::write(
			skill_dir.join("SKILL.md"),
			"---\nname: tracked\ndescription: valid\n---\n",
		)
		.unwrap();
		locked.get_mut("tracked").unwrap().skill_path = None;
		let rows = build_rows("global", master, &locked);
		assert_eq!(rows[0].health, "ok");
		assert!(!rows[0].updatable);

		locked.get_mut("tracked").unwrap().skill_path =
			Some("tracked/SKILL.md".to_string());
		let rows = build_rows("global", master, &locked);
		assert!(rows[0].updatable);
	}
}
