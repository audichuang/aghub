# CLI Sources management (`aghub-cli source <list|diff|sync>`)

**Status:** planned (design approved; Codex-reviewed in two rounds)
**Date:** 2026-06-19
**Related:** `docs/specs/2026-06-02-sources-and-universal-install.md` (the
original Sources feature — API + desktop only)

## Motivation

The "Sources" feature lets users who install skills from git repos (à la
`npx skills`) see **which sources they've installed from** and, per source,
**which of that source's skills are not yet installed / are outdated / were
removed upstream**, then install or update accordingly.

Today this lives **only in the desktop app + HTTP API**. Terminal users have
no source-centric view: to know "what repos did I install from" or "what's new
in this repo" they must open the GUI. This spec brings the same capability to
the CLI, scoped naturally to **the current project + global**.

## Scope of this work

In: a new top-level `source` subcommand group (`list` / `diff` / `sync`), the
extraction of the Sources domain logic out of the API route into a shared
crate so CLI and API consume one implementation, and a no-network install
primitive in `aghub-core`.

Out (explicit non-goals): cross-machine "all projects everywhere" aggregation
(the CLI has no project store — see below); a keyring credential UI in the CLI;
a TUI/interactive picker (deferred — `sync` uses flags + dry-run instead).

## Key constraint — the CLI has no project store

The desktop Sources page aggregates across **every** project the user has
opened, driven by the **frontend** store (`useProjects` + `useQueries` looping
each project). The CLI has no such store. Its world is **the current project
(detected by walking up for an agent marker via
`aghub_core::paths::find_project_root`) + global**. This is acceptable and in
fact matches the user's intent ("see this project + manage global"), but it is
**not** equivalent to the desktop "all projects" view. The default for
`source list` is therefore "current project (if detected) + global", each row
tagged with its scope.

## Design decisions (approved)

| #   | Decision                                                                                                                                                                                                                                                                                                                                                                                    |
| --- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Q1  | New top-level subcommand group `aghub-cli source <list\|diff\|sync>`, separate from the resource-centric `get/add/check/apply-update`. Mirrors the desktop "Sources" page as its own axis (source-centric, not resource-centric).                                                                                                                                                           |
| Q2  | `diff` is read-only. `sync <source>` carries one-shot flags `--update` (update all `installedOutdated`) and `--install-missing` (install all `notInstalled`). Follows the existing `delete`/`prune-lock` convention: **default dry-run, `--yes` to actually write**. Target agent(s) for install reuse the existing `-a <agent>` flag; layout reuses `--universal` (default isolated copy). |
| Q3  | `source list` with no scope flag defaults to **current project + global** (each row tagged). `-g`/`-p` narrow it.                                                                                                                                                                                                                                                                           |

## Architecture

### The problem found in review

The original assumption — "the API routes are thin wrappers, the CLI just
reuses a core function" — is **false**. `crates/api/src/routes/sources.rs` is
~1426 lines and computes the entire diff-state classification
(`notInstalled` / `installedCurrent` / `installedOutdated` / `renamed` /
`removed` / `deprecated` / `uncheckable`) **inline in the route**
(`sources.rs:399-472`, `:680-704`). There is no core/shared function to reuse.
A naive CLI would either re-implement this in parallel or bypass the existing
rename-guard / atomic-swap / install-lock safety paths.

So this work has a **prerequisite extraction phase**: lift the Sources domain
logic into a shared layer that both API and CLI consume.

### Layer 1 — Sources domain service in `skill-update`

`skill-update` is the existing shared network/git update orchestrator. Its docs
state that `crates/core` stays pure (no network, no keyring) while fetch +
credential resolution live in `skill-update`, and it already injects
`Fetcher` / `TokenResolver` traits and depends on `aghub-core`, `aghub-git`,
`skill`, `tokio`, `tempfile`, `gix`. It is the correct host: it can fetch
without dragging Rocket/keyring into the CLI, and reusing its existing trait
boundaries means API supplies a keyring resolver while CLI supplies an env
(`GIT_USERNAME`/`GIT_PASSWORD`) resolver.

`aghub-core` is the **wrong** host for the fetch-backed diff because its update
module is explicitly pure/no-network (`crates/core/src/skills/update.rs:1`). A
brand-new crate is unnecessary unless Sources later expands beyond skills.

