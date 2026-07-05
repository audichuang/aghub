use crate::ResourceType;
use aghub_core::models::{ResourceScope, Skill};
use anyhow::{anyhow, bail, Context, Result};
use serde_json::json;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

struct ApplySource {
	source: String,
	ref_name: Option<String>,
	skill_path: String,
}

struct FetchedSource {
	path: PathBuf,
	/// Resolved tip commit OID (40-hex) of the fetched ref, recorded into the
	/// lock's `refCommit` so the next `check` can preflight via ls-refs.
	oid: String,
	_guard: TempDir,
}

pub fn execute(
	resource: ResourceType,
	name: String,
	scope: ResourceScope,
	project_root: Option<&Path>,
	yes: bool,
	dry_run: bool,
	_json: bool,
) -> Result<()> {
	match resource {
		ResourceType::Skills => {}
		ResourceType::Mcps => bail!("`apply-update` only supports skills"),
	}
	// A dry-run mutates nothing, so it does not need --yes.
	if !yes && !dry_run {
		bail!("refusing to overwrite skill files without --yes");
	}
	let source = apply_source_from_lock(&name, scope, project_root)?;
	// Bail early (before paying for a fetch) when nothing is installed.
	if installed_skill_roots(&name, scope, project_root).is_empty() {
		bail!("skill '{name}' is locked but no installed copy was found");
	}

	let fetched = fetch_source(&source)?;
	let paths = apply_skill_update_from_fetched(
		&fetched.path,
		&source.skill_path,
		&name,
		scope,
		project_root,
		Some(&fetched.oid),
		dry_run,
	)?;
	// On a dry-run nothing was swapped, so hashing a target would read stale
	// content; the swap was skipped, so report no recomputed hash.
	let updated_hash = if dry_run {
		String::new()
	} else {
		updated_hash_for_paths(&paths)
	};
	println!(
		"{}",
		serde_json::to_string_pretty(&json!({
			"success": true,
			"dryRun": dry_run,
			"name": name,
			"scope": scope_name(scope),
			"oid": fetched.oid,
			"updatedHash": updated_hash,
			"paths": paths
				.iter()
				.map(|p| p.display().to_string())
				.collect::<Vec<_>>(),
			"error": null,
		}))?
	);
	Ok(())
}

/// Apply a skill update from an **already-fetched** repo working tree, then
/// update the scope's lock (content/computed hash + `refCommit`) exactly as
/// [`execute`] does. Fetch-free so callers that already materialized the source
/// (e.g. `source sync --update`) can reuse it without a second network round.
///
/// Mirrors the post-fetch body of [`execute`]: `sanitize_skill_path` →
/// `ensure_source_not_renamed` → containment assert → `stage_and_swap_dir`
/// loop → lock update. Returns the swapped install paths.
///
/// When `dry_run` is set, runs the full set of read-only guards
/// (`sanitize_skill_path`, `ensure_source_not_renamed`, containment assert) and
/// returns the targets it WOULD swap, but RETURNS before any
/// `stage_and_swap_dir`/`update_lock_hash` — so installed files and the lock are
/// left byte-for-byte untouched. The rename refusal still fires in dry-run.
#[allow(clippy::too_many_arguments)]
pub fn apply_skill_update_from_fetched(
	repo_root: &Path,
	skill_path: &str,
	name: &str,
	scope: ResourceScope,
	project_root: Option<&Path>,
	ref_commit: Option<&str>,
	dry_run: bool,
) -> Result<Vec<PathBuf>> {
	let targets = installed_skill_roots(name, scope, project_root);
	if targets.is_empty() {
		bail!("skill '{name}' is locked but no installed copy was found");
	}

	let skill_file =
		aghub_core::skills::update::sanitize_skill_path(repo_root, skill_path)
			.ok_or_else(|| {
				anyhow!("locked skillPath was not found in source")
			})?;
	// Refuse to silently overwrite the installed skill when the upstream source
	// renamed it (same skillPath, changed frontmatter `name`). This mirrors the
	// hardened API apply path; the shared predicate lives in `aghub-core` so both
	// surfaces enforce the same contract.
	ensure_source_not_renamed(&skill_file, name)?;
	let source_dir = skill_file.parent().unwrap_or(repo_root);
	let updated_hash = skill::compute_skill_folder_hash(source_dir)
		.context("failed to hash fetched skill")?;

	let agent_dirs = aghub_core::skills::removal::agent_skill_dirs_in_scope(
		scope,
		project_root,
	);
	aghub_core::skills::removal::assert_targets_contained(
		&targets,
		&agent_dirs,
		project_root,
	)
	.context("refusing to update a skill outside allowed skill roots")?;

	// Dry-run: every read-only guard above has passed; report the targets
	// without mutating any installed file or the lock.
	if dry_run {
		return Ok(targets);
	}

	let mut paths = Vec::new();
	for target in &targets {
		let outcome =
			aghub_core::skills::update::stage_and_swap_dir(source_dir, target)
				.with_context(|| {
					format!(
						"failed to replace installed skill at {}",
						target.display()
					)
				})?;
		// A post-swap temp-cleanup failure is a warning, never an abort: the
		// swap succeeded, so the remaining targets and the lock update must
		// still run (aborting would leave new content with a stale lock).
		if let Some(warning) = outcome.cleanup_warning {
			eprintln!("warning: {warning}");
		}
		paths.push(target.clone());
	}

	update_lock_hash(name, scope, project_root, &updated_hash, ref_commit)?;
	Ok(paths)
}

