# Symlink-Only Skill Install — Implementation Plan

> **Required sub-skill:** when executing this plan with an agentic worker, load and follow `superpowers:executing-plans` (and, per task, `superpowers:test-driven-development`). Each task below is a red → green → commit TDD loop; do not batch tasks.

**Spec**: `docs/specs/2026-06-19-symlink-only-skill-install.md` (the contract)
**Branch**: `feat/symlink-only-install` — **already created** off `feat/cli-sources`. Commits land directly on this branch. Do NOT create a branch.
**Status**: implementation-ready

## Goal

Converge the entire skill-**install** surface onto ONE model: **symlink-only**. A single `.agents/skills/<name>` Master plus per-agent links — NO copy as a user choice, NO copy fallback on the install path. Port Skills-Manager's MIT junction primitives so Windows installs survive without admin / Developer-Mode; delete every copy path on the install/removal/discovery surface; fix the agent auto-detection blind spot so the install UI never asks the user to pick an agent already covered by the Master or not installed.

## Architecture

- **New module (eventually replaces `install_layout.rs`)**: `crates/core/src/skills/linker/` — a directory module. `install_layout.rs` is kept as a thin re-export shim during the migration (Task 20) and deleted only at the end (Task 47a), so every commit builds the whole workspace.
    - `mod.rs`: descriptor-free mechanical core (std + `dirs`). `Linker { link, is_link, unlink }`, `LinkTarget {Relative, Absolute}`, `LinkOutcome {Linked, AlreadyLinked, Conflict}`, `LinkError {LinkUnsupported, NonAbsoluteTarget, Io}`, private `create_link`/`create_junction`/`normalize_path`/`relative_path`, `universal_canonical_dir`, the convenience layer (`install_universal`, `link_agents_to_canonical`, `UniversalInstallReport`), and the npx-contract `copy_dir_recursive` + `EXCLUDE_*` (Master materialization ONLY).
    - `classify.rs`: descriptor-aware auto-classification (the blind-spot fix). `LinkNeed {NativeReader, NeedsLink{agent_skills_dir}, Unsupported}`, `AgentLinkPlan {agent_id, need, installed, reads_master, writes_master}` (the last two are the REAL classifier facts surfaced for the coverage DTO, P2-G), `classify_agent`, `classify_all`.
- **Link mechanics**: Unix = `symlink`; Windows = native `symlink_dir` FIRST, then `cmd /C mklink /J <ABSOLUTE master>`; both fail => per-agent HARD error (`LinkError::LinkUnsupported`), NO copy. Junctions require an ABSOLUTE target.
- **Detection (P0)**: every `is_symlink()` probe on the install/removal/discovery surface becomes `Linker::is_link` so a Windows junction is recognised (a junction reports `is_dir()==true`, `is_symlink()==false`).
- **Hard-error policy (Decision 10)**: a per-agent `LinkError` is folded into that agent's result row (`installed:false, error:Some(msg)`), NOT a whole-install abort.
- **No silent no-op (Decision 11)**: the lock is written when `(wrote_master || installed_any)`, so a master-only install (every link fails) still records.
- **Coverage endpoint (new)**: `GET /api/v1/skills/coverage` → `Vec<AgentSkillCoverageDto>`, canonicalisation server-side, booleans only (no raw paths).
- **OUT OF SCOPE (Decision 9)**: `transfer.rs` cross-agent copy stays copy-based.

## Tech Stack

- Rust workspace; hard tabs (width 4), 80-col max, `cargo clippy -- -D warnings` (warnings = errors).
- `just test` == `cargo test --workspace`; `just preflight` is the pre-release gate.
- Per-crate single test: `cargo test --package <crate> <name> -- --exact`.
- Desktop: `bun` only (never npm/yarn/pnpm); ts-rs DTO export + prettier over `src/generated/` (regen alone shows a spurious ~121-file diff — run prettier, then diff, to isolate the real change). FE logic tests use Node's built-in runner: `node --test --experimental-strip-types <file>` (no vitest/jest installed).
- New module deps: `mod.rs` = std + `dirs` (already a workspace dep, NO Cargo.toml edit); `classify.rs` = `aghub-agents` + `crate::availability` + `crate::registry`. NO new crate, NO new `[workspace].members` line.

## Public API (pinned — every task uses these EXACT names)

From `linker/mod.rs`: `Linker`, `Linker::is_link(&Path)->bool`, `Linker::unlink(&Path)->io::Result<()>`, `Linker::link(master_dir, agent_skills_dir, skill_name, target: LinkTarget)->Result<LinkOutcome, LinkError>`; `LinkTarget {Relative, Absolute}`; `LinkOutcome {Linked, AlreadyLinked, Conflict}`; `LinkError {LinkUnsupported{target,link,source}, NonAbsoluteTarget{target}, Io(io::Error)}`; `universal_canonical_dir(Option<&Path>)->Option<PathBuf>`; `install_universal(source_root, canonical, agent_skills_dirs, target)->Result<UniversalInstallReport, LinkError>`; `link_agents_to_canonical(canonical, agent_skills_dirs, target)->Result<UniversalInstallReport, LinkError>`; `UniversalInstallReport {canonical, linked, already_linked, conflicts, failed}` (NO `copied_fallback`); `pub(crate) create_junction(abs_target, link)` (`#[cfg(windows)]`).

From `linker/classify.rs`: `LinkNeed {NativeReader, NeedsLink{agent_skills_dir}, Unsupported}`; `AgentLinkPlan {agent_id, need, installed, reads_master, writes_master}` (`reads_master`/`writes_master` carry the real read/write-master facts for the coverage DTO, P2-G); `classify_agent(descriptor: &AgentDescriptor, scope: ResourceScope, project_root: Option<&Path>, master_skills_dir: &Path)->AgentLinkPlan`; `classify_all(scope, project_root, master_skills_dir)->Vec<AgentLinkPlan>`.

From `crates/api/src/dto/agent_coverage.rs`: `AgentSkillCoverageDto { id, scope, reads_master, writes_master, needs_link, auto_covered, supported }`.

---

## File Structure (every file Created / Modified / Tested, by chunk)

```
crates/core/src/skills/
├── linker/                       # NEW module — REPLACES install_layout.rs   [Tasks 1-19]
│   ├── mod.rs                    #   CREATED: Linker, LinkTarget, LinkOutcome,
│   │                             #   LinkError, create_link, create_junction,
│   │                             #   normalize_path, relative_path,
│   │                             #   universal_canonical_dir, install_universal,
│   │                             #   link_agents_to_canonical,
│   │                             #   UniversalInstallReport, copy_dir_recursive
│   │                             #   + EXCLUDE_* (Master materialization)
│   └── classify.rs               #   CREATED: LinkNeed, AgentLinkPlan,
│                                 #   classify_agent, classify_all              [Tasks 12-19]
├── install_layout.rs             # Task 20: becomes a thin RE-EXPORT shim of
│                                 #   linker/ (NOT deleted). DELETED in Task 47a
│                                 #   once every consumer is migrated.          [Tasks 20, 47a]
├── mod.rs                        # MODIFIED: keep `pub mod install_layout;`
│                                 #   (re-export shim) + add `pub mod linker;`
│                                 #   in Task 1; drop install_layout in Task 47a [Tasks 1, 20, 47a]
├── install_fetched.rs            # MODIFIED: collapse dispatch to always-universal
│                                 #   classify-driven install_universal_layout
│                                 #   (fold report.failed AND report.conflicts);
│                                 #   lock gate -> (wrote_master || installed_any);
│                                 #   delete install_isolated/local copy_dir_recursive;
│                                 #   KEEP SkillInstallLayout/layout/use_relative_links
│                                 #   as ignored shim until Task 47a swaps in
│                                 #   target: LinkTarget; TESTED no-copy        [Tasks 21-25, 47a]
├── discovery.rs                  # MODIFIED: is_symlink() -> Linker::is_link;
│                                 #   TESTED T-DISCOVERY-JUNCTION-CANONICAL      [Tasks 33-34]
└── removal.rs                    # MODIFIED: execute_removal /
                                  #   plan_symlink_removal /
                                  #   dir_has_external_referrer ->
                                  #   Linker::is_link; TESTED junction tests     [Tasks 30-32]
crates/core/src/manager/skill.rs  # MODIFIED: add_skill_from_path + add_skill
                                  #   (manual-create) -> master+link;
                                  #   remove_skill_path + relink helpers ->
                                  #   Linker; TESTED junction + no-copy (both
                                  #   add_skill_from_path AND add_skill)          [Tasks 25-29, 34]
crates/core/Cargo.toml            # MODIFIED: add libc to [dev-dependencies]
                                  #   (if missing)                              [Task 8]
crates/core/src/transfer.rs       # UNCHANGED (Decision 9 — out of scope)       [—]
crates/core/tests/sources_install_tests.rs # MODIFIED: migrate the 8 layout/
                                  #   use_relative_links call sites to the shim's
                                  #   symlink-only behavior (no IsolatedCopy
                                  #   copy assertions) BEFORE Task 47a removes
                                  #   the fields                                 [Task 23a]
crates/cli/src/main.rs            # MODIFIED: Add `--universal` -> hidden no-op  [Task 35a]
crates/cli/src/commands/add.rs    # MODIFIED: collapse if-universal copy branches
                                  #   -> symlink-only add_skill / add_skill_from_path [Task 35a]
crates/cli/tests/cli_tests.rs     # MODIFIED: add symlink-only add + --universal
                                  #   no-op acceptance test                       [Task 35b]
crates/cli/src/commands/source.rs # MODIFIED: collapse apply_install copy branch
                                  #   (drop SkillInstallLayout/layout/
                                  #   use_relative_links use); `source sync
                                  #   --universal` becomes a no-op (P0-3)         [Task 35c]
crates/api/src/extractors.rs      # MODIFIED: absolutize project_root (P0-C)    [Task 36]
crates/api/src/dto/agent_coverage.rs # CREATED: AgentSkillCoverageDto           [Task 37]
crates/api/src/dto/mod.rs         # MODIFIED: add `pub mod agent_coverage;`     [Task 37]
crates/api/src/dto/skill.rs       # MODIFIED: remove GitInstallRequest.universal;
                                  #   enrich InstallSkillResponse with agents    [Tasks 40, 42]
crates/api/src/routes/coverage.rs # CREATED: GET /skills/coverage handler        [Task 38]
crates/api/src/routes/mod.rs      # MODIFIED: add `pub mod coverage;`           [Task 38]
crates/api/src/routes/skills.rs   # MODIFIED: git_install_skills, install_skill,
                                  #   delete_skill_by_path rewired (incl.
                                  #   absolutize project_root at :238, P1-F);
                                  #   entry_allowed (:942) -> Linker::is_link
                                  #   (P1-E2); delete dead copy helpers + tests   [Tasks 41-46, 36a, 43a]
crates/api/src/lib.rs             # MODIFIED: mount /skills/coverage route       [Task 39]
crates/api/src/bin/export-dto.rs  # MODIFIED: register AgentSkillCoverageDto     [Task 47]
crates/desktop/src/lib/api.ts                     # MODIFIED: agents.skillCoverage() [Task 51]
crates/desktop/src/requests/keys.ts               # MODIFIED: agents.coverage key   [Task 51]
crates/desktop/src/requests/agents.ts             # MODIFIED: useSkillCoverage hook  [Task 51]
crates/desktop/src/lib/agent-capabilities.ts      # MODIFIED: partitionByCoverage +
                                                  #   isAutoCoveredByMaster /
                                                  #   needsMasterLink              [Task 49]
crates/desktop/src/lib/agent-capabilities.test.ts # CREATED: node:test bucketing    [Tasks 48-49]
crates/desktop/src/lib/install-layout.ts          # DELETED                          [Task 53]
crates/desktop/src/lib/install-layout.test.ts     # DELETED                          [Task 53]
crates/desktop/src/pages/sources/index.tsx        # MODIFIED: drop toggle/state;
                                                  #   master-only install valid     [Task 54]
crates/desktop/src/components/import-github-skill-panel.tsx # MODIFIED: link-target
                                                  #   only AgentSelector + chips     [Task 60]
crates/desktop/src/lib/locales/en.ts              # MODIFIED: del installLayout*,
                                                  #   add coverage keys              [Task 56]
crates/desktop/src/lib/locales/zh-Hant.ts         # MODIFIED: same key edits         [Task 57]
crates/desktop/src/lib/locales/zh-Hans.ts         # MODIFIED: same key edits         [Task 57]
crates/desktop/src/generated/dto/*.ts             # REGENERATED: drop universal,
                                                  #   add AgentSkillCoverageDto,
                                                  #   InstallSkillResponse.agents;
                                                  #   prettier                       [Tasks 47, 58]
# --- goldens & CI that must NOT change behaviour (verification only) ---
crates/skill/tests/hash_parity_golden.rs          # VERIFIED: zero edits             [Task 64]
crates/skill/tests/npx_interop.rs                  # VERIFIED: zero edits             [Task 64]
crates/skill/tests/fixture_validation.rs           # VERIFIED: zero edits             [Task 64]
.github/workflows/ci.yml                           # VERIFIED: windows-latest+just test [Task 65]
.github/workflows/release.yml                      # VERIFIED: windows test gate       [Task 65]
justfile                                           # VERIFIED: just test==--workspace  [Task 65]
docs/superpowers/plans/2026-06-19-symlink-only-skill-install.md # this plan
```

---

## Global task order (each COMMIT leaves the WHOLE workspace building; never a window where omitting `universal` means copy)

The spec's implementation order is **core first** so the server is unconditionally-universal BEFORE the FE flag is removed.

> **Green-workspace-per-commit invariant (P0-1 + P0-2).** Every committed task must leave `cargo build --workspace` / `cargo test --workspace` green — not just one crate. The shared symbols `SkillInstallLayout`, `FetchedSkillInstallRequest.layout`, and `FetchedSkillInstallRequest.use_relative_links` are referenced by FOUR consumers outside `install_fetched.rs`: the core integration suite `crates/core/tests/sources_install_tests.rs` (8 call sites), the CLI `crates/cli/src/commands/source.rs::apply_install` (the `source sync` install helper), and the API routes `git_install_skills`/`install_skill`. Deleting those shared symbols early would break those consumers' commits. **Therefore the deletion of the shared copy types is deferred to a LATE task (Task 47a)**, after every consumer has been migrated; until then a thin backward-compatible SHIM keeps `layout`/`use_relative_links` accepted-and-ignored so the workspace keeps compiling at every commit. The dispatch inside `install_fetched_skill_and_lock` collapses to the always-universal path immediately (Task 22), so the install primitive is symlink-only from Task 23 on even while the now-ignored request fields linger for the shim window. **Any task that removes a shared symbol uses a WORKSPACE gate (`cargo build --workspace` / `just test`), not a per-crate `-p aghub-core` gate**, so a broken downstream consumer is caught in that same task.

1. **Tasks 1–11 — Mechanical linker core** (`linker/mod.rs`): scaffold the module alongside the still-present `install_layout.rs`, build `Linker {is_link, unlink, link}`, `LinkTarget`/`LinkOutcome`/`LinkError`, `create_link`/`create_junction`, the npx `copy_dir_recursive`, and the convenience layer. Crate stays green throughout (`install_layout.rs` untouched, all existing callers compile).
2. **Tasks 12–19 — Classifier** (`linker/classify.rs`): `LinkNeed`, `AgentLinkPlan`, `classify_agent`, `classify_all` + the per-scope descriptor-matrix unit tests. Pure addition.
3. **Tasks 20–35 — Core rewire (copy types kept as an ignored shim)**: move the linker primitives into `linker/` and re-point `install_layout` to it (Task 20 re-exports, no deletion yet); collapse the `install_fetched_skill_and_lock` dispatch to the always-universal classify-driven path while keeping `SkillInstallLayout`/`layout`/`use_relative_links` as accepted-but-ignored shim fields (Tasks 21–23) so `sources_install_tests.rs`, the CLI, and the API all still compile; rewrite `install_universal_layout` to classify-drive and fold BOTH `report.failed` AND `report.conflicts` (Task 23, P1-D); swap every `is_symlink()` → `Linker::is_link` on the install/removal/discovery surface; make `add_skill_from_path` AND `add_skill` (manual-create) master+link. After this the core primitive is symlink-only; the lingering request fields are dead weight removed in Task 47a. Each commit here builds the whole workspace because the shim preserves the request API.
4. **Tasks 35a–35c — CLI rewire**: collapse the `if universal {…} else {…copy}` branches in `add::execute` to the single symlink-only call, turn the `Add` command's `--universal` flag into a hidden deprecation no-op (Task 35a), pin `add` with a CLI test (Task 35b), AND collapse the `source sync` install helper's copy branch in `crates/cli/src/commands/source.rs::apply_install` to the always-universal request — making `source sync --universal` a no-op too (Task 35c, P0-3). Runs AFTER core is green (its symbols already exist) and BEFORE the API chunk (no API/DTO dependency). Closes the workspace-build hole: `crates/cli` is compiled and run by `cargo test --workspace`, so BOTH its `add` and `source sync` install branches must be converted here, not left to fail a later gate.
5. **Tasks 36–47 — API + coverage + P0-C**: absolutize `project_root` (extractors AND the `delete_skill_by_path` route, Tasks 36/36a, P1-F); add `AgentSkillCoverageDto` + `/skills/coverage` carrying REAL reads/writes-master facts (Tasks 37/38, P2-G); rewire `git_install_skills`/`install_skill`/`delete_skill_by_path`; route `entry_allowed` through `Linker::is_link` (Task 43a, P1-E2); remove `GitInstallRequest.universal`; delete dead copy helpers; regen DTOs. Now the server is unconditionally-universal — the latest safe point at which the FE may still send `universal` (the field is ignored).
    - **Task 47a — delete the shared copy shim**: ONLY after Tasks 21–46 have migrated every consumer (core tests in Task 23a, CLI in Task 35c, API in Tasks 41/42), delete `install_layout.rs`, the `SkillInstallLayout` enum, and the `layout`/`use_relative_links` request fields, replacing them with `target: LinkTarget`. Gated by `cargo build --workspace` + `just test`.
6. **Tasks 48–63 — Frontend**: bucketing helpers + `useSkillCoverage`, delete `install-layout.ts`/test/i18n keys, drop the `universal:` body line, partition the install UI on `auto_covered`/`needs_link`, regen the DTO surface.
7. **Tasks 64–67 — Cross-cutting verification**: npx-golden green-check (incl. T-MASTER-HASH-STABLE + T-LOCK-PARITY-LINK-VS-COPY, Task 64), Windows-CI confirmation, no-copy-survivor grep (now including `crates/cli`, Task 66), type-system no-copy guarantee + `just preflight`. Run Tasks 64 and 66 after each of the core/API chunks as a guard, and all four as the final gate.

**Ordering invariant (why this prevents a copy window):** the FE `universal` field is only _removed_ in step 5, which runs AFTER step 4 makes the server ignore it. Between today and step 4 the server still understands `universal`; from step 4 on it is universal-only and ignores the field. There is never a commit where the FE omits `universal` AND the server interprets its absence as "copy" under the old contract.

**Compile-order story (which task deletes the shared types, and why each prior commit builds):** Task 20 RE-EXPORTS `install_layout` from `linker` (no file deleted); Tasks 21–23 collapse the dispatch to always-universal but keep `layout`/`use_relative_links` as ignored shim fields, so `sources_install_tests.rs` + CLI + API all still compile and their commits are workspace-green; Task 23a migrates `sources_install_tests.rs` to the still-present shim's behavior (asserting symlink-only, not copy); Task 35c migrates the CLI; Tasks 41–42 migrate the API. **Task 47a is the SINGLE task that deletes `install_layout.rs` + `SkillInstallLayout` + the `layout`/`use_relative_links` fields**, and it runs only after every one of those consumers references `target: LinkTarget` instead. Its gate is the whole-workspace build, so a missed consumer fails _in Task 47a_, never as a silent broken intermediate commit.

---

## Tasks 1–11 — Mechanical linker core (`crates/core/src/skills/linker/mod.rs`)

> Module-level attribution doc comment (MIT, jiweiyeah/Skills-Manager) is included in Task 1's first impl step. `install_layout.rs` is left untouched here (Task 20 turns it into a thin re-export shim; Task 47a finally deletes it), so the crate keeps compiling and every existing caller stays green — temporary duplication is BY DESIGN.

### Task 1: Scaffold the linker module so the crate sees `crate::skills::linker`

**Step 1.1 — Write the failing test (module-exists smoke test).**
Create the new file `crates/core/src/skills/linker/mod.rs` with ONLY the attribution doc comment, the imports, and a single smoke test asserting the module compiles and `universal_canonical_dir` (the verbatim move) resolves. Use hard tabs.

```rust
//! Cross-platform directory-link primitives ported from jiweiyeah/Skills-Manager
//! (MIT) — linker.rs: is_symlink_or_junction / remove_symlink_or_junction /
//! create_windows_symlink / normalize_path. SM's iflow copy-mode is intentionally
//! NOT ported: aghub bans copy as a skill-install outcome.

use std::io;
use std::path::{Component, Path, PathBuf, MAIN_SEPARATOR};

/// Resolve the `.agents/skills` canonical SKILLS-DIR for a scope.
///
/// `project_root.is_some()` => `<root>/.agents/skills`; `None` =>
/// `~/.agents/skills`. The returned path is absolute iff the input root is
/// absolute (callers MUST pass an absolute project_root — Decision 6).
pub fn universal_canonical_dir(project_root: Option<&Path>) -> Option<PathBuf> {
	match project_root {
		Some(root) => Some(root.join(".agents").join("skills")),
		None => {
			dirs::home_dir().map(|home| home.join(".agents").join("skills"))
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn universal_canonical_dir_resolves_by_scope() {
		let project = Path::new("/tmp/proj");
		assert_eq!(
			universal_canonical_dir(Some(project)),
			Some(PathBuf::from("/tmp/proj/.agents/skills"))
		);
		if let Some(home) = dirs::home_dir() {
			assert_eq!(
				universal_canonical_dir(None),
				Some(home.join(".agents/skills"))
			);
		}
	}
}
```

Now register the module. Edit `crates/core/src/skills/mod.rs` — find the line `pub mod install_layout;` (currently line 3) and add `pub mod linker;` directly after it (keep `install_layout` — Task 20 turns it into a re-export shim, Task 47a removes it):

```rust
pub mod install_fetched;
pub mod install_layout;
pub mod linker;
pub mod prune;
```

**Step 1.2 — Run, expect FAIL (unused-imports compile error under `-D warnings`).**
The `Component`, `MAIN_SEPARATOR`, and `io` imports are unused until later tasks, so under `-D warnings` the build fails. Run:

```bash
cargo test --package aghub-core universal_canonical_dir_resolves_by_scope -- --exact
```

Expected: a compile error like `error: unused import: ... Component ... MAIN_SEPARATOR ... io` (`-D warnings` promotes it). This confirms the file is now part of the build.

**Step 1.3 — Minimal impl: silence the not-yet-used imports.**
Replace the import block in `crates/core/src/skills/linker/mod.rs`:

```rust
#[allow(unused_imports)]
use std::io;
#[allow(unused_imports)]
use std::path::{Component, Path, PathBuf, MAIN_SEPARATOR};
```

**Step 1.4 — Run, expect PASS.**

```bash
cargo test --package aghub-core universal_canonical_dir_resolves_by_scope -- --exact
```

Expected: `test result: ok. 1 passed; 0 failed`.

**Step 1.5 — Commit.**

```bash
git add crates/core/src/skills/linker/mod.rs crates/core/src/skills/mod.rs
git commit -m "feat(core): scaffold skills::linker module with universal_canonical_dir

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 2: `Linker::is_link` — lstat-based symlink/junction detection (port of SM `is_symlink_or_junction`)

**Step 2.1 — Write the failing test.**
Append to the `mod tests` block in `crates/core/src/skills/linker/mod.rs` (and a unix leaf for the symlink case):

```rust
	#[test]
	fn is_link_false_for_real_dir_and_missing() {
		use tempfile::tempdir;
		let tmp = tempdir().unwrap();
		let real = tmp.path().join("real");
		std::fs::create_dir_all(&real).unwrap();
		assert!(!Linker::is_link(&real), "a real dir is not a link");
		assert!(
			!Linker::is_link(&tmp.path().join("missing")),
			"a missing path is not a link"
		);
	}

	#[cfg(unix)]
	#[test]
	fn is_link_true_for_unix_symlink() {
		use tempfile::tempdir;
		let tmp = tempdir().unwrap();
		let target = tmp.path().join("target");
		std::fs::create_dir_all(&target).unwrap();
		let link = tmp.path().join("link");
		std::os::unix::fs::symlink(&target, &link).unwrap();
		assert!(Linker::is_link(&link), "a unix symlink IS a link");
	}
```

**Step 2.2 — Run, expect FAIL.**

```bash
cargo test --package aghub-core is_link_false_for_real_dir_and_missing -- --exact
```

Expected: `error[E0433]: failed to resolve: use of undeclared type Linker` (and the same for the unix test).

**Step 2.3 — Minimal impl.**
Add the `Linker` zero-sized struct and `is_link` above the `#[cfg(test)]` module in `crates/core/src/skills/linker/mod.rs`:

```rust
/// Zero-sized, stateless namespace for the directory-link primitives.
pub struct Linker;

impl Linker {
	/// lstat-based reparse-point detection: true for a Unix symlink OR a
	/// Windows symlink/junction (FILE_ATTRIBUTE_REPARSE_POINT 0x0400). Never
	/// follows the link. Ported from SM `is_symlink_or_junction`.
	pub fn is_link(path: &Path) -> bool {
		if let Ok(meta) = path.symlink_metadata() {
			if meta.file_type().is_symlink() {
				return true;
			}
			#[cfg(windows)]
			{
				use std::os::windows::fs::MetadataExt;
				const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
				if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
					return true;
				}
			}
		}
		false
	}
}
```

**Step 2.4 — Run, expect PASS.**

```bash
cargo test --package aghub-core is_link -- --nocapture
```

Expected: `test result: ok. 2 passed` (on Windows CI the unix one is skipped; the junction case is covered in Task 9).

**Step 2.5 — Commit.**

```bash
git add crates/core/src/skills/linker/mod.rs
git commit -m "feat(core): Linker::is_link reparse-point detection (port SM is_symlink_or_junction)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 3: `Linker::unlink` — remove a link without touching its target (port of SM `remove_symlink_or_junction`)

**Step 3.1 — Write the failing test.**
Append to `mod tests` in `crates/core/src/skills/linker/mod.rs`:

```rust
	#[test]
	fn unlink_is_idempotent_on_missing_path() {
		use tempfile::tempdir;
		let tmp = tempdir().unwrap();
		Linker::unlink(&tmp.path().join("nope"))
			.expect("unlinking a missing path is a no-op");
	}

	#[cfg(unix)]
	#[test]
	fn unlink_removes_symlink_but_keeps_target() {
		use tempfile::tempdir;
		let tmp = tempdir().unwrap();
		let target = tmp.path().join("target");
		std::fs::create_dir_all(&target).unwrap();
		std::fs::write(target.join("keep.txt"), "keep").unwrap();
		let link = tmp.path().join("link");
		std::os::unix::fs::symlink(&target, &link).unwrap();

		Linker::unlink(&link).unwrap();

		assert!(!Linker::is_link(&link), "link must be gone");
		assert!(
			std::fs::symlink_metadata(&link).is_err(),
			"link path must not exist"
		);
		assert!(
			target.join("keep.txt").exists(),
			"unlink must never touch the target"
		);
	}
```

**Step 3.2 — Run, expect FAIL.**

```bash
cargo test --package aghub-core unlink_is_idempotent_on_missing_path -- --exact
```

Expected: `error[E0599]: no function or associated item named unlink found for struct Linker`.

**Step 3.3 — Minimal impl.**
Add `unlink` inside `impl Linker` in `crates/core/src/skills/linker/mod.rs`, after `is_link`:

```rust
	/// Remove a link without touching its target: on Windows `remove_dir` then
	/// `remove_file` (a junction is a dir reparse point; a Unix symlink-to-dir
	/// needs `remove_file`). Idempotent on a missing path. Ported from SM
	/// `remove_symlink_or_junction`. Uses `remove_dir`, NEVER `remove_dir_all`,
	/// so it only unlinks the reparse point and never recurses into the Master.
	pub fn unlink(path: &Path) -> io::Result<()> {
		let result = {
			#[cfg(windows)]
			{
				std::fs::remove_dir(path)
					.or_else(|_| std::fs::remove_file(path))
			}
			#[cfg(unix)]
			{
				std::fs::remove_file(path)
			}
			#[cfg(not(any(unix, windows)))]
			{
				std::fs::remove_file(path)
			}
		};
		match result {
			Ok(()) => Ok(()),
			Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
			Err(e) => Err(e),
		}
	}
```

Remove the `#[allow(unused_imports)]` above `use std::io;` (it is now used).

**Step 3.4 — Run, expect PASS.**

```bash
cargo test --package aghub-core unlink -- --nocapture
```

Expected: `test result: ok. 2 passed` (idempotent-missing + unix symlink-removal; junction removal is Task 9 on Windows).

**Step 3.5 — Commit.**

```bash
git add crates/core/src/skills/linker/mod.rs
git commit -m "feat(core): Linker::unlink reparse-safe removal (port SM remove_symlink_or_junction)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 4: `LinkError`, `LinkTarget`, `LinkOutcome`, `normalize_path`, `relative_path` enums + helpers

**Step 4.1 — Write the failing test.**
Append to `mod tests` in `crates/core/src/skills/linker/mod.rs`:

```rust
	#[test]
	fn relative_path_computes_minimal_dotdot() {
		assert_eq!(
			relative_path(
				Path::new("/root/.cursor/skills"),
				Path::new("/root/.agents/skills/foo")
			),
			PathBuf::from("../../.agents/skills/foo")
		);
	}

	#[test]
	fn link_error_from_io_maps_to_io_variant() {
		let e: LinkError =
			io::Error::new(io::ErrorKind::Other, "boom").into();
		assert!(matches!(e, LinkError::Io(_)));
	}

	#[test]
	fn non_absolute_target_constructs() {
		let e = LinkError::NonAbsoluteTarget {
			target: PathBuf::from("rel/path"),
		};
		assert!(matches!(e, LinkError::NonAbsoluteTarget { .. }));
	}
```

**Step 4.2 — Run, expect FAIL.**

```bash
cargo test --package aghub-core relative_path_computes_minimal_dotdot -- --exact
```

Expected: `error[E0425]: cannot find function relative_path` and `error[E0433]: ... undeclared type LinkError`.

**Step 4.3 — Minimal impl.**
Add the public enums (use workspace `thiserror`), `normalize_path`, and the private `relative_path` above `pub struct Linker;` in `crates/core/src/skills/linker/mod.rs`:

```rust
/// Whether a created link's stored target is relative (project scope, portable)
/// or absolute (global scope). Windows junctions ALWAYS resolve to absolute
/// even when `Relative` is requested (junctions cannot store a relative target).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkTarget {
	Relative,
	Absolute,
}

/// Outcome of a single link attempt against one agent skills-dir.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkOutcome {
	/// Fresh link created (unix symlink / win symlink / win junction).
	Linked,
	/// A correct link to the same Master already existed (idempotent).
	AlreadyLinked,
	/// A foreign symlink/junction OR a real file/dir occupies the slot —
	/// NEVER clobbered.
	Conflict,
}

/// A link failure. Per-agent failures are folded into
/// [`UniversalInstallReport::failed`] (Decision 10); they are NOT propagated as
/// `Err` from the convenience layer except for pre-link invariant violations.
#[derive(Debug, thiserror::Error)]
pub enum LinkError {
	/// BOTH native symlink AND `cmd /C mklink /J` failed on Windows (or symlink
	/// is unsupported on a non-unix/non-windows platform). HARD per-agent
	/// error — NO copy fallback.
	#[error("could not link {link} -> {target}: {source}")]
	LinkUnsupported {
		target: PathBuf,
		link: PathBuf,
		source: io::Error,
	},
	/// Decision 6 violated: `abs_target` was not absolute, so a junction could
	/// not be created safely.
	#[error("junction target must be absolute: {target}")]
	NonAbsoluteTarget { target: PathBuf },
	#[error(transparent)]
	Io(#[from] io::Error),
}

/// Normalize path separators to the platform native separator. On Windows
/// `/`->`\` (feeds `cmd.exe` native separators); on Unix a no-op. Ported from
/// SM `normalize_path`.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn normalize_path(path: &Path) -> PathBuf {
	if MAIN_SEPARATOR == '\\' {
		PathBuf::from(path.to_string_lossy().replace('/', "\\"))
	} else {
		path.to_path_buf()
	}
}

/// Compute a relative path so a symlink created inside `from_dir` resolves to
/// `to_path`. Both should be absolute. Falls back to the absolute `to_path`
/// when the two share no common prefix (different roots).
fn relative_path(from_dir: &Path, to_path: &Path) -> PathBuf {
	let from: Vec<Component> = from_dir.components().collect();
	let to: Vec<Component> = to_path.components().collect();

	let mut common = 0;
	while common < from.len()
		&& common < to.len()
		&& from[common] == to[common]
	{
		common += 1;
	}
	if common == 0 {
		return to_path.to_path_buf();
	}

	let mut result = PathBuf::new();
	for _ in common..from.len() {
		result.push("..");
	}
	for component in &to[common..] {
		result.push(component.as_os_str());
	}
	if result.as_os_str().is_empty() {
		PathBuf::from(".")
	} else {
		result
	}
}
```

Remove the `#[allow(unused_imports)]` line above `use std::path::{Component, ...}` (all four are now used).

**Step 4.4 — Run, expect PASS.**

```bash
cargo test --package aghub-core --lib skills::linker -- --nocapture
```

Expected: all linker tests pass (`relative_path_computes_minimal_dotdot`, `link_error_from_io_maps_to_io_variant`, `non_absolute_target_constructs`, plus Tasks 1-3).

**Step 4.5 — Lint + commit.**

```bash
cargo clippy --package aghub-core -- -D warnings
git add crates/core/src/skills/linker/mod.rs
git commit -m "feat(core): LinkTarget/LinkOutcome/LinkError + normalize_path/relative_path

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 5: `EXCLUDE_*` + `copy_dir_recursive` (Master materialization, moved verbatim)

**Step 5.1 — Write the failing test.**
Append to `mod tests` in `crates/core/src/skills/linker/mod.rs` (the npx-contract exclude test, ported verbatim):

```rust
	#[test]
	fn copy_dir_recursive_excludes_vcs_cache_and_metadata() {
		use tempfile::tempdir;
		let tmp = tempdir().unwrap();
		let src = tmp.path().join("src");
		std::fs::create_dir_all(src.join(".git")).unwrap();
		std::fs::write(src.join(".git/config"), "x").unwrap();
		std::fs::create_dir_all(src.join("__pycache__")).unwrap();
		std::fs::write(src.join("__pycache__/m.pyc"), "x").unwrap();
		std::fs::create_dir_all(src.join("__pypackages__")).unwrap();
		std::fs::write(src.join("__pypackages__/p"), "x").unwrap();
		std::fs::write(src.join("metadata.json"), "{}").unwrap();
		std::fs::write(src.join("SKILL.md"), "real").unwrap();
		std::fs::create_dir_all(src.join("assets")).unwrap();
		std::fs::write(src.join("assets/a.txt"), "keep").unwrap();

		let dest = tmp.path().join("dest");
		copy_dir_recursive(&src, &dest).unwrap();

		assert!(dest.join("SKILL.md").exists());
		assert!(dest.join("assets/a.txt").exists());
		assert!(!dest.join(".git").exists(), ".git must be excluded");
		assert!(!dest.join("__pycache__").exists(), "__pycache__ excluded");
		assert!(
			!dest.join("__pypackages__").exists(),
			"__pypackages__ excluded"
		);
		assert!(
			!dest.join("metadata.json").exists(),
			"metadata.json must be excluded"
		);
	}
```

**Step 5.2 — Run, expect FAIL.**

```bash
cargo test --package aghub-core copy_dir_recursive_excludes_vcs_cache_and_metadata -- --exact
```

Expected: `error[E0425]: cannot find function copy_dir_recursive in this scope`.

**Step 5.3 — Minimal impl.**
Add the constants and fn above `pub struct Linker;` in `crates/core/src/skills/linker/mod.rs` (verbatim move from `install_layout.rs`, with the doc note required by the spec):

```rust
/// Names excluded when materializing a Master, mirroring upstream npx
/// `copyDirectory` (installer.ts) so the Master hashes identically to npx.
const EXCLUDE_FILES: &[&str] = &["metadata.json"];
const EXCLUDE_DIRS: &[&str] = &[".git", "__pycache__", "__pypackages__"];

/// Recursively copy a skill source tree into the canonical Master directory,
/// applying the npx exclude lists and dereferencing symlinks.
///
/// NOTE: this copy materializes the single Master only; it is NOT a per-agent
/// copy fallback. The converged install model bans copy as a per-agent outcome.
fn copy_dir_recursive(from: &Path, to: &Path) -> io::Result<()> {
	std::fs::create_dir_all(to)?;
	for entry in std::fs::read_dir(from)? {
		let entry = entry?;
		let file_name = entry.file_name();
		let name = file_name.to_string_lossy();
		let file_type = entry.file_type()?;
		if EXCLUDE_FILES.contains(&name.as_ref())
			|| (file_type.is_dir() && EXCLUDE_DIRS.contains(&name.as_ref()))
		{
			continue;
		}
		let from_path = entry.path();
		let to_path = to.join(&file_name);
		if file_type.is_dir() {
			copy_dir_recursive(&from_path, &to_path)?;
		} else {
			match std::fs::metadata(&from_path) {
				Ok(meta) if meta.is_dir() => {
					copy_dir_recursive(&from_path, &to_path)?
				}
				Ok(_) => {
					std::fs::copy(&from_path, &to_path)?;
				}
				Err(e)
					if e.kind() == io::ErrorKind::NotFound
						&& file_type.is_symlink() => {}
				Err(e) => return Err(e),
			}
		}
	}
	Ok(())
}
```

**Step 5.4 — Run, expect PASS.**

```bash
cargo test --package aghub-core copy_dir_recursive_excludes_vcs_cache_and_metadata -- --exact
```

Expected: `test result: ok. 1 passed`.

**Step 5.5 — Commit.**

```bash
git add crates/core/src/skills/linker/mod.rs
git commit -m "feat(core): move EXCLUDE_*/copy_dir_recursive Master materialization into linker

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 6: `create_link` control flow (unix arm) — `Linker::link` minimal happy path

