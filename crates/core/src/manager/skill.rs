use super::ConfigManager;
use crate::skills::linker::Linker;
use crate::{
	convert_skill,
	errors::{ConfigError, Result},
	models::Skill,
};
use log::{debug, info, warn};
use skill::sanitize::sanitize_name;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Outcome of an install-from-path.
///
/// `skill` is ALWAYS the skill as it exists on disk after the call — for a
/// re-add that is the untouched Master, NOT the parsed source, because a re-add
/// writes nothing and reporting the source would claim an edit landed.
#[derive(Debug, Clone)]
pub struct SkillAdd {
	/// The skill as it exists on disk after the call.
	pub skill: Skill,
	/// True when the skill was already installed and nothing was written.
	pub already_installed: bool,
	/// This call atomically claimed and wrote the Master. Attribution for a
	/// caller that must roll its own work back — a Master merely found and
	/// verified belongs to whoever wrote it.
	pub wrote_master: bool,
	/// Agent skills-dirs where this call created a FRESH referrer, straight
	/// from the linker — never reconstructed from `installed` (see
	/// crates/core/AGENTS.md "Mutation attribution").
	pub created_referrer_dirs: Vec<std::path::PathBuf>,
}

impl SkillAdd {
	/// A real install, carrying the materializer's OWN receipt so a caller that
	/// writes something AFTER this call (the API import route stamps the lock)
	/// can undo exactly this call's work when that later step fails. The
	/// receipt is not optional: a caller that cannot roll back is the bug this
	/// exists to prevent.
	fn installed(
		skill: Skill,
		materialized: &crate::skills::install_fetched::MaterializedMaster,
	) -> Self {
		Self {
			skill,
			already_installed: false,
			wrote_master: materialized.created_master,
			created_referrer_dirs: materialized.created_referrer_dirs.clone(),
		}
	}

	fn already_installed(skill: Skill) -> Self {
		Self {
			skill,
			already_installed: true,
			wrote_master: false,
			created_referrer_dirs: Vec::new(),
		}
	}
}

/// The skill as it exists at the Master on disk, with its paths pointed there.
///
/// `materialize_universal_master` PRESERVES a pre-existing Master, so what is
/// on disk may not be the caller's input; [`SkillAdd`] must report the disk.
///
/// `None` when the Master cannot be parsed. Both callers treat that as an
/// ERROR, never a fallback to their own input: nothing can read that Master.
fn master_on_disk(canonical: &Path, name: &str) -> Option<Skill> {
	let pkg = skill::parser::parse(canonical).ok()?;
	let mut found = convert_skill(pkg);
	// Keyed by the REQUESTED name: that is what the caller's `config.skills`
	// entry and later `get_skill(name)` use. A frontmatter/directory mismatch
	// is `doctor`'s problem.
	found.name = name.to_string();
	let md = canonical.join("SKILL.md").to_string_lossy().to_string();
	found.source_path = Some(md.clone());
	found.canonical_path = Some(md);
	Some(found)
}

/// Shared preparation for the two universal-install entry points
/// (`add_skill_universal` / `add_skill_from_path_universal`): resolves the
/// `.agents` canonical directory plus the current agent's symlink target.
struct UniversalPrep {
	agent_name: String,
	agent_write_dir: Option<PathBuf>,
	canonical_dir: PathBuf,
	use_relative: bool,
	/// How THIS agent relates to the Master at this scope, via the shared
	/// classifier (parity with the fetched/desktop install path).
	link_need: crate::skills::linker::LinkNeed,
}

/// Resolve a source_path string (potentially with `~/` prefix) to an absolute PathBuf
fn resolve_source_path(sp: &str) -> PathBuf {
	if let Some(stripped) = sp.strip_prefix("~/") {
		if let Some(home) = dirs::home_dir() {
			home.join(stripped)
		} else {
			PathBuf::from(sp)
		}
	} else {
		PathBuf::from(sp)
	}
}

/// Remove a skill's file or directory from disk.
///
/// `path` is the SKILL.md location resolved from `source_path`:
/// - Copy layout: `path` is `<target_dir>/<safe_name>/SKILL.md` (a real file).
/// - Universal layout: `path` is the canonical's SKILL.md (e.g.
///   `<project>/.aghub/<safe_name>/SKILL.md`); the per-agent symlink
///   that needs to be unlinked lives at `<target_dir>/<safe_name>`.
///
/// Universal skills leave the Master intact (other agents or `npx skills` may
/// reference it); removing it goes via [`ConfigManager::remove_skill_planned`].
// Kept only for its containment tests until the by-path entry (A6) takes over.
#[cfg_attr(not(test), allow(dead_code))]
fn remove_skill_path(
	path: &Path,
	safe_name: &str,
	is_link: bool,
	target_dir: Option<&Path>,
	roots: &[PathBuf],
) -> Result<()> {
	if is_link {
		// Universal layout: the symlink at `<target_dir>/<safe_name>` is what
		// should disappear. `path.parent()` is the canonical dir (a real
		// directory), not a link, so unlink via the target_dir-resolved path.
		if let Some(target) = target_dir {
			let link = target.join(safe_name);
			let needs_unlink = Linker::is_link(&link);
			if needs_unlink {
				Linker::unlink(&link).map_err(|e| {
					ConfigError::Io(std::io::Error::new(
						e.kind(),
						format!(
							"Failed to remove link '{}': {}",
							link.display(),
							e
						),
					))
				})?;
			}
		}
		// Idempotent: if the link is already gone (or was never created),
		// symlink_metadata returns NotFound and we leave the canonical alone.
		return Ok(());
	}

	let Some(parent) = path.parent() else {
		return std::fs::remove_file(path).map_err(|e| e.into());
	};

	let is_named_dir =
		parent.file_name().and_then(|n| n.to_str()) == Some(safe_name);
	if is_named_dir {
		// Containment guard: never `remove_dir_all` a directory that escapes the
		// allow-listed skill roots (canonicalize-escape protection), mirroring
		// the planned-removal path.
		if crate::skills::removal::assert_contained(parent, roots).is_none() {
			return Err(ConfigError::Io(std::io::Error::new(
				std::io::ErrorKind::PermissionDenied,
				format!(
					"Refusing to remove '{}': outside allow-listed skill roots",
					parent.display()
				),
			)));
		}
		std::fs::remove_dir_all(parent).map_err(|e| {
			ConfigError::Io(std::io::Error::new(
				e.kind(),
				format!(
					"Failed to remove directory '{}': {}",
					parent.display(),
					e
				),
			))
		})?;
	} else {
		std::fs::remove_file(path).map_err(|e| {
			ConfigError::Io(std::io::Error::new(
				e.kind(),
				format!("Failed to remove file '{}': {}", path.display(), e),
			))
		})?;
	}
	Ok(())
}

impl ConfigManager {
	/// Take the interprocess mutation lock for `scope` AND re-read this manager's
	/// config under it — the two are one step on purpose.
	///
	/// `self.config` came from the CALLER's pre-mutation `load()`, which another
	/// aghub process may already have invalidated; every decision below the
	/// guard must use a view read under it.
	///
	/// Every guarded `ConfigManager` mutation goes through here rather than
	/// calling `mutation_guard` directly, so none can forget the re-read. A
	/// dry-run takes neither. One documented exception: `update_skill`.
	/// See crates/core/AGENTS.md "Mutation attribution".
	fn guard_and_reload(
		&mut self,
		op: &str,
		scope: crate::models::ResourceScope,
	) -> Result<skill::lock::MutationGuard> {
		let guard = crate::skills::lock::mutation_guard(
			op,
			scope,
			self.project_root.as_deref(),
		)
		.map_err(ConfigError::Io)?;
		self.load()?;
		Ok(guard)
	}