New module `skill_update::sources` (domain types, **not** API DTOs):

```rust
pub enum SourceScope {
	Global,
	Project { root: PathBuf },
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
	pub fetcher: &'a dyn Fetcher,       // already in skill-update
	pub resolver: &'a dyn TokenResolver, // already in skill-update
}

pub fn list_sources(input: SourceListInput) -> Vec<SourceSummary>;

/// Blocking; the caller (Rocket) wraps it in spawn_blocking. The CLI calls it
/// directly.
pub fn diff_source(
	input: SourceDiffInput,
	deps: SourceDiffDeps<'_>,
) -> SourceDiffOutcome;

pub struct SourceSummary {
	pub source: String,
	pub source_url: String,
	pub source_type: String,
	pub scope: SourceScopeKind,
	pub skill_count: u32,
}

pub enum SourceDiffOutcome {
	Ok(SourceDiff),
	NeedsCredential,
	FetchFailed(FetchError),
	Uncheckable(UncheckableReason),
}

pub struct SourceDiff {
	pub source: String,
	pub git_ref: Option<String>,
	pub scopes: Vec<ScopedSourceDiff>, // PER-SCOPE, never merged
}

pub struct ScopedSourceDiff {
	pub scope: SourceScopeKind,
	pub skills: Vec<SourceSkillDiff>,
}

pub enum SourceSkillState {
	NotInstalled,
	InstalledCurrent,
	InstalledOutdated,
	Renamed,
	Removed,
	Deprecated,
	Uncheckable(UncheckableReason),
}
```

**Per-scope, never merged.** The current API baseline is keyed only by
`skillPath` (`sources.rs:160`) and inserts both global and project entries into
the _same_ map (`sources.rs:277`, `:308`), so a skill present in both scopes
silently overwrites. The new service takes multiple scopes but returns one
`ScopedSourceDiff` per scope. This is the fix for the `--all` collision found
in review.

**What stays in the API route after extraction:** `ScopeParams` parsing
(`crates/api/src/extractors.rs:31`), the Rocket `spawn_blocking` wrapper, the
keyring `TokenResolver` impl (`sources.rs:653`), DTO mapping to
`SourceDiffResponse` / `SourceSkillDiff` (`crates/api/src/dto/sources.rs`),
HTTP status mapping, and path redaction. The route no longer owns grouping or
diff classification.

### Layer 2 — No-network install primitive in `aghub-core`

`source sync --install-missing` must install a skill from an already-fetched
source **and** write the source/hash/ref into the install lock, exactly as the
API git-install path does. If it only reused `add --from`, the install lock
would not record the source and the skill would vanish from `source list`
afterwards (and break npx round-trip compatibility).

The pieces already exist but are not callable as one unit from the CLI:

- Lock writes are already public in `skill`:
  `skill::write_global_install_lock` (`crates/skill/src/install.rs:140`) and
  `write_project_install_lock` (`:168`) — both record
  source/sourceUrl/ref/skillPath/contentHash/refCommit.
- The layout primitive `aghub_core::skills::install_layout::install_universal`
  (`crates/core/src/skills/install_layout.rs:74`) and `universal_canonical_dir`
  (`:33`) are public.
- But install + lock-write are combined only **inside the API route**
  (`install_git_skill_to_dir` `skills.rs:651`, `install_git_skill_universal`
  `:683`, orchestrated by `git_install_skills` `:1979-2148`).

Extract a no-network primitive into `aghub-core` (it needs adapter/target
resolution and layout rules, but **not** Rocket/keyring/fetch):

```rust
pub enum SkillInstallLayout {
	IsolatedCopy,
	Universal,
}

pub struct FetchedSkillInstallRequest<'a> {
	pub skill_file: &'a Path,            // SKILL.md resolved under fetched root
	pub source: &'a skill::InstallLockSource,
	pub lock_skill_path: String,         // "<dir>/SKILL.md"
	pub ref_commit: Option<String>,
	pub scope: ResourceScope,            // GlobalOnly | ProjectOnly
	pub project_root: Option<&'a Path>,
	pub target_agents: &'a [AgentType],
	pub layout: SkillInstallLayout,
	pub expected_name: Option<&'a str>,  // guard: diff-row name vs frontmatter
}

pub struct FetchedSkillInstallReport {
	pub name: String,
	pub wrote_lock: bool,
	pub agent_results: Vec<AgentInstallResult>,
	pub installed_hash: String,
}

pub fn install_fetched_skill_and_lock(
	req: FetchedSkillInstallRequest<'_>,
) -> Result<FetchedSkillInstallReport, ConfigError>;
```