**Step 6.1 — Write the failing test (T-NOCOPY base, cross-platform via is_link).**
Append to `mod tests` in `crates/core/src/skills/linker/mod.rs`:

```rust
	#[test]
	fn link_creates_a_real_link_resolving_to_master() {
		use tempfile::tempdir;
		let tmp = tempdir().unwrap();
		let master = tmp.path().join(".agents/skills/my-skill");
		std::fs::create_dir_all(&master).unwrap();
		std::fs::write(master.join("SKILL.md"), "real").unwrap();
		let claude = tmp.path().join(".claude/skills");

		let outcome =
			Linker::link(&master, &claude, "my-skill", LinkTarget::Absolute)
				.unwrap();

		assert_eq!(outcome, LinkOutcome::Linked);
		let link = claude.join("my-skill");
		assert!(Linker::is_link(&link), "must be a real link, not a copy");
		// Write a sentinel into the Master AFTER linking, read it THROUGH the
		// link — proves a true link, not a coincidentally-identical copy.
		std::fs::write(master.join("sentinel.txt"), "via-link").unwrap();
		assert_eq!(
			std::fs::read_to_string(link.join("sentinel.txt")).unwrap(),
			"via-link"
		);
	}

	#[test]
	fn link_rejects_non_absolute_master() {
		let err = Linker::link(
			Path::new("rel/master"),
			Path::new("/tmp/agent/skills"),
			"x",
			LinkTarget::Absolute,
		)
		.unwrap_err();
		assert!(matches!(err, LinkError::NonAbsoluteTarget { .. }));
	}
```

**Step 6.2 — Run, expect FAIL.**

```bash
cargo test --package aghub-core link_creates_a_real_link_resolving_to_master -- --exact
```

Expected: `error[E0599]: no function or associated item named link found for struct Linker`.

**Step 6.3 — Minimal impl.**
Add `link` inside `impl Linker` (after `unlink`) and the private `create_link` (unix + fallback arms) below the impl block, in `crates/core/src/skills/linker/mod.rs`:

```rust
	/// Create `agent_skills_dir/<skill_name>` -> `master_dir` (the
	/// `.agents/skills/<name>` canonical SKILL-DIR, which MUST already exist and
	/// MUST be absolute). Creates `agent_skills_dir` if absent. lstat-inspects
	/// the occupant WITHOUT following it (via [`Linker::is_link`], so a junction
	/// is recognized): returns `AlreadyLinked` / `Conflict` without writing on
	/// collision. On a clean target: Unix => symlink; Windows => symlink_dir,
	/// else `cmd /C mklink /J <ABSOLUTE master>`; both fail =>
	/// `LinkError::LinkUnsupported`. `master_dir` not absolute =>
	/// `NonAbsoluteTarget`.
	pub fn link(
		master_dir: &Path,
		agent_skills_dir: &Path,
		skill_name: &str,
		target: LinkTarget,
	) -> Result<LinkOutcome, LinkError> {
		if !master_dir.is_absolute() {
			return Err(LinkError::NonAbsoluteTarget {
				target: master_dir.to_path_buf(),
			});
		}
		let link_path = agent_skills_dir.join(skill_name);
		let master_real = std::fs::canonicalize(master_dir)
			.unwrap_or_else(|_| master_dir.to_path_buf());

		// Inspect the existing occupant WITHOUT following it.
		match std::fs::symlink_metadata(&link_path) {
			Ok(_) => {
				if Self::is_link(&link_path) {
					let resolves = std::fs::canonicalize(&link_path)
						.map(|r| r == master_real)
						.unwrap_or(false);
					return Ok(if resolves {
						LinkOutcome::AlreadyLinked
					} else {
						LinkOutcome::Conflict
					});
				}
				return Ok(LinkOutcome::Conflict);
			}
			Err(e) if e.kind() == io::ErrorKind::NotFound => {}
			Err(e) => return Err(LinkError::Io(e)),
		}

		std::fs::create_dir_all(agent_skills_dir)?;

		let requested = match target {
			LinkTarget::Relative => {
				relative_path(agent_skills_dir, master_dir)
			}
			LinkTarget::Absolute => master_dir.to_path_buf(),
		};
		create_link(&requested, master_dir, &link_path)?;
		Ok(LinkOutcome::Linked)
	}
```

And below the `impl Linker` block:

```rust
/// Create a directory link at `link` pointing at `requested_target`
/// (possibly relative on Unix), falling back on Windows to a junction using
/// the absolute `abs_target`. Create-only: the caller has already verified the
/// slot is empty and `abs_target` is absolute.
#[cfg(unix)]
fn create_link(
	requested_target: &Path,
	_abs_target: &Path,
	link: &Path,
) -> Result<(), LinkError> {
	std::os::unix::fs::symlink(requested_target, link).map_err(|source| {
		LinkError::LinkUnsupported {
			target: requested_target.to_path_buf(),
			link: link.to_path_buf(),
			source,
		}
	})
}

#[cfg(not(any(unix, windows)))]
fn create_link(
	requested_target: &Path,
	_abs_target: &Path,
	link: &Path,
) -> Result<(), LinkError> {
	Err(LinkError::LinkUnsupported {
		target: requested_target.to_path_buf(),
		link: link.to_path_buf(),
		source: io::Error::new(
			io::ErrorKind::Unsupported,
			"symlinks are not supported on this platform",
		),
	})
}
```

**Step 6.4 — Run, expect PASS.**

```bash
cargo test --package aghub-core --lib skills::linker -- --nocapture
```

Expected: all linker tests pass including `link_creates_a_real_link_resolving_to_master` and `link_rejects_non_absolute_master`.

**Step 6.5 — Lint + commit.**

```bash
cargo clippy --package aghub-core -- -D warnings
git add crates/core/src/skills/linker/mod.rs
git commit -m "feat(core): Linker::link create-only path + unix create_link arm

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 7: Idempotency + conflict (no-clobber) coverage for `Linker::link`

**Step 7.1 — Write the tests.**
Append to `mod tests` in `crates/core/src/skills/linker/mod.rs`:

```rust
	#[test]
	fn link_is_idempotent_on_existing_correct_link() {
		use tempfile::tempdir;
		let tmp = tempdir().unwrap();
		let master = tmp.path().join(".agents/skills/my-skill");
		std::fs::create_dir_all(&master).unwrap();
		let claude = tmp.path().join(".claude/skills");

		let first =
			Linker::link(&master, &claude, "my-skill", LinkTarget::Absolute)
				.unwrap();
		let second =
			Linker::link(&master, &claude, "my-skill", LinkTarget::Absolute)
				.unwrap();

		assert_eq!(first, LinkOutcome::Linked);
		assert_eq!(second, LinkOutcome::AlreadyLinked);
	}

	#[test]
	fn link_never_clobbers_a_real_directory() {
		use tempfile::tempdir;
		let tmp = tempdir().unwrap();
		let master = tmp.path().join(".agents/skills/my-skill");
		std::fs::create_dir_all(&master).unwrap();
		let claude = tmp.path().join(".claude/skills");
		let occupied = claude.join("my-skill");
		std::fs::create_dir_all(&occupied).unwrap();
		std::fs::write(occupied.join("SKILL.md"), "pre-existing").unwrap();

		let outcome =
			Linker::link(&master, &claude, "my-skill", LinkTarget::Absolute)
				.unwrap();

		assert_eq!(outcome, LinkOutcome::Conflict);
		assert!(!Linker::is_link(&occupied), "must stay a real dir");
		assert_eq!(
			std::fs::read_to_string(occupied.join("SKILL.md")).unwrap(),
			"pre-existing"
		);
	}

	#[cfg(unix)]
	#[test]
	fn link_never_clobbers_a_foreign_link() {
		use tempfile::tempdir;
		let tmp = tempdir().unwrap();
		let master = tmp.path().join(".agents/skills/my-skill");
		std::fs::create_dir_all(&master).unwrap();
		let other = tmp.path().join("somewhere-else");
		std::fs::create_dir_all(&other).unwrap();
		std::fs::write(other.join("foreign.txt"), "foreign").unwrap();
		let claude = tmp.path().join(".claude/skills");
		std::fs::create_dir_all(&claude).unwrap();
		let slot = claude.join("my-skill");
		std::os::unix::fs::symlink(&other, &slot).unwrap();

		let outcome =
			Linker::link(&master, &claude, "my-skill", LinkTarget::Absolute)
				.unwrap();

		assert_eq!(outcome, LinkOutcome::Conflict);
		assert_eq!(
			std::fs::read_to_string(slot.join("foreign.txt")).unwrap(),
			"foreign",
			"foreign link must still resolve to its original target"
		);
	}
```

**Step 7.2 — Run.**
These exercise behavior already coded in Task 6 but are new assertions; run to confirm they pass.

```bash
cargo test --package aghub-core --lib skills::linker::tests::link_ -- --nocapture
```

Expected (behavior already correct): all PASS.

**Step 7.3 — Minimal impl (only if a test fails).**
If `link_is_idempotent_on_existing_correct_link` returns `Conflict` instead of `AlreadyLinked`, confirm `master_real` uses `std::fs::canonicalize(master_dir).unwrap_or_else(...)` (already in Task 6) — no change needed; the tests document the contract. If all pass, this step is a no-op.

**Step 7.4 — Run, expect PASS.**

```bash
cargo test --package aghub-core --lib skills::linker -- --nocapture
```

Expected: all pass.

**Step 7.5 — Commit.**

```bash
git add crates/core/src/skills/linker/mod.rs
git commit -m "test(core): pin Linker::link idempotency + no-clobber (real dir + foreign link)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 8: T-HARDERR — unix EACCES hard error, no copy, Master intact (root-skipped)

**Step 8.1 — Write the test.**
Append to `mod tests` in `crates/core/src/skills/linker/mod.rs`:

```rust
	#[cfg(unix)]
	#[test]
	fn link_hard_errors_when_symlink_create_is_denied() {
		use std::os::unix::fs::PermissionsExt;
		use tempfile::tempdir;
		// EACCES does not apply to root — skip there (matches removal.rs).
		if unsafe { libc::geteuid() } == 0 {
			return;
		}
		let tmp = tempdir().unwrap();
		let master = tmp.path().join(".agents/skills/my-skill");
		std::fs::create_dir_all(&master).unwrap();
		std::fs::write(master.join("SKILL.md"), "real").unwrap();
		// Pre-create the agent dir 0o500 so creating the link inside EACCESes.
		let claude = tmp.path().join(".claude/skills");
		std::fs::create_dir_all(&claude).unwrap();
		let original = std::fs::metadata(&claude).unwrap().permissions();
		std::fs::set_permissions(
			&claude,
			std::fs::Permissions::from_mode(0o500),
		)
		.unwrap();

		let result =
			Linker::link(&master, &claude, "my-skill", LinkTarget::Absolute);

		std::fs::set_permissions(&claude, original).unwrap();

		let err = result.unwrap_err();
		assert!(
			matches!(err, LinkError::LinkUnsupported { .. }),
			"denied symlink must be a hard LinkUnsupported, got {err:?}"
		);
		// No link created; Master (written first) intact — no copy fallback.
		assert!(
			std::fs::symlink_metadata(claude.join("my-skill")).is_err(),
			"no link must exist after a hard error (no copy fallback)"
		);
		assert_eq!(
			std::fs::read_to_string(master.join("SKILL.md")).unwrap(),
			"real",
			"Master must be intact"
		);
	}
```

**Step 8.2 — Run, expect FAIL or PASS-by-behavior.**

```bash
cargo test --package aghub-core link_hard_errors_when_symlink_create_is_denied -- --exact
```

Expected: if `libc` is not yet a dev-dep of `aghub-core`, it fails to compile (`error[E0433]: failed to resolve: use of undeclared crate or module libc`). Confirm by running; if it fails, add the dev-dep in Step 8.3.

**Step 8.3 — Minimal impl (add libc dev-dep if missing).**
If Step 8.2 failed with the undeclared-`libc` error, add `libc` to `crates/core/Cargo.toml` `[dev-dependencies]`. Edit the `[dev-dependencies]` block (preserve existing entries; example shape):

```toml
[dev-dependencies]
aghub-agents = { path = "../agents" }
tempfile = { workspace = true }
libc = { workspace = true }
```

The link hard-error behavior itself is already implemented (Task 6's `create_link` returns `LinkError::LinkUnsupported`; no copy fallback exists). No production-code change.

**Step 8.4 — Run, expect PASS.**

```bash
cargo test --package aghub-core link_hard_errors_when_symlink_create_is_denied -- --exact
```

Expected: `test result: ok. 1 passed` (or passes via the root early-return when run as root).

**Step 8.5 — Lint + commit.**

```bash
cargo clippy --package aghub-core -- -D warnings
git add crates/core/src/skills/linker/mod.rs crates/core/Cargo.toml
git commit -m "test(core): T-HARDERR unix EACCES hard error with no copy fallback

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 9: Windows arm — `create_link` symlink_dir → `create_junction` (extracted), hard error

> `#[cfg(windows)]` tests do NOT compile/run on the Linux dev box; they execute on the windows-latest leg of the 3-platform `just test` release gate. Locally, "expect PASS" means "no regression + compiles".

**Step 9.1 — Write the failing test (compile-gated; runs on windows-latest CI only).**
Append a Windows leaf module to `mod tests` in `crates/core/src/skills/linker/mod.rs`:

```rust
	#[cfg(windows)]
	mod windows_specific {
		use super::super::*;
		use tempfile::tempdir;

		// T-WIN-JUNCTION-DETECT: force the junction path directly so a junction
		// is exercised even when Developer Mode would let symlink_dir succeed.
		#[test]
		fn create_junction_makes_a_reparse_point_recognized_by_is_link() {
			let tmp = tempdir().unwrap();
			let master = std::fs::canonicalize(tmp.path())
				.unwrap()
				.join(".agents\\skills\\my-skill");
			std::fs::create_dir_all(&master).unwrap();
			std::fs::write(master.join("SKILL.md"), "real").unwrap();
			let claude = std::fs::canonicalize(tmp.path())
				.unwrap()
				.join(".claude\\skills");
			std::fs::create_dir_all(&claude).unwrap();
			let link = claude.join("my-skill");

			create_junction(&master, &link).unwrap();

			assert!(Linker::is_link(&link), "junction must be a link");
			assert!(
				!std::fs::symlink_metadata(&link)
					.unwrap()
					.file_type()
					.is_symlink(),
				"a junction reports is_symlink()==false (0x0400 branch)"
			);
			// T-WIN-JUNCTION-REMOVE: unlink removes the junction, keeps Master.
			Linker::unlink(&link).unwrap();
			assert!(!Linker::is_link(&link), "junction must be gone");
			assert!(
				master.join("SKILL.md").exists(),
				"Master must survive unlink (remove_dir, not remove_dir_all)"
			);
		}
	}
```

**Step 9.2 — Run (compile-check only on Linux; behavior verified on Windows CI).**

```bash
cargo test --package aghub-core --lib skills::linker -- --nocapture
```

Expected: all existing tests pass; the windows module is skipped. The genuine FAIL is the Windows compile: `create_junction` does not exist yet → on windows-latest CI this would be `error[E0425]: cannot find function create_junction`.

**Step 9.3 — Minimal impl.**
Add the Windows arm of `create_link` and the extracted `create_junction` below the existing `#[cfg(unix)]` `create_link`, in `crates/core/src/skills/linker/mod.rs`:

```rust
#[cfg(windows)]
fn create_link(
	requested_target: &Path,
	abs_target: &Path,
	link: &Path,
) -> Result<(), LinkError> {
	// Native symlink first (needs Dev Mode/admin); honors relative target.
	if std::os::windows::fs::symlink_dir(requested_target, link).is_ok() {
		return Ok(());
	}
	// Fallback: directory junction (no admin). MUST use the absolute target.
	create_junction(abs_target, link)
}

/// Create a directory junction at `link` pointing at the ABSOLUTE `abs_target`
/// via `cmd /C mklink /J`. Extracted as a named fn so tests can force the
/// junction path regardless of Developer Mode. Ported/adapted from SM
/// `create_windows_symlink` (junction arm); SM's pre-clean and GBK decoding are
/// dropped (the caller guarantees an empty, non-clobbered slot). Create-only.
#[cfg(windows)]
pub(crate) fn create_junction(
	abs_target: &Path,
	link: &Path,
) -> Result<(), LinkError> {
	use std::os::windows::process::CommandExt;
	use std::process::Command;

	let link_norm = normalize_path(link);
	let target_norm = normalize_path(abs_target);
	let output = Command::new("cmd")
		.args(["/C", "mklink", "/J"])
		.arg(&link_norm)
		.arg(&target_norm)
		.creation_flags(0x08000000) // CREATE_NO_WINDOW
		.output();

	match output {
		Ok(out) if out.status.success() => Ok(()),
		Ok(out) => {
			let stderr = String::from_utf8_lossy(&out.stderr);
			let stdout = String::from_utf8_lossy(&out.stdout);
			Err(LinkError::LinkUnsupported {
				target: abs_target.to_path_buf(),
				link: link.to_path_buf(),
				source: io::Error::new(
					io::ErrorKind::Other,
					format!(
						"mklink /J {} {} failed: {} {}",
						link_norm.display(),
						target_norm.display(),
						stderr.trim(),
						stdout.trim()
					),
				),
			})
		}
		Err(source) => Err(LinkError::LinkUnsupported {
			target: abs_target.to_path_buf(),
			link: link.to_path_buf(),
			source,
		}),
	}
}
```

`normalize_path` is now used on Windows, so the `#[cfg_attr(not(windows), allow(dead_code))]` from Task 4 stays correct (dead only on non-Windows).

**Step 9.4 — Run, expect PASS (Linux compile-check; Windows CI runs the junction test).**

```bash
cargo test --package aghub-core --lib skills::linker -- --nocapture
cargo build --package aghub-core
```

Expected: Linux build + tests green (windows block excluded). On the windows-latest `just test` leg, `create_junction_makes_a_reparse_point_recognized_by_is_link` executes and passes.

**Step 9.5 — Lint + commit.**

```bash
cargo clippy --package aghub-core -- -D warnings
git add crates/core/src/skills/linker/mod.rs
git commit -m "feat(core): Windows create_link symlink_dir->junction + extracted create_junction

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 10: Convenience layer — `UniversalInstallReport`, `install_universal`, `link_agents_to_canonical`

**Step 10.1 — Write the failing tests.**
Append to `mod tests` in `crates/core/src/skills/linker/mod.rs`:

```rust
	fn make_source(base: &Path) -> PathBuf {
		let src = base.join("src/my-skill");
		std::fs::create_dir_all(&src).unwrap();
		std::fs::write(
			src.join("SKILL.md"),
			"---\nname: my-skill\ndescription: x\n---\nbody",
		)
		.unwrap();
		std::fs::create_dir_all(src.join("assets")).unwrap();
		std::fs::write(src.join("assets/a.txt"), "hello").unwrap();
		src
	}

	#[test]
	fn install_universal_materializes_master_and_links_each_agent() {
		use tempfile::tempdir;
		let tmp = tempdir().unwrap();
		let root = std::fs::canonicalize(tmp.path()).unwrap();
		let src = make_source(&root);
		let canonical = root.join(".agents/skills/my-skill");
		let claude = root.join(".claude/skills");

		let report = install_universal(
			&src,
			&canonical,
			std::slice::from_ref(&claude),
			LinkTarget::Absolute,
		)
		.unwrap();

		assert!(canonical.join("SKILL.md").exists());
		assert!(canonical.join("assets/a.txt").exists());
		let link = claude.join("my-skill");
		assert!(Linker::is_link(&link));
		assert_eq!(report.linked, vec![link]);
		assert!(report.already_linked.is_empty());
		assert!(report.conflicts.is_empty());
		assert!(report.failed.is_empty());
		assert_eq!(report.canonical, canonical);
	}

	#[test]
	fn install_universal_rejects_non_absolute_canonical() {
		let report = install_universal(
			Path::new("/does/not/matter"),
			Path::new("rel/.agents/skills/x"),
			&[PathBuf::from("/tmp/agent")],
			LinkTarget::Absolute,
		);
		let err = report.unwrap_err();
		assert!(matches!(err, LinkError::NonAbsoluteTarget { .. }));
	}

	#[test]
	fn link_agents_to_canonical_folds_per_agent_into_report() {
		use tempfile::tempdir;
		let tmp = tempdir().unwrap();
		let root = std::fs::canonicalize(tmp.path()).unwrap();
		let canonical = root.join(".agents/skills/my-skill");
		std::fs::create_dir_all(&canonical).unwrap();
		std::fs::write(canonical.join("SKILL.md"), "real").unwrap();
		let claude = root.join(".claude/skills");
		let occupied = claude.join("my-skill");
		std::fs::create_dir_all(&occupied).unwrap();
		std::fs::write(occupied.join("SKILL.md"), "pre").unwrap();

		let report = link_agents_to_canonical(
			&canonical,
			std::slice::from_ref(&claude),
			LinkTarget::Absolute,
		)
		.unwrap();

		assert!(report.linked.is_empty());
		assert_eq!(report.conflicts, vec![occupied]);
		assert!(report.failed.is_empty());
	}
```

**Step 10.2 — Run, expect FAIL.**

```bash
cargo test --package aghub-core install_universal_materializes_master_and_links_each_agent -- --exact
```

Expected: `error[E0422]: cannot find struct ... UniversalInstallReport` / `error[E0425]: cannot find function install_universal`.

**Step 10.3 — Minimal impl.**
Add the report struct and the two convenience fns above `pub struct Linker;` (or just below `LinkOutcome`) in `crates/core/src/skills/linker/mod.rs`:

```rust
/// What a symlink-only install did on disk. There is NO `copied_fallback`
/// field — the converged model bans copy. Per-agent hard failures land in
/// `failed` (Decision 10), never as an `Err` from the convenience layer.
#[derive(Debug, Default)]
pub struct UniversalInstallReport {
	/// `.agents/skills/<name>` master SKILL-DIR.
	pub canonical: PathBuf,
	/// Agent skills-dirs where a fresh link to the master was created.
	pub linked: Vec<PathBuf>,
	/// Agent skills-dirs where a correct link already existed (idempotent).
	pub already_linked: Vec<PathBuf>,
	/// Agent skills-dirs left untouched: a real file/dir or foreign link
	/// occupied the slot (never clobbered).
	pub conflicts: Vec<PathBuf>,
	/// Per-agent hard link failures (Decision 10): NOT propagated as `Err`.
	pub failed: Vec<(PathBuf, LinkError)>,
}

/// Materialize the Master from `source_root` (npx-identical copy + exclusions)
/// if absent, then link each agent skills-dir. A per-agent link hard-error is
/// collected into `report.failed`, NOT returned as `Err`. `Err(LinkError)` is
/// reserved for pre-link invariant violations (`NonAbsoluteTarget`) or the
/// Master copy itself failing.
pub fn install_universal(
	source_root: &Path,
	canonical: &Path,
	agent_skills_dirs: &[PathBuf],
	target: LinkTarget,
) -> Result<UniversalInstallReport, LinkError> {
	if !canonical.is_absolute() {
		return Err(LinkError::NonAbsoluteTarget {
			target: canonical.to_path_buf(),
		});
	}
	if !canonical.exists() {
		if let Some(parent) = canonical.parent() {
			std::fs::create_dir_all(parent)?;
		}
		copy_dir_recursive(source_root, canonical)?;
	}
	link_agents_to_canonical(canonical, agent_skills_dirs, target)
}

/// Link each agent skills-dir to an already-materialized Master. Same
/// per-agent-soft-fail contract as [`install_universal`].
pub fn link_agents_to_canonical(
	canonical: &Path,
	agent_skills_dirs: &[PathBuf],
	target: LinkTarget,
) -> Result<UniversalInstallReport, LinkError> {
	if !canonical.is_absolute() {
		return Err(LinkError::NonAbsoluteTarget {
			target: canonical.to_path_buf(),
		});
	}
	let name = canonical
		.file_name()
		.ok_or_else(|| {
			LinkError::Io(io::Error::new(
				io::ErrorKind::InvalidInput,
				format!(
					"canonical path has no final component: {}",
					canonical.display()
				),
			))
		})?
		.to_string_lossy()
		.into_owned();

	let mut report = UniversalInstallReport {
		canonical: canonical.to_path_buf(),
		..Default::default()
	};

	for agent_dir in agent_skills_dirs {
		let link_path = agent_dir.join(&name);
		match Linker::link(canonical, agent_dir, &name, target) {
			Ok(LinkOutcome::Linked) => report.linked.push(link_path),
			Ok(LinkOutcome::AlreadyLinked) => {
				report.already_linked.push(link_path)
			}
			Ok(LinkOutcome::Conflict) => report.conflicts.push(link_path),
			Err(e) => report.failed.push((link_path, e)),
		}
	}

	Ok(report)
}
```

**Step 10.4 — Run, expect PASS.**

```bash
cargo test --package aghub-core --lib skills::linker -- --nocapture
```

Expected: all linker tests pass, including the three new convenience-layer tests.

**Step 10.5 — Lint + commit.**

```bash
cargo clippy --package aghub-core -- -D warnings
git add crates/core/src/skills/linker/mod.rs
git commit -m "feat(core): convenience layer install_universal/link_agents_to_canonical (no copy)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 11: Relative-target leaf test + full-suite green gate

**Step 11.1 — Write the test (unix relative-form leaf, T-REL-PROJECT/T-ABS-GLOBAL).**
Append a unix leaf to `mod tests` in `crates/core/src/skills/linker/mod.rs`:

```rust
	#[cfg(unix)]
	#[test]
	fn relative_links_use_dotdot_global_links_are_absolute() {
		use tempfile::tempdir;
		let tmp = tempdir().unwrap();
		let root = std::fs::canonicalize(tmp.path()).unwrap();
		let src = make_source(&root);
		let canonical = root.join(".agents/skills/my-skill");
		let claude = root.join(".claude/skills");

		install_universal(
			&src,
			&canonical,
			std::slice::from_ref(&claude),
			LinkTarget::Relative,
		)
		.unwrap();
		let rel = std::fs::read_link(claude.join("my-skill")).unwrap();
		assert!(rel.is_relative(), "expected relative link, got {rel:?}");
		assert_eq!(rel, PathBuf::from("../../.agents/skills/my-skill"));

		let cursor = root.join(".cursor/skills");
		install_universal(
			&src,
			&canonical,
			std::slice::from_ref(&cursor),
			LinkTarget::Absolute,
		)
		.unwrap();
		let abs = std::fs::read_link(cursor.join("my-skill")).unwrap();
		assert!(abs.is_absolute(), "expected absolute link, got {abs:?}");
		assert_eq!(abs, canonical);
	}
```

**Step 11.2 — Run, expect PASS-by-behavior.**

```bash
cargo test --package aghub-core relative_links_use_dotdot_global_links_are_absolute -- --exact
```

Expected: PASS (behavior implemented in Tasks 6+10). The test derives both `canonical` and the `read_link` result from the already-canonicalized `root`, so macOS `/var`→`/private` cannot cause a mismatch.

**Step 11.3 — Minimal impl.**
No production change expected. If 11.2 failed on macOS due to a `/var`→`/private` mismatch, re-verify the test uses `root` (the canonicalized path), not `tmp.path()`, consistently — the code above does.

**Step 11.4 — Run the full linker suite + crate gate, expect PASS.**

```bash
cargo test --package aghub-core --lib skills::linker -- --nocapture
cargo test --package aghub-core
cargo clippy --package aghub-core -- -D warnings
```

Expected: all linker tests pass; the whole `aghub-core` suite still passes (`install_layout.rs` untouched, no caller broken); clippy clean.

**Step 11.5 — Commit.**

```bash
git add crates/core/src/skills/linker/mod.rs
git commit -m "test(core): pin relative/absolute link form; full aghub-core suite green

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Tasks 12–19 — Agent classifier (`crates/core/src/skills/linker/classify.rs`)

> `classify_agent` takes `&AgentDescriptor` (per the pinned API) but resolves read/write dirs via `create_adapter(AgentType::from_str(descriptor.id))` so the `SKILLS_PATH_OVERRIDE` thread-local that `TestConfig` uses is honored. `master_skills_dir` is the SKILLS-DIR (`.agents/skills`), NEVER the skill-dir — the central correctness pin. `canonicalize_lenient` is applied to BOTH sides to defeat the macOS `/var`->`/private` mismatch. The global NativeReader set `{Codex,OpenCode,Cursor,Cline,Warp}` is derived purely from each descriptor's read closure — the hardcoded list lives ONLY in the test as the expected oracle.

### Task 12: Create classify.rs with `LinkNeed` + `AgentLinkPlan` types (compile-only stub)

**Step 1 — write the file (types + stub fns).** Create `crates/core/src/skills/linker/classify.rs` (hard tabs, 80-col):

```rust
//! Agent auto-classification for symlink-only skill install.
//!
//! Derives — purely from each agent's RESOLVED read/write skills-dir paths,
//! never from `capabilities.skills.universal` and never from a hardcoded
//! agent list — whether an agent already reads the `.agents/skills` master
//! (NativeReader, no link needed), needs a per-agent link (NeedsLink), or
//! cannot hold skills at this scope (Unsupported).
//!
//! All comparisons are SKILLS-DIR vs SKILLS-DIR: `master_skills_dir` is the
//! `.agents/skills` store, NOT the `.agents/skills/<name>` skill-dir.

use crate::AgentType;
use aghub_agents::{AgentDescriptor, ResourceScope};
use std::path::{Path, PathBuf};
use std::str::FromStr;

/// Whether an agent needs a per-agent link to the `.agents/skills` master.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkNeed {
	/// Agent's own skills-dir at this scope IS or already READS
	/// `.agents/skills`: sees the master directly, NO link required.
	NativeReader,
	/// Agent has a private skills-dir not mapped to the master: needs a link.
	NeedsLink { agent_skills_dir: PathBuf },
	/// Agent's skills-dir cannot be resolved for this scope.
	Unsupported,
}

