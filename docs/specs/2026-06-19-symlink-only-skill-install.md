# Symlink-Only Skill Install + Standalone Linker + Agent Auto-Detection

**Date**: 2026-06-19
**Status**: Design — implementation-ready
**Scope crates**: `aghub-core`, `aghub-api`, `aghub-cli`, `crates/desktop`, plus tests in `aghub-core`/`aghub-skill`
**Related**: `docs/specs/2026-06-02-sources-and-universal-install.md`, `docs/specs/2026-06-19-cli-sources-management.md`, `CONTEXT.md` (Master/Referrer/Relink), skills `aghub-skills` / `npx-skills-contract` / `upstream-skills-flow` / `testing-fs-failures`

---

## Goal & Locked Decisions

Converge the entire app onto **one** skill-install model: **symlink-only**. A single `.agents/skills/<name>` Master plus per-agent links. Port Skills-Manager's MIT junction primitives so Windows installs survive without admin/Developer-Mode, delete every copy path, and fix the agent auto-detection blind spot so the install/import UI stops asking the user to pick agents that are already covered by the Master or not installed.

**Locked decisions (do not relitigate):**

1. **ONE model — symlink-only.** A single `.agents/skills/<name>` Master + per-agent links. **NO copy as a user choice. NO copy fallback.**
2. **Forward-only.** Existing copy-installs on disk are **not** retroactively converted. The change is to the _creation_ path and the _detection_ paths, never a migration sweep.
3. **Link mechanics.** Unix = `symlink`. Windows = native `symlink_dir` **first**, then directory junction via `cmd /C mklink /J` (no admin). If **both** fail → **HARD ERROR** (no copy). Mirrors SM `create_windows_symlink`.
4. **No new crate dependency.** The junction is created via `std::process::Command` (`cmd /C mklink /J`), exactly as SM does.
5. **Port SM's MIT primitives with attribution:** `is_symlink_or_junction` (reparse-point `0x0400` detection), `remove_symlink_or_junction` (`remove_dir` then `remove_file`), `create_windows_symlink` (symlink_dir → mklink /J → hard error), `normalize_path`. **Do NOT port** SM's "iflow copy mode."
6. **Junctions need ABSOLUTE targets.** aghub uses relative links at project scope, absolute at global. The native `symlink_dir` attempt keeps the requested (possibly relative) target; the junction fallback **must** resolve to an absolute target even when a relative link was requested.
7. **npx round-trip stays intact.** v3 global + v1 project lock writes and the byte-identical folder hash are untouched. The Master is still materialized via the existing exclusion-list `copy_dir_recursive` — that is _Master materialization_, **not** a per-agent copy.
8. **The blind spot is fixed.** Agents whose own skills dir IS or already READS `.agents/skills` (per `AGENTS.md`: Codex/OpenCode/Cursor/Cline/Warp at global; a larger set at project) see the Master directly and need **no link**. The new design **auto-classifies** agents (reads-`.agents`-natively ⇒ no link; otherwise ⇒ needs link) and combines with CLI availability so the UI never asks the user to select an already-covered or not-installed agent.

---

## The Model

```
                          INSTALL  (one path, symlink-only)
                          ─────────────────────────────────
   source skill tree
        │
        │  copy_dir_recursive  (npx-identical exclusions:
        │   metadata.json / .git / __pycache__ / __pypackages__,
        │   dereference symlinks)  ── Master materialization ONLY
        ▼
   .agents/skills/<name>   ◄────────── the single Master (project: <root>/.agents/skills,
        ▲   ▲   ▲                                          global:  ~/.agents/skills)
        │   │   │
        │   │   │  per-agent links (Linker::link):
        │   │   │    Unix  -> symlink (relative @project / absolute @global)
        │   │   │    Win   -> symlink_dir, else `cmd /C mklink /J <ABS master>`
        │   │   │    both fail -> HARD ERROR (NO copy)
        │   │   │
   ┌────┴─┐ ┌┴────┐ ┌┴──────┐
   │.claude│ │.zed │ │.windsurf│ ...  NeedsLink agents (private skills dir)
   │/skills│ │/... │ │  ...    │
   └───────┘ └─────┘ └─────────┘

   Codex / OpenCode / Cursor / Cline / Warp (global) and a larger set (project):
        their resolved skill_read_paths ALREADY CONTAIN .agents/skills
        => NativeReader  => NO LINK  => "auto-covered" in the UI
```

Three buckets per `(agent, scope)`, derived purely from the descriptor's resolved read/write paths:

| Bucket                          | Condition                                                                | Install action                  | UI                               |
| ------------------------------- | ------------------------------------------------------------------------ | ------------------------------- | -------------------------------- |
| **NativeReader / auto-covered** | resolved `skill_read_paths` (or write dir) contains the canonical Master | reported installed, **no link** | read-only "already covered" chip |
| **NeedsLink**                   | has a write dir ≠ Master, does **not** read Master                       | `Linker::link`                  | selectable checkbox              |
| **Unsupported**                 | no write dir for this scope                                              | soft-fail row                   | hidden / muted                   |

---

## The Standalone Linker Module

### Location & name — MODULE under core, NOT a new crate

`crates/core/src/skills/linker/` — a directory module (`mod.rs` + `classify.rs`). It **replaces** the existing `crates/core/src/skills/install_layout.rs`.

Rationale (weighed against `Cargo.toml` `[workspace].members` and the `AGENTS.md` "Structure" dependency direction `agents → core → cli/api/desktop`):

1. The std-only link primitives are tiny and have **one** downstream consumer cluster — every caller already lives in or depends on `aghub-core` (manager/skill.rs, install_fetched.rs, removal.rs, api routes via core, CLI via core). A new crate would be used laterally by nothing, unlike the existing tool crates (`skill`, `git`, `json`, …) which each justify themselves by a distinct external dependency or reuse surface.
2. The "auto-classify which agents need a link" half is **not** std-only — it reads `AgentDescriptor` capabilities/paths. Splitting "mechanics crate" from "classify in core" would fragment one purpose across two homes. Keeping both in `crates/core/src/skills/linker/` puts the whole linker concern in one navigable place at the right layer.
3. It replaces an existing in-crate module, so the change is a mechanical in-crate move (`crate::skills::install_layout::*` → `crate::skills::linker::*`) — no `Cargo.toml` edit, no new `[workspace].members` line.

