use crate::commands::check::{self, SkillUpdateView, StatusView};
use crate::ResourceType;
use aghub_core::models::ResourceScope;
use anyhow::{anyhow, bail, Result};
use serde_json::json;
use skill_update::mutation::LockedSkillsResyncError;
use std::path::Path;

pub fn execute(
	resource: ResourceType,
	name: String,
	scope: ResourceScope,
	project_root: Option<&Path>,
	yes: bool,
	json: bool,
) -> Result<()> {
	match resource {
		ResourceType::Skills => {}
		// Unreachable from the CLI (clap rejects `mcps` at parse time via the
		// narrowed `SkillResource`); kept because this fn takes `ResourceType`.
		ResourceType::Mcps => bail!("`apply-update` only supports skills"),
	}
	if !yes {
		bail!("refusing to overwrite skill files without --yes");
	}
	let report = skill_update::mutation::resync_locked_skill(
		skill_update::mutation::LockedResyncRequest {
			name: &name,
			scope,
			project_root,
		},
		&skill_update::GitFetcher::new(),
		&crate::commands::source::EnvTokenResolver,
	)
	.map_err(|error| locked_resync_error(&name, error))?;
	let paths = report.swapped;
	let updated_hash = report.updated_hash;
	if json {
		println!(
			"{}",
			serde_json::to_string_pretty(&json!({
				"success": true,
				"name": name,
				"scope": scope_name(scope),
				"updatedHash": updated_hash,
				"paths": paths
					.iter()
					.map(|p| p.display().to_string())
					.collect::<Vec<_>>(),
				"error": null,
			}))?
		);
		return Ok(());
	}
	println!("updated skill '{name}' ({} scope)", scope_name(scope));
	for path in &paths {
		println!("  {}", path.display());
	}
	println!("hash: {updated_hash}");
	Ok(())
}

/// What `apply-update --outdated` would do, picked from the SAME views
/// `check --online` prints: every update-available row is a target, every
/// renamed row is reported and left alone (a rename is `source
/// accept-rename`'s transaction, not an in-place resync).
#[derive(Debug, Default, PartialEq)]
struct OutdatedPlan {
	names: Vec<String>,
	renamed: Vec<(String, String)>,
}

fn outdated_plan(views: &[SkillUpdateView]) -> OutdatedPlan {
	let mut plan = OutdatedPlan::default();
	for view in views {
		match &view.status {
			StatusView::UpdateAvailable { .. } => {
				plan.names.push(view.name.clone())
			}
			StatusView::Renamed { new_name } => {
				plan.renamed.push((view.name.clone(), new_name.clone()))
			}
			StatusView::UpToDate | StatusView::Uncheckable { .. } => {}
		}
	}
	plan
}