/// One agent's classification result for a given scope.
///
/// `reads_master` / `writes_master` are the REAL facts computed by the
/// classifier (does this agent's resolved read/write skills-dir resolve to the
/// `.agents/skills` master?), surfaced so the coverage DTO can report accurate
/// diagnostics (P2-G) instead of guessing. `need` is the 3-state derived from
/// them; the FE partitions on `need`, but the booleans are honest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentLinkPlan {
	pub agent_id: &'static str,
	pub need: LinkNeed,
	pub installed: bool,
	pub reads_master: bool,
	pub writes_master: bool,
}

/// Classify ONE agent against a scope + project_root + the canonical master
/// SKILLS-DIR (`.agents/skills`).
pub fn classify_agent(
	descriptor: &AgentDescriptor,
	scope: ResourceScope,
	project_root: Option<&Path>,
	master_skills_dir: &Path,
) -> AgentLinkPlan {
	let _ = (descriptor, scope, project_root, master_skills_dir);
	let _ = AgentType::from_str("claude");
	unimplemented!()
}

/// Classify ALL registered agents (`registry::ALL_AGENTS`).
pub fn classify_all(
	scope: ResourceScope,
	project_root: Option<&Path>,
	master_skills_dir: &Path,
) -> Vec<AgentLinkPlan> {
	let _ = (scope, project_root, master_skills_dir);
	unimplemented!()
}
```

**Step 2 — wire the module.** In `crates/core/src/skills/linker/mod.rs`, after the existing declarations (place `pub mod classify;` near the top, the re-export after it):

```rust
pub mod classify;
pub use classify::{classify_agent, classify_all, AgentLinkPlan, LinkNeed};
```

**Step 3 — run, expect compile success (types only).**

```bash
cargo build --package aghub-core
```

Expected: `Finished` with NO errors (the `unimplemented!()` bodies are reachable-but-unused). Do NOT run tests yet.

**Step 4 — commit.**

```bash
git add crates/core/src/skills/linker/classify.rs crates/core/src/skills/linker/mod.rs
git commit -m "feat(linker): scaffold classify.rs LinkNeed/AgentLinkPlan types

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 13: Failing test — Codex @global is a NativeReader

**Step 1 — write the failing test.** Append a test module to `crates/core/src/skills/linker/classify.rs`:

```rust
#[cfg(test)]
mod tests {
	use super::*;
	use crate::registry;
	use crate::skills::linker::universal_canonical_dir;

	fn plan_for(
		id: &str,
		scope: ResourceScope,
		project_root: Option<&Path>,
	) -> AgentLinkPlan {
		let master = universal_canonical_dir(project_root).unwrap();
		let descriptor = registry::ALL_AGENTS
			.iter()
			.find(|d| d.id == id)
			.unwrap_or_else(|| panic!("no descriptor for {id}"));
		classify_agent(descriptor, scope, project_root, &master)
	}

	#[test]
	fn codex_global_is_native_reader() {
		let plan = plan_for("codex", ResourceScope::GlobalOnly, None);
		assert_eq!(
			plan.need,
			LinkNeed::NativeReader,
			"codex reads ~/.agents/skills at global"
		);
		assert_eq!(plan.agent_id, "codex");
	}
}
```

**Step 2 — run, expect FAIL (panic in unimplemented).**

```bash
cargo test --package aghub-core skills::linker::classify::tests::codex_global_is_native_reader -- --exact
```

Expected FAIL with: `panicked at ... not implemented` (from `unimplemented!()` in `classify_agent`).

**Step 3 — implement `classify_agent`.** Replace the `classify_agent` stub body and add the private helper:

```rust
/// `fs::canonicalize(p)` falling back to `p` itself when the path does not
/// exist yet, so classification works before any dir is created. BOTH sides
/// of every comparison go through this to defeat the macOS `/var`->`/private`
/// prefix mismatch.
fn canonicalize_lenient(p: &Path) -> PathBuf {
	std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

pub fn classify_agent(
	descriptor: &AgentDescriptor,
	scope: ResourceScope,
	project_root: Option<&Path>,
	master_skills_dir: &Path,
) -> AgentLinkPlan {
	// Resolve read/write skills-dirs via the adapter so the
	// SKILLS_PATH_OVERRIDE thread-local (used by tests) is honored. The
	// descriptor id is always a valid AgentType string (it comes from the
	// registry); on the impossible parse failure, fall back to the
	// descriptor's own resolution so classification never panics.
	let (read_paths, write_dir) = match AgentType::from_str(descriptor.id) {
		Ok(agent_type) => {
			let adapter = crate::create_adapter(agent_type);
			(
				adapter.get_skills_paths(project_root, scope),
				adapter.target_skills_dir(project_root, scope),
			)
		}
		Err(_) => (
			descriptor.skill_read_paths(project_root, scope),
			descriptor.skill_write_path(project_root, scope),
		),
	};

	let canon = canonicalize_lenient(master_skills_dir);
	let reads_master = read_paths
		.iter()
		.any(|p| canonicalize_lenient(p) == canon);
	let writes_master =
		write_dir.as_ref().map(|p| canonicalize_lenient(p)) == Some(canon);

	let need = if reads_master || writes_master {
		LinkNeed::NativeReader
	} else if let Some(dir) = write_dir {
		LinkNeed::NeedsLink {
			agent_skills_dir: dir,
		}
	} else {
		LinkNeed::Unsupported
	};

	AgentLinkPlan {
		agent_id: descriptor.id,
		need,
		installed: crate::availability::check_agent_availability(descriptor)
			.is_available,
		// P2-G: surface the REAL facts so the coverage DTO is honest.
		reads_master,
		writes_master,
	}
}
```

> Verify the exact method/field names against current code before editing: `crate::create_adapter`, `AgentAdapter::get_skills_paths`, `AgentAdapter::target_skills_dir`, `AgentDescriptor::skill_read_paths`/`skill_write_path`, `crate::availability::check_agent_availability(...).is_available`, and `descriptor.id`. Use `codegraph_explore "create_adapter get_skills_paths target_skills_dir skill_read_paths check_agent_availability"` to confirm signatures; adapt the field/method names if any differ, keeping the algorithm identical.

**Step 4 — run, expect PASS.**

```bash
cargo test --package aghub-core skills::linker::classify::tests::codex_global_is_native_reader -- --exact
```

Expected: `test result: ok. 1 passed`.

**Step 5 — commit.**

```bash
git add crates/core/src/skills/linker/classify.rs
git commit -m "feat(linker): classify_agent NativeReader via resolved read/write dirs

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 14: Failing test — the full global NativeReader set {Codex, OpenCode, Cursor, Cline, Warp}

**Step 1 — write the test.** Inside the `tests` module, add:

```rust
	#[test]
	fn global_native_reader_set_matches_descriptors() {
		// Oracle (the AGENTS.md-documented global native set). This list is the
		// TEST expectation only — the impl derives it from descriptors.
		let expected_native = ["codex", "opencode", "cursor", "cline", "warp"];
		for id in expected_native {
			let plan = plan_for(id, ResourceScope::GlobalOnly, None);
			assert_eq!(
				plan.need,
				LinkNeed::NativeReader,
				"{id} should be a global NativeReader"
			);
		}
		// A clear non-native agent at global: Claude reads only ~/.claude/skills.
		let claude = plan_for("claude", ResourceScope::GlobalOnly, None);
		assert!(
			matches!(claude.need, LinkNeed::NeedsLink { .. }),
			"claude @global should NeedsLink, got {:?}",
			claude.need
		);
	}
```

**Step 2 — run, expect PASS (algorithm from Task 13 already covers this).**

```bash
cargo test --package aghub-core skills::linker::classify::tests::global_native_reader_set_matches_descriptors -- --exact
```

Expected: `test result: ok. 1 passed`. (Behavior-locking regression over the existing impl; no new production code. If it FAILS, the impl diverges from the documented set — fix the impl, not the test.)

**Step 3 — commit.**

```bash
git add crates/core/src/skills/linker/classify.rs
git commit -m "test(linker): pin global NativeReader set {codex,opencode,cursor,cline,warp}

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 15: Failing test — Amp/Kimi @global = NeedsLink, @project = NativeReader

**Step 1 — write the test.** Inside the `tests` module, add:

```rust
	#[test]
	fn amp_kimi_global_needs_link_but_project_is_native() {
		// At GLOBAL scope the universal flag appends the XDG dir
		// (~/.config/agents/skills), NOT ~/.agents/skills, so Amp/Kimi do NOT
		// read the global master and must be linked.
		for id in ["amp", "kimi"] {
			let plan = plan_for(id, ResourceScope::GlobalOnly, None);
			assert!(
				matches!(plan.need, LinkNeed::NeedsLink { .. }),
				"{id} @global should NeedsLink (XDG != ~/.agents/skills), \
				 got {:?}",
				plan.need
			);
		}

		// At PROJECT scope the universal flag appends
		// project_root/.agents/skills == the canonical master, so they ARE
		// NativeReaders.
		let tmp = tempfile::tempdir().unwrap();
		let root = std::fs::canonicalize(tmp.path()).unwrap();
		for id in ["amp", "kimi"] {
			let plan =
				plan_for(id, ResourceScope::ProjectOnly, Some(root.as_path()));
			assert_eq!(
				plan.need,
				LinkNeed::NativeReader,
				"{id} @project should be NativeReader (project .agents/skills \
				 == canonical)"
			);
		}
	}
```

**Step 2 — run, expect PASS (algorithm already covers it).**

```bash
cargo test --package aghub-core skills::linker::classify::tests::amp_kimi_global_needs_link_but_project_is_native -- --exact
```

Expected: `test result: ok. 1 passed`. If `amp`/`kimi` @global resolves to NativeReader, the impl is wrongly matching the XDG dir against the canonical — a real bug to fix in the impl. Do not weaken the test.

**Step 3 — commit.**

```bash
git add crates/core/src/skills/linker/classify.rs
git commit -m "test(linker): pin amp/kimi global=NeedsLink, project=NativeReader

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 16: Failing test — an agent with no skills support classifies as Unsupported

**Step 1 — write the test.** Inside the `tests` module, add:

```rust
	#[test]
	fn agent_without_skill_support_is_unsupported() {
		// jetbrains-ai has no skills scopes => no write dir, no master read.
		let plan = plan_for("jetbrains-ai", ResourceScope::GlobalOnly, None);
		assert_eq!(
			plan.need,
			LinkNeed::Unsupported,
			"jetbrains-ai @global should be Unsupported (no skills dir)"
		);
		let plan_p = {
			let tmp = tempfile::tempdir().unwrap();
			let root = std::fs::canonicalize(tmp.path()).unwrap();
			plan_for(
				"jetbrains-ai",
				ResourceScope::ProjectOnly,
				Some(root.as_path()),
			)
		};
		assert_eq!(
			plan_p.need,
			LinkNeed::Unsupported,
			"jetbrains-ai @project should be Unsupported"
		);
	}
```

**Step 2 — run, expect PASS (covered by the `else` arm of the impl).**

```bash
cargo test --package aghub-core skills::linker::classify::tests::agent_without_skill_support_is_unsupported -- --exact
```

Expected: `test result: ok. 1 passed`. If it FAILS because `jetbrains-ai` DOES expose a write dir, pick another registry agent whose `skill_write_path` returns `None` at both scopes (verify with a scratch assert `descriptor.skill_write_path(None, ResourceScope::GlobalOnly).is_none()`) and update the id — the impl is correct; only the chosen exemplar may need swapping.

**Step 3 — commit.**

```bash
git add crates/core/src/skills/linker/classify.rs
git commit -m "test(linker): pin Unsupported classification for skill-less agent

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 17: Failing test — `classify_all` + 3-state totality over the registry

**Step 1 — write the test.** Inside the `tests` module, add:

```rust
	fn assert_totality(plans: &[AgentLinkPlan]) {
		assert_eq!(
			plans.len(),
			registry::ALL_AGENTS.len(),
			"classify_all must cover every registered agent"
		);
		for plan in plans {
			let auto_covered = matches!(plan.need, LinkNeed::NativeReader);
			let needs_link = matches!(plan.need, LinkNeed::NeedsLink { .. });
			let unsupported = matches!(plan.need, LinkNeed::Unsupported);
			let count = [auto_covered, needs_link, unsupported]
				.iter()
				.filter(|b| **b)
				.count();
			assert_eq!(
				count, 1,
				"agent {} must be in exactly one bucket, got {:?}",
				plan.agent_id, plan.need
			);
		}
	}

	#[test]
	fn classify_all_is_total_at_global() {
		let master = universal_canonical_dir(None).unwrap();
		let plans = classify_all(ResourceScope::GlobalOnly, None, &master);
		assert_totality(&plans);
	}

	#[test]
	fn classify_all_is_total_at_project() {
		let tmp = tempfile::tempdir().unwrap();
		let root = std::fs::canonicalize(tmp.path()).unwrap();
		let master = universal_canonical_dir(Some(root.as_path())).unwrap();
		let plans = classify_all(
			ResourceScope::ProjectOnly,
			Some(root.as_path()),
			&master,
		);
		assert_totality(&plans);
	}
```

**Step 2 — run, expect FAIL (`classify_all` still `unimplemented!()`).**

```bash
cargo test --package aghub-core skills::linker::classify::tests::classify_all_is_total_at_global -- --exact
```

Expected FAIL with: `panicked at ... not implemented` (from `classify_all`).

**Step 3 — implement `classify_all`.** Replace the `classify_all` stub body with:

```rust
pub fn classify_all(
	scope: ResourceScope,
	project_root: Option<&Path>,
	master_skills_dir: &Path,
) -> Vec<AgentLinkPlan> {
	crate::registry::ALL_AGENTS
		.iter()
		.map(|descriptor| {
			classify_agent(descriptor, scope, project_root, master_skills_dir)
		})
		.collect()
}
```

**Step 4 — run BOTH totality tests, expect PASS.**

```bash
cargo test --package aghub-core skills::linker::classify::tests::classify_all_is_total -- --nocapture
```

Expected: `test result: ok. 2 passed`.

**Step 5 — commit.**

```bash
git add crates/core/src/skills/linker/classify.rs
git commit -m "feat(linker): classify_all over registry + 3-state totality tests

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 18: macOS /var->/private canonicalization regression test

**Step 1 — write the test.** Inside the `tests` module, add:

```rust
	#[test]
	fn classify_canonicalizes_both_sides() {
		let tmp = tempfile::tempdir().unwrap();
		// Deliberately use the RAW (possibly /var-symlinked) temp path as the
		// project_root, but a CANONICALIZED master skills-dir.
		let raw_root = tmp.path();
		let canon_root = std::fs::canonicalize(raw_root).unwrap();
		let master =
			universal_canonical_dir(Some(canon_root.as_path())).unwrap();
		let codex = registry::ALL_AGENTS
			.iter()
			.find(|d| d.id == "codex")
			.unwrap();
		let plan = classify_agent(
			codex,
			ResourceScope::ProjectOnly,
			Some(raw_root),
			&master,
		);
		assert_eq!(
			plan.need,
			LinkNeed::NativeReader,
			"codex @project must be NativeReader even when project_root is the \
			 raw (un-canonicalized) temp path and master is canonicalized"
		);
	}
```

**Step 2 — run, expect PASS (`canonicalize_lenient` on both sides already handles it).**

```bash
cargo test --package aghub-core skills::linker::classify::tests::classify_canonicalizes_both_sides -- --exact
```

Expected: `test result: ok. 1 passed`. On Linux this passes trivially; on macOS it pins the prefix-mismatch fix. If it FAILS on macOS, the impl canonicalized only one side — re-check that `reads_master`/`writes_master` both run each path through `canonicalize_lenient`.

**Step 3 — commit.**

```bash
git add crates/core/src/skills/linker/classify.rs
git commit -m "test(linker): canonicalize both sides (macOS /var->/private guard)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 19: Full-suite + clippy gate for the classifier

**Step 1 — run the whole linker classify suite.**

```bash
cargo test --package aghub-core skills::linker::classify -- --nocapture
```

Expected: all 7 classify tests pass — `codex_global_is_native_reader`, `global_native_reader_set_matches_descriptors`, `amp_kimi_global_needs_link_but_project_is_native`, `agent_without_skill_support_is_unsupported`, `classify_all_is_total_at_global`, `classify_all_is_total_at_project`, `classify_canonicalizes_both_sides` (7 passed, 0 failed).

**Step 2 — clippy (warnings = errors), expect clean.**

```bash
cargo clippy --package aghub-core -- -D warnings
```

Expected: `Finished` with no warnings. `use crate::AgentType;` IS used (by `AgentType::from_str`) — do not remove it.

**Step 3 — fmt check (hard tabs / 80-col).**

```bash
cargo fmt --package aghub-core -- --check
```

Expected: no diff. If a diff, run `cargo fmt --package aghub-core` and re-stage.

**Step 4 — commit any fmt/clippy fixups (skip if none).**

```bash
git add crates/core/src/skills/linker/classify.rs
git commit -m "chore(linker): clippy + fmt clean for classify.rs

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

(If Steps 1-3 produced no changes, skip this commit.)

---

## Tasks 20–35 — Core call-site rewiring (install_fetched / install_layout shim / manager.skill / removal / discovery; copy types KEPT as ignored shim until Task 47a)

> All `LinkError -> ConfigError` mappings below use `ConfigError::Io(std::io::Error::other(e.to_string()))` (Open Question 1 leaves a dedicated `ConfigError::Link` variant unlocked). If a later chunk adds `ConfigError::Link`, swap the `.map_err(...)` calls for cleaner messaging — behavior is unchanged either way. Per-agent `report.failed` entries are keyed back to their agent by `link.parent()` == the agent's skills-dir, which holds because `install_universal` builds each link path as `agent_skills_dir.join(name)`. Windows junction tests (`#[cfg(windows)]`) compile-out on the unix dev box and execute on the windows-latest `just test` leg.

### Task 20: Turn `install_layout.rs` into a thin RE-EXPORT shim of `linker/` (NON-breaking; workspace stays green)

Tasks 1–19 created `crates/core/src/skills/linker/` alongside the still-present `pub mod install_layout;`. This task makes `install_layout` a thin compatibility shim that re-exports the authoritative `linker` primitives, so every existing `install_layout::` caller (in `install_fetched.rs`, `manager/skill.rs`, the API, the CLI, and `crates/core/tests/sources_install_tests.rs`) keeps compiling. The file is NOT deleted here — that is Task 47a, after every consumer has been migrated to `linker::` + `LinkTarget`. **This is the P0-1 fix: the original plan deleted the file and committed broken; here the commit leaves the WHOLE workspace green.**

- [ ] Confirm `install_layout.rs` still exists: `ls crates/core/src/skills/install_layout.rs`.
- [ ] Replace the BODY of `crates/core/src/skills/install_layout.rs` (the moved-into-`linker` primitives `Linker`/`LinkTarget`/`LinkOutcome`/`LinkError`/`universal_canonical_dir`/`install_universal`/`link_agents_to_canonical`/`UniversalInstallReport`/`copy_dir_recursive` + `EXCLUDE_*`) with re-exports from `linker`, keeping ONLY the symbols that external consumers still name. The `SkillInstallLayout` enum + `install_isolated`/`resolve_target_dir` are NOT part of `install_layout` (they live in `install_fetched.rs`), so this file becomes:
    ```rust
    //! Backward-compatibility shim. The authoritative directory-link primitives
    //! now live in `crate::skills::linker`. This module re-exports them so the
    //! existing `install_layout::` call sites keep compiling during the
    //! symlink-only migration; it is DELETED in Task 47a once every consumer
    //! references `crate::skills::linker` directly.
    pub use crate::skills::linker::{
    	install_universal, link_agents_to_canonical, universal_canonical_dir,
    	LinkError, LinkOutcome, LinkTarget, Linker, UniversalInstallReport,
    };
    ```
    (Verify the exact set of symbols `install_layout` exports today before pruning — `grep -n "pub fn\|pub struct\|pub enum\|pub use" crates/core/src/skills/install_layout.rs` — and re-export every one that an external caller still names. `copy_dir_recursive`/`EXCLUDE_*` were file-private in `install_layout`, so they need NOT be re-exported unless a `grep -rn 'install_layout::copy_dir_recursive'` finds a caller; the canonical copy now lives in `linker/mod.rs` from Task 5.)
- [ ] In `crates/core/src/skills/mod.rs`, keep BOTH `pub mod install_layout;` (now the shim) and the `pub mod linker;` line added in Task 1.
- [ ] Run the WORKSPACE build (not just `-p aghub-core`) so any downstream consumer is caught now: `cargo build --workspace 2>&1 | tail -10`
- [ ] Expect: `Finished` — the shim keeps every `install_layout::` caller (core, API, CLI, `sources_install_tests.rs`) compiling. If a symbol an external caller names is missing from the re-export list, add it.
- [ ] Commit:
    ```bash
    git add -A && git commit -m "refactor(core): make skills::install_layout a re-export shim of skills::linker
    ```

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"

````

---

### Task 21: Repoint `install_fetched.rs` imports to `linker`; delete the LOCAL copy machinery (KEEP the shim fields) (RED compile)

> **Sequential RED chain (Tasks 21 → 22 → 23):** Tasks 21 and 22 deliberately end RED (no commit) — `aghub-core` does not compile again until Task 23 lands. Execute these three tasks **strictly in order in a single session**; do NOT dispatch them as independent/parallel subagents and do NOT run a between-task "is it green?" gate until after Task 23. The first green commit of this chunk is Task 23's. **The `layout` and `use_relative_links` request fields STAY (now ignored) — the `target: LinkTarget` swap is deferred to Task 47a so `sources_install_tests.rs`, the CLI, and the API keep compiling (P0-2).**

`install_fetched.rs:19-20` imports from `install_layout`; the request still carries `use_relative_links: bool` and a `layout` field. Re-point the import to `linker`, delete the LOCAL copy machinery (`install_isolated`/`resolve_target_dir`/local `copy_dir_recursive`), but KEEP the `SkillInstallLayout` enum and the `layout`/`use_relative_links` request fields as accepted-but-ignored shim fields.

