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

/// Shared preparation for the two universal-install entry points
/// (`add_skill_universal` / `add_skill_from_path_universal`): resolves the
/// `.agents` canonical directory plus the current agent's symlink target.
struct UniversalPrep {
	agent_name: String,
	agent_write_dir: Option<PathBuf>,
	canonical_dir: PathBuf,
	use_relative: bool,
	/// How THIS agent relates to the `.agents/skills` Master at this scope.
	/// `NativeReader` → it reads the Master directly, so no per-agent link is
	/// created (parity with the fetched/desktop install path). Computed via the
	/// shared classifier, not the narrow `agent_write_dir == canonical_dir` test.
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
///   `<project>/.agents/skills/<safe_name>/SKILL.md`); the per-agent symlink
///   that needs to be unlinked lives at `<target_dir>/<safe_name>`.
///
/// For universal skills we intentionally leave the canonical master intact
/// (other agents or `npx skills` may still reference it). Full layout-aware
/// removal of the canonical goes via
/// [`ConfigManager::remove_skill_planned`] with `all_agents = true`.
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
	pub fn add_skill(&mut self, skill: Skill) -> Result<()> {
		// Symlink-only model (Locked Decision 1): manual skill creation writes a
		// single .agents/skills/<name> Master and links THIS agent to it, exactly
		// like every other install path. The old isolated agent-local copy body is
		// removed; there is no copy install path. (`add_skill_universal` already
		// holds the duplicate-name guard, the unsupported-scope error, and
		// `save_current`; `universal_install_prep` resolves the agent name.)
		self.add_skill_universal(skill)
	}

	/// Add a skill in *universal* layout: write the real `SKILL.md` once into
	/// `.agents/skills/<name>` and symlink THIS agent's skills dir to it
	/// (npx-style). Sets `canonical_path` so layout-aware removal recognises
	/// the symlink. Both [`Self::add_skill`] and [`Self::add_skill_from_path`]
	/// now use this symlink-only path (Locked Decision 1); `--universal` is a
	/// deprecated no-op.
	///
	/// If the canonical `<canonical_dir>/<safe_name>` already exists on disk
	/// (because another agent installed the same skill, or an earlier
	/// `--universal` call did), the existing master is **left intact** — this
	/// mirrors the API path's `wrote_master = !canonical.exists()` rule and
	/// avoids silently clobbering edits to the canonical. The per-agent
	/// symlink is still created (idempotently).
	pub fn add_skill_universal(&mut self, skill: Skill) -> Result<()> {
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

		let config = self.config_mut()?;
		if config.skills.iter().any(|s| s.name == skill.name) {
			// Classify the already-installed state before deciding to error.
			let safe = sanitize_name(&skill.name);
			let canonical = canonical_dir.join(&safe);
			// (a) NativeReader: the agent reads the Master directly (its write-dir
			//     IS the Master, or its read paths include it), so it already sees
			//     the skill — re-add is an idempotent no-op.
			if matches!(
				link_need,
				crate::skills::linker::LinkNeed::NativeReader
			) {
				return Ok(());
			}
			// (b) Correct link already exists at the agent slot (AlreadyLinked).
			if let Some(ref agent_dir) = agent_write_dir {
				let slot = agent_dir.join(&safe);
				if Linker::is_link(&slot) {
					let master_real = std::fs::canonicalize(&canonical)
						.unwrap_or_else(|_| canonical.clone());
					if std::fs::canonicalize(&slot)
						.map(|r| r == master_real)
						.unwrap_or(false)
					{
						return Ok(());
					}
				}
			}
			// (c) Real foreign occupant or not yet linked: keep strict error.
			return Err(ConfigError::resource_exists("skill", &skill.name));
		}
		info!(
			"adding skill '{}' (universal layout) for agent '{}'",
			skill.name, agent_name
		);

		let safe_name = sanitize_name(&skill.name);
		let canonical = canonical_dir.join(&safe_name);
		// This path has a `Skill` struct, not a source tree, so the from-struct
		// SKILL.md is serialized here (intrinsic to this entry point). A
		// pre-existing master is reused without overwriting.
		if canonical.exists() {
			warn!(
				"canonical '{}' already exists; reusing without overwriting \
				 SKILL.md (use `aghub update` to refresh content)",
				canonical.display()
			);
		} else {
			std::fs::create_dir_all(&canonical)?;
			std::fs::write(
				canonical.join("SKILL.md"),
				format_skill(&skill, None),
			)?;
		}

		// Classify + link via the ONE shared materializer. The Master already
		// exists (written above), so the materializer's copy branch is skipped
		// and only the unified classify-then-link logic runs — the SAME code the
		// fetched/desktop path uses, so the two can no longer diverge.
		let target_link = if use_relative {
			crate::skills::linker::LinkTarget::Relative
		} else {
			crate::skills::linker::LinkTarget::Absolute
		};
		let (results, _wrote_master) =
			crate::skills::install_fetched::materialize_universal_master(
				&canonical,
				&safe_name,
				scope,
				project_root.as_deref(),
				std::slice::from_ref(&agent_type),
				target_link,
			)?;
		Self::ensure_single_agent_installed(&results, &link_need, &skill.name)?;

		let canonical_md =
			canonical.join("SKILL.md").to_string_lossy().to_string();
		let mut fs_skill = skill.clone();
		fs_skill.source_path = Some(canonical_md.clone());
		fs_skill.canonical_path = Some(canonical_md);
		config.skills.push(fs_skill);

		self.save_current()
	}

	pub fn get_skill(&self, name: &str) -> Option<&Skill> {
		self.config.as_ref()?.skills.iter().find(|s| s.name == name)
	}

	pub fn update_skill(&mut self, name: &str, skill: Skill) -> Result<()> {
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

			let content = format_skill(&skill, existing_body.as_deref());
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

		self.save_current()
	}

	pub fn remove_skill(&mut self, name: &str) -> Result<()> {
		let target_dir = self.target_skills_dir();
		let agent_name = self.adapter.name().to_string();
		// Allow-listed roots for the containment guard, computed before the
		// mutable borrow below.
		let roots = {
			let all_agent_dirs =
				crate::skills::removal::agent_skill_dirs_in_scope(
					self.scope,
					self.project_root.as_deref(),
				);
			crate::skills::removal::allowed_skill_roots(
				&all_agent_dirs,
				self.project_root.as_deref(),
			)
		};
		let config = self.config.as_ref().ok_or_else(|| {
			ConfigError::InvalidConfig("No configuration loaded".to_string())
		})?;
		let index = config
			.skills
			.iter()
			.position(|s| s.name == name)
			.ok_or_else(|| ConfigError::resource_not_found("skill", name))?;
		let existing_skill = config.skills[index].clone();

		let config = self.config_mut()?;
		info!("removing skill '{}' for agent '{}'", name, agent_name);
		let safe_name = sanitize_name(name);
		let file_path = if let Some(sp) = &existing_skill.source_path {
			Some(resolve_source_path(sp))
		} else {
			target_dir
				.as_ref()
				.map(|dir| dir.join(&safe_name).join("SKILL.md"))
		};
		let is_link = existing_skill.canonical_path.is_some();

		if let Some(path) = file_path {
			if path.exists() {
				remove_skill_path(
					&path,
					&safe_name,
					is_link,
					target_dir.as_deref(),
					&roots,
				)?;
			}
		}

		config.skills.remove(index);
		self.save_current()
	}

	/// Layout-aware skill removal with a default dry-run.
	///
	/// Builds a [`RemovalPlan`](crate::skills::removal::RemovalPlan) (symlink
	/// sweep + containment + canonical-keep checks), then deletes ONLY when it is
	/// not a dry-run AND either the plan is non-destructive or `confirm` is set.
	/// Deletion re-checks each path's type and containment at delete time (TOCTOU)
	/// and tolerates already-removed paths. On execution the per-scope skill lock
	/// IS pruned here and the result is reported in [`RemovalOutcome::prune`]
	/// (`NotRun` on a dry-run/unconfirmed op, `Pruned`/`Failed` on execute). A
	/// prune failure is non-fatal — the deletion already happened — but it does
	/// NOT always leave the lock untouched: a single-scope (`GlobalOnly` /
	/// `ProjectOnly`) failure leaves that one lock unchanged, whereas a `Both`
	/// prune reconciles two independent locks in sequence, so a project failure
	/// after the global lock was already pruned records that partial mutation in
	/// `Failed.pruned`.
	pub fn remove_skill_planned(
		&mut self,
		name: &str,
		all_agents: bool,
		dry_run: bool,
		confirm: bool,
	) -> Result<crate::skills::removal::RemovalOutcome> {
		use crate::skills::removal;

		let skill = self.skill_for_planned_removal(name, all_agents)?;

		let own_agent_dir = self.target_skills_dir();
		let scope = self.scope;
		let project_root = self.project_root.clone();
		let all_agent_dirs =
			removal::agent_skill_dirs_in_scope(scope, project_root.as_deref());
		let roots = removal::allowed_skill_roots(
			&all_agent_dirs,
			project_root.as_deref(),
		);
		let mut plan = removal::plan_removal(
			&skill,
			own_agent_dir.as_deref(),
			&all_agent_dirs,
			project_root.as_deref(),
			all_agents,
		);

		let executed = !dry_run && (!plan.needs_confirm || confirm);
		if !executed {
			return Ok(removal::RemovalOutcome {
				plan,
				executed: false,
				prune: removal::PruneStatus::NotRun,
			});
		}

		info!(
			"removing skill '{}' (layout={:?}, all_agents={})",
			name, plan.layout, all_agents
		);
		let report =
			removal::execute_removal(&plan, &roots).map_err(ConfigError::Io)?;
		for p in &report.skipped {
			warn!(
				"skipped removal of '{}' (outside skills roots)",
				p.display()
			);
		}
		for (p, error) in &report.failed {
			warn!("failed removal of '{}': {}", p.display(), error);
		}
		// Reflect what actually happened on disk in the returned plan.
		plan.paths = report.removed;
		plan.skipped.extend(report.skipped);
		plan.skipped
			.extend(report.failed.into_iter().map(|(path, _)| path));

		// Skills are disk-derived; drop the in-memory view (save_current persists
		// MCPs, not skills, so this is a best-effort cache update).
		let cfg = self.config_mut()?;
		if let Some(idx) = cfg.skills.iter().position(|s| s.name == name) {
			cfg.skills.remove(idx);
		}

		// Reconcile the per-scope lock against disk now the skill is gone,
		// through the single core-owned seam (also used by the API by-path copy
		// branch). It handles GlobalOnly/ProjectOnly/Both with the same lazy
		// semantics and is non-fatal on error.
		let prune = crate::skills::prune::prune_lock_for_scope(
			self.scope,
			self.project_root.as_deref(),
		);

		Ok(removal::RemovalOutcome {
			plan,
			executed: true,
			prune,
		})
	}

	fn skill_for_planned_removal(
		&self,
		name: &str,
		all_agents: bool,
	) -> Result<Skill> {
		let config = self.config.as_ref().ok_or_else(|| {
			ConfigError::InvalidConfig("No configuration loaded".to_string())
		})?;
		if let Some(skill) = config.skills.iter().find(|s| s.name == name) {
			return Ok(skill.clone());
		}

		if all_agents {
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
		}

		Err(ConfigError::resource_not_found("skill", name))
	}

	fn set_skill_enabled(&mut self, name: &str, enabled: bool) -> Result<()> {
		let agent_name = self.adapter.name().to_string();
		let config = self.config_mut()?;
		let skill = config
			.skills
			.iter_mut()
			.find(|s| s.name == name)
			.ok_or_else(|| ConfigError::resource_not_found("skill", name))?;
		info!(
			"setting skill '{}' enabled={} for agent '{}'",
			name, enabled, agent_name
		);
		skill.enabled = enabled;
		self.save_current()
	}