	/// Returns the [`SkillAdd`] receipt, so a caller can tell a real install
	/// from an idempotent no-op and report the skill AS IT EXISTS ON DISK
	/// rather than echoing back what was requested.
	pub fn add_skill(&mut self, skill: Skill) -> Result<SkillAdd> {
		// Symlink-only model: one Master, THIS agent linked to it, like every
		// other install path. There is no copy install path.
		self.add_skill_universal(skill)
	}

	/// Add a skill in *universal* layout: write the real `SKILL.md` once into
	/// the Master store and symlink THIS agent's skills dir to it (npx-style).
	/// Sets `canonical_path` so layout-aware removal recognises the symlink.
	/// `--universal` is a deprecated no-op.
	///
	/// An existing Master is **left intact** (mirrors the API path's
	/// `wrote_master = !canonical.exists()` rule); the per-agent symlink is
	/// still created idempotently.
	///
	/// The idempotent branch returns `SkillAdd::already_installed(<the skill
	/// ALREADY in config>)`, not the requested one, so a re-add never reports
	/// requested metadata that did not land.
	/// See docs/history/core-manager.md#re-add-reported-requested-metadata
	pub fn add_skill_universal(&mut self, skill: Skill) -> Result<SkillAdd> {
		let UniversalPrep {
			agent_name,
			agent_write_dir,
			canonical_dir,
			use_relative,
			link_need,
		} = self.universal_install_prep()?;
		// Capture materializer inputs BEFORE the mutable `config` borrow so the
		// shared materializer can run during the install.
		let scope = self.write_scope;
		let project_root = self.project_root.clone();
		let agent_type = self.agent_type();

		// Spans the duplicate-name check, the Master write and the link, so a
		// concurrent aghub cannot land its own Master between `exists()` and write.
		let _mutation_guard = self.guard_and_reload("add skill", scope)?;

		let config = self.config_mut()?;
		if let Some(existing) =
			config.skills.iter().find(|s| s.name == skill.name).cloned()
		{
			// Classify the already-installed state before deciding to error.
			let safe = sanitize_name(&skill.name);
			let canonical = canonical_dir.join(&safe);
			// Nothing reads the `.aghub` store, so idempotence is decided by the
			// link alone: a Referrer already resolving to this Master is the
			// no-op; anything else at that slot is a conflict.
			if let Some(ref agent_dir) = agent_write_dir {
				let slot = agent_dir.join(&safe);
				if Linker::is_link(&slot) {
					let master_real = std::fs::canonicalize(&canonical)
						.unwrap_or_else(|_| canonical.clone());
					if std::fs::canonicalize(&slot)
						.map(|r| r == master_real)
						.unwrap_or(false)
					{
						return Ok(SkillAdd::already_installed(existing));
					}
				}
			}
			// Foreign occupant or not yet linked: strict error.
			return Err(ConfigError::resource_exists("skill", &skill.name));
		}
		info!(
			"adding skill '{}' (universal layout) for agent '{}'",
			skill.name, agent_name
		);

		let safe_name = sanitize_name(&skill.name);
		let canonical = canonical_dir.join(&safe_name);
		crate::skills::linker::ensure_master_store_parent(&canonical)?;
		// This path has a `Skill` struct, not a source tree, so the from-struct
		// SKILL.md is serialized here (intrinsic to this entry point). A
		// pre-existing master is reused without overwriting.
		let canonical_existed = canonical.exists();
		if canonical_existed {
			warn!(
				"canonical '{}' already exists; reusing without overwriting \
				 SKILL.md (use `aghub update` to refresh content)",
				canonical.display()
			);
		} else {
			std::fs::create_dir_all(&canonical)?;
			std::fs::write(
				canonical.join("SKILL.md"),
				format_skill(&skill, None, &BTreeMap::new()),
			)?;
		}

		// Classify + link via the ONE shared materializer (same code as the
		// fetched/desktop path); the Master exists, so its copy branch is skipped.
		let target_link = if use_relative {
			crate::skills::linker::LinkTarget::Relative
		} else {
			crate::skills::linker::LinkTarget::Absolute
		};
		let results =
			crate::skills::install_fetched::materialize_universal_master(
				&canonical,
				&safe_name,
				scope,
				project_root.as_deref(),
				std::slice::from_ref(&agent_type),
				target_link,
			)?;
		Self::ensure_single_agent_installed(
			&results.agent_results,
			&link_need,
			&skill.name,
		)?;

		let canonical_md =
			canonical.join("SKILL.md").to_string_lossy().to_string();
		let mut fs_skill = skill.clone();
		fs_skill.source_path = Some(canonical_md.clone());
		fs_skill.canonical_path = Some(canonical_md);
		// What is ON DISK, which is not `fs_skill` when the Master was preserved.
		let on_disk = if canonical_existed {
			match master_on_disk(&canonical, &skill.name) {
				Some(found) => found,
				// Same rule as the from-path sibling: an unreadable master is an
				// error, not a reason to report the caller's own input back.
				None => {
					crate::skills::rename::rollback_materialized_install(
						&skill.name,
						scope,
						project_root.as_deref(),
						&results.created_referrer_dirs,
						false,
					);
					return Err(ConfigError::InvalidConfig(format!(
						"the existing master at '{}' does not parse, so nothing \
						 can read it and aghub cannot report what is installed. \
						 Fix or remove that master, then re-run.",
						canonical.display()
					)));
				}
			}
		} else {
			fs_skill.clone()
		};
		config.skills.push(on_disk.clone());

		// Deliberately NOT `save_current()`: `save()` serializes MCPs only, and
		// rewriting them here drops per-server fields aghub does not model.
		// See docs/history/core-manager.md#skill-mutations-rewrote-mcp-config
		Ok(SkillAdd::installed(on_disk, &results))
	}

	pub fn get_skill(&self, name: &str) -> Option<&Skill> {
		self.config.as_ref()?.skills.iter().find(|s| s.name == name)
	}