> **Future promotion**: if the mechanics later need to be reused by a lateral crate (e.g. `cc-plugins` junction-linking plugins), promote the **mechanical half** (`Linker` + `LinkTarget` + `LinkOutcome` + `LinkError`, no descriptor deps) to `crates/skill-link`. The API below is shaped so that promotion is a file move, `classify.rs` stays in core.

### File layout

```
crates/core/src/skills/linker/
├── mod.rs        # mechanical core (std only): Linker { link, is_link, unlink },
│                 #   LinkTarget, LinkOutcome, LinkError, create_link (win: symlink_dir
│                 #   -> mklink /J -> Err), normalize_path, relative_path,
│                 #   universal_canonical_dir, install_universal, link_agents_to_canonical,
│                 #   UniversalInstallReport, EXCLUDE_*/copy_dir_recursive (Master only)
└── classify.rs   # agent auto-classification (depends on aghub-agents descriptors):
                  #   LinkNeed, AgentLinkPlan, classify_agent, classify_all
```

`crates/core/src/skills/mod.rs`: change `pub mod install_layout;` → `pub mod linker;`. Update the ~9 call sites in the same commit (recommended; see Open Questions for the optional `#[deprecated] pub use linker as install_layout;` shim).

### Public API — mechanical core (`linker/mod.rs`, std only)

```rust
/// Whether a created link's stored target is relative (project scope, portable)
/// or absolute (global scope). Windows junctions ALWAYS resolve to absolute even
/// when Relative is requested (junctions cannot store a relative target).
pub enum LinkTarget { Relative, Absolute }

/// Outcome of a single link attempt against one agent dir.
pub enum LinkOutcome {
    Linked,        // fresh link created (unix symlink / win symlink / win junction)
    AlreadyLinked, // a correct link to the same Master already existed (idempotent)
    Conflict,      // foreign symlink/junction OR a real file/dir occupies the slot — NEVER clobbered
}

pub enum LinkError {
    /// BOTH native symlink AND `cmd /C mklink /J` failed on Windows (or symlink
    /// unsupported on a non-unix/non-windows platform). HARD ERROR — NO copy.
    LinkUnsupported { target: PathBuf, link: PathBuf, source: io::Error },
    Io(io::Error),
}

pub struct Linker; // zero-sized, stateless

impl Linker {
    /// Create `agent_dir/<skill_name>` -> `master_dir` (the `.agents/skills/<name>`
    /// canonical dir, which MUST already exist). Creates `agent_dir` if absent.
    /// lstat-inspects the occupant WITHOUT following it: returns AlreadyLinked /
    /// Conflict without writing on collision. On a clean target: Unix => symlink;
    /// Windows => symlink_dir, else `cmd /C mklink /J <ABSOLUTE master>`; both fail
    /// => LinkError::LinkUnsupported.
    pub fn link(
        master_dir: &Path,
        agent_dir: &Path,
        skill_name: &str,
        target: LinkTarget,
    ) -> Result<LinkOutcome, LinkError>;

    /// lstat-based reparse-point detection: true for a Unix symlink OR a Windows
    /// symlink/junction (FILE_ATTRIBUTE_REPARSE_POINT 0x0400). Never follows.
    /// Ported from SM `is_symlink_or_junction`.
    pub fn is_link(path: &Path) -> bool;

    /// Remove a link without touching its target: `remove_dir` then `remove_file`
    /// (a Windows junction is a dir reparse point; a Unix symlink-to-dir needs
    /// remove_file). Idempotent on a missing path. Ported from SM
    /// `remove_symlink_or_junction`.
    pub fn unlink(path: &Path) -> io::Result<()>;
}

/// `.agents/skills` store for a scope. project_root.is_some() => <root>/.agents/skills;
/// None => ~/.agents/skills. Moves verbatim from install_layout.rs.
pub fn universal_canonical_dir(project_root: Option<&Path>) -> Option<PathBuf>;
```

### Convenience layer (thin over `Linker::link`, kept in `linker/mod.rs`)

```rust
pub struct UniversalInstallReport {
    pub canonical: PathBuf,
    pub linked: Vec<PathBuf>,
    pub already_linked: Vec<PathBuf>,
    pub conflicts: Vec<PathBuf>,
    // NOTE: `copied_fallback` is REMOVED — the converged model bans copy.
}

/// Materialize the Master from source (npx-identical copy + exclusions) if
/// absent, then link each agent dir. Returns LinkError on a hard link failure
/// instead of silently copying.
pub fn install_universal(
    source_root: &Path,
    canonical: &Path,
    agent_skills_dirs: &[PathBuf],
    target: LinkTarget,
) -> Result<UniversalInstallReport, LinkError>;

/// Link agent dirs to an already-materialized Master (add_skill / rename relink).
pub fn link_agents_to_canonical(
    canonical: &Path,
    agent_skills_dirs: &[PathBuf],
    target: LinkTarget,
) -> Result<UniversalInstallReport, LinkError>;
```

> The current convenience fns take `use_relative_links: bool`. Migrate callers to `if project_scope { LinkTarget::Relative } else { LinkTarget::Absolute }`. `install_universal` / `link_agents_to_canonical` may keep a `bool` internally if it shortens the diff, but the public boundary uses `LinkTarget`.

### Public API — classification (`linker/classify.rs`, depends on aghub-agents)

This is the **blind-spot fix**.

