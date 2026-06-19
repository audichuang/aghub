# CLI Sources Management Implementation Plan (rev. 3 — post adversarial review rounds 1+2)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Bring the desktop "Sources" feature to the CLI as `aghub-cli source <list|diff|sync>`, scoped to the current project + global, by extracting the Sources domain logic out of the API route into shared crates that both API and CLI consume.

**Architecture:** Three layers. (1) Lift the Sources list + diff-classification (currently inline in `crates/api/src/routes/sources.rs`) into `skill_update::sources`, split into a **fetch layer** (`fetch_source_with_resolver` → `FetchedRepo`) and a **pure classification layer** (`classify_repo_skills(root, &baseline)`), with fetch + token injected via `Fetcher`/`TokenResolver`. The API keeps its current **merged-baseline, single-classification, flat** behavior (so existing route tests stay byte-identical); the CLI assembles a **per-scope** view itself from the same lower-level functions, reusing the one `FetchedRepo` for both diff and install. (2) Add a no-network `install_fetched_skill_and_lock` primitive to `aghub-core` that installs an already-fetched skill (isolated copy via the _existing route-local recursive copy semantics_, or universal via `install_universal`) into resolved agent dirs and writes the install lock, returning **per-agent** results. (3) Add the CLI `source` group consuming both, with an env-backed credential resolver and dry-run/`--yes` gating.

**Tech Stack:** Rust workspace (`skill-update`, `aghub-core`, `aghub-api`, `aghub-cli`), `gix`/`aghub-git`, `clap`, `assert_cmd`, Rocket. Hard tabs (width 4), 80-col, `cargo clippy -D warnings`.

**Spec:** `docs/specs/2026-06-19-cli-sources-management.md`

---

## Conventions for every task

- **Indentation: hard tabs, never spaces. Max 80 columns.** Enforced; clippy warnings are errors.
- Single test: `cargo test --package <crate> <test_name> -- --exact`.
- Phase gate before a "phase done" commit: `just preflight`.
- **Every commit must compile and pass tests on its own.** Never commit a state where the API route calls a function that has been deleted (see Task 2 below — move+delete+delegate land together).
- We are on `main`; if your harness requires a branch, create `feat/cli-sources` first.

---

## Decisions locked by the adversarial review (read before coding)

1. **API behavior is preserved by keeping the merged path, NOT by per-scope concat.** The old route merges global+project into one `Baseline` keyed by `skill_path` (project shadows global) and classifies once (`sources.rs:160`, `:277`, `:325`, `:371`). The shared module exposes a `merged_baseline_for_source` that reproduces this exactly; the API calls it. Per-scope is a CLI-only assembly.
2. **The service splits fetch from classify** so the CLI can reuse one `FetchedRepo` for diff _and_ install: `fetch_source_with_resolver(...) -> FetchedRepo` and `classify_repo_skills(root, &baseline) -> Vec<SourceSkillDiff>`. `diff_source` is a thin convenience wrapper used by the API only.
3. **`SourceSkillDiff` carries `reason: Option<UncheckableReason>`** so the `removed`→`"noPath"` signal and `uncheckable` reasons survive (DTO requires it: `dto/sources.rs:85`; tests assert it: `sources.rs:933`).
4. **The install primitive returns per-agent results** (`Vec<AgentInstallResult>`) so the API can rebuild its current per-agent success/invalid-agent response.
5. **Isolated copy uses the existing route-local recursive copy** (`copy_dir_recursive` + `get_skill_root`, `skills.rs:666-668`) — NOT `install_universal(.., &[], false)`, which excludes `metadata.json`/`.git`/… and would change behavior. Extract that copy into core verbatim.
6. **CLI credentials:** the env resolver returns a token used by `GitFetcher` as the `x-access-token` password (`git.rs:28`). `GIT_USERNAME` is not consumed by `GitFetcher`; CLI source auth is **token-in-`GIT_PASSWORD`** (also accept `GITHUB_TOKEN`). Document this; do not pretend username/password basic-auth works.
7. **The CLI test fetch hook must be a runtime hook**, not `#[cfg(test)]` (assert_cmd spawns the real binary). Gate it on `cfg(debug_assertions)`.
8. **CLI test isolation must set the full env set** `HOME`/`USERPROFILE`/`APPDATA`/`XDG_STATE_HOME` (global lock reads `XDG_STATE_HOME`, `skill/src/lock/io.rs:15`). Reuse the existing `isolated_cli`-style helper in `cli_tests.rs:204`.

---

## File Structure

**New files:**

- `crates/skill-update/src/sources.rs` — Sources domain: `list_sources`, `fetch_source_with_resolver`, `classify_repo_skills`, baseline builders, `diff_source` (API convenience), domain types.
- `crates/cli/src/commands/source.rs` — `source list`/`diff`/`sync` handlers; env `TokenResolver`; debug-only env `Fetcher`; output; dry-run/`--yes` gating.

**Modified files:**

