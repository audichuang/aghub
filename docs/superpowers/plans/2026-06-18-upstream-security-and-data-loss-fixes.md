# Upstream Security & Data-Loss Fixes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Port the five 🔴/🟠 correctness + security fixes that upstream `AkaraChen/aghub` has but our fork (`audichuang/aghub`, ahead by 148 commits) still lacks — without regressing our diverged skill/sources subsystem.

**Architecture:** Each task is an independent, separately-committable fix scoped to one subsystem (core skill import, cc-plugins tarball, agents/codex, api/skills routes). Where upstream's patch collides with our divergence, we **reuse our existing infrastructure** (`install_layout`, `allowed_skill_roots`/`assert_contained`) and port only the security-relevant logic; where it does not collide (Codex I/O, plugin tarball, github.com credential guard) we port upstream's helpers nearly verbatim. Every task is strict TDD: write a failing test that demonstrates the vuln/data-loss, watch it fail, implement the minimal fix, watch it pass, commit.

**Tech Stack:** Rust (hard tabs width 4, 80-col, `cargo clippy -D warnings`), Rocket v0.5 (api), `tar`/`flate2` (cc-plugins), `libc` O_NOFOLLOW (agents unix), ts-rs generated DTOs + prettier (desktop).

**Source of truth:** upstream commits `3ad9f1c` (copy-mode preserve), `52a938c` (tarball), `ffeec65` (codex), `91bd12d` (api skill paths), `2f13f0c` (github credential). merge-base `ca48d93`, upstream `714b971`.

**Cross-task notes:**

- Tasks are independent; recommended execution order is 1→5 (data-loss first, then RCE-level tarball, then the three hardening fixes). Tasks 4 and 5 both touch `crates/api/src/routes/skills.rs` but different functions — commit each separately to keep diffs reviewable.
- Every commit message ends with the trailer `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`.
- Line numbers are as-of plan authoring; if a referenced line has drifted, locate by the quoted function/symbol name (always given).

---

### Task 1: Copy(isolation)-mode skill import preserves full source tree

**Why / design decision:** Our `--universal` install already copies the whole skill tree, but the non-universal (isolation copy) path `add_skill_from_path → add_skill` only writes a synthesized `SKILL.md`, dropping `scripts/`, `references/`, `assets/`, and the original body; `convert_skill` even hard-codes `content: None`. We fix the data loss by **reusing our existing `install_layout` recursive copy** (npx-equivalent, symlink-deref, excludes `.git`/`__pycache__`) rather than porting upstream's parallel `copy_dir_recursive`/`staged_import_dir`/`SkillImportSource` (which would create a second drifting copy stack). Scope: local directory and `SKILL.md` sources only (our `add_skill_from_path` contract; `.skill`/zip is not an entry point here). `convert_skill` keeping `skill_pkg.content` is the only verbatim port — it IS the bug fix.

**Files:**

- Modify: `crates/core/src/lib.rs` (lines 53–67, `convert_skill` — `content: None` → keep `skill_pkg.content`)
- Modify: `crates/core/src/manager/skill.rs` (lines 526–538, `add_skill_from_path` — copy full source tree + rewrite `source_path`)
- Test: `crates/core/tests/test_agent_paths.rs` (append two tests + one helper)

- [ ] **Step 1: Write the failing test (directory source).** Append to the end of `crates/core/tests/test_agent_paths.rs`:

```rust
fn write_import_skill_with_resources(dir: &Path, name: &str, body: &str) {
	std::fs::create_dir_all(dir).unwrap();
	std::fs::write(
		dir.join("SKILL.md"),
		format!(
			"---\nname: {name}\ndescription: imported skill\n---\n\n{body}\n"
		),
	)
	.unwrap();
	std::fs::create_dir_all(dir.join("scripts")).unwrap();
	std::fs::create_dir_all(dir.join("references")).unwrap();
	std::fs::create_dir_all(dir.join("assets")).unwrap();
	std::fs::write(dir.join("scripts/setup.sh"), "echo setup").unwrap();
	std::fs::write(dir.join("references/guide.md"), "# Guide").unwrap();
	std::fs::write(dir.join("assets/logo.txt"), "logo").unwrap();
}

#[test]
fn skill_import_directory_preserves_body_and_resources() {
	let test =
		aghub_core::testing::TestConfig::new(aghub_core::AgentType::Claude)
			.unwrap();
	let source_dir = test.temp_dir().join("source/imported-skill");
	write_import_skill_with_resources(
		&source_dir,
		"imported-skill",
		"# Real imported instructions",
	);

	let mut manager = test.create_manager();
	manager.load().unwrap();
	let imported = manager.add_skill_from_path(&source_dir).unwrap();

	assert_eq!(imported.name, "imported-skill");
	assert!(imported
		.content
		.as_deref()
		.unwrap()
		.contains("# Real imported instructions"));

	let target_dir = test.skills_dir().join("imported-skill");
	let target_content =
		std::fs::read_to_string(target_dir.join("SKILL.md")).unwrap();
	assert!(target_content.contains("# Real imported instructions"));
	assert!(target_dir.join("scripts/setup.sh").exists());
	assert!(target_dir.join("references/guide.md").exists());
	assert!(target_dir.join("assets/logo.txt").exists());

	let mut reloaded = test.create_manager();
	reloaded.load().unwrap();
	let loaded = reloaded.get_skill("imported-skill").unwrap();
	assert!(loaded.source_path.as_deref().unwrap().contains("SKILL.md"));
}
```

If the test file lacks `use std::path::Path;` at the top, add it.

- [ ] **Step 2: Write the failing test (SKILL.md file source).** Append immediately after:

```rust
#[test]
fn skill_import_skill_md_file_copies_sibling_resources() {
	let test =
		aghub_core::testing::TestConfig::new(aghub_core::AgentType::Claude)
			.unwrap();
	let source_dir = test.temp_dir().join("source/md-skill");
	write_import_skill_with_resources(
		&source_dir,
		"md-skill",
		"# Body from SKILL.md path",
	);

	let mut manager = test.create_manager();
	manager.load().unwrap();
	let imported = manager
		.add_skill_from_path(&source_dir.join("SKILL.md"))
		.unwrap();

	assert_eq!(imported.name, "md-skill");
	let target_dir = test.skills_dir().join("md-skill");
	assert!(target_dir.join("scripts/setup.sh").exists());
	assert!(target_dir.join("assets/logo.txt").exists());
	let target_content =
		std::fs::read_to_string(target_dir.join("SKILL.md")).unwrap();
	assert!(target_content.contains("# Body from SKILL.md path"));
}
```

- [ ] **Step 3: Run tests, confirm RED.**

Run: `cargo test -p aghub-core --test test_agent_paths skill_import_ -- --exact --nocapture`
Expected: FAIL — `imported.content` is `None` so `.unwrap()` panics, and/or `assert!(target_dir.join("scripts/setup.sh").exists())` fails (current `add_skill` writes only a synthesized SKILL.md).

- [ ] **Step 4: Fix `convert_skill` to keep the body.** In `crates/core/src/lib.rs`, before constructing `models::Skill`, derive `content` and use it:

```rust
	let content = if skill_pkg.content.is_empty() {
		None
	} else {
		Some(skill_pkg.content)
	};

	models::Skill {
		name: skill_pkg.name,
		enabled: true,
		description: Some(skill_pkg.description),
		author: skill_pkg.author,
		version: skill_pkg.version,
		content,
		tools: skill_pkg
```

(Leave the remaining fields unchanged. `source` was already borrowed in the `match &skill_pkg.source` above, so moving `skill_pkg.content` here is fine.)

- [ ] **Step 5: Rewrite `add_skill_from_path` to copy the full tree.** In `crates/core/src/manager/skill.rs`, replace the body of `add_skill_from_path` (lines 526–538) with:

```rust
	pub fn add_skill_from_path(&mut self, path: &Path) -> Result<Skill> {
		debug!(
			"adding skill from path '{}' for agent '{}'",
			path.display(),
			self.adapter.name()
		);
		let skill_pkg = skill::parser::parse(path).map_err(|e| {
			ConfigError::InvalidConfig(format!("Failed to parse skill: {e}"))
		})?;
		let mut skill = convert_skill(skill_pkg);

		let target_dir = self.target_skills_dir().ok_or_else(|| {
			ConfigError::InvalidConfig(
				"Agent does not support persistent skill creation \
				 in the current scope"
					.into(),
			)
		})?;
		let safe_name = sanitize_name(&skill.name);
		let skill_dir = target_dir.join(&safe_name);
		let agent_name = self.adapter.name().to_string();

		{
			let config = self.config_mut()?;
			if config.skills.iter().any(|s| s.name == skill.name) {
				return Err(ConfigError::resource_exists(
					"skill",
					&skill.name,
				));
			}
			if skill_dir.exists() {
				return Err(ConfigError::resource_exists(
					"skill target",
					skill_dir.display().to_string(),
				));
			}
		}

		info!(
			"importing skill '{}' from '{}' for agent '{}'",
			skill.name,
			path.display(),
			agent_name
		);

		// Copy the FULL source tree (scripts/, references/, assets/, original
		// body) into the agent's own skills dir — the isolated copy layout.
		// Reuses install_layout's npx-equivalent recursive copy (symlink-deref,
		// .git/__pycache__ excluded) rather than re-synthesizing a thin
		// SKILL.md, which dropped every non-frontmatter file.
		let source_root = crate::skills::skill_source_root(path);
		crate::skills::install_layout::install_universal(
			&source_root,
			&skill_dir,
			&[],
			false,
		)
		.map_err(ConfigError::Io)?;

		skill.source_path =
			Some(skill_dir.join("SKILL.md").to_string_lossy().to_string());
		skill.canonical_path = None;
		self.config_mut()?.skills.push(skill.clone());
		self.save_current()?;
		Ok(skill)
	}
```

Note: `install_universal(source, canonical, &[], false)` with an empty `agent_skills_dirs` degrades to a pure recursive copy (no symlinks); `canonical_path = None` keeps it classified non-universal so removal takes the copy branch. `sanitize_name`, `convert_skill`, `info`, `config_mut`, `save_current` are all existing imports/methods in this file. **Verify during implementation** that `install_universal`'s real signature matches `(&Path source_root, &Path canonical, &[PathBuf] agent_skills_dirs, bool)` — if the 4th arg or order differs, adjust to the actual signature in `crates/core/src/skills/install_layout.rs`.

- [ ] **Step 6: Run tests, confirm GREEN.**

Run: `cargo test -p aghub-core --test test_agent_paths skill_import_ -- --exact --nocapture`
Expected: PASS — scripts/references/assets present, SKILL.md has the real body, content preserved.

- [ ] **Step 7: Full-crate regression + lint.**

Run: `cargo test -p aghub-core && just lint`
Expected: PASS, including the existing `add_skill_from_path_universal_copies_full_source_tree`; no new clippy warnings.

- [ ] **Step 8: Commit.**

```bash
git add crates/core/src/lib.rs crates/core/src/manager/skill.rs crates/core/tests/test_agent_paths.rs
git commit -m "$(cat <<'EOF'
fix(core): preserve full source tree on copy-mode skill import

add_skill_from_path now copies the entire source skill (scripts/,
references/, assets/, original body) into the agent's own skills dir via
install_layout's recursive copy, instead of writing a thin synthesized
SKILL.md that dropped every non-frontmatter file. convert_skill keeps
skill_pkg.content (None only when empty) so the imported body survives.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 2: Plugin tarball extraction path validation (zip-slip / symlink entry)

**Why / design decision:** `crates/cc-plugins/src/installer/git.rs::extract_tarball` joins archive-supplied paths directly and unpacks with no validation — a malicious tarball with `../` or symlink/hardlink entries can write outside the extraction dir (near-RCE). Port upstream `52a938c`'s self-contained helpers verbatim (they depend only on `std::path::Component`, `tar::EntryType`, `std::fs`). **No error-type change:** our `git.rs` already uses `anyhow::{Context, Result}` and upstream uses `anyhow::bail!` throughout — fully compatible. Rewrite the first iterator-chain collection pass into an explicit `for` loop so each entry path is validated as it is read.

**Files:**

- Modify: `crates/cc-plugins/src/installer/git.rs:5` (`use std::path::Path;` → add `Component, PathBuf`)
- Modify: `crates/cc-plugins/src/installer/git.rs` (after the `GitBasedInstaller` struct, ~line 7 — add `SafeArchivePath` + helpers)
- Modify: `crates/cc-plugins/src/installer/git.rs:91-162` (rewrite `extract_tarball` collection + extraction loops)
- Test: `crates/cc-plugins/src/installer/git.rs` (existing `#[cfg(test)] mod tests`, ~line 196 — add builders + attack tests)

- [ ] **Step 1: Write the failing tests (RED).** In `mod tests`, below `use tempfile::tempdir;`, add the tarball builders and four attack tests:

```rust
	use flate2::write::GzEncoder;
	use flate2::Compression;
	use std::io::Write;
	use tar::Builder;

	fn build_tarball<F>(write_entries: F) -> Vec<u8>
	where
		F: FnOnce(&mut Builder<&mut GzEncoder<Vec<u8>>>),
	{
		let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
		{
			let mut tar = Builder::new(&mut encoder);
			write_entries(&mut tar);
			tar.finish().unwrap();
		}
		encoder.finish().unwrap()
	}

	fn append_file<W: Write>(
		tar: &mut Builder<W>,
		path: &str,
		content: &[u8],
	) {
		let mut header = tar::Header::new_gnu();
		header.set_size(content.len() as u64);
		header.set_mode(0o644);
		header.set_cksum();
		tar.append_data(&mut header, path, content).unwrap();
	}

	/// Write a header with a raw (unsanitized) path so `tar` does not
	/// normalize away the `..` / leading `/` before our code sees it.
	fn append_raw_path_file<W: Write>(
		tar: &mut Builder<W>,
		path: &str,
		content: &[u8],
	) {
		assert!(path.len() < 100);
		let mut header = tar::Header::new_gnu();
		header.set_size(content.len() as u64);
		header.set_mode(0o644);
		header.as_mut_bytes()[..path.len()]
			.copy_from_slice(path.as_bytes());
		header.set_cksum();
		tar.append(&header, content).unwrap();
	}

	fn append_link<W: Write>(
		tar: &mut Builder<W>,
		entry_type: tar::EntryType,
		path: &str,
		target: &str,
	) {
		let mut header = tar::Header::new_gnu();
		header.set_entry_type(entry_type);
		header.set_size(0);
		header.set_mode(0o777);
		header.set_link_name(target).unwrap();
		header.set_cksum();
		tar.append_data(&mut header, path, std::io::empty())
			.unwrap();
	}

	#[test]
	fn extract_tarball_rejects_parent_directory_escape() {
		let temp_dir = tempdir().unwrap();
		let target_dir = temp_dir.path().join("target");
		let bytes = build_tarball(|tar| {
			append_file(
				tar,
				"repo-root-abc123/.claude-plugin/plugin.json",
				br#"{"name":"repo-root"}"#,
			);
			append_raw_path_file(
				tar,
				"repo-root-abc123/../escape.txt",
				b"escape",
			);
		});

		let error =
			GitBasedInstaller::extract_tarball(&bytes, "", &target_dir)
				.unwrap_err();

		assert!(error.to_string().contains("Unsafe archive path"));
		assert!(!temp_dir.path().join("escape.txt").exists());
	}

	#[test]
	fn extract_tarball_rejects_absolute_paths() {
		let temp_dir = tempdir().unwrap();
		let bytes = build_tarball(|tar| {
			append_file(
				tar,
				"repo-root-abc123/.claude-plugin/plugin.json",
				br#"{"name":"repo-root"}"#,
			);
			append_raw_path_file(
				tar,
				"/repo-root-abc123/absolute.txt",
				b"absolute",
			);
		});

		let error = GitBasedInstaller::extract_tarball(
			&bytes,
			"",
			temp_dir.path(),
		)
		.unwrap_err();

		assert!(error.to_string().contains("Unsafe archive path"));
	}

	#[test]
	fn extract_tarball_rejects_symlink_entries() {
		let temp_dir = tempdir().unwrap();
		let bytes = build_tarball(|tar| {
			append_file(
				tar,
				"repo-root-abc123/.claude-plugin/plugin.json",
				br#"{"name":"repo-root"}"#,
			);
			append_link(
				tar,
				tar::EntryType::Symlink,
				"repo-root-abc123/link",
				"../../outside",
			);
		});

		let error = GitBasedInstaller::extract_tarball(
			&bytes,
			"",
			temp_dir.path(),
		)
		.unwrap_err();

		assert!(
			error.to_string().contains("Unsafe archive entry type")
		);
		assert!(!temp_dir.path().join("link").exists());
	}

	#[test]
	fn extract_tarball_rejects_hard_link_entries() {
		let temp_dir = tempdir().unwrap();
		let bytes = build_tarball(|tar| {
			append_file(
				tar,
				"repo-root-abc123/.claude-plugin/plugin.json",
				br#"{"name":"repo-root"}"#,
			);
			append_link(
				tar,
				tar::EntryType::Link,
				"repo-root-abc123/hard-link",
				"repo-root-abc123/.claude-plugin/plugin.json",
			);
		});

		let error = GitBasedInstaller::extract_tarball(
			&bytes,
			"",
			temp_dir.path(),
		)
		.unwrap_err();

		assert!(
			error.to_string().contains("Unsafe archive entry type")
		);
		assert!(!temp_dir.path().join("hard-link").exists());
	}
```

`append_raw_path_file` writes the path bytes directly (bypassing `append_data` normalization) so `../` / leading `/` actually reach `extract_tarball`; GNU header path field is 100 bytes, hence `assert!(path.len() < 100)`.

Run: `cargo test --package aghub-cc-plugins --lib installer::git::tests::extract_tarball_rejects`
Expected: FAIL — current code returns `Ok` (so `.unwrap_err()` panics) and writes `escape.txt` outside root.

- [ ] **Step 2a: Fix imports.** `crates/cc-plugins/src/installer/git.rs:5`:

```rust
use std::path::{Component, Path, PathBuf};
```

- [ ] **Step 2b: Add the `SafeArchivePath` type + helpers** after the `GitBasedInstaller` struct definition (~line 7-9), before `build_http_client`:

```rust
#[derive(Clone, Debug, Eq, PartialEq)]
struct SafeArchivePath {
	archive_path: String,
}

impl SafeArchivePath {
	fn new(parts: Vec<String>) -> Self {
		Self {
			archive_path: parts.join("/"),
		}
	}

	fn as_archive_path(&self) -> &str {
		&self.archive_path
	}

	fn to_target_path(&self) -> PathBuf {
		let mut target_path = PathBuf::new();
		for part in self.archive_path.split('/') {
			target_path.push(part);
		}
		target_path
	}
}

/// Validate an archive entry path component-by-component, rejecting
/// absolute paths, `..`, root and Windows prefixes. Returns the
/// normalized forward-slash archive path on success.
fn safe_archive_relative_path(path: &Path) -> Result<SafeArchivePath> {
	if path.as_os_str().is_empty() || path.is_absolute() {
		anyhow::bail!("Unsafe archive path: {}", path.display());
	}

	let mut parts = Vec::new();
	for component in path.components() {
		match component {
			Component::Normal(part) => {
				let part = part.to_str().ok_or_else(|| {
					anyhow::anyhow!(
						"Archive path is not valid UTF-8: {}",
						path.display()
					)
				})?;
				if part.is_empty() {
					anyhow::bail!(
						"Unsafe empty archive path component: {}",
						path.display()
					);
				}
				parts.push(part.to_string());
			}
			Component::CurDir => {}
			Component::ParentDir
			| Component::RootDir
			| Component::Prefix(_) => {
				anyhow::bail!(
					"Unsafe archive path: {}",
					path.display()
				);
			}
		}
	}

	if parts.is_empty() {
		anyhow::bail!("Unsafe empty archive path: {}", path.display());
	}

	Ok(SafeArchivePath::new(parts))
}

/// Only regular files and directories may be extracted; symlinks and
/// hard links can redirect writes outside the extraction root.
fn ensure_safe_entry_type(
	entry_type: tar::EntryType,
	path: &str,
) -> Result<()> {
	if entry_type.is_file() || entry_type.is_dir() {
		return Ok(());
	}

	anyhow::bail!("Unsafe archive entry type {:?} for {}", entry_type, path)
}

/// Confirm a canonicalized path stays at or under the extraction root.
fn ensure_canonical_child(child: &Path, root: &Path) -> Result<()> {
	if child == root || child.starts_with(root) {
		return Ok(());
	}

	anyhow::bail!(
		"Archive entry target escaped extraction root: {}",
		child.display()
	)
}

/// Reject a target whose final component is a symlink, then create and
/// canonicalize the parent dir and confirm it is inside the root.
fn ensure_target_parent_safe(
	target_path: &Path,
	canonical_root: &Path,
) -> Result<()> {
	if let Ok(metadata) = std::fs::symlink_metadata(target_path) {
		if metadata.file_type().is_symlink() {
			anyhow::bail!(
				"Archive entry target is a symlink: {}",
				target_path.display()
			);
		}
	}

	let parent = target_path.parent().ok_or_else(|| {
		anyhow::anyhow!(
			"Archive entry target has no parent: {}",
			target_path.display()
		)
	})?;
	std::fs::create_dir_all(parent)?;
	let canonical_parent = parent.canonicalize().with_context(|| {
		format!("Failed to canonicalize parent {}", parent.display())
	})?;
	ensure_canonical_child(&canonical_parent, canonical_root)
}

/// Ensure the extraction target itself is a real directory (not a
/// symlink) before we canonicalize and write into it.
fn reset_extraction_target(target_dir: &Path) -> Result<()> {
	match std::fs::symlink_metadata(target_dir) {
		Ok(metadata) => {
			if metadata.file_type().is_symlink() {
				anyhow::bail!(
					"Extraction target is a symlink: {}",
					target_dir.display()
				);
			}
			if !metadata.is_dir() {
				anyhow::bail!(
					"Extraction target is not a directory: {}",
					target_dir.display()
				);
			}
		}
		Err(_) => {
			std::fs::create_dir_all(target_dir)?;
		}
	}
	Ok(())
}
```