	pub fn update_skill(&mut self, name: &str, skill: Skill) -> Result<()> {
		// A rename here is `rename_skill_master` + a relink of every Referrer —
		// itself transactional, so it must not interleave with another process's
		// install of either name. Scope (not write_scope) because the relink
		// sweeps every in-scope agent dir.
		//
		// The ONLY guarded mutation that does not `guard_and_reload` — a known
		// gap, not a decision: the re-read regressed two rename tests on macOS
		// ONLY (likely `/private/var` vs `/var`), not reproducible on Linux, so
		// the stale-view window stays OPEN here until that is understood.
		// See docs/history/core-manager.md#update-skill-withholds-the-re-read
		let _mutation_guard = crate::skills::lock::mutation_guard(
			"update skill",
			self.scope,
			self.project_root.as_deref(),
		)
		.map_err(ConfigError::Io)?;

		let target_dir = self.target_skills_dir();
		let agent_name = self.adapter.name().to_string();
		// Captured before the mutable borrow below so the universal-rename relink
		// (which needs the in-scope agent dirs + link style) can run without
		// re-borrowing `self`.
		let scope = self.scope;
		let write_scope = self.write_scope;
		let project_root = self.project_root.clone();
		let config = self.config.as_ref().ok_or_else(|| {
			ConfigError::InvalidConfig("No configuration loaded".to_string())
		})?;
		let index = config
			.skills
			.iter()
			.position(|s| s.name == name)
			.ok_or_else(|| ConfigError::resource_not_found("skill", name))?;
		let existing_skill = config.skills[index].clone();
		if existing_skill.canonical_path.is_some() {
			let store_root = if matches!(
				write_scope,
				crate::models::ResourceScope::ProjectOnly
			) {
				project_root.as_deref()
			} else {
				None
			};
			crate::skills::linker::reject_linked_master_store(store_root)?;
		}

		let config = self.config_mut()?;
		info!(
			"updating skill '{}' -> '{}' for agent '{}'",
			name, skill.name, agent_name
		);
		let safe_old_name = sanitize_name(name);
		// Prefer canonical path (real location) for writes
		let file_path = if let Some(cp) = &existing_skill.canonical_path {
			Some(resolve_source_path(cp))
		} else if let Some(sp) = &existing_skill.source_path {
			Some(resolve_source_path(sp))
		} else {
			target_dir.map(|dir| dir.join(&safe_old_name).join("SKILL.md"))
		};

		if let Some(path) = file_path {
			// Read existing body before any filesystem changes
			let existing_body = match skill::parser::parse(&path) {
				Ok(existing) => Some(existing.content),
				Err(skill::SkillError::NotFound(_)) => None,
				Err(e) => {
					return Err(ConfigError::InvalidConfig(format!(
						"Failed to parse existing skill '{}': {e}",
						path.display()
					)));
				}
			};
			// Likewise read BEFORE the rename moves the file: the author's own
			// frontmatter keys, which the reduced model below cannot carry.
			let preserved = preserved_frontmatter(&path);

			let mut final_file_path = path.clone();
			// A universal skill is rename-relinked (per-agent symlinks re-pointed)
			// and keeps its symlink layout; a copy skill is just renamed in place.
			let is_universal = existing_skill.canonical_path.is_some();
			let mut relinked_universal = false;

			// Handle rename
			if name != skill.name {
				let safe_new_name = sanitize_name(&skill.name);
				if let Some(parent) = path.parent() {
					if parent.file_name().and_then(|n| n.to_str())
						== Some(&safe_old_name)
					{
						// project scope → relative links, global → absolute
						// (mirrors `universal_install_prep`).
						let use_relative = matches!(
							write_scope,
							crate::models::ResourceScope::ProjectOnly
						) && project_root.is_some();
						// Rename the master + relink referrers as one transaction:
						// a failed relink rolls back so referrers never dangle.
						final_file_path = rename_skill_master(
							parent,
							path.file_name().unwrap(),
							&safe_old_name,
							&safe_new_name,
							is_universal,
							scope,
							project_root.as_deref(),
							use_relative,
						)?;
						relinked_universal = is_universal;
					} else if path.file_name().and_then(|n| n.to_str())
						== Some(&format!("{safe_old_name}.md"))
					{
						let new_path =
							path.with_file_name(format!("{safe_new_name}.md"));
						std::fs::rename(&path, &new_path).map_err(|e| {
							ConfigError::Io(std::io::Error::new(
								e.kind(),
								format!(
									"Failed to rename skill \
										 file '{}' -> '{}': {}",
									path.display(),
									new_path.display(),
									e
								),
							))
						})?;
						final_file_path = new_path;
					}
				}
			}

			if let Some(parent) = final_file_path.parent() {
				if !parent.exists() {
					std::fs::create_dir_all(parent)?;
				}
			}

			let content =
				format_skill(&skill, existing_body.as_deref(), &preserved);
			std::fs::write(&final_file_path, content)?;

			let mut fs_skill = skill.clone();
			if final_file_path == path {
				fs_skill.source_path = existing_skill.source_path.clone();
				fs_skill.canonical_path = existing_skill.canonical_path.clone();
			} else if relinked_universal {
				// Universal rename: source + canonical both point at the renamed
				// master, preserving the symlink layout for later removal.
				let md = final_file_path.to_string_lossy().to_string();
				fs_skill.source_path = Some(md.clone());
				fs_skill.canonical_path = Some(md);
			} else {
				fs_skill.source_path =
					Some(final_file_path.to_string_lossy().to_string());
				fs_skill.canonical_path = None;
			}
			config.skills[index] = fs_skill;
		} else {
			return Err(ConfigError::InvalidConfig(
				"Agent does not support persistent skill updates \
				 or source missing"
					.into(),
			));
		}

		// Not `save_current()` — it only serializes MCPs, so it cannot persist
		// a skill and rewrites `.mcp.json` as a side effect. See `add_skill`.
		Ok(())
	}
}

/// Patch representation for updating an existing [`Skill`].
///
/// Omitted fields (`None`) retain their existing values, while provided
/// fields overwrite them. For `tools`, passing an empty list or blank strings
/// clears the allowed tools list.
#[derive(Debug, Default, Clone)]
pub struct SkillPatch {
	pub name: Option<String>,
	pub description: Option<String>,
	pub author: Option<String>,
	pub version: Option<String>,
	pub content: Option<String>,
	pub tools: Option<Vec<String>>, // Some(vec![]) = clear
	pub enabled: Option<bool>,
}

impl SkillPatch {
	pub fn apply_to(self, existing: Skill) -> Skill {
		let tools = match self.tools {
			Some(list) => list
				.into_iter()
				.map(|s| s.trim().to_string())
				.filter(|s| !s.is_empty())
				.collect(),
			None => existing.tools,
		};

		Skill {
			name: self.name.unwrap_or(existing.name),
			enabled: self.enabled.unwrap_or(existing.enabled),
			description: self.description.or(existing.description),
			author: self.author.or(existing.author),
			version: self.version.or(existing.version),
			content: self.content.or(existing.content),
			tools,
			source_path: existing.source_path,
			canonical_path: existing.canonical_path,
			config_source: existing.config_source,
		}
	}
}

impl ConfigManager {
	/// Layout-aware skill removal with a default dry-run.
	///
	/// Builds a [`RemovalPlan`](crate::skills::removal::RemovalPlan) (symlink
	/// sweep + containment + canonical-keep checks), then deletes ONLY when it is
	/// not a dry-run AND either the plan is non-destructive or `confirm` is set.
	/// Deletion re-checks each path's type and containment at delete time (TOCTOU)
	/// and tolerates already-removed paths. On execution the per-scope skill lock
	/// is pruned and reported in [`RemovalOutcome::prune`] (`NotRun` on a
	/// dry-run/unconfirmed op). A prune failure is non-fatal. A single-scope
	/// failure leaves that one lock unchanged; under `Both` the two locks prune
	/// in sequence, so a partial prune is recorded in `Failed.pruned`.
	pub fn remove_skill_planned(
		&mut self,
		name: &str,
		all_agents: bool,
		dry_run: bool,
		confirm: bool,
	) -> Result<crate::skills::removal::RemovalOutcome> {
		let requested = [self.agent_type()];
		self.remove_skill_planned_for_agents(
			name, all_agents, dry_run, confirm, &requested,
		)
	}

	/// Batch-aware removal: `requested_agents` is the complete set authorized by
	/// this request to lose a shared Referrer. It must include this manager's
	/// agent; readers outside the set keep the Referrer if they need it.
	pub fn remove_skill_planned_for_agents(
		&mut self,
		name: &str,
		all_agents: bool,
		dry_run: bool,
		confirm: bool,
		requested_agents: &[crate::models::AgentType],
	) -> Result<crate::skills::removal::RemovalOutcome> {
		self.remove_skill_planned_inner(
			name,
			None,
			all_agents,
			dry_run,
			confirm,
			requested_agents,
		)
	}