- `crates/skill-update/src/lib.rs` — `pub mod sources;`.
- `crates/core/src/skills/install_layout.rs` (or a new `skills/install_fetched.rs`) — `install_fetched_skill_and_lock` + types + the moved `copy_dir_recursive`/`get_skill_root` helpers.
- `crates/api/src/routes/sources.rs` — gutted to keyring `TokenResolver` + DTO mapping + `merged_baseline_for_source` call.
- `crates/api/src/routes/skills.rs` — git-install delegates to the core primitive.
- `crates/cli/src/commands/apply_update.rs` — extract `apply_skill_update_from_fetched`.
- `crates/cli/src/main.rs` — `Source` subcommand + early dispatch (before the `-a all` special-case at `main.rs:292`).
- `crates/cli/src/commands/mod.rs` — `pub mod source;`.
- `AGENTS.md`, `UPSTREAM.md` — docs.

---

## PHASE 1 — Extract `skill_update::sources`

### Task 1.1: Module skeleton + domain types (with `reason`)

**Files:** Create `crates/skill-update/src/sources.rs`; modify `crates/skill-update/src/lib.rs`.

- [ ] **Step 1: Write domain types + stubs**

```rust
//! Sources domain service. Extracted from `crates/api/src/routes/sources.rs`
//! so the API and the CLI share one implementation. Fetch + credentials are
//! injected via [`crate::Fetcher`] / [`crate::TokenResolver`].

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::{Fetcher, FetchError, SourceRef, TokenResolver};
use aghub_core::skills::update::UncheckableReason;

#[derive(Clone, Debug)]
pub enum SourceScope {
	Global,
	Project { root: PathBuf },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceScopeKind {
	Global,
	Project,
}

#[derive(Clone, Debug)]
pub struct SourceSummary {
	pub source: String,
	pub source_url: String,
	pub source_type: String,
	pub scope: SourceScopeKind,
	pub skill_count: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SourceSkillState {
	NotInstalled,
	InstalledCurrent,
	InstalledOutdated,
	Renamed,
	Removed,
	Deprecated,
	Uncheckable,
}

impl SourceSkillState {
	pub fn as_wire(&self) -> &'static str {
		match self {
			Self::NotInstalled => "notInstalled",
			Self::InstalledCurrent => "installedCurrent",
			Self::InstalledOutdated => "installedOutdated",
			Self::Renamed => "renamed",
			Self::Removed => "removed",
			Self::Deprecated => "deprecated",
			Self::Uncheckable => "uncheckable",
		}
	}
}

#[derive(Clone, Debug)]
pub struct SourceSkillDiff {
	pub name: String,
	pub skill_path: String,
	pub description: Option<String>,
	pub version: Option<String>,
	pub author: Option<String>,
	pub state: SourceSkillState,
	pub previous_name: Option<String>,
	/// Wire reason string (e.g. "noPath", "local"); preserves the DTO `reason`
	/// field and the removed→noPath / uncheckable→reason signals.
	pub reason: Option<String>,
	/// Scope labels where this skill is installed ("global"/"project").
	pub installed_paths: Vec<String>,
}

/// skill_path -> installed baseline metadata.
pub(crate) struct BaselineEntry {
	pub installed_name: String,
	pub stored_hash: String,
	pub local_hashes: Vec<String>,
	pub scope_label: String,
}
pub(crate) type Baseline = BTreeMap<String, BaselineEntry>;

#[derive(Debug)]
pub enum SourceDiffOutcome {
	/// Flat skill list (API-compatible: merged baseline, classified once).
	/// Carries the resolved `git_ref` (query override → recorded ref → None)
	/// so the API response keeps the old recorded-ref fallback.
	Ok {
		git_ref: Option<String>,
		skills: Vec<SourceSkillDiff>,
	},
	NeedsCredential,
	FetchFailed,
	/// Local/ssh/unsupported scheme — known before any fetch. Carries the
	/// resolved git_ref too (the old route returned it on the early-out).
	UncheckableSource {
		git_ref: Option<String>,
		reason: UncheckableReason,
	},
}

pub struct SourceListInput {
	pub scopes: Vec<SourceScope>,
}

pub struct SourceDiffInput {
	pub source: String,
	pub git_ref: Option<String>,
	pub scopes: Vec<SourceScope>,
}

pub struct SourceDiffDeps<'a> {
	pub fetcher: &'a dyn Fetcher,
	pub resolver: &'a dyn TokenResolver,
}

pub fn list_sources(_input: SourceListInput) -> Vec<SourceSummary> {
	todo!("Task 1.2")
}

pub fn fetch_source_with_resolver(
	_source_ref: &SourceRef,
	_fetcher: &dyn Fetcher,
	_resolver: &dyn TokenResolver,
) -> Result<crate::FetchedRepo, FetchError> {
	todo!("Task 1.4")
}

/// Internal: classify discovered repo skills against a prebuilt baseline.
/// `Baseline`/`BaselineEntry` stay `pub(crate)` so they never leak across the
/// crate boundary; cross-crate callers use [`classify_scope`] / [`diff_source`].
pub(crate) fn classify_repo_skills(
	_root: &Path,
	_baseline: &Baseline,
) -> Vec<SourceSkillDiff> {
	todo!("Task 1.3")
}

/// PUBLIC CLI entry: build the baseline for one scope and classify the fetched
/// repo against it. Does NOT fetch (caller passes the already-fetched `root`),
/// so the CLI reuses one `FetchedRepo` for every scope and for install.
pub fn classify_scope(
	_root: &Path,
	_scope: &SourceScope,
	_source: &str,
) -> Vec<SourceSkillDiff> {
	todo!("Task 1.4")
}

/// PUBLIC API entry: merged-baseline, single-classification, flat output —
/// byte-identical to the old route. Fetches internally via `deps`.
pub fn diff_source(
	_input: SourceDiffInput,
	_deps: SourceDiffDeps<'_>,
) -> SourceDiffOutcome {
	todo!("Task 1.4")
}
```