- [ ] In `crates/core/src/skills/install_fetched.rs`, replace the import block (the `use crate::skills::install_layout::{...}` group plus the `use crate::adapters::create_adapter;` line) with:
```rust
use crate::models::ResourceScope;
use crate::skills::linker::classify::{classify_agent, LinkNeed};
use crate::skills::linker::{
	install_universal, universal_canonical_dir, LinkTarget,
};
use crate::skills::skill_source_root;
use crate::skills::update::{detect_rename, skill_renamed_message};
use aghub_agents::models::AgentType;
use skill::sanitize::sanitize_name;
````

(`create_adapter` import is dropped — `classify_agent` takes a descriptor via `registry::get`, and `resolve_target_dir` is deleted in Task 23. Keep `use std::path::Path;` and any other existing top-of-file imports.)

- [ ] Delete the local `copy_dir_recursive` (its doc comment + body, currently ~lines 27-51) — the canonical copy now lives in `linker/mod.rs` (Task 5).
- [ ] **KEEP** the `SkillInstallLayout` enum (currently ~lines 53-62) for now. Add a doc line marking it deprecated/ignored:
    ```rust
    	/// DEPRECATED shim — installs are always symlink-only now; this field is
    	/// accepted but IGNORED (the dispatch always uses install_universal_layout).
    	/// Removed in the final cleanup task once every caller drops it.
    ```
    (Place it above the enum and above the `pub layout` field below; the variants stay so external callers still construct `SkillInstallLayout::Universal`/`IsolatedCopy`.)
- [ ] **KEEP** the `pub layout: SkillInstallLayout,` and `pub use_relative_links: bool,` fields on `FetchedSkillInstallRequest` (they become ignored — Task 22's dispatch derives `LinkTarget` from `use_relative_links` so the existing relative/absolute semantics are preserved without an API break; Task 47a swaps them for `target: LinkTarget`).
- [ ] Run: `cargo build -p aghub-core 2>&1 | head -40`
- [ ] Expect FAIL — the `match req.layout` block still calls the now-deleted `install_isolated`, and `install_universal_layout` still has the old `use_relative_links: bool` param body referencing the deleted `resolve_target_dir`:
    ```
    error[E0425]: cannot find function `install_isolated`
    ```
    Expected; Task 22 rewrites the dispatch. (No commit yet — leave RED for Task 22.)

---

### Task 22: Rewrite `install_fetched_skill_and_lock` dispatch + lock gate (Decision 11); derive `LinkTarget` from the kept `use_relative_links` shim

Collapse the layout match into the single universal path (ignoring `req.layout`) and change the lock gate to `(wrote_master || installed_any)`.

- [ ] In `install_fetched_skill_and_lock`, replace the `let (agent_results, copied_any) = match req.layout { ... };` block with the unconditional universal path. Derive `LinkTarget` from the still-present `use_relative_links` field so the relative/absolute behavior is preserved without an API change (`req.layout` is now ignored):
    ```rust
    	// Symlink-only: req.layout is ignored (shim field, removed in Task 47a);
    	// the install is ALWAYS the universal master+link path.
    	let target = if req.use_relative_links {
    		LinkTarget::Relative
    	} else {
    		LinkTarget::Absolute
    	};
    	let (agent_results, wrote_master) = install_universal_layout(
    		&source_root,
    		&safe_name,
    		req.scope,
    		req.project_root,
    		req.target_agents,
    		target,
    	)?;
    ```
- [ ] Replace the lock-gate block:
    ```rust
    	let installed_any = agent_results.iter().any(|r| r.installed);
    	let wrote_lock = installed_any
    		&& should_write_install_lock(
    			&name,
    			copied_any,
    			req.scope,
    			req.project_root,
    		);
    ```
    with (Decision 11 — write when the Master was freshly materialized OR any agent received the skill):
    ```rust
    	let installed_any = agent_results.iter().any(|r| r.installed);
    	let wrote_lock = (wrote_master || installed_any)
    		&& should_write_install_lock(
    			&name,
    			wrote_master || installed_any,
    			req.scope,
    			req.project_root,
    		);
    ```
    (`should_write_install_lock(name, copy_gate_bool, scope, project_root)` is the existing 4-arg helper — keep its signature; only the boolean argument changes.)
- [ ] Run: `cargo build -p aghub-core 2>&1 | head -40`
- [ ] Expect FAIL — `install_isolated` is now dead/uncalled AND `install_universal_layout` still has the old `use_relative_links: bool` param + `resolve_target_dir` body (`expected LinkTarget, found bool`). Task 23 deletes `install_isolated` + rewrites `install_universal_layout`. (No commit yet.)

---

### Task 23: Rewrite `install_universal_layout` to use `classify_agent` + fold `report.failed`

Replace the `resolve_target_dir` write-only partition with the full classifier and fold per-agent `LinkError`s into result rows (Decision 10).

- [ ] Delete `install_isolated` and `resolve_target_dir` entirely.
- [ ] Replace the whole `install_universal_layout` fn with:

    ```rust
    /// Returns the per-agent results plus `wrote_master` — `true` only when the
    /// canonical master was NEWLY written on this run. NativeReader agents are
    /// reported installed with NO link; NeedsLink agents are linked via the
    /// copy-free linker; Unsupported agents soft-fail. A per-agent LinkError is
    /// folded into that agent's row (Decision 10), never aborting the install.
    fn install_universal_layout(
    	source_root: &Path,
    	safe_name: &str,
    	scope: ResourceScope,
    	project_root: Option<&Path>,
    	target_agents: &[AgentType],
    	target: LinkTarget,
    ) -> Result<(Vec<AgentInstallResult>, bool), crate::ConfigError> {
    	let canonical_root = if matches!(scope, ResourceScope::ProjectOnly) {
    		project_root
    	} else {
    		None
    	};
    	let Some(canonical_skills_dir) = universal_canonical_dir(canonical_root)
    	else {
    		let results = target_agents
    			.iter()
    			.map(|&agent| AgentInstallResult {
    				agent,
    				installed: false,
    				error: Some(
    					"Cannot resolve .agents canonical directory".to_string(),
    				),
    			})
    			.collect();
    		return Ok((results, false));
    	};
    	let canonical = canonical_skills_dir.join(safe_name);
    	let wrote_master = !canonical.exists();

    	// Classify every target agent against the canonical SKILLS-DIR (not the
    	// SKILL-DIR). `plans[i]` pairs 1:1 with `target_agents[i]`.
    	let plans: Vec<LinkNeed> = target_agents
    		.iter()
    		.map(|&agent| {
    			let descriptor = crate::registry::get(agent);
    			classify_agent(
    				descriptor,
    				scope,
    				project_root,
    				&canonical_skills_dir,
    			)
    			.need
    		})
    		.collect();
    	let symlink_dirs: Vec<std::path::PathBuf> = plans
    		.iter()
    		.filter_map(|need| match need {
    			LinkNeed::NeedsLink { agent_skills_dir } => {
    				Some(agent_skills_dir.clone())
    			}
    			_ => None,
    		})
    		.collect();

    	// Copy-free install: materialize the Master (if absent) and link each
    	// NeedsLink agent. A pre-link invariant violation (NonAbsoluteTarget) or a
    	// Master-copy failure returns Err; per-agent link failures land in
    	// report.failed.
    	let report =
    		install_universal(source_root, &canonical, &symlink_dirs, target)
    			.map_err(|e| {
    				crate::ConfigError::Io(std::io::Error::other(e.to_string()))
    			})?;
    	// Per-agent link errors keyed by the agent's skills-dir (the link parent).
    	let failed_by_dir: std::collections::HashMap<
    		std::path::PathBuf,
    		String,
    	> = report
    		.failed
    		.iter()
    		.filter_map(|(link, err)| {
    			link.parent().map(|p| (p.to_path_buf(), err.to_string()))
    		})
    		.collect();
    	// P1-D: a conflict (an occupied real dir, or a foreign link in the agent's
    	// skills-dir) is NOT a successful install — it was never clobbered. Fold
    	// report.conflicts by the agent skills-dir too, so a NeedsLink agent whose
    	// slot is occupied is reported `installed:false` with an error, never a
    	// silent `installed:true`.
    	let conflict_dirs: std::collections::HashSet<std::path::PathBuf> = report
    		.conflicts
    		.iter()
    		.filter_map(|link| link.parent().map(|p| p.to_path_buf()))
    		.collect();

    	let results = target_agents
    		.iter()
    		.zip(plans.iter())
    		.map(|(&agent, need)| match need {
    			LinkNeed::NativeReader => AgentInstallResult {
    				agent,
    				installed: true,
    				error: None,
    			},
    			LinkNeed::NeedsLink { agent_skills_dir } => {
    				if let Some(msg) = failed_by_dir.get(agent_skills_dir) {
    					AgentInstallResult {
    						agent,
    						installed: false,
    						error: Some(msg.clone()),
    					}
    				} else if conflict_dirs.contains(agent_skills_dir) {
    					AgentInstallResult {
    						agent,
    						installed: false,
    						error: Some(
    							"A real directory or a foreign link already \
    							 occupies this skill slot; it was not overwritten"
    								.to_string(),
    						),
    					}
    				} else {
    					AgentInstallResult {
    						agent,
    						installed: true,
    						error: None,
    					}
    				}
    			}
    			LinkNeed::Unsupported => AgentInstallResult {
    				agent,
    				installed: false,
    				error: Some(
    					"Agent does not support persistent skill creation in \
    					 this scope"
    						.to_string(),
    				),
    			},
    		})
    		.collect();

    	Ok((results, wrote_master))
    }
    ```

    > **Note on `AlreadyLinked`:** `report.already_linked` is idempotent success — the agent's slot already holds a correct link to this Master — and does NOT appear in either `failed_by_dir` or `conflict_dirs`, so the `else` arm correctly reports it `installed:true`. Only `failed` (hard link errors) and `conflicts` (occupied slots) demote an agent to `installed:false`.

- [ ] Run the WORKSPACE build so the still-present shim fields keep every consumer (core tests, CLI source.rs, API) compiling: `cargo build --workspace 2>&1 | tail -10`
- [ ] Expect: `Finished` — `aghub-core` compiles AND the downstream `sources_install_tests.rs`/CLI/API still build against the kept `layout`/`use_relative_links` shim fields. (The fields are now ignored by the dispatch; Task 23a updates the core integration tests' assertions, Task 35c the CLI, Tasks 41–42 the API, and Task 47a finally removes the fields.)
- [ ] Commit:
    ```bash
    git add -A && git commit -m "refactor(core): symlink-only install_fetched via classify_agent + linker
    ```

Delete install_isolated/local copy_dir_recursive; partition target agents
with classify_agent (NativeReader/NeedsLink/Unsupported); fold per-agent
LinkError AND conflicts into AgentInstallResult (Decisions 10 + P1-D); lock
gate becomes (wrote_master || installed_any) (Decision 11). The dispatch is
always-universal; the layout/use_relative_links request fields remain as an
ignored shim until the final cleanup task (workspace stays green).

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"

````

---

### Task 24: no-copy regression test for `install_fetched_skill_and_lock`

Prove the install primitive produces a true link to the Master, never a copy.

- [ ] First confirm the `InstallLockSource` variant shape: `grep -n "pub enum InstallLockSource" -A8 crates/skill/src/*.rs`. Use the actual variant in the test below (the assertion is link-vs-copy, not the lock source).
- [ ] Append to `crates/core/src/skills/install_fetched.rs` (add a `#[cfg(all(test, unix))] mod nocopy_tests` at the end):
```rust
#[cfg(all(test, unix))]
mod nocopy_tests {
	use super::*;
	use crate::skills::linker::Linker;
	use std::fs;
	use tempfile::tempdir;

	// T-NOCOPY (install_fetched): a NeedsLink agent receives a real symlink
	// to the Master, never a copy. Writing a sentinel into the Master AFTER
	// install and reading it back THROUGH the link proves it is a link.
	#[test]
	fn install_fetched_links_master_never_copies() {
		let tmp = tempdir().unwrap();
		let src = tmp.path().join("src/my-skill");
		fs::create_dir_all(&src).unwrap();
		fs::write(
			src.join("SKILL.md"),
			"---\nname: my-skill\ndescription: d\n---\nbody",
		)
		.unwrap();
		let root = tmp.path().canonicalize().unwrap();
		let req = FetchedSkillInstallRequest {
			skill_file: &src.join("SKILL.md"),
			source: &skill::InstallLockSource::Local {
				path: src.to_string_lossy().to_string(),
			},
			lock_skill_path: "my-skill/SKILL.md".to_string(),
			ref_commit: None,
			scope: ResourceScope::ProjectOnly,
			project_root: Some(&root),
			target_agents: &[AgentType::Claude],
			expected_name: None,
			// Shim fields (ignored by the always-universal dispatch; Task 47a
			// swaps them for `target: LinkTarget`). `use_relative_links: true`
			// preserves the project-scope relative-link behavior under test.
			layout: SkillInstallLayout::Universal,
			use_relative_links: true,
		};
		let report = install_fetched_skill_and_lock(req).unwrap();
		assert_eq!(report.name, "my-skill");

		let canonical = root.join(".agents/skills/my-skill");
		let link = root.join(".claude/skills/my-skill");
		assert!(Linker::is_link(&link), "agent dir must hold a link");
		fs::write(canonical.join("sentinel.txt"), "live").unwrap();
		assert_eq!(
			fs::read_to_string(link.join("sentinel.txt")).unwrap(),
			"live",
			"reading through the link must see the Master => not a copy"
		);
	}
}
````

(Adapt the `FetchedSkillInstallRequest` field set + `InstallLockSource` variant to the EXACT current shapes if they differ — the load-bearing assertion is the sentinel-through-link.)

- [ ] Run: `cargo test -p aghub-core install_fetched_links_master_never_copies -- --exact 2>&1 | tail -20`
- [ ] Expect PASS (the symlink-only impl from Task 23 satisfies it).
- [ ] Commit:
    ```bash
    git add -A && git commit -m "test(core): no-copy regression for install_fetched (link, not copy)
    ```

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"

````

---

### Task 23a: Migrate `crates/core/tests/sources_install_tests.rs` to symlink-only assertions (still on the shim fields)

`crates/core/tests/sources_install_tests.rs` constructs `FetchedSkillInstallRequest` at 8 call sites using `layout: SkillInstallLayout::{IsolatedCopy,Universal}` + `use_relative_links`. After Task 22 the dispatch IGNORES `layout` and is always symlink-only, so any test there that asserted an `IsolatedCopy` produced a real COPY in the agent's own dir is now WRONG (it gets a link to the `.agents` master instead). This task updates that integration suite to assert the symlink-only outcome, while it still compiles against the kept shim fields (the `target: LinkTarget` swap is Task 47a). **This must land in the SAME chunk that changed the dispatch (P0-2), before the API/CLI chunks, so `cargo test -p aghub-core` is green from here on.**

- [ ] Re-read the suite: `sed -n '1,60p' crates/core/tests/sources_install_tests.rs` and list every assertion that depends on copy-vs-link (`grep -n "IsolatedCopy\|use_relative_links\|copied\|is_symlink\|canonical_path\|\.claude/skills\|\.agents/skills\|assert" crates/core/tests/sources_install_tests.rs`).
- [ ] For each of the 8 `FetchedSkillInstallRequest { … }` literals: KEEP the `layout` and `use_relative_links` shim fields exactly as they are (so the file still compiles — Task 47a removes them). Do NOT change them to `target:` here.
- [ ] Update the per-test ASSERTIONS so each is correct under symlink-only:
- Any test that asserted an `IsolatedCopy` install put a real `SKILL.md` COPY directly in the agent's own dir (e.g. `<root>/.claude/skills/<name>/SKILL.md` is a real file, `canonical_path.is_none()`) must instead assert the symlink-only layout: `<root>/.agents/skills/<name>/SKILL.md` exists (Master) AND the agent dir entry is a link (`aghub_core::skills::linker::Linker::is_link(&<agent_dir>/<name>)` on unix) with `canonical_path.is_some()`.
- Tests that already asserted the `Universal` (master+link) layout keep their assertions (behavior is unchanged for those).
- Any assertion on `report.agent_results[..].installed` for a previously-copying agent stays `installed:true` IF that agent is a NeedsLink agent with a clean slot; verify against the classifier (a native-reader agent is `installed:true` with no link).
- [ ] Gate with the WORKSPACE-aware core test run (this is the suite the broken-commit finding was about): `cargo test -p aghub-core --test sources_install_tests 2>&1 | tail -25`
- [ ] Expect PASS — every call site compiles against the shim fields and every assertion reflects the symlink-only outcome. If a test still asserts a copy, fix the TEST (the impl is intentionally symlink-only now).
- [ ] Commit:
```bash
git add -A && git commit -m "test(core): migrate sources_install_tests to symlink-only assertions

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
````

---

### Task 25: `manager/skill.rs` — repoint imports + `add_skill_from_path` AND `add_skill` → master+link

`add_skill_from_path` currently copies the source into the agent's own dir; `add_skill` (manual-create) currently writes an isolated agent-local `SKILL.md`. Both must converge on the symlink-only model (Locked Decision 1, spec line 401-402). Make `add_skill_from_path` a thin wrapper over `add_skill_from_path_universal`, and `add_skill` a thin wrapper over `add_skill_universal` (both already write a Master + link this agent). Repoint every `install_layout::` reference to `linker::`.

- [ ] Replace the manual-create body of `add_skill` (`manager/skill.rs:122-151`, verified live) — the `if let Some(dir) = target_dir { … create_dir_all … write SKILL.md … canonical_path = None … } else { Err(...) }` block — with a delegating wrapper to `add_skill_universal`. The duplicate-name guard, the no-persistent-scope error, and `save_current` all already live inside `add_skill_universal`, so the wrapper carries no logic of its own:
    ```rust
    	pub fn add_skill(&mut self, skill: Skill) -> Result<()> {
    		// Symlink-only model (Locked Decision 1): manual skill creation writes a
    		// single .agents/skills/<name> Master and links THIS agent to it, exactly
    		// like every other install path. The old isolated agent-local copy body is
    		// removed; there is no copy install path. (`add_skill_universal` already
    		// holds the duplicate-name guard, the unsupported-scope error, and
    		// `save_current`; `universal_install_prep` resolves the agent name.)
    		self.add_skill_universal(skill)
    	}
    ```
    Note: the old `add_skill` set `canonical_path = None` (copy provenance); `add_skill_universal` sets `canonical_path = Some(<master>/SKILL.md)` (link provenance) — this is the intended change so layout-aware removal recognises the link. Verify no in-crate caller of `add_skill` asserts `canonical_path.is_none()` after creation (grep `crates/core/tests/integration_tests.rs` + `test_agent_paths.rs` for `add_skill(`; if a test asserts the old copy layout, update that TEST to the master+link layout, not the impl).
- [ ] Replace the copy body of `add_skill_from_path` with a delegating wrapper:
    ```rust
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
    ```
- [ ] In `add_skill_from_path_universal`, replace the `crate::skills::install_layout::install_universal(...)` call with the `linker::` path + `LinkTarget`:
    ```rust
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
    ```
- [ ] In `add_skill_universal`, replace `crate::skills::install_layout::link_agents_to_canonical(...)` with the `linker::` + `LinkTarget` form (same args, wrap with `if use_relative {Relative} else {Absolute}` and `.map_err(|e| ConfigError::Io(std::io::Error::other(e.to_string())))?`).
- [ ] In `universal_install_prep`, replace `crate::skills::install_layout::universal_canonical_dir(...)` with `crate::skills::linker::universal_canonical_dir(...)` (same args).
- [ ] In `universal_relink_agents`, replace `crate::skills::install_layout::link_agents_to_canonical(new_canonical, referrers, use_relative)` with the `linker::` + `LinkTarget` form + the same `.map_err`.
- [ ] In `rollback_master_rename`, replace `crate::skills::install_layout::link_agents_to_canonical(old_master, referrers, use_relative)?` with the `linker::` + `LinkTarget` form, but inside the `std::io::Result` closure map the error to `io::Error`:
    ```rust
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
    ```
- [ ] Run: `cargo build -p aghub-core 2>&1 | head -30` → expect `Finished`. Any remaining `install_layout::` reference names its exact line (`error[E0433]: could not find install_layout in skills`).
- [ ] Run the existing manager + integration skill tests (both exercise `add_skill`/`add_skill_from_path`): `cargo test -p aghub-core --lib manager::skill 2>&1 | tail -20 && cargo test -p aghub-core --test integration_tests 2>&1 | tail -20 && cargo test -p aghub-core --test test_agent_paths 2>&1 | tail -20` → expect PASS. If an `add_skill` test now fails because it asserted the old copy layout (`canonical_path.is_none()` / a copy in the agent's own dir), update that TEST to the master+link layout per the note above — the impl change is intended.
- [ ] Commit:
    ```bash
    git add -A && git commit -m "refactor(core): manager add_skill + add_skill_from_path are symlink-only; repoint to linker
    ```

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"

````

---

### Task 26: `manager/skill.rs::remove_skill_path` — `is_symlink()`→`Linker::is_link`, `remove_file`→`Linker::unlink`

A Windows junction reports `is_symlink()==false`, so the current probe orphans it. Swap both. The param is named `is_symlink`; rename to `is_link` for clarity, updating its one caller.

- [ ] Add `use crate::skills::linker::Linker;` near the top imports of `manager/skill.rs`.
- [ ] Rename the `remove_skill_path` param `is_symlink: bool` → `is_link: bool` and the `if is_symlink {` → `if is_link {`.
- [ ] Replace the probe + removal inside the `if is_link {` block:
```rust
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
````

- [ ] At the call site, rename the local `let is_symlink = existing_skill.canonical_path.is_some();` to `let is_link = existing_skill.canonical_path.is_some();` and pass `is_link` as the third arg.
- [ ] Run: `cargo build -p aghub-core 2>&1 | head -20` → expect `Finished`.
- [ ] Run the existing unix removal tests: `cargo test -p aghub-core --lib manager::skill::tests::remove_skill_path 2>&1 | tail -15` → expect PASS.
- [ ] Commit:
    ```bash
    git add -A && git commit -m "fix(core): remove_skill_path unlinks junctions via Linker (was is_symlink)
    ```

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"

````

---

### Task 27: T-REMOVE-SKILL-PATH-JUNCTION (windows)

Pin that `remove_skill_path` removes a junction referrer and leaves the Master intact.

- [ ] In the `#[cfg(test)] mod tests` of `crates/core/src/manager/skill.rs`, add (near the existing `remove_skill_path_*` tests):
```rust
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
````

(Verify `remove_skill_path`'s arg order/types against the current signature before writing; match it exactly.)

- [ ] Run: `cargo test -p aghub-core --lib manager::skill 2>&1 | tail -10` → expect PASS on unix (test compiled-out; no regression). windows-latest CI executes it.
- [ ] Commit:
    ```bash
    git add -A && git commit -m "test(core): T-REMOVE-SKILL-PATH-JUNCTION (windows junction removal)
    ```

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"

````

---

### Task 28: `manager/skill.rs` relink helpers — `is_symlink()`→`Linker::is_link`

Three relink probes skip a junction referrer on rename. Swap them.

- [ ] In `universal_relink_referrers`, replace the predicate with:
```rust
			Linker::is_link(&link)
				&& std::fs::canonicalize(&link)
					.map(|resolved| resolved == *old_real)
					.unwrap_or(false)
````

- [ ] In `universal_relink_agents`, replace the `if meta.file_type().is_symlink() { std::fs::remove_file(&old_link)... }` block with:
    ```rust
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
    ```
    (Replaces the whole `if let Ok(meta) = symlink_metadata { if is_symlink { remove_file } }` nest — `Linker::unlink` is idempotent on a missing path.)
- [ ] In `rollback_master_rename`'s `do_rollback` closure, replace the `if let Ok(meta) ... if meta.file_type().is_symlink() { std::fs::remove_file(&new_link)?; }` block with:
    ```rust
    			let new_link = dir.join(safe_new);
    			if Linker::is_link(&new_link) {
    				Linker::unlink(&new_link)?;
    			}
    ```
- [ ] Run: `cargo build -p aghub-core 2>&1 | head -20` → expect `Finished`.
- [ ] Run the rename transaction tests: `cargo test -p aghub-core --lib manager::skill 2>&1 | tail -15` → expect PASS.
- [ ] Commit:
    ```bash
    git add -A && git commit -m "fix(core): relink helpers recognize junctions via Linker::is_link
    ```

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"

````

---

### Task 29: `removal.rs::execute_removal` — `is_symlink()`→`Linker::is_link`, `remove_file`→`Linker::unlink`

`execute_removal` only unlinks a unix symlink; a junction falls into the `is_dir()` branch → `remove_dir_all` recurses into the shared Master = data loss. Swap the probe + removal, keeping the link check BEFORE the `is_dir()` branch.

- [ ] Add `use crate::skills::linker::Linker;` to the imports of `crates/core/src/skills/removal.rs`.
- [ ] Replace the symlink branch in `execute_removal`:
```rust
		let ft = meta.file_type();
		if ft.is_symlink() {
			match std::fs::remove_file(path) {
				Ok(()) => report.removed.push(path.clone()),
				Err(e) => report.failed.push((path.clone(), e)),
			}
		} else if ft.is_dir() {
````

with:

```rust
		let ft = meta.file_type();
		if Linker::is_link(path) {
			match Linker::unlink(path) {
				Ok(()) => report.removed.push(path.clone()),
				Err(e) => report.failed.push((path.clone(), e)),
			}
		} else if ft.is_dir() {
```

(The `assert_contained` guard + `remove_dir_all` for real dirs stay unchanged with their TOCTOU re-check; `Linker::unlink` does NOT bypass it.)

- [ ] Run: `cargo build -p aghub-core 2>&1 | head -20` → expect `Finished`.
- [ ] Run the existing execute_removal tests: `cargo test -p aghub-core --lib removal::tests::execute_removal 2>&1 | tail -15` → expect PASS.
- [ ] Commit:
    ```bash
    git add -A && git commit -m "fix(core): execute_removal unlinks junctions before is_dir branch (no remove_dir_all into Master)
    ```

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"

````

---

### Task 30: `removal.rs::plan_symlink_removal` + `dir_has_external_referrer` — `is_symlink()`→`Linker::is_link`

Two planning probes and the external-referrer guard skip junction referrers. Swap all three.

- [ ] In `plan_symlink_removal`, the canonical-resolved branch: replace `if meta.file_type().is_symlink() && targeted {` with `if Linker::is_link(&entry) && targeted {`.
- [ ] In the dangling/unresolvable branch: replace `if meta.file_type().is_symlink() && targeted {` with `if Linker::is_link(&entry) && targeted {`. (The `let Ok(meta) = symlink_metadata(&entry) else { continue; }` existence gate stays; if clippy flags `meta` unused, rename to `_meta`.)
- [ ] In `dir_has_external_referrer`, replace:
```rust
		let Ok(meta) = std::fs::symlink_metadata(&entry) else {
			continue;
		};
		if !meta.file_type().is_symlink() {
			continue;
		}
````

with:

```rust
		if !Linker::is_link(&entry) {
			continue;
		}
```

- [ ] Run: `cargo clippy -p aghub-core --all-targets 2>&1 | grep -E "warning|error" | head -20` → expect no unused-`meta` warning (apply the `_meta` rename if flagged). Expect clean.
- [ ] Run the existing plan_removal tests: `cargo test -p aghub-core --lib removal::tests::plan_removal 2>&1 | tail -20` → expect PASS.
- [ ] Commit:
    ```bash
    git add -A && git commit -m "fix(core): removal planner + external-referrer guard recognize junctions
    ```

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"

````

---

### Task 31: T-PLAN-JUNCTION-REFERRER + T-EXTERNAL-JUNCTION-REFERRER (windows)

Pin the two planning-path swaps from Task 30.

- [ ] In the `#[cfg(test)] mod tests` of `removal.rs`, add (match the existing test helpers' names — `write_skill_md`, `plan_removal`, `Layout::Symlink`, `Skill::new` — verify before writing):
```rust
	// T-PLAN-JUNCTION-REFERRER: a targeted junction referrer is planned for
	// unlink (not orphaned). windows-latest.
	#[cfg(windows)]
	#[test]
	fn plan_symlink_removal_schedules_junction_referrer() {
		use crate::skills::linker::create_junction;
		let tmp = tempdir().unwrap();
		let canonical = tmp.path().join(".agents/skills/foo");
		write_skill_md(&canonical);
		let claude = tmp.path().join(".claude/skills");
		std::fs::create_dir_all(&claude).unwrap();
		let link = claude.join("foo");
		create_junction(&canonical.canonicalize().unwrap(), &link).unwrap();

		let agent_dirs = vec![claude.clone()];
		let mut skill = Skill::new("foo");
		skill.canonical_path = Some(
			canonical.join("SKILL.md").to_string_lossy().to_string(),
		);
		let plan = plan_removal(
			&skill,
			Some(claude.as_path()),
			&agent_dirs,
			Some(tmp.path()),
			false,
		);
		assert_eq!(plan.layout, Layout::Symlink);
		assert!(
			plan.paths.contains(&link),
			"junction referrer must be planned for unlink, got {:?}",
			plan.paths
		);
	}

	// T-EXTERNAL-JUNCTION-REFERRER: dir_has_external_referrer sees a junction,
	// so a shared Master with a live junction referrer is NOT removed.
	// windows-latest.
	#[cfg(windows)]
	#[test]
	fn dir_has_external_referrer_detects_junction() {
		use crate::skills::linker::create_junction;
		let tmp = tempdir().unwrap();
		let master = tmp.path().join(".agents/skills/foo");
		write_skill_md(&master);
		let claude = tmp.path().join(".claude/skills");
		std::fs::create_dir_all(&claude).unwrap();
		create_junction(
			&master.canonicalize().unwrap(),
			&claude.join("foo"),
		)
		.unwrap();

		let agent_dirs = vec![claude.clone()];
		assert!(
			dir_has_external_referrer(&master, &agent_dirs, "foo"),
			"a junction referrer must count as an external referrer"
		);
	}
````

(Adapt `plan_removal`/`dir_has_external_referrer` arg lists to the real signatures.)

- [ ] Run: `cargo test -p aghub-core --lib removal 2>&1 | tail -10` → expect PASS on unix (windows tests compiled-out; existing tests green).
- [ ] Commit:
    ```bash
    git add -A && git commit -m "test(core): T-PLAN-JUNCTION-REFERRER + T-EXTERNAL-JUNCTION-REFERRER
    ```

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"

````

---

### Task 32: `discovery.rs` — `is_symlink()`→`Linker::is_link` + set canonical_path for junctions

`collect_skills` marks `canonical_path` only when `meta.file_type().is_symlink()`. A junction is rediscovered as a copy. Swap to `Linker::is_link`.

- [ ] Add `use crate::skills::linker::Linker;` to the imports of `crates/core/src/skills/discovery.rs`.
- [ ] Replace the detection block in `collect_skills`:
```rust
				// Detect symlink and record canonical path
				if let Ok(meta) = path.symlink_metadata() {
					if meta.file_type().is_symlink() {
						if let Ok(resolved) = fs::canonicalize(&path) {
							let canonical = resolved.join("SKILL.md");
							skill.canonical_path =
								crate::format_path_with_tilde(&canonical);
						}
					}
				}
````

with:

```rust
				// Detect a link (unix symlink OR windows junction) and record the
				// canonical path. A junction reports is_symlink()==false, so the
				// bare file-type check missed it; Linker::is_link sees both.
				if Linker::is_link(&path) {
					if let Ok(resolved) = fs::canonicalize(&path) {
						let canonical = resolved.join("SKILL.md");
						skill.canonical_path =
							crate::format_path_with_tilde(&canonical);
					}
				}
```

- [ ] Run: `cargo build -p aghub-core 2>&1 | head -15` → expect `Finished`.
- [ ] Run the existing discovery tests: `cargo test -p aghub-core --lib skills::discovery 2>&1 | tail -10` → expect PASS.
- [ ] Commit:
    ```bash
    git add -A && git commit -m "fix(core): discovery sets canonical_path for junction installs via Linker::is_link
    ```

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"

````

---

### Task 33: T-DISCOVERY-JUNCTION-CANONICAL (windows)

Pin the discovery swap: a junction install is discovered with `canonical_path` set.

- [ ] In the `#[cfg(test)] mod tests` of `discovery.rs`, add (match the real discovery entry — `load_skills_from_dir` or `collect_skills`):
```rust
	// T-DISCOVERY-JUNCTION-CANONICAL: a junction install is recognized as a
	// referrer (canonical_path set), not rediscovered as a plain copy.
	// windows-latest.
	#[cfg(windows)]
	#[test]
	fn discovery_sets_canonical_path_for_junction() {
		use crate::skills::linker::create_junction;
		let tmp = tempfile::tempdir().unwrap();
		let master = tmp.path().join(".agents/skills/foo");
		std::fs::create_dir_all(&master).unwrap();
		std::fs::write(
			master.join("SKILL.md"),
			"---\nname: foo\ndescription: d\n---\n",
		)
		.unwrap();
		let claude = tmp.path().join(".claude/skills");
		std::fs::create_dir_all(&claude).unwrap();
		create_junction(
			&master.canonicalize().unwrap(),
			&claude.join("foo"),
		)
		.unwrap();

		let skills = load_skills_from_dir(&claude);
		let foo = skills
			.iter()
			.find(|s| s.name == "foo")
			.expect("junction install must be discovered");
		assert!(
			foo.canonical_path.is_some(),
			"a junction must set canonical_path (recognized as a referrer)"
		);
	}
````

- [ ] Run: `cargo test -p aghub-core --lib skills::discovery 2>&1 | tail -10` → expect PASS on unix (windows test compiled-out).
- [ ] Commit:
    ```bash
    git add -A && git commit -m "test(core): T-DISCOVERY-JUNCTION-CANONICAL (junction recognized as referrer)
    ```

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"

````

---

### Task 34: no-copy regression on `add_skill_from_path` AND `add_skill` (unix)

Prove BOTH install entry points now produce a Master + link, not a copy in the agent dir: `add_skill_from_path` (import-from-path) and `add_skill` (manual-create, the GAP-2 conversion in Task 25).

- [ ] Before writing, run `grep -n "impl TestConfig\|pub fn builder\|pub fn manager\|set_skills_path_override\|pub fn new" crates/core/src/testing.rs` and match the EXACT constructors. If `TestConfig` cannot produce a project-scope `ConfigManager` with a temp project root, mirror the setup of the sibling `remove_skill_path_removes_contained_dir` test in this module.
- [ ] In the `#[cfg(test)] mod tests` of `crates/core/src/manager/skill.rs`, add (illustrative builder API — adapt to the real one):
```rust
	// no-copy regression: add_skill_from_path writes a .agents Master and a
	// link in the agent dir, never a private copy (Locked Decision 1).
	#[cfg(unix)]
	#[test]
	fn add_skill_from_path_links_master_not_copy() {
		use crate::skills::linker::Linker;
		let tc = crate::testing::TestConfig::builder()
			.agent(crate::models::AgentType::Claude)
			.scope(crate::models::ResourceScope::ProjectOnly)
			.build();
		let src = tc.temp_path().join("src/my-skill");
		std::fs::create_dir_all(&src).unwrap();
		std::fs::write(
			src.join("SKILL.md"),
			"---\nname: my-skill\ndescription: d\n---\nbody",
		)
		.unwrap();

		let mut mgr = tc.manager();
		mgr.add_skill_from_path(&src.join("SKILL.md")).unwrap();

		let project = tc.project_root();
		let canonical = project.join(".agents/skills/my-skill");
		let link = project.join(".claude/skills/my-skill");
		assert!(canonical.join("SKILL.md").exists(), "Master materialized");
		assert!(
			Linker::is_link(&link),
			"agent dir must hold a link to the Master, not a copy"
		);
	}

	// GAP-2 no-copy regression: add_skill (manual-create) writes a .agents
	// Master and a link in the agent dir, never a private copy, and records
	// canonical_path (link provenance) — proving the Task 25 add_skill ->
	// add_skill_universal delegation (Locked Decision 1).
	#[cfg(unix)]
	#[test]
	fn add_skill_manual_create_links_master_not_copy() {
		use crate::skills::linker::Linker;
		let tc = crate::testing::TestConfig::builder()
			.agent(crate::models::AgentType::Claude)
			.scope(crate::models::ResourceScope::ProjectOnly)
			.build();

		let mut mgr = tc.manager();
		let skill = crate::models::Skill::new("manual-skill".to_string());
		mgr.add_skill(skill).unwrap();

		let project = tc.project_root();
		let canonical = project.join(".agents/skills/manual-skill");
		let link = project.join(".claude/skills/manual-skill");
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
````

(Adjust `Skill::new`'s arg shape — `String` vs `&str` — to the live signature; grep `pub fn new` in `crates/agents`/`crates/core` models if the constructor differs.)

- [ ] Run: `cargo test -p aghub-core --lib add_skill_from_path_links_master_not_copy -- --exact 2>&1 | tail -20 && cargo test -p aghub-core --lib add_skill_manual_create_links_master_not_copy -- --exact 2>&1 | tail -20` → expect both PASS. If either FAILS asserting `Linker::is_link(&link)` is false, re-check that Task 25's wrappers delegate (`add_skill_from_path` → `add_skill_from_path_universal`; `add_skill` → `add_skill_universal`).
- [ ] Commit:
    ```bash
    git add -A && git commit -m "test(core): no-copy regression for add_skill_from_path + add_skill (Master + link)
    ```

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"

````

---

### Task 35: Full core gate — tests green + only the expected SHIM copy symbols survive

The WHOLE workspace must still build (the shim keeps API/CLI/`sources_install_tests.rs` compiling). `install_layout` and `SkillInstallLayout`/`use_relative_links` intentionally STILL EXIST as the ignored shim — they are removed in Task 47a, not here.

- [ ] Verify `install_layout` survives ONLY as the re-export shim (not as primitives): `grep -rn "install_layout" crates/core/src` → expect hits ONLY in `skills/mod.rs` (the `pub mod install_layout;` line) and `skills/install_layout.rs` (the shim file's own re-exports). NO `install_layout::` primitive call sites should remain in other files (Task 25 repointed them to `linker::`).
- [ ] Verify the dead copy MECHANICS are gone (these are removed in Task 21/23), but the shim TYPE survives: `grep -rn "install_isolated\|resolve_target_dir\|copied_fallback\|CopiedFallback" crates/core/src || echo "NONE"` → expect `NONE`. `SkillInstallLayout`/`IsolatedCopy` STILL appear (the ignored shim) — that is expected until Task 47a; do not delete them here.
- [ ] Run the full core test suite: `cargo test -p aghub-core 2>&1 | tail -25` → expect all PASS (including the migrated `sources_install_tests.rs` from Task 23a).
- [ ] Run the workspace build to confirm no downstream crate broke: `cargo build --workspace 2>&1 | tail -5` → expect `Finished`.
- [ ] Run clippy: `cargo clippy -p aghub-core --all-targets -- -D warnings 2>&1 | tail -15` → expect no warnings/errors.
- [ ] Run fmt check: `cargo fmt -p aghub-core -- --check 2>&1 | tail -10` → expect clean. If formatting reported, run `cargo fmt -p aghub-core`.
- [ ] Commit any formatting fixups:
```bash
git add -A && git commit -m "chore(core): fmt/clippy gate for symlink-only call-site rewiring

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
````

---

## Tasks 35a–35c — CLI layer (`crates/cli`: `--universal` deprecation no-op + collapse copy branches in BOTH `add` and `source sync`)

> **Why this chunk exists / ordering (GAP-1 + P0-3):** `crates/cli` is part of the workspace, so `cargo test --workspace` compiles and runs it. After Task 25 made `add_skill`/`add_skill_universal` and `add_skill_from_path`/`add_skill_from_path_universal` converge on the symlink-only model, the CLI's live `if universal { …universal } else { …copy }` branches in `add::execute` (`crates/cli/src/commands/add.rs`) still call the now-duplicated pair AND keep a user-visible `--universal` flag whose `false` value implied the banned copy behaviour (Locked Decision 1, spec §"CLI" line 455-457, Decision 3 / Open Question 3). This chunk runs **after** core is green (Task 35) and **before** the API chunk (Task 36) — core symbols it depends on already exist, and it does not depend on any API/DTO change. Spec line 457: install-from-path → always `add_skill_from_path`; rename re-add → always `add_skill`; manual-create → always `add_skill`; keep `--universal` as a clap flag so existing scripts don't error with unknown-arg, but ignore its value.
>
> **`source sync` IS on the install surface (P0-3 — corrects the earlier "out of scope" claim).** The `Sync` command's `universal: bool` (`main.rs:309` → SyncArgs.universal `source.rs:335` → `apply_install` arg `source.rs:685`) drives a live `let layout = if universal { Universal } else { IsolatedCopy };` copy branch at `source.rs:706` that constructs `FetchedSkillInstallRequest` with `SkillInstallLayout`. That is precisely the install surface Decision 1 governs (Sources install). After Task 22 the core dispatch already ignores `layout`, so `source sync` without `--universal` no longer copies — but `source.rs` still NAMES `SkillInstallLayout`/`layout`/`use_relative_links`, and `crates/cli` must be in the final no-copy grep (Task 66). Task 35c collapses that branch to the always-universal request and makes `source sync --universal` a no-op, BEFORE Task 47a removes the shim symbols (otherwise Task 47a would break `source.rs`).

### Task 35a: CLI `add::execute` — collapse copy branches; make `--universal` a deprecation no-op

The `--universal` clap flag stays (no unknown-arg break for callers) but its value is ignored. The three `if universal { … } else { … }` branches in `add::execute` each collapse to the single symlink-only call.

- [ ] Before editing, re-verify the live shape: `grep -n "universal\|add_skill" crates/cli/src/commands/add.rs` and `grep -n "universal" crates/cli/src/main.rs`. The `Add`-command flag is `main.rs:139-140`; the `add::execute` branches are `add.rs:37-41` (install-from-path), `add.rs:52-56` (rename re-add), `add.rs:72-76` (manual-create). The `Sync` flag at `main.rs:309` is NOT touched.
- [ ] In `crates/cli/src/main.rs`, mark the **Add** command's `--universal` flag deprecated-but-accepted. Replace the doc + `#[arg(long)]` at `main.rs:135-140` (the one inside the `Add` variant) with:
    ```rust
    		/// DEPRECATED — no-op. Installs are always symlink-only now: a single
    		/// `.agents/skills/<name>` master plus a per-agent link (npx-style). The
    		/// flag is accepted (so existing scripts don't error) but ignored; there
    		/// is no copy install path.
    		#[arg(long, hide = true)]
    		universal: bool,
    ```
    Leave the dispatch wiring in `main.rs:435-464` unchanged (it still passes `universal` through to `add::execute`; the value is now ignored downstream — keeping the param avoids a wider signature churn this task).
- [ ] In `crates/cli/src/commands/add.rs`, change the `universal` param to a deprecation no-op and collapse the three branches. Keep the param in the signature (called positionally from `main.rs`), but underscore-prefix it so clippy does not flag it unused:
    - Rename the param at `add.rs:26` from `universal: bool,` to `_universal: bool,`.
    - Replace the install-from-path block (`add.rs:32-41`) — the verbose line referencing `universal` and the `if universal { add_skill_from_path_universal } else { add_skill_from_path }` — with:
        ```rust
        				eprintln_verbose!(
        					"Importing skill from: {}",
        					from_path.display()
        				);
        				let mut skill =
        					manager.add_skill_from_path(&from_path)?;
        ```
    - Replace the rename re-add block (`add.rs:52-56`) — the `if universal { add_skill_universal } else { add_skill }` — with:
        ```rust
        					manager.add_skill(skill.clone())?;
        ```
    - Replace the manual-create block (`add.rs:72-76`) — the `if universal { add_skill_universal } else { add_skill }` — with:
        ```rust
        				manager.add_skill(skill.clone())?;
        ```
        After this, `add.rs` no longer references `add_skill_from_path_universal` or `add_skill_universal` (those remain public on `ConfigManager`; `add_skill`/`add_skill_from_path` now delegate to them per Task 25, so behaviour is identical and there is exactly one install model).
- [ ] Build the CLI: `cargo build -p aghub 2>&1 | head -30` → expect `Finished`. A `warning: unused variable: universal` would mean the underscore rename was missed; an `error[E0599] ... add_skill_from_path_universal` would mean a branch was not collapsed.
- [ ] Clippy the CLI: `cargo clippy -p aghub --all-targets -- -D warnings 2>&1 | tail -15` → expect no warnings/errors.
- [ ] Commit:
    ```bash
    git add -A && git commit -m "feat(cli): symlink-only add; --universal becomes a hidden deprecation no-op
    ```

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"

````

---

### Task 35b: CLI test — `add skill --from` produces a master+link, `--universal` accepted as a no-op

Pin the converged behaviour end-to-end through the CLI: an install-from-path writes a `.agents` master + a link (never an isolated copy), and passing the legacy `--universal` flag still succeeds (accepted, ignored).

- [ ] Before writing, re-read the existing CLI test harness: `grep -n "Command::cargo_bin\|fn aghub_cli\|fn .*test\|tempdir\|env(" crates/cli/tests/cli_tests.rs | head -40`. The harness (verified live) imports `use assert_cmd::Command;` and exposes a helper `fn aghub_cli() -> Command` that calls `Command::cargo_bin("aghub-cli")` and sets `HOME`/`USERPROFILE`/`APPDATA` to a fixtures dir for isolation. The test below builds its OWN isolated command so it can point `HOME` at a per-test temp dir (global-scope writes must not leak to the real home / fixtures); reuse `Command::cargo_bin("aghub-cli")` exactly. Match the closest existing `add`-based test and adapt the snippet below to that harness rather than inventing a new fixture.
- [ ] Add to `crates/cli/tests/cli_tests.rs` (`#[cfg(unix)]` — it asserts a real symlink; on Windows the junction path is covered by the core `#[cfg(windows)]` tests):
```rust
// Symlink-only install: `aghub add skill --from <dir>` writes a single
// .agents/skills/<name> master and a link in the agent's own dir — never an
// isolated copy. The legacy `--universal` flag is accepted but ignored (no
// unknown-arg error, identical result), proving the deprecation no-op.
#[cfg(unix)]
#[test]
fn cli_add_skill_from_path_is_symlink_only() {
	let tmp = tempfile::tempdir().unwrap();
	let project = tmp.path();
	// Agent marker so project-root detection picks this dir.
	std::fs::create_dir_all(project.join(".claude")).unwrap();
	let src = project.join("src/my-skill");
	std::fs::create_dir_all(&src).unwrap();
	std::fs::write(
		src.join("SKILL.md"),
		"---\nname: my-skill\ndescription: d\n---\nbody",
	)
	.unwrap();

	// Project scope; isolate HOME so nothing leaks to the real ~/.agents.
	let mut cmd =
		assert_cmd::Command::cargo_bin("aghub-cli").unwrap();
	cmd.env("HOME", project)
		.env("USERPROFILE", project)
		.env("APPDATA", project)
		.current_dir(project)
		.args(["-a", "claude", "-p", "add", "skill", "--from"])
		.arg(src.join("SKILL.md"));
	cmd.assert().success();

	let canonical = project.join(".agents/skills/my-skill");
	let link = project.join(".claude/skills/my-skill");
	assert!(
		canonical.join("SKILL.md").exists(),
		"a .agents master must be materialized"
	);
	assert!(
		std::fs::symlink_metadata(&link)
			.map(|m| m.file_type().is_symlink())
			.unwrap_or(false),
		"the agent dir must hold a link, not a copy"
	);

	// Legacy `--universal` flag: accepted (no unknown-arg error). Use a fresh
	// skill name so the duplicate-name guard does not reject it.
	let src2 = project.join("src/other-skill");
	std::fs::create_dir_all(&src2).unwrap();
	std::fs::write(
		src2.join("SKILL.md"),
		"---\nname: other-skill\ndescription: d\n---\nbody",
	)
	.unwrap();
	let mut cmd2 =
		assert_cmd::Command::cargo_bin("aghub-cli").unwrap();
	cmd2.env("HOME", project)
		.env("USERPROFILE", project)
		.env("APPDATA", project)
		.current_dir(project)
		.args(["-a", "claude", "-p", "add", "skill", "--universal", "--from"])
		.arg(src2.join("SKILL.md"));
	cmd2.assert().success();
	assert!(
		std::fs::symlink_metadata(project.join(".claude/skills/other-skill"))
			.map(|m| m.file_type().is_symlink())
			.unwrap_or(false),
		"--universal must be accepted and produce the same symlink result"
	);
}
````

(Verify the exact binary name passed to `cargo_bin` — `grep -n "cargo_bin" crates/cli/tests/cli_tests.rs` — and the project-scope invocation flags against the sibling tests; some harnesses set HOME/cwd differently. Adjust the TEST to the harness, not the impl.)

- [ ] Run: `cargo test -p aghub --test cli_tests cli_add_skill_from_path_is_symlink_only -- --exact 2>&1 | tail -20` → expect PASS. If it fails on `cargo_bin`, fix the binary name; if it fails the symlink assertion, re-check Task 25's `add_skill_from_path` delegation and Task 35a's branch collapse.
- [ ] Commit:
    ```bash
    git add -A && git commit -m "test(cli): pin symlink-only add-from-path + --universal no-op acceptance
    ```

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"

````

---

### Task 35c: CLI `source sync` — collapse `apply_install` copy branch; `--universal` becomes a no-op (P0-3)

`crates/cli/src/commands/source.rs::apply_install` (the `source sync` install helper) imports `SkillInstallLayout` and builds the request with `let layout = if universal { Universal } else { IsolatedCopy };` + `use_relative_links: matches!(scope, ProjectOnly)`. Collapse the copy branch so the Sources install path is symlink-only, and make the `Sync` command's `--universal` flag a no-op. Because Task 22 kept the `layout`/`use_relative_links` request fields as an ignored shim, this task only needs to STOP branching on `universal`; it keeps passing the shim fields (Task 47a removes them workspace-wide). After this, `source.rs` no longer makes a copy-vs-universal DECISION, satisfying the no-copy grep's inclusion of `crates/cli` (Task 66).

- [ ] Re-verify the live shape: `grep -n "universal\|SkillInstallLayout\|layout\|use_relative_links\|fn apply_install\|struct SyncArgs" crates/cli/src/commands/source.rs`. Confirmed live anchors: the `apply_install` `universal: bool` param at `source.rs:685`, the `let layout = if universal {…} else {…}` at `source.rs:706-710`, the request literal at `source.rs:712-723`, and the `apply_install(…, args.universal, …)` call at `source.rs:515`.
- [ ] In `crates/cli/src/main.rs`, mark the **Sync** command's `--universal` flag (`main.rs:308-309`, the `#[arg(long)] universal: bool,` inside the `Sync` variant) deprecated-but-accepted:
```rust
		/// DEPRECATED — no-op. `source sync` is always symlink-only now (a single
		/// `.agents/skills/<name>` master plus per-agent links). Accepted so
		/// existing scripts don't error, but ignored; there is no copy install.
		#[arg(long, hide = true)]
		universal: bool,
````

Leave the `universal: *universal` pass-through at `main.rs:147` unchanged (it still flows into `SyncArgs`; the value is now ignored downstream).

- [ ] In `crates/cli/src/commands/source.rs::apply_install`, underscore-prefix the now-unused param (`source.rs:685`): `universal: bool,` → `_universal: bool,`.
- [ ] Replace the `let layout = if universal { SkillInstallLayout::Universal } else { SkillInstallLayout::IsolatedCopy };` block (`source.rs:706-710`) by DELETING it, and in the `FetchedSkillInstallRequest { … }` literal keep the shim fields with the always-universal values:
    ```rust
    		layout: SkillInstallLayout::Universal,
    		use_relative_links: matches!(scope, ResourceScope::ProjectOnly),
    ```
    (The `use crate::…install_fetched::{…, SkillInstallLayout}` import at `source.rs:688-691` stays until Task 47a, which removes `SkillInstallLayout` workspace-wide and converts these to `target: LinkTarget`.)
- [ ] Build + clippy the CLI: `cargo build -p aghub 2>&1 | head -20` and `cargo clippy -p aghub --all-targets -- -D warnings 2>&1 | tail -15` → expect `Finished`, no warnings. A `warning: unused variable: universal` means the underscore rename was missed.
- [ ] (Optional, if a `source sync` integration test fixture exists) add a `#[cfg(unix)]` test that `source sync --install-missing --yes` produces a master+link (and that adding `--universal` yields the identical result). If no offline `source sync` fixture is reachable without network, rely on the core no-copy coverage (Task 24) + the no-copy grep (Task 66) instead and note that here — do NOT add a network-dependent test.
- [ ] Commit:
    ```bash
    git add -A && git commit -m "feat(cli): source sync is symlink-only; --universal becomes a hidden no-op (P0-3)
    ```

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"

````

---

## Tasks 36–47 — API layer (symlink-only install routes + `/skills/coverage` + DTO enrichment + absolute project-root precondition)

> Preconditions (Tasks 1–35c must be merged): `aghub_core::skills::linker` exists with `Linker::is_link`, `LinkTarget::{Relative,Absolute}`, `universal_canonical_dir`, `classify::{classify_all, classify_agent, LinkNeed, AgentLinkPlan}`; and `install_fetched_skill_and_lock`'s dispatch is symlink-only (always `install_universal_layout`, folding `report.failed` AND `report.conflicts`, lock gate = `wrote_master||installed_any`). **`FetchedSkillInstallRequest` still carries the ignored `layout`/`use_relative_links` shim fields at this point** — they are removed only in Task 47a. The API route tasks below therefore have two options for the request literal: (a) keep passing `layout: SkillInstallLayout::Universal` + `use_relative_links: matches!(scope, ProjectOnly)` and let Task 47a convert them with every other consumer, OR (b) note that the route is migrated to `target: LinkTarget` in Task 47a. **This plan takes option (a):** Tasks 41/42 below DROP the `req.universal`/`layout` DECISION logic (the copy branch) and pass the always-universal shim values, then Task 47a does the field rename across all consumers at once — so no API commit ever breaks the build. The API test harness uses in-file `#[cfg(test)] mod tests` calling async handlers via a `block_on` helper + `with_isolated_env`; new route tests follow that pattern and are `#[cfg(unix)]` where they redirect HOME. Several tasks carry "verify against current code" notes (GitCloneSessions/Session fields, global lock path, `file://` source scheme, export-dto helper shape, rocket builder name) — adjust the TEST, not the impl.

### Task 36: Absolutize project_root in `extractors.rs` (P0-C)

**Step 36.1 — Write the failing test.** Append to `crates/api/src/extractors.rs` (end of file):

```rust
#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn resolve_project_absolutizes_relative_root() {
		let params = ScopeParams {
			scope: Some("project".to_string()),
			project_root: Some("relative/proj".to_string()),
		};
		match params.resolve().expect("resolves") {
			ResolvedScope::Project { root } => assert!(
				root.is_absolute(),
				"project root must be absolutized, got {}",
				root.display()
			),
			_ => panic!("expected Project scope"),
		}
	}
}
````

(If `ScopeParams`'s field set differs, match it. `ResolvedScope` has no `Debug` — the `_ =>` arm avoids formatting it.)

**Step 36.2 — Run, expect FAIL.**

```bash
cargo test --package aghub-api resolve_project_absolutizes_relative_root -- --exact
```

Expect FAIL: `root must be absolutized` (resolve builds `PathBuf::from(root)` verbatim).

**Step 36.3 — Minimal impl.** In `crates/api/src/extractors.rs`, replace the `"project"` arm's `Ok(ResolvedScope::Project { root: PathBuf::from(root) })` with `Ok(ResolvedScope::Project { root: absolutize_root(root) })`, and add this helper above `impl ScopeParams`:

```rust
/// Resolve a possibly-relative project root to an ABSOLUTE path so the
/// universal-master canonical dir is absolute (junction targets require it —
/// spec Decision 6 / P0-C). Uses `canonicalize` when the path exists, else
/// joins onto the current dir without requiring existence.
pub fn absolutize_root(root: &str) -> PathBuf {
	let p = PathBuf::from(root);
	if p.is_absolute() {
		return p;
	}
	if let Ok(canon) = std::fs::canonicalize(&p) {
		return canon;
	}
	std::env::current_dir()
		.map(|cwd| cwd.join(&p))
		.unwrap_or(p)
}
```

**Step 36.4 — Run, expect PASS.**

```bash
cargo test --package aghub-api resolve_project_absolutizes_relative_root -- --exact
```

Expect PASS.

**Step 36.5 — Commit.**

```bash
git add crates/api/src/extractors.rs
git commit -m "feat(api): absolutize project_root in ScopeParams::resolve (P0-C)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 36a: Absolutize the raw `project_root` in `delete_skill_by_path` (P1-F — skills.rs:238)

The spec pins raw project-root handling at `skills.rs:238` for the delete path. Live, `delete_skill_by_path` (`crates/api/src/routes/skills.rs:199`) does `let project_root = req.project_root.as_ref().map(std::path::PathBuf::from);` at `:238` — a verbatim `PathBuf::from` with NO absolutization, unlike the install routes Task 36 fixes via `absolutize_root`. A relative `project_root` here makes the canonical-master/referrer resolution operate on a non-absolute path, so a junction/symlink referrer in a project deleted with a relative root can be mis-resolved. Apply the same `absolutize_root` treatment for consistency with the install surface (Decision 6 / P0-C).

**Step 36a.1 — Write the failing test.** Append to `crates/api/src/routes/skills.rs` `mod tests` (reuse `with_isolated_env` + `block_on` + the existing `by_path_req`-style helper):

```rust
	#[cfg(unix)]
	#[test]
	fn delete_by_path_absolutizes_relative_project_root() {
		with_isolated_env(|home, _state| {
			// A project with a .claude marker + a symlinked install.
			let proj = home.join("proj");
			let master = proj.join(".agents/skills/linked");
			std::fs::create_dir_all(&master).unwrap();
			std::fs::write(
				master.join("SKILL.md"),
				"---\nname: linked\ndescription: d\n---\n",
			)
			.unwrap();
			let skills = proj.join(".claude/skills");
			std::fs::create_dir_all(&skills).unwrap();
			let link = skills.join("linked");
			std::os::unix::fs::symlink(&master, &link).unwrap();

			// Drive delete with a RELATIVE project_root resolved against cwd.
			let prev = std::env::current_dir().unwrap();
			std::env::set_current_dir(home).unwrap();
			// Build the request with scope=project, project_root="proj"
			// (relative), path = the link, layout-aware = true. Adapt to the
			// real request type / helper used by the sibling delete tests.
			let req =
				by_path_req_with_scope(&link, Some(true), "project", "proj");
			let resp = block_on(delete_skill_by_path(Json(req)))
				.ok()
				.expect("handler ok")
				.into_inner();
			std::env::set_current_dir(prev).unwrap();

			assert!(resp.success, "delete must resolve the relative root");
			assert!(!link.exists(), "referrer link removed");
			assert!(
				master.join("SKILL.md").exists(),
				"shared master must survive"
			);
		});
	}
```

> Adapt the request constructor to the real `DeleteSkillByPathRequest` shape + the sibling delete-test helper (`grep -n "by_path_req\|DeleteSkillByPathRequest\|fn delete_skill_by_path" crates/api/src/routes/skills.rs`). If no scope-carrying helper exists, build the request literal inline with `scope: "project"`, `project_root: Some("proj".into())`. If the request type cannot carry a relative root meaningfully (e.g. the path is already absolute and project_root is only used for lock pruning), this test instead pins that the relative root is absolutized before the prune/containment step — assert no `project_root`-derived path error in `resp`.

**Step 36a.2 — Run, expect FAIL** (relative root resolved verbatim → containment/prune misses or errors):

```bash
cargo test --package aghub-api delete_by_path_absolutizes_relative_project_root -- --exact
```

**Step 36a.3 — Minimal impl.** In `crates/api/src/routes/skills.rs::delete_skill_by_path`, replace `:238`:

```rust
	let project_root = req.project_root.as_ref().map(std::path::PathBuf::from);
```

with the same absolutization the install routes use:

```rust
	let project_root = req
		.project_root
		.as_ref()
		.map(|r| crate::extractors::absolutize_root(r));
```

**Step 36a.4 — Run, expect PASS.**

```bash
cargo test --package aghub-api delete_by_path -- --nocapture
```

Expect all delete-by-path tests green (this one + the existing ones).

**Step 36a.5 — Commit.**

```bash
git add crates/api/src/routes/skills.rs
git commit -m "fix(api): absolutize project_root in delete_skill_by_path (P1-F, skills.rs:238)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 37: `AgentSkillCoverageDto` (serde shape)

**Step 37.1 — Create the DTO + failing test.** Create `crates/api/src/dto/agent_coverage.rs`:

```rust
use serde::Serialize;
use ts_rs::TS;

/// Per-agent skill-coverage projection for `GET /api/v1/skills/coverage`.
/// `needs_link`/`auto_covered`/`supported` are the FE-partitioning projection of
/// the `LinkNeed` 3-state; `reads_master`/`writes_master` are the REAL
/// classifier facts (whether the agent's resolved read/write skills-dir
/// resolves to the `.agents/skills` master), carried verbatim from
/// `AgentLinkPlan` (P2-G) rather than guessed. No raw paths are exposed. The
/// frontend partitions on `auto_covered`/`needs_link`; the master booleans are
/// honest diagnostics.
#[derive(Debug, Serialize, TS)]
#[ts(export)]
pub struct AgentSkillCoverageDto {
	pub id: String,
	pub scope: String,
	pub reads_master: bool,
	pub writes_master: bool,
	pub needs_link: bool,
	pub auto_covered: bool,
	pub supported: bool,
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn serializes_with_expected_keys() {
		let dto = AgentSkillCoverageDto {
			id: "claude".to_string(),
			scope: "global".to_string(),
			reads_master: false,
			writes_master: false,
			needs_link: true,
			auto_covered: false,
			supported: true,
		};
		let json = serde_json::to_string(&dto).unwrap();
		assert_eq!(
			json,
			r#"{"id":"claude","scope":"global","reads_master":false,"writes_master":false,"needs_link":true,"auto_covered":false,"supported":true}"#
		);
	}
}
```

Wire the module: in `crates/api/src/dto/mod.rs` add `pub mod agent_coverage;` (keep alphabetical).

**Step 37.2 — Run, expect PASS.**

```bash
cargo test --package aghub-api serializes_with_expected_keys -- --exact
```

Expect PASS (`serde_json` is already a dep via rocket). If `unresolved import serde_json`, add it under `[dev-dependencies]` in `crates/api/Cargo.toml`.

**Step 37.3 — Commit.**

```bash
git add crates/api/src/dto/agent_coverage.rs crates/api/src/dto/mod.rs
git commit -m "feat(api): add AgentSkillCoverageDto for /skills/coverage

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 38: `/skills/coverage` handler (buckets per scope)

**Step 38.1 — Create the handler + failing tests.** Create `crates/api/src/routes/coverage.rs`:

```rust
use rocket::get;
use rocket::serde::json::Json;

use crate::dto::agent_coverage::AgentSkillCoverageDto;
use crate::error::ApiResult;
use crate::extractors::{ResolvedScope, ScopeParams};
use aghub_core::models::ResourceScope;
use aghub_core::skills::linker::classify::{classify_all, LinkNeed};
use aghub_core::skills::linker::universal_canonical_dir;

/// `GET /api/v1/skills/coverage?scope=<global|project>&project_root=<path?>`
///
/// Classifies every registered agent against the canonical `.agents/skills`
/// master SKILLS-DIR for the requested scope and returns a per-agent coverage
/// projection. Canonicalization stays server-side. `project_root` is
/// absolutized by `ScopeParams::resolve` (P0-C) before reaching the classifier.
#[get("/skills/coverage?<params..>")]
pub async fn skills_coverage(
	params: ScopeParams,
) -> ApiResult<Vec<AgentSkillCoverageDto>> {
	let resolved = params.resolve()?;
	let (scope, project_root, scope_str) = match &resolved {
		ResolvedScope::Global => (ResourceScope::GlobalOnly, None, "global"),
		ResolvedScope::Project { root } => {
			(ResourceScope::ProjectOnly, Some(root.as_path()), "project")
		}
		ResolvedScope::All { .. } => {
			return Err(crate::error::ApiError::new(
				rocket::http::Status::BadRequest,
				"scope 'all' is not supported for coverage; use 'global' or \
				 'project'",
				"INVALID_PARAM",
			));
		}
	};

	let master_skills_dir = universal_canonical_dir(project_root)
		.ok_or_else(|| {
			crate::error::ApiError::new(
				rocket::http::Status::InternalServerError,
				"could not resolve the universal master skills directory",
				"COVERAGE_ERROR",
			)
		})?;

	let plans = classify_all(scope, project_root, &master_skills_dir);
	let dtos = plans
		.into_iter()
		.map(|plan| {
			let auto_covered = matches!(plan.need, LinkNeed::NativeReader);
			let needs_link = matches!(plan.need, LinkNeed::NeedsLink { .. });
			let supported = !matches!(plan.need, LinkNeed::Unsupported);
			AgentSkillCoverageDto {
				id: plan.agent_id.to_string(),
				scope: scope_str.to_string(),
				// P2-G: REAL facts from the classifier, not a guess. An agent
				// can read the master without writing to it (read-only native),
				// or write to it (write dir IS the master) — both are now
				// reported truthfully. The FE still partitions on auto_covered /
				// needs_link, but these diagnostics are honest.
				reads_master: plan.reads_master,
				writes_master: plan.writes_master,
				needs_link,
				auto_covered,
				supported,
			}
		})
		.collect();
	Ok(Json(dtos))
}

#[cfg(test)]
mod tests {
	use super::*;

	fn block_on<F: std::future::Future>(fut: F) -> F::Output {
		rocket::tokio::runtime::Builder::new_current_thread()
			.enable_all()
			.build()
			.unwrap()
			.block_on(fut)
	}

	#[test]
	fn global_scope_buckets_codex_native_claude_needs_link() {
		let params = ScopeParams {
			scope: Some("global".to_string()),
			project_root: None,
		};
		let dtos = block_on(skills_coverage(params))
			.ok()
			.expect("handler ok")
			.into_inner();
		let codex = dtos
			.iter()
			.find(|d| d.id == "codex")
			.expect("codex present");
		assert!(codex.auto_covered, "codex @global reads .agents/skills");
		assert!(!codex.needs_link);
		assert!(codex.supported);
		let claude = dtos
			.iter()
			.find(|d| d.id == "claude")
			.expect("claude present");
		assert!(
			claude.needs_link,
			"claude @global has a private skills dir => NeedsLink"
		);
		assert!(!claude.auto_covered);
	}

	#[test]
	fn coverage_rejects_scope_all() {
		let params = ScopeParams {
			scope: Some("all".to_string()),
			project_root: None,
		};
		let err = block_on(skills_coverage(params))
			.err()
			.expect("scope=all rejected");
		assert_eq!(err.status, rocket::http::Status::BadRequest);
	}
}
```

> Verify against current code: `ResolvedScope` variant names (`Global`/`Project{root}`/`All{..}`), `ApiError::new` signature + the `.status` field, `ApiResult` alias, and that agent ids match `AgentType::as_str()` (`codex`, `claude`). Adapt the match arms / ids if they differ — do NOT invent ids.

Wire the module: in `crates/api/src/routes/mod.rs` add `pub mod coverage;`.

**Step 38.2 — Run.**

```bash
cargo test --package aghub-api global_scope_buckets_codex_native_claude_needs_link -- --exact
cargo test --package aghub-api coverage_rejects_scope_all -- --exact
```

Expect PASS once it compiles (Task 17's classifier classifies Codex@global NativeReader, Claude@global NeedsLink). If the bucketing assertion FAILS, the bug is in the classifier (Tasks 13–18), not here — STOP and report.

**Step 38.3 — Commit.**

```bash
git add crates/api/src/routes/coverage.rs crates/api/src/routes/mod.rs
git commit -m "feat(api): GET /skills/coverage classifies agents server-side

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 39: Mount `/skills/coverage` in `lib.rs`

**Step 39.1 — Write the failing test.** Append to `crates/api/src/routes/coverage.rs` `mod tests`:

```rust
	#[test]
	fn coverage_route_is_mounted() {
		let client = rocket::local::blocking::Client::tracked(crate::rocket())
			.expect("rocket builds");
		let resp =
			client.get("/api/v1/skills/coverage?scope=global").dispatch();
		assert_eq!(resp.status(), rocket::http::Status::Ok);
	}
```

> Verify the app builder name first: `grep -n "pub fn rocket\|pub fn build\|fn rocket(" crates/api/src/lib.rs`. If it is `build()`, use `crate::build()`. If construction needs managed state/args, fall back to asserting the route is listed: `assert!(crate::rocket().routes().any(|r| r.uri.path() == "/api/v1/skills/coverage"));` (adjust the mount base if it differs).

**Step 39.2 — Run, expect FAIL.**

```bash
cargo test --package aghub-api coverage_route_is_mounted -- --exact
```

Expect FAIL: `404 Not Found` (route defined but not mounted).

**Step 39.3 — Mount the route.** In `crates/api/src/lib.rs`, inside the `routes![ … ]` macro (next to the other `routes::skills::*` entries), add:

```rust
				routes::coverage::skills_coverage,
```

**Step 39.4 — Run, expect PASS.**

```bash
cargo test --package aghub-api coverage_route_is_mounted -- --exact
```

Expect PASS.

**Step 39.5 — Commit.**

```bash
git add crates/api/src/lib.rs crates/api/src/routes/coverage.rs
git commit -m "feat(api): mount /skills/coverage route

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 40: Remove `GitInstallRequest.universal` (legacy-field guard)

**Step 40.1 — Add the regression guard test.** Append to `crates/api/src/dto/skill.rs` (add a `#[cfg(test)] mod tests` if none exists — check with `grep -n "mod tests" crates/api/src/dto/skill.rs`):

```rust
#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn git_install_request_ignores_legacy_universal_field() {
		// No deny_unknown_fields => a legacy client sending "universal" still
		// deserializes (field dropped), and the struct no longer has it.
		let json = r#"{"session_id":"s","skill_paths":[],"agents":[],"scope":"global","project_root":null,"universal":true}"#;
		let req: GitInstallRequest =
			serde_json::from_str(json).expect("parses, ignoring universal");
		assert_eq!(req.session_id, "s");
	}
}
```

**Step 40.2 — Run, expect PASS today.**

```bash
cargo test --package aghub-api git_install_request_ignores_legacy_universal_field -- --exact
```

Expect PASS (field still present, deserializes). This is the regression guard.

**Step 40.3 — Remove the field.** In `crates/api/src/dto/skill.rs`, delete the `pub universal: Option<bool>` field + its doc + `#[serde(default)]`/`#[ts(optional)]` attrs from `GitInstallRequest`, so the struct ends at `pub project_root: Option<String>,` then `}`.

**Step 40.4 — Run.**

```bash
cargo test --package aghub-api git_install_request_ignores_legacy_universal_field -- --exact
```

Expect FAIL TO COMPILE if any code reads `req.universal` — that is the `git_install_skills` route. **Do NOT commit yet** — Task 41 makes it compile. Proceed directly to Task 41.

---

### Task 41: `git_install_skills` — drop universal/copy branch, route through the primitive

**Step 41.1 — Write the failing route test.** Append to `crates/api/src/routes/skills.rs` `mod tests` (reuse `with_isolated_env`/`block_on`):

```rust
	#[cfg(unix)]
	#[test]
	fn git_install_writes_npx_lock_symlink_only() {
		use crate::routes::skills::GitCloneSessions;
		with_isolated_env(|home, _state| {
			let repo = home.join("srcrepo");
			let skill_dir = repo.join("my-skill");
			std::fs::create_dir_all(&skill_dir).unwrap();
			std::fs::write(
				skill_dir.join("SKILL.md"),
				"---\nname: my-skill\ndescription: d\n---\n",
			)
			.unwrap();

			let sessions = GitCloneSessions::default();
			let session_id = "sess-1".to_string();
			sessions.insert_for_test(
				session_id.clone(),
				&repo,
				"https://github.com/o/r",
				"main",
			);

			let req = GitInstallRequest {
				session_id: session_id.clone(),
				skill_paths: vec!["my-skill".to_string()],
				agents: vec!["claude".to_string()],
				scope: "global".to_string(),
				project_root: None,
			};
			let resp = block_on(git_install_skills(
				Json(req),
				rocket::State::from(&sessions),
			))
			.ok()
			.expect("handler ok")
			.into_inner();

			assert!(
				resp.results.iter().any(|r| r.agent == "claude"),
				"per-agent row present"
			);
			let master = home.join(".agents/skills/my-skill/SKILL.md");
			assert!(master.exists(), "universal master written: {master:?}");
			let lock = home.join(".config/agents/skills.lock.json");
			let lock_alt = home.join(".agents/skills-lock.json");
			assert!(
				lock.exists() || lock_alt.exists(),
				"a global skill install lock was written"
			);
		});
	}
```

> Verify, then adjust the TEST: (1) the session-insert seam — `grep -n "struct GitCloneSessions\|impl GitCloneSessions\|fn insert\|struct .*Session" crates/api/src/routes/skills.rs`; if no `insert_for_test`, construct the session inline as production does (move a `tempfile::TempDir` into it). (2) the real global lock path — `grep -n "write_global_install_lock\|global_install_lock_path\|skills.lock" crates/skill/src`; assert that exact file. (3) `rocket::State::from(&sessions)` may not exist; if the handler needs `&State<GitCloneSessions>`, drive through `Client::tracked` POSTing JSON to `/api/v1/skills/git/install` after seeding the session.

**Step 41.2 — Run, expect FAIL TO COMPILE.**

```bash
cargo test --package aghub-api git_install_writes_npx_lock_symlink_only -- --exact
```

Expect FAIL (Task 40 removed `universal`; the route still references `req.universal` and builds a `layout`).

**Step 41.3 — Rewrite the route body.** In `crates/api/src/routes/skills.rs::git_install_skills`:

- Absolutize the project root — replace `let project_root = req.project_root.as_ref().map(std::path::PathBuf::from);` with:
    ```rust
    	let project_root: Option<std::path::PathBuf> = req
    		.project_root
    		.as_ref()
    		.map(|r| crate::extractors::absolutize_root(r));
    ```
- Delete the `let layout = if req.universal.unwrap_or(false) { ... } else { ... };` DECISION branch entirely (this is the copy-vs-universal choice that must go).
- In the `FetchedSkillInstallRequest { … }` literal, set the still-present shim fields to the always-universal values (option (a) — Task 47a converts these to `target: LinkTarget` across every consumer at once, so the route keeps compiling now):
    ```rust
    			layout: aghub_core::skills::install_fetched::SkillInstallLayout::Universal,
    			use_relative_links: matches!(
    				resource_scope,
    				ResourceScope::ProjectOnly
    			),
    ```
    (Keep `build_git_install_groups` for now — Task 46 removes it; the `report.agent_results` → `GitInstallResultEntry` mapping is unchanged. The load-bearing change is deleting the `req.universal` DECISION; the request is now always-universal.)

**Step 41.4 — Run, expect PASS.**

```bash
cargo test --package aghub-api git_install_writes_npx_lock_symlink_only -- --exact
cargo test --package aghub-api git_install_request_ignores_legacy_universal_field -- --exact
```

Expect both PASS.

**Step 41.5 — Commit (Task 40 removal + Task 41 rewrite together).**

```bash
git add crates/api/src/dto/skill.rs crates/api/src/routes/skills.rs
git commit -m "feat(api): git_install_skills is symlink-only; drop GitInstallRequest.universal

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 42: Enrich `InstallSkillResponse` + rewrite `install_skill`

**Step 42.1 — Write the failing route test.** Append to `crates/api/src/routes/skills.rs` `mod tests`:

```rust
	#[cfg(unix)]
	#[test]
	fn install_skill_returns_per_agent_rows_symlink_only() {
		with_isolated_env(|home, _state| {
			let work = home.join("work");
			let skill_dir = work.join("my-skill");
			std::fs::create_dir_all(&skill_dir).unwrap();
			std::fs::write(
				skill_dir.join("SKILL.md"),
				"---\nname: my-skill\ndescription: d\n---\n",
			)
			.unwrap();
			let run = |args: &[&str]| {
				std::process::Command::new("git")
					.args(args)
					.current_dir(&work)
					.env("GIT_AUTHOR_NAME", "t")
					.env("GIT_AUTHOR_EMAIL", "t@t")
					.env("GIT_COMMITTER_NAME", "t")
					.env("GIT_COMMITTER_EMAIL", "t@t")
					.output()
					.unwrap();
			};
			run(&["init", "-q"]);
			run(&["add", "."]);
			run(&["commit", "-qm", "init"]);

			let req = InstallSkillRequest {
				source: format!("file://{}", work.display()),
				agents: vec!["claude".to_string()],
				skills: vec!["my-skill".to_string()],
				scope: "global".to_string(),
				project_path: None,
				install_all: Some(false),
			};
			let resp = block_on(install_skill(Json(req)))
				.ok()
				.expect("handler ok")
				.into_inner();
			assert!(resp.success, "install succeeded");
			assert!(
				resp.agents.iter().any(|a| a.agent == "claude"),
				"per-agent rows present"
			);
			assert!(
				home.join(".agents/skills/my-skill/SKILL.md").exists(),
				"master materialized (symlink-only)"
			);
		});
	}
```

> `clone_to_temp` requires `git`. Verify the `file://` scheme is accepted (`grep -n "file://\|scheme\|resolve_remote_source" crates/git/src`); if rejected, use the local-path form its tests accept. Match `InstallSkillRequest`'s exact field set.

**Step 42.2 — Run, expect FAIL TO COMPILE** (`InstallSkillResponse` has no `agents` field).

```bash
cargo test --package aghub-api install_skill_returns_per_agent_rows_symlink_only -- --exact
```

**Step 42.3 — Add `agents` to `InstallSkillResponse`.** In `crates/api/src/dto/skill.rs`, change `InstallSkillResponse` from `{ pub success: bool }` to:

```rust
#[derive(Debug, Serialize, TS)]
#[ts(export)]
pub struct InstallSkillResponse {
	pub success: bool,
	/// Per-agent install outcome rows (Decision 10: link failures are per-agent
	/// soft-fails, so an aggregate boolean cannot say WHICH agent failed).
	/// Reuses the git-install row shape for parity with `/skills/git/install`.
	pub agents: Vec<GitInstallResultEntry>,
}
```

**Step 42.4 — Rewrite the `install_skill` body.** In `crates/api/src/routes/skills.rs::install_skill`:

- Absolutize the root — replace `let project_root = req.project_path.as_ref().map(std::path::PathBuf::from);` with:
    ```rust
    	let project_root = req
    		.project_path
    		.as_ref()
    		.map(|r| crate::extractors::absolutize_root(r));
    ```
- Replace the whole copy-loop + manual-lock block (`let (dir_groups, invalid_agents) = build_git_install_groups(...)` through `Ok(Json(InstallSkillResponse { success }))`) with:

    ```rust
    	// Resolve requested agents; unknown agents become per-agent failure rows.
    	let mut invalid_rows: Vec<GitInstallResultEntry> = Vec::new();
    	let mut target_agents: Vec<(String, AgentType)> = Vec::new();
    	for agent_str in &req.agents {
    		match agent_str.parse::<AgentType>() {
    			Ok(a) => target_agents.push((agent_str.clone(), a)),
    			Err(_) => invalid_rows.push(GitInstallResultEntry {
    				name: String::new(),
    				agent: agent_str.clone(),
    				success: false,
    				error: Some(format!("Unknown agent '{agent_str}'")),
    			}),
    		}
    	}
    	let agent_types: Vec<AgentType> =
    		target_agents.iter().map(|(_, a)| *a).collect();

    	let ref_commit = gix::open(temp_dir.path())
    		.ok()
    		.and_then(|repo| repo.head_id().ok().map(|id| id.detach()))
    		.map(|oid| oid.to_string());

    	// Shim fields (ignored by the always-universal dispatch; Task 47a converts
    	// every consumer to `target: LinkTarget` at once). use_relative_links
    	// preserves the project=relative / global=absolute link form.
    	let use_relative_links =
    		matches!(resource_scope, ResourceScope::ProjectOnly);

    	let mut agent_rows: Vec<GitInstallResultEntry> = invalid_rows;
    	let mut any_installed = false;
    	for skill in &selected_skills {
    		let request =
    			aghub_core::skills::install_fetched::FetchedSkillInstallRequest {
    				skill_file: &skill.full_path,
    				source: &lock_source,
    				lock_skill_path: skill::lock_skill_file_path(
    					&skill.relative_dir,
    				),
    				ref_commit: ref_commit.clone(),
    				scope: resource_scope,
    				project_root: project_root.as_deref(),
    				target_agents: &agent_types,
    				expected_name: None,
    				layout:
    					aghub_core::skills::install_fetched::SkillInstallLayout::Universal,
    				use_relative_links,
    			};
    		match aghub_core::skills::install_fetched::install_fetched_skill_and_lock(
    			request,
    		) {
    			Ok(report) => {
    				for ((agent_str, _), agent_result) in
    					target_agents.iter().zip(report.agent_results)
    				{
    					let success = agent_result.error.is_none();
    					any_installed |= agent_result.installed;
    					agent_rows.push(GitInstallResultEntry {
    						name: if success {
    							report.name.clone()
    						} else {
    							skill.name.clone()
    						},
    						agent: agent_str.clone(),
    						success,
    						error: agent_result.error,
    					});
    				}
    			}
    			Err(e) => {
    				let message = ApiError::from(e).body.error;
    				for (agent_str, _) in &target_agents {
    					agent_rows.push(GitInstallResultEntry {
    						name: skill.name.clone(),
    						agent: agent_str.clone(),
    						success: false,
    						error: Some(message.clone()),
    					});
    				}
    			}
    		}
    	}

    	let success = any_installed && agent_rows.iter().all(|r| r.success);
    	Ok(Json(InstallSkillResponse {
    		success,
    		agents: agent_rows,
    	}))
    ```

> The request reuses the still-present `layout`/`use_relative_links` shim fields (Task 47a converts them to `target: LinkTarget` across all consumers); `use_relative_links` is a plain `bool` so re-binding it per loop iteration is free. Match the real names of `selected_skills`, `lock_source`, `skill.full_path`, `skill.relative_dir`, `skill.name`, `temp_dir`, `resource_scope`, and `ApiError::from(e).body.error` against the current source; adjust if they differ.

**Step 42.5 — Run, expect PASS.**

```bash
cargo test --package aghub-api install_skill_returns_per_agent_rows_symlink_only -- --exact
```

Expect PASS.

**Step 42.6 — Commit.**

```bash
git add crates/api/src/dto/skill.rs crates/api/src/routes/skills.rs
git commit -m "feat(api): install_skill is symlink-only with per-agent rows

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 43: `delete_skill_by_path` — route is-link probe via `Linker::is_link`

**Step 43.1 — Write the test (unix symlink characterization).** Append to `crates/api/src/routes/skills.rs` `mod tests`:

```rust
	#[cfg(unix)]
	#[test]
	fn delete_by_path_symlinked_install_uses_canonical_layout() {
		with_isolated_env(|home, _state| {
			let master = home.join(".agents/skills/linked");
			std::fs::create_dir_all(&master).unwrap();
			std::fs::write(
				master.join("SKILL.md"),
				"---\nname: linked\ndescription: d\n---\n",
			)
			.unwrap();
			let skills = home.join(".claude/skills");
			std::fs::create_dir_all(&skills).unwrap();
			let link = skills.join("linked");
			std::os::unix::fs::symlink(&master, &link).unwrap();

			let resp = block_on(delete_skill_by_path(Json(
				by_path_req(&link, Some(true)),
			)))
			.ok()
			.expect("handler ok")
			.into_inner();
			assert!(resp.success);
			assert!(!link.exists(), "referrer link removed");
			assert!(
				master.join("SKILL.md").exists(),
				"shared master must NOT be deleted"
			);
		});
	}
```

(Match the real `delete_skill_by_path` request type + the existing test helper that builds it — `grep -n "by_path_req\|DeleteSkillByPathRequest\|fn delete_skill_by_path" crates/api/src/routes/skills.rs`; reuse the established helper.)

**Step 43.2 — Run.**

```bash
cargo test --package aghub-api delete_by_path_symlinked_install_uses_canonical_layout -- --exact
```

Likely PASS already on unix (the `is_symlink()` probe catches a symlink) — this locks the contract before the swap. If it FAILS, that is a pre-existing bug — report and continue.

**Step 43.3 — Swap the probe.** In `crates/api/src/routes/skills.rs::delete_skill_by_path`, replace:

```rust
	let path_is_symlink = std::fs::symlink_metadata(&skill_dir)
		.map(|meta| meta.file_type().is_symlink())
		.unwrap_or(false);
```

with:

```rust
	let path_is_symlink = aghub_core::skills::linker::Linker::is_link(&skill_dir);
```

**Step 43.4 — Run, expect PASS (existing `delete_by_path_*` tests still pass).**

```bash
cargo test --package aghub-api delete_by_path -- --nocapture
```

Expect all green.

**Step 43.5 — Commit.**

```bash
git add crates/api/src/routes/skills.rs
git commit -m "fix(api): delete_skill_by_path detects junctions via Linker::is_link (P1-E)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 43a: Route `entry_allowed`'s symlink probe through `Linker::is_link` (P1-E2 — skills.rs:942)

The `entry_allowed` helper (`crates/api/src/routes/skills.rs:942`, used by the skill content/tree scan at `:900`) gates which entries under the allow-listed roots are returned. It uses a RAW `std::fs::symlink_metadata(path).map(|m| m.file_type().is_symlink())` at `:943-945` to decide whether to apply the `assert_contained` escape guard. A Windows junction reports `is_symlink() == false`, so a junction entry skips the containment check entirely and is treated as a plain (always-allowed) entry — the same junction-blind-spot bug the install/removal/discovery swaps fix. The no-copy grep (Task 66) flags ANY surviving `file_type().is_symlink()` in `skills.rs` as a P0 miss, so this MUST be routed through `Linker::is_link` (which recognizes junctions), keeping the containment guard correct on Windows.

**Step 43a.1 — Write the test (unix symlink characterization).** Append to `crates/api/src/routes/skills.rs` `mod tests`:

```rust
	// P1-E2: entry_allowed routes its link probe through Linker::is_link, so a
	// link (unix symlink / windows junction) is subjected to the containment
	// guard. A link that ESCAPES the allow-listed roots is excluded; a link
	// that stays inside is allowed; a plain real entry is always allowed.
	#[cfg(unix)]
	#[test]
	fn entry_allowed_excludes_escaping_link_keeps_contained() {
		let tmp = tempfile::tempdir().unwrap();
		let root = tmp.path().join("root");
		std::fs::create_dir_all(&root).unwrap();
		std::fs::write(root.join("real.txt"), "x").unwrap();
		// An escaping symlink: target outside the allow-listed root.
		let outside = tmp.path().join("outside");
		std::fs::create_dir_all(&outside).unwrap();
		let escaping = root.join("escape");
		std::os::unix::fs::symlink(&outside, &escaping).unwrap();
		let roots = vec![root.clone()];

		assert!(
			entry_allowed(&root.join("real.txt"), &roots),
			"a plain real entry is always allowed"
		);
		assert!(
			!entry_allowed(&escaping, &roots),
			"an escaping link must be excluded"
		);
	}
```

(Match `entry_allowed`'s real signature — `grep -n "fn entry_allowed" crates/api/src/routes/skills.rs`. If `entry_allowed` is private, the test lives in the same module's `mod tests`, which can see it.)

**Step 43a.2 — Run.**

```bash
cargo test --package aghub-api entry_allowed_excludes_escaping_link_keeps_contained -- --exact
```

Likely PASS already on unix (the raw `is_symlink()` catches a unix symlink) — this locks the contract before the swap.

**Step 43a.3 — Swap the probe.** In `entry_allowed` (`skills.rs:942-950`), replace:

```rust
	let is_symlink = std::fs::symlink_metadata(path)
		.map(|meta| meta.file_type().is_symlink())
		.unwrap_or(false);
	if !is_symlink {
		return true;
	}
```

with:

```rust
	// Recognize a windows junction too (is_symlink() == false for junctions);
	// without this a junction entry would skip the containment guard (P1-E2).
	if !aghub_core::skills::linker::Linker::is_link(path) {
		return true;
	}
```

**Step 43a.4 — Run, expect PASS.**

```bash
cargo test --package aghub-api entry_allowed -- --nocapture
```

Expect green (the unix behavior is identical; the swap only adds junction recognition).

**Step 43a.5 — Commit.**

```bash
git add crates/api/src/routes/skills.rs
git commit -m "fix(api): entry_allowed detects junctions via Linker::is_link (P1-E2, skills.rs:942)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 44: T-DELETE-BY-PATH-JUNCTION (windows)

Pin the Task 43 swap with a junction sibling.

- [ ] In `crates/api/src/routes/skills.rs` `mod tests`, add a `#[cfg(windows)]` sibling of `delete_by_path_symlinked_install_uses_canonical_layout` that builds the referrer via `aghub_core::skills::linker::create_junction(&master.canonicalize().unwrap(), &link)` instead of `std::os::unix::fs::symlink`, then asserts the same outcome (referrer gone, Master `SKILL.md` survives):

    ```rust
    	// T-DELETE-BY-PATH-JUNCTION: a junction install takes the canonical-layout
    	// branch (recognized via Linker::is_link) and is not orphaned; the shared
    	// Master survives. windows-latest.
    	#[cfg(windows)]
    	#[test]
    	fn delete_by_path_junction_install_uses_canonical_layout() {
    		use aghub_core::skills::linker::create_junction;
    		with_isolated_env(|home, _state| {
    			let master = home.join(".agents/skills/linked");
    			std::fs::create_dir_all(&master).unwrap();
    			std::fs::write(
    				master.join("SKILL.md"),
    				"---\nname: linked\ndescription: d\n---\n",
    			)
    			.unwrap();
    			let skills = home.join(".claude/skills");
    			std::fs::create_dir_all(&skills).unwrap();
    			let link = skills.join("linked");
    			create_junction(&master.canonicalize().unwrap(), &link).unwrap();

    			let resp = block_on(delete_skill_by_path(Json(
    				by_path_req(&link, Some(true)),
    			)))
    			.ok()
    			.expect("handler ok")
    			.into_inner();
    			assert!(resp.success);
    			assert!(
    				master.join("SKILL.md").exists(),
    				"shared master must survive junction delete"
    			);
    		});
    	}
    ```

    (`with_isolated_env` redirects HOME via env, which Windows `dirs::home_dir` ignores — if the existing tests document this limitation, gate this test to run only where HOME redirection works on the windows-latest runner, or assert against an explicitly-passed root. Match the established Windows test pattern in the file.)

- [ ] Run: `cargo test --package aghub-api delete_by_path -- --nocapture` → expect PASS on unix (windows test compiled-out).
- [ ] Commit:
    ```bash
    git add crates/api/src/routes/skills.rs
    git commit -m "test(api): T-DELETE-BY-PATH-JUNCTION (windows junction delete)
    ```

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"

````

---

### Task 45: T-REL-ROOT-ABSOLUTIZED — end-to-end relative-root install

Pin that a project-scope install with a RELATIVE `project_root` succeeds (no `NonAbsoluteTarget`) and produces an absolute master.

- [ ] Append to `crates/api/src/routes/skills.rs` `mod tests` (uses `with_isolated_env`'s env-lock serialization, so `set_current_dir` is safe):
```rust
	#[cfg(unix)]
	#[test]
	fn install_skill_relative_project_root_is_absolutized() {
		with_isolated_env(|home, _state| {
			let proj = home.join("proj");
			std::fs::create_dir_all(proj.join(".claude")).unwrap();
			let work = home.join("work");
			let skill_dir = work.join("my-skill");
			std::fs::create_dir_all(&skill_dir).unwrap();
			std::fs::write(
				skill_dir.join("SKILL.md"),
				"---\nname: my-skill\ndescription: d\n---\n",
			)
			.unwrap();
			let run = |args: &[&str]| {
				std::process::Command::new("git")
					.args(args)
					.current_dir(&work)
					.env("GIT_AUTHOR_NAME", "t")
					.env("GIT_AUTHOR_EMAIL", "t@t")
					.env("GIT_COMMITTER_NAME", "t")
					.env("GIT_COMMITTER_EMAIL", "t@t")
					.output()
					.unwrap();
			};
			run(&["init", "-q"]);
			run(&["add", "."]);
			run(&["commit", "-qm", "init"]);

			let prev = std::env::current_dir().unwrap();
			std::env::set_current_dir(home).unwrap();
			let req = InstallSkillRequest {
				source: format!("file://{}", work.display()),
				agents: vec!["claude".to_string()],
				skills: vec!["my-skill".to_string()],
				scope: "project".to_string(),
				project_path: Some("proj".to_string()), // RELATIVE
				install_all: Some(false),
			};
			let resp = block_on(install_skill(Json(req)))
				.ok()
				.expect("handler ok")
				.into_inner();
			std::env::set_current_dir(prev).unwrap();

			assert!(
				resp.agents.iter().all(|a| a
					.error
					.as_deref()
					.map(|e| !e.contains("absolute"))
					.unwrap_or(true)),
				"no NonAbsoluteTarget error rows"
			);
			assert!(
				proj.join(".agents/skills/my-skill/SKILL.md").exists(),
				"master written at absolutized project root"
			);
		});
	}
````

- [ ] Run: `cargo test --package aghub-api install_skill_relative_project_root_is_absolutized -- --exact` → expect PASS (Task 42 wires `absolutize_root` into `install_skill`).
- [ ] Commit:
    ```bash
    git add crates/api/src/routes/skills.rs
    git commit -m "test(api): T-REL-ROOT-ABSOLUTIZED for project-scope install
    ```

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"

````

---

### Task 46: Delete dead copy helpers + their in-file tests

After Tasks 41–43, the API-local copy helpers are nearly dead. `git_install_skills` still calls `build_git_install_groups` — replace that too, then delete the helpers.

- [ ] In `git_install_skills`, replace the `let (dir_groups, invalid_agents) = build_git_install_groups(...)` block + the `valid_agents`/`target_agents` derivation with inline agent resolution:
```rust
	let mut valid_agents: Vec<(String, AgentType)> = Vec::new();
	for agent_str in &req.agents {
		match agent_str.parse::<AgentType>() {
			Ok(a) => valid_agents.push((agent_str.clone(), a)),
			Err(_) => {
				for skill_path in &req.skill_paths {
					results.push(GitInstallResultEntry {
						name: skill_path.clone(),
						agent: agent_str.clone(),
						success: false,
						error: Some(format!("Unknown agent '{agent_str}'")),
					});
				}
			}
		}
	}
	let target_agents: Vec<AgentType> =
		valid_agents.iter().map(|(_, agent)| *agent).collect();
````

(The per-scope "agent does not support skill creation in this scope" pre-check that `build_git_install_groups` did is now handled by the classifier inside the primitive — an unsupported agent returns `Unsupported` → an error row. Re-run Task 41's test after this edit.)

- [ ] Delete the dead fns from `crates/api/src/routes/skills.rs`: `copy_dir_recursive`, `resolve_git_install_target_dir`, `install_git_skill_to_dir`, the `GitInstallAgentGroup`/`GitInstallGroups`/`GitInstallInvalidAgent` type aliases, `build_git_install_groups`, `should_write_install_lock`, and `skill_lock_contains` (verify `skill_lock_contains` has no other live caller first: `rg -n 'skill_lock_contains' crates/api/src/routes/skills.rs`; KEEP it if `write_skill_install_lock` or another live fn uses it).
- [ ] Delete the corresponding in-file tests (the `build_git_install_groups` test and the `should_write_install_lock` test).
- [ ] Verify the survivor grep: `rg 'copy_dir_recursive|IsolatedCopy|copied_fallback|CopiedFallback|install_git_skill_to_dir|build_git_install_groups|should_write_install_lock' crates/api crates/core` → expect ONLY `crates/core/src/skills/linker/mod.rs::copy_dir_recursive`, `crates/core/src/transfer.rs::copy_dir_recursive`, and `crates/core/src/skills/install_fetched.rs::should_write_install_lock`+`skill_lock_contains`. ZERO hits in `crates/api`.
- [ ] **GAP-4: import_skill regression** — the import route (`import_skill`, `skills.rs:1022-1060`, verified live) calls `manager.add_skill_from_path(...)` (`:1039`), which Task 25 converted to symlink-only, then writes the lock from the SOURCE folder (`get_skill_root(expand_tilde_path(&request.path))`, `:1042` — KEEP unchanged per spec line 447). Add an `#[cfg(unix)]` route test (mirror the env-isolation + temp-project harness of the sibling `import_skill_*` / `git_install_*` tests in this file's `mod tests`) proving import now produces a master+link, not a copy, and still writes the lock:
    ```rust
    	// GAP-4: import_skill inherits the symlink-only model via add_skill_from_path
    	// — it must materialize a .agents Master + a link (never an isolated copy)
    	// and still write the install lock from the SOURCE folder (spec line 447).
    	#[cfg(unix)]
    	#[test]
    	fn import_skill_links_master_and_writes_lock() {
    		let _guard = crate::routes::test_env_lock()
    			.lock()
    			.unwrap_or_else(|e| e.into_inner());
    		// Build an isolated project with a .claude marker + a source skill,
    		// invoke the import_skill handler (project scope), then assert:
    		//   1. <project>/.agents/skills/<name>/SKILL.md exists (Master)
    		//   2. <project>/.claude/skills/<name> is a symlink (Linker::is_link)
    		//   3. the project lock contains the skill (source-folder hash recorded)
    		// Fill the handler-invocation + lock-read in from the closest sibling
    		// test (e.g. git_install_existing_folder_without_lock_writes_lock) — the
    		// import path uses the same write_skill_install_lock helper.
    		// assert!(<project>.join(".agents/skills/<name>/SKILL.md").exists());
    		// assert!(aghub_core::skills::linker::Linker::is_link(
    		//     &<project>.join(".claude/skills/<name>")));
    		// let lock = skill::lock::local::read_local_lock(Some(&<project>));
    		// assert!(lock.skills.contains_key("<name>"));
    	}
    ```
    Adapt the handler-invocation + lock-read to the in-file test harness (the import route is sync; call it like the sibling route tests do, or drive it via the manager+`write_skill_install_lock` exactly as `import_skill` does if the handler is awkward to call directly). The load-bearing assertions are the three numbered lines.
- [ ] Build + lint: `cargo test --package aghub-api` and `cargo clippy --package aghub-api -- -D warnings` → expect compiles, all tests green, clippy clean.
- [ ] Commit:
    ```bash
    git add crates/api/src/routes/skills.rs
    git commit -m "refactor(api): delete dead copy-install helpers and their tests
    ```

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"

````

---

### Task 47: Regenerate ts-rs DTOs + prettier

Register the new DTO in the exporter and regen the generated surface.

- [ ] In `crates/api/src/bin/export-dto.rs`, find the export list (`grep -n "export\|AgentInfo\|GitInstallRequest" crates/api/src/bin/export-dto.rs`) and add an export call for `AgentSkillCoverageDto` next to the other skill DTOs, matching the EXACT pattern the file already uses (e.g. `export_one::<aghub_api::dto::agent_coverage::AgentSkillCoverageDto>(&out)?;` or the file's macro form — copy the established call shape; do not invent a helper).
- [ ] Regenerate then prettier (generated-DTO workflow — `generate:dto` alone shows a spurious ~121-file diff; prettier-then-diff isolates the real change):
```bash
cd crates/desktop && bun run generate:dto && bunx prettier --write src/generated/
````

- [ ] Inspect the real diff: `git -C /home/audichuang/research/aghub diff --stat crates/desktop/src/generated/` → expect exactly three meaningful changes: `GitInstallRequest.ts` (`universal?: boolean` removed), `InstallSkillResponse.ts` (`agents: Array<GitInstallResultEntry>` added + import), `AgentSkillCoverageDto.ts` (new file).
- [ ] Verify `AgentSkillCoverageDto.ts` has exactly the six keys `id, scope, reads_master, writes_master, needs_link, auto_covered, supported` (`id`/`scope: string`, the rest `boolean`, all required). If `dto/index.ts` is a barrel and the exporter did not auto-add the re-export, add `export * from "./AgentSkillCoverageDto";`.
- [ ] Commit:
    ```bash
    git add crates/api/src/bin/export-dto.rs crates/desktop/src/generated/
    git commit -m "chore(dto): regen ts-rs DTOs — coverage DTO, install response agents, drop universal
    ```

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"

````

Final API crate gate (no commit unless fmt re-ran):

```bash
cargo test --package aghub-api
cargo clippy --package aghub-api -- -D warnings
cargo fmt --package aghub-api -- --check
````

Expect all green. If fmt fails, run `cargo fmt --package aghub-api` and commit `style(api): rustfmt`.

---

### Task 47a: Delete the shared copy shim — `install_layout.rs`, `SkillInstallLayout`, `layout`/`use_relative_links` (workspace-gated)

This is the SINGLE task that removes the deferred shared copy types. By now EVERY consumer references the always-universal install: `install_fetched.rs` (dispatch, Task 22), `crates/core/tests/sources_install_tests.rs` (Task 23a), `manager/skill.rs` (Task 25), `crates/cli/src/commands/add.rs` (Task 35a) + `source.rs` (Task 35c), and the API routes `git_install_skills`/`install_skill` (Tasks 41/42) — all of them currently pass the IGNORED `layout: SkillInstallLayout::Universal` + `use_relative_links` shim fields. This task swaps `FetchedSkillInstallRequest`'s two shim fields for a single `target: LinkTarget`, deletes the `SkillInstallLayout` enum and the `install_layout.rs` re-export shim, and updates every construction site in lockstep — so the workspace builds at the single commit that lands them all. **This is why no earlier commit broke: the shim let every consumer compile until this point.**

- [ ] Enumerate every construction site FIRST (do not miss one): `rg -n 'layout:\s*SkillInstallLayout|SkillInstallLayout::|use_relative_links' crates/core crates/cli crates/api`. Expected sites: `install_fetched.rs` (the request struct def + the Task 24 `nocopy_tests` literal + any T-LOCK test from Task 64), `sources_install_tests.rs` (8 literals), `source.rs::apply_install`, `git_install_skills`, `install_skill`. Confirm the list before editing.
- [ ] In `crates/core/src/skills/install_fetched.rs`:
    - Delete the `SkillInstallLayout` enum + its deprecation doc.
    - On `FetchedSkillInstallRequest`, remove `pub layout: SkillInstallLayout,` and replace `pub use_relative_links: bool,` (+ doc) with:
        ```rust
        	/// Link style: relative links (project scope, portable) vs absolute
        	/// (global scope). Junctions always resolve absolute regardless.
        	pub target: LinkTarget,
        ```
    - In `install_fetched_skill_and_lock`, delete the Task 22 `let target = if req.use_relative_links {…} else {…};` derivation and pass `req.target` directly to `install_universal_layout`.
- [ ] Update every construction site to use `target: LinkTarget::{Relative,Absolute}` instead of the two shim fields:
    - `install_fetched.rs` test literals (Task 24 `nocopy_tests`, Task 64 lock test): `layout: …, use_relative_links: true` → `target: LinkTarget::Relative`.
    - `crates/core/tests/sources_install_tests.rs` (all 8): each `layout: SkillInstallLayout::X, use_relative_links: B` → `target: if B { LinkTarget::Relative } else { LinkTarget::Absolute }` (import `aghub_core::skills::linker::LinkTarget` at the top; drop the `SkillInstallLayout` import). Assertions are unchanged (Task 23a already made them symlink-only).
    - `crates/cli/src/commands/source.rs::apply_install`: replace the two shim-field lines with `target: if matches!(scope, ResourceScope::ProjectOnly) { LinkTarget::Relative } else { LinkTarget::Absolute },` and drop `SkillInstallLayout` from the `use …install_fetched::{…}` import (add `use aghub_core::skills::linker::LinkTarget;`).
    - `crates/api/src/routes/skills.rs` `git_install_skills` + `install_skill`: replace the `layout: …::SkillInstallLayout::Universal, use_relative_links: …` lines with `target: if matches!(resource_scope, ResourceScope::ProjectOnly) { aghub_core::skills::linker::LinkTarget::Relative } else { aghub_core::skills::linker::LinkTarget::Absolute },`.
- [ ] In `crates/core/src/skills/mod.rs`, delete the `pub mod install_layout;` line. `git rm crates/core/src/skills/install_layout.rs`.
- [ ] WORKSPACE gate (the whole point — a missed consumer fails HERE, not as a silent broken commit): `cargo build --workspace 2>&1 | tail -15` → expect `Finished`. Any `error[E0560]: struct FetchedSkillInstallRequest has no field named layout` or `cannot find type SkillInstallLayout` names the construction site you missed — fix it, do not re-add the field.
- [ ] `just test 2>&1 | tail -25` (== `cargo test --workspace`) → expect all green.
- [ ] `cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -15` → expect clean.
- [ ] Commit:
    ```bash
    git add -A && git commit -m "refactor: remove SkillInstallLayout/install_layout shim; FetchedSkillInstallRequest takes target: LinkTarget
    ```

The symlink-only migration deferred deleting the shared copy types until
every consumer (core, core tests, CLI add + source sync, API git/install
routes) was migrated. This is that single workspace-green cleanup commit.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"

````

---

## Tasks 48–63 — Desktop frontend: symlink-only install convergence + coverage bucketing

> Branch `feat/symlink-only-install` is already checked out. Commands run from `crates/desktop` unless an absolute path is shown. FE logic tests use Node's built-in runner: `node --test --experimental-strip-types <file>` (no vitest/jest). TS uses hard tabs; `bun` only.
>
> **Cross-chunk dependency:** Task 47 already regenerated `crates/desktop/src/generated/dto/AgentSkillCoverageDto.ts` (fields `id, scope, reads_master, writes_master, needs_link, auto_covered, supported`), removed `universal` from `GitInstallRequest`, and added `InstallSkillResponse.agents`. Tasks 48–60 reference the generated `AgentSkillCoverageDto` type; if it is not yet present, run Task 47's regen first. Task 58 below re-runs the regen to pick up any drift.

### Task 48: Pure bucketing partition — failing test

Create `crates/desktop/src/lib/agent-capabilities.test.ts`:

```ts
import assert from "node:assert/strict";
// No FE test runner (no vitest/jest) is installed here; this pure-logic test
// uses Node's built-in runner, matching the other desktop helper tests.
// eslint-disable-next-line test/no-import-node-test
import { test } from "node:test";
import type { AgentSkillCoverageDto } from "../generated/dto";
import {
	isAutoCoveredByMaster,
	needsMasterLink,
	partitionByCoverage,
} from "./agent-capabilities.ts";

function cov(
	id: string,
	over: Partial<AgentSkillCoverageDto>,
): AgentSkillCoverageDto {
	return {
		id,
		scope: "global",
		reads_master: false,
		writes_master: false,
		needs_link: false,
		auto_covered: false,
		supported: true,
		...over,
	};
}

test("isAutoCoveredByMaster / needsMasterLink read the server booleans", () => {
	assert.equal(isAutoCoveredByMaster(cov("a", { auto_covered: true })), true);
	assert.equal(isAutoCoveredByMaster(cov("a", { needs_link: true })), false);
	assert.equal(isAutoCoveredByMaster(undefined), false);
	assert.equal(needsMasterLink(cov("b", { needs_link: true })), true);
	assert.equal(needsMasterLink(cov("b", { auto_covered: true })), false);
	assert.equal(needsMasterLink(undefined), false);
});

test("partitionByCoverage splits installable into autoCovered + linkTargets", () => {
	const installable = [{ id: "codex" }, { id: "claude" }, { id: "zed" }];
	const coverage: Record<string, AgentSkillCoverageDto> = {
		codex: cov("codex", { auto_covered: true, reads_master: true }),
		claude: cov("claude", { needs_link: true }),
		zed: cov("zed", { needs_link: true }),
	};
	const { autoCovered, linkTargets } = partitionByCoverage(
		installable,
		coverage,
	);
	assert.deepEqual(
		autoCovered.map((a) => a.id),
		["codex"],
	);
	assert.deepEqual(
		linkTargets.map((a) => a.id),
		["claude", "zed"],
	);
});

test("partitionByCoverage uses needs_link/auto_covered only, not reads/writes_master", () => {
	const installable = [{ id: "amp" }];
	const coverage: Record<string, AgentSkillCoverageDto> = {
		amp: cov("amp", { reads_master: true, needs_link: true }),
	};
	const { autoCovered, linkTargets } = partitionByCoverage(
		installable,
		coverage,
	);
	assert.deepEqual(autoCovered, []);
	assert.deepEqual(
		linkTargets.map((a) => a.id),
		["amp"],
	);
});

test("partitionByCoverage drops nothing: missing coverage entry is neither bucket", () => {
	const installable = [{ id: "ghost" }];
	const { autoCovered, linkTargets } = partitionByCoverage(installable, {});
	assert.deepEqual(autoCovered, []);
	assert.deepEqual(linkTargets, []);
});
````

Run, expecting FAIL (symbols do not exist yet):

```bash
cd /home/audichuang/research/aghub/crates/desktop && node --test --experimental-strip-types src/lib/agent-capabilities.test.ts
```

Expected: a missing-export / import-resolution error; non-zero exit.

---

### Task 49: Pure bucketing partition — minimal impl + pass + commit

Add the three helpers to `crates/desktop/src/lib/agent-capabilities.ts`. Extend the type import at line 1:

```ts
import type {
	AgentInfo,
	AgentSkillCoverageDto,
	TransportDto,
} from "../generated/dto";
```

(Match the file's actual existing import members; add `AgentSkillCoverageDto` to the list.) Then append at the end of the file:

```ts
export function isAutoCoveredByMaster(
	cov: AgentSkillCoverageDto | undefined,
): boolean {
	return cov?.auto_covered ?? false;
}

export function needsMasterLink(
	cov: AgentSkillCoverageDto | undefined,
): boolean {
	return cov?.needs_link ?? false;
}

/**
 * Partition an already-filtered installable agent set into the two
 * server-derived buckets. Bucketing uses ONLY `auto_covered` / `needs_link`
 * (the faithful projection of the core `LinkNeed` 3-state) — never the
 * informational `reads_master` / `writes_master`. Agents with no coverage
 * entry fall into neither bucket (Unsupported / not yet resolved).
 */