```rust
pub enum LinkNeed {
    /// Agent's own skills dir at this scope IS or already READS .agents/skills
    /// (descriptor-driven): sees the Master directly, NO link required.
    NativeReader,
    /// Agent has a private skills dir not mapped to the Master: needs a link.
    NeedsLink { agent_dir: PathBuf },
    /// Agent's skills dir cannot be resolved for this scope.
    Unsupported,
}

pub struct AgentLinkPlan {
    pub agent_id: &'static str,
    pub need: LinkNeed,
    pub installed: bool, // from availability::check_agent_availability
}

/// Classify ONE agent against a scope + project_root + an already-resolved
/// canonical Master dir. NativeReader iff `master_dir` appears in the agent's
/// `skill_read_paths(project_root, scope)` (covers Codex/OpenCode/Cursor/Cline/
/// Warp @global, the broader project set, and the XDG `universal:true` agents
/// Amp/Kimi at their XDG path), OR the agent's `skill_write_path == master_dir`.
pub fn classify_agent(
    descriptor: &AgentDescriptor,
    scope: ResourceScope,
    project_root: Option<&Path>,
    master_dir: &Path,
) -> AgentLinkPlan;

/// Classify ALL registered agents (registry::iter_all / ALL_AGENTS). Callers
/// filter to `installed && matches!(need, NeedsLink{..})` for the link set, and
/// surface NativeReader / Unsupported / not-installed as "already covered" /
/// "skipped" rather than asking the user.
pub fn classify_all(
    scope: ResourceScope,
    project_root: Option<&Path>,
    master_dir: &Path,
) -> Vec<AgentLinkPlan>;
```

**Classifier algorithm** (reuse existing primitives — never a hardcoded agent list, `AGENTS.md` warns those drift):

```text
let adapter   = create_adapter(agent);
let canonical = universal_canonical_dir(if scope==ProjectOnly { project_root } else { None });
let read_paths = adapter.get_skills_paths(project_root, scope);   // = skill_read_paths
let write_dir  = adapter.target_skills_dir(project_root, scope);  // = skill_write_path
let canon = canonical.map(canonicalize_lenient);                  // unwrap_or(path)
let reads_master  = canon matches any canonicalize_lenient(read_path);
let writes_master = Some(canon) == write_dir.map(canonicalize_lenient);
NativeReader  if reads_master || writes_master
NeedsLink     if write_dir.is_some() && !reads_master && !writes_master
Unsupported   otherwise
```

`canonicalize_lenient(p) = std::fs::canonicalize(p).unwrap_or(p.to_path_buf())` so it works before the dir exists (same trick `link_one` uses at `install_layout.rs:149`). **Canonicalize BOTH sides** to defeat the macOS `/var`→`/private` prefix mismatch (per MEMORY release-test-gate note).

### Dependencies

- `linker/mod.rs`: **std only** — `std::path`, `std::io`, `std::fs`, `std::os::unix::fs::symlink` / `std::os::windows::fs::symlink_dir`, `std::process::Command` for `cmd /C mklink /J`, plus `std::os::windows::fs::MetadataExt` (`file_attributes`) for the reparse bit, and `std::os::windows::process::CommandExt` (`creation_flags`). `dirs` (already a workspace dep) only for `universal_canonical_dir`'s global home.
- `linker/classify.rs`: `aghub-agents` (`AgentDescriptor`, `Capabilities`, `ResourceScope`) + `crate::availability` + `crate::registry`. No new deps.
- `LinkError`: use the workspace `thiserror` for `From<io::Error>` consistency (optional but recommended).

### Isolation rationale (module boundary, not crate boundary)

The mechanical core takes only paths + a `LinkTarget` enum and is descriptor-free; `classify.rs` takes descriptors. Both test in isolation (the existing `#[cfg(all(test, unix))] mod tests` already proves the mechanics test with `tempdir`). The mechanical/classify split is what keeps a future crate-promotion a clean file move.

---

## Porting Skills-Manager

Source: **jiweiyeah/Skills-Manager**, `src-tauri/src/services/linker.rs`, MIT-licensed. We port only the four cross-platform link primitives. Everything else in SM (iflow copy-mode, lock-status enums, hub import) stays behind.

### Function-by-function map (SM → aghub)

| SM symbol (linker.rs)                  | aghub destination                                                           | Notes                                                                                                                                                                                                                 |
| -------------------------------------- | --------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `normalize_path` (10-18)               | `linker/mod.rs` private/`pub(crate)` helper                                 | Verbatim. `/`→`\` on Windows (`MAIN_SEPARATOR=='\\'`), no-op on Unix. Feeds `cmd.exe` native separators. No other aghub caller.                                                                                       |
| `is_symlink_or_junction` (101-118)     | `Linker::is_link`                                                           | Verbatim: `symlink_metadata().file_type().is_symlink()` first; on Windows `meta.file_attributes() & 0x0400 (FILE_ATTRIBUTE_REPARSE_POINT) != 0` via `MetadataExt`. **The lynchpin** (see "Why this is load-bearing"). |
| `remove_symlink_or_junction` (120-136) | `Linker::unlink`                                                            | Verbatim: Windows `remove_dir(p).or_else(                                                                                                                                                                             | \_  | remove_file(p))`; Unix `remove_file(p)`. Uses `remove_dir`**not**`remove_dir_all`— only unlinks the reparse point, never recurses into the Master. Tolerate`NotFound`. |
| `create_windows_symlink` (217-275)     | private `create_link(requested_target, abs_target, link)` (the Windows arm) | Adapted: see below. Replaces the current `create_dir_symlink` (`install_layout.rs:258-274`).                                                                                                                          |

### `create_link` control flow (replaces `install_layout.rs:258-274`)

```text
create_link(requested_target, abs_target, link):
  // requested_target may be RELATIVE (project) or absolute (global).
  // abs_target is ALWAYS the absolute canonical Master path.

  #[cfg(unix)]:
      std::os::unix::fs::symlink(requested_target, link)   // honors relative/abs
      -> Ok | Err   // HARD ERROR, no copy

  #[cfg(windows)]:
      if std::os::windows::fs::symlink_dir(requested_target, link).is_ok(): Ok   // native (Dev Mode/admin)
      let out = Command::new("cmd")
                  .args(["/C", "mklink", "/J"])
                  .arg(normalize_path(link))
                  .arg(normalize_path(abs_target))            // junction needs ABSOLUTE
                  .creation_flags(0x08000000)                 // CREATE_NO_WINDOW
                  .output();
      if out.status.success(): Ok
      Err(LinkError::LinkUnsupported { .. })                  // BOTH failed -> HARD ERROR, NO copy

  #[cfg(not(any(unix, windows)))]:
      Err(LinkError::LinkUnsupported { .. })