`reset_extraction_target`'s `Err(_)` branch creates the dir so a not-yet-existing `temp_dir/target` (as in the tests) can be canonicalized.

- [ ] **Step 2c: Rewrite the first collection loop** (replaces ~lines 91-107):

```rust
		let mut entry_errors = Vec::new();
		let mut entries = Vec::new();
		for entry in archive.entries().context(
			"Failed to read tarball entries - archive may be \
			 corrupted or not a valid gzip file",
		)? {
			let entry = match entry {
				Ok(entry) => entry,
				Err(err) => {
					entry_errors.push(format!("{err:?}"));
					continue;
				}
			};
			let path =
				entry.path().context("Failed to read tar entry path")?;
			let path_str = path.to_string_lossy();
			if path_str.contains("pax_global_header") {
				continue;
			}
			let safe_path = safe_archive_relative_path(&path)?;
			entries.push(safe_path.as_archive_path().to_string());
		}
```

Keep the existing empty-tarball check and `find_common_prefix_static` call that follow.

- [ ] **Step 2d: Rewrite the extraction section.** First, run `subdir` through validation and reset+canonicalize the target before the loop (replaces the `extract_prefix` computation, ~lines 130-135):

```rust
		let subdir = subdir.trim_matches('/');
		let extract_prefix = if subdir.is_empty() {
			prefix.clone()
		} else {
			let safe_subdir =
				safe_archive_relative_path(Path::new(subdir))?;
			format!("{}{}/", prefix, safe_subdir.as_archive_path())
		};

		reset_extraction_target(target_dir)?;
		let canonical_target_dir =
			target_dir.canonicalize().with_context(|| {
				format!(
					"Failed to canonicalize extraction target {}",
					target_dir.display()
				)
			})?;
```

Then replace the `for entry in archive.entries()?` body (~lines 137-162):

```rust
		for entry in archive.entries()? {
			let mut entry = entry?;
			let path = entry.path()?;
			let safe_path = safe_archive_relative_path(&path)?;
			let path_str = safe_path.as_archive_path();

			if path_str.starts_with(&extract_prefix) {
				let relative_path = path_str
					.strip_prefix(&extract_prefix)
					.ok_or_else(|| {
						anyhow::anyhow!("Failed to strip prefix")
					})?;

				if relative_path.is_empty() {
					continue;
				}

				let relative_path = safe_archive_relative_path(
					Path::new(relative_path),
				)?;
				let target_path =
					target_dir.join(relative_path.to_target_path());
				let entry_type = entry.header().entry_type();
				ensure_safe_entry_type(
					entry_type,
					safe_path.as_archive_path(),
				)?;

				if entry_type.is_dir() {
					std::fs::create_dir_all(&target_path)?;
					let canonical_dir = target_path
						.canonicalize()
						.with_context(|| {
							format!(
								"Failed to canonicalize extracted \
								 directory {}",
								target_path.display()
							)
						})?;
					ensure_canonical_child(
						&canonical_dir,
						&canonical_target_dir,
					)?;
				} else {
					ensure_target_parent_safe(
						&target_path,
						&canonical_target_dir,
					)?;
					entry.unpack(&target_path)?;
				}
			}
		}
```

(The commit_sha computation after the loop is unchanged.)

- [ ] **Step 3: Run tests, confirm GREEN.**

Run: `cargo test --package aghub-cc-plugins --lib installer::git`
Expected: PASS — existing `test_find_common_prefix`, `test_extract_tarball_from_repo_root` plus the four new attack tests. Then `just lint` (hard-tab/80-col/clippy clean).

- [ ] **Step 4: Commit.**

```bash
git add crates/cc-plugins/src/installer/git.rs
git commit -m "$(cat <<'EOF'
fix(plugins): validate archive extraction paths

Reject `..`, absolute, and root archive paths; allow only file/dir
entries (block symlink/hardlink); canonicalize extracted paths and
confirm they stay under the extraction root. Prevents zip-slip /
link-redirect writes outside the target dir. Ports upstream 52a938c.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 3: Codex sub-agent file I/O hardening (symlink attack)

**Why / design decision:** `crates/agents/src/agents/codex/sub_agent.rs` reads with `fs::read_to_string` (follows symlinks), loads `.toml` filtered only by extension (reads symlinked toml), and `save_to_dir` uses `fs::write` which follows a symlinked target to clobber an external victim file. Port upstream `ffeec65`: read via `O_NOFOLLOW` (unix), filter non-regular `.toml` on load, write via staging temp + `rename`, and canonicalize to confirm paths stay inside the agents dir. Unix uses `libc::O_NOFOLLOW` via `OpenOptionsExt::custom_flags`; `cfg(not(unix))` provides a plain-read fallback. Public signatures unchanged (`parse_file → Option<SubAgent>`, `load_from_dir → Vec<SubAgent>`, `save_to_dir → Result<()>`). `libc 0.2.186` is already in `Cargo.lock` (transitive), so this only promotes it to a direct dependency.

**Files:**

- Modify: `Cargo.toml` (root `[workspace.dependencies]` — add `libc = "0.2"`)
- Modify: `crates/agents/Cargo.toml` (`[dependencies]` — add `libc = { workspace = true }`)
- Modify: `crates/agents/src/agents/codex/sub_agent.rs` (imports ~10-11, `parse_file` ~19-20, `load_from_dir` + helpers ~127-140, `save_to_dir` ~142-149)
- Test: `crates/agents/src/agents/codex/sub_agent.rs` (`#[cfg(test)] mod tests`, add two `#[cfg(unix)]` tests)

- [ ] **Step 1: Add dependencies.** Root `Cargo.toml`, after the `# TOML parsing` group (after `toml_edit = "0.25"`):

```toml
# FFI (O_NOFOLLOW for hardened file I/O)
libc = "0.2"
```

`crates/agents/Cargo.toml` `[dependencies]`, before `which = "8"`:

```toml
libc = { workspace = true }
```

Run: `cargo build --package aghub-agents` — confirms it compiles and `libc 0.2.186` is unchanged in the lock.

- [ ] **Step 2: Write the failing `#[cfg(unix)]` tests (RED).** In `mod tests`, after `roundtrip_save_load` and before `sanitize_filename_basic`:

```rust
	#[cfg(unix)]
	#[test]
	fn load_ignores_symlinked_toml_files() {
		use std::os::unix::fs::symlink;

		let dir = TempDir::new().unwrap();
		let outside = dir.path().join("outside.toml");
		fs::write(
			&outside,
			concat!(
				"name = \"outside\"\n",
				"developer_instructions = \"secret\"\n",
			),
		)
		.unwrap();
		let agents_dir = dir.path().join("agents");
		fs::create_dir(&agents_dir).unwrap();
		symlink(&outside, agents_dir.join("evil.toml")).unwrap();

		let loaded = load_from_dir(&agents_dir);
		assert!(loaded.is_empty());
	}

	#[cfg(unix)]
	#[test]
	fn save_rejects_symlinked_target_without_clobbering_victim() {
		use std::os::unix::fs::symlink;

		let dir = TempDir::new().unwrap();
		let agents_dir = dir.path().join("agents");
		fs::create_dir(&agents_dir).unwrap();
		let victim = dir.path().join("victim.txt");
		fs::write(&victim, "do not overwrite").unwrap();
		symlink(&victim, agents_dir.join("evil.toml")).unwrap();
		let agent = SubAgent {
			name: "evil".to_string(),
			description: Some("malicious".to_string()),
			instruction: Some("clobber".to_string()),
			source_path: None,
			config_source: None,
		};

		let err = save_to_dir(&agents_dir, &agent).unwrap_err();
		assert!(err.to_string().contains("symlink"));
		assert_eq!(fs::read_to_string(&victim).unwrap(), "do not overwrite");
	}
```

`save_rejects_*` uses `name = "evil"` so `sanitize_filename("evil") == "evil"` hits `agents_dir/evil.toml` (the symlink). **Verify during implementation** that the `SubAgent` struct literal fields (`name`, `description`, `instruction`, `source_path`, `config_source`) match `crates/agents/src/models.rs` exactly — if the struct has more/fewer fields, mirror the existing `roundtrip_save_load` test's construction.

Run: `cargo test --package aghub-agents agents::codex::sub_agent::tests::load_ignores_symlinked_toml_files agents::codex::sub_agent::tests::save_rejects_symlinked_target_without_clobbering_victim`
Expected: FAIL — `load` reads the symlinked toml (`loaded` not empty); `save` follows the symlink and overwrites `victim.txt`, returning `Ok` so `.unwrap_err()` panics.

- [ ] **Step 3a: Fix imports** (`sub_agent.rs:8-11`). **MERGE, do not replace** — the existing `crate::errors` / `crate::models` imports are still required (`ConfigError`, `Result`, `SubAgent`, `ResourceScope` are used by the helpers and the public load/save fns). The full import block becomes:

```rust
use crate::errors::{ConfigError, Result};
use crate::models::{ResourceScope, SubAgent};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
```

(If the existing file imports these crate types via slightly different paths, keep the existing `use crate::...` lines verbatim and only widen the `std::fs`/`std::io`/`std::path`/`std::time` lines as shown. Confirmed real signature of the formatter: `fn format(agent: &SubAgent, original_content: Option<&str>) -> Result<String>` — the `format(agent, original.as_deref())?` call in Step 3e is correct as written.)

- [ ] **Step 3b: `parse_file` read line** (`sub_agent.rs:20`) — `let content = fs::read_to_string(path).ok()?;` becomes:

```rust
	let content = read_regular_file(path).ok()?;
```

- [ ] **Step 3c: `load_from_dir` filter** (`sub_agent.rs:131-137`) — replace the `.filter(|e| { … })` with:

```rust
	let mut agents: Vec<SubAgent> = entries
		.flatten()
		.filter(is_regular_toml_entry)
		.filter_map(|e| parse_file(&e.path()))
		.collect();
```

- [ ] **Step 3d: Add helpers** after `load_from_dir`'s closing `}` (~line 140), before `save_to_dir`:

```rust
fn is_regular_toml_entry(entry: &fs::DirEntry) -> bool {
	if entry.path().extension().and_then(|x| x.to_str()) != Some("toml") {
		return false;
	}
	entry.file_type().map(|t| t.is_file()).unwrap_or(false)
}

fn safe_canonical_dir(dir: &Path) -> Result<PathBuf> {
	fs::create_dir_all(dir)?;
	let meta = fs::symlink_metadata(dir)?;
	if meta.file_type().is_symlink() || !meta.is_dir() {
		return Err(ConfigError::InvalidConfig(format!(
			"Codex sub-agent directory is not a real directory: {}",
			dir.display()
		)));
	}
	dir.canonicalize().map_err(ConfigError::from)
}

fn validate_existing_file(file: &Path, canonical_dir: &Path) -> Result<()> {
	let meta = fs::symlink_metadata(file)?;
	if meta.file_type().is_symlink() {
		return Err(ConfigError::InvalidConfig(format!(
			"Refusing to follow Codex sub-agent symlink: {}",
			file.display()
		)));
	}
	if !meta.is_file() {
		return Err(ConfigError::InvalidConfig(format!(
			"Codex sub-agent path is not a regular file: {}",
			file.display()
		)));
	}
	let canonical_file = file.canonicalize()?;
	if !canonical_file.starts_with(canonical_dir) {
		return Err(ConfigError::InvalidConfig(format!(
			"Codex sub-agent path escapes agents directory: {}",
			file.display()
		)));
	}
	Ok(())
}

fn read_regular_file(path: &Path) -> Result<String> {
	let canonical_dir = path
		.parent()
		.and_then(|p| p.canonicalize().ok())
		.ok_or_else(|| {
			ConfigError::InvalidConfig(format!(
				"Codex sub-agent path has no parent: {}",
				path.display()
			))
		})?;
	validate_existing_file(path, &canonical_dir)?;
	let mut content = String::new();
	open_no_follow(path)?.read_to_string(&mut content)?;
	Ok(content)
}

fn read_original(file: &Path, canonical_dir: &Path) -> Result<Option<String>> {
	match fs::symlink_metadata(file) {
		Ok(_) => {}
		Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
			return Ok(None);
		}
		Err(e) => return Err(e.into()),
	}
	validate_existing_file(file, canonical_dir)?;
	let mut content = String::new();
	open_no_follow(file)?.read_to_string(&mut content)?;
	Ok(Some(content))
}

#[cfg(unix)]
fn open_no_follow(path: &Path) -> std::io::Result<File> {
	use std::os::unix::fs::OpenOptionsExt;

	OpenOptions::new()
		.read(true)
		.custom_flags(libc::O_NOFOLLOW)
		.open(path)
}

#[cfg(not(unix))]
fn open_no_follow(path: &Path) -> std::io::Result<File> {
	OpenOptions::new().read(true).open(path)
}

fn write_replace(file: &Path, content: &str) -> Result<()> {
	let dir = file.parent().ok_or_else(|| {
		ConfigError::InvalidConfig(format!(
			"Codex sub-agent path has no parent: {}",
			file.display()
		))
	})?;
	let file_name =
		file.file_name().and_then(|n| n.to_str()).ok_or_else(|| {
			ConfigError::InvalidConfig(format!(
				"Codex sub-agent path has invalid filename: {}",
				file.display()
			))
		})?;
	let suffix = SystemTime::now()
		.duration_since(UNIX_EPOCH)
		.map(|d| d.as_nanos())
		.unwrap_or_default();
	let tmp = dir.join(format!(".{file_name}.{suffix}.tmp"));
	let result = (|| -> Result<()> {
		let mut handle =
			OpenOptions::new().write(true).create_new(true).open(&tmp)?;
		handle.write_all(content.as_bytes())?;
		handle.sync_all()?;
		drop(handle);
		fs::rename(&tmp, file)?;
		Ok(())
	})();
	if result.is_err() {
		let _ = fs::remove_file(&tmp);
	}
	result
}
```