/// `apply-update skills --outdated`: the CLI's "update all" — check every
/// locked skill in ONE scope online, then resync the outdated ones through the
/// same batch seam the desktop's update-all uses. Without `--yes` it previews.
pub fn execute_outdated(
	scope: ResourceScope,
	project_root: Option<&Path>,
	yes: bool,
	json: bool,
) -> Result<()> {
	let want_global = match scope {
		ResourceScope::GlobalOnly => true,
		ResourceScope::ProjectOnly => false,
		// The scope table rejects --all before dispatch.
		ResourceScope::Both => {
			bail!("apply-update requires --global or --project, not --all")
		}
	};
	let locks = crate::commands::read_locks_checked(
		want_global,
		if want_global { None } else { project_root },
	)?;
	let views = check::collect_update_views(locks, project_root, true)?;
	let plan = outdated_plan(&views);
	let renamed_json: Vec<_> = plan
		.renamed
		.iter()
		.map(|(name, new_name)| json!({ "name": name, "newName": new_name }))
		.collect();

	if !yes || plan.names.is_empty() {
		if json {
			println!(
				"{}",
				serde_json::to_string_pretty(&json!({
					"dryRun": !yes,
					"scope": scope_name(scope),
					"skills": plan.names,
					"renamed": renamed_json,
					"results": [],
				}))?
			);
			return Ok(());
		}
		if plan.names.is_empty() {
			println!("Nothing to update ({} scope).", scope_name(scope));
		} else {
			println!(
				"{} skill(s) can be updated ({} scope; pass --yes to apply — \
				 this OVERWRITES local edits to them):",
				plan.names.len(),
				scope_name(scope)
			);
			for name in &plan.names {
				println!("  would update: {name}");
			}
		}
		print_renamed_note(&plan.renamed);
		return Ok(());
	}

	let outcomes = skill_update::mutation::resync_locked_skills(
		skill_update::mutation::LockedSkillsResyncRequest {
			// The names come from this run's own lock read, so there is no
			// independent Sources row to assert against.
			source_group: None,
			names: &plan.names,
			scope,
			project_root,
		},
		&skill_update::GitFetcher::new(),
		&crate::commands::source::EnvTokenResolver,
	)
	.map_err(|error| match error {
		LockedSkillsResyncError::EmptyRequest => {
			anyhow!("no skills to update")
		}
		LockedSkillsResyncError::Preflight(error) => {
			locked_resync_error("", error)
		}
	})?;

	let rows: Vec<(String, Result<String, String>)> = outcomes
		.into_iter()
		.map(|row| {
			let result = row.outcome.map(|report| report.updated_hash).map_err(
				|error| locked_resync_error(&row.name, error).to_string(),
			);
			(row.name, result)
		})
		.collect();
	let failed = rows.iter().filter(|(_, r)| r.is_err()).count();

	if json {
		let results: Vec<_> = rows
			.iter()
			.map(|(name, result)| match result {
				Ok(hash) => json!({
					"name": name,
					"success": true,
					"updatedHash": hash,
					"error": null,
				}),
				Err(error) => json!({
					"name": name,
					"success": false,
					"updatedHash": null,
					"error": error,
				}),
			})
			.collect();
		println!(
			"{}",
			serde_json::to_string_pretty(&json!({
				"dryRun": false,
				"scope": scope_name(scope),
				"skills": plan.names,
				"renamed": renamed_json,
				"results": results,
			}))?
		);
	} else {
		for (name, result) in &rows {
			match result {
				Ok(_) => println!("updated: {name}"),
				Err(error) => println!("failed: {name} — {error}"),
			}
		}
		println!(
			"{} updated, {failed} failed ({} scope)",
			rows.len() - failed,
			scope_name(scope)
		);
		print_renamed_note(&plan.renamed);
	}

	if failed > 0 {
		crate::note_answer_on_stdout();
		bail!("{failed} skill update(s) failed (see the results above)");
	}
	Ok(())
}

fn print_renamed_note(renamed: &[(String, String)]) {
	for (name, new_name) in renamed {
		println!(
			"skipped: {name} was renamed upstream to '{new_name}' — run \
			 `aghub-cli source accept-rename {name} {new_name}`"
		);
	}
}

fn locked_resync_error(
	name: &str,
	error: skill_update::mutation::LockedResyncError,
) -> anyhow::Error {
	use aghub_core::skills::resync::ResyncError;
	use skill_update::mutation::LockedResyncError;

	match error {
		LockedResyncError::UnsupportedScope(_) => {
			anyhow!("apply-update requires --global or --project, not --all")
		}
		LockedResyncError::ProjectRootRequired => {
			anyhow!("project root is required")
		}
		LockedResyncError::LockEntryNotFound { scope } => match scope {
			ResourceScope::GlobalOnly => {
				anyhow!("skill '{name}' is not in global lock")
			}
			ResourceScope::ProjectOnly => {
				anyhow!("skill '{name}' is not in project lock")
			}
			ResourceScope::Both => anyhow!("skill '{name}' is not in lock"),
		},
		LockedResyncError::MissingSkillPath => {
			anyhow!("locked skill has no skillPath")
		}
		LockedResyncError::NotInstalled
		| LockedResyncError::Resync(ResyncError::NotInstalled) => {
			anyhow!("skill '{name}' is locked but no installed copy was found")
		}
		LockedResyncError::CredentialBackendUnavailable
		| LockedResyncError::Fetch(
			skill_update::FetchError::BackendUnavailable,
		) => anyhow!("Credential backend is unavailable; retry later."),
		LockedResyncError::InvalidSkillPath => {
			anyhow!("locked skillPath is not a valid skill folder")
		}
		LockedResyncError::SourceGroupMismatch => {
			anyhow!("skill source changed while updating; retry the command")
		}
		LockedResyncError::SourceSkillNotFound => {
			anyhow!("locked skillPath was not found in source")
		}
		LockedResyncError::Fetch(skill_update::FetchError::Auth) => {
			anyhow!("failed to fetch source repository: authentication failed")
		}
		LockedResyncError::Fetch(skill_update::FetchError::Network(detail)) => {
			// The detail can quote the locked source URL verbatim, and a lock
			// written from `https://user:token@host/repo` carries that userinfo.
			anyhow!(
				"failed to fetch source repository: {}",
				aghub_git::redact_url_userinfo(&detail)
			)
		}
		LockedResyncError::Resync(ResyncError::Renamed { new_name }) => {
			anyhow!(aghub_core::skills::update::skill_renamed_message(
				name, &new_name
			))
		}
		LockedResyncError::Resync(other) => anyhow!(other.to_string()),
	}
}