```

**Adaptations vs SM:**

- Signature unified cross-platform `(requested_target, abs_target, link)` so callers don't branch per-OS.
- **Drop SM's pre-clean step** (SM:227-235 removes an existing occupant). aghub's `Linker::link` already lstat-inspects the occupant and only reaches `create_link` when the slot is `NotFound`; pre-cleaning would let us clobber a foreign occupant, violating the no-clobber invariant (`install_layout.rs:358` `never_clobbers...` test). **Create-only.**
- **Junction target MUST be absolute.** The native attempt uses `requested_target` (possibly relative, for portability); the `mklink /J` fallback uses `abs_target`. Never pass a `..\..` relative string to `mklink /J` — it silently produces a broken junction.
- **Drop SM's GBK best-effort decoding** (SM:259-272 — it was a no-op that already used `from_utf8_lossy`). Keep plain `String::from_utf8_lossy` on stderr/stdout for the error message, and the informative `mklink /J {link} {target}` error format.
- Keep `creation_flags(0x08000000)` (CREATE_NO_WINDOW).
- **Hard error on both-fail.** No copy.

### How `install_layout.rs` collapses into `linker/`

| Current `install_layout.rs` symbol (line)                         | Fate                                                                                                                                                                                                                                    |
| ----------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `universal_canonical_dir` (33)                                    | Verbatim → `linker/mod.rs`.                                                                                                                                                                                                             |
| `UniversalInstallReport` (43-58)                                  | Kept, **minus `copied_fallback`** (51-53).                                                                                                                                                                                              |
| `enum LinkResult` (59-64)                                         | Replaced by public `LinkOutcome`; **drop `CopiedFallback`** (62).                                                                                                                                                                       |
| `install_universal` (74)                                          | Kept as convenience fn; delegates each link to `Linker::link`, returns `LinkError` (no copy fallback).                                                                                                                                  |
| `link_agents_to_canonical` (93)                                   | Kept; loop over `Linker::link`; drop the `CopiedFallback` arm (128-130).                                                                                                                                                                |
| `link_one` (138)                                                  | Becomes the body of `Linker::link`. lstat occupant inspection (146-164) reused as-is; `use_relative` branch (168-172) → `LinkTarget`; the `create_dir_symlink` + copy-fallback (174-182) **REPLACED** by `create_link` with HARD ERROR. |
| `relative_path` (188)                                             | Private helper in `linker/mod.rs`. Feeds the `Relative` arm; the `abs_target` for the junction fallback is just the absolute `canonical` directly.                                                                                      |
| `EXCLUDE_FILES` / `EXCLUDE_DIRS` / `copy_dir_recursive` (217-256) | **Move verbatim.** Still materializes the Master byte-identically to npx (round-trip contract). Add a doc note: _"this copy materializes the single Master only; it is NOT a per-agent copy fallback."_                                 |
| `create_dir_symlink` cfg-arms (258-274)                           | **Rewritten** into `create_link` (above).                                                                                                                                                                                               |
| `#[cfg(all(test, unix))] mod tests` (276-518)                     | Moves; restructured per Test Strategy; `copied_fallback`/copy assertions deleted; `is_link`/`unlink`/hard-error/junction tests added.                                                                                                   |

### Why `is_symlink_or_junction` is load-bearing (the latent Windows bug it fixes)

aghub today uses bare `meta.file_type().is_symlink()` in spots that an `mklink /J` junction would silently fail (a junction reports `is_dir()==true`, `is_symlink()==false`):

- `install_layout.rs:148` (`link_one` already-linked-vs-conflict): a junction we created last run would be seen as a **foreign real dir** → `Conflict`, breaking idempotency on every Windows re-install.
- `removal.rs:430` (`execute_removal`): a junction falls through to the `ft.is_dir()` branch → `remove_dir_all` **recurses into the shared `.agents` Master** = data loss. Must become `if Linker::is_link(path) { Linker::unlink(path) }` placed **before** the `is_dir()` branch.
- `manager/skill.rs` `universal_relink_referrers` (~718) and `universal_relink_agents` (~740), and `rollback_master_rename` (~864): the "is this entry a symlink pointing at the old Master" probe — a Windows junction referrer would be skipped on rename, orphaning it. Swap all to `Linker::is_link`.

### Explicitly DROP (do not port)

- SM iflow copy-mode: `IFLOW_TOOL_ID`, `tool_uses_copy_mode`, `enable_skill_for_tool`/`disable_skill_for_tool`, `check_link_for_tool`, `check_link_for_scoped_skill`.
- `CopyModeMetadata` + `.skills-manager-source.json` + all `copy_mode_*` helpers (SM:28-75).
- SM `copy_dir_all` / `copy_dir_all_include_hidden` / `copy_dir_all_with_options` (SM:443-489) — aghub keeps its own npx-contract `copy_dir_recursive`.
- SM `LinkStatus`/`LinkResult`/`LinkReport`/`check_link`/`sync_all_for_tool`/`import_to_hub` — aghub has its own report types + rename/relink transaction.

---

## Call-site Rewiring & Copy Removal

The good news: `crates/core/src/skills/install_fetched.rs::install_universal_layout` (311-402) ALREADY implements the symlink-shaped path and ALREADY half-classifies natives via `dir == canonical_skills_dir` (350). The blind spot is that the **live API/CLI routes do not take this path** — they call duplicate copy-based helpers. Most of this work is deleting copy branches and re-pointing live callers at the one shared primitive, then refining the primitive to also skip `reads_master` (not just `writes_master`) natives.

### Core — `install_fetched.rs`

- **Delete** `SkillInstallLayout::IsolatedCopy` (56-58), `install_isolated` (262-309), the local `copy_dir_recursive` (37-51), and the `match req.layout` dispatch (208-224). `install_fetched_skill_and_lock` ALWAYS uses `install_universal_layout`. Remove the `layout` field from `FetchedSkillInstallRequest` (88) — full removal over a one-variant enum.
- **Refine** `install_universal_layout` (347-360) to exclude not just `writes_master` but `reads_master`, via `classify::classify_agent`: `NativeReader` → `AgentInstallResult{installed:true, error:None}` (no link); `NeedsLink` → collect `agent_dir` into the symlink set; `Unsupported` → soft-fail. Then call `install_universal` (now copy-free).
- Lock-write signal unchanged: `copied_any`/`wrote_master` stays `true` when the Master was freshly written; auto-covered agents count as installed. v3/v1 lock writing stays in `write_install_lock`.