	pub fn disable_skill(&mut self, name: &str) -> Result<()> {
		self.set_skill_enabled(name, false)
	}

	pub fn enable_skill(&mut self, name: &str) -> Result<()> {
		self.set_skill_enabled(name, true)
	}

	pub fn add_skill_from_path(&mut self, path: &Path) -> Result<Skill> {
		debug!(
			"adding skill from path '{}' for agent '{}'",
			path.display(),
			self.adapter.name()
		);
		// Symlink-only model (Locked Decision 1): every install-from-path writes
		// a single .agents/skills/<name> Master and links THIS agent to it. The
		// old isolated-copy body is removed; there is no copy install path.
		self.add_skill_from_path_universal(path)
	}

	/// Symlink-only install from a local path (Locked Decision 1): parses the
	/// skill then writes the real source tree once into
	/// `.agents/skills/<name>` (canonical) and symlinks THIS agent's skills
	/// dir to it. Both [`Self::add_skill_from_path`] and [`Self::add_skill`]
	/// now delegate here; `--universal` is a deprecated no-op. The full
	/// source tree (`assets/`, `scripts/`, `examples/`, etc.) is preserved
	/// — matching the API path's `install_git_skill_universal` behaviour.
	pub fn add_skill_from_path_universal(
		&mut self,
		path: &Path,
	) -> Result<Skill> {
		debug!(
			"adding skill (universal) from path '{}' for agent '{}'",
			path.display(),
			self.adapter.name()
		);
		let skill_pkg = skill::parser::parse(path).map_err(|e| {
			ConfigError::InvalidConfig(format!("Failed to parse skill: {e}"))
		})?;
		let skill = convert_skill(skill_pkg);

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

		let config = self.config_mut()?;
		if config.skills.iter().any(|s| s.name == skill.name) {
			let safe = sanitize_name(&skill.name);
			let canonical = canonical_dir.join(&safe);
			// NativeReader: reads the Master directly → re-add is a no-op.
			if matches!(
				link_need,
				crate::skills::linker::LinkNeed::NativeReader
			) {
				return Ok(skill.clone());
			}
			if let Some(ref agent_dir) = agent_write_dir {
				let slot = agent_dir.join(&safe);
				if Linker::is_link(&slot) {
					let master_real = std::fs::canonicalize(&canonical)
						.unwrap_or_else(|_| canonical.clone());
					if std::fs::canonicalize(&slot)
						.map(|r| r == master_real)
						.unwrap_or(false)
					{
						return Ok(skill.clone());
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

		// ONE materializer: the same `materialize_universal_master` the
		// fetched/desktop path uses. It copies source -> canonical only when
		// canonical is absent (a pre-existing master is preserved) and links
		// only a NeedsLink agent (a NativeReader reads the Master directly).
		let source_root = crate::skills::skill_source_root(path);
		let target_link = if use_relative {
			crate::skills::linker::LinkTarget::Relative
		} else {
			crate::skills::linker::LinkTarget::Absolute
		};
		let (results, _wrote_master) =
			crate::skills::install_fetched::materialize_universal_master(
				&source_root,
				&safe_name,
				scope,
				project_root.as_deref(),
				std::slice::from_ref(&agent_type),
				target_link,
			)?;
		Self::ensure_single_agent_installed(&results, &link_need, &skill.name)?;

		let canonical_md =
			canonical.join("SKILL.md").to_string_lossy().to_string();
		let mut fs_skill = skill.clone();
		fs_skill.source_path = Some(canonical_md.clone());
		fs_skill.canonical_path = Some(canonical_md);
		config.skills.push(fs_skill);

		self.save_current()?;
		Ok(skill)
	}

	/// Map the single-agent result from `materialize_universal_master` onto the
	/// CLI add path's historical error contract.
	///
	/// The add path linked ONLY a `NeedsLink` agent and errored when that link
	/// hit a real foreign occupant (the old `report.conflicts` check) or a hard
	/// link failure. A `NativeReader` reads the Master directly and an
	/// `Unsupported` agent had no writable dir — neither was ever an error
	/// (the skill is still recorded against the Master). So enforce the
	/// error-free result ONLY for a `NeedsLink` agent, exactly as before.
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
		let canonical_dir = crate::skills::linker::universal_canonical_dir(
			project_root_for_canonical.as_deref(),
		)
		.ok_or_else(|| {
			ConfigError::InvalidConfig(
				"Cannot resolve .agents canonical skills directory".into(),
			)
		})?;
		// Classify THIS agent against the Master with the same scope/root that
		// derived `canonical_dir`, mirroring the fetched/desktop install path. A
		// read-only-master NativeReader (e.g. OpenCode, which writes
		// `.opencode/skills` but reads `.agents/skills`) is detected here — the
		// narrow `agent_write_dir == canonical_dir` test used to miss it.
		let descriptor = crate::registry::get(self.agent_type());
		let link_need = crate::skills::linker::agent_link_need(
			descriptor,
			self.write_scope,
			self.project_root.as_deref(),
			&canonical_dir,
		);
		Ok(UniversalPrep {
			agent_name: self.adapter.name().to_string(),
			agent_write_dir: self.target_skills_dir(),
			use_relative: project_root_for_canonical.is_some(),
			canonical_dir,
			link_need,
		})
	}

	/// Whether THIS manager's agent reads the `.agents/skills` Master directly at
	/// its scope (a NativeReader) and so receives the Master only, with no
	/// per-agent link. Used by the CLI to report "already covered" after an add.
	pub fn skill_target_is_native_reader(&self) -> bool {
		self.universal_install_prep()
			.map(|prep| {
				matches!(
					prep.link_need,
					crate::skills::linker::LinkNeed::NativeReader
				)
			})
			.unwrap_or(false)
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
	crate::skills::linker::link_agents_to_canonical(
		new_canonical,
		referrers,
		if use_relative {
			crate::skills::linker::LinkTarget::Relative
		} else {
			crate::skills::linker::LinkTarget::Absolute
		},
	)
	.map_err(|e| ConfigError::Io(std::io::Error::other(e.to_string())))?;
	Ok(())
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
#[allow(clippy::too_many_arguments)]
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
		crate::skills::linker::link_agents_to_canonical(
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

/// Serialize frontmatter fields as structured YAML via serde_yaml
fn serialize_frontmatter(skill: &Skill) -> String {
	let mut map = BTreeMap::new();
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
fn format_skill(skill: &Skill, existing_body: Option<&str>) -> String {
	let yaml = serialize_frontmatter(skill);
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
mod tests {
	use super::*;

	#[cfg(unix)]
	#[test]
	fn add_skill_universal_writes_master_and_symlinks_agent() {
		use crate::create_adapter;
		use crate::models::AgentType;

		let tmp = tempfile::tempdir().unwrap();
		let root = tmp.path();
		let mut mgr = ConfigManager::new(
			create_adapter(AgentType::Claude),
			false,
			Some(root),
		);
		mgr.load().unwrap();

		let mut skill = Skill::new("uni-skill");
		skill.description = Some("universal test".to_string());
		mgr.add_skill_universal(skill).unwrap();

		// Real master lives under .agents/skills (NOT duplicated per agent).
		assert!(root.join(".agents/skills/uni-skill/SKILL.md").exists());
		// Claude's own dir holds a symlink that resolves to the master.
		let link = root.join(".claude/skills/uni-skill");
		assert!(std::fs::symlink_metadata(&link)
			.unwrap()
			.file_type()
			.is_symlink());
		assert!(link.join("SKILL.md").exists());
	}

	#[cfg(unix)]
	#[test]
	fn add_skill_universal_idempotent_readd_is_noop() {
		use crate::create_adapter;
		use crate::models::AgentType;
		use crate::skills::linker::Linker;

		let tmp = tempfile::tempdir().unwrap();
		let root = tmp.path();
		let mut mgr = ConfigManager::new(
			create_adapter(AgentType::Claude),
			false,
			Some(root),
		);
		mgr.load().unwrap();

		let mut skill = Skill::new("idem-skill");
		skill.description = Some("idempotent test".to_string());

		// First install must succeed.
		mgr.add_skill_universal(skill.clone()).unwrap();

		let master = root.join(".agents/skills/idem-skill");
		let link = root.join(".claude/skills/idem-skill");
		assert!(
			master.join("SKILL.md").exists(),
			"master must exist after first add"
		);
		assert!(Linker::is_link(&link), "link must exist after first add");

		// Second install — same skill name — must be a no-op Ok(()), not error.
		mgr.add_skill_universal(skill).unwrap();

		// Master and link must still be intact.
		assert!(
			master.join("SKILL.md").exists(),
			"master must survive re-add"
		);
		assert!(Linker::is_link(&link), "link must survive re-add");
	}

	#[cfg(unix)]
	#[test]
	fn add_skill_universal_real_conflict_still_errors() {
		use crate::create_adapter;
		use crate::models::AgentType;

		let tmp = tempfile::tempdir().unwrap();
		let root = tmp.path();
		let mut mgr = ConfigManager::new(
			create_adapter(AgentType::Claude),
			false,
			Some(root),
		);
		mgr.load().unwrap();

		let mut skill = Skill::new("conflict-skill");
		skill.description = Some("conflict test".to_string());

		// Pre-place a REAL directory (not a link) at the agent's slot.
		let slot = root.join(".claude/skills/conflict-skill");
		std::fs::create_dir_all(&slot).unwrap();
		std::fs::write(slot.join("SKILL.md"), "foreign").unwrap();

		// add must error — real dir occupies the slot.
		let res = mgr.add_skill_universal(skill);
		assert!(
			res.is_err(),
			"must error when a real foreign dir occupies the agent slot"
		);
		// Foreign content must survive (no-clobber).
		assert_eq!(
			std::fs::read_to_string(slot.join("SKILL.md")).unwrap(),
			"foreign"
		);
	}

	#[cfg(unix)]
	#[test]
	fn add_skill_writes_master_and_symlinks_agent() {
		use crate::create_adapter;
		use crate::models::AgentType;

		let tmp = tempfile::tempdir().unwrap();
		let root = tmp.path();
		let mut mgr = ConfigManager::new(
			create_adapter(AgentType::Claude),
			false,
			Some(root),
		);
		mgr.load().unwrap();

		let mut skill = Skill::new("manual-skill");
		skill.description = Some("manual test".to_string());
		mgr.add_skill(skill).unwrap();

		assert!(root.join(".agents/skills/manual-skill/SKILL.md").exists());
		let link = root.join(".claude/skills/manual-skill");
		assert!(std::fs::symlink_metadata(&link)
			.unwrap()
			.file_type()
			.is_symlink());
		assert!(link.join("SKILL.md").exists());

		let saved = mgr.get_skill("manual-skill").unwrap();
		assert!(saved.canonical_path.is_some());
	}

	// no-copy regression: add_skill_from_path writes a .agents Master and a
	// link in the agent dir, never a private copy (Locked Decision 1).
	#[cfg(unix)]
	#[test]
	fn add_skill_from_path_links_master_not_copy() {
		use crate::create_adapter;
		use crate::models::AgentType;
		use crate::skills::linker::Linker;

		let tmp = tempfile::tempdir().unwrap();
		let root = tmp.path();
		let mut mgr = ConfigManager::new(
			create_adapter(AgentType::Claude),
			false,
			Some(root),
		);
		mgr.load().unwrap();

		// Create a source skill directory with SKILL.md
		let src = tmp.path().join("src/my-skill");
		std::fs::create_dir_all(&src).unwrap();
		std::fs::write(
			src.join("SKILL.md"),
			"---\nname: my-skill\ndescription: d\n---\nbody",
		)
		.unwrap();

		mgr.add_skill_from_path(&src.join("SKILL.md")).unwrap();

		let canonical = root.join(".agents/skills/my-skill");
		let link = root.join(".claude/skills/my-skill");
		assert!(canonical.join("SKILL.md").exists(), "Master materialized");
		assert!(
			Linker::is_link(&link),
			"agent dir must hold a link to the Master, not a copy"
		);
	}

	// GAP-2 no-copy regression: add_skill (manual-create) writes a .agents
	// Master and a link in the agent dir, never a private copy, and records
	// canonical_path (link provenance) -- proving the Task 25 add_skill ->
	// add_skill_universal delegation (Locked Decision 1).
	#[cfg(unix)]
	#[test]
	fn add_skill_manual_create_links_master_not_copy() {
		use crate::create_adapter;
		use crate::models::AgentType;
		use crate::skills::linker::Linker;

		let tmp = tempfile::tempdir().unwrap();
		let root = tmp.path();
		let mut mgr = ConfigManager::new(
			create_adapter(AgentType::Claude),
			false,
			Some(root),
		);
		mgr.load().unwrap();

		let mut skill = Skill::new("manual-skill");
		skill.description = Some("manual create test".to_string());
		mgr.add_skill(skill).unwrap();

		let canonical = root.join(".agents/skills/manual-skill");
		let link = root.join(".claude/skills/manual-skill");
		assert!(
			canonical.join("SKILL.md").exists(),
			"manual-create must materialize a .agents Master"
		);
		assert!(
			Linker::is_link(&link),
			"manual-create must link the agent dir to the Master, not copy"
		);
		// Link provenance, not copy provenance.
		let recorded = mgr.get_skill("manual-skill").unwrap();
		assert!(
			recorded.canonical_path.is_some(),
			"manual-create must record canonical_path (link provenance)"
		);
	}

	// T3: a project NativeReader (OpenCode reads `.agents/skills` directly) must
	// receive the Master ONLY — no redundant per-agent symlink in its own dir —
	// matching the fetched/desktop install path's classify-based behaviour. The
	// old `agent_write_dir == canonical_dir` check missed read-only-master
	// NativeReaders (OpenCode writes `.opencode/skills` but reads `.agents`).
	#[cfg(unix)]
	#[test]
	fn add_skill_native_reader_writes_master_without_link() {
		use crate::create_adapter;
		use crate::models::AgentType;

		let tmp = tempfile::tempdir().unwrap();
		let root = tmp.path();
		let mut mgr = ConfigManager::new(
			create_adapter(AgentType::OpenCode),
			false,
			Some(root),
		);
		mgr.load().unwrap();

		let mut skill = Skill::new("native-skill");
		skill.description = Some("native reader test".to_string());
		mgr.add_skill(skill).unwrap();

		// Master materialized once under `.agents/skills`.
		assert!(root.join(".agents/skills/native-skill/SKILL.md").exists());
		// OpenCode reads the Master directly → NO link in its own skills dir.
		assert!(
			!root.join(".opencode/skills/native-skill").exists(),
			"NativeReader must not get a redundant per-agent link"
		);
		// Still recorded with universal provenance.
		assert!(mgr
			.get_skill("native-skill")
			.unwrap()
			.canonical_path
			.is_some());
		// And the classifier agrees this agent needs no link.
		assert!(mgr.skill_target_is_native_reader());
	}

	#[test]
	fn remove_skill_path_refuses_dir_outside_allowed_roots() {
		// Defense-in-depth: the legacy copy-removal helper must never
		// `remove_dir_all` a directory that escapes the allow-listed skill roots,
		// even if a crafted `source_path` points outside them.
		let tmp = tempfile::tempdir().unwrap();
		let outside = tmp.path().join("outside/foo");
		std::fs::create_dir_all(&outside).unwrap();
		std::fs::write(outside.join("SKILL.md"), "x").unwrap();
		let allowed = tmp.path().join("allowed");
		std::fs::create_dir_all(&allowed).unwrap();
		let roots = vec![allowed];

		let res = remove_skill_path(
			&outside.join("SKILL.md"),
			"foo",
			false,
			None,
			&roots,
		);

		assert!(res.is_err(), "must refuse to remove a dir outside roots");
		assert!(outside.exists(), "out-of-root dir must survive");
	}

	#[test]
	fn remove_skill_path_removes_contained_dir() {
		let tmp = tempfile::tempdir().unwrap();
		let skills = tmp.path().join("skills");
		let foo = skills.join("foo");
		std::fs::create_dir_all(&foo).unwrap();
		std::fs::write(foo.join("SKILL.md"), "x").unwrap();
		let roots = vec![skills.clone()];

		remove_skill_path(&foo.join("SKILL.md"), "foo", false, None, &roots)
			.unwrap();

		assert!(!foo.exists(), "a contained skill dir is removed normally");
	}

	// T-REMOVE-SKILL-PATH-JUNCTION: a junction referrer is unlinked on remove,
	// and the shared Master directory + its files survive (remove_dir, not
	// remove_dir_all). Runs on windows-latest (junctions need no admin).
	#[cfg(windows)]
	#[test]
	fn remove_skill_path_unlinks_junction_keeps_master() {
		use crate::skills::linker::create_junction;
		let tmp = tempfile::tempdir().unwrap();
		let master = tmp.path().join(".agents/skills/foo");
		std::fs::create_dir_all(&master).unwrap();
		std::fs::write(master.join("SKILL.md"), "---\nname: foo\n---\n")
			.unwrap();
		let claude = tmp.path().join(".claude/skills");
		std::fs::create_dir_all(&claude).unwrap();
		let link = claude.join("foo");
		let abs_master = master.canonicalize().unwrap();
		create_junction(&abs_master, &link).unwrap();

		let roots = vec![tmp.path().to_path_buf()];
		remove_skill_path(
			&master.join("SKILL.md"),
			"foo",
			true, // is_link
			Some(claude.as_path()),
			&roots,
		)
		.unwrap();

		assert!(
			std::fs::symlink_metadata(&link).is_err(),
			"junction must be unlinked"
		);
		assert!(
			master.join("SKILL.md").exists(),
			"shared Master must survive (remove_dir, not remove_dir_all)"
		);
	}

	#[test]
	fn test_format_skill_preserves_body() {
		let mut skill = Skill::new("test-skill");
		skill.description = Some("A test".to_string());
		let body = "\n# Original Title\n\nInstruction content.\n";
		let output = format_skill(&skill, Some(body));
		assert!(output.contains("# Original Title"));
		assert!(output.contains("Instruction content."));
		// Frontmatter should be valid YAML
		assert!(output.starts_with("---\n"));
		assert!(output.contains("---\n\n# Original Title"));
	}

	#[test]
	fn test_format_skill_generates_placeholder_without_body() {
		let skill = Skill::new("test-skill");
		let output = format_skill(&skill, None);
		assert!(output.contains("# test-skill"));
	}

	#[test]
	fn test_format_skill_stays_parseable_by_skill_crate() {
		let skill = Skill::new("test-skill");
		let output = format_skill(&skill, None);
		let parsed = skill::parser::parse_skill_md(&output).unwrap();
		assert_eq!(parsed.name, "test-skill");
		assert_eq!(parsed.description, "");
	}

	#[test]
	fn test_format_skill_quotes_colon_in_description() {
		let mut skill = Skill::new("test");
		skill.description = Some("Source: https://example.com".to_string());
		let output = format_skill(&skill, None);
		// serde_yaml should quote the value containing ':'
		let reparsed: BTreeMap<String, String> = serde_yaml::from_str(
			output
				.trim_start_matches("---\n")
				.split("---\n")
				.next()
				.unwrap(),
		)
		.expect("Should produce valid YAML");
		assert_eq!(reparsed["description"], "Source: https://example.com");
	}

	#[test]
	fn test_format_skill_quotes_numeric_values() {
		let mut skill = Skill::new("test");
		skill.version = Some("123".to_string());
		skill.author = Some("true".to_string());
		let output = format_skill(&skill, None);
		let reparsed: BTreeMap<String, String> = serde_yaml::from_str(
			output
				.trim_start_matches("---\n")
				.split("---\n")
				.next()
				.unwrap(),
		)
		.expect("Should produce valid YAML");
		assert_eq!(reparsed["version"], "123");
		assert_eq!(reparsed["author"], "true");
	}

	// -----------------------------------------------------------------------
	// P0-K fix: remove_skill for universal mode was a no-op
	// -----------------------------------------------------------------------

	#[cfg(unix)]
	#[test]
	fn remove_skill_unlinks_agent_symlink_but_preserves_canonical() {
		use crate::create_adapter;
		use crate::models::AgentType;

		let tmp = tempfile::tempdir().unwrap();
		let root = tmp.path();
		let mut mgr = ConfigManager::new(
			create_adapter(AgentType::Claude),
			false,
			Some(root),
		);
		mgr.load().unwrap();

		// Install a universal skill
		let mut skill = Skill::new("rm-test");
		skill.description = Some("test".to_string());
		mgr.add_skill_universal(skill).unwrap();

		let canonical = root.join(".agents/skills/rm-test/SKILL.md");
		let link = root.join(".claude/skills/rm-test");
		assert!(canonical.exists());
		assert!(std::fs::symlink_metadata(&link)
			.unwrap()
			.file_type()
			.is_symlink());

		// Remove the skill
		mgr.remove_skill("rm-test").unwrap();

		// Agent symlink should be gone
		assert!(!link.exists());
		// Canonical should still be there (single-agent removal keeps it)
		assert!(canonical.exists());
		// Config entry should be removed
		assert!(mgr.config.as_ref().unwrap().skills.is_empty());
	}

	#[cfg(unix)]
	#[test]
	fn remove_skill_universal_idempotent_when_symlink_already_gone() {
		use crate::create_adapter;
		use crate::models::AgentType;

		let tmp = tempfile::tempdir().unwrap();
		let root = tmp.path();
		let mut mgr = ConfigManager::new(
			create_adapter(AgentType::Claude),
			false,
			Some(root),
		);
		mgr.load().unwrap();

		let mut skill = Skill::new("rm-idem");
		skill.description = Some("test".to_string());
		mgr.add_skill_universal(skill).unwrap();

		// Manually remove the symlink before calling remove_skill
		let link = root.join(".claude/skills/rm-idem");
		assert!(link.exists());
		std::fs::remove_file(&link).unwrap();
		assert!(!link.exists());

		// Should not error even though the symlink is already gone
		mgr.remove_skill("rm-idem").unwrap();
	}

	#[cfg(unix)]
	#[test]
	fn remove_skill_preserves_canonical_for_multi_agent_ref() {
		use crate::create_adapter;
		use crate::models::AgentType;

		let tmp = tempfile::tempdir().unwrap();
		let root = tmp.path();

		// Claude installs first
		let mut mgr = ConfigManager::new(
			create_adapter(AgentType::Claude),
			false,
			Some(root),
		);
		mgr.load().unwrap();
		let mut skill = Skill::new("multi-ref");
		skill.description = Some("test".to_string());
		mgr.add_skill_universal(skill).unwrap();

		// Cursor discovers the skill from .agents/skills/ on load (Cursor
		// scans that directory). No need to install again — the canonical is
		// shared and Cursor reads it directly.
		let mut mgr2 = ConfigManager::new(
			create_adapter(AgentType::Cursor),
			false,
			Some(root),
		);
		mgr2.load().unwrap();
		assert!(
			mgr2.config
				.as_ref()
				.unwrap()
				.skills
				.iter()
				.any(|s| s.name == "multi-ref"),
			"Cursor should discover multi-ref from .agents/skills/ on load"
		);

		let canonical = root.join(".agents/skills/multi-ref/SKILL.md");
		assert!(canonical.exists());

		// Remove from Claude only
		mgr.remove_skill("multi-ref").unwrap();

		// Claude symlink gone, canonical preserved
		assert!(!root.join(".claude/skills/multi-ref").exists());
		assert!(canonical.exists());

		// Cursor can still discover the skill (canonical is intact)
		let mut mgr3 = ConfigManager::new(
			create_adapter(AgentType::Cursor),
			false,
			Some(root),
		);
		mgr3.load().unwrap();
		assert!(
			mgr3.config
				.as_ref()
				.unwrap()
				.skills
				.iter()
				.any(|s| s.name == "multi-ref"),
			"Cursor should still find multi-ref after Claude's removal"
		);
	}

	// -----------------------------------------------------------------------
	// P1 fix: renaming a universal skill must re-point the per-agent symlinks
	// and preserve the symlink layout (canonical_path), not dangle + downgrade.
	// -----------------------------------------------------------------------

	#[cfg(unix)]
	#[test]
	fn update_skill_universal_rename_relinks_agents_and_keeps_canonical() {
		use crate::create_adapter;
		use crate::models::AgentType;

		let tmp = tempfile::tempdir().unwrap();
		let root = tmp.path();
		let mut mgr = ConfigManager::new(
			create_adapter(AgentType::Claude),
			false,
			Some(root),
		);
		mgr.load().unwrap();

		let mut skill = Skill::new("old-uni");
		skill.description = Some("universal".to_string());
		mgr.add_skill_universal(skill).unwrap();

		// Rename old-uni -> new-uni via the update path.
		let mut renamed = Skill::new("new-uni");
		renamed.description = Some("universal".to_string());
		mgr.update_skill("old-uni", renamed).unwrap();

		// Canonical master is renamed (old gone, new present).
		assert!(root.join(".agents/skills/new-uni/SKILL.md").exists());
		assert!(!root.join(".agents/skills/old-uni").exists());

		// The old-name agent symlink is fully removed (not left dangling).
		assert!(
			std::fs::symlink_metadata(root.join(".claude/skills/old-uni"))
				.is_err(),
			"old-name symlink must be removed, not left dangling"
		);

		// A new-name agent symlink exists and resolves to the renamed master.
		let new_link = root.join(".claude/skills/new-uni");
		let meta = std::fs::symlink_metadata(&new_link)
			.expect("new-name agent symlink must exist");
		assert!(
			meta.file_type().is_symlink(),
			"new agent path must be a symlink"
		);
		assert!(
			new_link.join("SKILL.md").exists(),
			"symlink must resolve through to the renamed master"
		);

		// The layout stays "symlink": canonical_path is preserved (not None),
		// so later layout-aware removal still classifies it correctly.
		let s = mgr.get_skill("new-uni").expect("renamed skill in config");
		assert!(
			s.canonical_path.is_some(),
			"canonical_path must be preserved on a universal rename"
		);
	}

	#[cfg(unix)]
	#[test]
	fn update_skill_rename_refuses_when_target_dir_already_exists() {
		use crate::create_adapter;
		use crate::models::AgentType;

		let tmp = tempfile::tempdir().unwrap();
		let root = tmp.path();
		let mut mgr = ConfigManager::new(
			create_adapter(AgentType::Claude),
			false,
			Some(root),
		);
		mgr.load().unwrap();

		let mut skill = Skill::new("collide-old");
		skill.description = Some("u".to_string());
		mgr.add_skill_universal(skill).unwrap();

		// A DIFFERENT (e.g. another skill's) master already occupies the target
		// name. `fs::rename` onto an empty target dir would silently succeed and
		// clobber it; the rename must refuse instead.
		std::fs::create_dir_all(root.join(".agents/skills/collide-new"))
			.unwrap();

		let res = mgr.update_skill("collide-old", Skill::new("collide-new"));

		assert!(res.is_err(), "must refuse to rename onto an existing dir");
		assert!(
			root.join(".agents/skills/collide-old/SKILL.md").exists(),
			"the original master must be preserved on conflict"
		);
	}

	/// Whether 0o555 perms actually block writes for this process. Returns false
	/// when running as root (perm bits are bypassed), so permission-injection
	/// tests can skip instead of failing spuriously in root CI.
	#[cfg(unix)]
	fn perms_enforced(under: &std::path::Path) -> bool {
		use std::os::unix::fs::PermissionsExt;
		let probe = under.join(".perm-probe");
		std::fs::create_dir(&probe).unwrap();
		std::fs::set_permissions(
			&probe,
			std::fs::Permissions::from_mode(0o555),
		)
		.unwrap();
		let blocked = std::fs::write(probe.join("x"), b"x").is_err();
		std::fs::set_permissions(
			&probe,
			std::fs::Permissions::from_mode(0o755),
		)
		.unwrap();
		std::fs::remove_dir_all(&probe).ok();
		blocked
	}

	#[cfg(unix)]
	#[test]
	fn update_skill_universal_rename_rolls_back_when_relink_fails() {
		use crate::create_adapter;
		use crate::models::AgentType;
		use std::os::unix::fs::PermissionsExt;

		let tmp = tempfile::tempdir().unwrap();
		let root = tmp.path();
		if !perms_enforced(root) {
			eprintln!("skipping: 0o555 not enforced (running as root)");
			return;
		}

		let mut mgr = ConfigManager::new(
			create_adapter(AgentType::Claude),
			false,
			Some(root),
		);
		mgr.load().unwrap();

		let mut skill = Skill::new("roll-old");
		skill.description = Some("u".to_string());
		mgr.add_skill_universal(skill).unwrap();

		let referrer_dir = root.join(".claude/skills");
		assert!(
			std::fs::symlink_metadata(referrer_dir.join("roll-old"))
				.unwrap()
				.file_type()
				.is_symlink(),
			"precondition: referrer symlink exists"
		);

		// Make the referrer dir read-only so the relink fails AFTER the master
		// has been renamed — the partial-failure window the rollback must close.
		let orig = std::fs::metadata(&referrer_dir).unwrap().permissions();
		std::fs::set_permissions(
			&referrer_dir,
			std::fs::Permissions::from_mode(0o555),
		)
		.unwrap();

		let res = mgr.update_skill("roll-old", Skill::new("roll-new"));

		// Restore perms before asserting so tempdir teardown always works.
		std::fs::set_permissions(&referrer_dir, orig).unwrap();

		let err = res.expect_err("a failed relink must surface as an error");
		// The rollback SUCCEEDS here (the master is renamed back and
		// `link_agents_to_canonical` folds per-link failures into its report
		// rather than erroring), so the original relink failure is returned
		// UNCHANGED — not re-wrapped as a recovery hint. The message must name
		// the stale link it failed on and must NOT claim manual restore.
		let msg = err.to_string();
		assert!(
			msg.contains("roll-old"),
			"the original relink error must name the failing link: {msg}"
		);
		assert!(
			!msg.contains("move them back"),
			"a recovered rollback must not emit ManualRestore wording: {msg}"
		);
		assert!(
			root.join(".agents/skills/roll-old/SKILL.md").exists(),
			"rollback must rename the master back to its old name"
		);
		assert!(
			!root.join(".agents/skills/roll-new").exists(),
			"no half-renamed master may be left behind"
		);
		let link = referrer_dir.join("roll-old");
		assert!(
			std::fs::symlink_metadata(&link)
				.map(|m| m.file_type().is_symlink())
				.unwrap_or(false),
			"the surviving referrer symlink must remain"
		);
		assert!(
			link.join("SKILL.md").exists(),
			"the referrer must still resolve to the master (not dangling)"
		);
	}

	#[cfg(unix)]
	#[test]
	fn update_skill_universal_rename_rollback_restores_removed_referrer() {
		use crate::create_adapter;
		use crate::models::AgentType;
		use std::os::unix::fs::PermissionsExt;

		let tmp = tempfile::tempdir().unwrap();
		let root = tmp.path();
		if !perms_enforced(root) {
			eprintln!("skipping: 0o555 not enforced (running as root)");
			return;
		}

		let mut mgr = ConfigManager::new(
			create_adapter(AgentType::Claude),
			false,
			Some(root),
		);
		mgr.load().unwrap();

		let mut skill = Skill::new("roll2-old");
		skill.description = Some("u".to_string());
		mgr.add_skill_universal(skill).unwrap();

		// Add a SECOND referrer in RooCode's project skills dir (it sorts after
		// Claude in AgentType::ALL, so Claude's symlink is removed first, then
		// RooCode's removal fails — forcing the rollback to RESTORE Claude's
		// already-removed symlink).
		let master = root.join(".agents/skills/roll2-old");
		let roo_dir = root.join(".roo/skills");
		std::fs::create_dir_all(&roo_dir).unwrap();
		std::os::unix::fs::symlink(&master, roo_dir.join("roll2-old")).unwrap();
		assert_eq!(
			std::fs::canonicalize(roo_dir.join("roll2-old")).unwrap(),
			std::fs::canonicalize(&master).unwrap(),
			"precondition: second referrer resolves to the master"
		);

		let claude_dir = root.join(".claude/skills");
		let roo_orig = std::fs::metadata(&roo_dir).unwrap().permissions();
		std::fs::set_permissions(
			&roo_dir,
			std::fs::Permissions::from_mode(0o555),
		)
		.unwrap();

		let res = mgr.update_skill("roll2-old", Skill::new("roll2-new"));

		std::fs::set_permissions(&roo_dir, roo_orig).unwrap();

		assert!(res.is_err(), "a failed relink must surface as an error");
		assert!(
			master.join("SKILL.md").exists(),
			"rollback must rename the master back to its old name"
		);
		// The FIRST referrer (Claude) had its symlink removed before the failure;
		// rollback must have recreated it pointing back at the master.
		let claude_link = claude_dir.join("roll2-old");
		assert!(
			std::fs::symlink_metadata(&claude_link)
				.map(|m| m.file_type().is_symlink())
				.unwrap_or(false),
			"the removed referrer symlink must be restored"
		);
		assert!(
			claude_link.join("SKILL.md").exists(),
			"the restored referrer must resolve to the master"
		);
	}

	// -----------------------------------------------------------------------
	// T2 (#8): structured rollback reason via RecoveryHint.
	// -----------------------------------------------------------------------

	/// Rollback's own master-restore rename fails (its parent is read-only), so
	/// the master is the ONLY surviving copy and stays at `new_master`. The
	/// error must carry RecoveryHint::ManualRestore wording naming BOTH the
	/// recover-from (new_master) and restore-to (old_master) paths plus an
	/// actionable next step. Driven through the real `rollback_master_rename`.
	#[cfg(unix)]
	#[test]
	fn rename_rollback_failure_reports_manual_restore() {
		use std::os::unix::fs::PermissionsExt;

		let tmp = tempfile::tempdir().unwrap();
		let root = tmp.path();
		if !perms_enforced(root) {
			eprintln!("skipping: 0o555 not enforced (running as root)");
			return;
		}

		// The renamed master holds the only copy of the contents.
		let skills_dir = root.join(".agents/skills");
		let old_master = skills_dir.join("recover-old");
		let new_master = skills_dir.join("recover-new");
		std::fs::create_dir_all(&new_master).unwrap();
		std::fs::write(new_master.join("SKILL.md"), "real").unwrap();

		// Read-only parent: the rollback's `rename(new_master, old_master)`
		// cannot create the old-name entry, so the restore step itself fails.
		let orig = std::fs::metadata(&skills_dir).unwrap().permissions();
		std::fs::set_permissions(
			&skills_dir,
			std::fs::Permissions::from_mode(0o555),
		)
		.unwrap();

		let err = rollback_master_rename(
			&new_master,
			&old_master,
			&[],
			"recover-new",
			false,
			ConfigError::Io(std::io::Error::other("relink boom")),
		);

		// Restore perms before asserting so tempdir teardown always works.
		std::fs::set_permissions(&skills_dir, orig).unwrap();

		let msg = err.to_string();
		assert!(
			msg.contains(&new_master.display().to_string()),
			"recover_from (new_master) path missing from: {msg}"
		);
		assert!(
			msg.contains(&old_master.display().to_string()),
			"restore_to (old_master) path missing from: {msg}"
		);
		assert!(
			msg.contains("relink boom"),
			"the original relink failure must still be named: {msg}"
		);
		assert!(
			msg.contains("move them back"),
			"missing ManualRestore next step in: {msg}"
		);
		// The master must still be the renamed copy (rollback could not move it).
		assert!(
			new_master.join("SKILL.md").exists(),
			"the only surviving copy must remain at new_master"
		);
	}

	/// Rollback restores the master successfully, but a leftover new-name
	/// referrer symlink can't be removed (its dir is read-only), so the relink
	/// step of the rollback fails. Data is safe (master is back at old_master);
	/// the error must report RecoveryHint::BrokenSymlink for the offending link,
	/// NOT ManualRestore.
	#[cfg(unix)]
	#[test]
	fn rename_rollback_relink_failure_reports_broken_symlink() {
		use std::os::unix::fs::PermissionsExt;

		let tmp = tempfile::tempdir().unwrap();
		let root = tmp.path();
		if !perms_enforced(root) {
			eprintln!("skipping: 0o555 not enforced (running as root)");
			return;
		}

		let skills_dir = root.join(".agents/skills");
		let old_master = skills_dir.join("brk-old");
		let new_master = skills_dir.join("brk-new");
		std::fs::create_dir_all(&new_master).unwrap();
		std::fs::write(new_master.join("SKILL.md"), "real").unwrap();

		// A referrer dir holding a stale NEW-name symlink the rollback must
		// remove; the dir is read-only so the `unlink` fails AFTER the master
		// is renamed back.
		let referrer = root.join(".claude/skills");
		std::fs::create_dir_all(&referrer).unwrap();
		std::os::unix::fs::symlink(&new_master, referrer.join("brk-new"))
			.unwrap();
		let orig = std::fs::metadata(&referrer).unwrap().permissions();
		std::fs::set_permissions(
			&referrer,
			std::fs::Permissions::from_mode(0o555),
		)
		.unwrap();

		let err = rollback_master_rename(
			&new_master,
			&old_master,
			std::slice::from_ref(&referrer),
			"brk-new",
			false,
			ConfigError::Io(std::io::Error::other("relink boom")),
		);

		// Restore perms before asserting so tempdir teardown always works.
		std::fs::set_permissions(&referrer, orig).unwrap();

		let msg = err.to_string();
		assert!(
			msg.contains("broken link"),
			"missing BrokenSymlink next step in: {msg}"
		);
		assert!(
			msg.contains(&referrer.join("brk-new").display().to_string()),
			"the offending link path must be named: {msg}"
		);
		assert!(
			!msg.contains("move them back"),
			"a restored master must not emit ManualRestore wording: {msg}"
		);
		// The master must be safely back at its old name.
		assert!(
			old_master.join("SKILL.md").exists(),
			"rollback must have restored the master to old_master"
		);
		assert!(
			!new_master.exists(),
			"no half-renamed master may survive a recovered rollback"
		);
	}

	// -----------------------------------------------------------------------
	// P1 fix (Windows): junction referrer rename/relink + rollback
	// -----------------------------------------------------------------------

	#[cfg(windows)]
	#[test]
	fn update_skill_universal_rename_relinks_junction_and_keeps_canonical() {
		use crate::create_adapter;
		use crate::models::AgentType;

		let tmp = tempfile::tempdir().unwrap();
		let root = std::fs::canonicalize(tmp.path()).unwrap();
		let mut mgr = ConfigManager::new(
			create_adapter(AgentType::Claude),
			false,
			Some(&root),
		);
		mgr.load().unwrap();

		// Install universal skill (writes Master + creates a junction
		// referrer in .claude/skills/ via Linker::link which falls through
		// to create_junction when symlink_dir is unavailable).
		let mut skill = Skill::new("old-uni-win");
		skill.description = Some("universal".to_string());
		mgr.add_skill_universal(skill).unwrap();

		// Rename old-uni-win -> new-uni-win via the update path.
		let mut renamed = Skill::new("new-uni-win");
		renamed.description = Some("universal".to_string());
		mgr.update_skill("old-uni-win", renamed).unwrap();

		// Canonical master is renamed (old gone, new present).
		assert!(
			root.join(".agents\\skills\\new-uni-win\\SKILL.md").exists(),
			"renamed master must exist"
		);
		assert!(
			!root.join(".agents\\skills\\old-uni-win").exists(),
			"old master must be gone"
		);

		// The old-name referrer link is fully removed (not left dangling).
		assert!(
			std::fs::symlink_metadata(
				root.join(".claude\\skills\\old-uni-win")
			)
			.is_err(),
			"old-name junction must be removed, not left dangling"
		);

		// A new-name referrer exists and resolves to the renamed master.
		let new_link = root.join(".claude\\skills\\new-uni-win");
		assert!(
			crate::skills::linker::Linker::is_link(&new_link),
			"new-name referrer must be a reparse point"
		);
		assert!(
			new_link.join("SKILL.md").exists(),
			"junction must resolve through to the renamed master"
		);

		// canonical_path is preserved.
		let s = mgr
			.get_skill("new-uni-win")
			.expect("renamed skill in config");
		assert!(
			s.canonical_path.is_some(),
			"canonical_path must be preserved on a universal rename"
		);
	}

	// -----------------------------------------------------------------------
	// P1-B fix: add_skill_universal silently overwrites existing canonical
	// -----------------------------------------------------------------------

	#[cfg(unix)]
	#[test]
	fn add_skill_universal_does_not_overwrite_existing_canonical() {
		use crate::create_adapter;
		use crate::models::AgentType;

		let tmp = tempfile::tempdir().unwrap();
		let root = tmp.path();
		let mut mgr = ConfigManager::new(
			create_adapter(AgentType::Claude),
			false,
			Some(root),
		);
		mgr.load().unwrap();

		// Manually pre-create the canonical with old content
		let canonical = root.join(".agents/skills/preexist");
		std::fs::create_dir_all(&canonical).unwrap();
		std::fs::write(
			canonical.join("SKILL.md"),
			"---\nname: preexist\ndescription: old\n---\nOld content.\n",
		)
		.unwrap();

		// Install a skill with the same sanitized name
		let mut skill = Skill::new("preexist");
		skill.description = Some("new".to_string());
		mgr.add_skill_universal(skill).unwrap();

		// The SKILL.md should NOT have been overwritten
		let content =
			std::fs::read_to_string(canonical.join("SKILL.md")).unwrap();
		assert!(
			content.contains("Old content."),
			"SKILL.md was overwritten: {content}"
		);

		// Symlink should still be created (idempotent)
		let link = root.join(".claude/skills/preexist");
		assert!(std::fs::symlink_metadata(&link)
			.unwrap()
			.file_type()
			.is_symlink());
	}

	#[cfg(unix)]
	#[test]
	fn add_skill_universal_fresh_install_writes_canonical() {
		use crate::create_adapter;
		use crate::models::AgentType;

		let tmp = tempfile::tempdir().unwrap();
		let root = tmp.path();
		let mut mgr = ConfigManager::new(
			create_adapter(AgentType::Claude),
			false,
			Some(root),
		);
		mgr.load().unwrap();

		let mut skill = Skill::new("fresh");
		skill.description = Some("fresh install".to_string());
		mgr.add_skill_universal(skill).unwrap();

		let canonical = root.join(".agents/skills/fresh/SKILL.md");
		assert!(canonical.exists());
		let content = std::fs::read_to_string(&canonical).unwrap();
		assert!(content.contains("fresh install"));

		let link = root.join(".claude/skills/fresh");
		assert!(std::fs::symlink_metadata(&link)
			.unwrap()
			.file_type()
			.is_symlink());
	}

	// -----------------------------------------------------------------------
	// P0-A fix: add_skill_from_path_universal dropped all non-SKILL.md assets
	// -----------------------------------------------------------------------

	#[cfg(unix)]
	#[test]
	fn add_skill_from_path_universal_copies_full_source_tree() {
		use crate::create_adapter;
		use crate::models::AgentType;

		let tmp = tempfile::tempdir().unwrap();
		let root = tmp.path();
		let mut mgr = ConfigManager::new(
			create_adapter(AgentType::Claude),
			false,
			Some(root),
		);
		mgr.load().unwrap();

		// Create a source skill with assets
		let src = root.join("src/my-skill");
		std::fs::create_dir_all(&src).unwrap();
		std::fs::write(
			src.join("SKILL.md"),
			"---\nname: my-skill\ndescription: test\n---\nBody.\n",
		)
		.unwrap();
		std::fs::create_dir_all(src.join("assets")).unwrap();
		std::fs::write(src.join("assets/data.json"), "{}").unwrap();
		std::fs::create_dir_all(src.join("scripts")).unwrap();
		std::fs::write(src.join("scripts/setup.sh"), "#!/bin/sh\necho ok")
			.unwrap();

		let skill = mgr.add_skill_from_path_universal(&src).unwrap();
		assert_eq!(skill.name, "my-skill");

		// Canonical should have the full tree
		let canonical = root.join(".agents/skills/my-skill");
		assert!(canonical.join("SKILL.md").exists());
		assert!(canonical.join("assets/data.json").exists());
		assert!(canonical.join("scripts/setup.sh").exists());
		assert_eq!(
			std::fs::read_to_string(canonical.join("assets/data.json"))
				.unwrap(),
			"{}"
		);

		// Agent dir should be a symlink
		let link = root.join(".claude/skills/my-skill");
		assert!(std::fs::symlink_metadata(&link)
			.unwrap()
			.file_type()
			.is_symlink());

		// Reading assets via the symlink should work
		assert_eq!(
			std::fs::read_to_string(link.join("assets/data.json")).unwrap(),
			"{}"
		);
	}

	#[cfg(unix)]
	#[test]
	fn add_skill_from_path_universal_accepts_skill_md_file() {
		use crate::create_adapter;
		use crate::models::AgentType;

		let tmp = tempfile::tempdir().unwrap();
		let root = tmp.path();
		let mut mgr = ConfigManager::new(
			create_adapter(AgentType::Claude),
			false,
			Some(root),
		);
		mgr.load().unwrap();

		// Pass SKILL.md file directly (should use parent as source root)
		let src = root.join("src/other-skill");
		std::fs::create_dir_all(&src).unwrap();
		std::fs::write(
			src.join("SKILL.md"),
			"---\nname: other-skill\ndescription: test\n---\n",
		)
		.unwrap();
		std::fs::write(src.join("extra.txt"), "bonus").unwrap();

		let skill_md = src.join("SKILL.md");
		let skill = mgr.add_skill_from_path_universal(&skill_md).unwrap();
		assert_eq!(skill.name, "other-skill");

		// extra.txt should have been copied to canonical
		let canonical = root.join(".agents/skills/other-skill");
		assert!(canonical.join("extra.txt").exists());
		assert_eq!(
			std::fs::read_to_string(canonical.join("extra.txt")).unwrap(),
			"bonus"
		);
	}

	#[cfg(unix)]
	#[test]
	fn add_skill_from_path_universal_does_not_overwrite_existing_canonical() {
		use crate::create_adapter;
		use crate::models::AgentType;

		let tmp = tempfile::tempdir().unwrap();
		let root = tmp.path();

		// Pre-create canonical with old content
		let canonical = root.join(".agents/skills/shared-skill");
		std::fs::create_dir_all(&canonical).unwrap();
		std::fs::write(
			canonical.join("SKILL.md"),
			"---\nname: shared-skill\ndescription: old\n---\nOld version.\n",
		)
		.unwrap();

		// Source has updated content
		let src = root.join("src/shared-skill");
		std::fs::create_dir_all(&src).unwrap();
		std::fs::write(
			src.join("SKILL.md"),
			"---\nname: shared-skill\ndescription: new\n---\nNew version.\n",
		)
		.unwrap();

		// Claude installs from path — canonical already exists, should NOT
		// be overwritten
		let mut mgr = ConfigManager::new(
			create_adapter(AgentType::Claude),
			false,
			Some(root),
		);
		mgr.load().unwrap();
		mgr.add_skill_from_path_universal(&src).unwrap();

		let content =
			std::fs::read_to_string(canonical.join("SKILL.md")).unwrap();
		assert!(
			content.contains("Old version."),
			"Canonical should not be overwritten: {content}"
		);

		// Cursor discovers the skill from .agents/skills/ on load (Cursor
		// scans that directory), confirming the canonical is intact and
		// accessible to other agents.
		let mut mgr2 = ConfigManager::new(
			create_adapter(AgentType::Cursor),
			false,
			Some(root),
		);
		mgr2.load().unwrap();
		assert!(
			mgr2.config
				.as_ref()
				.unwrap()
				.skills
				.iter()
				.any(|s| s.name == "shared-skill"),
			"Cursor should discover shared-skill from .agents/skills/ on load"
		);
	}

	// -----------------------------------------------------------------------
	// Task 5: remove_skill_planned owns the post-delete lock prune.
	// -----------------------------------------------------------------------

	// Reuse the ONE shared global-lock guard so these tests serialize on the
	// same mutex as the prune.rs tests (separate static LOCKs would race on the
	// shared XDG_STATE_HOME global lock when the whole suite runs in-process).
	use crate::skills::prune::test_lock::GlobalLockGuard;

	fn locked_entry() -> skill::SkillLockEntry {
		skill::SkillLockEntry {
			source: "o/r".to_string(),
			source_type: "github".to_string(),
			source_url: "https://github.com/o/r".to_string(),
			ref_name: None,
			skill_path: None,
			skill_folder_hash: "h".to_string(),
			content_hash: None,
			ref_commit: None,
			installed_at: "t".to_string(),
			updated_at: "t".to_string(),
			plugin_name: None,
		}
	}

	#[test]
	fn remove_skill_planned_prunes_lock_on_execute() {
		use crate::create_adapter;
		use crate::models::AgentType;

		let _g = GlobalLockGuard::new();
		let tmp = tempfile::tempdir().unwrap();
		let skills_dir = tmp.path().join("skills");
		std::fs::create_dir_all(&skills_dir).unwrap();
		// A real skill on disk so execute actually deletes something.
		let skill_dir = skills_dir.join("prune-me-skill");
		std::fs::create_dir_all(&skill_dir).unwrap();
		std::fs::write(
			skill_dir.join("SKILL.md"),
			"---\nname: prune-me-skill\ndescription: d\n---\n",
		)
		.unwrap();

		crate::adapter::set_skills_path_override(
			"claude",
			Some(skills_dir.clone()),
		);

		// Lock entry for the skill being removed (gets pruned once gone) plus an
		// orphan that is never on disk (also pruned).
		skill::lock::add_skill_to_lock("prune-me-skill", locked_entry())
			.unwrap();
		skill::lock::add_skill_to_lock(
			"orphan-never-on-disk-xyz",
			locked_entry(),
		)
		.unwrap();

		let mut mgr =
			ConfigManager::new(create_adapter(AgentType::Claude), true, None);
		mgr.load().unwrap();

		let outcome = mgr
			.remove_skill_planned("prune-me-skill", false, false, true)
			.unwrap();

		crate::adapter::set_skills_path_override("claude", None);

		assert!(outcome.executed, "copy single-agent removal executes");
		let pruned = match &outcome.prune {
			crate::skills::removal::PruneStatus::Pruned(keys) => keys,
			other => panic!("prune must run on execute, got {other:?}"),
		};
		// The reported keys must name the orphan that was actually dropped — not
		// just "some prune ran". The removed skill is disk-derived and may be
		// gone from the in-memory view before the scan, but the never-on-disk
		// orphan must always be reported as pruned.
		assert!(
			pruned.contains(&"orphan-never-on-disk-xyz".to_string()),
			"reported pruned keys must include the dropped orphan, got {pruned:?}"
		);
		let lock = skill::read_skill_lock();
		assert!(
			!lock.skills.contains_key("prune-me-skill"),
			"removed skill's lock entry must be pruned"
		);
		assert!(
			!lock.skills.contains_key("orphan-never-on-disk-xyz"),
			"orphan lock entry must be pruned"
		);
	}

	/// Regression for the `PruneStatus::Failed` path through the REAL manager
	/// (not synthetic `prune_status`/`combine_prune` inputs): force the
	/// post-delete lock write to fail and assert the skill is still deleted, the
	/// lock is left unchanged, and the outcome is `Failed { reason, pruned }`. A
	/// prune failure is non-fatal — deletion already happened.
	#[cfg(unix)]
	#[test]
	fn remove_skill_planned_failed_prune_keeps_lock_and_deletes_skill() {
		use crate::create_adapter;
		use crate::models::AgentType;
		use std::os::unix::fs::PermissionsExt;

		let _g = GlobalLockGuard::new();
		// GlobalLockGuard points XDG_STATE_HOME at a fresh temp dir; the lock
		// lives at $XDG_STATE_HOME/skills/.skill-lock.json.
		let state = std::env::var("XDG_STATE_HOME").unwrap();
		let lock_dir = std::path::Path::new(&state).join("skills");

		let tmp = tempfile::tempdir().unwrap();
		let skills_dir = tmp.path().join("skills");
		let skill_dir = skills_dir.join("fail-prune-skill");
		std::fs::create_dir_all(&skill_dir).unwrap();
		std::fs::write(
			skill_dir.join("SKILL.md"),
			"---\nname: fail-prune-skill\ndescription: d\n---\n",
		)
		.unwrap();

		crate::adapter::set_skills_path_override(
			"claude",
			Some(skills_dir.clone()),
		);

		// Seed an orphan a successful prune WOULD drop, then make the lock dir
		// read-only so the prune's atomic temp+rename write fails (Io error).
		skill::lock::add_skill_to_lock(
			"orphan-never-on-disk-xyz",
			locked_entry(),
		)
		.unwrap();
		if !perms_enforced(&lock_dir) {
			crate::adapter::set_skills_path_override("claude", None);
			eprintln!("skip: perms not enforced (root)");
			return;
		}
		let orig = std::fs::metadata(&lock_dir).unwrap().permissions();
		std::fs::set_permissions(
			&lock_dir,
			std::fs::Permissions::from_mode(0o555),
		)
		.unwrap();

		let mut mgr =
			ConfigManager::new(create_adapter(AgentType::Claude), true, None);
		mgr.load().unwrap();
		let outcome = mgr
			.remove_skill_planned("fail-prune-skill", false, false, true)
			.unwrap();

		// RESTORE perms before any assertion so a failed assert never leaks an
		// unremovable temp dir.
		std::fs::set_permissions(&lock_dir, orig).unwrap();
		crate::adapter::set_skills_path_override("claude", None);

		assert!(outcome.executed, "deletion runs even if the prune fails");
		assert!(
			!skill_dir.exists(),
			"the skill is deleted before the prune is attempted"
		);
		match outcome.prune {
			crate::skills::removal::PruneStatus::Failed { reason, pruned } => {
				assert!(!reason.is_empty(), "failure reason is reported");
				assert!(
					pruned.is_empty(),
					"single-scope write failure drops nothing: {pruned:?}"
				);
			}
			other => panic!("expected Failed, got {other:?}"),
		}
		let lock = skill::read_skill_lock();
		assert!(
			lock.skills.contains_key("orphan-never-on-disk-xyz"),
			"a failed prune must leave the lock unchanged"
		);
	}

	#[test]
	fn remove_skill_planned_dry_run_leaves_prune_notrun() {
		use crate::create_adapter;
		use crate::models::AgentType;

		let _g = GlobalLockGuard::new();
		let tmp = tempfile::tempdir().unwrap();
		let skills_dir = tmp.path().join("skills");
		std::fs::create_dir_all(&skills_dir).unwrap();
		let skill_dir = skills_dir.join("keep-me-skill");
		std::fs::create_dir_all(&skill_dir).unwrap();
		std::fs::write(
			skill_dir.join("SKILL.md"),
			"---\nname: keep-me-skill\ndescription: d\n---\n",
		)
		.unwrap();

		crate::adapter::set_skills_path_override(
			"claude",
			Some(skills_dir.clone()),
		);

		// Orphan present in the lock: a real prune WOULD drop it; a dry-run must
		// not, proving prune never ran.
		skill::lock::add_skill_to_lock(
			"orphan-never-on-disk-xyz",
			locked_entry(),
		)
		.unwrap();

		let mut mgr =
			ConfigManager::new(create_adapter(AgentType::Claude), true, None);
		mgr.load().unwrap();

		let outcome = mgr
			.remove_skill_planned("keep-me-skill", false, true, false)
			.unwrap();

		crate::adapter::set_skills_path_override("claude", None);

		assert!(!outcome.executed, "dry-run must not delete");
		assert_eq!(
			outcome.prune,
			crate::skills::removal::PruneStatus::NotRun,
			"dry-run leaves prune NotRun"
		);
		let lock = skill::read_skill_lock();
		assert!(
			lock.skills.contains_key("orphan-never-on-disk-xyz"),
			"dry-run must not prune the lock"
		);
	}

	/// The confirm-gated branch (destructive op, NOT yet confirmed) is also a
	/// non-executed path: it must leave `prune == NotRun` and the lock untouched,
	/// exactly like a dry-run. Distinct from the dry-run test because the gate is
	/// `needs_confirm && !confirm` (all-agents), not `dry_run`.
	#[test]
	fn remove_skill_planned_unconfirmed_destructive_leaves_prune_notrun() {
		use crate::create_adapter;
		use crate::models::AgentType;

		let _g = GlobalLockGuard::new();
		let tmp = tempfile::tempdir().unwrap();
		let skills_dir = tmp.path().join("skills");
		let skill_dir = skills_dir.join("gated-skill");
		std::fs::create_dir_all(&skill_dir).unwrap();
		std::fs::write(
			skill_dir.join("SKILL.md"),
			"---\nname: gated-skill\ndescription: d\n---\n",
		)
		.unwrap();

		crate::adapter::set_skills_path_override(
			"claude",
			Some(skills_dir.clone()),
		);
		// Orphan a successful prune WOULD drop — proves the gated branch never
		// reaches the prune.
		skill::lock::add_skill_to_lock(
			"orphan-never-on-disk-xyz",
			locked_entry(),
		)
		.unwrap();

		let mut mgr =
			ConfigManager::new(create_adapter(AgentType::Claude), true, None);
		mgr.load().unwrap();
		// all_agents=true => needs_confirm; confirm=false => gated, not executed.
		let outcome = mgr
			.remove_skill_planned("gated-skill", true, false, false)
			.unwrap();

		crate::adapter::set_skills_path_override("claude", None);

		assert!(!outcome.executed, "unconfirmed destructive op must not run");
		assert_eq!(
			outcome.prune,
			crate::skills::removal::PruneStatus::NotRun,
			"gated (unconfirmed) op leaves prune NotRun"
		);
		assert!(skill_dir.exists(), "gated op must not delete");
		let lock = skill::read_skill_lock();
		assert!(
			lock.skills.contains_key("orphan-never-on-disk-xyz"),
			"gated op must not prune the lock"
		);
	}

	// The pure combine_prune / prune_status folds now live with the
	// prune_lock_for_scope seam they feed (crate::skills::prune tests).
	// The cases below exercise the seam through the REAL manager.
	use crate::skills::removal::PruneStatus;

	#[test]
	fn remove_skill_planned_project_scope_without_root_leaves_prune_notrun() {
		// ProjectOnly scope with no project root: the manager must NOT attempt a
		// project prune (it has no lock to reconcile) — matching the old caller
		// behavior. Prune is NotRun and the global lock is untouched even though
		// an orphan sits in it.
		use crate::create_adapter;
		use crate::models::AgentType;

		let _g = GlobalLockGuard::new();
		let tmp = tempfile::tempdir().unwrap();
		let skills_dir = tmp.path().join("skills");
		let skill_dir = skills_dir.join("proj-no-root-skill");
		std::fs::create_dir_all(&skill_dir).unwrap();
		std::fs::write(
			skill_dir.join("SKILL.md"),
			"---\nname: proj-no-root-skill\ndescription: d\n---\n",
		)
		.unwrap();

		crate::adapter::set_skills_path_override(
			"claude",
			Some(skills_dir.clone()),
		);
		skill::lock::add_skill_to_lock(
			"orphan-never-on-disk-xyz",
			locked_entry(),
		)
		.unwrap();

		// global=false, project_root=None => ResourceScope::ProjectOnly, no root.
		let mut mgr =
			ConfigManager::new(create_adapter(AgentType::Claude), false, None);
		mgr.load().unwrap();
		let outcome = mgr
			.remove_skill_planned("proj-no-root-skill", false, false, true)
			.unwrap();

		crate::adapter::set_skills_path_override("claude", None);

		assert!(outcome.executed, "removal still executes");
		assert_eq!(
			outcome.prune,
			PruneStatus::NotRun,
			"project prune without a root must be NotRun, got {:?}",
			outcome.prune
		);
		let lock = skill::read_skill_lock();
		assert!(
			lock.skills.contains_key("orphan-never-on-disk-xyz"),
			"no prune ran, so the global lock is untouched"
		);
	}

	fn local_entry() -> skill::lock::local::LocalSkillLockEntry {
		skill::lock::local::LocalSkillLockEntry {
			source: "o/r".to_string(),
			ref_name: None,
			source_type: "github".to_string(),
			computed_hash: "h".to_string(),
			skill_path: None,
			ref_commit: None,
		}
	}

	#[test]
	fn remove_skill_planned_both_scope_prunes_global_and_project_locks() {
		// Both scope reconciles two independent locks (global + project). Seed an
		// orphan in each, execute the removal, and assert the returned Pruned keys
		// name BOTH dropped orphans and that both locks are updated on disk.
		use crate::create_adapter;
		use crate::models::{AgentType, ResourceScope};

		let _g = GlobalLockGuard::new();
		let project = tempfile::tempdir().unwrap();
		let skills_dir = project.path().join("skills");
		let skill_dir = skills_dir.join("both-skill");
		std::fs::create_dir_all(&skill_dir).unwrap();
		std::fs::write(
			skill_dir.join("SKILL.md"),
			"---\nname: both-skill\ndescription: d\n---\n",
		)
		.unwrap();

		crate::adapter::set_skills_path_override(
			"claude",
			Some(skills_dir.clone()),
		);
		// One orphan per lock — neither is on disk, so a real prune drops both.
		skill::lock::add_skill_to_lock("orphan-global-xyz", locked_entry())
			.unwrap();
		skill::lock::local::add_skill_to_local_lock(
			"orphan-project-xyz",
			local_entry(),
			Some(project.path()),
		)
		.unwrap();

		// scope=Both with a project root; write_scope=ProjectOnly (global=false).
		let mut mgr = ConfigManager::with_scope(
			create_adapter(AgentType::Claude),
			false,
			Some(project.path()),
			ResourceScope::Both,
		);
		mgr.load().unwrap();
		let outcome = mgr
			.remove_skill_planned("both-skill", false, false, true)
			.unwrap();

		crate::adapter::set_skills_path_override("claude", None);

		assert!(outcome.executed, "Both-scope removal executes");
		let pruned = match &outcome.prune {
			PruneStatus::Pruned(keys) => keys,
			other => panic!("expected Pruned, got {other:?}"),
		};
		assert!(
			pruned.contains(&"orphan-global-xyz".to_string()),
			"global orphan must be reported pruned, got {pruned:?}"
		);
		assert!(
			pruned.contains(&"orphan-project-xyz".to_string()),
			"project orphan must be reported pruned, got {pruned:?}"
		);
		let global = skill::read_skill_lock();
		assert!(
			!global.skills.contains_key("orphan-global-xyz"),
			"global lock orphan must be pruned on disk"
		);
		let local = skill::lock::local::read_local_lock(Some(project.path()));
		assert!(
			!local.skills.contains_key("orphan-project-xyz"),
			"project lock orphan must be pruned on disk"
		);
	}

	/// Regression (issue #1): `Both` must short-circuit on a GLOBAL prune
	/// failure — the project lock must be left UNTOUCHED, not mutated behind a
	/// `Failed { pruned: [] }`. Force the GLOBAL lock write to fail (read-only
	/// global lock dir) while a project orphan sits ready to drop, then assert
	/// the project lock still holds its orphan and prune is `Failed` with an
	/// empty `pruned`.
	#[cfg(unix)]
	#[test]
	fn remove_skill_planned_both_global_failure_leaves_project_lock_untouched()
	{
		use crate::create_adapter;
		use crate::models::{AgentType, ResourceScope};
		use std::os::unix::fs::PermissionsExt;

		let _g = GlobalLockGuard::new();
		let state = std::env::var("XDG_STATE_HOME").unwrap();
		let lock_dir = std::path::Path::new(&state).join("skills");

		let project = tempfile::tempdir().unwrap();
		let skills_dir = project.path().join("skills");
		let skill_dir = skills_dir.join("both-skill");
		std::fs::create_dir_all(&skill_dir).unwrap();
		std::fs::write(
			skill_dir.join("SKILL.md"),
			"---\nname: both-skill\ndescription: d\n---\n",
		)
		.unwrap();

		crate::adapter::set_skills_path_override(
			"claude",
			Some(skills_dir.clone()),
		);
		// Seed an orphan in EACH lock; neither is on disk so a real prune would
		// drop both. The project orphan must SURVIVE because the global prune
		// fails first and the project prune must never run.
		skill::lock::add_skill_to_lock("orphan-global-xyz", locked_entry())
			.unwrap();
		skill::lock::local::add_skill_to_local_lock(
			"orphan-project-xyz",
			local_entry(),
			Some(project.path()),
		)
		.unwrap();

		if !perms_enforced(&lock_dir) {
			crate::adapter::set_skills_path_override("claude", None);
			eprintln!("skip: perms not enforced (root)");
			return;
		}
		// Make the GLOBAL lock dir read-only so its atomic temp+rename fails.
		let orig = std::fs::metadata(&lock_dir).unwrap().permissions();
		std::fs::set_permissions(
			&lock_dir,
			std::fs::Permissions::from_mode(0o555),
		)
		.unwrap();

		let mut mgr = ConfigManager::with_scope(
			create_adapter(AgentType::Claude),
			false,
			Some(project.path()),
			ResourceScope::Both,
		);
		mgr.load().unwrap();
		let outcome = mgr
			.remove_skill_planned("both-skill", false, false, true)
			.unwrap();

		std::fs::set_permissions(&lock_dir, orig).unwrap();
		crate::adapter::set_skills_path_override("claude", None);

		assert!(outcome.executed, "Both-scope removal still executes");
		match outcome.prune {
			PruneStatus::Failed { reason, pruned } => {
				assert!(
					!reason.is_empty(),
					"global failure reason is reported"
				);
				assert!(
					pruned.is_empty(),
					"global failed before pruning anything: {pruned:?}"
				);
			}
			other => panic!("expected Failed on global failure, got {other:?}"),
		}
		let local = skill::lock::local::read_local_lock(Some(project.path()));
		assert!(
			local.skills.contains_key("orphan-project-xyz"),
			"global failure must short-circuit: the project lock is untouched"
		);
	}

	/// Regression (issue #4): the global-success / project-FAIL partial path
	/// through the REAL `remove_skill_planned` (not synthetic `combine_prune`
	/// inputs). Force the PROJECT lock write to fail AFTER the global prune
	/// succeeds, then assert the global lock WAS mutated and prune is
	/// `Failed { pruned: [<global key>] }`.
	#[cfg(unix)]
	#[test]
	fn remove_skill_planned_both_project_failure_reports_partial_global_pruned()
	{
		use crate::create_adapter;
		use crate::models::{AgentType, ResourceScope};
		use std::os::unix::fs::PermissionsExt;

		let _g = GlobalLockGuard::new();

		let project = tempfile::tempdir().unwrap();
		let skills_dir = project.path().join("skills");
		let skill_dir = skills_dir.join("both-skill");
		std::fs::create_dir_all(&skill_dir).unwrap();
		std::fs::write(
			skill_dir.join("SKILL.md"),
			"---\nname: both-skill\ndescription: d\n---\n",
		)
		.unwrap();

		crate::adapter::set_skills_path_override(
			"claude",
			Some(skills_dir.clone()),
		);
		// Global orphan WILL be pruned; project orphan would be pruned too, but
		// the project lock write fails so the project lock stays intact.
		skill::lock::add_skill_to_lock("orphan-global-xyz", locked_entry())
			.unwrap();
		skill::lock::local::add_skill_to_local_lock(
			"orphan-project-xyz",
			local_entry(),
			Some(project.path()),
		)
		.unwrap();

		if !perms_enforced(project.path()) {
			crate::adapter::set_skills_path_override("claude", None);
			eprintln!("skip: perms not enforced (root)");
			return;
		}
		// The project lock is `<root>/skills-lock.json`; making the project root
		// read-only blocks the atomic temp+rename inside it (the global lock
		// lives under XDG_STATE_HOME and stays writable, so global succeeds).
		let orig = std::fs::metadata(project.path()).unwrap().permissions();
		std::fs::set_permissions(
			project.path(),
			std::fs::Permissions::from_mode(0o555),
		)
		.unwrap();

		let mut mgr = ConfigManager::with_scope(
			create_adapter(AgentType::Claude),
			false,
			Some(project.path()),
			ResourceScope::Both,
		);
		mgr.load().unwrap();
		let outcome = mgr
			.remove_skill_planned("both-skill", false, false, true)
			.unwrap();

		std::fs::set_permissions(project.path(), orig).unwrap();
		crate::adapter::set_skills_path_override("claude", None);

		assert!(outcome.executed, "Both-scope removal still executes");
		match outcome.prune {
			PruneStatus::Failed { reason, pruned } => {
				assert!(
					!reason.is_empty(),
					"project failure reason is reported"
				);
				assert_eq!(
					pruned,
					vec!["orphan-global-xyz".to_string()],
					"the global keys dropped before the project failure must \
					 be reported, got {pruned:?}"
				);
			}
			other => panic!("expected partial Failed, got {other:?}"),
		}
		let global = skill::read_skill_lock();
		assert!(
			!global.skills.contains_key("orphan-global-xyz"),
			"the global lock WAS mutated before the project failure"
		);
	}

	#[test]
	fn remove_skill_planned_project_scope_with_root_prunes_project_lock() {
		// ProjectOnly scope WITH a root: the project lock is reconciled and the
		// dropped orphan is reported. (The no-root variant is covered above.)
		use crate::create_adapter;
		use crate::models::AgentType;

		let _g = GlobalLockGuard::new();
		let project = tempfile::tempdir().unwrap();
		let skills_dir = project.path().join("skills");
		let skill_dir = skills_dir.join("proj-root-skill");
		std::fs::create_dir_all(&skill_dir).unwrap();
		std::fs::write(
			skill_dir.join("SKILL.md"),
			"---\nname: proj-root-skill\ndescription: d\n---\n",
		)
		.unwrap();

		crate::adapter::set_skills_path_override(
			"claude",
			Some(skills_dir.clone()),
		);
		skill::lock::local::add_skill_to_local_lock(
			"orphan-project-xyz",
			local_entry(),
			Some(project.path()),
		)
		.unwrap();

		// global=false + a project root => ResourceScope::ProjectOnly with a root.
		let mut mgr = ConfigManager::new(
			create_adapter(AgentType::Claude),
			false,
			Some(project.path()),
		);
		mgr.load().unwrap();
		let outcome = mgr
			.remove_skill_planned("proj-root-skill", false, false, true)
			.unwrap();

		crate::adapter::set_skills_path_override("claude", None);

		assert!(outcome.executed, "ProjectOnly removal executes");
		let pruned = match &outcome.prune {
			PruneStatus::Pruned(keys) => keys,
			other => panic!("expected Pruned, got {other:?}"),
		};
		assert!(
			pruned.contains(&"orphan-project-xyz".to_string()),
			"project orphan must be reported pruned, got {pruned:?}"
		);
		let local = skill::lock::local::read_local_lock(Some(project.path()));
		assert!(
			!local.skills.contains_key("orphan-project-xyz"),
			"project lock orphan must be pruned on disk"
		);
	}

	/// ProjectOnly scope WITH a root where the PROJECT lock write FAILS: the
	/// prune is non-fatal, so the skill is still removed, the single-scope
	/// failure drops nothing (`Failed { pruned: [] }`), and the project lock
	/// stays intact. Mirrors the Both-project-failure technique (RO root).
	#[cfg(unix)]
	#[test]
	fn remove_skill_planned_project_scope_with_root_failed_prune_keeps_lock() {
		use crate::create_adapter;
		use crate::models::AgentType;
		use std::os::unix::fs::PermissionsExt;

		let _g = GlobalLockGuard::new();
		let project = tempfile::tempdir().unwrap();
		let skills_dir = project.path().join("skills");
		let skill_dir = skills_dir.join("proj-fail-skill");
		std::fs::create_dir_all(&skill_dir).unwrap();
		std::fs::write(
			skill_dir.join("SKILL.md"),
			"---\nname: proj-fail-skill\ndescription: d\n---\n",
		)
		.unwrap();

		crate::adapter::set_skills_path_override(
			"claude",
			Some(skills_dir.clone()),
		);
		// Orphan a successful project prune WOULD drop — but the write fails,
		// so it must survive in the project lock.
		skill::lock::local::add_skill_to_local_lock(
			"orphan-project-xyz",
			local_entry(),
			Some(project.path()),
		)
		.unwrap();

		if !perms_enforced(project.path()) {
			crate::adapter::set_skills_path_override("claude", None);
			eprintln!("skip: perms not enforced (root)");
			return;
		}
		// The project lock is `<root>/skills-lock.json`; a read-only root
		// blocks the atomic temp+rename inside it (skills_dir was created
		// beforehand, so the skill itself is still deletable under it).
		let orig = std::fs::metadata(project.path()).unwrap().permissions();
		std::fs::set_permissions(
			project.path(),
			std::fs::Permissions::from_mode(0o555),
		)
		.unwrap();

		// global=false + a project root => ProjectOnly scope with a root.
		let mut mgr = ConfigManager::new(
			create_adapter(AgentType::Claude),
			false,
			Some(project.path()),
		);
		mgr.load().unwrap();
		let outcome = mgr
			.remove_skill_planned("proj-fail-skill", false, false, true)
			.unwrap();

		// RESTORE perms before any assertion so a failed assert never leaks an
		// unremovable temp dir.
		std::fs::set_permissions(project.path(), orig).unwrap();
		crate::adapter::set_skills_path_override("claude", None);

		assert!(outcome.executed, "deletion runs even if the prune fails");
		assert!(
			!skill_dir.exists(),
			"the skill is deleted before the prune is attempted"
		);
		match outcome.prune {
			PruneStatus::Failed { reason, pruned } => {
				assert!(!reason.is_empty(), "failure reason is reported");
				assert!(
					pruned.is_empty(),
					"single-scope write failure drops nothing: {pruned:?}"
				);
			}
			other => panic!("expected Failed, got {other:?}"),
		}
		let local = skill::lock::local::read_local_lock(Some(project.path()));
		assert!(
			local.skills.contains_key("orphan-project-xyz"),
			"a failed project prune must leave the project lock unchanged"
		);
	}

	// T3: exhaustive branch coverage of the helper that maps the shared
	// materializer's single-agent result onto the CLI add path's historical
	// error contract. NeedsLink-ok -> Ok; NeedsLink-error -> Err(resource_exists);
	// NativeReader / Unsupported -> Ok regardless of the result (the agent reads
	// the Master directly, or had no writable dir — neither was ever an error).
	#[test]
	fn ensure_single_agent_installed_covers_every_branch() {
		use crate::models::AgentType;
		use crate::skills::install_fetched::AgentInstallResult;
		use crate::skills::linker::LinkNeed;

		let needs_link = LinkNeed::NeedsLink {
			agent_skills_dir: PathBuf::from("/x"),
		};

		// NeedsLink + error-free result -> Ok.
		let ok = [AgentInstallResult {
			agent: AgentType::Claude,
			installed: true,
			error: None,
		}];
		assert!(ConfigManager::ensure_single_agent_installed(
			&ok,
			&needs_link,
			"s"
		)
		.is_ok());

		// NeedsLink + a soft failure (occupied slot / link error) -> Err.
		let conflict = [AgentInstallResult {
			agent: AgentType::Claude,
			installed: false,
			error: Some("slot occupied".to_string()),
		}];
		let err = ConfigManager::ensure_single_agent_installed(
			&conflict,
			&needs_link,
			"my-skill",
		)
		.unwrap_err();
		assert!(
			matches!(err, ConfigError::ResourceExists { .. }),
			"a NeedsLink soft-failure must surface resource_exists, got {err:?}"
		);

		// NeedsLink + empty results (defensive) -> Err.
		assert!(ConfigManager::ensure_single_agent_installed(
			&[],
			&needs_link,
			"s"
		)
		.is_err());

		// NativeReader -> Ok even with a soft-failure result (never an error).
		assert!(ConfigManager::ensure_single_agent_installed(
			&conflict,
			&LinkNeed::NativeReader,
			"s"
		)
		.is_ok());

		// Unsupported -> Ok even with a soft-failure result (no writable dir;
		// the Master is still recorded — old behaviour preserved).
		assert!(ConfigManager::ensure_single_agent_installed(
			&conflict,
			&LinkNeed::Unsupported,
			"s"
		)
		.is_ok());
	}

	// T3 parity guard: the CLI add-from-path materialization and the
	// fetched/desktop materialization must produce a BYTE-IDENTICAL
	// `.agents/skills/<name>/SKILL.md` and the same agent link shape for the
	// same source skill — so the two install paths can never diverge again
	// (they once did when the CLI used a narrower link check). Both copy the
	// source tree verbatim; only the canonical SKILL.md bytes + link shape are
	// asserted, not the lock (the lock contract is pinned elsewhere).
	#[cfg(unix)]
	#[test]
	fn cli_add_and_fetched_install_produce_identical_master_and_link() {
		use crate::create_adapter;
		use crate::models::{AgentType, ResourceScope};
		use crate::skills::install_fetched::{
			install_fetched_skill_and_lock, FetchedSkillInstallRequest,
		};
		use crate::skills::linker::{LinkTarget, Linker};

		// One source skill, copied verbatim by both paths. Non-canonical
		// frontmatter ordering + a body + an asset so a re-serialization (which
		// would NOT be byte-identical) is detectable.
		let src_tmp = tempfile::tempdir().unwrap();
		let src = src_tmp.path().join("parity-skill");
		std::fs::create_dir_all(&src).unwrap();
		let skill_md =
			"---\ndescription: parity\nname: parity-skill\n---\nThe body.\n";
		std::fs::write(src.join("SKILL.md"), skill_md).unwrap();
		std::fs::create_dir_all(src.join("assets")).unwrap();
		std::fs::write(src.join("assets/data.json"), "{}").unwrap();

		// Path A: CLI add-from-path universal install.
		let cli_root_tmp = tempfile::tempdir().unwrap();
		let cli_root = cli_root_tmp.path().canonicalize().unwrap();
		let mut mgr = ConfigManager::new(
			create_adapter(AgentType::Claude),
			false,
			Some(&cli_root),
		);
		mgr.load().unwrap();
		mgr.add_skill_from_path_universal(&src).unwrap();

		// Path B: fetched/desktop universal install of the same source.
		let fetched_root_tmp = tempfile::tempdir().unwrap();
		let fetched_root = fetched_root_tmp.path().canonicalize().unwrap();
		let lock_source = skill::InstallLockSource {
			source: "local/test".to_string(),
			source_type: "local".to_string(),
			source_url: "file:///local/test".to_string(),
			ref_name: None,
		};
		let req = FetchedSkillInstallRequest {
			skill_file: &src.join("SKILL.md"),
			source: &lock_source,
			lock_skill_path: "parity-skill/SKILL.md".to_string(),
			ref_commit: None,
			scope: ResourceScope::ProjectOnly,
			project_root: Some(&fetched_root),
			target_agents: &[AgentType::Claude],
			expected_name: None,
			target: LinkTarget::Relative,
		};
		install_fetched_skill_and_lock(req).unwrap();

		// The canonical Master SKILL.md must be byte-identical across paths.
		let cli_master = cli_root.join(".agents/skills/parity-skill/SKILL.md");
		let fetched_master =
			fetched_root.join(".agents/skills/parity-skill/SKILL.md");
		let cli_bytes = std::fs::read(&cli_master).unwrap();
		let fetched_bytes = std::fs::read(&fetched_master).unwrap();
		assert_eq!(
			cli_bytes, fetched_bytes,
			"CLI-add and fetched-install master SKILL.md must be \
			 byte-identical"
		);
		assert_eq!(
			cli_bytes,
			skill_md.as_bytes(),
			"both paths must copy the source SKILL.md verbatim"
		);

		// The asset must survive on both (whole-tree copy, not SKILL.md only).
		assert_eq!(
			std::fs::read_to_string(
				cli_root.join(".agents/skills/parity-skill/assets/data.json")
			)
			.unwrap(),
			std::fs::read_to_string(
				fetched_root
					.join(".agents/skills/parity-skill/assets/data.json")
			)
			.unwrap(),
		);

		// Identical link shape: each agent dir holds a symlink to its Master.
		let cli_link = cli_root.join(".claude/skills/parity-skill");
		let fetched_link = fetched_root.join(".claude/skills/parity-skill");
		assert!(Linker::is_link(&cli_link), "CLI add must leave a link");
		assert!(
			Linker::is_link(&fetched_link),
			"fetched install must leave a link"
		);
		assert!(cli_link.join("SKILL.md").exists());
		assert!(fetched_link.join("SKILL.md").exists());
	}
}