/// Recompute the folder hash for the JSON `updatedHash` field from a swapped
/// install path. `execute` historically emitted the hash computed off the
/// fetched source dir; after the swap that dir's content is now on disk at each
/// target, so hashing the first target yields the identical value.
fn updated_hash_for_paths(paths: &[PathBuf]) -> String {
	paths
		.first()
		.and_then(|p| skill::compute_skill_folder_hash(p).ok())
		.unwrap_or_default()
}

/// Parse the fetched `SKILL.md` and refuse the apply if the upstream frontmatter
/// `name` no longer matches the locked name (an upstream rename). Reuses the
/// shared `aghub-core` rename contract so the CLI, API apply, and API sync paths
/// all behave identically.
fn ensure_source_not_renamed(
	skill_file: &Path,
	locked_name: &str,
) -> Result<()> {
	let parsed =
		skill::parse(skill_file).context("failed to parse fetched skill")?;
	if let Some(new_name) =
		aghub_core::skills::update::detect_rename(&parsed.name, locked_name)
	{
		bail!(aghub_core::skills::update::skill_renamed_message(
			locked_name,
			&new_name
		));
	}
	Ok(())
}

fn apply_source_from_lock(
	name: &str,
	scope: ResourceScope,
	project_root: Option<&Path>,
) -> Result<ApplySource> {
	match scope {
		ResourceScope::GlobalOnly => {
			let entry = skill::lock::global::get_skill_from_lock(name)
				.ok_or_else(|| {
					anyhow!("skill '{name}' is not in global lock")
				})?;
			let skill_path = entry
				.skill_path
				.ok_or_else(|| anyhow!("locked skill has no skillPath"))?;
			Ok(ApplySource {
				source: entry.source_url,
				ref_name: entry.ref_name,
				skill_path,
			})
		}
		ResourceScope::ProjectOnly => {
			let root = project_root
				.ok_or_else(|| anyhow!("project root is required"))?;
			let lock = skill::lock::local::read_local_lock(Some(root));
			let entry = lock.skills.get(name).cloned().ok_or_else(|| {
				anyhow!("skill '{name}' is not in project lock")
			})?;
			let skill_path = entry
				.skill_path
				.ok_or_else(|| anyhow!("locked skill has no skillPath"))?;
			Ok(ApplySource {
				source: entry.source,
				ref_name: entry.ref_name,
				skill_path,
			})
		}
		ResourceScope::Both => {
			bail!("apply-update requires --global or --project, not --all")
		}
	}
}

fn fetch_source(source: &ApplySource) -> Result<FetchedSource> {
	let resolved = aghub_git::resolve_remote_source(&source.source)
		.context("failed to resolve remote source")?;
	let creds = aghub_git::read_credentials();
	let (bare, oid) = aghub_git::fetch_ref_to_temp(
		&resolved.clone_url,
		source.ref_name.as_deref(),
		creds.as_ref(),
		Some(std::time::Duration::from_secs(30)),
	)
	.context("failed to fetch source repository")?;
	let repo = gix::open(bare.path()).context("failed to open fetched repo")?;
	let object = repo
		.find_object(oid)
		.context("failed to read fetched HEAD")?;
	let tree = object
		.peel_to_tree()
		.context("failed to peel fetched tree")?;
	let materialized = tempfile::TempDir::new()?;
	aghub_git::materialize_tree(&repo, tree.id, materialized.path())?;
	Ok(FetchedSource {
		path: materialized.path().to_path_buf(),
		oid: oid.to_string(),
		_guard: materialized,
	})
}