	/// Remove one exact discovered location. Its in-scope read directory is
	/// derived here, so a same-name skill at another location cannot be chosen
	/// by the manager's name-deduplicated view.
	pub fn remove_skill_planned_at_dir_for_agents(
		&mut self,
		name: &str,
		target_entry: &std::path::Path,
		all_agents: bool,
		dry_run: bool,
		confirm: bool,
		requested_agents: &[crate::models::AgentType],
	) -> Result<crate::skills::removal::RemovalOutcome> {
		self.remove_skill_planned_inner(
			name,
			Some(target_entry),
			all_agents,
			dry_run,
			confirm,
			requested_agents,
		)
	}

	/// True when the named skill exists in the in-scope lock file.
	pub(crate) fn skill_has_lock_entry(&self, name: &str) -> bool {
		let in_global = self.scope != crate::models::ResourceScope::ProjectOnly
			&& skill::read_skill_lock().skills.contains_key(name);
		let in_project = self.scope != crate::models::ResourceScope::GlobalOnly
			&& self
				.project_root
				.as_deref()
				.map(|r| {
					skill::read_local_lock(Some(r)).skills.contains_key(name)
				})
				.unwrap_or(false);
		in_global || in_project
	}

	/// Produce a no-op removal outcome for an absent skill, attributing
	/// `Verdict::LockOnly` if an in-scope lock entry exists, else `Verdict::Absent`.
	pub fn skill_noop_outcome(
		&self,
		name: &str,
	) -> crate::skills::removal::RemovalOutcome {
		let has_lock = self.skill_has_lock_entry(name);
		crate::skills::removal::RemovalOutcome::noop(has_lock)
	}

	fn remove_skill_planned_inner(
		&mut self,
		name: &str,
		target_entry: Option<&std::path::Path>,
		all_agents: bool,
		dry_run: bool,
		confirm: bool,
		requested_agents: &[crate::models::AgentType],
	) -> Result<crate::skills::removal::RemovalOutcome> {
		use crate::skills::removal;
		if !requested_agents.contains(&self.agent_type()) {
			return Err(ConfigError::InvalidConfig(
				"removal request does not include the target agent".into(),
			));
		}

		// Taken FIRST on any executing path, before the target lookup and the
		// referrer sweep: both decide what to delete. A dry-run takes none.
		let _mutation_guard = if dry_run {
			None
		} else {
			Some(self.guard_and_reload("remove skill", self.scope)?)
		};

		let skill = self.skill_for_planned_removal(name, all_agents)?;

		let scope = self.scope;
		let project_root = self.project_root.clone();
		let all_agent_dirs =
			removal::agent_skill_dirs_in_scope(scope, project_root.as_deref());
		let roots = removal::allowed_skill_roots(
			&all_agent_dirs,
			project_root.as_deref(),
		);
		let own_agent_dir = if let Some(target_entry) = target_entry {
			let requested_entry = removal::entry_identity(target_entry);
			let target_dir = requested_agents
				.iter()
				.flat_map(|agent| {
					crate::create_adapter(*agent)
						.get_skills_paths(project_root.as_deref(), scope)
				})
				.filter(|dir| {
					let root = skill::lock::resolve_existing(dir);
					requested_entry != root
						&& requested_entry.starts_with(root)
						&& removal::assert_contained(dir, &roots).is_some()
				})
				.max_by_key(|dir| dir.components().count())
				.ok_or_else(|| {
					ConfigError::InvalidConfig(
						"requested skill location is not in an in-scope read directory"
							.into(),
					)
				})?;
			let (found, incomplete) =
				crate::skills::discovery::load_skills_from_dir_partial(
					&target_dir,
				);
			if incomplete {
				return Err(ConfigError::InvalidConfig(
					"requested skill location could not be fully read".into(),
				));
			}
			let distinct_masters: std::collections::HashSet<_> = found
				.iter()
				.filter(|found| found.name == name)
				.filter_map(removal::skill_root)
				.map(|root| skill::lock::resolve_existing(&root))
				.collect();
			if distinct_masters.len() > 1 {
				return Err(ConfigError::InvalidConfig(
					"requested skill location is ambiguous: multiple Masters have this name"
						.into(),
				));
			}
			let selected_master = removal::skill_root(&skill)
				.map(|path| skill::lock::resolve_existing(&path));
			let requested_master = skill::lock::resolve_existing(target_entry);
			if selected_master.as_deref() != Some(requested_master.as_path()) {
				return Err(ConfigError::InvalidConfig(
					"requested skill location does not match the loaded Master"
						.into(),
				));
			}
			Some(target_dir)
		} else {
			self.target_skills_dir()
		};
		let mut plan = removal::plan_removal_for_agents(
			&skill,
			own_agent_dir.as_deref(),
			&all_agent_dirs,
			project_root.as_deref(),
			scope,
			all_agents,
			requested_agents,
		);

		let executed = !dry_run && (!plan.needs_confirm || confirm);

		// Refuse rather than report a removal that cannot happen — and ask
		// DISCOVERY whether it can, never the plan. This is the ONE home for the
		// verdict (CLI delete, API delete, `transfer::reconcile_skill`).
		// `--all-agents` widens the read dirs to every agent's: its promise is
		// "gone everywhere". See crates/core/AGENTS.md "Did that removal take
		// anything away?".
		// See docs/history/core-removal.md#all-agents-delete-asked-only-the-initiator
		let read_dirs = if all_agents {
			all_agent_dirs.clone()
		} else {
			self.adapter
				.get_skills_paths(project_root.as_deref(), scope)
		};
		let effect =
			removal::read_effect_after(&read_dirs, &skill.name, &plan.paths);

		// A removal that goes ahead while something else still serves the skill
		// has to SAY so, through `skipped` ("present and deliberately not taken").
		if effect.changed {
			for survivor in &effect.survivors {
				if !plan.skipped.contains(survivor) {
					plan.skipped.push(survivor.clone());
				}
			}
		}

		let unmanaged_dirs = removal::unmanaged_skill_dirs(
			&all_agent_dirs,
			project_root.as_deref(),
			requested_agents,
		);

		let readers_outside_fn = || {
			let mut readers_outside: Vec<&'static str> = Vec::new();
			for path in effect.survivors.iter().chain(plan.skipped.iter()) {
				if let Some(parent) = path.parent() {
					for id in removal::skill_dir_readers_outside(
						parent,
						scope,
						project_root.as_deref(),
						requested_agents,
					) {
						if !readers_outside.contains(&id) {
							readers_outside.push(id);
						}
					}
				}
			}
			readers_outside
		};

		let git_refusal_fn = || {
			effect.survivors.iter().chain(plan.skipped.iter()).find_map(
				|path| {
					let reason = if all_agents {
						removal::shared_slot_git_keep(
							path,
							project_root.as_deref(),
						)
					} else {
						removal::single_agent_keep_reason(
							path,
							&all_agent_dirs,
							name,
							project_root.as_deref(),
							scope,
							requested_agents,
						)
					}?;
					removal::git_keep_hint(&reason, path)
				},
			)
		};

		let verdict = removal::Verdict::compute(removal::VerdictInputs {
			plan_paths: &plan.paths,
			plan_skipped: &plan.skipped,
			initial_shared_master_kept: plan.shared_master_kept,
			effect: &effect,
			all_agents,
			unmanaged_dirs: &unmanaged_dirs,
			has_lock_entry: self.skill_has_lock_entry(name),
			git_refusal: &git_refusal_fn,
			readers_outside: &readers_outside_fn,
		});

		plan.shared_master_kept = verdict.shared_master_kept();
		if matches!(verdict, removal::Verdict::Refused { .. }) {
			let mut still: Vec<std::path::PathBuf> = Vec::new();
			for path in effect.survivors.iter().chain(plan.skipped.iter()) {
				if !still.contains(path) {
					still.push(path.clone());
				}
			}
			plan.still_read_from = still;
		}

		if executed {
			if let removal::Verdict::Refused { ref reason } = verdict {
				let operation = if all_agents {
					"remove from every agent"
				} else {
					"remove for this agent alone"
				};
				return Err(ConfigError::unsupported_operation(
					operation,
					reason,
					self.adapter.name(),
				));
			}
		}

		// Kept never commits: it would report `removed` and prune lock.
		if matches!(verdict, removal::Verdict::Kept { .. }) {
			return removal::RemovalOutcome::preview(
				plan,
				verdict,
				scope,
				project_root.as_deref(),
				name,
			);
		}

		if !executed {
			return removal::RemovalOutcome::preview(
				plan,
				verdict,
				scope,
				project_root.as_deref(),
				name,
			);
		}

		info!(
			"removing skill '{}' (layout={:?}, all_agents={})",
			name, plan.layout, all_agents
		);
		// Execute + fold the real result back into the plan + reconcile the
		// per-scope lock, through the ONE producer both surfaces use.
		let mut outcome = removal::RemovalOutcome::commit(
			plan,
			&roots,
			scope,
			project_root.as_deref(),
			name,
		)?;

		if outcome.failed_paths.is_empty() {
			outcome.verdict = verdict;
		}

		// Skills are disk-derived; drop the in-memory view (save_current persists
		// MCPs, not skills, so this is a best-effort cache update).
		let cfg = self.config_mut()?;
		if let Some(idx) = cfg.skills.iter().position(|s| s.name == name) {
			cfg.skills.remove(idx);
		}

		Ok(outcome)
	}