Behavior: parse `skill_file`; if `expected_name` differs from the frontmatter
name, fail with the shared rename message (`crates/core/src/skills/update.rs:38`,
`:48`). Resolve target dirs via the adapter (equivalent to the current API
`skills.rs:642` helper). Isolated copy → copy the full skill folder into each
target dir. Universal → `universal_canonical_dir` + link selected agent dirs
(matching `skills.rs:2039-2070`). Then call the appropriate
`skill::write_*_install_lock`.

### `--update` reuses the apply-update path, not the install primitive

`source sync --update` operates on **already-installed** skills, so it must use
the atomic-swap update path (`stage_and_swap_dir`,
`crates/core/src/skills/update.rs:137`) with its rename guard — **not** the
install-missing primitive (which must never overwrite an existing dir). In
practice `--update` reuses the same logic as the existing `apply-update`
command (`crates/cli/src/commands/apply_update.rs`), driven by the diff's
`installedOutdated` rows.

### Fetch model — CLI does not replay API sessions

The API scan/install flow caches a `TempDir`, credential token, and branches in
`GitCloneSession` (`crates/api/src/state.rs:7`), keyed by `session_id`. The
Sources diff already returns `session_id: None` (`sources.rs:205`). The CLI must
**not** depend on that session machinery. It fetches once with
`skill_update::GitFetcher`, keeps the `FetchedRepo` in-process, and reads
`FetchedRepo.root` / `FetchedRepo.oid` (`crates/skill-update/src/lib.rs:147`) to
supply the install lock's `ref_commit`. So `sync` fetches the repo once and
drives both diff classification and install/update from that single in-process
`FetchedRepo`.

## State handling (aligned with desktop)

The desktop installs only `notInstalled`, updates only `installedOutdated`,
treats `renamed`/`removed` as cleanup, and shows but never installs
`deprecated`. The CLI mirrors this:

| State               | `--install-missing` |       `--update`        | Notes                                                                            |
| ------------------- | :-----------------: | :---------------------: | -------------------------------------------------------------------------------- |
| `notInstalled`      |     ✅ install      |            —            |                                                                                  |
| `installedCurrent`  |          —          |            —            | nothing to do                                                                    |
| `installedOutdated` |          —          | ✅ update (atomic swap) |                                                                                  |
| `deprecated`        |       ❌ skip       |            —            | reported, never auto-installed                                                   |
| `renamed`           |         ❌          |           ❌            | reported only; rename guard refuses overwrite. Suggest delete-old + install-new. |
| `removed`           |          —          |        ❌ refuse        | upstream removed it; reported only                                               |
| `uncheckable`       |          —          |            —            | reported with reason (ssh / local / unsupported scheme)                          |

## Credential handling

The CLI has no GUI credential binding. It uses the existing CLI credential
model — `GIT_USERNAME` / `GIT_PASSWORD` env (as `check --online` /
`apply-update` already do) — via an env-backed `TokenResolver`. When
`diff_source` returns `NeedsCredential`, the CLI prints a source-level message
("set GIT_USERNAME / GIT_PASSWORD, or bind a credential in the desktop app")
rather than faking a diff. A future `--credential-id` that reads the keyring is
possible **only** if the keyring resolver is first lifted out of the API into a
shared layer — out of scope here.

## Command surface

```text
aghub-cli source list [-g | -p | --all] [--json]
# default (no scope flag): --all = global + current project (if detected)

aghub-cli source diff <source-or-url> [-g | -p | --all] [--ref <ref>] [--json]
# read-only; rows include scope; --all runs SEPARATE per-scope diffs

aghub-cli source sync <source-or-url> (-g | -p) [--update] [--install-missing]
                      [-a <agent>] [--universal] [--yes] [--json]
# default dry-run (prints the plan); --yes writes.
# --update        -> only installedOutdated
# --install-missing -> only notInstalled (excludes deprecated)
# require an explicit single scope for a writing sync (no --all execution)
```