fn skill_root(skill: &Skill) -> Option<PathBuf> {
	let raw = skill
		.canonical_path
		.as_deref()
		.or(skill.source_path.as_deref())?;
	let path = if let Some(stripped) = raw.strip_prefix("~/") {
		dirs::home_dir().map(|home| home.join(stripped))?
	} else {
		PathBuf::from(raw)
	};
	let is_skill_file = path
		.file_name()
		.is_some_and(|name| name == std::ffi::OsStr::new("SKILL.md"));
	Some(if is_skill_file {
		path.parent().map(Path::to_path_buf).unwrap_or(path)
	} else {
		path
	})
}

fn installed_skill_roots(
	name: &str,
	scope: ResourceScope,
	project_root: Option<&Path>,
) -> Vec<PathBuf> {
	let mut roots = Vec::new();
	for agent in aghub_core::load_all_agents(scope, project_root) {
		for skill in agent.skills {
			if skill.name != name {
				continue;
			}
			let Some(root) = skill_root(&skill) else {
				continue;
			};
			if !roots.contains(&root) {
				roots.push(root);
			}
		}
	}
	roots
}

fn update_lock_hash(
	name: &str,
	scope: ResourceScope,
	project_root: Option<&Path>,
	hash: &str,
	ref_commit: Option<&str>,
) -> Result<()> {
	match scope {
		ResourceScope::GlobalOnly => {
			skill::lock::global::modify_skill_lock(|lock| {
				let Some(entry) = lock.skills.get_mut(name) else {
					return Err(anyhow!(
						"skill '{name}' is not in global lock"
					));
				};
				entry
					.apply_content_hash(hash, &chrono::Utc::now().to_rfc3339());
				if let Some(oid) = ref_commit {
					entry.ref_commit = Some(oid.to_string());
				}
				Ok(())
			})??;
		}
		ResourceScope::ProjectOnly => {
			let root = project_root
				.ok_or_else(|| anyhow!("project root is required"))?;
			skill::lock::local::modify_local_lock(Some(root), |lock| {
				let Some(entry) = lock.skills.get_mut(name) else {
					return Err(anyhow!(
						"skill '{name}' is not in project lock"
					));
				};
				entry.apply_computed_hash(hash);
				if let Some(oid) = ref_commit {
					entry.ref_commit = Some(oid.to_string());
				}
				Ok(())
			})??;
		}
		ResourceScope::Both => {
			bail!("apply-update requires --global or --project, not --all")
		}
	}
	Ok(())
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
	use std::sync::{Mutex, MutexGuard, OnceLock};
	use tempfile::{tempdir, TempDir};

	#[test]
	fn ensure_source_not_renamed_bails_when_upstream_name_differs() {
		let dir = tempdir().unwrap();
		let skill_file = dir.path().join("SKILL.md");
		std::fs::write(
			&skill_file,
			"---\nname: new-skill\ndescription: d\n---\nbody\n",
		)
		.unwrap();

		let err =
			ensure_source_not_renamed(&skill_file, "old-skill").unwrap_err();
		let msg = err.to_string();
		assert!(msg.contains("renamed"), "msg: {msg}");
		assert!(msg.contains("new-skill"), "msg: {msg}");
	}

	#[test]
	fn ensure_source_not_renamed_ok_when_names_match() {
		let dir = tempdir().unwrap();
		let skill_file = dir.path().join("SKILL.md");
		std::fs::write(
			&skill_file,
			"---\nname: same-skill\ndescription: d\n---\nbody\n",
		)
		.unwrap();

		assert!(ensure_source_not_renamed(&skill_file, "same-skill").is_ok());
	}

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

	/// `GlobalLockGuard` only isolates `XDG_STATE_HOME` (the lock dir). The
	/// dry-run tests also need an isolated `HOME` so the installed Claude skill
	/// under `~/.claude/skills/<name>` is discovered from a tempdir, not the
	/// developer's real home. Restores `HOME` on drop.
	struct HomeGuard {
		_temp: TempDir,
		old: Option<String>,
	}

	impl HomeGuard {
		fn new() -> (Self, PathBuf) {
			let temp = tempdir().unwrap();
			let home = temp.path().to_path_buf();
			let old = std::env::var("HOME").ok();
			std::env::set_var("HOME", &home);
			(Self { _temp: temp, old }, home)
		}
	}

	impl Drop for HomeGuard {
		fn drop(&mut self) {
			match &self.old {
				Some(value) => std::env::set_var("HOME", value),
				None => std::env::remove_var("HOME"),
			}
		}
	}

	/// Install a Claude-scoped skill at `~/.claude/skills/<name>/SKILL.md` with
	/// the given body and return its folder path (the swap target).
	fn install_claude_skill(home: &Path, name: &str, body: &str) -> PathBuf {
		let dir = home.join(".claude/skills").join(name);
		std::fs::create_dir_all(&dir).unwrap();
		std::fs::write(dir.join("SKILL.md"), body).unwrap();
		dir
	}

	/// A fetched source working tree containing a single `SKILL.md` declaring
	/// `name` with the given body. Returned `TempDir` keeps it alive.
	fn fetched_source(name: &str, body: &str) -> (TempDir, PathBuf) {
		let dir = tempdir().unwrap();
		std::fs::write(
			dir.path().join("SKILL.md"),
			format!("---\nname: {name}\ndescription: {body}\n---\n{body}\n"),
		)
		.unwrap();
		let root = dir.path().to_path_buf();
		(dir, root)
	}

	#[cfg(unix)]
	#[test]
	fn dry_run_reports_targets_without_mutating() {
		let _guard = GlobalLockGuard::new();
		let (_home_guard, home) = HomeGuard::new();

		let installed_body =
			"---\nname: mytool\ndescription: original\n---\nold body\n";
		let installed = install_claude_skill(&home, "mytool", installed_body);
		skill::lock::global::add_skill_to_lock("mytool", global_entry())
			.unwrap();

		// Upstream has the SAME name but new content; a real apply would swap.
		let (_src_guard, src_root) =
			fetched_source("mytool", "renamed-content-upstream");

		let paths = apply_skill_update_from_fetched(
			&src_root,
			"SKILL.md",
			"mytool",
			ResourceScope::GlobalOnly,
			None,
			Some("deadbeefcafef00d"),
			true,
		)
		.unwrap();

		// Reports the target it WOULD swap.
		assert_eq!(paths, vec![installed.clone()]);

		// Installed SKILL.md is byte-for-byte unchanged.
		let still =
			std::fs::read_to_string(installed.join("SKILL.md")).unwrap();
		assert_eq!(still, installed_body);

		// Lock hash + ref_commit untouched.
		let lock = skill::lock::global::read_skill_lock();
		let entry = &lock.skills["mytool"];
		assert!(entry.content_hash.is_none(), "dry-run must not write hash");
		assert!(entry.ref_commit.is_none(), "dry-run must not write oid");
		assert_eq!(entry.skill_folder_hash, "tree-v1");
	}

	#[cfg(unix)]
	#[test]
	fn dry_run_on_renamed_upstream_still_refuses() {
		let _guard = GlobalLockGuard::new();
		let (_home_guard, home) = HomeGuard::new();

		let installed_body =
			"---\nname: mytool\ndescription: original\n---\nold body\n";
		let installed = install_claude_skill(&home, "mytool", installed_body);
		skill::lock::global::add_skill_to_lock("mytool", global_entry())
			.unwrap();

		// Upstream renamed the skill (same skillPath, different frontmatter name).
		let (_src_guard, src_root) =
			fetched_source("renamed-tool", "renamed upstream");

		let err = apply_skill_update_from_fetched(
			&src_root,
			"SKILL.md",
			"mytool",
			ResourceScope::GlobalOnly,
			None,
			None,
			true,
		)
		.unwrap_err();
		let msg = err.to_string();
		assert!(msg.contains("renamed"), "msg: {msg}");
		assert!(msg.contains("renamed-tool"), "msg: {msg}");

		// Still no mutation on the refusal path.
		let still =
			std::fs::read_to_string(installed.join("SKILL.md")).unwrap();
		assert_eq!(still, installed_body);
	}

	#[cfg(unix)]
	#[test]
	fn apply_skill_update_mutates_when_not_dry_run() {
		// Pins that dry_run=false still performs the swap + lock write, so the
		// gate is the ONLY behavior change (the non-executed branch leaves state
		// untouched; this is the executed branch).
		let _guard = GlobalLockGuard::new();
		let (_home_guard, home) = HomeGuard::new();

		let installed = install_claude_skill(
			&home,
			"mytool",
			"---\nname: mytool\ndescription: original\n---\nold body\n",
		);
		skill::lock::global::add_skill_to_lock("mytool", global_entry())
			.unwrap();

		let (_src_guard, src_root) = fetched_source("mytool", "new upstream");

		apply_skill_update_from_fetched(
			&src_root,
			"SKILL.md",
			"mytool",
			ResourceScope::GlobalOnly,
			None,
			Some("deadbeefcafef00d"),
			false,
		)
		.unwrap();

		let after =
			std::fs::read_to_string(installed.join("SKILL.md")).unwrap();
		assert!(after.contains("new upstream"), "swap must apply: {after}");

		let lock = skill::lock::global::read_skill_lock();
		let entry = &lock.skills["mytool"];
		assert!(entry.content_hash.is_some(), "apply must write hash");
		assert_eq!(entry.ref_commit.as_deref(), Some("deadbeefcafef00d"));
	}
}
