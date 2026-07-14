//! `aghub-cli doctor` — read-only skill health across scopes.
//!
//! Reconciles each scope's skill lock against the on-disk universal master
//! (`.agents/skills`): what's installed, where it came from (git → updatable, or
//! local), and whether disk and lock agree. One table instead of
//! cross-referencing `source list` + `check` + `prune-lock`. Never writes.

use std::collections::BTreeMap;
use std::path::Path;

use aghub_core::paths::find_project_root;
use aghub_core::skills::linker::universal_canonical_dir;
use anyhow::{bail, Result};
use serde::Serialize;
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
	/// refresh it. A display hint; `check --online` is authoritative.
	updatable: bool,
	master: MasterState,
	/// `ok` | `orphan-lock` (lock entry, no master on disk) | `untracked`
	/// (master on disk, no lock entry) | `master-is-symlink`.
	health: &'static str,
}

/// Health verdict from lock membership + on-disk master state. Pure so it unit
/// tests without touching the filesystem.
fn health_of(tracked: bool, master: &MasterState) -> &'static str {
	match (tracked, master) {
		(true, MasterState::Dir) => "ok",
		(true, MasterState::Link) => "master-is-symlink",
		(true, MasterState::Missing) => "orphan-lock",
		// Untracked rows are generated from the disk scan, so they are always
		// present on disk; the missing arm cannot occur but stays exhaustive.
		(false, MasterState::Missing) => "orphan-lock",
		(false, _) => "untracked",
	}
}

/// Human source label + whether it's a git source (updatable). Pure.
fn source_display(source: &str, source_type: &str) -> (String, bool) {
	let t = source_type.to_ascii_lowercase();
	if t == "local" || source.is_empty() {
		return ("local".to_string(), false);
	}
	let git = matches!(t.as_str(), "github" | "git" | "gitlab" | "bitbucket");
	// github shorthand is already `owner/repo`; other providers keep their type
	// prefix so `mintlify:bun.com` doesn't read as a git repo.
	let label = if t == "github" {
		source.to_string()
	} else {
		format!("{source_type}:{source}")
	};
	(label, git)
}

/// On-disk state of the master path for one skill.
fn master_state(path: &Path) -> MasterState {
	match std::fs::symlink_metadata(path) {
		Err(_) => MasterState::Missing,
		Ok(md) if md.file_type().is_symlink() => MasterState::Link,
		Ok(_) => MasterState::Dir,
	}
}

/// Skill dir names under the master that actually contain a `SKILL.md`.
fn master_skills_on_disk(master: &Path) -> Vec<String> {
	let Ok(rd) = std::fs::read_dir(master) else {
		return Vec::new();
	};
	rd.filter_map(|e| e.ok())
		.filter(|e| e.path().join("SKILL.md").is_file())
		.filter_map(|e| e.file_name().into_string().ok())
		.collect()
}

/// Build the rows for one scope: every lock entry reconciled against the master,
/// plus any master skill with no lock entry (`untracked`).
fn build_rows(
	scope: &'static str,
	master: &Path,
	locked: &BTreeMap<String, (String, String)>,
) -> Vec<DoctorRow> {
	let mut rows = Vec::new();
	for (name, (source, source_type)) in locked {
		let state = master_state(&master.join(name));
		let (label, updatable) = source_display(source, source_type);
		let health = health_of(true, &state);
		rows.push(DoctorRow {
			scope,
			skill: name.clone(),
			source: label,
			updatable,
			master: state,
			health,
		});
	}
	for name in master_skills_on_disk(master) {
		if !locked.contains_key(&name) {
			rows.push(DoctorRow {
				scope,
				skill: name,
				source: "—".to_string(),
				updatable: false,
				master: MasterState::Dir,
				health: "untracked",
			});
		}
	}
	rows.sort_by(|a, b| a.skill.cmp(&b.skill));
	rows
}

/// Global lock entries reduced to `(name → (source, source_type))`.
fn global_locked() -> BTreeMap<String, (String, String)> {
	skill::get_all_locked_skills()
		.into_iter()
		.map(|(k, e)| (k, (e.source, e.source_type)))
		.collect()
}

/// Project lock entries reduced to `(name → (source, source_type))`.
fn project_locked(root: &Path) -> BTreeMap<String, (String, String)> {
	skill::read_local_lock(Some(root))
		.skills
		.into_iter()
		.map(|(k, e)| (k, (e.source, e.source_type)))
		.collect()
}

/// Dispatch the `doctor` subcommand. `-g` global only, `-p` project only,
/// default/`--all` = global plus the current project (when a root is detected).
pub fn execute(
	global: bool,
	project: bool,
	all: bool,
	json: bool,
) -> Result<()> {
	if global && project {
		bail!("choose either -g or -p, not both");
	}
	let _ = all; // default already spans global + project; --all is the explicit spelling.

	let cwd = std::env::current_dir()?;
	let project_root = find_project_root(&cwd);

	let (want_global, want_project) = if global {
		(true, false)
	} else if project {
		(false, true)
	} else {
		(true, true)
	};

	let mut rows: Vec<DoctorRow> = Vec::new();
	if want_global {
		if let Some(master) = universal_canonical_dir(None) {
			rows.extend(build_rows("global", &master, &global_locked()));
		}
	}
	if want_project {
		match project_root {
			Some(root) => {
				if let Some(master) = universal_canonical_dir(Some(&root)) {
					rows.extend(build_rows(
						"project",
						&master,
						&project_locked(&root),
					));
				}
			}
			None if project => bail!(
				"project scope requires a project root, but none was found from \
				 the current directory (need an agent marker like .claude/, \
				 .opencode/, .mcp.json, …)"
			),
			None => {}
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
	builder.push_record(["SCOPE", "SKILL", "SOURCE", "MASTER", "HEALTH"]);
	for r in &rows {
		builder.push_record([
			r.scope.to_string(),
			r.skill.clone(),
			r.source.clone(),
			r.master.label().to_string(),
			r.health.to_string(),
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
			"note: {untracked} untracked skill(s) on disk with no lock — install \
			 via `source sync` to track for updates"
		);
	}
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn health_ok_when_tracked_and_master_is_a_dir() {
		assert_eq!(health_of(true, &MasterState::Dir), "ok");
	}

	#[test]
	fn health_orphan_lock_when_tracked_but_master_missing() {
		assert_eq!(health_of(true, &MasterState::Missing), "orphan-lock");
	}

	#[test]
	fn health_untracked_when_on_disk_but_not_locked() {
		assert_eq!(health_of(false, &MasterState::Dir), "untracked");
	}

	#[test]
	fn health_flags_a_master_that_is_itself_a_symlink() {
		assert_eq!(health_of(true, &MasterState::Link), "master-is-symlink");
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
	fn build_rows_marks_untracked_master_skill() {
		let tmp = tempfile::tempdir().unwrap();
		let master = tmp.path();
		// A skill dir on disk with a SKILL.md but no lock entry.
		let d = master.join("loose");
		std::fs::create_dir_all(&d).unwrap();
		std::fs::write(d.join("SKILL.md"), "x").unwrap();
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
			("owner/repo".to_string(), "github".to_string()),
		);
		let rows = build_rows("global", tmp.path(), &locked);
		assert_eq!(rows.len(), 1);
		assert_eq!(rows[0].skill, "gone");
		assert_eq!(rows[0].health, "orphan-lock");
		assert!(rows[0].updatable);
	}
}