### Core — `manager/skill.rs`

- `add_skill_from_path` (526-588): stop copying into the agent's own dir. Make it a thin wrapper → `self.add_skill_from_path_universal(path)` (597-657, the reference impl that already excludes the native via `Some(d) if d != canonical_dir => vec![d], _ => []` at 634-639, preserves no-clobber, keeps the duplicate-name guard 619-621). Delete the old copy body (567-584).
- `add_skill` manual-create (122-151) → delegate to `add_skill_universal` (165-219) so manual creation also writes a Master + link (needed for "one model"; see Open Questions if manual-create is meant to stay agent-local).
- Rename relink helpers (`universal_relink_referrers` ~706-725, `universal_relink_agents` ~731-761, `rollback_master_rename` ~848-886): swap `is_symlink()` → `Linker::is_link`. Order matters — the rename caller unlinks stale links first (`Linker::unlink`) before re-linking, so the idempotency `canonicalize` does not misfire on a moved Master.

### Core — `removal.rs`

Replace `execute_removal`'s symlink branch (`removal.rs:429-433`):

```rust
if Linker::is_link(path) {            // was: ft.is_symlink()
    match Linker::unlink(path) {       // was: remove_file(path)  (remove_dir-then-remove_file)
        Ok(()) => report.removed.push(path.clone()),
        Err(e) => report.failed.push((path.clone(), e)),
    }
} else if ft.is_dir() {
    // unchanged: assert_contained + remove_dir_all  (containment re-check stays here)
}
```

**Security**: `Linker::unlink` is only the symlink/junction-unlink half. The `assert_contained` guard + `remove_dir_all` for real dirs stay in `execute_removal` with their TOCTOU re-check (`removal.rs:436`). `Linker::unlink` must NOT bypass that guard.

### API — `crates/api/src/routes/skills.rs`