export function partitionByCoverage<A extends { id: string }>(
	installable: A[],
	coverage: Record<string, AgentSkillCoverageDto>,
): { autoCovered: A[]; linkTargets: A[] } {
	const autoCovered = installable.filter((a) =>
		isAutoCoveredByMaster(coverage[a.id]),
	);
	const linkTargets = installable.filter((a) =>
		needsMasterLink(coverage[a.id]),
	);
	return { autoCovered, linkTargets };
}
```

Run, expecting PASS:

```bash
cd /home/audichuang/research/aghub/crates/desktop && node --test --experimental-strip-types src/lib/agent-capabilities.test.ts
```

Expected: `# pass 4` / `# fail 0`.

Commit:

```bash
git add crates/desktop/src/lib/agent-capabilities.ts crates/desktop/src/lib/agent-capabilities.test.ts && git commit -m "$(cat <<'EOF'
feat(desktop): add coverage bucketing helpers (auto_covered/needs_link)

partitionByCoverage + isAutoCoveredByMaster + needsMasterLink, the
server-derived split for the symlink-only install UI. node:test unit
tests pin that bucketing uses needs_link/auto_covered only and drops no
agent.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 50: (folded into Task 49 — no separate task)

> The CHUNK 5 draft split the helper impl and its commit; both are done in Task 49. This task number is intentionally a no-op to keep numbering stable. Skip.

---

### Task 51: Coverage query key + client method + hook

No isolated test runner for React Query hooks exists; verification is `bun run typecheck`. Make all three edits, then typecheck.

**51a.** `crates/desktop/src/requests/keys.ts` — add a coverage key to the `agents` block:

```ts
	agents: {
		all: () => ["agents"] as const,
		list: () => ["agents", "list"] as const,
		availability: () => ["agents", "availability"] as const,
		coverage: (scope: string, projectRoot?: string | null) =>
			["agents", "coverage", scope, projectRoot ?? null] as const,
	},
