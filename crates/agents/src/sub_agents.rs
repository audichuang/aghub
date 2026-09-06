//! File-system helpers for agents that store sub-agents as markdown files.
//!
//! These functions are only relevant for agent implementations that persist
//! sub-agent definitions in a directory (e.g. Claude, OpenCode).  Generic
//! infrastructure lives in `descriptor.rs`; only the markdown-file-based I/O
//! strategy lives here.
//!
//! Two on-disk layouts exist in the wild, both handled by [`SubAgentLayout`]:
//! one file per agent (`<name>.md`, or Copilot's `<name>.agent.md`) and one
//! DIRECTORY per agent holding a fixed file (Antigravity's `<name>/agent.md`).
//! The layout is a descriptor-declared fact, not a per-call-site guess, so the
//! read filter, the name extraction and the write filename can never drift
//! apart — writing `<name>.md` where the vendor only looks for `<name>.agent.md`
//! is invisible to any aghub-side round-trip assertion.

use crate::descriptor::{OptionalPathFn, OptionalProjectPathFn};
use crate::errors::{ConfigError, Result};
use crate::models::{ResourceScope, SubAgent};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

// ── On-disk layout ───────────────────────────────────────────────────────────

/// How an agent lays out sub-agent definitions inside its agents dir.
///
/// The variants exist because the difference is not cosmetic: it decides the
/// read filter, the name, AND the filename written. Getting only one of the
/// three right produces files aghub round-trips with itself and the vendor
/// never sees.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubAgentLayout {
	/// One file per sub-agent, named `<name><suffix>`. `.md` for Claude, Grok
	/// and OpenCode; `.agent.md` for Copilot, whose docs state the identity is
	/// "the configuration file's name (minus `.md` or `.agent.md`)".
	Flat { suffix: &'static str },
	/// One DIRECTORY per sub-agent holding a fixed file — Antigravity's
	/// `<name>/agent.md`, mirroring how its skills are `<name>/SKILL.md`.
	///
	/// `source_path` stays the INNER FILE so the manager's three file-shaped
	/// operations (stale-file `remove_file` on rename, the
	/// `with_extension("md.aghub-tomb")` tombstone, and the tombstone cleanup)
	/// keep working unchanged against a directory layout.
	// ponytail: a delete or rename leaves the now-empty `<name>/` dir behind.
	// Antigravity skips a dir with no `agent.md` and so does the loader below,
	// so it is litter, not data loss. Teach the manager to prune the parent only
	// if users actually complain about the empty dirs.
	Nested { file_name: &'static str },
}

impl SubAgentLayout {
	/// The layout every pre-existing caller used.
	pub const MARKDOWN: Self = Self::Flat { suffix: ".md" };

	/// The sub-agent name a directory entry carries, or `None` when the entry
	/// is not a sub-agent in this layout.
	fn name_of(&self, file_name: &str) -> Option<String> {
		match self {
			// `strip_suffix` (not `file_stem`) — `file_stem` on
			// `reviewer.agent.md` yields `reviewer.agent`.
			Self::Flat { suffix } => file_name.strip_suffix(suffix),
			Self::Nested { .. } => Some(file_name),
		}
		.filter(|name| !name.is_empty())
		.map(str::to_string)
	}

	/// Where a sub-agent called `name` lives under `dir`.
	fn path_in(&self, dir: &Path, name: &str) -> PathBuf {
		match self {
			Self::Flat { suffix } => dir.join(format!("{name}{suffix}")),
			Self::Nested { file_name } => dir.join(name).join(file_name),
		}
	}
}

// ── Frontmatter schema ───────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize, Default)]
struct SubAgentFrontmatter {
	/// `default` because `name` is OPTIONAL in several vendors' schemas —
	/// Copilot's reference says the identity is the file name and the
	/// frontmatter `name` is only a display name. Without the default, a
	/// perfectly valid `description`-only file fails to deserialize and falls
	/// into the "no frontmatter" branch, which puts the raw YAML into the
	/// instruction body. An empty name still falls back to the layout name.
	#[serde(default)]
	pub name: String,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub description: Option<String>,
	/// Every frontmatter key aghub does not model, kept verbatim.
	///
	/// A save rewrites EVERY sub-agent in the directory, not just the edited
	/// one, so without this creating one agent silently strips `tools`,
	/// `model`, `target`, `mcp-servers`, … off all its siblings. Same contract
	/// the MCP serializers hold for keys they do not own.
	#[serde(
		flatten,
		default,
		skip_serializing_if = "serde_yaml::Mapping::is_empty"
	)]
	pub extra: serde_yaml::Mapping,
}

