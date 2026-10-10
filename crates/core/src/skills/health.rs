//! Skill health: one scope's lock reconciled against its Master store, plus an
//! optional per-agent Referrer audit. The ONE home for `doctor`'s rows; CLI (and
//! later API) only render.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Serialize;

use crate::errors::{ConfigError, Result};
use crate::models::{AgentType, ResourceScope};
use crate::scope::WriteScope;
use crate::skills::enumerate;
use crate::skills::linker::{is_store_bookkeeping, master_store_dir, Linker};
use crate::skills::shape::{
	classify_shape, compat_roster, SkillShape, ViolationKind,
};

/// Health rows for ONE scope. Reads that scope's lock fail-CLOSED (an unreadable
/// lock is an error, never "untracked"), with the same scope→lock mapping as
/// `repair::repair_all`: global never reads a project lock.
pub fn report(
	scope: &WriteScope,
	agents: Option<&[AgentType]>,
) -> Result<Vec<DoctorRow>> {
	let locked = match scope {
		WriteScope::Global => global_locked(
			skill::lock::read_global_lock_checked().map_err(ConfigError::Io)?,
		),
		WriteScope::Project { root } => project_locked(
			skill::lock::local::read_local_lock_checked(Some(root))
				.map_err(ConfigError::Io)?,
		),
	};
	let Some(master) = master_store_dir(scope.project_root()) else {
		return Ok(Vec::new());
	};
	let mut rows = build_rows(scope.label(), &master, &locked);
	if let Some(agents) = agents {
		for row in &mut rows {
			row.link_audit = audit_agent_links(
				&row.skill,
				&master,
				scope.resource_scope(),
				scope.project_root(),
				agents,
				locked.contains_key(&row.skill),
			);
		}
	}
	Ok(rows)
}

/// On-disk state of a skill's master directory (`.aghub/<name>`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MasterState {
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
pub enum AgentLinkState {
	/// The agent holds no Referrer while the Master is healthy — installed and
	/// deliberately NOT granted to this agent. Not an issue: it is the state
	/// the `.aghub` store exists to express (it replaced `AutoCovered`).
	///
	/// With no persisted authorization it is indistinguishable from a grant
	/// removed by hand. Accepted: a second source of truth would drift.
	Withheld,
	Unsupported,
	Linked,
	Missing,
	Dangling,
	ForeignLink,
	RealPathConflict,
	/// The Referrer is a link whose target is ANOTHER link. Its canonical
	/// endpoint is still the Master, but `repair` plans a `Relink` for it.
	Chain,
	/// The Master itself is a link, or something that is not a directory. Not a
	/// per-agent fault: `classify_shape` reports it for every pair against that
	/// Master, and the row's `health` column names the same thing once.
	MasterUnusable,
	Inaccessible,
	/// This agent has no slot for the skill AND the master is untracked — a
	/// leftover (e.g. what `delete --yes` kept for another reader), not a
	/// missing link. The remedy is opposite to `Missing`'s: there is no source
	/// to re-link from, and `source sync --install-missing` would REINSTALL a
	/// skill the user just deleted.
	OrphanMaster,
}

impl AgentLinkState {
	pub fn label(self) -> &'static str {
		match self {
			Self::Withheld => "withheld",
			Self::Unsupported => "unsupported",
			Self::Linked => "linked",
			Self::Missing => "missing",
			Self::Dangling => "dangling",
			Self::ForeignLink => "foreign-link",
			Self::RealPathConflict => "real-path-conflict",
			Self::Chain => "chain",
			Self::MasterUnusable => "master-unusable",
			Self::Inaccessible => "inaccessible",
			Self::OrphanMaster => "orphan-master",
		}
	}
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentLinkAudit {
	pub agent: String,
	pub state: AgentLinkState,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub path: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum LinkAudit {
	NotRequested,
	/// Every agent row is in a healthy state.
	Verified {
		agents: Vec<AgentLinkAudit>,
	},
	/// At least one agent row is not. The summary must never contradict its
	/// rows — it is what an automated caller reads first.
	Issues {
		agents: Vec<AgentLinkAudit>,
	},
}

/// The advice bucket one broken referrer falls into. ONE note per bucket, so
/// the command a user copies actually fixes the state that produced it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Remedy {
	/// Reinstall from the source, or inspect it by hand. (`repair` also
	/// relinks a dangling or foreign one; splitting this bucket further is an
	/// open follow-up.)
	Reinstall,
	/// Re-point the Referrer at the Master: `aghub-cli repair`.
	Relink,
	/// `repair` refuses it; the store entry must be replaced by hand.
	ReplaceMaster,
	/// A master with no lock entry and no slot — remove it if nothing reads it.
	LeftoverMaster,
}