	fn skill_for_planned_removal(
		&self,
		name: &str,
		all_agents: bool,
	) -> Result<Skill> {
		let config = self.config.as_ref().ok_or_else(|| {
			ConfigError::InvalidConfig("No configuration loaded".to_string())
		})?;
		let from_caller =
			config.skills.iter().find(|s| s.name == name).cloned();
		if !all_agents {
			return from_caller
				.ok_or_else(|| ConfigError::resource_not_found("skill", name));
		}

		// The store scan runs BEFORE either early return: its refusals (duplicate
		// Master, fail-closed read) judge THE STORE, which an agent Referrer
		// says nothing about. A Master without Referrers is still found here
		// (via discovery, never a `dir.join(name)` guess).
		// See docs/history/core-manager.md#store-checks-skipped-for-a-linked-master
		use crate::models::ResourceScope;
		use crate::skills::{
			discovery::load_master_skills, linker::master_store_dir,
		};
		let mut stores = Vec::new();
		if self.scope != ResourceScope::GlobalOnly {
			if let Some(root) = self.project_root.as_deref() {
				stores.extend(master_store_dir(Some(root)));
			}
		}
		if self.scope != ResourceScope::ProjectOnly {
			stores.extend(master_store_dir(None));
		}
		// The orphan fallback, taken only if neither agent path answers. The
		// first store with exactly one match wins: scanning every store would let
		// a GLOBAL duplicate refuse a project removal that never touched it.
		let mut orphan_master = None;
		for store in stores {
			// FAIL CLOSED on a store this cannot fully read: an unreadable
			// `SKILL.md` has an UNKNOWN name and may be this very skill under
			// another folder name, hiding a duplicate or answering `absent`
			// with bytes on disk. `load_master_skills` is `Err`-or-nothing on
			// purpose; the error names the path.
			let mut found = load_master_skills(&store)?
				.into_iter()
				.filter(|skill| skill.name == name);
			match (found.next(), found.next()) {
				(Some(mut skill), None) => {
					if skill.canonical_path.is_none() {
						skill.canonical_path = skill.source_path.clone();
					}
					orphan_master = Some(skill);
					break;
				}
				// Two live Masters under one frontmatter name: refuse rather
				// than delete one by `read_dir` order (as resync does).
				(Some(first), Some(second)) => {
					return Err(ConfigError::InvalidConfig(format!(
						"skill '{name}' has two Masters in {}: '{}' and \
						 '{}'. Remove or rename one of them, then re-run.",
						store.display(),
						first.source_path.as_deref().unwrap_or("<unknown>"),
						second.source_path.as_deref().unwrap_or("<unknown>"),
					)));
				}
				(None, _) => {}
			}
		}

		// Selection order is unchanged: the caller's own agent, then a peer,
		// then the unlinked Master.
		if let Some(skill) = from_caller {
			return Ok(skill);
		}
		for resources in
			crate::load_all_agents(self.scope, self.project_root.as_deref())
		{
			if let Some(skill) =
				resources.skills.into_iter().find(|s| s.name == name)
			{
				debug!(
					"using '{}' skill from agent '{}' for all-agent removal",
					name, resources.agent_id
				);
				return Ok(skill);
			}
		}

		orphan_master
			.ok_or_else(|| ConfigError::resource_not_found("skill", name))
	}

	/// Refuse: there is no writer for a skill's enabled flag.
	///
	/// `save()` serializes MCPs only, so a flipped flag cannot persist; an
	/// honest refusal beats a silent no-op that rewrites `.mcp.json`.
	/// See docs/history/core-manager.md#skill-mutations-rewrote-mcp-config
	fn set_skill_enabled(&mut self, name: &str, enabled: bool) -> Result<()> {
		// Resolve the name FIRST: a missing skill is a not-found, and callers
		// (including the API's 404) depend on that answer winning over the
		// unsupported-operation refusal below.
		let config = self.config_mut()?;
		if !config.skills.iter().any(|s| s.name == name) {
			return Err(ConfigError::resource_not_found("skill", name));
		}
		Err(ConfigError::unsupported_operation(
			if enabled { "enable" } else { "disable" },
			"skill",
			self.adapter.name(),
		))
	}

	pub fn disable_skill(&mut self, name: &str) -> Result<()> {
		self.set_skill_enabled(name, false)
	}

	pub fn enable_skill(&mut self, name: &str) -> Result<()> {
		self.set_skill_enabled(name, true)
	}

	pub fn add_skill_from_path(&mut self, path: &Path) -> Result<SkillAdd> {
		debug!(
			"adding skill from path '{}' for agent '{}'",
			path.display(),
			self.adapter.name()
		);
		// Symlink-only model: there is no copy install path.
		self.add_skill_from_path_universal(path, None)
	}