// ── File parsing / formatting ────────────────────────────────────────────────

/// Parse a single sub-agent markdown file, naming it from the path.
///
/// Kept for the `.md` layout's callers; prefer [`parse_sub_agent_file_named`]
/// where the layout already knows the name.
pub fn parse_sub_agent_file(path: &Path) -> std::io::Result<Option<SubAgent>> {
	let fallback = path
		.file_stem()
		.and_then(|n| n.to_str())
		.unwrap_or("unknown")
		.to_string();
	parse_sub_agent_file_named(path, &fallback)
}

/// Parse a single sub-agent markdown file.
///
/// Reads YAML frontmatter (`name`, `description`) using the `aghub-markdown`
/// crate and uses the document body as the instruction.  When the file has no
/// frontmatter (or an empty `name`), `fallback_name` is used — the caller
/// derives it from the LAYOUT, because a file stem is wrong for every layout
/// but the plain `.md` one.
pub fn parse_sub_agent_file_named(
	path: &Path,
	fallback_name: &str,
) -> std::io::Result<Option<SubAgent>> {
	if !is_regular_file(path)? {
		return Ok(None);
	}
	// `.ok()?` here was the level BELOW the one `fe0db092` fixed: an
	// unreadable file read as "no sub-agent by that name", so `transfer`'s
	// already-exists check passed and the write OVERWROTE it. Verified: the
	// same command exits 1 "Resource already exists" at mode 0644 and exits 0
	// `success: true` at mode 0000, having replaced the file's contents.
	let content = match fs::read_to_string(path) {
		Ok(content) => content,
		// Vanished between the stat and the read: gone IS the answer.
		Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
			return Ok(None);
		}
		Err(error) => return Err(at_path(path, error)),
	};
	let stem = fallback_name.to_string();

	Ok(Some(
		match aghub_markdown::parse_opt::<SubAgentFrontmatter>(&content) {
			Ok((Some(front), body)) => SubAgent {
				name: if front.name.is_empty() {
					stem
				} else {
					front.name
				},
				description: front.description,
				instruction: Some(body.to_string()),
				source_path: Some(path.to_string_lossy().into_owned()),
				config_source: None,
			},
			Ok((None, body)) => SubAgent {
				name: stem,
				description: None,
				instruction: Some(body.to_string()),
				source_path: Some(path.to_string_lossy().into_owned()),
				config_source: None,
			},
			Err(_) => SubAgent {
				name: stem,
				description: None,
				instruction: Some(content),
				source_path: Some(path.to_string_lossy().into_owned()),
				config_source: None,
			},
		},
	))
}

/// Format a [`SubAgent`] as markdown with YAML frontmatter.
pub fn format_sub_agent(agent: &SubAgent) -> Result<String> {
	format_sub_agent_preserving(agent, serde_yaml::Mapping::new())
}

/// Read back the frontmatter keys aghub does not own from an existing file.
///
/// Best effort by design: an absent, unreadable or unparsable file simply has
/// nothing to preserve, and must not block the write — losing the extras is bad,
/// refusing to save is worse.
fn unowned_frontmatter(path: &Path) -> serde_yaml::Mapping {
	let Ok(content) = fs::read_to_string(path) else {
		return serde_yaml::Mapping::new();
	};
	match aghub_markdown::parse_opt::<SubAgentFrontmatter>(&content) {
		Ok((Some(front), _)) => front.extra,
		_ => serde_yaml::Mapping::new(),
	}
}

/// Format a [`SubAgent`], re-emitting `extra` alongside the fields aghub owns.
pub fn format_sub_agent_preserving(
	agent: &SubAgent,
	extra: serde_yaml::Mapping,
) -> Result<String> {
	let front = SubAgentFrontmatter {
		name: agent.name.clone(),
		description: agent.description.clone(),
		extra,
	};
	let default_body;
	let body: &str = if let Some(instruction) = &agent.instruction {
		instruction.as_str()
	} else {
		default_body = format!("\n# {}\n\n", agent.name);
		&default_body
	};
	aghub_markdown::render(&front, body)
		.map_err(|e| ConfigError::InvalidConfig(e.to_string()))
}

fn sanitize_filename(name: &str) -> String {
	let mut out = name
		.to_lowercase()
		.chars()
		.map(|c| {
			if c.is_alphanumeric() || c == '-' || c == '_' || c == '.' {
				c
			} else {
				'-'
			}
		})
		.collect::<String>();
	while out.contains("--") {
		out = out.replace("--", "-");
	}
	out.trim_matches('-').to_string()
}