impl AgentLinkState {
	/// The remediation bucket for this state, or `None` when it is not an issue.
	///
	/// `withheld` and `unsupported` are correct resting states (not granted;
	/// cannot hold a skill), not problems.
	///
	/// The notes iterate THIS, so a new state cannot inherit advice that does
	/// not fix it: `source sync --install-missing` fixes neither `chain` (its
	/// canonical endpoint IS the Master, so `Linker::link` writes nothing) nor
	/// `master-unusable`. See docs/history/cli.md#doctor-link-audit
	pub fn remedy(self) -> Option<Remedy> {
		match self {
			Self::Withheld | Self::Unsupported | Self::Linked => None,
			// `plan_repair` gives a chain a `Relink`; `repair` really does
			// re-point it (`source sync` cannot — see above).
			Self::Chain => Some(Remedy::Relink),
			// `plan_repair` REFUSES this one. Claim no more: `verify_shape`
			// does NOT block on it, and no other flow gates on the shape.
			Self::MasterUnusable => Some(Remedy::ReplaceMaster),
			Self::OrphanMaster => Some(Remedy::LeftoverMaster),
			Self::Missing
			| Self::Dangling
			| Self::ForeignLink
			| Self::RealPathConflict
			| Self::Inaccessible => Some(Remedy::Reinstall),
		}
	}

	/// Is this state something a caller should act on? Anything with a remedy.
	pub fn is_issue(self) -> bool {
		self.remedy().is_some()
	}
}

impl LinkAudit {
	pub fn label(&self) -> String {
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
	pub fn label(&self) -> &'static str {
		match self {
			Self::Dir => "dir",
			Self::Link => "link",
			Self::Missing => "missing",
		}
	}
}

/// One skill row in the doctor report.
#[derive(Debug, Clone, Serialize)]
pub struct DoctorRow {
	pub scope: &'static str,
	pub skill: String,
	/// Displayable source (`owner/repo`, `local`, or `type:source`).
	pub source: String,
	/// True when the source is a git repo — i.e. `check`/`apply-update` can
	/// refresh it. JSON-only hint; `check --online` is authoritative.
	pub updatable: bool,
	pub master: MasterState,
	/// `ok` | `orphan-lock` (lock entry, no master on disk) | `untracked`
	/// (master on disk, no lock entry) | `master-is-symlink`.
	pub health: &'static str,
	/// Explicitly distinguishes the default Master-only audit from an optional
	/// roster-aware referrer audit.
	#[serde(rename = "linkAudit")]
	pub link_audit: LinkAudit,
}

/// Which `health` values `--fail-on-issues` gates on. ONE definition, read by
/// both [`DoctorRow::is_issue`] and [`DoctorRow::issue_axis`] — two copies
/// disagreeing would report the wrong axis in the failure message.
///
/// NOT simply `health != "ok"`: `untracked` (a skill authored in place, as in
/// this very repo) is a legitimate resting state, not a reason to fail CI.
///
/// `master-is-symlink` IS an issue: `classify_shape` calls it
/// `Violation(MasterIsLink)` and `plan_repair` refuses it, and the two axes
/// must never answer one fact differently. See docs/history/cli.md#doctor-link-audit
pub fn health_is_issue(health: &str) -> bool {
	matches!(
		health,
		"orphan-lock" | "invalid-skill" | "master-is-symlink"
	)
}