- [ ] **Step 3e: Rewrite `save_to_dir`** (`sub_agent.rs:142-149`):

```rust
fn save_to_dir(dir: &Path, agent: &SubAgent) -> Result<()> {
	let canonical_dir = safe_canonical_dir(dir)?;
	let safe = sanitize_filename(&agent.name);
	let file = dir.join(format!("{safe}.toml"));
	let original = read_original(&file, &canonical_dir)?;
	let content = format(agent, original.as_deref())?;
	write_replace(&file, &content)?;
	let canonical_file = file.canonicalize()?;
	if !canonical_file.starts_with(&canonical_dir) {
		return Err(ConfigError::InvalidConfig(format!(
			"Codex sub-agent path escapes agents directory: {}",
			file.display()
		)));
	}
	Ok(())
}
```

**Verify during implementation:** the existing `save_to_dir` calls a formatting fn — confirm its real name/signature (the spec assumes `format(agent, original.as_deref())`). If the current code calls it differently (e.g. `format_sub_agent(agent)` with no original arg), keep the existing call shape and only wrap the write in `write_replace` + the symlink guard via `read_original`. The key invariant: `read_original` must run (and reject a symlinked target) **before** any write.

- [ ] **Step 4: Run tests, confirm GREEN.**

Run: `cargo test --package aghub-agents agents::codex::sub_agent::tests`
Expected: PASS — existing `parse_toml_*`, `format_preserves_extra_fields`, `roundtrip_save_load`, `sanitize_filename_basic`, plus the two new unix tests. Then `just lint && just fmt`.

- [ ] **Step 5: Commit.**

```bash
git add Cargo.toml crates/agents/Cargo.toml crates/agents/src/agents/codex/sub_agent.rs Cargo.lock
git commit -m "$(cat <<'EOF'
fix(agents): harden Codex sub-agent file I/O against symlink attacks

Refuse to follow symlinks on read (O_NOFOLLOW on unix), filter
non-regular .toml entries on load, and write via staging temp + rename
so a symlinked target can never clobber an external victim file.
Ports upstream ffeec65. Public signatures unchanged.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 4: Constrain `/skills/content` and `/skills/tree` reads to allow-listed roots

**Why / design decision:** `get_skill_content` reads any path via `expand_tilde_path` + `read_to_string` (no root check → arbitrary file read); `get_skill_tree`/`build_skill_tree_node` uses `std::fs::metadata` (follows symlinks). Port upstream `91bd12d`'s protection but **reuse our existing `allowed_skill_roots` + `assert_contained`** (already used by `delete_skill_by_path`) instead of upstream's parallel `canonical_*`/`ensure_skill_*` helpers — one root definition, no drift. Add `scope` + `project_root` to the two query DTOs (needed to compute legal roots), which regenerates the frontend DTOs (run `generate:dto` + prettier per memory). **Do not touch `delete_skill_by_path`** — it already has equivalent coverage.

**Files:**

- Modify: `crates/api/src/dto/skill.rs:373-383` (`SkillContentQuery` + `SkillTreeQuery` — add `scope`, `project_root`)
- Modify: `crates/api/src/routes/skills.rs:916-977` (`build_skill_tree_node` — `metadata` → `symlink_metadata`, reject symlink)
- Modify: `crates/api/src/routes/skills.rs:1486-1517` (`get_skill_content` + `get_skill_tree` — guard before read; add `assert_skill_read_allowed` helper)
- Generated: `crates/desktop/src/generated/dto/SkillContentQuery.ts`, `SkillTreeQuery.ts` (regenerated, do not hand-edit)
- Test: `crates/api/src/lib.rs` (`mod tests`)

- [ ] **Step 1: Ensure test deps + write the failing tests (RED).** The tests live in **`crates/api/src/lib.rs` `mod tests`** (~line 276), NOT in `skills.rs`. They use `url::form_urlencoded` to build query strings; `url` is in workspace deps but not yet a dev-dep of this crate. Add it under `[dev-dependencies]` in `crates/api/Cargo.toml` (alongside the existing `tempfile` dev-dep):

```toml
url = { workspace = true }
```

Then add the tests to `mod tests` (`Client`, `Status` and `tempfile` are already used by existing route tests there; reference the new crate as `url::form_urlencoded`):

```rust
	#[test]
	fn skill_content_rejects_path_outside_skills_roots() {
		let project = tempfile::tempdir().expect("project dir");
		let skill_dir = project.path().join(".claude/skills/legit");
		std::fs::create_dir_all(&skill_dir).expect("skill dir");
		std::fs::write(
			skill_dir.join("SKILL.md"),
			"---\nname: legit\ndescription: d\n---\n\n# Body\n",
		)
		.expect("write skill");

		let client = Client::tracked(build_rocket(
			rocket::Config::default(),
			default_app_data_dir(),
		))
		.expect("client");

		let mut q = url::form_urlencoded::Serializer::new(String::new());
		q.append_pair("path", "/etc/passwd");
		q.append_pair("scope", "project");
		q.append_pair("project_root", &project.path().to_string_lossy());
		let uri = format!("/api/v1/skills/content?{}", q.finish());

		let response = client.get(&uri).dispatch();
		// assert_skill_read_allowed returns Status::Forbidden when the
		// canonicalized path is outside the allow-listed roots.
		assert_eq!(
			response.status(),
			Status::Forbidden,
			"reading outside skills roots must be refused, not served"
		);
	}

	#[test]
	fn skill_tree_rejects_parent_dir_traversal() {
		let project = tempfile::tempdir().expect("project dir");
		let skill_dir = project.path().join(".claude/skills/legit");
		std::fs::create_dir_all(&skill_dir).expect("skill dir");
		std::fs::write(
			skill_dir.join("SKILL.md"),
			"---\nname: legit\ndescription: d\n---\n\n# Body\n",
		)
		.expect("write skill");

		let client = Client::tracked(build_rocket(
			rocket::Config::default(),
			default_app_data_dir(),
		))
		.expect("client");

		let escape = skill_dir
			.join("../../../../../../etc")
			.to_string_lossy()
			.to_string();
		let mut q = url::form_urlencoded::Serializer::new(String::new());
		q.append_pair("path", &escape);
		q.append_pair("scope", "project");
		q.append_pair("project_root", &project.path().to_string_lossy());
		let uri = format!("/api/v1/skills/tree?{}", q.finish());

		let response = client.get(&uri).dispatch();
		assert_eq!(
			response.status(),
			Status::Forbidden,
			"traversal must be refused"
		);
	}

	#[cfg(unix)]
	#[test]
	fn skill_tree_rejects_symlink_escaping_root() {
		use std::os::unix::fs::symlink;
		let project = tempfile::tempdir().expect("project dir");
		let skills = project.path().join(".claude/skills");
		std::fs::create_dir_all(&skills).expect("skills dir");
		let outside = tempfile::tempdir().expect("outside");
		std::fs::create_dir_all(outside.path().join("secret"))
			.expect("secret dir");
		let evil = skills.join("evil");
		symlink(outside.path().join("secret"), &evil).expect("symlink");

		let client = Client::tracked(build_rocket(
			rocket::Config::default(),
			default_app_data_dir(),
		))
		.expect("client");

		let mut q = url::form_urlencoded::Serializer::new(String::new());
		q.append_pair("path", &evil.to_string_lossy());
		q.append_pair("scope", "project");
		q.append_pair("project_root", &project.path().to_string_lossy());
		let uri = format!("/api/v1/skills/tree?{}", q.finish());

		let response = client.get(&uri).dispatch();
		assert_eq!(
			response.status(),
			Status::Forbidden,
			"a skills-root entry that is a symlink out of tree must be refused"
		);
	}