Add `pub mod sources;` to `crates/skill-update/src/lib.rs` after `mod git;` (lib.rs:14).

- [ ] **Step 2:** `cargo build --package skill-update` → compiles.
- [ ] **Step 3:** Commit `feat(skill-update): sources module skeleton + domain types`.

### Task 1.2: `list_sources` (per-scope, lock-only)

**Files:** modify `crates/skill-update/src/sources.rs`.

- [ ] **Step 1: Failing test**

```rust
#[cfg(test)]
mod list_tests {
	use super::*;
	#[test]
	fn list_sources_global_only_all_global_scope() {
		let out = list_sources(SourceListInput {
			scopes: vec![SourceScope::Global],
		});
		assert!(out.iter().all(|s| s.scope == SourceScopeKind::Global));
	}
}
```

Run: `cargo test --package skill-update list_sources_global_only_all_global_scope -- --exact` → FAIL (`todo!`).

- [ ] **Step 2: Implement** `list_sources` + `global_sources` + `project_sources` + `reconstruct_source_url`, ported from `sources.rs:61-120`, returning `SourceSummary` with `SourceScopeKind` (drop `is_private`/`credential_status` — they were always `false`/`NotRequired` and become DTO defaults in the API mapper). Code body identical to spec/old route except the return type.

- [ ] **Step 3:** Run → PASS. **Step 4:** Commit `feat(skill-update): list_sources (per-scope, lock-only)`.

### Task 1.3: Port pure classification (`classify_repo_skills` + helpers + baselines)

Move these from `crates/api/src/routes/sources.rs` **as copies** (do NOT delete from the route yet — deletion happens in Task 1.5 in one compiling commit): `source_matches` (:126), `lock_baseline_for_source` (split — see below), `local_hashes_for_installed` (:341), `build_source_skill_diffs` (:380), `skill_successors_from_changelog`/`skill_renames_in_line`/`backtick_spans` (:482-586), `is_deprecated_skill_path` (:588), `classify_source_skill_diff` (:592), `classify_installed` (:665), `parse_meta` (:716), `reason_str`.