	/// Symlink-only install from a local path: parses the skill, writes the full
	/// source tree once into the Master store and symlinks THIS agent's skills
	/// dir to it (matching the API's `install_git_skill_universal`).
	///
	/// `as_name` (CLI `add --from <path> --name <new>`) is applied BEFORE the
	/// duplicate check, so the install is ONE step inside ONE lock span.
	/// See docs/history/core-manager.md#add-as-name-was-import-then-rename
	pub fn add_skill_from_path_universal(
		&mut self,
		path: &Path,
		as_name: Option<&str>,
	) -> Result<SkillAdd> {
		debug!(
			"adding skill (universal) from path '{}' for agent '{}'",
			path.display(),
			self.adapter.name()
		);
		let skill_pkg = skill::parser::parse(path).map_err(|e| {
			ConfigError::InvalidConfig(format!("Failed to parse skill: {e}"))
		})?;
		let mut skill = convert_skill(skill_pkg);
		// The source's OWN name, kept because the Master's SKILL.md still
		// carries it after the copy and discovery keys on that field.
		let source_name = skill.name.clone();
		if let Some(requested) = as_name {
			skill.name = requested.to_string();
		}

		let UniversalPrep {
			agent_name,
			agent_write_dir,
			canonical_dir,
			use_relative,
			link_need,
		} = self.universal_install_prep()?;
		// Capture the materializer inputs BEFORE borrowing `config` mutably so
		// the shared materializer can run while the config check is in flight.
		let scope = self.write_scope;
		let project_root = self.project_root.clone();
		let agent_type = self.agent_type();

		// Same span as `add_skill_universal`: duplicate check → Master → link,
		// with config re-read under the lock.
		let _mutation_guard =
			self.guard_and_reload("add skill from path", scope)?;

		let config = self.config_mut()?;
		if config.skills.iter().any(|s| s.name == skill.name) {
			// An explicit `--name` never takes the idempotent no-op branches
			// (they write NOTHING); refusing BEFORE any write needs no rollback.
			if as_name.is_some() {
				return Err(ConfigError::resource_exists("skill", &skill.name));
			}
			let safe = sanitize_name(&skill.name);
			let canonical = canonical_dir.join(&safe);
			// The no-op reports the INSTALLED skill, never the one parsed from
			// `path`: nothing was written.
			let installed = || {
				config
					.skills
					.iter()
					.find(|s| s.name == skill.name)
					.cloned()
					.unwrap_or_else(|| skill.clone())
			};
			// Idempotence is decided by the RESOLVED link target, never by the
			// skill name: a name match on an unrelated same-named skill once let
			// `reconcile --remove` delete the only copy.
			// See docs/history/core-manager.md#name-based-idempotence-deleted-the-source
			if let Some(ref agent_dir) = agent_write_dir {
				let slot = agent_dir.join(&safe);
				if Linker::is_link(&slot) {
					let master_real = std::fs::canonicalize(&canonical)
						.unwrap_or_else(|_| canonical.clone());
					if std::fs::canonicalize(&slot)
						.map(|r| r == master_real)
						.unwrap_or(false)
					{
						return Ok(SkillAdd::already_installed(installed()));
					}
				}
			}
			return Err(ConfigError::resource_exists("skill", &skill.name));
		}
		info!(
			"adding skill '{}' (universal layout, from path) for agent '{}'",
			skill.name, agent_name
		);

		let safe_name = sanitize_name(&skill.name);
		let canonical = canonical_dir.join(&safe_name);

		// ONE materializer, shared with the fetched/desktop path: copies only
		// when the Master is absent (a pre-existing one is preserved), then links.
		let source_root = crate::skills::skill_source_root(path);
		let target_link = if use_relative {
			crate::skills::linker::LinkTarget::Relative
		} else {
			crate::skills::linker::LinkTarget::Absolute
		};
		let results =
			crate::skills::install_fetched::materialize_universal_master(
				&source_root,
				&safe_name,
				scope,
				project_root.as_deref(),
				std::slice::from_ref(&agent_type),
				target_link,
			)?;
		Self::ensure_single_agent_installed(
			&results.agent_results,
			&link_need,
			&skill.name,
		)?;

		// The copied SKILL.md still says `name: <source_name>`, and discovery
		// keys on frontmatter, so a renamed install must rewrite it. Gated on the
		// materializer's OWN receipt (`created_master`, an atomic create-claim):
		// never edit a Master someone else owns (crates/core/AGENTS.md
		// "Mutation attribution").
		if skill.name != source_name {
			if !results.created_master {
				undo_created_referrers(&results, &safe_name);
				return Err(ConfigError::resource_exists("skill", &skill.name));
			}
			if let Err(e) = rewrite_master_skill_name(&canonical, &skill.name) {
				// Undo this call's own writes: the Master it just claimed and
				// every referrer it just linked.
				undo_created_referrers(&results, &safe_name);
				let _ = std::fs::remove_dir_all(&canonical);
				return Err(ConfigError::Io(e));
			}
		}

		let canonical_md =
			canonical.join("SKILL.md").to_string_lossy().to_string();
		let mut fs_skill = skill.clone();
		fs_skill.source_path = Some(canonical_md.clone());
		fs_skill.canonical_path = Some(canonical_md);
		// What is ON DISK: `created_master` is false exactly when a pre-existing
		// Master was preserved and the parsed source never written.
		let on_disk = if results.created_master {
			fs_skill.clone()
		} else {
			warn!(
				"canonical '{}' already exists; reusing it without overwriting \
				 SKILL.md — the skill reported below is the MASTER on disk, not \
				 the source just read (use `aghub update` to refresh content)",
				canonical.display()
			);
			match master_on_disk(&canonical, &skill.name) {
				Some(found) => found,
				// Do NOT fall back to the parsed source: nothing can read a
				// Master that does not parse. Undo the referrer and say so.
				None => {
					crate::skills::rename::rollback_materialized_install(
						&skill.name,
						scope,
						project_root.as_deref(),
						&results.created_referrer_dirs,
						false,
					);
					return Err(ConfigError::InvalidConfig(format!(
						"the existing master at '{}' does not parse, so nothing \
						 can read it and aghub cannot report what is installed. \
						 Fix or remove that master, then re-run.",
						canonical.display()
					)));
				}
			}
		};
		config.skills.push(on_disk.clone());

		// Report the entry ON DISK, never the parsed source (whose `source_path`
		// names the caller's input). Not `save_current()` — see `add_skill`.
		Ok(SkillAdd::installed(on_disk, &results))
	}

	/// Map the single-agent result from `materialize_universal_master` onto the
	/// CLI add path's historical error contract.
	///
	/// A `NeedsLink` agent must link cleanly (a foreign occupant or hard link
	/// failure is an error). `Unsupported` is rejected earlier in the
	/// materializer's preflight, so its arm here is defensive only.
	fn ensure_single_agent_installed(
		results: &[crate::skills::install_fetched::AgentInstallResult],
		link_need: &crate::skills::linker::LinkNeed,
		skill_name: &str,
	) -> Result<()> {
		if !matches!(
			link_need,
			crate::skills::linker::LinkNeed::NeedsLink { .. }
		) {
			return Ok(());
		}
		match results.first() {
			Some(r) if r.error.is_none() => Ok(()),
			Some(r)
				if r.error.as_deref().is_some_and(|e| {
					e.contains(crate::skills::linker::LINKED_STORE_ERROR)
				}) =>
			{
				Err(ConfigError::InvalidConfig(
					crate::skills::linker::LINKED_STORE_ERROR.to_string(),
				))
			}
			_ => Err(ConfigError::resource_exists("skill", skill_name)),
		}
	}

	pub fn validate_skill_path(&self, path: &Path) -> Vec<String> {
		let mut errors = Vec::new();
		match skill::parser::parse(path) {
			Ok(_) => {}
			Err(e) => {
				warn!("skill validation failed for '{}': {e}", path.display());
				errors.push(format!("Parse error: {e}"));
			}
		}
		errors
	}

	fn target_skills_dir(&self) -> Option<PathBuf> {
		self.adapter
			.target_skills_dir(self.project_root.as_deref(), self.scope)
	}

	fn universal_install_prep(&self) -> Result<UniversalPrep> {
		let project_root_for_canonical = match self.write_scope {
			crate::models::ResourceScope::ProjectOnly => {
				self.project_root.clone()
			}
			_ => None,
		};
		let canonical_dir = crate::skills::linker::master_store_dir(
			project_root_for_canonical.as_deref(),
		)
		.ok_or_else(|| {
			ConfigError::InvalidConfig(
				"Cannot resolve the .aghub Master store directory".into(),
			)
		})?;
		// Where THIS agent's Referrer goes, with the same scope/root that derived
		// `canonical_dir`, mirroring the fetched/desktop install path.
		let descriptor = crate::registry::get(self.agent_type());
		let link_need = crate::skills::linker::agent_link_need(
			descriptor,
			self.write_scope,
			self.project_root.as_deref(),
		);
		Ok(UniversalPrep {
			agent_name: self.adapter.name().to_string(),
			agent_write_dir: self.target_skills_dir(),
			use_relative: project_root_for_canonical.is_some(),
			canonical_dir,
			link_need,
		})
	}