```

**Verify during implementation:** confirm `build_rocket`'s real signature and the `default_app_data_dir()` test helper exist in `crates/api/src/lib.rs` tests (upstream `f04f142` added route-level tests using this shape; match whatever the existing tests use).

Run: `cargo test -p aghub-api skill_content_rejects_path_outside_skills_roots skill_tree_rejects_parent_dir_traversal skill_tree_rejects_symlink_escaping_root`
Expected: FAIL — all return `Status::Ok` (content served / tree listed / symlink followed).

- [ ] **Step 2: Add DTO fields** (`crates/api/src/dto/skill.rs:373-383`):

```rust
#[derive(Debug, TS, rocket::FromForm)]
#[ts(export)]
pub struct SkillContentQuery {
	pub path: String,
	pub scope: Option<String>,
	pub project_root: Option<String>,
}

#[derive(Debug, TS, rocket::FromForm)]
#[ts(export)]
pub struct SkillTreeQuery {
	pub path: String,
	pub scope: Option<String>,
	pub project_root: Option<String>,
}
```

- [ ] **Step 3: Reject symlinks in `build_skill_tree_node`** (`skills.rs:919`) — change the opening metadata read:

```rust
	let metadata = std::fs::symlink_metadata(path).map_err(|e| {
		ApiError::new(
			Status::NotFound,
			format!("Failed to read skill path metadata: {e}"),
			"SKILL_PATH_NOT_FOUND",
		)
	})?;
	if metadata.file_type().is_symlink() {
		return Err(ApiError::new(
			Status::BadRequest,
			format!("Skill tree cannot include symlink '{}'", path.display()),
			"INVALID_SKILL_PATH",
		));
	}
```

(The subsequent `metadata.is_dir()` usage is unchanged.)

- [ ] **Step 4: Add the shared guard helper** in `skills.rs`, above `get_skill_content`:

```rust
/// Resolve the allow-listed skills roots for a (scope, project_root) pair and
/// assert `path` canonicalizes to inside one of them. Mirrors the containment
/// guard used by `delete_skill_by_path`, so content/tree reads cannot escape
/// the skills tree (incl. via `..` or a symlink whose target is out of tree).
fn assert_skill_read_allowed(
	path: &Path,
	resource_scope: ResourceScope,
	project_root: Option<&Path>,
) -> Result<PathBuf, ApiError> {
	let agent_dirs = aghub_core::skills::removal::agent_skill_dirs_in_scope(
		resource_scope,
		project_root,
	);
	let roots = aghub_core::skills::removal::allowed_skill_roots(
		&agent_dirs,
		project_root,
	);
	aghub_core::skills::removal::assert_contained(path, &roots).ok_or_else(
		|| {
			ApiError::new(
				Status::Forbidden,
				"Refusing to read: resolved path is outside the \
				 allow-listed skills roots",
				"SKILL_PATH_OUTSIDE_ROOT",
			)
		},
	)
}
```

**Verify during implementation:** confirm the exact paths/signatures of `aghub_core::skills::removal::{agent_skill_dirs_in_scope, allowed_skill_roots, assert_contained}` and the `ResourceScope` type / `resolved_to_resource_scope` helper by reading how `delete_skill_by_path` (skills.rs ~199-335) calls them, and mirror that exactly.

- [ ] **Step 5: Guard `get_skill_content`** (`skills.rs:1487`):

```rust
#[get("/skills/content?<query..>")]
pub fn get_skill_content(query: SkillContentQuery) -> ApiResult<String> {
	let resolved = ScopeParams {
		scope: query.scope.clone(),
		project_root: query.project_root.clone(),
	}
	.resolve()?;
	let (resource_scope, project_root) = resolved_to_resource_scope(&resolved);

	let path = expand_tilde_path(&query.path);
	let safe_path = assert_skill_read_allowed(
		&path,
		resource_scope,
		project_root.as_deref(),
	)?;

	let content = std::fs::read_to_string(&safe_path).map_err(|e| {
		ApiError::new(
			Status::NotFound,
			format!("Failed to read skill file: {e}"),
			"SKILL_FILE_NOT_FOUND",
		)
	})?;

	let skill = skill::parser::parse_skill_md(&content).map_err(|e| {
		ApiError::new(
			Status::BadRequest,
			format!("Invalid skill format: {e}"),
			"INVALID_SKILL_FORMAT",
		)
	})?;

	Ok(Json(skill.content))
}
```

- [ ] **Step 6: Guard `get_skill_tree`** (`skills.rs:1510`):

```rust
#[get("/skills/tree?<query..>")]
pub fn get_skill_tree(
	query: SkillTreeQuery,
) -> ApiResult<SkillTreeNodeResponse> {
	let resolved = ScopeParams {
		scope: query.scope.clone(),
		project_root: query.project_root.clone(),
	}
	.resolve()?;
	let (resource_scope, project_root) = resolved_to_resource_scope(&resolved);

	let path = expand_tilde_path(&query.path);
	let root = get_skill_root(path);
	let safe_root = assert_skill_read_allowed(
		&root,
		resource_scope,
		project_root.as_deref(),
	)?;
	let tree = build_skill_tree_node(&safe_root)?;
	Ok(Json(tree))
}
```

**Verify during implementation:** both handlers are **fully rewritten** (the current bodies have zero scope handling), not patched. The Step 5/6 code blocks ARE the complete new bodies — confirm `get_skill_root`, `expand_tilde_path`, `parse_skill_md`, `SkillTreeNodeResponse` still exist with these names and keep their call shape; the only new behavior is the prepended scope resolution + `assert_skill_read_allowed` guard before the read.

- [ ] **Step 7: Run tests, confirm GREEN.**

Run: `cargo test -p aghub-api --lib skill_content_rejects_path_outside_skills_roots skill_tree_rejects_parent_dir_traversal skill_tree_rejects_symlink_escaping_root`
Expected: PASS. Then `cargo test -p aghub-api` (no regression) and `just lint`.

- [ ] **Step 8: Regenerate DTOs + prettier.**

```bash
cd crates/desktop && bun run generate:dto
bunx prettier --write 'src/generated/dto/SkillContentQuery.ts' 'src/generated/dto/SkillTreeQuery.ts'
```

Run: `git -C /home/audichuang/research/aghub status --short crates/desktop/src/generated/dto/`
Expected: only `SkillContentQuery.ts` and `SkillTreeQuery.ts` changed (each gains `scope?` + `project_root?`), no spurious 121-file diff (run prettier before diffing per the generated-DTO workflow).

- [ ] **Step 9: Commit.**

```bash
git add crates/api/Cargo.toml Cargo.lock crates/api/src/dto/skill.rs crates/api/src/routes/skills.rs crates/api/src/lib.rs crates/desktop/src/generated/dto/SkillContentQuery.ts crates/desktop/src/generated/dto/SkillTreeQuery.ts
git commit -m "$(cat <<'EOF'
fix(api): constrain skill content/tree reads to allow-listed roots