Notes:

- `list` / `diff` are read-only and have **no** dry-run flag (dry-run on a
  read-only op is meaningless).
- `sync` with neither `--update` nor `--install-missing` prints the plan and
  exits asking the user to choose an action (no implicit "do everything").
- `<source-or-url>` accepts the lock's stored `source` string or a re-fetch URL
  (resolved the same way the API does).

## CLI plumbing — `source list`/`diff` must be lock-only

The CLI today builds and loads a default agent config up front; only
Add/Check/PruneLock tolerate a missing config (`crates/cli/src/main.rs:341`,
`:354`). `source list` / `source diff` are lock-only / fetch-only and must
**not** fail just because (e.g.) a Claude config is absent. They must branch
**before** the manager is loaded (add them to the tolerate-missing set). Only
`sync --install-missing` / `--update` need a manager and `-a` target.

## File map (planned)

**`crates/skill-update`** (new shared layer)

- `src/sources.rs` — `list_sources`, `diff_source`, domain types, diff-state
  classification lifted from the API route.
- reuse existing `Fetcher` / `TokenResolver` / `GitFetcher`.

**`crates/core`**

- `src/skills/install_layout.rs` (or a sibling) — new
  `install_fetched_skill_and_lock` + `FetchedSkillInstallRequest` / `Report`.

**`crates/api`**

- `src/routes/sources.rs` — gutted to: scope parsing, `spawn_blocking`,
  keyring `TokenResolver`, DTO mapping, status/redaction. Calls
  `skill_update::sources`. Behavior must stay byte-identical for existing
  endpoints (existing route tests are the regression net).

**`crates/cli`**

- `src/main.rs` — `Source` subcommand enum + the lock-only tolerate-missing
  branch.
- `src/commands/source.rs` — `list` / `diff` / `sync` handlers; env-backed
  `TokenResolver`; dry-run/`--yes` gating; output (table + `--json`).
- `src/commands/mod.rs` — wire the module.

**Docs**

- `AGENTS.md` CLI command surface — add the `source` group.
- `UPSTREAM.md` — note the extraction if it touches ported upstream code.

## Testing

- `skill_update::sources` unit tests: each diff state
  (`notInstalled`/`current`/`outdated`/`renamed`/`removed`/`deprecated`/
  `uncheckable`) with an injected fake `Fetcher`, and the **per-scope, no-merge**
  invariant (same skill in global + project does not collide). Port the existing
  classification assertions from `sources.rs` tests.
- `aghub-core` install primitive: isolated-copy and universal layouts both write
  the correct install lock; rename guard rejects a frontmatter-name mismatch;
  never overwrites an existing dir.
- CLI (`crates/cli/tests/cli_tests.rs` via `assert_cmd`): `source list` with no
  config present (tolerate-missing); `sync` default dry-run writes nothing;
  `--yes` writes and the skill then shows in `source list`;
  `--install-missing` skips `deprecated`; `removed` refuses update.
- API regression: existing `sources.rs` route tests must still pass unchanged
  after the extraction.

## Implementation order

1. Extract `skill_update::sources` (list + diff + classification); make the API
   route delegate to it; keep API tests green. **No CLI yet.**
2. Extract `install_fetched_skill_and_lock` into `aghub-core`; make the API git
   install path delegate to it; keep API tests green.
3. Add the CLI `source` subcommand group consuming both, with env credential
   resolver, dry-run/`--yes`, and `--json`.
4. Docs (`AGENTS.md`, `UPSTREAM.md`).

Steps 1 and 2 are independent and could land as separate PRs before 3.

## Known follow-ups / out of scope

- Keyring-backed credentials in the CLI (`--credential-id`) — needs the keyring
  resolver lifted out of the API first.
- A `source cleanup` subcommand for `renamed`/`removed` (delete-old/relink) —
  reported here, not actioned.
- Cross-machine "all projects" aggregation — the CLI is current-project + global
  by design.