```

(Preserve the existing keys; add only `coverage`.)

**51b.** `crates/desktop/src/lib/api.ts` — add `AgentSkillCoverageDto` to the `import type { ... }` block (alphabetically), then add the client method inside the `agents:` block:

```ts
		skillCoverage(
			scope: "global" | "project",
			projectRoot?: string | null,
		): Promise<AgentSkillCoverageDto[]> {
			return client
				.get("skills/coverage", {
					searchParams: {
						scope,
						...(projectRoot
							? { project_root: projectRoot }
							: {}),
					},
				})
				.json();
		},
```

**51c.** `crates/desktop/src/requests/agents.ts` — add the query options + the `useSkillCoverage` hook. Append (preserving the existing `agentsListQueryOptions`/`agentAvailabilityQueryOptions` exports and the file's existing imports — add `queryOptions, useQuery` from `@tanstack/react-query`, `AgentSkillCoverageDto` from generated dto, `AgentScope` from `../lib/agent-capabilities`, `useApi` from `../hooks/use-api`, `queryKeys` from `./keys` if not already imported):

```ts
export function agentSkillCoverageQueryOptions({
	api,
	scope,
	projectRoot,
}: {
	api: ApiClient;
	scope: AgentScope;
	projectRoot?: string | null;
}) {
	return queryOptions({
		queryKey: queryKeys.agents.coverage(scope, projectRoot),
		queryFn: () => api.agents.skillCoverage(scope, projectRoot ?? null),
	});
}