impl DoctorRow {
	/// Is this row something a caller should act on?
	///
	/// `health` is the lock ↔ Master axis and is ALWAYS computed; `link_audit`
	/// is the per-agent referrer axis and exists only under `--verify-links`.
	/// A gate that reads one and not the other is inert exactly when the other
	/// is the only thing that ran.
	pub fn is_issue(&self) -> bool {
		let health_bad = health_is_issue(self.health);
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
	pub fn issue_axis(&self) -> Option<&'static str> {
		let links_bad = matches!(&self.link_audit, LinkAudit::Issues { .. });
		let health_bad = health_is_issue(self.health);
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
///
/// **The verdict comes from [`classify_shape`], not from here** — everything
/// below renames core's answer. See docs/history/cli.md#doctor-link-audit
fn inspect_agent_link(
	master_skill: &Path,
	agent_skills_dir: &Path,
	skill_name: &str,
) -> AgentLinkState {
	let slot = agent_skills_dir.join(skill::sanitize_name(skill_name));
	// The one question core does not answer: `classify_shape` folds an
	// unreadable slot into "absent", and a permission-denied dir must not be
	// reported as a missing link.
	match std::fs::symlink_metadata(&slot) {
		Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
			return AgentLinkState::Inaccessible;
		}
		// No slot at all: answer BEFORE asking core, whose Master-first order
		// would let `MasterIsLink` outrank `Absent` and erase the leftover vs
		// missing-link distinction the caller needs (opposite remedies; pinned
		// by `third_review_sibling_shapes_stay_fixed`). The Master's own fault
		// is still named once, by the row's `health`.
		Err(_) => return AgentLinkState::Missing,
		Ok(_) => {}
	}
	match classify_shape(&slot, master_skill) {
		SkillShape::Conformant => AgentLinkState::Linked,
		// The caller downgrades this to `withheld` / `orphan-master` when a
		// live Master says the absence is deliberate.
		SkillShape::Absent => AgentLinkState::Missing,
		// A real directory occupies the slot. `UnmigratedCopy` (no Master) and
		// `ForkedCopy` (beside a live Master) differ for repair, not for a
		// reader: both are "something real sits where a link belongs".
		SkillShape::UnmigratedCopy
		| SkillShape::ForeignDir
		| SkillShape::AliasedMaster
		| SkillShape::Violation(
			ViolationKind::ForkedCopy | ViolationKind::ReferrerIsNotADir,
		) => AgentLinkState::RealPathConflict,
		SkillShape::Violation(ViolationKind::Chain { .. }) => {
			AgentLinkState::Chain
		}
		SkillShape::Violation(ViolationKind::ForeignTarget) => {
			AgentLinkState::ForeignLink
		}
		SkillShape::Violation(ViolationKind::Dangling) => {
			AgentLinkState::Dangling
		}
		SkillShape::Violation(
			ViolationKind::MasterIsLink | ViolationKind::MasterIsNotADir,
		) => AgentLinkState::MasterUnusable,
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
	let master_skill = master.join(skill::sanitize_name(skill_name));
	let roster = compat_roster(scope, project_root);
	let agents = agents
		.iter()
		.map(|agent| {
			let dirs = roster
				.iter()
				.find(|a| a.id == agent.as_str())
				.expect("roster covers every agent");
			let (state, path) = match &dirs.write {
				None => (AgentLinkState::Unsupported, None),
				Some(write) => {
					let mut agent_skills_dir = write.clone();
					let mut state = inspect_agent_link(
						&master_skill,
						&agent_skills_dir,
						skill_name,
					);
					// Empty write slot: also look in the agent's other read
					// dirs (the ones repair's compat sweep visits), and report
					// the path where it was actually FOUND.
					if state == AgentLinkState::Missing {
						for dir in dirs.compat_dirs() {
							let found = inspect_agent_link(
								&master_skill,
								dir,
								skill_name,
							);
							if found != AgentLinkState::Missing {
								state = found;
								agent_skills_dir = dir.clone();
								break;
							}
						}
					}
					// An absent slot beside a REACHABLE Master is withheld
					// (tracked) or a leftover (untracked), never a missing link.
					// `exists()` follows links, so a dangling master symlink
					// stays `missing` (`master_state` would say `Link`: a false
					// green). `Dangling` referrers do NOT downgrade — a relink
					// replaces them.
					if state == AgentLinkState::Missing && master_skill.exists()
					{
						state = if tracked {
							AgentLinkState::Withheld
						} else {
							AgentLinkState::OrphanMaster
						};
					}
					let reported_slot =
						agent_skills_dir.join(skill::sanitize_name(skill_name));
					(state, Some(reported_slot.to_string_lossy().into_owned()))
				}
			};
			AgentLinkAudit {
				agent: agent.as_str().to_string(),
				state,
				path,
			}
		})
		.collect::<Vec<AgentLinkAudit>>();
	// `verified` only when no row is an issue.
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
	enumerate::entries(master, false)
		.0
		.into_iter()
		.filter(|p| Linker::is_link(p) || p.is_dir())
		.filter_map(|p| p.file_name()?.to_str().map(str::to_string))
		// aghub's own bookkeeping (e.g. `repair`'s `.quarantine`) is not a
		// skill; listing it would fail `--fail-on-issues` forever.
		.filter(|name| !is_store_bookkeeping(name))
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
		let master_skill = master.join(skill::sanitize_name(name));
		let state = master_state(&master_skill);
		let valid_skill = matches!(state, MasterState::Dir)
			&& skill::parser::parse_skill_dir(&master_skill).is_ok_and(
				|parsed| {
					skill::sanitize_name(&parsed.name)
						== skill::sanitize_name(name)
				},
			);
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

	for dir_name in master_skills_on_disk(master) {
		// A folder a lock key already claims is that skill's row, valid or not.
		if locked.keys().any(|k| skill::sanitize_name(k) == dir_name) {
			continue;
		}
		let path = master.join(&dir_name);
		let state = master_state(&path);
		let parsed = skill::parser::parse_skill_dir(&path).ok();
		let valid_skill = matches!(state, MasterState::Dir)
			&& parsed
				.as_ref()
				.is_some_and(|p| skill::sanitize_name(&p.name) == dir_name);

		if valid_skill {
			let parsed = parsed.expect("valid_skill implies parsed is Some");
			let health = health_of(false, &state, true);
			rows.push(DoctorRow {
				scope,
				skill: parsed.name,
				source: "—".to_string(),
				updatable: false,
				master: state,
				health,
				link_audit: LinkAudit::NotRequested,
			});
		} else {
			let health = health_of(false, &state, false);
			rows.push(DoctorRow {
				scope,
				skill: dir_name,
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
/// Takes the ALREADY-READ lock: a second, fail-open read could see a
/// truncated file. See docs/history/cli.md#lock-snapshot-fails-closed
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

	// Antigravity reads `.agent/skills` but writes `.agents/skills`. A Referrer
	// parked in the read-only dir must audit as LINKED, not `withheld` — the
	// agent really is loading it, and `withheld` is an issue the user cannot
	// act on. Revert-prove by deleting the `compat_dirs` fallback loop in
	// `audit_agent_links`: this goes `withheld`.
	#[cfg(unix)]
	#[test]
	fn a_referrer_in_a_private_read_only_dir_is_linked_not_withheld() {
		use std::os::unix::fs::symlink;

		let tmp = tempfile::tempdir().unwrap();
		let root = tmp.path();
		let master = root.join(".aghub");
		write_skill(&master.join("foo"), "foo");
		let legacy = root.join(".agent/skills");
		std::fs::create_dir_all(&legacy).unwrap();
		symlink(master.join("foo"), legacy.join("foo")).unwrap();

		let audit = audit_agent_links(
			"foo",
			&master,
			ResourceScope::ProjectOnly,
			Some(root),
			&[AgentType::Antigravity],
			true,
		);

		let LinkAudit::Verified { agents } = &audit else {
			panic!("a readable Referrer is not an issue: {audit:?}");
		};
		assert_eq!(agents[0].state, AgentLinkState::Linked);
		assert!(
			agents[0]
				.path
				.as_deref()
				.is_some_and(|p| p.contains(".agent/skills")),
			"the row must name where it was FOUND, got {:?}",
			agents[0].path
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
		// Built with the SAME joins production uses — the descriptor's
		// `root.join(".codex/skills")`, then the skill name — because this
		// compares STRINGS. Collapsing the tail into one
		// `join(".codex/skills/gone")` is identical on Unix but yields
		// `...\.codex/skills/gone` on Windows against production's
		// `...\.codex/skills\gone`, so it passes every local preflight and
		// fails only on the CI Windows leg.
		assert_eq!(
			agents[0].path.as_deref(),
			Some(
				tmp.path()
					.join(".codex/skills")
					.join("gone")
					.to_string_lossy()
					.as_ref()
			)
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

	/// A two-hop chain (agent Referrer -> shared slot -> Master) is what npx's
	/// `createSymlink` leaves behind. Its ENDPOINT is the Master, so comparing
	/// canonicalized endpoints certifies it healthy — while `plan_repair` gives
	/// the same layout a `Relink`. doctor must not answer "clean" where repair
	/// answers "fix": that contradiction is what routing through
	/// `skills::shape::classify_shape` removes.
	#[cfg(unix)]
	#[test]
	fn a_two_hop_chain_is_an_issue_not_a_healthy_link() {
		use std::os::unix::fs::symlink;

		let tmp = tempfile::tempdir().unwrap();
		let root = std::fs::canonicalize(tmp.path()).unwrap();
		let master = root.join(".aghub");
		write_skill(&master.join("foo"), "foo");

		let shared = root.join(".agents/skills");
		std::fs::create_dir_all(&shared).unwrap();
		symlink(master.join("foo"), shared.join("foo")).unwrap();

		let claude = root.join(".claude/skills");
		std::fs::create_dir_all(&claude).unwrap();
		symlink(shared.join("foo"), claude.join("foo")).unwrap();

		assert_eq!(
			std::fs::canonicalize(claude.join("foo")).unwrap(),
			std::fs::canonicalize(master.join("foo")).unwrap(),
			"precondition: the endpoints DO agree, so endpoint equality alone \
			 would pass this"
		);

		let audit = audit_agent_links(
			"foo",
			&master,
			ResourceScope::ProjectOnly,
			Some(&root),
			&[AgentType::Claude],
			true,
		);
		let LinkAudit::Issues { agents } = &audit else {
			panic!("a chain is what repair relinks; doctor must report it: {audit:?}")
		};
		assert_eq!(agents[0].state, AgentLinkState::Chain);
	}

	/// `.agents/skills` symlinked into the store: the leaf lstats as a real
	/// directory while BEING the Master. core calls that `AliasedMaster` and
	/// refuses to act on it — `plan_repair` returns `Refuse { AliasedMaster }`
	/// and `verify_shape` blocks the delete — so doctor reporting it is the
	/// consistent answer, not a false alarm.
	///
	/// A PIN, deliberately: `SkillShape::is_actionable()` is `false` here, so
	/// routing `is_issue` through it would not clear this row either.
	#[cfg(unix)]
	#[test]
	fn an_aliased_master_stays_an_issue_because_repair_refuses_it() {
		use std::os::unix::fs::symlink;

		let tmp = tempfile::tempdir().unwrap();
		let root = std::fs::canonicalize(tmp.path()).unwrap();
		let master = root.join(".aghub");
		write_skill(&master.join("foo"), "foo");
		std::fs::create_dir_all(root.join(".cline")).unwrap();
		symlink(&master, root.join(".cline/skills")).unwrap();

		let slot = root.join(".cline/skills/foo");
		assert!(
			!Linker::is_link(&slot) && slot.is_dir(),
			"precondition: the leaf lstats as a real directory"
		);

		let audit = audit_agent_links(
			"foo",
			&master,
			ResourceScope::ProjectOnly,
			Some(&root),
			&[AgentType::Cline],
			true,
		);
		let LinkAudit::Issues { agents } = &audit else {
			panic!("repair refuses this layout; doctor must not call it clean: {audit:?}")
		};
		assert_eq!(agents[0].state, AgentLinkState::RealPathConflict);
	}

	/// `MasterUnusable` fires only when the agent HAS a slot. With no slot the
	/// row must stay `Missing` so the caller can downgrade it to
	/// `orphan-master` — a leftover Master and a missing link have opposite
	/// remedies, and `classify_shape` checking the Master first would otherwise
	/// answer both with one state. The Master's own fault is reported once, in
	/// the row's `health` column.
	#[cfg(unix)]
	#[test]
	fn a_linked_master_is_a_per_agent_fault_only_where_a_slot_exists() {
		use std::os::unix::fs::symlink;

		let tmp = tempfile::tempdir().unwrap();
		let root = std::fs::canonicalize(tmp.path()).unwrap();
		let elsewhere = root.join("elsewhere/foo");
		write_skill(&elsewhere, "foo");

		// The Master itself is a link, which `classify_shape` reports before
		// it looks at any Referrer.
		let master = root.join(".aghub");
		std::fs::create_dir_all(&master).unwrap();
		symlink(&elsewhere, master.join("foo")).unwrap();

		let claude = root.join(".claude/skills");
		std::fs::create_dir_all(&claude).unwrap();

		assert_eq!(
			inspect_agent_link(&master.join("foo"), &claude, "foo"),
			AgentLinkState::Missing,
			"no slot: the Master's fault must not outrank this agent's absence"
		);

		symlink(master.join("foo"), claude.join("foo")).unwrap();
		assert_eq!(
			inspect_agent_link(&master.join("foo"), &claude, "foo"),
			AgentLinkState::MasterUnusable,
			"slot present: now the unusable Master IS this row's answer"
		);
	}

	#[test]
	fn build_rows_handles_unsanitized_skill_name_in_lock() {
		let tmp = tempfile::tempdir().unwrap();
		let master = tmp.path();
		let skill_dir = master.join("pdf-tools");
		std::fs::create_dir_all(&skill_dir).unwrap();
		std::fs::write(
			skill_dir.join("SKILL.md"),
			"---\nname: PDF Tools\ndescription: valid\n---\n",
		)
		.unwrap();

		let locked = BTreeMap::from([(
			"PDF Tools".to_string(),
			LockedSkill {
				source: "owner/repo".to_string(),
				source_type: "github".to_string(),
				skill_path: Some("pdf-tools/SKILL.md".to_string()),
			},
		)]);
		let rows = build_rows("global", master, &locked);
		assert_eq!(rows.len(), 1);
		assert_eq!(rows[0].skill, "PDF Tools");
		assert_eq!(rows[0].health, "ok");
		assert_eq!(rows[0].master, MasterState::Dir);
		assert!(rows[0].updatable);
	}

	#[test]
	fn build_rows_marks_untracked_unsanitized_skill_name() {
		let tmp = tempfile::tempdir().unwrap();
		let master = tmp.path();
		let skill_dir = master.join("pdf-tools");
		std::fs::create_dir_all(&skill_dir).unwrap();
		std::fs::write(
			skill_dir.join("SKILL.md"),
			"---\nname: PDF Tools\ndescription: valid\n---\n",
		)
		.unwrap();

		let rows = build_rows("global", master, &BTreeMap::new());
		assert_eq!(rows.len(), 1);
		assert_eq!(rows[0].skill, "PDF Tools");
		assert_eq!(rows[0].health, "untracked");
		assert_eq!(rows[0].master, MasterState::Dir);
	}

	#[cfg(unix)]
	#[test]
	fn inspect_agent_link_resolves_unsanitized_skill_name() {
		use std::os::unix::fs::symlink;

		let tmp = tempfile::tempdir().unwrap();
		let master = tmp.path().join("master/pdf-tools");
		write_skill(&master, "PDF Tools");

		let agent_dir = tmp.path().join("agent-skills");
		std::fs::create_dir_all(&agent_dir).unwrap();
		symlink(&master, agent_dir.join("pdf-tools")).unwrap();

		assert_eq!(
			inspect_agent_link(&master, &agent_dir, "PDF Tools"),
			AgentLinkState::Linked
		);
	}

	#[test]
	fn build_rows_untracked_folder_with_disagreeing_name_is_invalid_skill() {
		let tmp = tempfile::tempdir().unwrap();
		let master = tmp.path();
		let skill_dir = master.join("foo");
		std::fs::create_dir_all(&skill_dir).unwrap();
		std::fs::write(
			skill_dir.join("SKILL.md"),
			"---\nname: bar\ndescription: name disagrees with folder\n---\n",
		)
		.unwrap();

		let rows = build_rows("global", master, &BTreeMap::new());
		assert_eq!(rows.len(), 1);
		assert_eq!(rows[0].skill, "foo");
		assert_eq!(rows[0].health, "invalid-skill");
		assert_eq!(rows[0].master, MasterState::Dir);
	}

	#[test]
	fn build_rows_nested_skill_in_group_dir_produces_no_phantom_row() {
		let tmp = tempfile::tempdir().unwrap();
		let master = tmp.path();
		let nested_skill_dir = master.join("grp/sub");
		std::fs::create_dir_all(&nested_skill_dir).unwrap();
		std::fs::write(
			nested_skill_dir.join("SKILL.md"),
			"---\nname: sub\ndescription: nested\n---\n",
		)
		.unwrap();

		let top_level_sub = master.join("sub");
		std::fs::create_dir_all(&top_level_sub).unwrap();
		std::fs::write(
			top_level_sub.join("SKILL.md"),
			"---\nname: other\ndescription: top-level sub with mismatch\n---\n",
		)
		.unwrap();

		let rows = build_rows("global", master, &BTreeMap::new());
		assert_eq!(rows.len(), 2);
		let grp_row = rows
			.iter()
			.find(|r| r.skill == "grp")
			.expect("grp row exists");
		assert_eq!(grp_row.health, "invalid-skill");
		assert_eq!(grp_row.master, MasterState::Dir);

		let sub_row = rows
			.iter()
			.find(|r| r.skill == "sub")
			.expect("sub row exists");
		assert_eq!(sub_row.health, "invalid-skill");
		assert_eq!(sub_row.master, MasterState::Dir);
	}

	#[test]
	fn build_rows_malformed_sibling_does_not_affect_healthy_untracked_skill() {
		let tmp = tempfile::tempdir().unwrap();
		let master = tmp.path();
		let healthy = master.join("pdf-tools");
		std::fs::create_dir_all(&healthy).unwrap();
		std::fs::write(
			healthy.join("SKILL.md"),
			"---\nname: PDF Tools\ndescription: healthy\n---\n",
		)
		.unwrap();

		let broken = master.join("broken");
		std::fs::create_dir_all(&broken).unwrap();
		std::fs::write(broken.join("SKILL.md"), "not: valid: yaml: [{{")
			.unwrap();

		let rows = build_rows("global", master, &BTreeMap::new());
		assert_eq!(rows.len(), 2);

		let healthy_row = rows
			.iter()
			.find(|r| r.skill == "PDF Tools")
			.expect("healthy row exists");
		assert_eq!(healthy_row.health, "untracked");
		assert_eq!(healthy_row.master, MasterState::Dir);

		let broken_row = rows
			.iter()
			.find(|r| r.skill == "broken")
			.expect("broken row exists");
		assert_eq!(broken_row.health, "invalid-skill");
		assert_eq!(broken_row.master, MasterState::Dir);
	}

	#[test]
	fn build_rows_lock_entry_whose_frontmatter_sanitizes_to_the_folder_is_ok() {
		let tmp = tempfile::tempdir().unwrap();
		let master = tmp.path();
		let skill_dir = master.join("pdf-tools");
		std::fs::create_dir_all(&skill_dir).unwrap();
		std::fs::write(
			skill_dir.join("SKILL.md"),
			"---\nname: Pdf-Tools\ndescription: casing differs from lock\n---\n",
		)
		.unwrap();

		let locked = BTreeMap::from([(
			"PDF Tools".to_string(),
			LockedSkill {
				source: "owner/repo".to_string(),
				source_type: "github".to_string(),
				skill_path: Some("pdf-tools/SKILL.md".to_string()),
			},
		)]);
		let rows = build_rows("global", master, &locked);
		// `Pdf-Tools` sanitizes to the folder name, so the lock row is healthy
		// and the folder is not also reported as an untracked duplicate.
		assert_eq!(rows.len(), 1);
		let lock_row = rows
			.iter()
			.find(|r| r.skill == "PDF Tools")
			.expect("lock row exists");
		assert_eq!(lock_row.health, "ok");
		assert_eq!(lock_row.master, MasterState::Dir);
	}

	struct EnvVarGuard(&'static str, Option<std::ffi::OsString>);

	impl EnvVarGuard {
		fn set(key: &'static str, value: &Path) -> Self {
			let previous = std::env::var_os(key);
			std::env::set_var(key, value);
			Self(key, previous)
		}
	}

	impl Drop for EnvVarGuard {
		fn drop(&mut self) {
			match self.1.take() {
				Some(value) => std::env::set_var(self.0, value),
				None => std::env::remove_var(self.0),
			}
		}
	}

	/// Points `HOME` and `AGHUB_DATA_DIR` at fresh dirs under `root`. Callers
	/// take `GlobalLockGuard` first, so these guards drop before it.
	fn isolate_home(root: &Path) -> (EnvVarGuard, EnvVarGuard) {
		let home = root.join("home");
		let data = root.join("data");
		std::fs::create_dir_all(&home).unwrap();
		std::fs::create_dir_all(&data).unwrap();
		(
			EnvVarGuard::set("HOME", &home),
			EnvVarGuard::set("AGHUB_DATA_DIR", &data),
		)
	}

	/// Same entry literal as `enumerate::tests::seed_lock`.
	fn seed_project_lock(root: &Path, key: &str) {
		skill::add_skill_to_local_lock(
			key,
			skill::LocalSkillLockEntry {
				source_url: None,
				ref_commit: None,
				source: "o/r".to_string(),
				ref_name: None,
				source_type: "github".to_string(),
				computed_hash: "h".to_string(),
				skill_path: None,
			},
			Some(root),
		)
		.unwrap();
	}

	#[cfg(unix)]
	#[test]
	fn report_unsanitized_name_is_healthy_and_linked() {
		use crate::skills::prune::test_lock::GlobalLockGuard;
		use std::os::unix::fs::symlink;

		let _lock = GlobalLockGuard::new();
		let tmp = tempfile::tempdir().unwrap();
		let root = std::fs::canonicalize(tmp.path()).unwrap();
		let (_home, _data) = isolate_home(&root);
		let master = root.join(".aghub/pdf-tools");
		write_skill(&master, "PDF Tools");
		let claude = root.join(".claude/skills");
		std::fs::create_dir_all(&claude).unwrap();
		symlink(&master, claude.join("pdf-tools")).unwrap();
		seed_project_lock(&root, "PDF Tools");

		let rows = report(
			&WriteScope::project(root.clone()),
			Some(&[AgentType::Claude][..]),
		)
		.unwrap();
		assert_eq!(rows.len(), 1);
		assert_eq!(rows[0].skill, "PDF Tools");
		assert_eq!(rows[0].health, "ok");
		assert_eq!(rows[0].master, MasterState::Dir);
		let LinkAudit::Verified { agents } = &rows[0].link_audit else {
			panic!(
				"a readable Referrer is not an issue: {:?}",
				rows[0].link_audit
			);
		};
		assert_eq!(agents[0].state, AgentLinkState::Linked);
	}

	#[cfg(unix)]
	#[test]
	fn report_dangling_referrer_is_an_issue() {
		use crate::skills::prune::test_lock::GlobalLockGuard;
		use std::os::unix::fs::symlink;

		let _lock = GlobalLockGuard::new();
		let tmp = tempfile::tempdir().unwrap();
		let root = std::fs::canonicalize(tmp.path()).unwrap();
		let (_home, _data) = isolate_home(&root);
		let master = root.join(".aghub/pdf-tools");
		write_skill(&master, "PDF Tools");
		let claude = root.join(".claude/skills");
		std::fs::create_dir_all(&claude).unwrap();
		symlink(root.join("gone"), claude.join("pdf-tools")).unwrap();
		seed_project_lock(&root, "PDF Tools");

		let rows = report(
			&WriteScope::project(root.clone()),
			Some(&[AgentType::Claude][..]),
		)
		.unwrap();
		assert_eq!(rows.len(), 1);
		let LinkAudit::Issues { agents } = &rows[0].link_audit else {
			panic!(
				"a dangling Referrer must be an issue: {:?}",
				rows[0].link_audit
			);
		};
		assert_eq!(agents[0].state, AgentLinkState::Dangling);
		assert!(rows[0].is_issue());
	}

	#[test]
	fn report_lowercase_skill_md_is_ok() {
		use crate::skills::prune::test_lock::GlobalLockGuard;

		let _lock = GlobalLockGuard::new();
		let tmp = tempfile::tempdir().unwrap();
		let root = std::fs::canonicalize(tmp.path()).unwrap();
		let (_home, _data) = isolate_home(&root);
		let master = root.join(".aghub/lower");
		std::fs::create_dir_all(&master).unwrap();
		std::fs::write(
			master.join("skill.md"),
			"---\nname: lower\ndescription: test\n---\n",
		)
		.unwrap();
		seed_project_lock(&root, "lower");

		let rows = report(&WriteScope::project(root.clone()), None).unwrap();
		assert_eq!(rows.len(), 1);
		assert_eq!(rows[0].health, "ok");
	}

	#[cfg(unix)]
	#[test]
	fn report_unreadable_skill_md_is_invalid_not_orphan() {
		use crate::skills::prune::test_lock::GlobalLockGuard;
		use std::os::unix::fs::PermissionsExt;

		// EACCES does not apply to root — skip there (matches linker/mod.rs).
		if unsafe { libc::geteuid() } == 0 {
			return;
		}
		let _lock = GlobalLockGuard::new();
		let tmp = tempfile::tempdir().unwrap();
		let root = std::fs::canonicalize(tmp.path()).unwrap();
		let (_home, _data) = isolate_home(&root);
		let master = root.join(".aghub/locked");
		write_skill(&master, "locked");
		let skill_md = master.join("SKILL.md");
		std::fs::set_permissions(
			&skill_md,
			std::fs::Permissions::from_mode(0o000),
		)
		.unwrap();
		seed_project_lock(&root, "locked");

		let rows = report(&WriteScope::project(root.clone()), None);
		// Restore before any assertion can fail, so the tempdir still drops.
		let _ = std::fs::set_permissions(
			&skill_md,
			std::fs::Permissions::from_mode(0o644),
		);
		let rows = rows.unwrap();
		assert_eq!(rows.len(), 1);
		assert_eq!(rows[0].health, "invalid-skill");
		assert_eq!(rows[0].master, MasterState::Dir);
	}

	#[cfg(unix)]
	#[test]
	fn report_compat_dir_shared_with_other_agents_agrees_with_repair() {
		use crate::skills::prune::test_lock::GlobalLockGuard;
		use std::os::unix::fs::symlink;

		let _lock = GlobalLockGuard::new();
		let tmp = tempfile::tempdir().unwrap();
		let root = std::fs::canonicalize(tmp.path()).unwrap();
		let (_home, _data) = isolate_home(&root);
		let master = root.join(".aghub/foo");
		write_skill(&master, "foo");
		seed_project_lock(&root, "foo");
		// Grok's write slot is empty; it reads the shared `.agents/skills`.
		let shared = root.join(".agents/skills");
		std::fs::create_dir_all(&shared).unwrap();
		symlink(&master, shared.join("foo")).unwrap();
		std::fs::create_dir_all(root.join(".grok/skills")).unwrap();

		assert!(crate::skills::shape::readers_of(
			ResourceScope::ProjectOnly,
			Some(&root),
			"foo"
		)
		.contains(&"grok"));
		let rows = report(
			&WriteScope::project(root.clone()),
			Some(&[AgentType::Grok][..]),
		)
		.unwrap();
		let LinkAudit::Verified { agents } = &rows[0].link_audit else {
			panic!(
				"a shared-slot Referrer is readable: {:?}",
				rows[0].link_audit
			);
		};
		assert_eq!(agents[0].state, AgentLinkState::Linked);
		assert_eq!(
			agents[0].path.as_deref(),
			Some(
				root.join(".agents/skills")
					.join("foo")
					.to_string_lossy()
					.as_ref()
			)
		);
	}

	#[test]
	fn report_global_scope_does_not_read_project_lock() {
		use crate::skills::prune::test_lock::GlobalLockGuard;

		let _lock = GlobalLockGuard::new();
		let tmp = tempfile::tempdir().unwrap();
		let root = std::fs::canonicalize(tmp.path()).unwrap();
		let (_home, _data) = isolate_home(&root);
		let proj = root.join("home/proj");
		std::fs::create_dir_all(&proj).unwrap();
		skill::add_skill_to_local_lock(
			"proj-only",
			skill::LocalSkillLockEntry {
				source_url: None,
				ref_commit: None,
				source: "o/r".to_string(),
				ref_name: None,
				source_type: "github".to_string(),
				computed_hash: "h".to_string(),
				skill_path: None,
			},
			Some(&proj),
		)
		.unwrap();
		let state = std::path::PathBuf::from(
			std::env::var_os("XDG_STATE_HOME").expect("set by GlobalLockGuard"),
		);
		let lock_dir = state.join("skills");
		std::fs::create_dir_all(&lock_dir).unwrap();
		std::fs::write(
			lock_dir.join(".skill-lock.json"),
			r#"{"version":3,"skills":{"glob-only":{"source":"owner/glob","sourceType":"github","sourceUrl":"https://github.com/owner/glob","skillPath":"SKILL.md","skillFolderHash":"","installedAt":"t","updatedAt":"t"}}}"#,
		)
		.unwrap();

		let rows = report(&WriteScope::Global, None).unwrap();
		let names: Vec<&str> = rows.iter().map(|r| r.skill.as_str()).collect();
		assert_eq!(names, vec!["glob-only"]);
		assert_eq!(rows[0].health, "orphan-lock");
	}

	#[test]
	fn report_unreadable_lock_fails_closed() {
		use crate::skills::prune::test_lock::GlobalLockGuard;

		let _lock = GlobalLockGuard::new();
		let tmp = tempfile::tempdir().unwrap();
		let root = std::fs::canonicalize(tmp.path()).unwrap();
		let (_home, _data) = isolate_home(&root);
		let lock_path = root.join("skills-lock.json");
		std::fs::write(&lock_path, "not json at all").unwrap();

		let err = report(&WriteScope::project(root.clone()), None).unwrap_err();
		assert!(err.to_string().contains("could not be read"), "{err}");
		assert_eq!(
			std::fs::read_to_string(&lock_path).unwrap(),
			"not json at all"
		);
	}
}