// Symlink hardening is LEAF-level by design: a sub-agent `.md` file (or the
// agents dir) that is itself a symlink is refused, so a planted symlink can't
// leak or overwrite an out-of-tree target. We deliberately do NOT reject
// symlinked *ancestors* — those are benign and outside the tool's control
// (e.g. macOS `/var`→`/private`, a symlinked `$HOME` or project dir), and an
// attacker who controls an ancestor of the user's own config dir has already won.
fn is_regular_file(path: &Path) -> std::io::Result<bool> {
	// NotFound is an answer; every other error means the entry is THERE and we
	// could not look at it. Mapping that to `false` is the same "unreadable
	// reads as absent" mistake, one level down from the dir traversal.
	let meta = match fs::symlink_metadata(path) {
		Ok(meta) => meta,
		Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
			return Ok(false);
		}
		Err(error) => return Err(at_path(path, error)),
	};
	let file_type = meta.file_type();
	Ok(file_type.is_file() && !file_type.is_symlink())
}

/// Name the path in an I/O error.
///
/// `std::io::Error` out of `fs` carries no path, so these failures reached the
/// user as a bare `Permission denied (os error 13)` about a directory they had
/// not asked about — `get mcps` dying on an unreadable `~/.claude/agents` told
/// them nothing they could act on.
fn at_path(path: &Path, error: std::io::Error) -> std::io::Error {
	std::io::Error::new(error.kind(), format!("{}: {error}", path.display()))
}

fn ensure_safe_sub_agent_dir(dir: &Path) -> Result<()> {
	fs::create_dir_all(dir)?;
	let meta = fs::symlink_metadata(dir)?;
	let file_type = meta.file_type();
	if !file_type.is_dir() || file_type.is_symlink() {
		return Err(ConfigError::InvalidConfig(format!(
			"Sub-agent path is not a regular directory: {}",
			dir.display()
		)));
	}
	Ok(())
}

fn assert_safe_destination(file: &Path) -> Result<bool> {
	match fs::symlink_metadata(file) {
		Ok(meta) => {
			let file_type = meta.file_type();
			if !file_type.is_file() || file_type.is_symlink() {
				return Err(ConfigError::InvalidConfig(format!(
					"Refusing to overwrite unsafe sub-agent file: {}",
					file.display()
				)));
			}
			Ok(true)
		}
		Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
		Err(e) => Err(e.into()),
	}
}

fn unique_temp_path(dir: &Path, safe: &str) -> PathBuf {
	let nanos = SystemTime::now()
		.duration_since(UNIX_EPOCH)
		.map(|d| d.as_nanos())
		.unwrap_or_default();
	dir.join(format!(".{safe}.{}.{}.tmp", std::process::id(), nanos))
}

fn write_sub_agent_file(file: &Path, content: &str) -> Result<()> {
	let dir = file.parent().ok_or_else(|| {
		ConfigError::InvalidConfig(format!(
			"Sub-agent file has no parent directory: {}",
			file.display()
		))
	})?;
	let existed = assert_safe_destination(file)?;
	let safe = file
		.file_stem()
		.and_then(|n| n.to_str())
		.unwrap_or("sub-agent");
	let temp = unique_temp_path(dir, safe);
	let mut handle = fs::OpenOptions::new()
		.write(true)
		.create_new(true)
		.open(&temp)?;
	handle.write_all(content.as_bytes())?;
	handle.sync_all()?;
	drop(handle);

	if existed {
		assert_safe_destination(file)?;
		fs::remove_file(file)?;
	}
	if let Err(e) = fs::rename(&temp, file) {
		let _ = fs::remove_file(&temp);
		return Err(e.into());
	}
	Ok(())
}

// ── Directory-level I/O ──────────────────────────────────────────────────────

/// Load sub-agents from a directory of `*.md` files.
///
/// `Err` when the directory EXISTS but cannot be read or traversed. "Absent"
/// and "unreadable" are different answers and this used to return the same
/// empty list for both, which turned an I/O anomaly into a confident
/// `RESOURCE_NOT_FOUND` — and, in the skill loader that made the same mistake,
/// into a silent deletion of a shared master a genuine holder was still
/// reading. A refused sub-agent dir (a symlinked one) stays empty: that is a
/// deliberate policy answer, not a failure to look.
pub fn load_sub_agents_from_dir(dir: &Path) -> Result<Vec<SubAgent>> {
	load_sub_agents_from_dir_with(dir, SubAgentLayout::MARKDOWN)
}