	/// Every OTHER agent that would receive this skill through the SAME Referrer
	/// directory at this scope — who else a grant reaches.
	pub fn skill_target_shares_with(&self) -> Vec<&'static str> {
		self.universal_install_prep()
			.map(|prep| match prep.link_need {
				crate::skills::linker::LinkNeed::NeedsLink {
					ref referrer_dir,
				} => crate::skills::linker::shared_with(
					self.agent_type().as_str(),
					referrer_dir,
					self.write_scope,
					self.project_root.as_deref(),
				),
				crate::skills::linker::LinkNeed::Unsupported => Vec::new(),
			})
			.unwrap_or_default()
	}
}

/// In-scope agent skill dirs whose `<dir>/<safe_old>` entry is a symlink that
/// resolves to `old_real` (the already-canonicalized old master) — i.e. the
/// per-agent views that point at the universal master and must be re-pointed
/// after the master is renamed. MUST be called BEFORE the master is renamed
/// (the per-link `canonicalize` resolves through the still-present master).
fn universal_relink_referrers(
	old_real: &Path,
	safe_old: &str,
	agent_dirs: &[PathBuf],
) -> Vec<PathBuf> {
	agent_dirs
		.iter()
		.filter(|dir| {
			let link = dir.join(safe_old);
			Linker::is_link(&link)
				&& std::fs::canonicalize(&link)
					.map(|resolved| resolved == *old_real)
					.unwrap_or(false)
		})
		.cloned()
		.collect()
}

/// After a universal master is renamed to `new_canonical`, unlink each agent's
/// now-dangling old-name symlink and recreate a new-name symlink pointing at the
/// renamed master (relative/absolute per `use_relative`). Keeps the symlink
/// layout intact so the skill still works for every linked agent.
fn universal_relink_agents(
	new_canonical: &Path,
	referrers: &[PathBuf],
	safe_old: &str,
	use_relative: bool,
) -> Result<()> {
	for dir in referrers {
		let old_link = dir.join(safe_old);
		if Linker::is_link(&old_link) {
			Linker::unlink(&old_link).map_err(|e| {
				ConfigError::Io(std::io::Error::new(
					e.kind(),
					format!(
						"Failed to unlink stale link '{}': {}",
						old_link.display(),
						e
					),
				))
			})?;
		}
	}
	let report = crate::skills::linker::link_agents_to_canonical(
		new_canonical,
		referrers,
		if use_relative {
			crate::skills::linker::LinkTarget::Relative
		} else {
			crate::skills::linker::LinkTarget::Absolute
		},
	)
	.map_err(|e| ConfigError::Io(std::io::Error::other(e.to_string())))?;
	// A per-agent conflict/failure is DATA to an install but a hard FAILURE to
	// a rename (the old-name link is already gone), so error into the rollback.
	// Kept here, not in the shared `link_agents_to_canonical`.
	if let Some(shortfall) = relink_shortfall(&report) {
		return Err(ConfigError::Io(std::io::Error::other(shortfall)));
	}
	Ok(())
}

/// The referrers a relink failed to re-point, as one error message — `None`
/// when every referrer was linked. Shared by the relink and its rollback: a
/// rollback that silently tolerated a conflict would leave the very dangling
/// referrer the rollback exists to prevent.
fn relink_shortfall(
	report: &crate::skills::linker::UniversalInstallReport,
) -> Option<String> {
	if report.conflicts.is_empty() && report.failed.is_empty() {
		return None;
	}
	let mut blocked: Vec<String> = report
		.conflicts
		.iter()
		.map(|p| format!("{} (occupied)", p.display()))
		.collect();
	blocked.extend(
		report
			.failed
			.iter()
			.map(|(p, e)| format!("{} ({e})", p.display())),
	);
	Some(format!(
		"could not re-point {} skill referrer(s) at the renamed master: {}",
		blocked.len(),
		blocked.join(", ")
	))
}

/// Rename a skill's master directory and, for a universal skill, re-point its
/// referrers. The rename and the relink are one transaction: if the relink
/// fails, the master is renamed back and the old-name symlinks restored, so a
/// partial failure can never leave referrers dangling. The transaction boundary
/// is deliberately rename + relink only — a later SKILL.md write or config save
/// runs against an already-consistent filesystem (see
/// docs/adr/0001-transactional-universal-skill-rename.md). Returns the SKILL.md
/// path inside the renamed master.
#[allow(clippy::too_many_arguments)]
fn rename_skill_master(
	old_master: &Path,
	file_name: &std::ffi::OsStr,
	safe_old: &str,
	safe_new: &str,
	is_universal: bool,
	scope: crate::models::ResourceScope,
	project_root: Option<&Path>,
	use_relative: bool,
) -> Result<PathBuf> {
	// Record the referrers BEFORE the rename: each per-link `canonicalize`
	// resolves through the still-present master. Resolve the old master first
	// and ABORT if it cannot be resolved — better to fail than to rename it and
	// silently orphan every per-agent symlink.
	let referrers = if is_universal {
		let old_real = std::fs::canonicalize(old_master).map_err(|e| {
			ConfigError::Io(std::io::Error::new(
				e.kind(),
				format!(
					"Failed to resolve skill master '{}': {e}",
					old_master.display()
				),
			))
		})?;
		let agent_dirs = crate::skills::removal::agent_skill_dirs_in_scope(
			scope,
			project_root,
		);
		universal_relink_referrers(&old_real, safe_old, &agent_dirs)
	} else {
		Vec::new()
	};

	let new_master = old_master.with_file_name(safe_new);
	// Never rename onto an existing skill dir: `fs::rename` onto an empty target
	// would silently clobber it — a real data-loss risk for the shared `.agents`
	// universal master.
	if new_master.exists() {
		return Err(ConfigError::resource_exists("skill", safe_new));
	}
	std::fs::rename(old_master, &new_master).map_err(|e| {
		ConfigError::Io(std::io::Error::new(
			e.kind(),
			format!(
				"Failed to rename skill directory '{}' -> '{}': {e}",
				old_master.display(),
				new_master.display()
			),
		))
	})?;

	if is_universal {
		if let Err(relink_err) = universal_relink_agents(
			&new_master,
			&referrers,
			safe_old,
			use_relative,
		) {
			return Err(rollback_master_rename(
				&new_master,
				old_master,
				&referrers,
				safe_new,
				use_relative,
				relink_err,
			));
		}
	}
	Ok(new_master.join(file_name))
}