get_skill_content / get_skill_tree now resolve scope + project_root,
build the allow-listed skills roots via agent_skill_dirs_in_scope +
allowed_skill_roots, and assert_contained() the requested path before
reading — reusing the delete-by-path containment guard.
build_skill_tree_node uses symlink_metadata and rejects symlinks so a
link in a skills root cannot escape the tree. SkillContentQuery /
SkillTreeQuery gain scope + project_root (regenerated DTOs).

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 5: Restrict git-scan explicit credentials to github.com

**Why / design decision:** `git_scan_skills`'s explicit `req.credential_id` path takes `cred.token` and clones `req.url` with no host check — token leaks to any host. The automatic path (`resolve_token_for_source`) is already host-scoped, but we apply the same guard to both (matching upstream `2f13f0c`: any token ⇒ must be github.com HTTPS). Port `require_github_credential_url` (parse with `url` crate; allow only `scheme()=="https"` && host ASCII-case-eq `"github.com"`; reject lookalikes like `github.com.attacker.example`, http downgrade, and parse failures). `url = "2.5"` is already in root workspace deps; just reference it in the api crate.

**Files:**

- Modify: `crates/api/Cargo.toml` (`[dependencies]` — add `url = { workspace = true }`)
- Modify: `crates/api/src/routes/skills.rs` (add `require_github_credential_url` before `#[post("/skills/git/scan", …)]` ~line 1568; insert guard in `git_scan_skills` after `credential_token` is computed, before the clone spawn)
- Test: `crates/api/src/routes/skills.rs` (`#[cfg(test)] mod tests`, ~line 2182)

- [ ] **Step 1: Write the failing tests (RED).** In the `mod tests` of `skills.rs` (has `use super::*;`), add:

```rust
	#[test]
	fn github_credential_url_accepts_github_https() {
		assert!(require_github_credential_url(
			"https://github.com/owner/repo.git",
		)
		.is_ok());
	}

	#[test]
	fn github_credential_url_rejects_non_github_hosts() {
		let err = require_github_credential_url(
			"https://evil.example/x.git",
		)
		.unwrap_err();

		assert_eq!(err.status, Status::BadRequest);
		assert_eq!(err.body.code, "INVALID_GITHUB_CREDENTIAL_URL");
	}

	#[test]
	fn github_credential_url_rejects_github_lookalikes() {
		let err = require_github_credential_url(
			"https://github.com.attacker.example/x.git",
		)
		.unwrap_err();

		assert_eq!(err.status, Status::BadRequest);
		assert_eq!(err.body.code, "INVALID_GITHUB_CREDENTIAL_URL");
	}

	#[test]
	fn github_credential_url_rejects_non_https_github() {
		let err =
			require_github_credential_url("http://github.com/x.git")
				.unwrap_err();

		assert_eq!(err.status, Status::BadRequest);
		assert_eq!(err.body.code, "INVALID_GITHUB_CREDENTIAL_URL");
	}
```

**Verify during implementation:** confirm `ApiError`'s public fields are `status: Status` and `body.code: &'static str` (read `crates/api/src/error.rs`); if the field names differ, adjust the assertions to the real shape.

Run: `cargo test --package aghub-api github_credential_url`
Expected: FAIL to compile — `cannot find function require_github_credential_url`.

- [ ] **Step 2: Add the `url` dependency** in `crates/api/Cargo.toml` `[dependencies]` (after `reqwest`):

```toml
url = { workspace = true }
```

- [ ] **Step 3: Add `require_github_credential_url`** before `#[post("/skills/git/scan", data = "<body>")]` (~line 1569):

```rust
fn require_github_credential_url(url: &str) -> Result<(), ApiError> {
	let parsed = url::Url::parse(url).map_err(|_| {
		ApiError::new(
			Status::BadRequest,
			"GitHub credentials can only be used with github.com HTTPS URLs",
			"INVALID_GITHUB_CREDENTIAL_URL",
		)
	})?;

	let host = parsed.host_str().unwrap_or_default();
	if parsed.scheme() == "https" && host.eq_ignore_ascii_case("github.com") {
		return Ok(());
	}

	Err(ApiError::new(
		Status::BadRequest,
		"GitHub credentials can only be used with github.com HTTPS URLs",
		"INVALID_GITHUB_CREDENTIAL_URL",
	))
}
```

- [ ] **Step 4: Insert the guard** in `git_scan_skills`, after the `credential_token` let-binding completes (~line 1602) and before `let url = req.url.clone();` (~line 1613):

```rust
	if credential_token.is_some() {
		require_github_credential_url(&req.url)?;
	}

```

`Status` is already imported (`use rocket::http::Status;`), `ApiError` via `use crate::{…}`, `url::Url` used by full path. `req.url` is `String` so `&req.url` coerces to `&str`.

**Verify during implementation:** confirm the variable is named `credential_token` and is an `Option`, and that the insertion point is after both the explicit-`credential_id` and session-reuse branches have merged into it (so the guard covers both). If the handler returns a different `Result`/error type than `ApiError`, adapt `require_github_credential_url`'s return + the `?` accordingly.

- [ ] **Step 5: Run tests, confirm GREEN.**

Run: `cargo test --package aghub-api github_credential_url`
Expected: PASS (4 tests). Then `cargo build -p aghub-api` (lock auto-adds `url`) and `just lint`.

- [ ] **Step 6: Commit.**

```bash
git add crates/api/Cargo.toml crates/api/src/routes/skills.rs Cargo.lock
git commit -m "$(cat <<'EOF'
fix(api): restrict GitHub credentials to github.com scans

git_scan_skills now requires any credential-bearing scan URL to be an
https://github.com URL (host ASCII-case-exact), rejecting lookalike
hosts, http downgrade, and arbitrary hosts before the token is handed to
git clone. Ports upstream 2f13f0c.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Self-Review Checklist (run before execution)

1. **Spec coverage:** all five upstream commits (`3ad9f1c`, `52a938c`, `ffeec65`, `91bd12d` content/tree subset, `2f13f0c`) have a task. `91bd12d`'s delete-by-path is intentionally excluded (already covered by our `assert_contained`).
2. **Placeholder scan:** no TBD/"add error handling"/"similar to Task N" — every code step has full code. The "**Verify during implementation**" notes are deliberate guards against line-number/signature drift, not placeholders.
3. **Type consistency:** `assert_skill_read_allowed`, `require_github_credential_url`, `SafeArchivePath`, `write_replace`/`read_original`/`open_no_follow` are each defined once and called consistently within their task.
