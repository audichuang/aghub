# Deepen "Resync an installed skill" + converge safety guards (scope-A)

**Status**: planned (2026-06-24) · **Driver**: architecture review (deepening candidate #1)

## Problem

"Re-sync an installed skill from a freshly-fetched source" is orchestrated **three
times**, each re-doing the same sequence (discover install targets → rename guard →
hash → containment assert → `stage_and_swap_dir` → lock-hash write):

- CLI `apply_skill_update_from_fetched` — `crates/cli/src/commands/apply_update.rs:78`
  (used by `aghub apply-update` and `source sync --update`)
- API `apply_skill_update_inner` — `crates/api/src/routes/skills_update.rs:525`
- API `git_sync_skill` — `crates/api/src/routes/skills.rs:2158` (session/temp-dir, interactive desktop flow)

The low-level swap primitive `stage_and_swap_dir` (`crates/core/src/skills/update.rs:151`,
"replaces target and skips symlinks") is already shared. What is **not** shared is the
destructive-transaction *caller knowledge* around it, and it has drifted:

| Concern    | CLI                          | API apply-update             | API git-sync                         |
| ---------- | ---------------------------- | ---------------------------- | ------------------------------------ |
| Rename guard | shared `detect_rename`     | shared `detect_rename`       | raw `parsed.name != req.name` (bypass) |
| Containment  | `assert_targets_contained` (lenient) | `assert_targets_contained` (lenient) | `assert_targets_strictly_contained` (strict) |
| refCommit    | passes OID                 | passes OID                   | passes `None` (lazy — OID is readable) |

Plus two helpers are duplicated across surfaces:
- `installed_skill_roots` — canonical in `crates/skill-update/src/lib.rs:58`, but private
  copies in `crates/cli/src/commands/apply_update.rs:253` and `crates/api/src/routes/skills.rs:566`.
- `update_lock_hash` — copies in `crates/cli/src/commands/apply_update.rs:275` and
  `crates/api/src/routes/skills_update.rs:356`.

## Decisions

- **Home = `aghub-core`.** Resync is destructive filesystem mutation over primitives core
  already owns (`stage_and_swap_dir`, removal containment, `detect_rename`, `load_all_agents`,
  lock r/w via `crates/skill`). `skill-update` is the network/credential update-**check**
  crate and depends on core — it must not host disk mutation.
- **Seam after fetch/session/sanitize.** The core function takes an already-resolved skill
  dir (matching the existing `install_fetched_skill_and_lock` convention). Routes/CLI keep
  fetch, token, session, confirm, and request validation; `sanitize_skill_path` stays the
  shared core helper each caller invokes.
- **Converge divergences on the safe side**: strict containment everywhere (lenient accepts
  `target == root`, which would replace the whole skills root); shared `detect_rename`
  everywhere (git-sync's raw compare is the outlier — semantically equal today, but bypasses
  the stable rename message/code contract); `ref_commit` stays an `Option` and each caller
  passes `Some(oid)` when it has one.
- **Multi-target swap: attempt all, but any failure aborts before the lock advances.**
  Adversarial review (Codex) caught that advancing the lock on a *partial* swap is unsound:
  the update-check drops differing per-agent hashes as ambiguous and falls back to the lock,
  so a failed target would silently read as up-to-date. Universal installs resolve to a single
  canonical Master target (N=1); N>1 only for isolated-copy installs. The deep module reports
  the failed targets in the `Swap` error and leaves the lock untouched so a re-sync is forced.
- **No `plan_actions` module.** The CLI's state→action policy is two filters with no hidden
  depth (`crates/cli/src/commands/source.rs:502`). Revisit only if eligibility grows a third
  rule (e.g. `Renamed → accept-rename`) AND the desktop's TS eligibility would otherwise
  drift — then expose `allowed_actions_for_state` via the diff DTO.

## Work (sequenced, independently shippable, test-gated)

### PR 1 — Converge safety guards (highest value / lowest risk; no new API)
- `git_sync_skill`: replace raw rename compare with shared `detect_rename` path (stable
  `SKILL_RENAMED` code + message).
- CLI `apply_skill_update_from_fetched` and API `apply_skill_update_inner`:
  `assert_targets_contained` → `assert_targets_strictly_contained`.
- `git_sync_skill`: read the session temp repo's HEAD OID (like `git_install`,
  `crates/api/src/routes/skills.rs:2055`) and pass `Some(oid)` — **iff** the session repo has
  a readable HEAD; else keep `None`.
- Tests: renamed-upstream refused identically on all three paths; strict containment rejects a
  root-target; git-sync writes `refCommit` when available.

### PR 2 — Single home for `installed_skill_roots`
- Move into `aghub-core`, co-located with the root-resolution family in `removal.rs`
  (`agent_skill_dirs_in_scope`, `allowed_skill_roots`). Delete the CLI + API private copies;
  `skill-update`'s version calls the core one.

### PR 3 — Single home for `update_lock_hash`
- Move the lock-update orchestration into core (entry methods `apply_content_hash` /
  `apply_computed_hash` already live in `crates/skill`). Delete the CLI + API copies.

### PR 4 — Deep module `core::skills::resync::resync_installed_skill`
```rust
pub struct ResyncRequest<'a> {
    pub source_dir: &'a Path,       // already-sanitized fetched skill dir
    pub name: &'a str,              // locked name (rename-guard target)
    pub scope: ResourceScope,
    pub project_root: Option<&'a Path>,
    pub ref_commit: Option<&'a str>,
}
pub struct ResyncReport {
    pub swapped: Vec<PathBuf>,
    pub updated_hash: String,
}
pub enum ResyncError {
    NotInstalled, Renamed { new_name: String }, OutOfTree(PathBuf),
    Parse(String), Hash(String), LockUpdate(String),
    InvalidScope, MissingProjectRoot, Io(std::io::Error),
}
pub fn resync_installed_skill(req: ResyncRequest) -> Result<ResyncReport, ResyncError>;
```
- Sequence: `installed_skill_roots` (PR2, empty→NotInstalled) → parse +
  `detect_rename` → `compute_skill_folder_hash` →
  `assert_targets_strictly_contained` (PR1) → swap every target (any failure
  aborts before the lock advances) → `update_…_lock_hash` (PR3).
- Refactor the three call sites to thin wrappers (CLI maps to anyhow; API maps `ResyncError`
  → ApiError codes; routes keep fetch/session/sanitize).
- Tests at the interface (the test surface): not-installed, renamed-refused, out-of-tree,
  happy-path, universal Master swap + symlink skip, partial-failure best-effort report.
  Delete the now-redundant per-surface duplicated tests.

### PR 5 — Do not implement `plan_actions`
- Record the trigger condition (above) and stop.

## Deletion test
- Delete PR4's `resync_installed_skill` → destructive-transaction caller knowledge returns to
  three sites (passes — earns its keep).
- Delete PR2/PR3 homes → two private copies regrow (passes).
- PR1 is not a module; it tightens existing call sites toward the safe predicate.

## Review

Adversarially reviewed by Codex (GPT-5.5). Outcome: A/B keep-with-reshape, C keep, D cut.
Codex corrections folded in: seam after sanitize (match `install_fetched_skill_and_lock`);
`ResyncError` widened beyond the original four; `refCommit None` is lazy not policy (OID is
readable); home is core not skill-update. Open item Codex did not raise, decided here:
multi-target transactionality → best-effort per ADR-0001.