/// Undo a partial universal rename: put the master back, drop any half-created
/// new-name symlinks, and restore the old-name symlinks. Returns the original
/// relink error on success. If the rollback itself fails, returns a compound
/// error naming both failures plus a structured [`RecoveryHint`] next step:
/// `ManualRestore` (master still at the new name — the only surviving copy)
/// when the master-restore rename fails, or `BrokenSymlink` (master safely
/// restored, a stale link blocks the relink) when a link op fails.
/// (see [`crate::skills::update::RecoveryHint`])
fn rollback_master_rename(
	new_master: &Path,
	old_master: &Path,
	referrers: &[PathBuf],
	safe_new: &str,
	use_relative: bool,
	relink_err: ConfigError,
) -> ConfigError {
	// Which step of the rollback failed, so the caller can pick the right
	// RecoveryHint: a failed master-restore leaves the only copy at new_master
	// (ManualRestore); a failed unlink/relink means the master is safely back
	// but a stale/foreign link blocks the relink (BrokenSymlink).
	enum RbFail {
		Restore(std::io::Error),
		Relink { err: std::io::Error, link: PathBuf },
	}
	let do_rollback = || -> std::result::Result<(), RbFail> {
		// Put the master back first so old-name symlinks resolve again.
		std::fs::rename(new_master, old_master).map_err(RbFail::Restore)?;
		// Remove any new-name symlinks the partial relink managed to create
		// (they now point at the vanished new_master).
		for dir in referrers {
			let new_link = dir.join(safe_new);
			if Linker::is_link(&new_link) {
				Linker::unlink(&new_link).map_err(|err| RbFail::Relink {
					err,
					link: new_link.clone(),
				})?;
			}
		}
		// Recreate any old-name symlinks the partial relink removed; ones still
		// present resolve to the restored master and are left untouched.
		let report = crate::skills::linker::link_agents_to_canonical(
			old_master,
			referrers,
			if use_relative {
				crate::skills::linker::LinkTarget::Relative
			} else {
				crate::skills::linker::LinkTarget::Absolute
			},
		)
		.map_err(|e| RbFail::Relink {
			err: std::io::Error::other(e.to_string()),
			link: old_master.to_path_buf(),
		})?;
		// Same as the forward relink: an unlinked row breaks the rollback's promise.
		if let Some(shortfall) = relink_shortfall(&report) {
			return Err(RbFail::Relink {
				err: std::io::Error::other(shortfall),
				link: old_master.to_path_buf(),
			});
		}
		Ok(())
	};
	match do_rollback() {
		Ok(()) => relink_err,
		// Master could not be restored: it is the ONLY surviving copy at
		// new_master and must be moved back to old_master by hand.
		Err(RbFail::Restore(rb_err)) => {
			let hint = crate::skills::update::RecoveryHint::ManualRestore {
				recover_from: new_master.to_path_buf(),
				restore_to: old_master.to_path_buf(),
			};
			ConfigError::Io(std::io::Error::other(format!(
				"skill relink failed ({relink_err}) and rollback also \
				 failed ({rb_err}); {}",
				hint.next_step()
			)))
		}
		// Master is safely restored, but a dangling/foreign link blocks the
		// relink — point at the offending link, data is not at risk.
		Err(RbFail::Relink { err: rb_err, link }) => {
			let hint =
				crate::skills::update::RecoveryHint::BrokenSymlink { link };
			ConfigError::Io(std::io::Error::other(format!(
				"skill relink failed ({relink_err}) and rollback also \
				 failed ({rb_err}); {}",
				hint.next_step()
			)))
		}
	}
}

/// Point a freshly-copied Master's frontmatter `name:` at the name it was
/// installed under. Everything else — body, `license`, `compatibility`, any key
/// the author wrote — is carried through the raw map untouched; only `name` is
/// replaced. Deliberately NOT via `format_skill`, which reserializes from the
/// reduced model and would drop exactly those keys.
fn rewrite_master_skill_name(
	master: &Path,
	new_name: &str,
) -> std::io::Result<()> {
	let md = master.join("SKILL.md");
	let text = std::fs::read_to_string(&md)?;
	let (metadata, body) = skill::parse_frontmatter(&text).map_err(|e| {
		std::io::Error::other(format!(
			"cannot rename '{}': unreadable frontmatter ({e})",
			md.display()
		))
	})?;
	let mut map: BTreeMap<String, serde_yaml::Value> =
		metadata.into_iter().collect();
	map.insert(
		"name".to_string(),
		serde_yaml::Value::String(new_name.to_string()),
	);
	let yaml = serde_yaml::to_string(&map).map_err(std::io::Error::other)?;
	std::fs::write(&md, format!("---\n{yaml}---\n\n{body}\n"))
}

/// Drop the referrer links THIS install created, by the linker's own receipt.
/// Best-effort: it runs on a path that is already returning an error, and a
/// link that is gone is the state we wanted anyway.
fn undo_created_referrers(
	results: &crate::skills::install_fetched::MaterializedMaster,
	safe_name: &str,
) {
	for dir in &results.created_referrer_dirs {
		let link = dir.join(safe_name);
		if Linker::is_link(&link) {
			let _ = Linker::unlink(&link);
		}
	}
}

/// The frontmatter keys aghub's reduced [`Skill`] model owns. Everything else
/// in a SKILL.md's frontmatter belongs to the skill's author and is carried
/// through untouched by [`preserved_frontmatter`].
const MODELED_FRONTMATTER_KEYS: [&str; 5] =
	["name", "description", "author", "version", "allowed-tools"];

/// The frontmatter keys of an existing SKILL.md that aghub does NOT model, so a
/// rewrite can put them back.
///
/// Otherwise `update_skill` reserializes from the reduced model and silently
/// drops `license` / `compatibility` / any author key. MODELED keys are
/// stripped rather than merged, so clearing one (`author: None`) still clears it.
///
/// Best-effort: an unreadable file yields an empty map, never a failed update.
fn preserved_frontmatter(path: &Path) -> BTreeMap<String, serde_yaml::Value> {
	let Ok(text) = std::fs::read_to_string(path) else {
		return BTreeMap::new();
	};
	let Ok((metadata, _)) = skill::parse_frontmatter(&text) else {
		return BTreeMap::new();
	};
	metadata
		.into_iter()
		.filter(|(key, _)| !MODELED_FRONTMATTER_KEYS.contains(&key.as_str()))
		.collect()
}

/// Serialize frontmatter fields as structured YAML via serde_yaml, on top of
/// the author's own unmodeled keys (`preserved`, from [`preserved_frontmatter`];
/// empty when writing a brand-new file).
fn serialize_frontmatter(
	skill: &Skill,
	preserved: &BTreeMap<String, serde_yaml::Value>,
) -> String {
	let mut map = preserved.clone();
	map.insert(
		"name".to_string(),
		serde_yaml::Value::String(skill.name.clone()),
	);
	let description = skill
		.description
		.as_deref()
		.unwrap_or("")
		.replace('\n', " ");
	map.insert(
		"description".to_string(),
		serde_yaml::Value::String(description),
	);
	if let Some(author) = &skill.author {
		map.insert(
			"author".to_string(),
			serde_yaml::Value::String(author.clone()),
		);
	}
	if let Some(version) = &skill.version {
		map.insert(
			"version".to_string(),
			serde_yaml::Value::String(version.clone()),
		);
	}
	if !skill.tools.is_empty() {
		map.insert(
			"allowed-tools".to_string(),
			serde_yaml::Value::String(skill.tools.join(",")),
		);
	}
	serde_yaml::to_string(&map).unwrap_or_default()
}

/// Format a Skill as a valid SKILL.md, preserving existing body content
/// unless new body content is explicitly supplied.
fn format_skill(
	skill: &Skill,
	existing_body: Option<&str>,
	preserved: &BTreeMap<String, serde_yaml::Value>,
) -> String {
	let yaml = serialize_frontmatter(skill, preserved);
	let mut out = String::from("---\n");
	out.push_str(&yaml);
	out.push_str("---\n");

	if let Some(body) = skill.content.as_deref().or(existing_body) {
		out.push_str(body);
	} else {
		out.push_str(&format!("\n# {}\n\n", skill.name));
	}

	out
}

#[cfg(test)]
mod tests;