/// Load sub-agents from a directory laid out per `layout`.
///
/// See [`load_sub_agents_from_dir`] for the absent-vs-unreadable contract; it
/// applies identically here.
pub fn load_sub_agents_from_dir_with(
	dir: &Path,
	layout: SubAgentLayout,
) -> Result<Vec<SubAgent>> {
	match fs::symlink_metadata(dir) {
		Ok(meta) => {
			let file_type = meta.file_type();
			// Symlink hardening (see `is_regular_file`): a symlinked agents
			// dir is REFUSED, which is an answer, not an error.
			if !file_type.is_dir() || file_type.is_symlink() {
				return Ok(Vec::new());
			}
		}
		Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
			return Ok(Vec::new());
		}
		Err(error) => return Err(at_path(dir, error).into()),
	}
	let mut agents: Vec<SubAgent> = Vec::new();
	for entry in fs::read_dir(dir).map_err(|e| at_path(dir, e))? {
		// A per-entry error is "could not read", not "not there": with mode
		// 0400 on the dir, `read_dir` succeeds and every stat under it fails.
		let entry = entry.map_err(|e| at_path(dir, e))?;
		let path = entry.path();
		// Match on the FILE NAME, not `extension()`: a `.agent.md` suffix has
		// extension `md`, so an extension filter would also swallow a stray
		// `README.md` and name `reviewer.agent.md` as `reviewer.agent`.
		let Some(name) = path
			.file_name()
			.and_then(|n| n.to_str())
			.and_then(|n| layout.name_of(n))
		else {
			continue;
		};
		if let SubAgentLayout::Nested { .. } = layout {
			// A nested agent is a real DIRECTORY. Screening here rather than
			// letting the inner probe decide is not just tidiness: a stray
			// `notes.md` sitting beside the agent dirs makes the inner
			// `<file>/agent.md` stat fail with ENOTDIR, which is neither
			// NotFound nor Ok, so the whole directory would fail to load
			// because of one unrelated file. `file_type` comes from `lstat`,
			// so a symlinked `<name>` is excluded too — same leaf-level
			// hardening the flat layout gets from `is_regular_file`.
			let file_type = entry.file_type().map_err(|e| at_path(&path, e))?;
			if !file_type.is_dir() || file_type.is_symlink() {
				continue;
			}
		}
		let file = layout.path_in(dir, &name);
		if let Some(agent) = parse_sub_agent_file_named(&file, &name)? {
			agents.push(agent);
		}
	}
	agents.sort_by(|a, b| a.name.cmp(&b.name));
	Ok(agents)
}

/// Write a single sub-agent to `dir` as a `*.md` file.
///
/// The directory is created if absent.
pub fn save_sub_agent_to_dir(dir: &Path, agent: &SubAgent) -> Result<()> {
	save_sub_agent_to_dir_with(dir, agent, SubAgentLayout::MARKDOWN)
}

/// Write a single sub-agent to `dir` using `layout`.
///
/// The directory (and, for a nested layout, the per-agent directory) is created
/// if absent.
pub fn save_sub_agent_to_dir_with(
	dir: &Path,
	agent: &SubAgent,
	layout: SubAgentLayout,
) -> Result<()> {
	ensure_safe_sub_agent_dir(dir)?;
	let safe = sanitize_filename(&agent.name);
	let file = layout.path_in(dir, &safe);
	if let SubAgentLayout::Nested { .. } = layout {
		// The per-agent dir gets the same symlink hardening as its parent: a
		// planted `<name>` symlink must not redirect the write out of tree.
		ensure_safe_sub_agent_dir(&dir.join(&safe))?;
	}
	let extra = unowned_frontmatter(&file);
	write_sub_agent_file(&file, &format_sub_agent_preserving(agent, extra)?)?;
	Ok(())
}

// ── Scoped load / save ───────────────────────────────────────────────────────

