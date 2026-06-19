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

	/// Add a skill in *universal* layout (opt-in): write the real `SKILL.md`
	/// once into `.agents/skills/<name>` and symlink THIS agent's skills dir to
	/// it (npx-style). Sets `canonical_path` so layout-aware removal recognises
	/// the symlink. The default [`Self::add_skill`] copy behaviour is unchanged;
	/// callers opt in (e.g. CLI `--universal`).
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
		} = self.universal_install_prep()?;

		let config = self.config_mut()?;
		if config.skills.iter().any(|s| s.name == skill.name) {
			return Err(ConfigError::resource_exists("skill", &skill.name));
		}
		info!(
			"adding skill '{}' (universal layout) for agent '{}'",
			skill.name, agent_name
		);

		let safe_name = sanitize_name(&skill.name);
		let canonical = canonical_dir.join(&safe_name);
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

		// Symlink this agent's own skills dir to the master, unless its write dir
		// IS the canonical dir (then the master already lives there).
		if let Some(agent_dir) = &agent_write_dir {
			if agent_dir != &canonical_dir {
				crate::skills::linker::link_agents_to_canonical(
					&canonical,
					std::slice::from_ref(agent_dir),
					if use_relative {
						crate::skills::linker::LinkTarget::Relative
					} else {
						crate::skills::linker::LinkTarget::Absolute
					},
				)
				.map_err(|e| {
					ConfigError::Io(std::io::Error::other(e.to_string()))
				})?;
			}
		}

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
	/// and tolerates already-removed paths. The lock is NOT pruned here — pruning
	/// is a separate, explicit step (`skills::prune`).
	pub fn remove_skill_planned(
		&mut self,
		name: &str,
		all_agents: bool,
		dry_run: bool,
		confirm: bool,
	) -> Result<crate::skills::removal::RemovalOutcome> {
		use crate::skills::removal;

		let config = self.config.as_ref().ok_or_else(|| {
			ConfigError::InvalidConfig("No configuration loaded".to_string())
		})?;
		let skill = config
			.skills
			.iter()
			.find(|s| s.name == name)
			.cloned()
			.ok_or_else(|| ConfigError::resource_not_found("skill", name))?;

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

		Ok(removal::RemovalOutcome {
			plan,
			executed: true,
		})
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

	/// Universal-layout variant of [`Self::add_skill_from_path`]: parses the
	/// skill then installs it in `.agents/skills/<name>` (canonical) with a
	/// per-agent symlink in this agent's skills dir. The full source tree
	/// (`assets/`, `scripts/`, `examples/`, etc.) is copied to the canonical
	/// — matching the API path's `install_git_skill_universal` behaviour. The
	/// pre-fix implementation only wrote the synthesized `SKILL.md` and
	/// silently dropped every other file the source contained.
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
		} = self.universal_install_prep()?;

		let config = self.config_mut()?;
		if config.skills.iter().any(|s| s.name == skill.name) {
			return Err(ConfigError::resource_exists("skill", &skill.name));
		}
		info!(
			"adding skill '{}' (universal layout, from path) for agent '{}'",
			skill.name, agent_name
		);

		let safe_name = sanitize_name(&skill.name);
		let canonical = canonical_dir.join(&safe_name);

		// `install_universal` only copies source -> canonical when canonical
		// is absent, so a pre-existing master is preserved (idempotent across
		// multi-agent installs of the same skill).
		let source_root = crate::skills::skill_source_root(path);
		let symlink_dirs: Vec<PathBuf> = match &agent_write_dir {
			Some(d) if d.as_path() != canonical_dir.as_path() => {
				vec![d.clone()]
			}
			_ => Vec::new(),
		};
		crate::skills::linker::install_universal(
			&source_root,
			&canonical,
			&symlink_dirs,
			if use_relative {
				crate::skills::linker::LinkTarget::Relative
			} else {
				crate::skills::linker::LinkTarget::Absolute
			},
		)
		.map_err(|e| ConfigError::Io(std::io::Error::other(e.to_string())))?;

		let canonical_md =
			canonical.join("SKILL.md").to_string_lossy().to_string();
		let mut fs_skill = skill.clone();
		fs_skill.source_path = Some(canonical_md.clone());
		fs_skill.canonical_path = Some(canonical_md);
		config.skills.push(fs_skill);

		self.save_current()?;
		Ok(skill)
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
		Ok(UniversalPrep {
			agent_name: self.adapter.name().to_string(),
			agent_write_dir: self.target_skills_dir(),
			use_relative: project_root_for_canonical.is_some(),
			canonical_dir,
		})
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
/// relink error on success; if the rollback itself fails, returns a compound
/// error naming both failures and the master path that needs manual recovery.
#[allow(clippy::too_many_arguments)]
fn rollback_master_rename(
	new_master: &Path,
	old_master: &Path,
	referrers: &[PathBuf],
	safe_new: &str,
	use_relative: bool,
	relink_err: ConfigError,
) -> ConfigError {
	let do_rollback = || -> std::io::Result<()> {
		// Put the master back first so old-name symlinks resolve again.
		std::fs::rename(new_master, old_master)?;
		// Remove any new-name symlinks the partial relink managed to create
		// (they now point at the vanished new_master).
		for dir in referrers {
			let new_link = dir.join(safe_new);
			if Linker::is_link(&new_link) {
				Linker::unlink(&new_link)?;
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
		.map_err(|e| std::io::Error::other(e.to_string()))?;
		Ok(())
	};
	match do_rollback() {
		Ok(()) => relink_err,
		Err(rb_err) => ConfigError::Io(std::io::Error::other(format!(
			"skill relink failed ({relink_err}) and rollback also failed \
			 ({rb_err}); the skill master may need manual recovery at '{}'",
			old_master.display()
		))),
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

		assert!(res.is_err(), "a failed relink must surface as an error");
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
}