- `git_install_skills` (1959-2155): delete the `if req.universal {…} else { copy }` branch (2026-2117). Always route through `install_fetched_skill_and_lock` (`scope`, `project_root`, `target_agents` from `req.agents`, `use_relative_links = matches!(scope, ProjectOnly)`). Map `report.agent_results` → `GitInstallResultEntry`. The native agent returns `installed:true,error:None` (auto-classification). Lock write happens **inside** the primitive — delete the trailing manual `should_write_install_lock`/`write_skill_install_lock` block (2119-2146). Keep the precomputed `ref_commit` and the invalid-agent soft-failure rows (2010-2019).
- `install_skill` (1324-1450): same conversion. Replace the `selected_skills × dir_groups` loop (1396-1409) with one `install_fetched_skill_and_lock` call per skill. Drop `copied_skill_names` + manual lock loop (1420-1445).
- `import_skill` (1064-1102): inherits Edit via `add_skill_from_path`. The route's `write_skill_install_lock` hashes the SOURCE folder (`get_skill_root(expand_tilde_path(&request.path))`) — **KEEP unchanged**.
- **Delete dead copy helpers** (grep for live callers first): `install_git_skill_to_dir` (638-662), `copy_dir_recursive` wrapper (621-627), `install_git_skill_universal` (670-704), `build_git_install_groups` (710-755), `resolve_git_install_target_dir` (629-636), and `should_write_install_lock` (890-897)/`skill_lock_contains` (870-888) if no live caller remains. Rewrite the in-file tests at 2641-2676 to use the universal primitive or delete them (else the crate won't compile).

### `install_layout.rs` → `linker/`

`UniversalInstallReport.copied_fallback`, `LinkResult::CopiedFallback`, and the copy-on-symlink-failure branch in `link_one` are gone (covered above under the collapse). Ensure no caller reads `copied_fallback`.

### CLI — `crates/cli/src/commands/add.rs` + `main.rs:140`

`--universal` becomes a **deprecation no-op** (keep the clap flag so existing scripts don't error with unknown-arg; ignore its value). In `add::execute`, collapse the `if universal {…} else {…copy}` branches (37-41 install-from-path → always `add_skill_from_path`; 52-56 rename re-add → always `add_skill`; 72-76 manual-create → always `add_skill`). Optionally emit a verbose deprecation note.

### NOT touched

- `transfer.rs::copy_dir_recursive` (206; callers 646/700) is the cross-agent **transfer** action — distinct user action, out of scope (Open Question whether it also becomes link-only).
- `linker/mod.rs::copy_dir_recursive` (the EXCLUDE-list one, ex-`install_layout.rs:220`) STAYS — Master materialization.
- `InstallSkillRequest`/`install_all` legacy route — not the git-install/Sources flow.

### Verification grep

```bash
rg 'copy_dir_recursive|IsolatedCopy|copied_fallback|CopiedFallback|install_git_skill_to_dir|install_git_skill_universal|should_write_install_lock' crates/api crates/core
# only intended survivors: linker::copy_dir_recursive (Master), transfer::copy_dir_recursive, the lock helper
```

---

## Agent Auto-Detection (the blind-spot fix)

### The two facts already in the code

1. The install layer **half-solves** this: `install_universal_layout` skips a link when `dir == canonical_skills_dir` (`install_fetched.rs:350`), recording `installed:true`. But it only checks the **write** dir.
2. The classification axis is the **read** paths. "Reads `.agents/skills` natively" is a property of `AgentDescriptor::skill_read_paths` → `global_skill_read_paths`/`project_skill_read_paths` (`descriptor.rs:181-236`). An agent can write to its private dir **and** read the Master; for those the install correctly links, but the UI cannot tell the user they're free.

### Critical nuance (code vs AGENTS.md)

`descriptor.rs:181-209` appends `.agents`/XDG read paths via TWO mechanisms:

- Each descriptor's `global_skill_paths.read` / `project_skill_paths.read` **closures** push `.agents/skills` (this is how Codex/OpenCode/Cursor/Cline/Warp @global, and the larger project set, read the Master) — **not** via the universal flag.
- `capabilities.skills.universal == true` (only **Amp** and **Kimi**) appends the **XDG** path `get_universal_skills_path()` = `$XDG_CONFIG_HOME/agents/skills` (default `~/.config/agents/skills`) at global, and `project_root/.agents/skills` at project.

**Therefore** the classifier MUST be derived from the resolved `skill_read_paths` list — **never** from `capabilities.skills.universal` and **never** from a hardcoded name list. Consequence: **Amp/Kimi at global scope are NOT NativeReaders of `~/.agents/skills`** (their universal read path is the XDG dir), so they DO still need a link at global unless their descriptor's own read closure also lists the real canonical. (Product decision flagged in Open Questions.)

### Install-set computation

`install_universal_layout` partition per target agent (using `classify::classify_agent` for the active scope):

```text
for agent in target_agents:
    plan = classify_agent(descriptor, scope, project_root, master_dir)
    match plan.need {
        NativeReader        => results.push(installed:true, error:None)     // no link
        NeedsLink{agent_dir} => symlink_dirs.push(agent_dir)               // link
        Unsupported         => results.push(installed:false, error:..)     // soft-fail
    }
install_universal(source_root, &master_dir, &symlink_dirs, target)
```

The Master stays byte-identical (`EXCLUDE_*` unchanged); only the **link set** shrinks. Lock-write unchanged (auto-covered count as installed → `installed_any` stays true). At global scope the canonical is `~/.agents/skills` and the native subset is the smaller `{Codex, OpenCode, Cursor, Cline, Warp}` — discovered automatically from each descriptor's global read closure.

### UX change (both surfaces)

Today both surfaces compute a **flat** "installable" list and either send all of it (Sources) or force a manual pick (Import). New UX partitions the same `isUsable && supportsSkillMutation` base set into three buckets:

- **"Already covered" (NativeReader, auto-covered)** — read-only informational chips with `agentCoveredBadge`. Copy: _"These agents read the shared `.agents/skills` master directly — no link needed."_ Not selectable, not sent as link targets.
- **"Will be linked" (NeedsLink)** — the only selectable group; pre-checked = all usable+needs-link agents.
- **"Not available here" (Unsupported / not installed)** — hidden or muted.

The Master is always written even with **zero** link targets → an empty selection is now **valid** (master-only install). Old "must pick an agent" / "no agents" guards on the install path are removed.

### Data / DTO — coverage endpoint (Option A, preferred)

Canonicalization stays **server-side** (the frontend MUST NOT reimplement path canonicalization — macOS prefix + symlink resolution would diverge). No raw paths exposed (API anti-pattern), booleans only.

```text
GET /api/v1/skills/coverage?scope=<global|project>&project_root=<path?>
  -> Json<Vec<AgentSkillCoverageDto>>

#[derive(Serialize, TS)] #[ts(export)]
struct AgentSkillCoverageDto {
    id: String,
    scope: String,
    reads_master: bool,
    writes_master: bool,
    needs_link: bool,
    auto_covered: bool,   // reads_master || writes_master
    supported: bool,
}
```

Handler maps `classify::classify_all` over `registry::iter_all`. The DTO/endpoint is **per-scope** and requires `project_root` when `scope=project` (mirror `ScopeParams`).

**Option B (booleans on `AgentInfo`) is REJECTED**: `list_agents` passes `Path::new("")` as project_root (`agents.rs:28`), so project-scope canonical would be wrong, and it can't express `needs_link` vs `unsupported`. Scope+project_root must be request inputs.

Mount the route in `crates/api/src/lib.rs`; DTO in `crates/api/src/dto/`.

---

## Frontend Changes (`crates/desktop`)

### 1. Delete the install-layout abstraction

- **Delete** `src/lib/install-layout.ts` (`InstallLayout`, `DEFAULT_INSTALL_LAYOUT`, `isUniversalLayout`) and `src/lib/install-layout.test.ts`. With the toggle gone there is nothing to map.
- **`src/pages/sources/index.tsx`**:
    - Remove the `install-layout` import (48-52) and `ToggleButton`/`ToggleButtonGroup` from `@heroui/react` (17-26).
    - Delete the `installLayout`/`setInstallLayout` state (393-395) and the toggle UI block (855-894).
    - In `installFromSource`'s `gitInstall({...})` (~770), remove `universal: isUniversalLayout(installLayout)`.
- **`src/components/import-github-skill-panel.tsx`**: `handleInstall` (209-231) already sends no `universal` — no install-flag change; both surfaces produce the same body shape.

### 2. `GitInstallRequest.universal` — REMOVE the field

The field (`crates/api/src/dto/skill.rs:267-282`; generated `GitInstallRequest.ts:9-15`) is `Option<bool>` where the default (absent/false) meant **copy** — now a banned behavior. Keeping a field whose default requests banned behavior is a footgun.

- **Remove** `pub universal: Option<bool>` + its doc + `#[serde(default)]`/`#[ts(optional)]`.
- Confirm `GitInstallRequest` has **no `deny_unknown_fields`**, so a legacy client still sending `"universal": true|false` parses fine (field ignored) — forward-compatible.
- **Regenerate DTOs** per the prettier workflow (MEMORY: `generate:dto` alone shows a spurious 121-file diff): run the ts-rs export, then prettier over `src/generated/`, so the only real diff is the dropped `universal?: boolean`.
- `requests/skills.ts:314` passes the body straight through — no field reference, safe.

**Rejected (keep-ignored):** leaves a lying `universal?: boolean` in the generated surface inviting a future "wire it up" that reintroduces copy.

### 3. i18n

Delete from `src/lib/locales/{en,zh-Hant,zh-Hans}.ts`: `installLayoutLabel`, `installLayoutIsolation`, `installLayoutUniversal`, `installLayoutHint` (en.ts:1120-1124, zh-Hant.ts:1093-1096, zh-Hans.ts:1097-1100). Re-grep before deleting.

Add (same three files): `sourceInstallCoveredTitle`, `sourceInstallCoveredHint`, `sourceInstallLinkTargetsTitle`, `sourceInstallLinkTargetsHint`, `sourceInstallNoLinkTargets`, `agentCoveredBadge`.

### 4. Wire the coverage DTO into both surfaces

- New query options in `requests/agents.ts` + a `useSkillCoverage(scope, projectRoot)` hook joining `AvailableAgent` with the coverage DTO by id; helpers `isAutoCoveredByMaster(cov)` / `needsMasterLink(cov)` in `lib/agent-capabilities.ts`. **Re-query on scope change** (global vs project native sets differ).
- Partition the existing `isUsable && supportsSkillMutation` base set:
    ```ts
    const installable = availableAgents.filter(
    	(a) => a.isUsable && supportsSkillMutation(a, scope),
    );
    const autoCovered = installable.filter((a) => coverage[a.id]?.auto_covered);
    const linkTargets = installable.filter((a) => coverage[a.id]?.needs_link);
    ```
- **Import panel** (`import-github-skill-panel.tsx`): `AgentSelector` (817-846) feeds from `linkTargets` only; default `selectedAgents` (131) = first `linkTargets` (not first installable); **relax** the required-selection rule (820-825) to valid when ≥1 link target selected OR `linkTargets.length === 0`; render a read-only "Already covered" chip list (or `sourceInstallNoLinkTargets` when empty).
- **Sources page** (`index.tsx`): `installAgentIds` (450-460) → `linkTargetAgentIds`; an empty list is **no longer an error** (master-only install proceeds) — remove the no-agents early-return/toast on the install path (732). Add an inline summary "N linked / M auto-covered". The **removal** path `deleteInstalledSkillByName` (523) must keep resolving against the **full installable set**, not the link-target subset (flagged Open Question).
- `AgentSelector` itself is unchanged; the auto-covered list is plain `Chip`s, NOT a disabled selector (keeps "these are not choices" unambiguous).

### Implementation order (each step compiles)

1. FE-only: delete toggle/state/`install-layout.ts`/test/i18n, drop the `universal:` line. **Coordinate with the server going unconditionally-universal** so there's no window where omitting the field means "copy" under the old contract.
2. DTO removal + regen + prettier.
3. Coverage wiring (only cross-facet dependency: the core classifier + `/skills/coverage` route).

---

## Test Strategy (incl. Windows-CI junction tests)

### The single biggest fix

`install_layout.rs:378` declares the link test module `#[cfg(all(test, unix))]`, which keeps **every** link test off Windows CI — the junction path would ship untested. **Split** into a cross-platform base (`#[cfg(test)] mod tests`, asserting via `Linker::is_link` which works on both OSes) plus a nested `#[cfg(unix)] mod unix_specific` for `read_link`-relative-form assertions (junctions don't preserve relative form). Windows junctions need **no admin / no Dev Mode**, so the junction tests actually **execute** on `windows-latest` in the 3-platform `just test` gate.

### NO-COPY regression contract (strongest assertions)

- **Compile-time guarantee**: removing `UniversalInstallReport.copied_fallback` and `LinkResult::CopiedFallback` means the type system no longer admits a copy outcome. Sweep all crates (api/cli) for any pattern-match on `copied_fallback`/`CopiedFallback` before deleting — a miss fails the workspace build on all platforms.
- **T-NOCOPY** (cross-platform): after `install_universal`, assert `Linker::is_link(&link) == true`; on Windows assert it's a reparse point (`is_dir()==true && is_link==true`), never a plain dir. Then write a sentinel into the Master **after** linking and read it back **through** the link — proves a true link, not a coincidentally-identical copy.

### Windows junction (runs on windows-latest) — new `#[cfg(all(test, windows))]` module

- **T-WIN-JUNCTION-CREATE**: install one agent; assert `linked` has it, `Linker::is_link` true, reading `link\assets\a.txt` returns Master content.
- **T-WIN-JUNCTION-DETECT**: create a junction via `create_link`; assert `Linker::is_link == true` AND `symlink_metadata().file_type().is_symlink() == false` — pins the `0x0400` reparse branch that bare `is_symlink()` misses.
- **T-WIN-JUNCTION-REMOVE**: `Linker::unlink` a junction; assert link gone AND Master + files intact (proves `remove_dir`, not `remove_dir_all`).
- **T-WIN-JUNCTION-ABS-TARGET**: install at PROJECT scope with `LinkTarget::Relative`; assert the junction still resolves — pins that the fallback recomputed an **absolute** target before `mklink /J` (never passed `..\..`).

### Hard-error when neither symlink nor junction works

- **T-HARDERR** (`#[cfg(unix)]`): `set_permissions(agent_dir, 0o500)` after `create_dir_all` to force EACCES; assert `install_universal` returns `Err`, the link path does NOT exist, nothing copied. Use the `testing-fs-failures` skill technique; **skip under root** (`geteuid()==0`).
- **T-HARDERR-WIN**: junction almost always succeeds on the runner, so prefer **(b)** a deterministic unit test of the decision via an injectable `choose_link_outcome<F,G>(try_symlink, try_junction)` where both fakes return `Err` → `Err`, proving no third (copy) attempt exists. (Option (a): point the link parent at an invalid path; less reliable.) Document the choice in the test doc-comment.
- **T-HARDERR-NOPARTIAL** (both): after a forced hard error, Master intact (written first), agent link path absent.

### Idempotency / conflict / relative-vs-absolute / native (restructured base+leaf)

- **T-IDEMP** (cross-platform; from `is_idempotent_on_existing_correct_symlink`:331): install twice → second run `already_linked`, empty `linked`, link not re-created (assert stable `modified()` / unix inode). Detection via `Linker::is_link` so a Windows junction is recognized, not re-attempted or mis-flagged Conflict.
- **T-CONFLICT-REALDIR** (cross-platform; from `never_clobbers...`:357): pre-create a real dir; assert `conflicts` has it, content preserved, not a link after.
- **T-CONFLICT-FOREIGN-LINK** (cross-platform, NEW): pre-create a symlink/junction to a DIFFERENT target; assert `conflicts`, not clobbered, still resolves to the foreign target.
- **T-REL-PROJECT / T-ABS-GLOBAL** (`#[cfg(unix)]`; keep `relative_links_use_dotdot...`:389): asserts `read_link == "../../.agents/skills/my-skill"` (project) and `== canonical` (global). On Windows covered by T-WIN-JUNCTION-ABS-TARGET. Keep `relative_path_computes_minimal_dotdot` cross-platform (pure `Path`).
- **T-AGENTS-NATIVE-NO-LINK** (cross-platform, NEW): given a realistic exclusion list, the report's `linked` set never contains a path inside `.agents/skills` (Master never self-links; no native agent dir gets a redundant link). The per-agent matrix (Codex/OpenCode/Cursor/Cline/Warp @global excluded; Claude/Gemini/etc included) lives in **classify.rs unit tests** — see below.

### Classifier unit tests (`linker/classify.rs`)

Per-scope, against **real descriptors** (the most error-prone part — a wrong scope silently drops a needed link):

- Global: `{Codex, OpenCode, Cursor, Cline, Warp}` → `NativeReader`; Claude/Gemini/Copilot/Windsurf/RooCode/Mistral/… → `NeedsLink`.
- Project: the broader set (Codex/OpenCode/Cursor/Cline/Copilot/Gemini/Antigravity/Amp/Kimi/Warp) → `NativeReader`.
- Amp/Kimi @global → **NeedsLink** against `~/.agents/skills` (their universal read path is the XDG dir, not `~/.agents/skills`) — pins the nuance.
- Wire these into the existing `descriptor_regression.rs` style so classification can never drift from the AGENTS.md-documented native sets.
- macOS `/var`→`/private`: canonicalize BOTH sides in the comparison; add a test on a `tempfile` path under the symlinked `/tmp` root.

### NPX hash / lock golden tests stay GREEN, unchanged

- `crates/skill/tests/hash_parity_golden.rs` (CI-BLOCKING) and `npx_interop.rs` require **zero** edits — the hash is computed over the Master's files, symlink-vs-copy is irrelevant, and `EXCLUDE_FILES`/`EXCLUDE_DIRS`/traversal order are untouched. **Do not modify** `copy_dir_recursive`'s exclude lists.
- Add **T-MASTER-HASH-STABLE**: hash the Master before and after linking N agents → assert equal (linking must never mutate the Master).

### Removal tests (`removal.rs`)

Keep the existing `#[cfg(unix)]` symlink tests; add Windows-junction siblings: `execute_removal` on a junction unlinks (single reparse point) and **never** `remove_dir_all`s into the Master.

---

## Out of Scope

- **No global setting / config file** for install mode — there is no mode anymore.
- **No retroactive conversion** of existing copy-installs (forward-only). Coverage is a pre-install planning aid; it must not trigger relinking of legacy copies.
- **No copy escape hatch** anywhere (no hidden flag, no "advanced" toggle, no FE env override).
- **No iflow copy-mode** ported.
- **No `transfer.rs` cross-agent transfer redesign** (Open Question only).
- **No removal-path redesign** beyond keeping `deleteInstalledSkillByName` on the full installable set.
- **No `InstallSkillRequest`/`install_all` legacy route changes.**

---

## Open Questions

1. **Migration shim**: keep `#[deprecated] pub use linker as install_layout;` for one release, or update all ~9 call sites in one commit? _Recommend single-commit (in-crate, low risk)._
2. **`ConfigError::Link(LinkError)`** dedicated variant vs mapping into `ConfigError::Io`? A dedicated variant gives the UI a clean "links unsupported on this platform" message. _Recommend dedicated variant._
3. **`SkillInstallLayout::IsolatedCopy` / `install_isolated`**: physically delete now, or keep dead behind a removed flag to ease reading old copy-installs? _Recommend delete (copy banned as a choice)._
4. **CLI `--universal`**: hidden no-op for one release (safe) vs remove (breaks `--universal` callers with unknown-arg)? _Recommend hidden no-op._
5. **`GitInstallRequest.universal`**: confirm no `deny_unknown_fields` on the struct so legacy bodies keep parsing after removal.
6. **`add_skill` manual-create**: converge to `.agents` Master+link, or stay agent-local? _Assumed YES for "one model" — confirm._
7. **`transfer.rs` cross-agent transfer**: also become link-only, or out of scope? _Out of scope for this install-focused work unless decided otherwise._
8. **Amp/Kimi @global**: auto-cover against `~/.agents/skills` or link? Their universal read path is the XDG `~/.config/agents/skills`, so today they'd be **linked**. _Product decision needed._
9. **Coverage delivery**: server DTO endpoint (Option A, preferred) vs a static frontend lib keyed by scope — confirm A.
10. **Sources removal path** `deleteInstalledSkillByName` agent resolution under the link-target narrowing — keep full installable set (recommended).
11. **`classify_all` shape**: caller resolves `master_dir` via `universal_canonical_dir` first (current shape) vs the fn also returning the resolved master / "needs creating" flag.
12. **mklink /J quoting**: confirm the exact `Command` arg vector (`cmd`, `/C`, `mklink`, `/J`, `<link>`, `<abs target>`) handles spaces in paths (rely on `Command`'s per-arg quoting, not a single joined string).
13. **`conflicts` bucket**: distinguish foreign-symlink vs real-dir for the UI, or single bucket?

---

## Attribution & Licensing

The four ported primitives — `is_symlink_or_junction`, `remove_symlink_or_junction`, the `symlink_dir → mklink /J → hard error` ladder (`create_windows_symlink`), and `normalize_path` — are derived from **jiweiyeah/Skills-Manager** (`src-tauri/src/services/linker.rs`), **MIT-licensed**.

Add a module-level doc comment in `crates/core/src/skills/linker/mod.rs`:

```rust
//! Cross-platform directory-link primitives ported from jiweiyeah/Skills-Manager
//! (MIT) — linker.rs: is_symlink_or_junction / remove_symlink_or_junction /
//! create_windows_symlink / normalize_path. SM's iflow copy-mode is intentionally
//! NOT ported: aghub bans copy as a skill-install outcome.
```

Record the borrow in `UPSTREAM.md` as an **MIT-attributed external code borrow**, explicitly distinct from the `AkaraChen/aghub` fork ledger (this is a third-party MIT borrow, not a fork-sync row). Note the **behavior change** (removal of the Windows copy fallback → now a HARD ERROR) so it surfaces in release notes; a Windows environment that previously "worked" via copy (no Dev Mode AND `mklink /J` blocked) now hard-errors. If `UPSTREAM.md`'s structure makes a third-party borrow awkward, a `NOTICE`/`THIRD-PARTY.md` file is the alternative (Open Question 4 of the porting facet).