/**
 * Coverage DTOs for the active scope, keyed by agent id. Re-queries on scope
 * change (the global vs project NativeReader sets differ — see the classifier).
 */
export function useSkillCoverage(
	scope: AgentScope,
	projectRoot?: string | null,
): { coverage: Record<string, AgentSkillCoverageDto>; isLoading: boolean } {
	const api = useApi();
	const { data, isLoading } = useQuery(
		agentSkillCoverageQueryOptions({ api, scope, projectRoot }),
	);
	const coverage: Record<string, AgentSkillCoverageDto> = {};
	for (const entry of data ?? []) coverage[entry.id] = entry;
	return { coverage, isLoading };
}
```

(Verify `AgentScope` is exported from `lib/agent-capabilities.ts` and `ApiClient` from `./client`; match the real type names.)

Run the typecheck gate, expecting PASS:

```bash
cd /home/audichuang/research/aghub/crates/desktop && bun run typecheck
```

Expected: exits 0, no diagnostics. (If `AgentSkillCoverageDto` is reported missing, run Task 47/Task 58 regen first.)

Commit:

```bash
cd /home/audichuang/research/aghub && git add crates/desktop/src/requests/keys.ts crates/desktop/src/lib/api.ts crates/desktop/src/requests/agents.ts && git commit -m "$(cat <<'EOF'
feat(desktop): wire /skills/coverage query + useSkillCoverage hook

Adds the agents.skillCoverage API client call, the agents.coverage query
key, and a useSkillCoverage(scope, projectRoot) hook that joins coverage
DTOs into a by-id record and re-queries on scope change.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 52: (folded into Task 51 — no separate task)

> The CHUNK 5 draft split the data-path commit; it is done in Task 51. Skip.

---

### Task 53: Delete the install-layout abstraction + its test

Confirm the only consumers are the Sources page (Task 54 removes them) and the test file:

```bash
cd /home/audichuang/research/aghub && grep -rn "install-layout\|isUniversalLayout\|DEFAULT_INSTALL_LAYOUT\|InstallLayout" crates/desktop/src
```

Expected: matches only in `crates/desktop/src/lib/install-layout.ts`, `crates/desktop/src/lib/install-layout.test.ts`, and `crates/desktop/src/pages/sources/index.tsx`.

Delete the two files:

```bash
cd /home/audichuang/research/aghub && git rm crates/desktop/src/lib/install-layout.ts crates/desktop/src/lib/install-layout.test.ts
```

Do NOT typecheck yet — `sources/index.tsx` still imports them; that compile error is fixed in Task 54. Commit happens together in Task 55.

---

### Task 54: Sources page — drop toggle, universal arg, and no-agents guard

Edit `crates/desktop/src/pages/sources/index.tsx`. (Verify each block against the current file before editing — the page is large.)

**54a.** Remove `ToggleButton`/`ToggleButtonGroup` from the `@heroui/react` import.

**54b.** Remove the install-layout import block:

```ts
import {
	DEFAULT_INSTALL_LAYOUT,
	type InstallLayout,
	isUniversalLayout,
} from "../../lib/install-layout";
```

**54c.** Delete the `installLayout` state:

```ts
const [installLayout, setInstallLayout] = useState<InstallLayout>(
	DEFAULT_INSTALL_LAYOUT,
);
```

**54d.** Rename `installAgentIds` → `installableAgentIds` (it still derives from the full installable set; removal resolves against it):

```ts
const installableAgentIds = useMemo(
	() =>
		availableAgents
			.filter(
				(agent) =>
					agent.isUsable && supportsSkillMutation(agent, updateScope),
			)
			.map((agent) => agent.id),
	[availableAgents, updateScope],
);
```

**54e.** Update `deleteInstalledSkillByName` to use the renamed set (removal keeps its no-agents guard):

```ts
const deleteInstalledSkillByName = async (name: string) => {
	if (installableAgentIds.length === 0) {
		throw new Error(t("sourceRemoveNoAgents"));
	}

	await api.skills.delete(
		installableAgentIds[0],
		name,
		updateScope,
		updateProjectRoot ?? undefined,
		true,
	);
};
```

**54f.** Update the `deleteAllRemovedSkills` guard reference from `installAgentIds.length === 0` to `installableAgentIds.length === 0`.

**54g.** In `installFromSource`, remove the no-agents early-return (master-only install is now valid), pass the installable set as `agents`, and drop the `universal:` arg. Remove the `if (installAgentIds.length === 0) { toast.danger(...); return; }` block, and change the `gitInstall` call's `agents: installAgentIds` to `agents: installableAgentIds`, deleting the `universal: isUniversalLayout(installLayout),` line.

**54h.** Delete the entire install-layout toggle UI block (the `<div>` wrapping the `installLayoutLabel` text + the `ToggleButtonGroup`).

Run the typecheck gate, expecting PASS:

```bash
cd /home/audichuang/research/aghub/crates/desktop && bun run typecheck
```

Expected: exits 0, no diagnostics. (`GitInstallRequest` no longer has `universal`, so omitting it is correct; if Task 47 has not landed, the generated type still has an optional `universal?` and omitting it is still fine.)

---

### Task 55: Commit Sources page convergence + install-layout deletion

```bash
cd /home/audichuang/research/aghub && git add crates/desktop/src/pages/sources/index.tsx crates/desktop/src/lib/install-layout.ts crates/desktop/src/lib/install-layout.test.ts && git commit -m "$(cat <<'EOF'
feat(desktop): sources page is symlink-only (drop layout toggle)

Deletes install-layout.ts + its test, removes the isolation/universal
ToggleButtonGroup, stops sending `universal:` on gitInstall, and removes
the no-agents early-return so a master-only install (empty link set) is
valid. Removal still resolves against the full installable set.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
)"
```

Expected: commit succeeds.

---

### Task 56: i18n — en.ts (delete installLayout keys, add 6 coverage keys)

Edit `crates/desktop/src/lib/locales/en.ts`. Delete the four `installLayoutLabel` / `installLayoutIsolation` / `installLayoutUniversal` / `installLayoutHint` keys, and add the six coverage keys:

```ts
	sourceInstallCoveredTitle: "Already covered",
	sourceInstallCoveredHint:
		"These agents read the shared .agents master directly, so no link is created.",
	sourceInstallLinkTargetsTitle: "Will be linked",
	sourceInstallLinkTargetsHint:
		"These agents get a symlink to the shared .agents master.",
	sourceInstallNoLinkTargets:
		"No agents need a link here — the master is written on its own.",
	agentCoveredBadge: "Covered",
```

Keep `sourceInstallNoAgents` and `sourceRemoveNoAgents` (removal still uses `sourceRemoveNoAgents`). Verify no install-layout key references remain:

```bash
cd /home/audichuang/research/aghub && grep -rn "installLayoutLabel\|installLayoutIsolation\|installLayoutUniversal\|installLayoutHint" crates/desktop/src
```

Expected: empty (no matches). If `sourceInstallNoAgents` shows zero uses across `crates/desktop/src`, delete it too; otherwise keep.

---

### Task 57: i18n — zh-Hant.ts and zh-Hans.ts

**57a.** `crates/desktop/src/lib/locales/zh-Hant.ts` — delete the four `installLayout*` keys, add:

```ts
	sourceInstallCoveredTitle: "已涵蓋",
	sourceInstallCoveredHint:
		"這些 Agent 會直接讀取共用的 .agents 主檔，因此不會建立連結。",
	sourceInstallLinkTargetsTitle: "將建立連結",
	sourceInstallLinkTargetsHint:
		"這些 Agent 會取得指向共用 .agents 主檔的軟連結。",
	sourceInstallNoLinkTargets:
		"此處沒有 Agent 需要連結 — 只會寫入主檔。",
	agentCoveredBadge: "已涵蓋",
```

**57b.** `crates/desktop/src/lib/locales/zh-Hans.ts` — delete the four `installLayout*` keys, add:

```ts
	sourceInstallCoveredTitle: "已覆盖",
	sourceInstallCoveredHint:
		"这些 Agent 会直接读取共享的 .agents 主文件，因此不会创建链接。",
	sourceInstallLinkTargetsTitle: "将创建链接",
	sourceInstallLinkTargetsHint:
		"这些 Agent 会获得指向共享 .agents 主文件的软链接。",
	sourceInstallNoLinkTargets:
		"此处没有 Agent 需要链接 — 只会写入主文件。",
	agentCoveredBadge: "已覆盖",
```

Run the typecheck gate (locale objects are typed; missing/extra keys across locales surface as a tsc error if a shared `Resources` type is enforced):

```bash
cd /home/audichuang/research/aghub/crates/desktop && bun run typecheck
```

Expected: exits 0, no diagnostics.

---

### Task 58: Regenerate the DTO surface (verify / pick up drift) + prettier

Task 47 already regenerated the DTO surface from the Rust side. Re-run here to pick up any drift and confirm the FE-visible types:

```bash
cd /home/audichuang/research/aghub/crates/desktop && bun run generate:dto && bunx prettier --write src/generated
```

Inspect:

```bash
cd /home/audichuang/research/aghub && git --no-pager diff --stat crates/desktop/src/generated
cd /home/audichuang/research/aghub && grep -n "universal" crates/desktop/src/generated/dto/GitInstallRequest.ts
cd /home/audichuang/research/aghub && grep -n "AgentSkillCoverageDto" crates/desktop/src/generated/dto/index.ts
```

Expected: `universal` grep empty; `AgentSkillCoverageDto` re-export present; the `--stat` shows only the three intended files (or nothing new if Task 47 already committed them).

Run typecheck:

```bash
cd /home/audichuang/research/aghub/crates/desktop && bun run typecheck
```

Expected: exits 0.

---

### Task 59: Commit i18n + DTO regen