/// Project-scope containment: refuse a sub-agent dir that escapes the project
/// root once symlinks are resolved (e.g. an untrusted clone whose `.claude` /
/// `.opencode` is a symlink redirecting reads/writes out of tree). Canonicalizing
/// BOTH sides keeps benign system symlinks safe — macOS `/var`→`/private`, or a
/// symlinked project dir the user chose, resolve consistently on both sides. The
/// deepest existing ancestor is probed so a not-yet-created agents dir is still
/// checked before anything is written. Global scope is intentionally NOT
/// contained: `~/.claude` is the user's own and may legitimately be a symlink.
fn ensure_within_project_root(dir: &Path, project_root: &Path) -> Result<()> {
	let root = project_root.canonicalize().map_err(|e| {
		ConfigError::InvalidConfig(format!("project root unavailable: {e}"))
	})?;
	let mut probe = dir;
	loop {
		match probe.canonicalize() {
			Ok(real) if real.starts_with(&root) => return Ok(()),
			Ok(_) => {
				return Err(ConfigError::InvalidConfig(format!(
					"Refusing sub-agent dir outside the project root: {}",
					dir.display()
				)))
			}
			Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
				match probe.parent() {
					Some(parent) => probe = parent,
					None => {
						return Err(ConfigError::InvalidConfig(format!(
							"Sub-agent dir has no existing ancestor: {}",
							dir.display()
						)))
					}
				}
			}
			Err(e) => return Err(e.into()),
		}
	}
}

/// Load sub-agents from the directory determined by `scope`.
pub fn load_scoped_sub_agents(
	project_root: Option<&Path>,
	scope: ResourceScope,
	global_dir: Option<OptionalPathFn>,
	project_dir: Option<OptionalProjectPathFn>,
) -> Result<Vec<SubAgent>> {
	load_scoped_sub_agents_with(
		project_root,
		scope,
		global_dir,
		project_dir,
		SubAgentLayout::MARKDOWN,
	)
}

/// Load sub-agents from the directory determined by `scope`, using `layout`.
pub fn load_scoped_sub_agents_with(
	project_root: Option<&Path>,
	scope: ResourceScope,
	global_dir: Option<OptionalPathFn>,
	project_dir: Option<OptionalProjectPathFn>,
	layout: SubAgentLayout,
) -> Result<Vec<SubAgent>> {
	match scope {
		ResourceScope::GlobalOnly => {
			let Some(dir) = global_dir.and_then(|f| f()) else {
				return Ok(Vec::new());
			};
			load_sub_agents_from_dir_with(&dir, layout)
		}
		ResourceScope::ProjectOnly => {
			let Some(root) = project_root else {
				return Ok(Vec::new());
			};
			let Some(dir) = project_dir.and_then(|f| f(root)) else {
				return Ok(Vec::new());
			};
			// Don't read sub-agents from a dir that escapes the project root
			// (e.g. an untrusted clone's symlinked `.claude`).
			if ensure_within_project_root(&dir, root).is_err() {
				return Ok(Vec::new());
			}
			load_sub_agents_from_dir_with(&dir, layout)
		}
		ResourceScope::Both => Err(ConfigError::InvalidConfig(
			"Sub-agent load unavailable for Both scope".to_string(),
		)),
	}
}

/// Persist a full sub-agent list to the scoped directory.
///
/// The directory is created if absent.  Files for removed entries are
/// **not** deleted here — that is handled by `remove_sub_agent` in the
/// manager.
pub fn save_scoped_sub_agents(
	project_root: Option<&Path>,
	scope: ResourceScope,
	agents: &[SubAgent],
	global_dir: Option<OptionalPathFn>,
	project_dir: Option<OptionalProjectPathFn>,
) -> Result<()> {
	save_scoped_sub_agents_with(
		project_root,
		scope,
		agents,
		global_dir,
		project_dir,
		SubAgentLayout::MARKDOWN,
	)
}

