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
		master_store_dir, Linker,
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
	/// The agent holds no Referrer while the Master is healthy — the skill is
	/// installed and deliberately NOT granted to this agent.
	///
	/// This is the state the whole `.aghub` store exists to make expressible, so
	/// it is NOT an issue. It replaces `AutoCovered`, whose question ("does this
	/// agent read the Master without a link") stopped having an answer when the
	/// Master moved to a store nothing reads.
	///
	/// Under D8 (no persisted authorization) `Withheld` is indistinguishable
	/// from a grant the user removed by hand. Accepted: the alternative is a
	/// second source of truth that drifts, and a doctor that cries wolf.
	Withheld,
	Unsupported,
	Linked,
	Missing,
	Dangling,
	ForeignLink,
	RealPathConflict,
	Inaccessible,
	/// This agent has no slot for the skill AND the master is untracked — a
	/// leftover, not a missing link.
	///
	/// Distinguished from `Missing` because the REMEDY is the opposite. A
	/// `missing` referrer is repaired by re-linking from its source; an orphan
	/// master has no source to re-link from, and doctor's blanket
	/// "`source sync --install-missing`" advice would REINSTALL a skill the
	/// user had just deleted — `delete --yes` deliberately keeps the master
	/// when another agent still reads it, and that leftover landed here.
	OrphanMaster,
}