**Pre-req verification (confirmed by review — do these, don't re-litigate):**

- `installed_skill_roots` is API-only (`sources.rs:25` imports it from `routes::skills_update`; a duplicate is at `skills.rs:564`). **Move it into `skill-update`** (it is a lock→disk resolver) and have both API routes import it from there.
- `RepoDiscoveredSkill: Clone` — **confirmed yes** (`skill/src/install.rs:11`), so `classify_repo_skills` may be called per scope on a cloned `Vec`.
- canonical `detect_rename` is `aghub_core::skills::update::detect_rename` (already imported in `skill-update` at `lib.rs:23`) — use it, not the API-local `skills::rename::detect_rename`.

**Files:** modify `crates/skill-update/src/sources.rs` (+ move `installed_skill_roots`).

- [ ] **Step 1: Implement `classify_repo_skills(root, baseline)`** = the body of `build_source_skill_diffs` (`sources.rs:380-480`), but constructing `SourceSkillState` variants instead of `String`, and setting `reason` for the `removed` row (`Some("noPath".into())`, matching `sources.rs:474`) and for `uncheckable` (`Some(reason_str(reason))`). Discovery (`skill::discover_repo_skills`) moves INTO this fn so callers pass only `root`.

- [ ] **Step 2: Implement the baseline builders** by splitting `lock_baseline_for_source` (`sources.rs:254-339`) into:
    - `merged_baseline_for_source(scopes: &[SourceScope], source: &str) -> (Baseline, String, Option<String>)` — **API path**: loops global then each project scope, inserting into ONE `Baseline` (project shadows global on duplicate `skill_path`), exactly as the old code. Returns `(baseline, source_type, recorded_ref)`.
    - `baseline_for_scope(scope: &SourceScope, source: &str) -> (Baseline, String, Option<String>)` — **CLI path**: one scope only.
      Both reuse a private `insert_scope_entries(&mut Baseline, …)` so the logic is DRY.

- [ ] **Step 3: Port classification unit tests** from `sources.rs` (~`:900-1356`) into `sources.rs`'s test module, rewriting assertions to `SourceSkillState` variants and `reason` (e.g. `assert_eq!(d.state, SourceSkillState::Removed); assert_eq!(d.reason.as_deref(), Some("noPath"));`). These use local temp repos, no network.

- [ ] **Step 4:** `cargo test --package skill-update sources` → all ported classification tests PASS.
- [ ] **Step 5:** Commit `refactor(skill-update): port sources classification + baselines (copy; route still owns its own)`.

### Task 1.4: `fetch_source_with_resolver` + `diff_source` (injected auth)

**Files:** modify `crates/skill-update/src/sources.rs`.

- [ ] **Step 1: Implement `fetch_source_with_resolver`** — the generic form of `fetch_source_lazily_auth` (`sources.rs:610-635`): try `fetcher.fetch(sr, None)`; on `Err`, resolve `token = resolver.resolve(&sr.source, keychain_host_for_source(&sr.source).as_deref())`; if `None` return the first error, else retry `fetcher.fetch(sr, Some(&token))`. Map nothing — return `Result<FetchedRepo, FetchError>` (caller maps Auth→NeedsCredential, Network→FetchFailed).

- [ ] **Step 2: Implement `diff_source`** (API convenience): trim source; `merged_baseline_for_source(&input.scopes, &source)` → `(baseline, source_type, recorded_ref)`; `source_type` default `"github"`; `let git_ref = input.git_ref.clone().or(recorded_ref);`; `if let Some(reason) = precheck_source(&source_type, &source) { return UncheckableSource { git_ref, reason } }`; build `SourceRef`; `match fetch_source_with_resolver(...) { Err(Auth)=>NeedsCredential, Err(Network)=>FetchFailed, Ok(repo)=> Ok { git_ref, skills: classify_repo_skills(repo.root.as_path(), &baseline) } }`.

- [ ] **Step 2b: Implement `classify_scope`** (CLI): `let (baseline, _src_type, _ref) = baseline_for_scope(scope, source); classify_repo_skills(root, &baseline)`. (No fetch — `root` is already fetched by the caller.)

- [ ] **Step 3: Failing test (fake Fetcher)** — `DirFetcher` returning a temp dir as `FetchedRepo`, `NoToken` resolver; build a source dir with `alpha/SKILL.md`; assert `diff_source(... scopes:[Global])` → `Ok { skills, .. }` containing `alpha` as `NotInstalled`. (Write the `DirFetcher`/`NoToken`/`write_skill` helpers as in the spec.)

Run: `cargo test --package skill-update diff_source_reports_not_installed -- --exact` → was FAIL (todo), now PASS after Steps 1-2.

- [ ] **Step 4:** Commit `feat(skill-update): fetch_source_with_resolver + diff_source (injected auth)`.

### Task 1.5: Delegate the API route + delete moved code (ONE compiling commit)

**Files:** modify `crates/api/src/routes/sources.rs` (and the `installed_skill_roots` import site in `skills_update.rs`).

- [ ] **Step 1: Add a keyring `TokenResolver`** in the route:

```rust
struct KeyringResolver;
impl skill_update::TokenResolver for KeyringResolver {
	fn resolve(&self, source: &str, _host: Option<&str>) -> Option<String> {
		token_for_source(source) // existing, sources.rs:653
	}
}
```

- [ ] **Step 2: Rewrite `list_sources` route** → build `scopes` from `ResolvedScope`, call `skill_update::sources::list_sources`, map each `SourceSummary` → `SourceSummaryResponse` (set `is_private:false`, `credential_status:NotRequired`).

- [ ] **Step 3: Rewrite `diff_source` route** → resolve scope to `Vec<SourceScope>`; `spawn_blocking` calling `skill_update::sources::diff_source(SourceDiffInput{source, git_ref, scopes}, SourceDiffDeps{ fetcher:&GitFetcher, resolver:&KeyringResolver })`; map `SourceDiffOutcome`: `Ok{git_ref, skills}`→`SourceDiffResponse{ skills: skills.map(map_diff_to_dto), git_ref, session_id:None, needs_credential:false }`; `NeedsCredential`→`needs_credential:true, skills:[], git_ref:<the query git_ref>`; `FetchFailed`→`BadGateway SOURCE_FETCH_FAILED`; `UncheckableSource{git_ref,..}`→`SourceDiffResponse{ git_ref, skills:[], needs_credential:false, session_id:None }` (matches the old `precheck_source` early return at `sources.rs:201` which returned the resolved `git_ref`). `map_diff_to_dto` copies `name/skill_path/description/version/author/previous_name/reason/installed_paths` and `state: d.state.as_wire().to_string()`.

- [ ] **Step 4: Delete** all now-duplicated private fns from the route (`global_sources`, `project_sources`, `lock_baseline_for_source`, `build_source_skill_diffs`, `classify_*`, `fetch_source_lazily_auth`, `diff_blocking`, `skill_successors_from_changelog`, `is_deprecated_skill_path`, `parse_meta`, etc.) **and** the classification `#[test]`s that called them (they now live in `skill-update`). Keep `token_for_source`, `reconstruct_source_url` only if still used by the DTO mapper. Update `skills_update.rs` to re-export/import `installed_skill_roots` from `skill_update`.

- [ ] **Step 5:** `cargo test --package aghub-api sources` → remaining route-level tests (HTTP shape) PASS. Then `just preflight` → PASS. **This single commit moves the route to delegation with everything compiling.**

- [ ] **Step 6:** Commit `refactor(api): delegate Sources to skill_update::sources; remove duplicated logic`.

---

## PHASE 2 — `install_fetched_skill_and_lock` in `aghub-core`

### Task 2.1: Types + moved copy helpers + signature

**Files:** create/modify `crates/core/src/skills/install_fetched.rs`; modify `crates/core/src/skills/mod.rs`.

- [ ] **Step 1: Move the recursive copy** `copy_dir_recursive` + `get_skill_root` + `sanitize_name` semantics from `crates/api/src/routes/skills.rs` into `aghub-core` (the API will call the core versions). **The moved `copy_dir_recursive` must return `std::io::Result<()>` (or `Result<(), ConfigError>`), NOT `ApiError`** — `aghub-core` cannot depend on Rocket. The API caller maps the error to `ApiError` at its boundary. Preserve the traversal/exclusion behavior exactly (decision #5).

- [ ] **Step 2: Add types + stub**

```rust
use std::path::Path;
use crate::models::ResourceScope;
use aghub_agents::models::AgentType;

#[derive(Clone, Copy, Debug)]
pub enum SkillInstallLayout {
	IsolatedCopy,
	Universal,
}

pub struct AgentInstallResult {
	pub agent: AgentType,
	pub installed: bool,
	pub error: Option<String>,
}

pub struct FetchedSkillInstallRequest<'a> {
	pub skill_file: &'a Path,            // SKILL.md inside the fetched tree
	pub source: &'a skill::InstallLockSource,
	pub lock_skill_path: String,         // npx-form "<dir>/SKILL.md"
	pub ref_commit: Option<String>,
	pub scope: ResourceScope,            // GlobalOnly | ProjectOnly
	pub project_root: Option<&'a Path>,
	pub target_agents: &'a [AgentType],
	pub layout: SkillInstallLayout,
	pub expected_name: Option<&'a str>,  // rename guard vs frontmatter
	pub use_relative_links: bool,        // universal: relative (project) vs absolute (global)
}

pub struct FetchedSkillInstallReport {
	pub name: String,
	pub wrote_lock: bool,
	pub installed_hash: String,
	pub agent_results: Vec<AgentInstallResult>,
}

pub fn install_fetched_skill_and_lock(
	_req: FetchedSkillInstallRequest<'_>,
) -> Result<FetchedSkillInstallReport, crate::ConfigError> {
	todo!("Tasks 2.2 / 2.3")
}
```

**Confirmed shapes (don't re-verify):** `skill::InstallLockSource` = `{source, source_type, source_url, ref_name}` (`install.rs:18`). `write_global_install_lock(skill_name, source, skill_path, source_dir, ref_commit)` (`install.rs:140`). `write_project_install_lock` additionally needs `cwd`/project root (`install.rs:168`). The lock writers compute the hash from `source_dir` themselves — pass the **fetched source dir**, do NOT pass a pre-computed hash.

- [ ] **Step 3:** `cargo build --package aghub-core` → compiles. **Step 4:** Commit `feat(core): install_fetched types + moved copy helpers`.

### Task 2.2: Isolated-copy install + per-agent results + global lock (TDD)

**Files:** modify `crates/core/src/skills/install_fetched.rs`; test `crates/core/tests/sources_install_tests.rs`.

- [ ] **Step 1: Failing test** — build a temp fetched skill dir (`alpha/SKILL.md`); `set_skills_path_override("claude", tmp_agent_dir)` (AGENTS.md Testing); call the primitive with `IsolatedCopy`, `GlobalOnly`, `target_agents:[Claude]`; assert (a) `<agent dir>/alpha/SKILL.md` exists, (b) `agent_results` has one `installed:true` for Claude, (c) the global lock read-back has `alpha` with `entry.source == req.source.source` and a non-placeholder hash. Run `-- --exact` → FAIL.

- [ ] **Step 2: Implement** the isolated branch: `parse(skill_file)`; rename guard (`expected_name` vs frontmatter name → `ConfigError` with the shared rename message from `update.rs:38`); `source_root = get_skill_root(skill_file)`; `safe_name = sanitize_name(&parsed.name)`. For each `target_agents` resolve dir via `create_adapter(agent).target_skills_dir(project_root, scope)` (mirror `skills.rs:642`) — `None` → `AgentInstallResult{installed:false, error:Some("no skills dir")}`. **Preserve the API's no-clobber semantics (`skills.rs:666`):** `let dest = dir.join(&safe_name); if !dest.exists() { copy_dir_recursive(&source_root, &dest)?; installed:true } else { installed:false }` (an existing dir is a success-no-op, NOT an overwrite). Compute `wrote_lock` with the same `should_write_install_lock` rule the API uses (`skills.rs:903` — only write the lock when at least one agent actually got the skill / per existing semantics); when writing, `write_global_install_lock(name, source, lock_skill_path, source_root, ref_commit)`. `installed_hash = compute_skill_folder_hash(source_root)?`.

- [ ] **Step 3:** Run → PASS. **Step 4:** Commit `feat(core): isolated-copy fetched install + per-agent results + global lock`.

### Task 2.3: Universal layout + project-scope lock + rename-guard test (TDD)

- [ ] **Step 1: Failing tests:** (a) `Universal` writes the master into `universal_canonical_dir` (`install_layout.rs:33`) and links target agent dirs (assert canonical `SKILL.md` exists + an agent dir symlink resolves to it); (b) `ProjectOnly` writes the project lock via `write_project_install_lock`; (c) rename guard: `expected_name:Some("alpha")` vs a `beta` frontmatter → `Err`, nothing written.
- [ ] **Step 2: Implement** the `Universal` branch mirroring `install_git_skill_universal` (`skills.rs:683-716`). **`universal_canonical_dir` takes `Option<&Path>` only** (`install_layout.rs:33`), so compute `let canonical_root = if matches!(scope, ResourceScope::ProjectOnly) { project_root } else { None }; let canonical_skills_dir = universal_canonical_dir(canonical_root); let canonical = canonical_skills_dir.join(&safe_name);` then `symlink_dirs = target dirs whose path != canonical_skills_dir`; `install_universal(&source_root, &canonical, &symlink_dirs, use_relative_links)`. Add the `ProjectOnly` lock-write branch (`write_project_install_lock`, which also needs the project root / cwd, `install.rs:168`).
- [ ] **Step 3:** Run all three → PASS. **Step 4:** Commit `feat(core): universal + project-scope fetched install + rename guard`.

### Task 2.4: API git-install delegates (behavior preserved)

- [ ] **Step 1:** In `crates/api/src/routes/skills.rs`, keep `build_git_install_groups`/agent grouping and per-skill source normalization (`resolve_remote_source` → `install_lock_source_from_resolved`, `skills.rs:811`); replace the inner install+lock body (`skills.rs:2034-2148`) with a call to `install_fetched_skill_and_lock`, mapping `FetchedSkillInstallReport.agent_results` back into the existing API response. Delete `install_git_skill_to_dir`/`install_git_skill_universal` if fully subsumed.
- [ ] **Step 2:** `cargo test --package aghub-api skills` → existing git-install tests PASS unchanged.
- [ ] **Step 3:** `just preflight` → PASS. **Step 4:** Commit `refactor(api): git-install delegates to core install_fetched_skill_and_lock`.

---

## PHASE 3 — CLI `source` subcommand

### Task 3.0: Extract `apply_skill_update_from_fetched` (no second fetch)

**Files:** modify `crates/cli/src/commands/apply_update.rs`.

- [ ] **Step 1:** Extract the post-fetch body of `execute` (`apply_update.rs:44-end`: `sanitize_skill_path` → `ensure_source_not_renamed` → containment assert → `stage_and_swap_dir` loop → **lock update**) into:

```rust
pub fn apply_skill_update_from_fetched(
	repo_root: &std::path::Path,
	skill_path: &str,         // npx-form "<dir>/SKILL.md"
	name: &str,
	scope: ResourceScope,
	project_root: Option<&std::path::Path>,
	ref_commit: Option<&str>, // FetchedRepo.oid, for the lock refCommit heal
) -> anyhow::Result<Vec<std::path::PathBuf>> { /* moved body */ }
```

**Critical (P1-6):** the existing `execute` updates the lock's content hash + `refCommit` AFTER the swap (`apply_update.rs:82`). The extracted fn MUST do that same lock update internally (recompute `updated_hash` from `source_dir`, write it + `ref_commit` back into the scope's lock) — not just return paths. Have `execute` call it after its own `fetch_source`, passing `fetched.oid` (or the existing recorded ref commit) as `ref_commit`. This keeps `apply-update` byte-identical and gives `source sync --update` a fetch-free, lock-correct entry point.

- [ ] **Step 2:** `cargo test --package aghub-cli apply` (existing apply-update tests) → PASS. **Step 3:** Commit `refactor(cli): extract apply_skill_update_from_fetched`.

### Task 3.1: `Source` subcommand enum + early dispatch

**Files:** modify `crates/cli/src/main.rs`, `crates/cli/src/commands/mod.rs`; create `crates/cli/src/commands/source.rs`.

- [ ] **Step 1: Add the enum** (note the `--ref` alias for decision-spec parity):

```rust
Source {
	#[command(subcommand)]
	action: SourceAction,
},
```

```rust
#[derive(clap::Subcommand)]
pub enum SourceAction {
	List { #[arg(long)] json: bool },
	Diff {
		source: String,
		#[arg(long = "ref", alias = "git-ref")]
		git_ref: Option<String>,
		#[arg(long)] json: bool,
	},
	Sync {
		source: String,
		#[arg(long = "ref", alias = "git-ref")]
		git_ref: Option<String>,
		#[arg(long)] update: bool,
		#[arg(long)] install_missing: bool,
		#[arg(long)] universal: bool,
		#[arg(long)] yes: bool,
		#[arg(long)] json: bool,
	},
}
```

- [ ] **Step 2: Early dispatch BEFORE the `-a all` special-case** (`main.rs:292`, `handle_all_agents`). Immediately after CLI parse / global-flag setup and before any `handle_all_agents`/adapter/manager work:

```rust
if let Commands::Source { action } = &cli.command {
	return commands::source::execute(
		action, cli.global, cli.project, cli.all, &cli.agent,
	);
}
```

(`source` resolves its own scope + builds managers internally only for `sync` writes. It does not take `-a all`; the agent for installs comes from `-a <agent>`, passed as `&cli.agent`.)

- [ ] **Step 3:** `pub mod source;` in `commands/mod.rs`; stub `pub fn execute(action: &crate::SourceAction, global: bool, project: bool, all: bool, agent: &str) -> anyhow::Result<()> { todo!() }`. Make `SourceAction` reachable (re-export from `main.rs` or define in `commands::source` and import into `main.rs`). The `sync` arm parses `agent` into `AgentType` (via the same path as the top-level `-a`) for `target_agents`.
- [ ] **Step 4:** `cargo build --package aghub-cli` → compiles. **Step 5:** Commit `feat(cli): source subcommand scaffold + early dispatch`.

### Task 3.2: `source list` (TDD, fully isolated env)

**Files:** modify `crates/cli/src/commands/source.rs`; test `crates/cli/tests/cli_tests.rs`.

- [ ] **Step 1: Failing test** — reuse the `isolated_cli`-style helper (`cli_tests.rs:204`) that sets `HOME`/`USERPROFILE`/`APPDATA`/`XDG_STATE_HOME` to a temp dir, then:

```rust
#[test]
fn source_list_runs_with_no_agent_config() {
	let env = isolated_cli(); // sets HOME/USERPROFILE/APPDATA/XDG_STATE_HOME
	env.cmd().args(["source", "list"]).assert().success();
}
```

Run `-- --exact` → FAIL (`todo!`).

- [ ] **Step 2: Implement the `List` arm** — scopes: always `Global`; if not `-g` and `find_project_root(current_dir())` is `Some(root)`, add `Project{root}`; `-p` → project only; `-g` → global only. Call `list_sources`; render a `tabled` table `SOURCE | SCOPE | SKILLS | URL`; `--json` prints `serde_json` of a serializable view.
- [ ] **Step 3:** Run → PASS. **Step 4:** Commit `feat(cli): source list`.

### Task 3.3: `source diff` (TDD, debug env fetcher + token resolver)

- [ ] **Step 1: Add the CLI fetch + token plumbing** in `commands/source.rs`:

```rust
struct EnvTokenResolver;
impl skill_update::TokenResolver for EnvTokenResolver {
	fn resolve(&self, _s: &str, _h: Option<&str>) -> Option<String> {
		std::env::var("GIT_PASSWORD")
			.or_else(|_| std::env::var("GITHUB_TOKEN"))
			.ok()
	}
}

/// Production fetch is `skill_update::GitFetcher`. Under debug builds only, an
/// env hook lets `assert_cmd` e2e tests point at a local dir (no network).
struct CliFetcher;
impl skill_update::Fetcher for CliFetcher {
	fn fetch(
		&self,
		sr: &skill_update::SourceRef,
		token: Option<&str>,
	) -> Result<skill_update::FetchedRepo, skill_update::FetchError> {
		#[cfg(debug_assertions)]
		if let Some(root) = std::env::var_os("AGHUB_TEST_SOURCE_FETCH_ROOT") {
			let root = std::path::PathBuf::from(root);
			return if root.is_dir() {
				Ok(skill_update::FetchedRepo {
					root,
					oid: "test-fetch-root".into(),
					_guard: None,
				})
			} else {
				Err(skill_update::FetchError::Network)
			};
		}
		skill_update::GitFetcher.fetch(sr, token)
	}
}
```

- [ ] **Step 2: Failing e2e test** — `isolated_cli()` + `.env("AGHUB_TEST_SOURCE_FETCH_ROOT", source_dir)` where `source_dir` has `alpha/SKILL.md`; assert `source diff owner/repo` succeeds and output contains `alpha` + `notInstalled`. Run → FAIL.
- [ ] **Step 3: Implement the `Diff` arm** — resolve scopes (as `list`); `precheck_source` early-out; fetch ONCE via `fetch_source_with_resolver(&SourceRef{source, ref_}, &CliFetcher, &EnvTokenResolver)`; for each scope call the public `skill_update::sources::classify_scope(repo.root.as_path(), &scope, &source)` → render per-scope rows `STATE | NAME | SKILL_PATH | SCOPE`. (Use `classify_scope`, NOT the `pub(crate)` `baseline_for_scope`/`classify_repo_skills`, which are not visible cross-crate.) Map errors: `Auth`→needs-credential message ("set GIT_PASSWORD / GITHUB_TOKEN, or bind a credential in the desktop app"), `Network`→fetch-failed message, `precheck` Some→uncheckable message. `--json` prints structured per-scope diff.
- [ ] **Step 4:** Run → PASS. **Step 5:** Commit `feat(cli): source diff (per-scope, env credentials, debug fetch hook)`.

### Task 3.4: `source sync` (dry-run default; --update / --install-missing; TDD)

- [ ] **Step 1: Failing e2e tests (three):**
    1. `source sync owner/repo --install-missing -g` (no `--yes`) → exit success, prints a plan, the target agent dir stays empty AND no global lock entry is created.
    2. `source sync owner/repo --install-missing --yes -g -a claude` → installs `alpha`; a follow-up `source list` shows the source.
    3. `source sync owner/repo --install-missing --yes -g -a claude` against a source containing `deprecated/foo/SKILL.md` → `foo` (state `deprecated`) is skipped.
       All use `isolated_cli()` + `AGHUB_TEST_SOURCE_FETCH_ROOT`.

- [ ] **Step 2: Implement the `Sync` arm:**
    - Require exactly one writing scope when `--yes` (`-g` or `-p`; error on `--all`/none).
    - Fetch ONCE (`fetch_source_with_resolver`); classify via the public `classify_scope(repo.root, &scope, &source)`.
    - Action set: `--install-missing` → `state == NotInstalled` (exclude `Deprecated`/`Renamed`/`Removed`); `--update` → `state == InstalledOutdated`. Neither flag → print plan + message "pass --update and/or --install-missing"; return.
    - Dry-run (no `--yes`): print the plan; return.
    - Parse the target agent once: `let target = AgentType::from_str(agent);` (same parse the top-level `-a` uses); `let target_agents = [target];`.
    - `--yes` install-missing: for each row, `sanitize_skill_path(repo.root, &row.skill_path)` → SKILL.md; build `InstallLockSource` via `aghub_git::resolve_remote_source(&source)` (+ `install_lock_source_from_resolved` semantics: source/source_type/source_url/ref_name); call `aghub_core::skills::install_fetched::install_fetched_skill_and_lock` with `target_agents: &target_agents`, `layout` from `--universal`, `scope`, `project_root`, `expected_name:Some(&row.name)`, `ref_commit:Some(repo.oid.clone())`, `use_relative_links: scope==ProjectOnly`.
    - `--yes --update`: for each `InstalledOutdated` row, call `commands::apply_update::apply_skill_update_from_fetched(&repo.root, &row.skill_path, &row.name, scope, project_root, Some(&repo.oid))` — reusing the SAME fetched repo (no second fetch) and passing the fetched commit for the lock `refCommit` heal.
    - Aggregate + print results; `--json` prints structured outcome.

- [ ] **Step 3:** Run the three tests → PASS. **Step 4:** Commit `feat(cli): source sync (dry-run default; --update/--install-missing)`.

### Task 3.5: `--json` stability + help

- [ ] **Step 1:** Test that `source list --json` / `source diff --json` emit valid JSON whose `state` values are the `as_wire()` strings.
- [ ] **Step 2:** `aghub-cli source --help` / `source sync --help` render without panic, descriptions present.
- [ ] **Step 3:** Commit `test(cli): source --json + help`.

---

## PHASE 4 — Docs

### Task 4.1: AGENTS.md + UPSTREAM.md

- [ ] **Step 1:** Add to `AGENTS.md` "CLI Command Surface":

```
  source list                         # installed sources (current project + global)
  source diff <source> [--ref R]      # read-only per-skill state vs installed
  source sync <source> [--ref R]      # --update (outdated) / --install-missing (notInstalled);
                                      #   dry-run default, --yes to apply; -a <agent>, --universal
```

- [ ] **Step 2:** Note in `UPSTREAM.md` that Sources list/diff + git-install logic moved into `skill_update::sources` / `aghub_core::skills::install_fetched` with **no behavior change** (round-trip lock contract preserved).
- [ ] **Step 3:** `just preflight` → PASS. **Commit** `docs: document CLI source subcommand`.

---

## Self-Review

**Spec coverage:** list/diff/sync → 3.2/3.3/3.4 ✅; extract to skill-update → Phase 1 ✅; install primitive writes lock → Phase 2 ✅; per-scope CLI / merged API → decisions #1-2 + Tasks 1.3/3.3 ✅; state alignment (skip deprecated, refuse removed/renamed) → 3.4 ✅; env credentials + needsCredential message → decision #6 + 3.3 ✅; dry-run default → 3.4 ✅; list/diff lock-only no config → 3.1/3.2 ✅; API behavior preserved → 1.5/2.4 (existing tests unchanged) ✅; fetch once → decision #2 + 3.4 ✅.

**Adversarial findings resolved (round 1):** P0-1 (compiling commits) → Task 1.3 copies / 1.5 deletes-in-one-commit. P0-2 (byte-identical) → decision #1, merged baseline kept for API. P0-3 (`reason`) → `SourceSkillDiff.reason` in 1.1, set in 1.3. P0-4 (fetch once) → fetch/classify split, decision #2 + 3.4. P0-5 (per-agent report) → `agent_results` in 2.1. P0-6 (copy semantics) → decision #5, move `copy_dir_recursive` in 2.1. P1s: `--ref` alias (3.1), full env isolation (3.2), debug fetch hook (3.3), token-in-GIT_PASSWORD (3.3 + decision #6), InstallLockSource via resolve_remote_source (3.4), apply-update extraction (3.0), dispatch before `-a all` (3.1).

**Adversarial findings resolved (round 2 → this rev):**

- `execute` now receives `&cli.agent` (dispatch + signature, Task 3.1).
- `Baseline`/`BaselineEntry` stay `pub(crate)`; cross-crate callers use the public `classify_scope` / `diff_source` (Task 1.1 + 1.4; diff/sync arms updated to call `classify_scope`).
- `SourceDiffOutcome::Ok { git_ref, skills }` + `UncheckableSource { git_ref, reason }` preserve the API's recorded-ref fallback in the response (Task 1.1 + 1.5 mapping).
- Isolated install preserves the no-clobber `if !dest.exists()` + `should_write_install_lock` semantics (Task 2.2).
- `universal_canonical_dir(Option<&Path>)` — real signature; compute `canonical_root` from scope (Task 2.3).
- Moved `copy_dir_recursive` returns `io::Result`/`ConfigError`, not `ApiError` (Task 2.1).
- `apply_skill_update_from_fetched` updates the lock (hash + `refCommit`) internally, not just returns paths (Task 3.0, P1-6).

**Type consistency:** `SourceSkillState`/`as_wire` (1.1) used in 1.3/1.5/3.3/3.5. `Baseline`/`BaselineEntry` (1.1) built by `merged_baseline_for_source`/`baseline_for_scope` (1.3), consumed by `classify_repo_skills` (1.3) in 1.5/3.3/3.4. `FetchedSkillInstallRequest` fields (2.1) consumed verbatim in 2.4/3.4. `fetch_source_with_resolver`/`classify_repo_skills` (1.4/1.3) called in 3.3/3.4. `apply_skill_update_from_fetched` (3.0) called in 3.4. ✅
