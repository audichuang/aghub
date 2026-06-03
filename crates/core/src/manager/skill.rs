use super::ConfigManager;
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
	is_symlink: bool,
	target_dir: Option<&Path>,
) -> Result<()> {
	if is_symlink {
		// Universal layout: the symlink at `<target_dir>/<safe_name>` is what
		// should disappear. `path.parent()` is the canonical dir (a real
		// directory), not a link, so unlink via the target_dir-resolved path.
		if let Some(target) = target_dir {
			let link = target.join(safe_name);
			let needs_unlink = std::fs::symlink_metadata(&link)
				.map(|m| m.file_type().is_symlink())
				.unwrap_or(false);
			if needs_unlink {
				std::fs::remove_file(&link).map_err(|e| {
					ConfigError::Io(std::io::Error::new(
						e.kind(),
						format!(
							"Failed to remove symlink '{}': {}",
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
		let target_dir = self.target_skills_dir();
		let agent_name = self.adapter.name().to_string();
		let config = self.config_mut()?;
		if config.skills.iter().any(|s| s.name == skill.name) {
			return Err(ConfigError::resource_exists("skill", &skill.name));
		}
		info!("adding skill '{}' for agent '{}'", skill.name, agent_name);

		if let Some(dir) = target_dir {
			let safe_name = sanitize_name(&skill.name);
			let skill_dir = dir.join(&safe_name);
			std::fs::create_dir_all(&skill_dir)?;
			let content = format_skill(&skill, None);
			std::fs::write(skill_dir.join("SKILL.md"), content)?;
			let mut fs_skill = skill.clone();
			fs_skill.source_path =
				Some(skill_dir.join("SKILL.md").to_string_lossy().to_string());
			fs_skill.canonical_path = None;
			config.skills.push(fs_skill);
		} else {
			return Err(ConfigError::InvalidConfig(
				"Agent does not support persistent skill creation \
				 in the current scope"
					.into(),
			));
		}

		self.save_current()
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
				crate::skills::install_layout::link_agents_to_canonical(
					&canonical,
					std::slice::from_ref(agent_dir),
					use_relative,
				)
				.map_err(ConfigError::Io)?;
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

			// Handle rename
			if name != skill.name {
				let safe_new_name = sanitize_name(&skill.name);
				if let Some(parent) = path.parent() {
					if parent.file_name().and_then(|n| n.to_str())
						== Some(&safe_old_name)
					{
						let new_parent = parent.with_file_name(&safe_new_name);
						std::fs::rename(parent, &new_parent).map_err(|e| {
							ConfigError::Io(std::io::Error::new(
								e.kind(),
								format!(
									"Failed to rename skill \
										 directory '{}' -> '{}': {}",
									parent.display(),
									new_parent.display(),
									e
								),
							))
						})?;
						final_file_path =
							new_parent.join(path.file_name().unwrap());
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
		let is_symlink = existing_skill.canonical_path.is_some();

		if let Some(path) = file_path {
			if path.exists() {
				remove_skill_path(
					&path,
					&safe_name,
					is_symlink,
					target_dir.as_deref(),
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
		let skill_pkg = skill::parser::parse(path).map_err(|e| {
			ConfigError::InvalidConfig(format!("Failed to parse skill: {e}"))
		})?;
		let skill = convert_skill(skill_pkg);
		self.add_skill(skill.clone())?;
		Ok(skill)
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
		crate::skills::install_layout::install_universal(
			&source_root,
			&canonical,
			&symlink_dirs,
			use_relative,
		)
		.map_err(ConfigError::Io)?;

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
		let canonical_dir =
			crate::skills::install_layout::universal_canonical_dir(
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