fn scope_name(scope: ResourceScope) -> &'static str {
	match scope {
		ResourceScope::GlobalOnly => "global",
		ResourceScope::ProjectOnly => "project",
		ResourceScope::Both => "all",
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use aghub_core::skills::lock::update_lock_hash;
	use std::sync::{Mutex, MutexGuard, OnceLock};
	use tempfile::{tempdir, TempDir};

	struct GlobalLockGuard {
		_temp: TempDir,
		old: Option<String>,
		_lock: MutexGuard<'static, ()>,
	}

	impl GlobalLockGuard {
		fn new() -> Self {
			static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
			let guard = LOCK
				.get_or_init(|| Mutex::new(()))
				.lock()
				.unwrap_or_else(|e| e.into_inner());
			let temp = tempdir().unwrap();
			let old = std::env::var("XDG_STATE_HOME").ok();
			std::env::set_var("XDG_STATE_HOME", temp.path());
			Self {
				_temp: temp,
				old,
				_lock: guard,
			}
		}
	}

	impl Drop for GlobalLockGuard {
		fn drop(&mut self) {
			match &self.old {
				Some(value) => std::env::set_var("XDG_STATE_HOME", value),
				None => std::env::remove_var("XDG_STATE_HOME"),
			}
		}
	}

	fn global_entry() -> skill::SkillLockEntry {
		skill::SkillLockEntry {
			source: "owner/repo".to_string(),
			source_type: "github".to_string(),
			source_url: "https://github.com/owner/repo".to_string(),
			ref_name: Some("main".to_string()),
			skill_path: Some("SKILL.md".to_string()),
			skill_folder_hash: "tree-v1".to_string(),
			content_hash: None,
			ref_commit: None,
			installed_at: "t".to_string(),
			updated_at: "t".to_string(),
			plugin_name: None,
		}
	}

	#[test]
	fn global_update_lock_hash_clears_npx_folder_hash() {
		let _guard = GlobalLockGuard::new();
		skill::lock::global::add_skill_to_lock("legacy", global_entry())
			.unwrap();

		update_lock_hash(
			"legacy",
			ResourceScope::GlobalOnly,
			None,
			"content-v2",
			None,
		)
		.unwrap();

		let lock = skill::lock::global::read_skill_lock();
		let entry = &lock.skills["legacy"];
		assert_eq!(entry.content_hash.as_deref(), Some("content-v2"));
		assert_eq!(entry.skill_folder_hash, "");
	}

	#[test]
	fn global_apply_update_writes_ref_commit() {
		let _guard = GlobalLockGuard::new();
		skill::lock::global::add_skill_to_lock("legacy", global_entry())
			.unwrap();

		update_lock_hash(
			"legacy",
			ResourceScope::GlobalOnly,
			None,
			"content-v2",
			Some("deadbeefcafef00d"),
		)
		.unwrap();

		let lock = skill::lock::global::read_skill_lock();
		let entry = &lock.skills["legacy"];
		assert_eq!(entry.content_hash.as_deref(), Some("content-v2"));
		assert_eq!(entry.ref_commit.as_deref(), Some("deadbeefcafef00d"));
	}

	#[test]
	fn project_apply_update_writes_ref_commit() {
		let project = tempdir().unwrap();
		skill::lock::local::add_skill_to_local_lock(
			"legacy",
			skill::lock::local::LocalSkillLockEntry {
				source_url: None,
				source: "owner/repo".to_string(),
				ref_name: Some("main".to_string()),
				source_type: "github".to_string(),
				computed_hash: "old".to_string(),
				skill_path: Some("SKILL.md".to_string()),
				ref_commit: None,
			},
			Some(project.path()),
		)
		.unwrap();

		update_lock_hash(
			"legacy",
			ResourceScope::ProjectOnly,
			Some(project.path()),
			"content-v2",
			Some("deadbeefcafef00d"),
		)
		.unwrap();

		let lock = skill::lock::local::read_local_lock(Some(project.path()));
		let entry = &lock.skills["legacy"];
		assert_eq!(entry.computed_hash, "content-v2");
		assert_eq!(entry.ref_commit.as_deref(), Some("deadbeefcafef00d"));
	}

	#[test]
	fn outdated_plan_targets_update_available_and_reports_renamed() {
		let view = |name: &str, status: StatusView| SkillUpdateView {
			name: name.to_string(),
			scope: "global".to_string(),
			checked: true,
			status,
		};
		let views = [
			view(
				"stale",
				StatusView::UpdateAvailable {
					current: "a".to_string(),
					available: "b".to_string(),
				},
			),
			view("fresh", StatusView::UpToDate),
			view(
				"moved",
				StatusView::Renamed {
					new_name: "moved-v2".to_string(),
				},
			),
			view(
				"offline",
				StatusView::Uncheckable {
					reason: "network".to_string(),
				},
			),
			view(
				"stale-too",
				StatusView::UpdateAvailable {
					current: "c".to_string(),
					available: "d".to_string(),
				},
			),
		];
		assert_eq!(
			outdated_plan(&views),
			OutdatedPlan {
				names: vec!["stale".to_string(), "stale-too".to_string()],
				renamed: vec![("moved".to_string(), "moved-v2".to_string())],
			}
		);
	}

	// Pin the PRODUCTION variant→message mapping used by `execute`. A
	// cfg(test) shadow of this mapping (`apply_skill_update_from_fetched`)
	// once carried these assertions, leaving the real seam uncovered —
	// swapping two arms below must fail this test.
	#[test]
	fn locked_resync_error_maps_variants_to_cli_messages() {
		use aghub_core::skills::resync::ResyncError;
		use skill_update::mutation::LockedResyncError;

		let cases = [
			(
				LockedResyncError::NotInstalled,
				"skill 'keep' is locked but no installed copy was found",
			),
			(
				LockedResyncError::Resync(ResyncError::NotInstalled),
				"skill 'keep' is locked but no installed copy was found",
			),
			(
				LockedResyncError::UnsupportedScope(ResourceScope::Both),
				"apply-update requires --global or --project, not --all",
			),
			(
				LockedResyncError::SourceSkillNotFound,
				"locked skillPath was not found in source",
			),
			(
				LockedResyncError::InvalidSkillPath,
				"locked skillPath is not a valid skill folder",
			),
			(
				LockedResyncError::CredentialBackendUnavailable,
				"Credential backend is unavailable; retry later.",
			),
			(
				LockedResyncError::LockEntryNotFound {
					scope: ResourceScope::ProjectOnly,
				},
				"skill 'keep' is not in project lock",
			),
			(
				LockedResyncError::LockEntryNotFound {
					scope: ResourceScope::GlobalOnly,
				},
				"skill 'keep' is not in global lock",
			),
		];
		for (error, expected) in cases {
			assert_eq!(
				locked_resync_error("keep", error).to_string(),
				expected
			);
		}

		// Renamed routes through the shared rename message (old + new name).
		let msg = locked_resync_error(
			"keep",
			LockedResyncError::Resync(ResyncError::Renamed {
				new_name: "keep-v2".to_string(),
			}),
		)
		.to_string();
		assert!(
			msg.contains("keep") && msg.contains("keep-v2"),
			"rename mapping must carry both names, got: {msg}"
		);
	}
}
