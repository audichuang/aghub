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
/// `skill` is ALWAYS the skill as it exists on disk after the call — for an
/// install that is what was just written, and for a re-add of something already
/// present it is the untouched Master, NOT the source file that was parsed.
/// The distinction is the whole point of the type: a re-add writes nothing, and
/// a caller that reported the parsed source as if it had been installed told
/// the user their edit had landed when it had not.
#[derive(Debug, Clone)]
pub struct SkillAdd {
	/// The skill as it exists on disk after the call.
	pub skill: Skill,
	/// True when the skill was already installed and nothing was written.
	pub already_installed: bool,
	/// This call atomically claimed and wrote the `.agents/skills/<name>`
	/// Master. Attribution for a caller that must roll its own work back — a
	/// Master merely found and verified belongs to whoever wrote it.
	pub wrote_master: bool,
	/// Agent skills-dirs where this call created a FRESH referrer, straight
	/// from the linker. Never reconstructed from `installed`: a NativeReader
	/// row is installed with no link, and its first read path IS the Master.
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
/// `materialize_universal_master` PRESERVES a pre-existing Master rather than
/// overwriting it (deliberate). When it does, what is on disk is that Master —
/// not the skill the caller just handed in — and [`SkillAdd`]'s contract is
/// that `skill` is ALWAYS what is on disk after the call. Returning the input
/// told a user re-adding an edited source that their new description/version
/// had landed while disk kept the old content, and told `transfer`/`reconcile`
/// that the copy had carried the content over.
///
/// `None` when the Master cannot be parsed. Both callers treat that as an
/// ERROR, not as a reason to fall back to the caller's own input: nothing can
/// read a master that does not parse, so an install reporting success over one
/// is a `get skills` that answers `[]` a second later.
fn master_on_disk(canonical: &Path, name: &str) -> Option<Skill> {
	let pkg = skill::parser::parse(canonical).ok()?;
	let mut found = convert_skill(pkg);
	// Keyed by the REQUESTED name, deliberately — but not for the reason it
	// looks like: discovery reads the frontmatter, not the directory. The point
	// is that this value is what the caller's own `config.skills` entry and
	// every later `get_skill(name)` lookup use, so returning the master's
	// frontmatter name here would hand back a skill findable under neither.
	// A master whose frontmatter disagrees with its directory is its own
	// problem, and `doctor` is where it belongs.
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
	/// Take the interprocess mutation lock for `scope` AND re-read this manager's
	/// config under it — the two are one step on purpose.
	///
	/// `self.config` was populated by a `load()` the CALLER made before entering
	/// the mutation, so the guard alone still leaves every decision below it
	/// (duplicate-name check, target lookup, referrer sweep) resolved from a view
	/// another aghub process may already have invalidated: it reinstalls the same
	/// name, or repoints a Master, and this flow then acts on the stale
	/// `source_path` / `canonical_path`. The API widens that window further by
	/// awaiting plugin detection between its `load()` and the mutation.
	///
	/// Every guarded `ConfigManager` mutation goes through here rather than
	/// calling `mutation_guard` directly, so a new one cannot acquire the lock and
	/// forget the re-read. A dry-run takes neither. One documented exception,
	/// `update_skill` — see the comment there for the macOS-only regression that
	/// keeps its re-read out for now.
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
	///
	/// The two idempotent branches below return
	/// `SkillAdd::already_installed(<the skill ALREADY in config>)`, not the
	/// requested one. They used to return a bare `Ok(())`, which cost the
	/// caller both facts it needs: `aghub add skills -n <existing>` printed
	/// "added skill", serialized `already_installed: false`, and echoed the
	/// REQUESTED `--description` / `--version` / `--tools` back on exit 0 while
	/// the Master on disk kept its old content. Re-running `add` with corrected
	/// metadata is a standard scripted repair, and it silently did nothing.
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
		// concurrent aghub cannot land its own Master between our `exists()` and
		// our write (both would then write SKILL.md, last one wins). Re-reads
		// config under the lock — see `guard_and_reload`.
		let _mutation_guard = self.guard_and_reload("add skill", scope)?;

		let config = self.config_mut()?;
		if let Some(existing) =
			config.skills.iter().find(|s| s.name == skill.name).cloned()
		{
			// Classify the already-installed state before deciding to error.
			let safe = sanitize_name(&skill.name);
			let canonical = canonical_dir.join(&safe);
			// The "agent reads the Master directly" branch lived here. It is
			// gone with `LinkNeed::NativeReader`: nothing reads the `.aghub`
			// store, so idempotence is decided by the link below and by nothing
			// else. Re-adding for an agent whose Referrer already resolves to
			// this Master is the no-op; anything else at that slot is a conflict.
			// (a) Correct link already exists at the agent slot (AlreadyLinked).
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

		// Classify + link via the ONE shared materializer. The Master already
		// exists (written above), so the materializer's copy branch is skipped
		// and only the unified classify-then-link logic runs — the SAME code the
		// fetched/desktop path uses, so the two can no longer diverge.
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

		// Deliberately NOT `save_current()`: `save()` serializes MCPs and
		// nothing else, so it cannot persist a skill — the on-disk work is already
		// done above. Calling it here rewrote the agent's MCP config from the
		// normalized model as a pure side effect, dropping per-server fields aghub
		// does not model. See `set_skill_enabled` for the same defect in its
		// starkest form.
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
		// The ONLY guarded mutation that does not `guard_and_reload`, and that is a
		// known gap, not a decision: re-reading here regressed two rename tests on
		// macOS ONLY ("the referrer must resolve to the master" — the relink lands
		// somewhere that does not resolve). The paths this flow renames come from
		// the reloaded entry, and on macOS a filesystem-resolved path is
		// `/private/var/...` where the caller's root is `/var/...`; something in
		// that pair breaks the relink. It could not be reproduced on Linux — a
		// project root that is itself a symlink still passes — so the re-read is
		// withheld until the macOS behaviour is understood rather than guessed at.
		// The stale-view window Codey found therefore stays OPEN here (it is closed
		// in add / add-from-path / remove / remove-planned); see
		// docs/specs/2026-07-29-skill-mutation-interprocess-lock.md.
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

	pub fn remove_skill(&mut self, name: &str) -> Result<()> {
		// Whether the CALLER's view held it, recorded before the re-read below.
		// After that re-read "another process removed it while we waited for the
		// lock" and "there is no such skill" are indistinguishable, and they need
		// opposite answers: the first is this removal's goal already met, the
		// second is a real not-found.
		let was_in_callers_view = self
			.config
			.as_ref()
			.is_some_and(|c| c.skills.iter().any(|s| s.name == name));

		// Guarded like every other disk mutation: this unlinks a Referrer or
		// deletes a private copy. It may NOT take a shared Master — see the
		// `single_agent_keep_reason` guard below; `remove_skill_planned` is the
		// layout-aware seam that can. Re-reads config
		// under the lock because the PATHS it deletes come from the entry
		// (`source_path` / `canonical_path`), and a stale entry points at whatever
		// another process has since put there.
		let _mutation_guard =
			self.guard_and_reload("remove skill", self.scope)?;

		let target_dir = self.target_skills_dir();
		let agent_name = self.adapter.name().to_string();
		// Allow-listed roots for the containment guard, plus the agent dirs the
		// referrer guard sweeps. Both computed before the mutable borrow below.
		let project_root = self.project_root.clone();
		let all_agent_dirs = crate::skills::removal::agent_skill_dirs_in_scope(
			self.scope,
			project_root.as_deref(),
		);
		let roots = crate::skills::removal::allowed_skill_roots(
			&all_agent_dirs,
			project_root.as_deref(),
		);
		let config = self.config.as_ref().ok_or_else(|| {
			ConfigError::InvalidConfig("No configuration loaded".to_string())
		})?;
		let Some(index) = config.skills.iter().position(|s| s.name == name)
		else {
			// Absent under the lock. Skills live on disk (a Master plus symlinks),
			// never in an agent's JSON, so there is nothing left to write out —
			// the same already-gone tolerance the path removal below applies.
			return if was_in_callers_view {
				Ok(())
			} else {
				Err(ConfigError::resource_not_found("skill", name))
			};
		};
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
				// This is a PUBLIC seam whose `!is_link` branch ends in
				// `remove_dir_all`, and the entry it deletes comes from
				// discovery: an agent that reads `.agents/skills` directly gets
				// the shared Master with `canonical_path = None`, so without
				// this the seam ate a shared Master and returned Ok. The
				// single-agent rule is `plan_copy_removal`'s, shared verbatim
				// rather than restated — a `dir_has_external_referrer`-only
				// version of it let a Master with no symlinks but ~10 other
				// NativeReaders through, which is silent data loss with no
				// dangling link left behind to notice it by.
				//
				// Nothing legitimately deletes a Master through here: this is
				// single-agent by construction, and every project Master has
				// NativeReaders. Layout-aware removal that may take one is
				// `remove_skill_planned`, per this method's doc.
				if !is_link {
					if let Some(dir) = path.parent() {
						if crate::skills::removal::single_agent_keep_reason(
							dir,
							&all_agent_dirs,
							name,
							project_root.as_deref(),
						)
						.is_some()
						{
							return Err(ConfigError::unsupported_operation(
								"remove for this agent alone",
								"shared universal master (or a folder another \
								 agent's link still points at) — use \
								 remove_skill_planned",
								&agent_name,
							));
						}
					}
				}
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
		// Not `save_current()` — it only serializes MCPs, so it cannot persist
		// a skill and rewrites `.mcp.json` as a side effect. See `add_skill`.
		Ok(())
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

		// Taken FIRST on any executing path — before the target is even looked up,
		// let alone planned. Everything downstream is a decision about what to
		// delete: `skill_for_planned_removal` picks the target, and `plan_removal`
		// runs the referrer sweep that decides whether a shared Master may go.
		// Reading any of that outside the lock and deleting inside it means acting
		// on a view another process has already invalidated (it links a new
		// Referrer to that Master, or replaces the same-name skill, and we delete
		// its work anyway). A dry-run mutates nothing and deliberately takes none.
		// `guard_and_reload` because the target is chosen from `self.config`, which
		// the caller loaded before this method — see it for why the two are one
		// step.
		let _mutation_guard = if dry_run {
			None
		} else {
			Some(self.guard_and_reload("remove skill", self.scope)?)
		};

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

		// Refuse rather than report a removal that cannot happen — and ask
		// DISCOVERY whether it can, not the plan. `plan.shared_master_kept &&
		// plan.paths.is_empty()` used to stand in for the question and answered
		// it wrong twice over: an agent that reads its own dir AND the Master
		// has a real path to unlink, so the plan looked effective while the
		// Master went on serving the skill; and a skill whose FOLDER name
		// differs from its frontmatter `name` planned NOTHING with
		// `shared_master_kept` false, so `delete --yes` reported `removed` with
		// the skill untouched on disk.
		//
		// This is the ONE home for the verdict. `transfer::reconcile_skill`
		// used to keep its own copy and refuse shapes that the CLI `delete` and
		// the API delete routes — which come through here — reported as
		// `removed`; both now read the same answer.
		// `--all-agents` promises "gone everywhere", so the question widens with
		// it: asking only the INITIATING agent's read dirs let an
		// `all_agents` delete report a clean `removed` while a second agent
		// went on discovering the skill from a layout the planner had missed.
		// Same dirs the planner just swept, so the two cannot disagree unless
		// the planner really did leave something behind.
		let read_dirs = if all_agents {
			all_agent_dirs.clone()
		} else {
			self.adapter
				.get_skills_paths(project_root.as_deref(), scope)
		};
		let effect =
			removal::read_effect_after(&read_dirs, &skill.name, &plan.paths);
		// Survivors alone are NOT the verdict for a single agent: a removal that
		// really shrinks the set of locations this agent reads from DID take
		// something away, even though something else still serves the skill.
		// Requiring zero survivors here made a private copy shadowing a Master
		// undeletable by every verb the product has (`delete`, both API delete
		// routes, `reconcile --remove`) except `--all-agents`, which takes the
		// Master too — and the old behaviour it replaced was not lying, it
		// really did remove that copy. `--all-agents` promises "gone
		// everywhere", so THERE any survivor is a broken promise.
		// A survivor the PLANNER itself kept is not evidence of a broken
		// promise. `plan_copy_removal` keeps the agent's OWN directory when
		// another agent's live link points into it, and the symlink sweep keeps
		// the master when a peer dir could not be swept — in both the planner
		// understood the request, declined to orphan a peer, and SAID so in
		// `skipped`. Refusing on top of that tells the user their delete made no
		// sense AND throws away the one list naming what was missed.
		//
		// EVERY survivor must be accounted for, not just one: a survivor the
		// planner never saw (an npx-era or grouped layout it walked past) is
		// exactly the silent `removed`-that-removed-nothing this guard exists to
		// catch, and it is invisible in `skipped` by construction.
		//
		// Excluded for a shared MASTER, whose keep IS the shape the refusal was
		// written for: "remove for this agent alone" really does take nothing
		// away there. `shared_master_kept` is the planner's own flag, read BEFORE
		// the fold below, so the two cannot be confused.
		let accounted_for = |survivor: &std::path::Path| {
			let survivor =
				crate::skills::linker::classify::canonicalize_lenient(survivor);
			plan.skipped.iter().any(|kept| {
				crate::skills::linker::classify::canonicalize_lenient(kept)
					== survivor
			})
		};
		let all_survivors_reported = !plan.shared_master_kept
			&& effect
				.survivors
				.iter()
				.all(|survivor| accounted_for(survivor));
		// The planner took NOTHING and named everything that survived: it
		// understood the request and spared the entry (another agent's live
		// link points into the caller's own directory).
		let spared_everything = all_survivors_reported
			&& plan.paths.is_empty()
			&& !effect.survivors.is_empty();
		// `--all-agents` asserts a POSTCONDITION ("nothing reads it anymore"),
		// and a read dir that could not be enumerated cannot support one. The
		// single-agent branch deliberately ignores it: that one decides whether
		// to REFUSE, and one odd sibling must not make a skill undeletable.
		let blocks = if all_agents {
			// No narrowing here, deliberately: `--all-agents` promises "gone
			// everywhere", so a survivor is a broken promise whether or not
			// the planner saw it coming. An out-of-tree entry the containment
			// check refuses IS reported in `skipped` and still leaves the
			// skill readable — reporting that as `removed` is the lie. What
			// the refusal owes the caller is the LIST, which it carries below.
			!effect.survivors.is_empty() || effect.incomplete
		} else {
			// `paths.is_empty()` is what separates the two single-agent
			// shapes, and it is not decoration. A plan that removes NOTHING
			// and reports the survivor in `skipped` understood the request and
			// spared the entry (a peer links into the caller's own directory)
			// — the `kept` contract. A plan that removes something and STILL
			// leaves the agent reading the skill is the npx-era Referrer
			// beside its Master: the unlink looks effective, the Master goes
			// on serving it, and reporting that as `removed` is the lie this
			// guard exists to refuse. Both keep the survivor in `skipped`, so
			// `skipped` alone cannot tell them apart.
			!effect.survivors.is_empty()
				&& !effect.changed
				&& !spared_everything
		};

		// A preview must still PREVIEW, so record the fact instead of refusing:
		// `RemovalView` reads this into `kept`, whose contract is "success, and
		// THE ENTITY IS STILL THERE". Only an executing call refuses, which is
		// the contract `single_agent_delete_keeps_shared_master_and_reports_it`
		// pins.
		//
		// This flag is the ONLY producer of `shared_master_kept` with a
		// non-empty `paths`, which is why `RemovalView` can key `kept` off
		// `shared_master_kept && (paths.is_empty() || !executed)` without a new
		// field: a layout with a real path to unlink that STILL leaves the
		// agent reading the skill (an npx-era Referrer beside the Master it
		// points at) previews as `kept` instead of promising a `--yes` that only
		// ever answers `unsupported_operation`. Keep the two in step — a preview
		// must never green-light what the line below refuses. Fold `blocks`, not
		// raw survivors: folding survivors would mark the ALLOWED private-copy
		// removal `kept` while `--yes` went ahead and removed it.
		plan.shared_master_kept |= blocks;

		// A removal that goes ahead while something else still serves the skill
		// has to SAY so. `skipped` is the existing carrier for "present and
		// deliberately not taken", already rendered by the CLI and serialized by
		// both surfaces, so the honest half of the old behaviour costs no wire
		// change: the answer becomes "removed <private copy>, and this agent now
		// reads it from <master>" instead of a bare `removed`.
		if effect.changed {
			for survivor in &effect.survivors {
				if !plan.skipped.contains(survivor) {
					plan.skipped.push(survivor.clone());
				}
			}
		}

		// Name WHERE, computed ONCE and carried on the plan so every surface
		// reads the same answer. A bare "still discoverable somewhere" is the
		// dead end `skipped` was added to avoid: one place was not taken, and
		// the only thing the caller can act on is which one. Survivors are what
		// still hands out the skill; `skipped` adds what the sweep could not
		// even look at (an agent dir it failed to list leaves no survivor to
		// report, precisely because it could not be read).
		//
		// Set AFTER the `effect.changed` fold above, so `skipped` is complete.
		// Order-preserving `contains` rather than `dedup()`: that fold may have
		// pushed the survivors into `skipped` already, and `dedup()` drops only
		// CONSECUTIVE repeats, so survivors-then-skipped printed each one twice.
		if blocks {
			let mut still: Vec<std::path::PathBuf> = Vec::new();
			for path in effect.survivors.iter().chain(plan.skipped.iter()) {
				if !still.contains(path) {
					still.push(path.clone());
				}
			}
			plan.still_read_from = still;
		}

		if executed && blocks {
			let where_ = plan
				.still_read_from
				.iter()
				.map(|path| path.display().to_string())
				.collect::<Vec<_>>()
				.join(", ");
			// The `--all-agents` refusal is a DIFFERENT failure — the sweep left
			// a copy behind somewhere — and "remove for this agent alone …
			// reads from the shared master" describes neither the request the
			// user made nor the reason, leaving them nothing to act on.
			let (operation, reason) = if all_agents {
				(
					"remove from every agent".to_string(),
					format!("skill still discoverable afterwards in: {where_}"),
				)
			} else {
				// The single-agent refusal had the SAME dead end and did not get
				// the same treatment: it named the other AGENTS reading the
				// Master while staying silent about the paths, so the one thing
				// the user could act on — a leftover Referrer in this agent's
				// own second read dir — was invisible. Observed on antigravity,
				// whose write slot moved while `.gemini/antigravity/skills` kept
				// a link the planner never schedules.
				let reason = if where_.is_empty() {
					"skill it reads from the shared master".to_string()
				} else {
					format!(
						"skill it reads from the shared master; it is still \
						 served to this agent from: {where_}"
					)
				};
				("remove for this agent alone".to_string(), reason)
			};
			return Err(ConfigError::unsupported_operation(
				&operation,
				&reason,
				self.adapter.name(),
			));
		}

		// The SECOND disjunct is the planner's OWN keep, and it must land here
		// rather than in `commit` for the same reason the first one does. The
		// planner sets `shared_master_kept` when an exhaustive sweep finished
		// with nothing to take and something reported in `skipped`, and `blocks`
		// cannot always see that: `read_effect_after` stops at a directory whose
		// root `SKILL.md` parses (`collect_skills`), while the planner's own
		// sweep recurses into it (`collect_entry_paths`), so a broken link
		// NESTED inside an unrelated healthy skill folder makes the planner keep
		// the Master while leaving `effect.incomplete` false and `survivors`
		// empty. That fell through to `commit`, which reported `kept` with
		// `executed: true` AND ran a scope-wide lock GC the preview had just
		// promised would not run (`RemovalOutcome::preview` gates its prune
		// disclosure on exactly this flag) — dropping an unrelated skill's
		// source provenance on a delete that removed nothing.
		if spared_everything
			|| (all_agents && plan.shared_master_kept && plan.paths.is_empty())
		{
			// NOT `commit`, even for a confirmed call. `commit` sets
			// `executed: true` for the whole branch, so a run that removed
			// nothing serialized as `outcome: "removed"` with the skill still
			// on disk — and its lock prune dropped the entry for a skill that
			// is still installed, taking the source provenance with it.
			//
			// `shared_master_kept` is what `RemovalView` keys `kept` off, and
			// that is what this IS: kept because something else still holds
			// the skill, or because the sweep could not prove nothing does.
			plan.shared_master_kept = true;
			return removal::RemovalOutcome::preview(
				plan,
				true,
				scope,
				project_root.as_deref(),
				name,
			);
		}

		if !executed {
			// Disclose the scope-wide lock prune the COMMIT would run, through
			// the ONE producer both surfaces use — handing it `blocks`, the
			// SAME flag the refusal a few lines above reads, so a preview can
			// never promise a prune the commit will refuse to perform.
			return removal::RemovalOutcome::preview(
				plan,
				blocks,
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
		let outcome = removal::RemovalOutcome::commit(
			plan,
			&roots,
			scope,
			project_root.as_deref(),
			name,
		)?;

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

			// A Master without Referrers is invisible to every agent. Source
			// cleanup still owns it: otherwise it reports `absent` while both
			// the bytes and the lock entry survive. Discovery answers "which
			// Master is this" — never a `dir.join(name)` guess — so a Master
			// whose FOLDER name differs from its frontmatter `name` is still
			// found, and aghub's own bookkeeping in the store is still skipped.
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
			for store in stores {
				// FAIL CLOSED on a store this cannot fully read, and note what
				// that rules out: an entry whose `SKILL.md` will not open has
				// an UNKNOWN frontmatter name, so it may be this very skill —
				// under a folder name that is not `sanitize_name(name)`, which
				// is a shape aghub supports. Trusting the partial list there
				// buys one convenience (an unreadable sibling no longer fails
				// an unrelated delete) and sells three lies for it: a hidden
				// same-name Master slips past the duplicate refusal below, an
				// unreadable non-canonical Master answers `absent` with its
				// bytes still on disk, and a probe of the canonical slot alone
				// cannot see either. `load_master_skills` is `Err`-or-nothing
				// on purpose; the error names the path, which is the whole
				// remedy.
				let mut found = load_master_skills(&store)?
					.into_iter()
					.filter(|skill| skill.name == name);
				match (found.next(), found.next()) {
					(Some(mut skill), None) => {
						if skill.canonical_path.is_none() {
							skill.canonical_path = skill.source_path.clone();
						}
						return Ok(skill);
					}
					// Two live Masters under one frontmatter name. `find()`
					// would pick by `read_dir` order and delete one of them
					// arbitrarily; `resync::refuse_conflicting_copy` already
					// refuses this shape, and a DELETE has even less licence
					// to guess.
					(Some(first), Some(second)) => {
						return Err(ConfigError::InvalidConfig(format!(
							"skill '{name}' has two Masters in {}: '{}' and \
							 '{}'. Remove or rename one of them, then re-run.",
							store.display(),
							first.source_path.as_deref().unwrap_or("<unknown>"),
							second
								.source_path
								.as_deref()
								.unwrap_or("<unknown>"),
						)));
					}
					(None, _) => {}
				}
			}
		}

		Err(ConfigError::resource_not_found("skill", name))
	}

	/// Refuse: there is no writer for a skill's enabled flag.
	///
	/// `save()` serializes MCPs and nothing else, so flipping `Skill::enabled`
	/// in memory was dropped on the floor — while `save_current()` rewrote the
	/// agent's MCP config as a side effect, stripping unknown per-server fields
	/// from `.mcp.json`. A silent no-op that damages an unrelated file is worse
	/// than an honest refusal.
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
		// Symlink-only model (Locked Decision 1): every install-from-path writes
		// a single .agents/skills/<name> Master and links THIS agent to it. The
		// old isolated-copy body is removed; there is no copy install path.
		self.add_skill_from_path_universal(path, None)
	}

	/// Symlink-only install from a local path (Locked Decision 1): parses the
	/// skill then writes the real source tree once into
	/// `.agents/skills/<name>` (canonical) and symlinks THIS agent's skills
	/// dir to it. Both [`Self::add_skill_from_path`] and [`Self::add_skill`]
	/// now delegate here; `--universal` is a deprecated no-op. The full
	/// source tree (`assets/`, `scripts/`, `examples/`, etc.) is preserved
	/// — matching the API path's `install_git_skill_universal` behaviour.
	///
	/// `as_name` installs the source under a name of the caller's choosing (CLI
	/// `add --from <path> --name <new>`). It is applied BEFORE the duplicate
	/// check, so the whole install is ONE step inside ONE lock span. It used to
	/// be import-then-`update_skill`-rename in the CLI, which released the lock
	/// between the two halves (another process could swap the Master out from
	/// under the rename) and stranded the imported skill when the rename failed.
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
			// below: those report success having written NOTHING, which for a
			// caller who pointed at a specific source silently discards it. A
			// hard refusal BEFORE any write is also what makes this flow need
			// no rollback — there is nothing half-installed to undo.
			if as_name.is_some() {
				return Err(ConfigError::resource_exists("skill", &skill.name));
			}
			let safe = sanitize_name(&skill.name);
			let canonical = canonical_dir.join(&safe);
			// Both no-op branches below report the INSTALLED skill, never the
			// one just parsed from `path`. The install writes nothing when the
			// Master already exists, so returning the parsed copy claimed an
			// overwrite that never happened — `add --from` printed the source
			// file's frontmatter while disk still held the old Master.
			let installed = || {
				config
					.skills
					.iter()
					.find(|s| s.name == skill.name)
					.cloned()
					.unwrap_or_else(|| skill.clone())
			};
			// The NativeReader branch is gone with the variant. Its hard-won
			// lesson survives in the check below: idempotence is decided by the
			// RESOLVED link target, never by the skill name. Deciding on the name
			// once reported `already_installed` for an agent holding an unrelated
			// same-named skill, and paired with a `reconcile --remove` — whose
			// gate only asks whether the copy ERRORED — it silently deleted the
			// source: the copy did nothing, the delete ran, the content was gone.
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

		// The copy carried the SOURCE's SKILL.md verbatim, so its frontmatter
		// still says `name: <source_name>` — and discovery reads that field,
		// not the folder name (see `skills::discovery`), so without this the
		// renamed install would be rediscovered under the source's name.
		//
		// Gated on the materializer's OWN receipt: rewriting a Master this call
		// did NOT create edits a folder someone else owns, and with it the
		// folder hash the npx lock contract is checked against. `created_master`
		// is an atomic create-claim, not an exists-probe, so it is safe to trust
		// here (crates/core/AGENTS.md "Mutation attribution").
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
		// What is ON DISK. `created_master` is the materializer's own receipt:
		// it is false exactly when a pre-existing Master was preserved, and then
		// the source we parsed was never written.
		let on_disk = if results.created_master {
			fs_skill.clone()
		} else {
			// Say so. The from-struct sibling has warned about this since it was
			// written; this path said nothing at all, so a user re-adding an
			// edited source saw only a success.
			warn!(
				"canonical '{}' already exists; reusing it without overwriting \
				 SKILL.md — the skill reported below is the MASTER on disk, not \
				 the source just read (use `aghub update` to refresh content)",
				canonical.display()
			);
			match master_on_disk(&canonical, &skill.name) {
				Some(found) => found,
				// Do NOT fall back to the parsed source. The warning above has
				// just promised the caller that what follows is the master on
				// disk, and nothing can read a master that does not parse — a
				// `get skills` right after this returns nothing at all. Undo the
				// referrer and say what is wrong.
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

		// Report the entry as it now exists ON DISK, like `add_skill_universal`
		// does — never the parsed source, whose `source_path` still names the
		// caller's input directory. The CLI used to paper over this by re-reading
		// config after its rename step; with the rename gone the wrong paths
		// would have reached `--json` verbatim.
		//
		// Not `save_current()` — see `add_skill`.
		Ok(SkillAdd::installed(on_disk, &results))
	}

	/// Map the single-agent result from `materialize_universal_master` onto the
	/// CLI add path's historical error contract.
	///
	/// The add path linked ONLY a `NeedsLink` agent and errored when that link
	/// hit a real foreign occupant (the old `report.conflicts` check) or a hard
	/// link failure. A `NativeReader` reads the Master directly and is never an
	/// error. An `Unsupported` agent no longer reaches this helper on the add
	/// path: `materialize_universal_master` rejects it in preflight (hard
	/// error, nothing written) before per-agent results exist — its arm here
	/// is defensive only. So enforce the error-free result ONLY for a
	/// `NeedsLink` agent.
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
	/// directory at this scope.
	///
	/// Replaces `skill_target_is_native_reader`, whose question ("does this agent
	/// already see the Master without a link") has no answer any more — nothing
	/// reads the store. The question that took its place is the one a user
	/// actually needs answered before granting: who else does this reach.
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
	// A per-agent conflict or failure is DATA to an INSTALL (the caller reports
	// it as an "already covered" row) and a hard FAILURE to a RENAME: the
	// old-name link was unlinked a few lines up, so a referrer that did not get
	// its new-name link now has no link at all — the skill vanishes for that
	// agent while the rename returns Ok. Erroring hands `rename_skill_master`
	// its rollback, which puts the master and the old-name links back. The
	// check belongs HERE and not in `link_agents_to_canonical`: that primitive
	// is shared with the install paths, which legitimately consume the rows.
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
		// Same reason as the forward relink: a per-agent row that is `Ok` but
		// not LINKED still leaves that referrer missing, and here it is the
		// rollback's own promise ("the old-name symlinks are restored") that
		// would be false.
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
/// Without this, every `update_skill` — a `--description` edit, and since the
/// rename went transactional a plain rename too — reserialized the file from a
/// model that has no `license` / `compatibility` / anything else, silently
/// deleting fields the author wrote. The MODELED keys are stripped rather than
/// merged, so clearing one (`author: None`) still clears it.
///
/// Best-effort by design: an unreadable or frontmatter-less file yields an
/// empty map, i.e. exactly the previous behaviour, so this can never turn a
/// working update into a failure.
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