impl AgentLinkState {
	fn label(self) -> &'static str {
		match self {
			Self::Withheld => "withheld",
			Self::Unsupported => "unsupported",
			Self::Linked => "linked",
			Self::Missing => "missing",
			Self::Dangling => "dangling",
			Self::ForeignLink => "foreign-link",
			Self::RealPathConflict => "real-path-conflict",
			Self::Inaccessible => "inaccessible",
			Self::OrphanMaster => "orphan-master",
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
	/// Every agent row is in a healthy state.
	Verified {
		agents: Vec<AgentLinkAudit>,
	},
	/// At least one agent row is not. Reported `verified` before, while its own
	/// rows said `missing` — a summary that contradicted its own detail, and
	/// the one an automated caller reads first.
	Issues {
		agents: Vec<AgentLinkAudit>,
	},
}

impl AgentLinkState {
	/// Is this state something a caller should act on?
	///
	/// `autoCovered` and `unsupported` are not problems — they are the correct
	/// resting state for an agent that reads the master directly or cannot hold
	/// a skill at all.
	fn is_issue(self) -> bool {
		match self {
			Self::Withheld | Self::Unsupported | Self::Linked => false,
			Self::Missing
			| Self::Dangling
			| Self::ForeignLink
			| Self::RealPathConflict
			| Self::Inaccessible
			| Self::OrphanMaster => true,
		}
	}
}

impl LinkAudit {
	fn label(&self) -> String {
		match self {
			Self::NotRequested => "not-requested".to_string(),
			Self::Verified { agents } | Self::Issues { agents } => agents
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

impl DoctorRow {
	/// Is this row something a caller should act on?
	///
	/// `health` is the lock ↔ Master axis and is ALWAYS computed; `link_audit`
	/// is the per-agent referrer axis and exists only under `--verify-links`.
	/// A gate that reads one and not the other is inert exactly when the other
	/// is the only thing that ran.
	fn is_issue(&self) -> bool {
		// NOT simply `health != "ok"`. Two non-`ok` values are legitimate
		// resting states, and gating on them makes `--fail-on-issues` red for
		// setups that are working exactly as designed:
		//
		// - `untracked`: a hand-written skill in `.agents/skills` with no lock
		//   entry. That is how a skill authored in place looks — this very repo
		//   is that layout — and the note doctor prints for it ("back up, then
		//   delete before reinstalling via source sync") is the wrong advice
		//   for one, let alone a reason to fail CI.
		// - `master-is-symlink`: a SUPPORTED layout, as the NativeReader branch
		//   below says in so many words.
		//
		// `orphan-lock` and `invalid-skill` ARE actionable: a lock entry with
		// no master on disk, and a master whose SKILL.md does not parse.
		let health_bad = matches!(self.health, "orphan-lock" | "invalid-skill");
		let links_bad = match &self.link_audit {
			LinkAudit::NotRequested | LinkAudit::Verified { .. } => false,
			LinkAudit::Issues { agents } => {
				agents.iter().any(|audit| audit.state.is_issue())
			}
		};
		health_bad || links_bad
	}

	/// Which axis made this row an issue — for a message that points at the one
	/// that actually ran.
	fn issue_axis(&self) -> Option<&'static str> {
		let links_bad = matches!(&self.link_audit, LinkAudit::Issues { .. });
		let health_bad = matches!(self.health, "orphan-lock" | "invalid-skill");
		match (health_bad, links_bad) {
			(true, true) => Some("both"),
			(true, false) => Some("health"),
			(false, true) => Some("links"),
			(false, false) => None,
		}
	}
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

/// `tracked` = the skill has a lock entry.
///
/// Passed in rather than derived from the row's `health`: `health_of` checks
/// `invalid-skill` BEFORE the untracked arm, so an untracked master with a
/// broken SKILL.md reports `invalid-skill`, and reading `health == "untracked"`
/// would miss it.
fn audit_agent_links(
	skill_name: &str,
	master: &Path,
	scope: ResourceScope,
	project_root: Option<&Path>,
	agents: &[AgentType],
	tracked: bool,
) -> LinkAudit {
	let master_skill = master.join(skill_name);
	let agents = agents
		.iter()
		.map(|agent| {
			let need =
				agent_link_need(registry::get(*agent), scope, project_root);
			let (state, path) = match need {
				LinkNeed::Unsupported => (AgentLinkState::Unsupported, None),
				LinkNeed::NeedsLink { referrer_dir } => {
					let agent_skills_dir = referrer_dir;
					let mut state = inspect_agent_link(
						&master_skill,
						&agent_skills_dir,
						skill_name,
					);
					// An absent slot for an UNTRACKED master is a leftover, not
					// a missing link — there is no source to relink from, and
					// the blanket sync-repair advice would reinstall a skill the
					// user just deleted. Gated on `!= Missing` rather than
					// `== Dir` so a master that is itself a symlink (health
					// `master-is-symlink`) also stays out of that bucket.
					// `Dangling` deliberately does NOT downgrade: it is a real
					// artifact a relink replaces.
					if state == AgentLinkState::Missing
						&& !matches!(
							master_state(&master_skill),
							MasterState::Missing
						) {
						// An absent slot beside a live Master is either a
						// leftover (untracked: nothing to relink FROM) or the
						// withheld state this feature exists to create. Neither
						// is a missing link, and the blanket
						// `source sync --install-missing` advice would
						// reinstall a skill the user deliberately narrowed.
						state = if tracked {
							AgentLinkState::Withheld
						} else {
							AgentLinkState::OrphanMaster
						};
					}
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
		.collect::<Vec<AgentLinkAudit>>();
	// `verified` must not be reported while a row says otherwise — the summary
	// contradicting its own detail is what let `doctor --verify-links && echo
	// healthy` print healthy over a dangling referrer.
	if agents.iter().any(|audit| audit.state.is_issue()) {
		LinkAudit::Issues { agents }
	} else {
		LinkAudit::Verified { agents }
	}
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
///
/// Takes the ALREADY-READ lock. `doctor` presents these entries as its answer,
/// so re-reading here after the fail-closed check left a window in which a
/// non-aghub writer could truncate the file — the second read falls open to an
/// empty lock and every still-installed skill is reported `untracked`, with
/// remediation that says to delete it.
fn global_locked(
	lock: skill::lock::SkillLockFile,
) -> BTreeMap<String, LockedSkill> {
	lock.skills
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

/// Project lock entries reduced to `(name → (source, source_type))`. See
/// [`global_locked`] for why the lock is passed in.
fn project_locked(
	lock: skill::lock::local::LocalSkillLockFile,
) -> BTreeMap<String, LockedSkill> {
	lock.skills
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
	scope: &crate::Scope,
	json: bool,
	verify_links: bool,
	agent: &str,
	fail_on_issues: bool,
) -> Result<()> {
	let scopes = crate::commands::source::read_scopes(scope);
	// doctor's whole point is reporting lock health, and it must not report a
	// clean-but-empty world for a lock it could not parse — it classified the
	// still-present skills `untracked` and told the caller to delete them.
	// ONE read of each lock, consumed below — see `LockSnapshot`.
	let mut locks = crate::commands::source::read_scope_locks_checked(&scopes)?;
	let roster = verify_links.then(|| resolve_roster(agent)).transpose()?;

	let mut rows: Vec<DoctorRow> = Vec::new();
	for scope in &scopes {
		let (root, resource_scope, locked) = match scope {
			SourceScope::Global => (
				None,
				ResourceScope::GlobalOnly,
				global_locked(locks.global.take().unwrap_or_default()),
			),
			SourceScope::Project { root } => (
				Some(root.as_path()),
				ResourceScope::ProjectOnly,
				project_locked(locks.project.take().unwrap_or_default()),
			),
		};
		if let Some(master) = master_store_dir(root) {
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
						locked.contains_key(&row.skill),
					);
				}
			}
			rows.extend(scope_rows);
		}
	}

	// Counted before the JSON early-return: `--fail-on-issues` must mean the
	// same thing in both output modes.
	//
	// Counts BOTH halves of what doctor reports. Deriving it from `link_audit`
	// alone made the flag inert without `--verify-links` — every row is
	// `notRequested` then — so `doctor --fail-on-issues` exited 0 over an
	// `untracked` master or an `invalid-skill`: the same false green the flag
	// exists to remove, one level up.
	let issue_count = rows.iter().filter(|row| row.is_issue()).count();
	// Name the axis that actually failed. A hard-coded "agent referrer
	// issue(s)" pointed at the link audit even when it had never run
	// (`--fail-on-issues` without `--verify-links`), and contradicted doctor's
	// own note two lines above.
	let axes: std::collections::BTreeSet<&'static str> =
		rows.iter().filter_map(DoctorRow::issue_axis).collect();
	let gate = |issues: usize| -> Result<()> {
		if fail_on_issues && issues > 0 {
			let what = if axes.contains("both")
				|| (axes.contains("health") && axes.contains("links"))
			{
				"skill health and agent referrer issues"
			} else if axes.contains("links") {
				"agent referrer issues"
			} else {
				"skill health issues"
			};
			// Name the UNIT. This counts ROWS (skills); the note above counts
			// per-agent referrer records, so one dangling skill across three
			// agents legitimately prints "3 …" there and "1 …" here. Two bare
			// "N issue(s)" lines disagreeing on screen read as a bug.
			anyhow::bail!(
				"{issues} skill(s) with {what} — see the report above"
			);
		}
		Ok(())
	};

	if json {
		println!("{}", serde_json::to_string_pretty(&rows)?);
		// The report above IS the answer; without this the failure renderer
		// would append a second JSON document and every parse of stdout fails.
		crate::note_answer_on_stdout();
		return gate(issue_count);
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
	let audits = || {
		rows.iter()
			.filter_map(|row| match &row.link_audit {
				LinkAudit::NotRequested => None,
				LinkAudit::Verified { agents }
				| LinkAudit::Issues { agents } => Some(agents),
			})
			.flatten()
	};

	// Orphan masters get their OWN note. The blanket
	// "source sync --install-missing" advice pointed at reinstalling them —
	// and `delete --yes` deliberately keeps a master another agent still reads,
	// so the leftover it produces landed in that bucket. doctor was telling the
	// caller to put back what they had just removed, while a second note two
	// lines up told them to delete it.
	let orphan_masters = audits()
		.filter(|audit| audit.state == AgentLinkState::OrphanMaster)
		.count();
	if orphan_masters > 0 {
		eprintln!(
			"note: {orphan_masters} orphan master(s) — a master with no lock \
			 entry and no slot for this agent. There is no source to relink \
			 from; remove the master directory if nothing reads it. Check the \
			 other agents' rows first: an untracked master can still have a \
			 live `linked` referrer, and deleting it would dangle that link."
		);
	}

	let broken_links = audits()
		.filter(|audit| {
			audit.state != AgentLinkState::OrphanMaster
				&& audit.state.is_issue()
		})
		.count();
	if broken_links > 0 {
		// The old text was not a runnable command: `source sync` takes a
		// required `<SOURCE>` positional and needs `--yes` to write, so copying
		// it produced a clap usage error, and adding `--yes` alone produced
		// another silent dry-run.
		eprintln!(
			"note: {broken_links} agent referrer issue(s) — repair a missing or \
			 dangling link with:\n  aghub-cli {scope_flag} -a <agent> source \
			 sync <source> --skill <name> --install-missing --yes\n\
			 (<source> is the SOURCE column of `aghub-cli source list`.) \
			 Foreign links and real-path conflicts are not repaired by sync — \
			 inspect those.",
			scope_flag = if scope.resource_scope()
				== ResourceScope::ProjectOnly
			{
				"-p"
			} else {
				"-g"
			}
		);
	}
	gate(issue_count)
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
		// `tracked: false` on purpose: this is a NativeReader, and the
		// orphan-master downgrade applies only to a NeedsLink slot. If it ever
		// leaks here, a Master-reading agent with no master at all would be
		// reported as a leftover to delete rather than something missing.
		let audit = audit_agent_links(
			"gone",
			&master,
			ResourceScope::ProjectOnly,
			Some(tmp.path()),
			&[AgentType::Codex],
			false,
		);
		// `Issues`, not `Verified` — the summary must not contradict its own
		// rows, which is what let `doctor --verify-links && echo healthy` print
		// healthy over a broken tree.
		let LinkAudit::Issues { agents } = audit else {
			panic!("a missing referrer is an issue, not a clean verification")
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