```bash
cd /home/audichuang/research/aghub && git add crates/desktop/src/lib/locales/en.ts crates/desktop/src/lib/locales/zh-Hant.ts crates/desktop/src/lib/locales/zh-Hans.ts crates/desktop/src/generated && git commit -m "$(cat <<'EOF'
feat(desktop): coverage i18n keys + regenerate DTO (drop universal)

Removes installLayout* keys and adds the 6 coverage strings in en /
zh-Hant / zh-Hans. Confirms the ts-rs DTO surface (GitInstallRequest
without `universal`, AgentSkillCoverageDto exported); prettier applied.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
)"
```

Expected: commit succeeds (skip if the generated diff is empty after Task 47's commit — then commit only the locale files).

---

### Task 60: Import panel — coverage-driven AgentSelector + Already-covered chips

Edit `crates/desktop/src/components/import-github-skill-panel.tsx`.

**60a.** Extend imports. Replace:

```ts
import { useApi } from "../hooks/use-api";
import { supportsSkillMutation } from "../lib/agent-capabilities";
```

with:

```ts
import { useApi } from "../hooks/use-api";
import {
	partitionByCoverage,
	supportsSkillMutation,
} from "../lib/agent-capabilities";
import { useSkillCoverage } from "../requests/agents";
```

**60b.** Right after the `skillAgents` useMemo block, insert:

```ts
const scope = projectPath ? "project" : "global";
const { coverage } = useSkillCoverage(scope, projectPath ?? null);
const { autoCovered, linkTargets } = useMemo(
	() => partitionByCoverage(skillAgents, coverage),
	[skillAgents, coverage],
);
```

**60c.** Default-select the first **linkTarget**. Replace `selectedAgents: skillAgents[0] ? [skillAgents[0].id] : [],` with `selectedAgents: linkTargets[0] ? [linkTargets[0].id] : [],`.

**60d.** Feed `AgentSelector` from `linkTargets`, relax the required-selection rule (valid when ≥1 selected OR no link targets exist), and render an "Already covered" chip list (or the empty hint). Replace the `<Controller name="selectedAgents" ...>` block with the coverage-driven version (the `validate` rule becomes `linkTargets.length === 0 || value.length > 0 ? true : t("validationAgentsRequired")`; `AgentSelector agents={linkTargets}`; `label={t("sourceInstallLinkTargetsTitle")}`, `emptyHelpText={t("sourceInstallLinkTargetsHint")}`; when `linkTargets.length === 0` render `<p>{t("sourceInstallNoLinkTargets")}</p>`; when `autoCovered.length > 0` render a labeled `<Chip>` list using `t("sourceInstallCoveredTitle")`/`t("sourceInstallCoveredHint")` and each `agent.display_name`):

```tsx
<Controller
	name="selectedAgents"
	control={control}
	rules={{
		validate: (value) =>
			linkTargets.length === 0 || value.length > 0
				? true
				: t("validationAgentsRequired"),
	}}
	render={({ field, fieldState }) => (
		<div className="space-y-4">
			{linkTargets.length > 0 ? (
				<AgentSelector
					agents={linkTargets}
					selectedKeys={new Set(field.value)}
					onSelectionChange={(keys) => field.onChange([...keys])}
					label={t("sourceInstallLinkTargetsTitle")}
					emptyMessage={t("noAgentsAvailable")}
					emptyHelpText={t("sourceInstallLinkTargetsHint")}
					variant="secondary"
					errorMessage={fieldState.error?.message}
				/>
			) : (
				<p className="text-xs text-muted">
					{t("sourceInstallNoLinkTargets")}
				</p>
			)}
			{autoCovered.length > 0 && (
				<div className="space-y-1.5">
					<span className="text-sm font-medium text-foreground">
						{t("sourceInstallCoveredTitle")}
					</span>
					<span className="block text-xs text-muted">
						{t("sourceInstallCoveredHint")}
					</span>
					<div className="flex flex-wrap gap-1.5 pt-1">
						{autoCovered.map((agent) => (
							<Chip key={agent.id} size="sm" variant="secondary">
								{agent.display_name}
							</Chip>
						))}
					</div>
				</div>
			)}
		</div>
	)}
/>
```

(Match the existing field names — `noAgentsAvailable`, `validationAgentsRequired`, `AgentSelector` prop names, `Chip` import. `display_name` exists on `AgentInfo`, which `AvailableAgent` extends.)

**60e.** Relax the Card-1 "Scan" button so it no longer disables on `skillAgents.length === 0`. Remove the `|| skillAgents.length === 0` clause from its `isDisabled`.

Run the typecheck gate, expecting PASS:

```bash
cd /home/audichuang/research/aghub/crates/desktop && bun run typecheck
```

Expected: exits 0, no diagnostics.

---

### Task 61: Lint + format the changed frontend files

```bash
cd /home/audichuang/research/aghub/crates/desktop && bunx prettier --write src/components/import-github-skill-panel.tsx src/pages/sources/index.tsx src/requests/agents.ts src/lib/api.ts src/requests/keys.ts src/lib/agent-capabilities.ts src/lib/agent-capabilities.test.ts src/lib/locales/en.ts src/lib/locales/zh-Hant.ts src/lib/locales/zh-Hans.ts && bun run lint:check && bun run typecheck
```

Expected: prettier writes (possibly 0 changes), `lint:check` exits 0, `typecheck` exits 0.

Re-run the bucketing unit test:

```bash
cd /home/audichuang/research/aghub/crates/desktop && node --test --experimental-strip-types src/lib/agent-capabilities.test.ts
```

Expected: `# pass 4` / `# fail 0`.

---

### Task 62: Commit the import panel + lint pass

```bash
cd /home/audichuang/research/aghub && git add crates/desktop/src/components/import-github-skill-panel.tsx crates/desktop/src/pages/sources/index.tsx crates/desktop/src/requests/agents.ts crates/desktop/src/lib/api.ts crates/desktop/src/requests/keys.ts crates/desktop/src/lib/agent-capabilities.ts crates/desktop/src/lib/agent-capabilities.test.ts crates/desktop/src/lib/locales/en.ts crates/desktop/src/lib/locales/zh-Hant.ts crates/desktop/src/lib/locales/zh-Hans.ts && git commit -m "$(cat <<'EOF'
feat(desktop): import panel selects link targets, shows covered agents

AgentSelector now feeds from the needs_link bucket only and defaults to
the first link target; required-selection is relaxed (valid with >=1
selected or zero link targets). Auto-covered agents render as read-only
"Covered" chips; an empty link set shows the master-only hint. Scan no
longer disables when there are zero installable agents.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
)"
```

Expected: commit succeeds.

---

### Task 63: Final desktop verification gate

```bash
cd /home/audichuang/research/aghub/crates/desktop && bun run format:check && bun run lint:check && bun run typecheck && node --test --experimental-strip-types src/lib/agent-capabilities.test.ts
```

Expected: `format:check` exits 0, `lint:check` exits 0, `typecheck` exits 0, and the test run ends `# fail 0`. (Add other existing `node:test` suites to the run if present, e.g. `src/lib/connection-logic.test.ts`.)

Confirm the install-layout module is fully gone:

```bash
cd /home/audichuang/research/aghub && git ls-files crates/desktop/src/lib/install-layout.ts crates/desktop/src/lib/install-layout.test.ts
```

Expected: empty (both files deleted from tracking).

---

## Tasks 64–67 — Cross-cutting verification (npx golden, Windows CI, no-copy survivor grep, type-system no-copy guarantee)

> These four are verification/grep/build gates (no production code). Run Tasks 64 and 66 after each of the core (Task 35) and API (Task 47) chunks as a guard, and ALL of Tasks 64–67 as the final gate. All `git diff` baselines compare against `origin/feat/cli-sources` (the pre-feature point).

### Task 64: Confirm the npx hash / interop goldens stay GREEN (unchanged) + add T-MASTER-HASH-STABLE and T-LOCK-PARITY-LINK-VS-COPY

The spec requires `hash_parity_golden.rs` and `npx_interop.rs` to need ZERO edits and `copy_dir_recursive`'s exclude lists to stay byte-identical. The spec Test Strategy ALSO names two NEW link-era regression tests (spec lines 653-654) that are NOT subsumed by the existing goldens and need owning code here: **T-MASTER-HASH-STABLE** (linking N agents must never mutate the Master's folder hash) and **T-LOCK-PARITY-LINK-VS-COPY** (the install lock written via the link-era path is byte-identical to one written from the same source folder in the copy-era, since both hash the SOURCE folder).

- [ ] Run the three goldens (run BEFORE Task 1 to record a baseline, and re-run after Tasks 11/23/35/47 and as the final gate):
    ```bash
    cargo test --package skill --test hash_parity_golden -- --nocapture && \
    cargo test --package skill --test npx_interop -- --nocapture && \
    cargo test --package skill --test fixture_validation -- --nocapture
    ```
    Expected: each binary ends `test result: ok. <N> passed; 0 failed`. If ANY golden fails, the change touched Master materialization (`copy_dir_recursive`, `EXCLUDE_FILES`, `EXCLUDE_DIRS`, or traversal order) — STOP and revert; the round-trip contract (Decision 7) is broken.
- [ ] Prove the golden files themselves were not edited:
    ```bash
    git diff --stat origin/feat/cli-sources -- \
      crates/skill/tests/hash_parity_golden.rs \
      crates/skill/tests/npx_interop.rs \
      crates/skill/tests/fixtures
    ```
    Expected: empty (no lines). Any diff is a spec violation.
- [ ] Confirm the exclude lists are byte-identical after the move into `linker/mod.rs`:
    ```bash
    git show origin/feat/cli-sources:crates/core/src/skills/install_layout.rs \
      | sed -n '/EXCLUDE_FILES/,/^}/p' > /tmp/old_excludes.txt
    sed -n '/EXCLUDE_FILES/,/^}/p' crates/core/src/skills/linker/mod.rs > /tmp/new_excludes.txt
    diff /tmp/old_excludes.txt /tmp/new_excludes.txt
    ```
    Expected: empty (`diff` prints nothing, exit 0). A non-empty diff means an exclude entry changed — the npx folder hash will drift.
- [ ] **T-MASTER-HASH-STABLE** (spec line 653) — add a cross-platform core test proving that linking agents to a Master never mutates the Master's folder hash. Add to the `#[cfg(test)] mod tests` of `crates/core/src/skills/linker/mod.rs` (reuses `make_source` from Task 10 and `link_agents_to_canonical` from Task 10):

    ```rust
    	// T-MASTER-HASH-STABLE: linking N agents to a materialized Master must
    	// never change the Master's folder hash (links live OUTSIDE the Master;
    	// the npx round-trip contract, Decision 7, depends on this).
    	#[test]
    	fn linking_agents_does_not_mutate_master_folder_hash() {
    		use tempfile::tempdir;
    		let tmp = tempdir().unwrap();
    		let root = std::fs::canonicalize(tmp.path()).unwrap();
    		let src = make_source(&root);
    		let canonical = root.join(".agents/skills/my-skill");
    		// Materialize the Master once (copy + npx exclusions), no links yet.
    		install_universal(&src, &canonical, &[], LinkTarget::Absolute).unwrap();
    		let before =
    			skill::compute_skill_folder_hash(&canonical).unwrap();

    		// Now link three agents to the SAME Master.
    		let agents: Vec<std::path::PathBuf> = ["claude", "zed", "windsurf"]
    			.iter()
    			.map(|a| root.join(format!(".{a}/skills")))
    			.collect();
    		link_agents_to_canonical(&canonical, &agents, LinkTarget::Absolute)
    			.unwrap();
    		let after =
    			skill::compute_skill_folder_hash(&canonical).unwrap();

    		assert_eq!(before, after, "linking must not mutate the Master hash");
    	}
    ```

    (Confirm the hash fn name/signature: `grep -n "pub fn compute_skill_folder_hash" crates/skill/src/`. If `aghub-core` does not already depend on the `skill` crate in a way that exposes it to the test, mirror how `install_fetched.rs` calls `skill::compute_skill_folder_hash` — it does, so the path is available. Adjust the call to the exact signature.)

- [ ] **T-LOCK-PARITY-LINK-VS-COPY** (spec line 654, NEW) — pin that the WHOLE install lock ENTRY (not just its hash) written by the link-era path is byte-identical to a copy-era fixture, since both eras key the lock on the SOURCE folder (`skill_source_root` → `compute_skill_folder_hash` → `write_install_lock` in `install_fetched.rs`, not the installed dir). **P2-F fix:** the earlier draft's assertions only compared `skillFolderHash`/`source_type`, which does NOT justify a "byte-identical" claim. This version SERIALIZES the written lock entry to JSON and compares the FULL entry against a committed copy-era fixture (captured from the pre-feature copy implementation), so a schema/field/ordering drift fails the test. Add a core test in `crates/core/src/skills/install_fetched.rs`'s `#[cfg(test)] mod tests` (mirror the existing install-lock tests' env-isolation + temp-project setup — grep `mod tests` in that file and copy the closest `*_writes_lock` test's harness):
    ```rust
    	// T-LOCK-PARITY-LINK-VS-COPY: the FULL install-lock entry written by the
    	// symlink-only (link-era) path is byte-identical to the copy-era fixture,
    	// because both eras hash the SOURCE folder and write the same schema. Pins
    	// the round-trip contract (Decision 7) at the FULL-ENTRY level (every
    	// field + key order), not just the folder hash.
    	#[test]
    	fn install_lock_entry_byte_identical_to_copy_era_fixture() {
    		// 1. Build an isolated project + a deterministic source skill (fixed
    		//    SKILL.md bytes so compute_skill_folder_hash is reproducible).
    		// 2. Run the symlink-only install via install_fetched_skill_and_lock
    		//    (target: LinkTarget::Relative — the field is `target` after Task 47a).
    		// 3. Read the written lock entry for the skill via the sibling tests'
    		//    lock-read helper (skill::lock::*::read_*_lock).
    		// 4. Serialize that entry with serde_json::to_value and assert it equals
    		//    the COPY-ERA fixture value below, field-for-field. The fixture is the
    		//    JSON a copy-era install wrote for the SAME source (skillPath, source,
    		//    sourceType, ref, skillFolderHash, + any version/installedAt schema
    		//    keys). Capture it ONCE from `git show origin/feat/cli-sources` era
    		//    behavior (or compute the hash inline since the source is fixed) and
    		//    inline it as a serde_json::json!({...}) literal.
    		//
    		//   let expected_hash =
    		//       skill::compute_skill_folder_hash(&source_root).unwrap();
    		//   let entry = <read the written lock entry>;
    		//   let got = serde_json::to_value(&entry).unwrap();
    		//   let want = serde_json::json!({
    		//       "skillPath": "my-skill/SKILL.md",
    		//       "source": "<resolved lock source>",
    		//       "sourceType": "local",            // or "github" per the source
    		//       "skillFolderHash": expected_hash, // copy-era == link-era
    		//       /* + every other schema field the copy-era entry carried */
    		//   });
    		//   assert_eq!(got, want, "link-era lock entry must match copy-era byte-for-byte");
    	}
    ```
    Adapt the body to the post-Task-47a `FetchedSkillInstallRequest` shape (`target: LinkTarget`) and the lock-entry struct's serde shape. The load-bearing assertion is `serde_json::to_value(written_entry) == <full copy-era fixture>` — every field, not just the hash. Build the `want` fixture from the actual copy-era schema (verify field names + serde rename attrs against `crates/skill/src/install.rs` / the lock entry struct + `git show origin/feat/cli-sources:crates/core/src/skills/install_fetched.rs` for what the copy path recorded). If a field is intrinsically run-dependent (e.g. an `installedAt` timestamp), assert it is present/well-formed separately and compare the rest of the object — note that explicitly in the test rather than dropping it.
- [ ] Run the two new tests: `cargo test -p aghub-core --lib linking_agents_does_not_mutate_master_folder_hash -- --exact 2>&1 | tail -15 && cargo test -p aghub-core --lib install_lock_entry_byte_identical_to_copy_era_fixture -- --exact 2>&1 | tail -15` → expect both PASS.
- [ ] Commit the two new regression tests (the golden-green checks above stay verification-only — no edits to golden files):
    ```bash
    git add -A && git commit -m "test(core): T-MASTER-HASH-STABLE + T-LOCK-PARITY-LINK-VS-COPY (round-trip guards)
    ```

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"

````
- [ ] The golden green-checks and exclude-list diffs above remain verification-only; no commit for those.

---

### Task 65: Confirm Windows CI runs the junction tests via `just test` on `windows-latest`

The biggest fix relocates the link tests off the `#[cfg(all(test, unix))]` gate so the junction tests EXECUTE on `windows-latest`. Confirm the CI plumbing carries them.

- [ ] Confirm the `test` job matrix includes `windows-latest` and runs `just test`:
```bash
grep -n 'windows-latest' .github/workflows/ci.yml
grep -n 'run: just test' .github/workflows/ci.yml
````

Expected: a `windows-latest` entry in the `test` job's matrix and a `run: just test` step. If `windows-latest` is absent from the `test` job, the junction tests never execute — STOP and add it.

- [ ] Confirm `just test` is `cargo test --workspace` (so the relocated `linker/` tests + `#[cfg(windows)]` blocks are picked up automatically):
    ```bash
    sed -n '18,19p' justfile
    ```
    Expected: `test:` then `cargo test --workspace`.
- [ ] Confirm the release gate also runs `just test` on `windows-latest`:
    ```bash
    grep -n 'windows-latest' .github/workflows/release.yml | head -1
    grep -n 'run: just test' .github/workflows/release.yml
    ```
    Expected: a `windows-latest` matrix entry + `run: just test` inside the release `test` job.
- [ ] Confirm the Windows junction tests force the junction via `create_junction` (NOT via `symlink_dir` failing — Developer Mode makes `symlink_dir` succeed) and that T-WIN-JUNCTION-DETECT asserts both `Linker::is_link == true` AND `is_symlink() == false`:
    ```bash
    grep -n 'cfg(windows)' crates/core/src/skills/linker/mod.rs
    grep -n 'create_junction' crates/core/src/skills/linker/mod.rs
    ```
    Expected: a `#[cfg(windows)] mod windows_specific` and at least one test calling `create_junction(...)` directly (from Task 9). A junction test that only calls `Linker::link` and hopes `symlink_dir` fails is a spec violation — flag back to Task 9.
- [ ] Verification-only; no commit.

---

### Task 66: No-copy-survivor verification grep (install/removal/discovery surface — INCLUDES `crates/cli`)

After all chunks land (incl. Task 47a's shim deletion), the only `copy_dir_recursive` / copy-layout symbols left must be the expected survivors. **The grep now covers `crates/cli` too (P0-3): `source.rs::apply_install` was a copy-vs-universal decision site and must show ZERO copy-layout symbols after Task 35c + Task 47a.**

- [ ] Run the spec's verification grep over ALL THREE consumer crates (run after Task 47a; re-run as the final gate):

    ```bash
    rg 'copy_dir_recursive|IsolatedCopy|SkillInstallLayout|copied_fallback|CopiedFallback|install_git_skill_to_dir|install_git_skill_universal|build_git_install_groups|should_write_install_lock' crates/api crates/cli crates/core
    ```

    Expected — EXACTLY these survivors and nothing else:
    - `crates/core/src/skills/linker/mod.rs::copy_dir_recursive` (Master materialization — KEEP)
    - `crates/core/src/transfer.rs::copy_dir_recursive` (cross-agent transfer, Decision 9 — KEEP)
    - `crates/core/src/skills/install_fetched.rs::should_write_install_lock` + `skill_lock_contains` (lock gate helpers — KEEP)

    ZERO hits in `crates/api` and ZERO in `crates/cli`. `SkillInstallLayout` must appear NOWHERE (Task 47a deleted it). Any `IsolatedCopy`/`copied_fallback`/`CopiedFallback`/`SkillInstallLayout` anywhere is a leftover copy path and MUST be removed. A `SkillInstallLayout` hit specifically means a construction site Task 47a missed — fix that site.

- [ ] Disambiguate the known false-positive-shaped survivor — `crates/core/src/skills/update.rs::copy_dir_recursive_skip_symlinks` is a DIFFERENT symbol (skill-update staging, not the install Master copy) and is NOT matched by the grep above. Confirm it is untouched:
    ```bash
    grep -n 'copy_dir_recursive_skip_symlinks' crates/core/src/skills/update.rs
    git diff origin/feat/cli-sources -- crates/core/src/skills/update.rs
    ```
    Expected: the symbol present; the `git diff` empty (update.rs is out of scope).
- [ ] Confirm no surviving `is_symlink()` probe on the install/removal/discovery surface (every one must be `Linker::is_link`), INCLUDING the `entry_allowed` helper fixed by Task 43a (`skills.rs:942`):
    ```bash
    grep -rn 'file_type().is_symlink()' \
      crates/core/src/skills/discovery.rs \
      crates/core/src/skills/removal.rs \
      crates/core/src/manager/skill.rs \
      crates/api/src/routes/skills.rs
    ```
    Expected: empty (no lines) — including `crates/api/src/routes/skills.rs`, where BOTH `delete_skill_by_path`'s `path_is_symlink` (Task 43) AND `entry_allowed` (Task 43a) are now routed through `Linker::is_link`. A surviving probe at any of these is a P0 miss — route it through `Linker::is_link`. (Test-module `is_symlink()` assertions in `linker/mod.rs` are allowed — that file is deliberately excluded from the grep; T-WIN-JUNCTION-DETECT legitimately asserts `is_symlink()==false`. Test-module `is_symlink()` ASSERTIONS inside `skills.rs`'s `mod tests` are also allowed — only NON-test probe sites count.)
- [ ] Verification-only; no commit.

---

### Task 67: Type-system no-copy guarantee + cross-platform NO-COPY runtime assertion + `just preflight`

- [ ] Prove no code still pattern-matches a deleted copy outcome (a miss fails the workspace build on every platform):
    ```bash
    grep -rn 'copied_fallback\|CopiedFallback' crates/
    ```
    Expected: empty (no lines). Any match (a `report.copied_fallback` read, or a `LinkResult::CopiedFallback` arm) must be deleted before the build compiles — this is the compile-time no-copy guarantee.
- [ ] Full workspace build + clippy as the cross-cutting type-system gate:
    ```bash
    cargo clippy --workspace --all-targets -- -D warnings
    ```
    Expected: ends `Finished` with no `error:` / no `warning:`. A compile error mentioning `copied_fallback`, `CopiedFallback`, `IsolatedCopy`, or `SkillInstallLayout` means a caller of a deleted symbol survived — fix the caller, do not re-add the symbol.
- [ ] Run the cross-platform T-NOCOPY runtime test (defined in Task 6's base test module):
    ```bash
    cargo test --package aghub-core no_copy -- --nocapture
    ```
    Expected: a `test result: ok.` line; the T-NOCOPY test (and, on Windows, the `windows_specific` junction tests) appear in the running list. (The unix install_fetched no-copy test name is `install_fetched_links_master_never_copies` — run it explicitly too if `no_copy` does not match it.)
- [ ] Final full gate (the authoritative cross-platform check; the same gate CI runs):
    ```bash
    just preflight
    ```
    Expected: `fmt --check`, `clippy -D warnings`, desktop `typecheck`, `cargo test --workspace`, and doc tests all pass, ending green. On Windows the same `just test` leg runs the junction tests; locally on Linux the `#[cfg(windows)]` tests are skipped but the Windows CI leg covers them.
- [ ] Verification-only; the green gate is the deliverable. When `just preflight` is green AND Tasks 64–66 all pass, the feature satisfies the spec's NO-COPY regression contract.

---

## Test Strategy coverage map (spec T-\* → owning task)

| Spec T-\* item                                                                                    | Owning task                                                                                                            | Notes                                                                               |
| ------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------- |
| Test-module split (base + `#[cfg(unix)]` + `#[cfg(windows)]`)                                     | Tasks 1–11                                                                                                             | confirmed cross-cutting by Task 65                                                  |
| Compile-time no-copy guarantee (delete `copied_fallback`/`CopiedFallback`)                        | Task 4 (delete), Task 67 (verify)                                                                                      | grep + workspace build                                                              |
| Green-workspace-per-commit (shim until late delete)                                               | Task 20 (shim), Tasks 21–23 (ignored fields), Task 47a (delete)                                                        | P0-1 + P0-2; every commit builds the whole workspace                                |
| sources_install_tests.rs migrated to symlink-only assertions                                      | Task 23a                                                                                                               | P0-2; same chunk as the dispatch change                                             |
| T-NOCOPY (is_link true + sentinel-through-link)                                                   | Task 6 (linker), Task 24 (install_fetched), Task 34 (add_skill_from_path + add_skill manual-create), re-run by Task 67 | cross-platform                                                                      |
| T-MASTER-HASH-STABLE (linking N agents never mutates Master hash)                                 | Task 64                                                                                                                | spec line 653; new core test                                                        |
| T-LOCK-PARITY-LINK-VS-COPY (FULL lock entry byte-identical to copy-era fixture)                   | Task 64                                                                                                                | spec line 654 (NEW); P2-F: compares serialized full entry vs fixture, not just hash |
| install_universal_layout folds report.conflicts (occupied slot → installed:false)                 | Task 23                                                                                                                | P1-D; conflict is not a successful install                                          |
| Coverage DTO carries REAL reads/writes-master facts                                               | Task 38 (handler), Tasks 12/13 (AgentLinkPlan fields)                                                                  | P2-G; not guessed from auto_covered                                                 |
| T-WIN-JUNCTION-DETECT (force via `create_junction`; is_link true & is_symlink false)              | Task 9                                                                                                                 | windows-latest; Task 65 verifies it calls `create_junction`                         |
| T-WIN-JUNCTION-REMOVE (`unlink` junction, Master intact)                                          | Task 9                                                                                                                 | proves `remove_dir` not `remove_dir_all`                                            |
| T-NONABS-TARGET-ERR (non-abs canonical → `NonAbsoluteTarget`)                                     | Tasks 6 & 10                                                                                                           | unit tests of the assertion                                                         |
| T-HARDERR (`#[cfg(unix)]` EACCES via 0o500; skip under root)                                      | Task 8                                                                                                                 | `testing-fs-failures` technique                                                     |
| T-IDEMP (2nd run already_linked)                                                                  | Task 7                                                                                                                 | detection via `Linker::is_link`                                                     |
| T-CONFLICT-REALDIR (real dir not clobbered)                                                       | Task 7                                                                                                                 |                                                                                     |
| T-CONFLICT-FOREIGN-LINK (foreign link not clobbered)                                              | Task 7                                                                                                                 | NEW                                                                                 |
| T-REL-PROJECT / T-ABS-GLOBAL (`#[cfg(unix)]` read_link form)                                      | Task 11                                                                                                                |                                                                                     |
| `relative_path_computes_minimal_dotdot` (cross-platform)                                          | Task 4                                                                                                                 | kept                                                                                |
| T-AGENTS-NATIVE-NO-LINK (no link inside `.agents/skills`)                                         | Tasks 13–18 (classify matrix)                                                                                          |                                                                                     |
| Classifier matrix (global natives; project broader set; Amp/Kimi @global=NeedsLink)               | Tasks 13–16                                                                                                            | real descriptors                                                                    |
| 3-state totality                                                                                  | Task 17                                                                                                                | bucketing invariant                                                                 |
| macOS /var→/private (canonicalize both sides)                                                     | Task 18                                                                                                                |                                                                                     |
| NPX hash/interop goldens stay GREEN, zero edits                                                   | Task 64                                                                                                                | green-check + diff-stat empty                                                       |
| execute_removal junction unlink (not remove_dir_all)                                              | Task 29 (impl)                                                                                                         |                                                                                     |
| T-REMOVE-SKILL-PATH-JUNCTION (`#[cfg(windows)]`)                                                  | Task 27                                                                                                                | P0 missed call site                                                                 |
| T-DISCOVERY-JUNCTION-CANONICAL (`#[cfg(windows)]`)                                                | Task 33                                                                                                                | P0-A                                                                                |
| T-PLAN-JUNCTION-REFERRER (`#[cfg(windows)]`)                                                      | Task 31                                                                                                                | P0-B                                                                                |
| T-EXTERNAL-JUNCTION-REFERRER (`#[cfg(windows)]`)                                                  | Task 31                                                                                                                | P0-B                                                                                |
| T-DELETE-BY-PATH-JUNCTION (`#[cfg(windows)]`)                                                     | Task 44                                                                                                                | P1-E                                                                                |
| delete_skill_by_path absolutizes relative project_root (skills.rs:238)                            | Task 36a                                                                                                               | P1-F                                                                                |
| entry_allowed routes link probe through Linker::is_link (skills.rs:942)                           | Task 43a                                                                                                               | P1-E2; junction-blind containment guard                                             |
| T-REL-ROOT-ABSOLUTIZED (relative root → absolute canonical)                                       | Task 45                                                                                                                | P0-C                                                                                |
| Windows-CI actually runs the junction tests                                                       | Task 65                                                                                                                | confirm ci.yml + release.yml + justfile                                             |
| No-copy survivor grep (incl. `crates/cli`)                                                        | Task 66                                                                                                                | spec verification grep + P0-3                                                       |
| DTO regen + prettier (GitInstallRequest.universal removed; InstallSkillResponse rows)             | Tasks 47, 58                                                                                                           | generated-DTO workflow                                                              |
| CLI `add --universal` deprecation no-op + collapse copy branches (spec §line 455-457, Decision 3) | Task 35a (impl), Task 35b (test)                                                                                       | `crates/cli` add.rs + main.rs                                                       |
| CLI `source sync --universal` no-op + collapse apply_install copy branch                          | Task 35c                                                                                                               | P0-3; `crates/cli` source.rs + main.rs; on the install surface                      |
| import_skill inherits symlink-only via add_skill_from_path (spec line 447)                        | Task 46 (GAP-4 test)                                                                                                   | master+link + lock-from-source                                                      |

**Unmapped check**: every T-_ and named-fixture item in the spec's Test Strategy appears above, AND every spec Call-site-Rewiring surface has an owning task — including the CLI section (spec line 455-457 → Tasks 35a/35b for `add`, Task 35c for `source sync`), the `add_skill` manual-create conversion (spec line 402 → Task 25 + Task 34), and import_skill (spec line 447 → Task 46 GAP-4 test). Every Codex review finding is owned: P0-1/P0-2 (green-workspace-per-commit) → Task 20 shim + Tasks 21–23 ignored fields + Task 23a + Task 47a; P0-3 (`source sync` in scope) → Task 35c + Task 66; P1-D (fold conflicts) → Task 23; P1-E2 (`entry_allowed`) → Task 43a; P1-F (delete-by-path root) → Task 36a; P2-F (T-LOCK-PARITY full entry) → Task 64; P2-G (coverage real facts) → Tasks 12/13/38. The only items not a discrete T-_ are golden-stays-green (→ Task 64) and Windows-CI plumbing (→ Task 65). No spec Test Strategy item and no spec Call-site-Rewiring surface is unmapped.