/// Persist a full sub-agent list to the scoped directory, using `layout`.
pub fn save_scoped_sub_agents_with(
	project_root: Option<&Path>,
	scope: ResourceScope,
	agents: &[SubAgent],
	global_dir: Option<OptionalPathFn>,
	project_dir: Option<OptionalProjectPathFn>,
	layout: SubAgentLayout,
) -> Result<()> {
	let dir = match scope {
		ResourceScope::GlobalOnly => global_dir.and_then(|f| f()),
		ResourceScope::ProjectOnly => {
			project_root.and_then(|root| project_dir.and_then(|f| f(root)))
		}
		ResourceScope::Both => {
			return Err(ConfigError::InvalidConfig(
				"Sub-agent save unavailable for Both scope".to_string(),
			))
		}
	}
	.ok_or_else(|| {
		ConfigError::InvalidConfig(format!(
			"Sub-agent directory unavailable for {:?} scope",
			scope
		))
	})?;
	// Project scope: refuse to write into a dir that escapes the project root
	// (an untrusted clone's symlinked `.claude`/`.opencode`). Global is the
	// user's own and may legitimately be symlinked, so it is not contained.
	if matches!(scope, ResourceScope::ProjectOnly) {
		if let Some(root) = project_root {
			ensure_within_project_root(&dir, root)?;
		}
	}
	for agent in agents {
		save_sub_agent_to_dir_with(&dir, agent, layout)?;
	}
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;
	use tempfile::TempDir;

	#[test]
	fn parse_file_with_frontmatter() {
		let dir = TempDir::new().unwrap();
		// macOS TempDir sits under /var (a symlink to /private/var); canonicalize
		// so the symlink-component guard in parse/save does not reject the path.
		let dir_path = dir.path().canonicalize().unwrap();
		let path = dir_path.join("my-agent.md");
		fs::write(
			&path,
			"---\nname: My Agent\ndescription: does stuff\n---\nDo the thing.",
		)
		.unwrap();

		let agent = parse_sub_agent_file(&path).unwrap().unwrap();
		assert_eq!(agent.name, "My Agent");
		assert_eq!(agent.description, Some("does stuff".to_string()));
		assert_eq!(agent.instruction, Some("Do the thing.".to_string()));
	}

	#[test]
	fn parse_file_without_frontmatter() {
		let dir = TempDir::new().unwrap();
		// Canonicalize to drop the macOS /var symlink (see above).
		let dir_path = dir.path().canonicalize().unwrap();
		let path = dir_path.join("plain.md");
		fs::write(&path, "Just plain text.").unwrap();

		let agent = parse_sub_agent_file(&path).unwrap().unwrap();
		assert_eq!(agent.name, "plain"); // file stem
		assert_eq!(agent.instruction, Some("Just plain text.".to_string()));
	}

	#[test]
	fn roundtrip_save_load() {
		let dir = TempDir::new().unwrap();
		// Canonicalize to drop the macOS /var symlink (see above).
		let dir_path = dir.path().canonicalize().unwrap();
		let agent = SubAgent {
			name: "Test Agent".to_string(),
			description: Some("desc: with colon".to_string()),
			instruction: Some("Do X.".to_string()),
			source_path: None,
			config_source: None,
		};
		save_sub_agent_to_dir(&dir_path, &agent).unwrap();

		let loaded = load_sub_agents_from_dir(&dir_path).unwrap();
		assert_eq!(loaded.len(), 1);
		assert_eq!(loaded[0].name, "Test Agent");
		assert_eq!(loaded[0].description, Some("desc: with colon".to_string()));
		assert_eq!(loaded[0].instruction, Some("Do X.".to_string()));
	}

	fn agent(name: &str) -> SubAgent {
		SubAgent {
			name: name.to_string(),
			description: Some("d".to_string()),
			instruction: Some("Do X.".to_string()),
			source_path: None,
			config_source: None,
		}
	}

	const COPILOT: SubAgentLayout = SubAgentLayout::Flat {
		suffix: ".agent.md",
	};
	const ANTIGRAVITY: SubAgentLayout = SubAgentLayout::Nested {
		file_name: "agent.md",
	};

	// The WRITE side is the one an aghub-only round trip cannot catch: aghub
	// writing `reviewer.md` and reading `reviewer.md` back is green while
	// Copilot, which only looks for `*.agent.md`, sees nothing. So assert the
	// FILENAME on disk, never a round trip.
	#[test]
	fn suffix_layout_writes_the_vendor_filename() {
		let dir = TempDir::new().unwrap();
		save_sub_agent_to_dir_with(dir.path(), &agent("Reviewer"), COPILOT)
			.unwrap();

		assert!(dir.path().join("reviewer.agent.md").is_file());
		assert!(
			!dir.path().join("reviewer.md").exists(),
			"a bare .md is a file the vendor never reads"
		);
	}

	// `file_stem()` on `reviewer.agent.md` is `reviewer.agent`. Every other test
	// in this file writes frontmatter, so only a frontmatter-LESS fixture can
	// fail on the name extraction.
	#[test]
	fn suffix_layout_strips_the_whole_suffix_for_the_name() {
		let dir = TempDir::new().unwrap();
		fs::write(dir.path().join("reviewer.agent.md"), "Just a body.\n")
			.unwrap();

		let loaded =
			load_sub_agents_from_dir_with(dir.path(), COPILOT).unwrap();

		assert_eq!(loaded.len(), 1);
		assert_eq!(loaded[0].name, "reviewer");
	}

	// An extension filter (`extension() == "md"`) would load this as a phantom
	// sub-agent, which then makes `transfer`'s already-exists check refuse a
	// legitimate copy.
	#[test]
	fn suffix_layout_ignores_a_plain_markdown_file() {
		let dir = TempDir::new().unwrap();
		fs::write(dir.path().join("README.md"), "not an agent").unwrap();
		fs::write(dir.path().join("reviewer.agent.md"), "body").unwrap();

		let loaded =
			load_sub_agents_from_dir_with(dir.path(), COPILOT).unwrap();

		assert_eq!(
			loaded.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(),
			vec!["reviewer"]
		);
	}

	// Copilot's reference makes frontmatter `name` OPTIONAL and `description`
	// required. Before `#[serde(default)]` on `name`, such a file failed to
	// deserialize and the raw YAML ended up in the instruction body.
	#[test]
	fn frontmatter_without_a_name_still_parses_as_frontmatter() {
		let dir = TempDir::new().unwrap();
		fs::write(
			dir.path().join("reviewer.agent.md"),
			"---\ndescription: Reviews code\n---\n\nBody here.\n",
		)
		.unwrap();

		let loaded =
			load_sub_agents_from_dir_with(dir.path(), COPILOT).unwrap();

		assert_eq!(loaded[0].name, "reviewer");
		assert_eq!(
			loaded[0].description,
			Some("Reviews code".to_string()),
			"description must come from frontmatter, not be swallowed as body"
		);
		assert_eq!(
			loaded[0].instruction.as_deref().map(str::trim),
			Some("Body here."),
			"the YAML must not leak into the body"
		);
	}

	#[test]
	fn nested_layout_round_trips_through_a_per_agent_directory() {
		let dir = TempDir::new().unwrap();
		let dir_path = dir.path().canonicalize().unwrap();
		save_sub_agent_to_dir_with(
			&dir_path,
			&agent("Code Reviewer"),
			ANTIGRAVITY,
		)
		.unwrap();

		assert!(dir_path.join("code-reviewer/agent.md").is_file());

		let loaded =
			load_sub_agents_from_dir_with(&dir_path, ANTIGRAVITY).unwrap();
		assert_eq!(loaded.len(), 1);
		assert_eq!(loaded[0].name, "Code Reviewer");
		// `source_path` is the INNER FILE so the manager's `remove_file` /
		// `with_extension` operations keep working against a directory layout.
		assert_eq!(
			loaded[0].source_path.as_deref(),
			Some(dir_path.join("code-reviewer/agent.md").to_str().unwrap())
		);
	}

	#[test]
	fn nested_layout_skips_a_directory_without_the_agent_file() {
		let dir = TempDir::new().unwrap();
		fs::create_dir(dir.path().join("empty-shell")).unwrap();
		fs::write(dir.path().join("stray.md"), "not an agent").unwrap();

		let loaded =
			load_sub_agents_from_dir_with(dir.path(), ANTIGRAVITY).unwrap();

		assert!(loaded.is_empty());
	}

	// A save rewrites EVERY agent in the directory, not just the edited one, so
	// without preservation `create` silently strips `tools` / `model` off all
	// the siblings it never touched.
	#[test]
	fn save_preserves_frontmatter_keys_aghub_does_not_model() {
		let dir = TempDir::new().unwrap();
		fs::write(
			dir.path().join("reviewer.agent.md"),
			"---\nname: reviewer\ndescription: old\ntools: [\"read\", \"edit\"]\nmodel: gpt-5.2\n---\n\nBody.\n",
		)
		.unwrap();

		let mut updated = agent("reviewer");
		updated.description = Some("new".to_string());
		save_sub_agent_to_dir_with(dir.path(), &updated, COPILOT).unwrap();

		let text =
			fs::read_to_string(dir.path().join("reviewer.agent.md")).unwrap();
		assert!(
			text.contains("description: new"),
			"aghub-owned field updates"
		);
		assert!(
			text.contains("model: gpt-5.2"),
			"unowned key must survive, got:\n{text}"
		);
		assert!(
			text.contains("read") && text.contains("edit"),
			"unowned list must survive, got:\n{text}"
		);
	}

	#[test]
	fn sanitize_filename_basic() {
		assert_eq!(sanitize_filename("My Agent!"), "my-agent");
		let result = sanitize_filename("hello world");
		assert!(!result.contains(' '));
	}

	#[cfg(unix)]
	#[test]
	fn load_ignores_symlinked_markdown_files() {
		use std::os::unix::fs::symlink;

		let dir = TempDir::new().unwrap();
		let target = dir.path().join("secret.md");
		let agents_dir = dir.path().join("agents");
		fs::create_dir(&agents_dir).unwrap();
		fs::write(&target, "TOP_SECRET").unwrap();
		symlink(&target, agents_dir.join("leak.md")).unwrap();

		let loaded = load_sub_agents_from_dir(&agents_dir).unwrap();

		assert!(loaded.is_empty());
	}

	#[cfg(unix)]
	#[test]
	fn save_refuses_to_overwrite_symlinked_markdown_files() {
		use std::os::unix::fs::symlink;

		let dir = TempDir::new().unwrap();
		let target = dir.path().join("victim.md");
		fs::write(&target, "ORIGINAL").unwrap();
		symlink(&target, dir.path().join("evil.md")).unwrap();
		let agent = SubAgent {
			name: "evil".to_string(),
			description: None,
			instruction: Some("OVERWRITE".to_string()),
			source_path: None,
			config_source: None,
		};

		let result = save_sub_agent_to_dir(dir.path(), &agent);

		assert!(result.is_err());
		assert_eq!(fs::read_to_string(&target).unwrap(), "ORIGINAL");
	}

	// Regression: a symlinked ANCESTOR of the agents dir (e.g. macOS
	// `/var`→`/private`, or a symlinked `$HOME`) must NOT break sub-agent I/O.
	// The earlier whole-absolute-path symlink walk rejected this and turned the
	// macOS CI red; protection is leaf-level, so an ancestor symlink is fine.
	#[cfg(unix)]
	#[test]
	fn works_through_symlinked_ancestor_dir() {
		use std::os::unix::fs::symlink;

		let root = TempDir::new().unwrap();
		let real = root.path().join("real");
		fs::create_dir(&real).unwrap();
		// `link` is a symlinked ancestor; the agents dir lives beneath it.
		let link = root.path().join("link");
		symlink(&real, &link).unwrap();
		let agents_dir = link.join("agents");

		let agent = SubAgent {
			name: "Ancestor Agent".to_string(),
			description: Some("d".to_string()),
			instruction: Some("body".to_string()),
			source_path: None,
			config_source: None,
		};

		// Save must succeed despite the symlinked ancestor, and load back.
		save_sub_agent_to_dir(&agents_dir, &agent).unwrap();
		let loaded = load_sub_agents_from_dir(&agents_dir).unwrap();
		assert_eq!(loaded.len(), 1);
		assert_eq!(loaded[0].name, "Ancestor Agent");
	}

	// Project-scope containment: a symlinked `.claude` escaping the project
	// (untrusted-clone write-escape) must be refused.
	#[cfg(unix)]
	#[test]
	fn within_root_rejects_symlinked_config_dir() {
		use std::os::unix::fs::symlink;

		let tmp = TempDir::new().unwrap();
		let outside = tmp.path().join("outside");
		fs::create_dir(&outside).unwrap();
		let project = tmp.path().join("project");
		fs::create_dir(&project).unwrap();
		symlink(&outside, project.join(".claude")).unwrap();

		let agents_dir = project.join(".claude/agents");
		let err =
			ensure_within_project_root(&agents_dir, &project).unwrap_err();
		assert!(matches!(err, ConfigError::InvalidConfig(_)));
	}

	// A real `.claude` (even before the agents dir exists) is allowed.
	#[test]
	fn within_root_allows_real_config_dir() {
		let tmp = TempDir::new().unwrap();
		let project = tmp.path().join("project");
		fs::create_dir_all(project.join(".claude")).unwrap();
		let agents_dir = project.join(".claude/agents"); // not yet created
		assert!(ensure_within_project_root(&agents_dir, &project).is_ok());
	}

	// macOS-safety: reaching the project root THROUGH a symlink (mimics
	// `/var`→`/private`, or a symlinked project dir) must still be allowed —
	// both sides canonicalize consistently.
	#[cfg(unix)]
	#[test]
	fn within_root_allows_symlinked_project_root() {
		use std::os::unix::fs::symlink;

		let tmp = TempDir::new().unwrap();
		let real_project = tmp.path().join("real");
		fs::create_dir_all(real_project.join(".claude")).unwrap();
		let link_root = tmp.path().join("link");
		symlink(&real_project, &link_root).unwrap();

		let agents_dir = link_root.join(".claude/agents");
		assert!(ensure_within_project_root(&agents_dir, &link_root).is_ok());
	}
}
