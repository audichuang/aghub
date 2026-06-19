# Symlink-Only Skill Install + Standalone Linker + Agent Auto-Detection

**Date**: 2026-06-19
**Status**: Design — implementation-ready
**Scope crates**: `aghub-core`, `aghub-api`, `aghub-cli`, `crates/desktop`, plus tests in `aghub-core`/`aghub-skill`
**Related**: `docs/specs/2026-06-02-sources-and-universal-install.md`, `docs/specs/2026-06-19-cli-sources-management.md`, `CONTEXT.md` (Master/Referrer/Relink), skills `aghub-skills` / `npx-skills-contract` / `upstream-skills-flow` / `testing-fs-failures`

> **Line-number convention.** Every `file:line` in this spec was verified live
> against the working tree on 2026-06-19 and is an **anchor at time of writing**;
> the implementer must `grep` the symbol, not trust the integer, before editing
> (lines drift). The Skills-Manager (SM) line numbers in "Porting
> Skills-Manager" have now been **VERIFIED** against a local checkout of SM
> `src-tauri/src/services/linker.rs` (see the porting table for exact refs);
> the behaviour described (not the line) remains the contract.

---

## Goal & Locked Decisions

Converge the entire **skill-install** surface onto **one** model: **symlink-only**. A single `.agents/skills/<name>` Master plus per-agent links. Port Skills-Manager's MIT junction primitives so Windows installs survive without admin/Developer-Mode, delete every copy path **on the install surface**, and fix the agent auto-detection blind spot so the install/import UI stops asking the user to pick agents that are already covered by the Master or not installed.

**Locked decisions (do not relitigate):**

1. **ONE install model — symlink-only.** A single `.agents/skills/<name>` Master + per-agent links. **NO copy as a user choice. NO copy fallback on the install path.** "Install path" = the git-install / Sources / Import / `add-skill` / manual-create flows enumerated in "Call-site Rewiring". This decision does **not** reach `transfer.rs` cross-agent copy, which is a _different user action_ — see Decision 9.
2. **Forward-only.** Existing copy-installs on disk are **not** retroactively converted. The change is to the _creation_ path and the _detection_ paths, never a migration sweep.
3. **Link mechanics.** Unix = `symlink`. Windows = native `symlink_dir` **first**, then directory junction via `cmd /C mklink /J` (no admin). If **both** fail → **HARD ERROR** (no copy). Mirrors SM `create_windows_symlink`.
4. **No new crate dependency.** The junction is created via `std::process::Command` (`cmd /C mklink /J`), exactly as SM does.
5. **Port SM's MIT primitives with attribution:** `is_symlink_or_junction` (reparse-point `0x0400` detection), `remove_symlink_or_junction` (`remove_dir` then `remove_file`), `create_windows_symlink` (symlink_dir → mklink /J → hard error), `normalize_path`. **Do NOT port** SM's "iflow copy mode."
6. **Junctions need ABSOLUTE targets.** aghub uses relative links at project scope, absolute at global. The native `symlink_dir` attempt keeps the requested (possibly relative) target; the junction fallback **must** resolve to an absolute target even when a relative link was requested. **Precondition (asserted):** the `abs_target` passed to `mklink /J` MUST be absolute. The convenience layer asserts `canonical.is_absolute()` before linking and returns `LinkError::NonAbsoluteTarget` if not — project-root callers must pass an absolute root. **This is NOT free today:** the API takes project roots raw via `PathBuf::from(...)` (`extractors.rs:51`, `skills.rs:1949-1950`, `skills.rs:238`), so a relative `project_root` would break every link. The absolutization is therefore a **normative requirement** — see "Call-site Rewiring → Absolute project-root precondition (P0-C)".
7. **npx round-trip stays intact.** v3 global + v1 project lock writes and the byte-identical folder hash are untouched. The Master is still materialized via the existing exclusion-list `copy_dir_recursive` — that is _Master materialization_, **not** a per-agent copy.
8. **The blind spot is fixed.** Agents whose own skills dir IS or already READS `.agents/skills` see the Master directly and need **no link**. The design **auto-classifies** agents from their _resolved read/write paths_ (never a hardcoded list) and combines with CLI availability so the UI never asks the user to select an already-covered or not-installed agent.
9. **`transfer.rs` cross-agent transfer is OUT OF SCOPE and stays copy-based.** It is a distinct user action ("copy skill X from agent A to agent B"), not an install. Decision 1's copy ban is scoped to the install surface precisely so this is not a contradiction. Converting transfer to link-only is explicitly deferred (Out of Scope); the `transfer::copy_dir_recursive` survivor in the verification grep is **expected**.
10. **Hard-error policy is per-agent soft-fail, NOT whole-install abort.** A `LinkError` from `Linker::link` for one agent is folded into that agent's `AgentInstallResult { installed: false, error: Some(msg) }` row (matching today's behaviour at `install_fetched.rs:431-441`, the `Err(e)` soft-fail mapping). It does **not** abort the whole install or propagate as `Err(ConfigError)`. The _only_ hard `Err(ConfigError)` paths remain the pre-write guards (rename guard, scope guard, hash failure) and the lock-write failure. **The "no silent no-op" rule** (Decision 11) closes the gap the per-agent policy would otherwise leave.
11. **No silent no-op.** The lock is written whenever the **Master was freshly materialized on this run** (`wrote_master`), independent of whether any per-agent link succeeded. An install that writes the Master but whose every link hard-errors still records the install (lock written, Master present) and surfaces per-agent error rows — it is never a silent no-op. (Today the lock gate is `installed_any && should_write_install_lock(...)` at `install_fetched.rs:256-262`; this changes to write when `(wrote_master || installed_any) && should_write_install_lock(...)`, where `installed_any` now also counts auto-covered NativeReaders. See "Call-site Rewiring → install_fetched.rs".)

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
   .agents/skills/<name>   ◄────────── the single Master (project: <root>/.agents/skills/<name>,
        ▲   ▲   ▲                                          global:  ~/.agents/skills/<name>)
        │   │   │
        │   │   │  per-agent links (Linker::link):
        │   │   │    Unix  -> symlink (relative @project / absolute @global)
        │   │   │    Win   -> symlink_dir, else `cmd /C mklink /J <ABS master>`
        │   │   │    both fail -> per-agent soft-fail row (NO copy), Master still recorded
        │   │   │
   ┌────┴─┐ ┌┴────┐ ┌┴──────┐
   │.claude│ │.zed │ │.windsurf│ ...  NeedsLink agents (private skills dir)
   │/skills│ │/... │ │  ...    │
   └───────┘ └─────┘ └─────────┘

   Codex / OpenCode / Cursor / Cline / Warp (global) and a larger set (project):
        their resolved skill_read_paths ALREADY CONTAIN .agents/skills
        => NativeReader  => NO LINK  => "auto-covered" in the UI
```

> **Vocabulary pin (resolves the skills-dir / skill-dir confusion).**
>
> - **skills-dir** = a directory that _holds_ skills, e.g. `.agents/skills`, `~/.claude/skills`. This is what `get_skills_paths` / `target_skills_dir` return.
> - **skill-dir** (a.k.a. the **Master** / `canonical`) = a single skill's directory, `.agents/skills/<name>`. This is what `Linker::link` links _to_.
>   The **classifier** (`classify.rs`) reasons in **skills-dir** terms: an agent is a NativeReader iff its resolved skills-dir set contains the canonical **skills-dir** (`.agents/skills`). The **linker** (`mod.rs::Linker::link`) takes the **skill-dir** (`.agents/skills/<name>`) as `master_dir`. The convenience layer bridges them: it is handed the canonical **skill-dir** and links each NeedsLink agent's **skills-dir** to it. **These two are never compared against each other** (the central bug the reviewer flagged).

Three buckets per `(agent, scope)`, derived purely from the descriptor's resolved read/write **skills-dir** paths:

| Bucket                          | Condition (all in skills-dir terms)                                                                                               | Install action                                        | UI                               |
| ------------------------------- | --------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------- | -------------------------------- |
| **NativeReader / auto-covered** | resolved `get_skills_paths` (read) OR `target_skills_dir` (write) contains the canonical Master **skills-dir** (`.agents/skills`) | reported installed, **no link**                       | read-only "already covered" chip |
| **NeedsLink**                   | has a write skills-dir ≠ canonical skills-dir, and does **not** read it                                                           | `Linker::link(<skill-dir>, <agent skills-dir>, name)` | selectable checkbox              |
| **Unsupported**                 | no write skills-dir for this scope                                                                                                | soft-fail row                                         | hidden / muted                   |

---

## The Standalone Linker Module

### Location & name — MODULE under core, NOT a new crate

`crates/core/src/skills/linker/` — a directory module (`mod.rs` + `classify.rs`). It **replaces** the existing `crates/core/src/skills/install_layout.rs`.

Rationale (weighed against `Cargo.toml` `[workspace].members` and the `AGENTS.md` "Structure" dependency direction `agents → core → cli/api/desktop`):

1. The link primitives have **one** downstream consumer cluster — every caller already lives in or depends on `aghub-core` (manager/skill.rs, install_fetched.rs, removal.rs, api routes via core, CLI via core). A new crate would be used laterally by nothing, unlike the existing tool crates (`skill`, `git`, `json`, …) which each justify themselves by a distinct external dependency or reuse surface.
2. The "auto-classify which agents need a link" half is **not** std-only — it reads `AgentDescriptor` capabilities/paths. Splitting "mechanics crate" from "classify in core" would fragment one purpose across two homes. Keeping both in `crates/core/src/skills/linker/` puts the whole linker concern in one navigable place at the right layer.
3. It replaces an existing in-crate module, so the change is a mechanical in-crate move (`crate::skills::install_layout::*` → `crate::skills::linker::*`) — no `Cargo.toml` edit, no new `[workspace].members` line.

> **Future promotion**: if the mechanics later need to be reused by a lateral crate (e.g. `cc-plugins` junction-linking plugins), promote the **mechanical half** (`Linker` + `LinkTarget` + `LinkOutcome` + `LinkError`, no descriptor deps) to `crates/skill-link`. The API below is shaped so that promotion is a file move; `classify.rs` stays in core. Note `universal_canonical_dir` depends on `dirs`, so the promoted crate would carry a `dirs` dep (it is **std+dirs**, not std-only — see "Dependencies").

### File layout

```
crates/core/src/skills/linker/
├── mod.rs        # mechanical core (std + dirs): Linker { link, is_link, unlink },
│                 #   LinkTarget, LinkOutcome, LinkError, create_link (win: symlink_dir
│                 #   -> mklink /J -> Err), create_junction, normalize_path, relative_path,
│                 #   universal_canonical_dir, install_universal, link_agents_to_canonical,
│                 #   UniversalInstallReport, EXCLUDE_*/copy_dir_recursive (Master only)
└── classify.rs   # agent auto-classification (depends on aghub-agents descriptors):
                  #   LinkNeed, AgentLinkPlan, classify_agent, classify_all
```

`crates/core/src/skills/mod.rs`: change `pub mod install_layout;` → `pub mod linker;`. Update the call sites in the same commit (recommended; see Open Questions for the optional `#[deprecated] pub use linker as install_layout;` shim). The live call sites are: `install_fetched.rs:19-20` (`use crate::skills::install_layout::{install_universal, universal_canonical_dir}`) and the manager rename/relink helpers in `crates/core/src/manager/skill.rs` — `install_universal` at `:574` and `:640`, `universal_canonical_dir` at `:684`, `link_agents_to_canonical` at `:202`, `:754`, `:871`. There are **no `install_layout::*` call sites in `crates/api`** (the API routes call `install_fetched::install_fetched_skill_and_lock`, not the layout primitives directly), so the earlier `skills.rs:695`/`:2036` anchors were incorrect and are removed.

### Public API — mechanical core (`linker/mod.rs`, std + dirs)

```rust
/// Whether a created link's stored target is relative (project scope, portable)
/// or absolute (global scope). Windows junctions ALWAYS resolve to absolute even
/// when Relative is requested (junctions cannot store a relative target).
pub enum LinkTarget { Relative, Absolute }

/// Outcome of a single link attempt against one agent skills-dir.
pub enum LinkOutcome {
    Linked,        // fresh link created (unix symlink / win symlink / win junction)
    AlreadyLinked, // a correct link to the same Master already existed (idempotent)
    Conflict,      // foreign symlink/junction OR a real file/dir occupies the slot — NEVER clobbered
}

pub enum LinkError {
    /// BOTH native symlink AND `cmd /C mklink /J` failed on Windows (or symlink
    /// unsupported on a non-unix/non-windows platform). HARD per-agent error — NO copy.
    LinkUnsupported { target: PathBuf, link: PathBuf, source: io::Error },
    /// The absolute target invariant (Decision 6) was violated: `abs_target` was
    /// not absolute, so a junction could not be created safely.
    NonAbsoluteTarget { target: PathBuf },
    Io(io::Error),
}

pub struct Linker; // zero-sized, stateless

impl Linker {
    /// Create `agent_skills_dir/<skill_name>` -> `master_dir` (the
    /// `.agents/skills/<name>` canonical SKILL-DIR, which MUST already exist and
    /// MUST be absolute). Creates `agent_skills_dir` if absent. lstat-inspects the
    /// occupant WITHOUT following it (via `Linker::is_link`, so a junction is
    /// recognized): returns AlreadyLinked / Conflict without writing on collision.
    /// On a clean target: Unix => symlink; Windows => symlink_dir, else
    /// `cmd /C mklink /J <ABSOLUTE master>`; both fail => LinkError::LinkUnsupported.
    /// `master_dir` not absolute => NonAbsoluteTarget.
    pub fn link(
        master_dir: &Path,        // the SKILL-DIR: .agents/skills/<name>, absolute
        agent_skills_dir: &Path,  // the agent's SKILLS-DIR: e.g. ~/.claude/skills
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

/// `.agents/skills` store (the SKILLS-DIR) for a scope. project_root.is_some() =>
/// <root>/.agents/skills; None => ~/.agents/skills. Moves verbatim from
/// install_layout.rs:33. The returned path is absolute iff the input root is
/// absolute (callers MUST pass an absolute project_root — see Decision 6).
pub fn universal_canonical_dir(project_root: Option<&Path>) -> Option<PathBuf>;
```

### Convenience layer (thin over `Linker::link`, kept in `linker/mod.rs`)

```rust
pub struct UniversalInstallReport {
    pub canonical: PathBuf,        // the SKILL-DIR
    pub linked: Vec<PathBuf>,
    pub already_linked: Vec<PathBuf>,
    pub conflicts: Vec<PathBuf>,
    pub failed: Vec<(PathBuf, LinkError)>, // per-agent hard link failures (Decision 10)
    // NOTE: `copied_fallback` is REMOVED — the converged model bans copy.
}

/// Materialize the Master from source (npx-identical copy + exclusions) if
/// absent, then link each agent skills-dir. A per-agent link hard-error is
/// collected into `report.failed` (Decision 10), NOT returned as Err. The
/// returned `Err(LinkError)` is reserved for pre-link invariant violations
/// (NonAbsoluteTarget) or the Master copy itself failing.
pub fn install_universal(
    source_root: &Path,
    canonical: &Path,             // the SKILL-DIR (.agents/skills/<name>), MUST be absolute
    agent_skills_dirs: &[PathBuf],
    target: LinkTarget,
) -> Result<UniversalInstallReport, LinkError>;

/// Link agent skills-dirs to an already-materialized Master (add_skill / rename
/// relink). Same per-agent-soft-fail contract as install_universal.
pub fn link_agents_to_canonical(
    canonical: &Path,             // the SKILL-DIR, MUST be absolute
    agent_skills_dirs: &[PathBuf],
    target: LinkTarget,
) -> Result<UniversalInstallReport, LinkError>;
```

> The current convenience fns take `use_relative_links: bool`. Migrate callers to `if project_scope { LinkTarget::Relative } else { LinkTarget::Absolute }`. `install_universal` / `link_agents_to_canonical` may keep a `bool` internally if it shortens the diff, but the public boundary uses `LinkTarget`.
>
> **Relative-vs-absolute resolution.** The convenience layer is handed exactly one path — `canonical` (the absolute skill-dir). Per agent it computes BOTH targets internally: the **requested** target = `relative_path(agent_skills_dir, canonical)` when `LinkTarget::Relative` else `canonical`; and the **abs_target** = `canonical` (always absolute). It passes both into `Linker::link`'s internal `create_link(requested_target, abs_target, link)`. The junction-needs-absolute invariant therefore depends ONLY on `canonical` being absolute, which `install_universal` asserts up front (Decision 6 / `NonAbsoluteTarget`).

### Public API — classification (`linker/classify.rs`, depends on aghub-agents)

This is the **blind-spot fix**.

```rust
pub enum LinkNeed {
    /// Agent's own skills-dir at this scope IS or already READS .agents/skills
    /// (descriptor-driven): sees the Master directly, NO link required.
    NativeReader,
    /// Agent has a private skills-dir not mapped to the Master: needs a link.
    NeedsLink { agent_skills_dir: PathBuf },
    /// Agent's skills-dir cannot be resolved for this scope.
    Unsupported,
}

pub struct AgentLinkPlan {
    pub agent_id: &'static str,
    pub need: LinkNeed,
    pub installed: bool, // from availability::check_agent_availability
}

/// Classify ONE agent against a scope + project_root + the canonical Master
/// SKILLS-DIR (`.agents/skills`, NOT the skill-dir). NativeReader iff
/// `master_skills_dir` appears in the agent's resolved `get_skills_paths(...)`
/// (read) — covers Codex/OpenCode/Cursor/Cline/Warp @global and the broader
/// project set — OR the agent's `target_skills_dir(...)` (write) == that dir.
/// Both sides are canonicalized leniently (macOS /var->/private). Amp/Kimi's
/// universal flag appends the XDG dir, NOT ~/.agents/skills, so they are NOT
/// NativeReaders of the canonical at global (see "Critical nuance").
pub fn classify_agent(
    descriptor: &AgentDescriptor,
    scope: ResourceScope,
    project_root: Option<&Path>,
    master_skills_dir: &Path,   // the SKILLS-DIR: .agents/skills
) -> AgentLinkPlan;

/// Classify ALL registered agents (registry::ALL_AGENTS). Callers filter to
/// `installed && matches!(need, NeedsLink{..})` for the link set, and surface
/// NativeReader / Unsupported / not-installed as "already covered" / "skipped"
/// rather than asking the user.
pub fn classify_all(
    scope: ResourceScope,
    project_root: Option<&Path>,
    master_skills_dir: &Path,   // the SKILLS-DIR
) -> Vec<AgentLinkPlan>;
```

**Classifier algorithm** (reuse existing primitives — never a hardcoded agent list, `AGENTS.md` warns those drift). **All comparisons are skills-dir vs skills-dir** — this is the central fix:

```text
let adapter      = create_adapter(agent);
// master_skills_dir is .agents/skills (the SKILLS-DIR), passed in by the caller
// via universal_canonical_dir(...) — NOT the .agents/skills/<name> skill-dir.
let read_paths   = adapter.get_skills_paths(project_root, scope);   // skills-dirs (read)
let write_dir    = adapter.target_skills_dir(project_root, scope);  // skills-dir (write)
let canon        = canonicalize_lenient(master_skills_dir);
let reads_master  = read_paths.iter().any(|p| canonicalize_lenient(p) == canon);
let writes_master = write_dir.as_ref().map(canonicalize_lenient) == Some(canon);
NativeReader  if reads_master || writes_master
NeedsLink{ agent_skills_dir: write_dir.unwrap() }
              if write_dir.is_some() && !reads_master && !writes_master
Unsupported   otherwise
```

`canonicalize_lenient(p) = std::fs::canonicalize(p).unwrap_or(p.to_path_buf())` so it works before the dir exists (same trick `link_one` uses at `install_layout.rs:149`). **Canonicalize BOTH sides** to defeat the macOS `/var`→`/private` prefix mismatch (per MEMORY release-test-gate note).

> **Bucketing invariant.** For any `(agent, scope)`, exactly one of `{ auto_covered (= NativeReader), needs_link (= NeedsLink), !supported (= Unsupported) }` is true. The DTO booleans are a faithful projection of the single `LinkNeed` 3-state (see "Data / DTO"); the frontend never derives a bucket from `reads_master`/`writes_master` independently — those are _informational only_, and partitioning uses `needs_link` / `auto_covered`, which are computed server-side from `LinkNeed`. No agent can fall through.

### Dependencies

- `linker/mod.rs`: **std + `dirs`** — `std::path`, `std::io`, `std::fs`, `std::os::unix::fs::symlink` / `std::os::windows::fs::symlink_dir`, `std::process::Command` for `cmd /C mklink /J`, `std::os::windows::fs::MetadataExt` (`file_attributes`) for the reparse bit, and `std::os::windows::process::CommandExt` (`creation_flags`). `dirs` (already a workspace dep) is used by `universal_canonical_dir`'s global home — so the module is **std+dirs**, not std-only.
- `linker/classify.rs`: `aghub-agents` (`AgentDescriptor`, `Capabilities`, `ResourceScope`) + `crate::availability` + `crate::registry`. No new deps.
- `LinkError`: use the workspace `thiserror` for `From<io::Error>` consistency (optional but recommended). See Open Question 1 for `ConfigError::Link`.

### Isolation rationale (module boundary, not crate boundary)

The mechanical core takes only paths + a `LinkTarget` enum and is descriptor-free; `classify.rs` takes descriptors. Both test in isolation (the existing test module already proves the mechanics test with `tempdir`). The mechanical/classify split is what keeps a future crate-promotion a clean file move.

---

## Porting Skills-Manager

Source: **jiweiyeah/Skills-Manager**, `src-tauri/src/services/linker.rs`, MIT-licensed. We port only the four cross-platform link primitives. Everything else in SM (iflow copy-mode, lock-status enums, hub import) stays behind.

> **SM line numbers below are VERIFIED** against a local checkout of SM `src-tauri/src/services/linker.rs` (714 lines). The reparse detection is confirmed at SM `linker.rs:111-112` (`const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;` then `meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0`). The _behaviour_ (not the line) remains the contract.

### Function-by-function map (SM → aghub)

| SM symbol (linker.rs, VERIFIED lines)     | aghub destination                                                                                                                  | Notes                                                                                                                                                                                                                                                     |
| ----------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `normalize_path` (SM:12-18)               | `linker/mod.rs` `pub(crate)` helper                                                                                                | `/`→`\` on Windows (`MAIN_SEPARATOR=='\\'`), no-op on Unix. Feeds `cmd.exe` native separators. No other aghub caller.                                                                                                                                     |
| `is_symlink_or_junction` (SM:101-118)     | `Linker::is_link`                                                                                                                  | `symlink_metadata().file_type().is_symlink()` first; on Windows `meta.file_attributes() & 0x0400 (FILE_ATTRIBUTE_REPARSE_POINT) != 0` via `MetadataExt` (const declared at SM:111, checked at SM:112). **The lynchpin** (see "Why this is load-bearing"). |
| `remove_symlink_or_junction` (SM:122-136) | `Linker::unlink`                                                                                                                   | Windows `remove_dir(p).or_else(                                                                                                                                                                                                                           | \_  | remove_file(p))`; Unix `remove_file(p)`. Uses `remove_dir`**not**`remove_dir_all`— only unlinks the reparse point, never recurses into the Master. Tolerate`NotFound`. |
| `create_windows_symlink` (SM:218-275)     | private `create_link(requested_target, abs_target, link)` (Windows arm) + extracted `pub(crate) create_junction(abs_target, link)` | Adapted: see below. Replaces the current `create_dir_symlink` (`install_layout.rs:258-274`). The junction arm is extracted as a named fn so tests can force the junction path (T-WIN-JUNCTION-DETECT).                                                    |

### `create_link` control flow (replaces `install_layout.rs:258-274`)

```text
create_link(requested_target, abs_target, link):
  // requested_target may be RELATIVE (project) or absolute (global).
  // abs_target is ALWAYS the absolute canonical Master skill-dir path.
  // Caller (Linker::link) has already verified abs_target.is_absolute().

  #[cfg(unix)]:
      std::os::unix::fs::symlink(requested_target, link)   // honors relative/abs
      -> Ok | Err   // HARD per-agent error, no copy

  #[cfg(windows)]:
      if std::os::windows::fs::symlink_dir(requested_target, link).is_ok(): Ok   // native (Dev Mode/admin)
      create_junction(abs_target, link)                    // -> Ok | Err(LinkUnsupported)

  #[cfg(not(any(unix, windows)))]:
      Err(LinkError::LinkUnsupported { .. })

create_junction(abs_target, link):       // #[cfg(windows)] pub(crate)
  let out = Command::new("cmd")
              .args(["/C", "mklink", "/J"])
              .arg(normalize_path(link))
              .arg(normalize_path(abs_target))             // junction needs ABSOLUTE
              .creation_flags(0x08000000)                  // CREATE_NO_WINDOW
              .output();
  if out.status.success(): Ok
  Err(LinkError::LinkUnsupported { .. })                   // -> HARD per-agent error, NO copy
```

**Adaptations vs SM:**

- Signature unified cross-platform `(requested_target, abs_target, link)` so callers don't branch per-OS.
- **Drop SM's pre-clean step** (SM removes an existing occupant). aghub's `Linker::link` already lstat-inspects the occupant and only reaches `create_link` when the slot is `NotFound`; pre-cleaning would let us clobber a foreign occupant, violating the no-clobber invariant (`install_layout.rs:357` `never_clobbers...` test). **Create-only.**
- **Junction target MUST be absolute.** The native attempt uses `requested_target` (possibly relative, for portability); the `mklink /J` fallback uses `abs_target`. Never pass a `..\..` relative string to `mklink /J` — it silently produces a broken junction.
- **Drop SM's GBK best-effort decoding** (a no-op that already used `from_utf8_lossy`). Keep plain `String::from_utf8_lossy` on stderr/stdout for the error message, and the informative `mklink /J {link} {target}` error format.
- **Quoting.** Use `Command::new("cmd").args(["/C", "mklink", "/J", <link>, <abs_target>])` with each path as a **separate arg** (Rust applies `cmd`'s argument quoting) — never a single joined string. `normalize_path` only swaps separators; it does NOT add quotes. Paths with `cmd` metacharacters (`&`, `^`, `%`, `(`, `)`) under `/C` are a known `cmd.exe` hazard; aghub's `.agents/skills/<sanitized_name>` paths cannot contain them (the name passes through `skill::sanitize::sanitize_name`), and project roots with such characters are out of scope (Open Question 7). Spaces ARE handled by per-arg quoting and are covered by T-WIN-JUNCTION-CREATE on a spaced path.
- Keep `creation_flags(0x08000000)` (CREATE_NO_WINDOW).
- **Hard per-agent error on both-fail.** No copy.
- **`file_attributes` is `symlink_metadata`-based** (never follows). On Windows a reparse point that is also a mount/dedup point can carry `0x0400`; this is an accepted edge — aghub only creates junctions/symlinks under `.agents`, and the no-clobber check means a foreign reparse point is reported `Conflict`, not removed.

### How `install_layout.rs` collapses into `linker/`

| Current `install_layout.rs` symbol (verified line)                | Fate                                                                                                                                                                                                                                                                                                                                                                                    |
| ----------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `universal_canonical_dir` (33)                                    | Verbatim → `linker/mod.rs`.                                                                                                                                                                                                                                                                                                                                                             |
| `UniversalInstallReport` (44-57)                                  | Kept, **minus `copied_fallback`** (51-53); add `failed: Vec<(PathBuf, LinkError)>`.                                                                                                                                                                                                                                                                                                     |
| `enum LinkResult` (59-64)                                         | Replaced by public `LinkOutcome`; **drop `CopiedFallback`** (62).                                                                                                                                                                                                                                                                                                                       |
| `install_universal` (74-87)                                       | Kept as convenience fn; delegates each link to `Linker::link`, returns `LinkError` only for pre-link invariant / Master-copy failure (per-agent failures into `report.failed`).                                                                                                                                                                                                         |
| `link_agents_to_canonical` (93-136)                               | Kept; loop over `Linker::link`; drop the `CopiedFallback` arm (128-130); collect hard failures into `report.failed`.                                                                                                                                                                                                                                                                    |
| `link_one` (138-183)                                              | Becomes the body of `Linker::link`. lstat occupant inspection (146-164) reused (swap `meta.file_type().is_symlink()` at 148 → `Linker::is_link` so a junction is recognized); `use_relative` branch (168-172) → `LinkTarget` + internal `relative_path`/`abs_target` split; the `create_dir_symlink` + copy-fallback (174-182) **REPLACED** by `create_link` with per-agent hard error. |
| `relative_path` (188-213)                                         | Private helper in `linker/mod.rs`. Feeds the `Relative` requested target; the `abs_target` for the junction fallback is the absolute `canonical` directly.                                                                                                                                                                                                                              |
| `EXCLUDE_FILES` / `EXCLUDE_DIRS` / `copy_dir_recursive` (217-256) | **Move verbatim.** Still materializes the Master byte-identically to npx (round-trip contract). Add a doc note: _"this copy materializes the single Master only; it is NOT a per-agent copy fallback."_                                                                                                                                                                                 |
| `create_dir_symlink` cfg-arms (258-274)                           | **Rewritten** into `create_link` + `create_junction` (above).                                                                                                                                                                                                                                                                                                                           |
| `#[cfg(all(test, unix))] mod tests` (276-518)                     | Moves; **gate changed** to a cross-platform base + `#[cfg(unix)]` leaf + `#[cfg(windows)]` leaf (see Test Strategy); `copied_fallback`/copy assertions deleted; `is_link`/`unlink`/hard-error/junction tests added.                                                                                                                                                                     |

### Why `is_symlink_or_junction` is load-bearing (the latent Windows bug it fixes)

aghub today uses bare `meta.file_type().is_symlink()` in spots where an `mklink /J` junction would silently fail (a junction reports `is_dir()==true`, `is_symlink()==false`):

- `install_layout.rs:148` (`link_one` already-linked-vs-conflict): a junction we created last run would be seen as a **foreign real dir** → `Conflict`, breaking idempotency on every Windows re-install.
- `discovery.rs:49` (`collect_skills`): discovery marks a referrer's `canonical_path` ONLY when `meta.file_type().is_symlink()` is true (the check at `discovery.rs:48-49`, canonicalize at `:50-54`). A Windows junction has `is_symlink()==false`, so a junction install is **rediscovered as an ordinary directory** — `canonical_path` stays unset — and later delete/update treats a junction install as a copy. Must become `if Linker::is_link(&path)`, setting `canonical_path` for junctions too. **(P0 — discovery was the missed call site.)**
- `removal.rs:430` (`execute_removal`): the `ft.is_symlink()` branch (430-434) misses a junction, which then falls through to the `ft.is_dir()` branch (435-444) → `remove_dir_all` **recurses into the shared `.agents` Master** = data loss. Must become `if Linker::is_link(path) { Linker::unlink(path) }` placed **before** the `is_dir()` branch.
- `removal.rs:248` and `:263` (`plan_symlink_removal`): the removal PLANNING path. Both branches gate on `meta.file_type().is_symlink() && targeted` (`:248` for entries resolving to the canonical, `:263` for dangling/unresolvable links). A **targeted junction referrer** is not planned for unlink → it is silently left orphaned. Swap both to `Linker::is_link(&entry) && targeted`. **(P0 — planning path was missed.)**
- `removal.rs:364` (`dir_has_external_referrer`): `if !meta.file_type().is_symlink() { continue; }` skips a junction, so an **external junction referrer is not detected**. A shared Master with a live junction referrer in another agent's dir could then be wrongly removed. Swap to `if !Linker::is_link(&entry) { continue; }`. **(P0 — external-referrer detection was missed.)**
- `crates/api/src/routes/skills.rs:380-381` (`delete_skill_by_path`): `path_is_symlink = symlink_metadata(&skill_dir).map(|meta| meta.file_type().is_symlink())` feeds the canonical-layout branch decision at `:387` (`|| path_is_symlink`). A junction install passes through this route with `path_is_symlink == false` and bypasses the canonical-layout handling. Swap the probe to `Linker::is_link(&skill_dir)` (or route through the planned remover). **(P1 — delete route was missed.)**
- `manager/skill.rs:60-62` (`remove_skill_path`): probes `m.file_type().is_symlink()` then `remove_file(&link)` (64) — a junction has `is_symlink()==false`, so `needs_unlink` is false and the junction is left orphaned on delete. Swap the probe to `Linker::is_link` and the removal to `Linker::unlink`. (A separate removal path from `execute_removal` — both must be fixed.)
- `manager/skill.rs` `universal_relink_referrers` (718), `universal_relink_agents` (740), `rollback_master_rename` (864): the "is this entry a symlink pointing at the old Master" probe — a Windows junction referrer would be skipped on rename, orphaning it. Swap all to `Linker::is_link`.

### Explicitly DROP (do not port)

- SM iflow copy-mode: `IFLOW_TOOL_ID`, `tool_uses_copy_mode`, `enable_skill_for_tool`/`disable_skill_for_tool`, `check_link_for_tool`, `check_link_for_scoped_skill`.
- `CopyModeMetadata` + `.skills-manager-source.json` + all `copy_mode_*` helpers.
- SM `copy_dir_all` / `copy_dir_all_include_hidden` / `copy_dir_all_with_options` — aghub keeps its own npx-contract `copy_dir_recursive`.
- SM `LinkStatus`/`LinkResult`/`LinkReport`/`check_link`/`sync_all_for_tool`/`import_to_hub` — aghub has its own report types + rename/relink transaction.

---

## Call-site Rewiring & Copy Removal

The good news: `crates/core/src/skills/install_fetched.rs::install_universal_layout` (346-444) ALREADY implements the symlink-shaped path and ALREADY half-classifies natives via the `.filter(|dir| *dir != canonical_skills_dir)` native skip (397) — but only the **write** dir. The blind spot is that the **live API/CLI routes do not take this path** — they call duplicate copy-based helpers. Most of this work is deleting copy branches and re-pointing live callers at the one shared primitive, then refining the primitive to also skip `reads_master` (not just `writes_master`) natives via the classifier.

### Core — `install_fetched.rs`

- **Delete** the whole `SkillInstallLayout` enum incl. `IsolatedCopy` (54-62 — one-variant enums don't earn their keep), `install_isolated` (292-339), the local `copy_dir_recursive` (37-51), and the `match req.layout` dispatch (229-249). `install_fetched_skill_and_lock` ALWAYS uses `install_universal_layout`. Remove the `layout` field from `FetchedSkillInstallRequest` (88).
- **Refine** `install_universal_layout` (346-441): replace the per-agent `match resolve_target_dir(...)` block (387-411) with `classify::classify_agent(descriptor, scope, project_root, &canonical_skills_dir)`: `NativeReader` → `AgentInstallResult{installed:true, error:None}` (no link); `NeedsLink{agent_skills_dir}` → collect into the symlink set; `Unsupported` → soft-fail. Then call `install_universal` (now copy-free). Fold `report.failed` per-agent link errors into `AgentInstallResult{installed:false, error:Some(msg)}` (preserves today's 431-441 per-agent-soft-fail behaviour, the `Err(e)` mapping — Decision 10).
- **No silent no-op (Decision 11).** Change the lock gate in `install_fetched_skill_and_lock` (256-262) from `installed_any && should_write_install_lock(...)` to `(wrote_master || installed_any) && should_write_install_lock(...)`, where `install_universal_layout` already returns `wrote_master` (379) as the second tuple element (`copied_any`). NativeReaders count as `installed`, so an all-NeedsLink install whose links all fail still records the install because `wrote_master == true`. `should_write_install_lock`/`skill_lock_contains` (112-137) stay.
- **Absolute-root precondition (Decision 6 / P0-C).** `install_universal_layout` computes `canonical_root` (356-360) and `canonical_skills_dir` via `universal_canonical_dir` (361). At project scope `project_root` flows from the route's `parse_install_scope` + raw `PathBuf::from`; the route MUST resolve it to an absolute path before calling the primitive — see the normative **"Absolute project-root precondition (P0-C)"** subsection below for the exact entry points that take roots raw and the required fix. `install_universal` asserts `canonical.is_absolute()` and returns `NonAbsoluteTarget` otherwise; the primitive folds that into a per-agent error row for every NeedsLink agent rather than panicking.

### Core — `discovery.rs`

- `collect_skills` (the canonical-path detection at `discovery.rs:48-54`): swap the `meta.file_type().is_symlink()` probe (`:49`) → `Linker::is_link(&path)` so a Windows **junction** install is recognized as a referrer and gets its `canonical_path` set (`:50-54` unchanged). Without this fix a junction install is rediscovered as an ordinary copy on Windows, and every downstream consumer of `Skill.canonical_path` (delete/update/relink) treats it as a copy. **(P0 — this call site was missed by the earlier draft.)** Add the Windows regression test `T-DISCOVERY-JUNCTION-CANONICAL` (see Test Strategy).

### Core — `manager/skill.rs`

- `remove_skill_path` (47-119): swap the `m.file_type().is_symlink()` probe (60-62) → `Linker::is_link(&link)` and the `std::fs::remove_file(&link)` (64) → `Linker::unlink(&link)`. (Required so a Windows junction is actually removed on delete and never orphaned. The real-dir branch 81-117 with its `assert_contained` guard is unchanged.)
- `add_skill_from_path` (526-588): stop copying into the agent's own dir. Make it a thin wrapper → `self.add_skill_from_path_universal(path)` (597+, the reference impl that already excludes the native via the `Some(d) if d != canonical_dir` pattern, preserves no-clobber, keeps the duplicate-name guard). Delete the old copy body.
- `add_skill` manual-create (122-164) → delegate to `add_skill_universal` (165-219) so manual creation also writes a Master + link (required by Locked Decision 1's "one model" — manual-create convergence is **decided**, not open).
- Rename relink helpers (`universal_relink_referrers` 706-725 incl. the `is_symlink()` at 718, `universal_relink_agents` 731-761 incl. 740, `rollback_master_rename` 848-886 incl. 864): swap `meta.file_type().is_symlink()` → `Linker::is_link`. Order matters — the rename caller unlinks stale links first (`Linker::unlink`) before re-linking, so the idempotency `canonicalize` does not misfire on a moved Master.

### Core — `removal.rs`

Replace `execute_removal`'s symlink branch (`removal.rs:430-434`), keeping the link check **before** the `is_dir()` branch (435-444):

```rust
if Linker::is_link(path) {             // was: ft.is_symlink()  (line 430)
    match Linker::unlink(path) {        // was: remove_file(path) — now remove_dir-then-remove_file
        Ok(()) => report.removed.push(path.clone()),
        Err(e) => report.failed.push((path.clone(), e)),
    }
} else if ft.is_dir() {
    // unchanged (435-444): assert_contained + remove_dir_all (containment re-check stays)
}
```

**Security**: `Linker::unlink` is only the symlink/junction-unlink half. The `assert_contained` guard + `remove_dir_all` for real dirs stay in `execute_removal` (437-441) with their TOCTOU re-check. `Linker::unlink` must NOT bypass that guard.

Two more `is_symlink()` probes in this file — the **removal-planning** and **external-referrer-detection** paths — were missed by the earlier draft and MUST also route through `Linker::is_link` (a junction has `is_symlink()==false`, so without these the planner skips junction referrers and the external-referrer guard fails to see a live junction, risking removal of a still-referenced Master):

- `plan_symlink_removal` (`removal.rs:221`): both `meta.file_type().is_symlink() && targeted` checks — the canonical-resolved branch (`:248`) and the dangling/unresolvable branch (`:263`) — become `Linker::is_link(&entry) && targeted`. A targeted junction referrer is now planned for unlink instead of orphaned.
- `dir_has_external_referrer` (`removal.rs:351`): `if !meta.file_type().is_symlink() { continue; }` (`:364`) becomes `if !Linker::is_link(&entry) { continue; }` so an external junction referrer is detected and a shared Master is not wrongly removed.

Add `T-PLAN-JUNCTION-REFERRER` (a targeted junction referrer is planned + unlinked) and `T-EXTERNAL-JUNCTION-REFERRER` (an external junction referrer blocks Master removal) — see Test Strategy.

### Absolute project-root precondition — NORMATIVE (Decision 6, P0-C)

Decision 6 asserted the `abs_target` passed to `mklink /J` must be absolute and claimed "callers today pass absolute roots." **That claim is FALSE for the API.** Project roots reach the install path **raw**, via `PathBuf::from(...)` with no absolutization:

- `extractors.rs:51` — `ResolvedScope::Project { root: PathBuf::from(root) }`.
- `git_install_skills` — `project_root = req.project_root.as_ref().map(PathBuf::from)` (`skills.rs:1949-1950`).
- `install_skill` — `project_root = req.project_root.as_ref().map(PathBuf::from)` (`skills.rs:238`).
- (the same raw pattern also appears at the prune route, `skills.rs:493`.)

A **relative** `project_root` (e.g. a client sending `"./my-project"`) yields a relative `.agents/skills/<name>` canonical, so `universal_canonical_dir(Some(relative_root))` returns a relative path, `install_universal`'s `canonical.is_absolute()` assertion fails, and **every** NeedsLink agent gets a `NonAbsoluteTarget` per-agent error row — a silent, total install failure under the new contract.

**Requirement (normative):** every API project-scope entry point that reaches the classifier/linker MUST absolutize `project_root` **before** the call — `std::fs::canonicalize(root)` when the path exists, else `std::env::current_dir()?.join(root)`. Apply at the route boundary (`git_install_skills`, `install_skill`) and/or centrally in `extractors.rs` so `ResolvedScope::Project.root` is always absolute. **AND/OR** make `universal_canonical_dir` itself return an absolute path (join a relative `project_root` with cwd before building `.agents/skills`). The belt-and-braces choice — absolutize at the boundary _and_ defensively in `universal_canonical_dir` — is recommended; either alone closes the bug. Pin with `T-REL-ROOT-ABSOLUTIZED` (see Test Strategy).

### API — `crates/api/src/routes/skills.rs`

- `git_install_skills` (1918+): delete the `let layout = if req.universal.unwrap_or(false) { Universal } else { IsolatedCopy }` branch (1988-1992). Always route through `install_fetched_skill_and_lock` (`scope`, `project_root` **resolved to absolute** — see P0-C below, `target_agents` from `req.agents`, `target = matches!(scope, ProjectOnly) ? Relative : Absolute`). Map `report.agent_results` → `GitInstallResultEntry`. Native agents return `installed:true,error:None`. Lock write happens **inside** the primitive — delete any trailing manual `should_write_install_lock`/`write_skill_install_lock` block. Keep the precomputed `ref_commit` and the invalid-agent soft-failure rows.
- `install_skill` (1283+) — **IN SCOPE** (the old "Out of Scope" exclusion of `InstallSkillRequest` is removed; this resolves the rewrite-vs-forbid contradiction). Same conversion: replace the `selected_skills × dir_groups` copy loop (1354-1366) with one `install_fetched_skill_and_lock` call per discovered skill. Drop `copied_skill_names` (declared 1352; used 1358-1360 and 1379) + the manual lock loop (1378-1403). `project_root` MUST be resolved to absolute before the call (P0-C). `InstallSkillRequest` (`dto/skill.rs:138-145`) has **no `universal` field** (it was always copy); after conversion it is symlink-only like every other install route.
    - **`InstallSkillResponse` per-agent rows (P1-D).** Today `InstallSkillResponse` (`dto/skill.rs:149-151`) is only `{ success: bool }`. Now that `install_skill` routes through the core primitive and Decision 10 makes link failures **per-agent soft-fails**, an aggregate boolean cannot surface _which_ agents failed. **Add per-agent result entries** to `InstallSkillResponse` — e.g. `pub agents: Vec<GitInstallResultEntry>` (reuse the git-install row type or a parallel `InstallSkillResultEntry { agent, installed, error }`) alongside `success`. This is a ts-rs DTO change: **regenerate the DTO and run prettier** over `src/generated/` per the generated-DTO workflow (MEMORY: `generate:dto` alone shows a spurious 121-file diff; prettier-then-diff to isolate the real change). _Exception (only if a per-agent shape is rejected):_ explicitly document `InstallSkillResponse` keeping the aggregate boolean as a deliberate exception to Decision 10's per-agent surfacing — but the per-agent rows are the recommended choice for parity with `git/install`.
- `import_skill` (1023-1060): inherits the symlink behaviour via `add_skill_from_path`. The route's `write_skill_install_lock` hashes the SOURCE folder (`get_skill_root(expand_tilde_path(&request.path))`, at `:1042`) — **KEEP unchanged**.
- `delete_skill_by_path` (1023... no — the route at `:199`): route the canonical-layout decision through `Linker::is_link` instead of the bare `is_symlink()` probe at `:380-381` (which feeds `path_is_symlink` into the `|| path_is_symlink` decision at `:387`). A junction install otherwise bypasses canonical-layout handling. **(P1-E — this route was missed.)** Add `T-DELETE-BY-PATH-JUNCTION` (see Test Strategy).
- **Delete dead copy helpers** (grep for live callers first; delete only once `install_skill` + `git_install_skills` no longer call them): `copy_dir_recursive`, `install_git_skill_to_dir`, and `build_git_install_groups` (`:668`). (`install_git_skill_universal` does **not** exist in the current source — earlier references to it are removed.) The API-local `should_write_install_lock` (`:848`) becomes dead **only after** `install_skill` stops calling it — since `install_skill` is now in scope and routed through the core primitive, that caller goes away and the helper can be deleted (resolves the lock-helper survivor question). Rewrite/delete the in-file tests at `:2518`/`:2582` (`build_git_install_groups` / `should_write_install_lock`) else the crate won't compile.

### `install_layout.rs` → `linker/`

`UniversalInstallReport.copied_fallback`, `LinkResult::CopiedFallback`, and the copy-on-symlink-failure branch in `link_one` are gone (covered above). Ensure no caller reads `copied_fallback`.

### CLI — `crates/cli/src/commands/add.rs` + `main.rs`

`--universal` becomes a **deprecation no-op** (keep the clap flag so existing scripts don't error with unknown-arg; ignore its value). In `add::execute`, collapse the `if universal {…} else {…copy}` branches (install-from-path → always `add_skill_from_path`; rename re-add → always `add_skill`; manual-create → always `add_skill`). Optionally emit a verbose deprecation note. Verify exact branch line ranges against `add.rs` before editing.

### NOT touched

- `transfer.rs::copy_dir_recursive` (206; callers ~646/700) is the cross-agent **transfer** action — distinct user action, OUT OF SCOPE per Locked Decision 9 (NOT a contradiction with Decision 1, which is scoped to install). Stays copy-based.
- `linker/mod.rs::copy_dir_recursive` (the EXCLUDE-list one, ex-`install_layout.rs:220`) STAYS — Master materialization.

### Verification grep

```bash
rg 'copy_dir_recursive|IsolatedCopy|copied_fallback|CopiedFallback|install_git_skill_to_dir|install_git_skill_universal|build_git_install_groups|should_write_install_lock' crates/api crates/core
# EXPECTED survivors only:
#   crates/core/src/skills/linker/mod.rs::copy_dir_recursive       (Master materialization)
#   crates/core/src/transfer.rs::copy_dir_recursive                (cross-agent transfer, Decision 9)
#   crates/core/src/skills/install_fetched.rs::should_write_install_lock + skill_lock_contains
# Anything else is a leftover copy path and MUST be removed.
```

---

## Agent Auto-Detection (the blind-spot fix)

### The two facts already in the code

1. The install layer **half-solves** this: `install_universal_layout` skips a link via `.filter(|dir| *dir != canonical_skills_dir)` (`install_fetched.rs:397`), recording `installed:true`. But it only checks the **write** skills-dir.
2. The classification axis is the **read** skills-dirs. "Reads `.agents/skills` natively" is a property of `AgentDescriptor::skill_read_paths` → `global_skill_read_paths` (`descriptor.rs:181`) / `project_skill_read_paths` (`descriptor.rs:197`). An agent can write to its private dir **and** read the Master; for those the install correctly links, but the UI cannot tell the user they're free.

### Critical nuance (code vs AGENTS.md)

`descriptor.rs:181-213` appends `.agents`/XDG read paths via TWO mechanisms:

- Each descriptor's `global_skill_paths.read` / `project_skill_paths.read` **closures** push `.agents/skills` (this is how Codex/OpenCode/Cursor/Cline/Warp @global, and the larger project set, read the Master) — **not** via the universal flag.
- `capabilities.skills.universal == true` (only **Amp** and **Kimi**) appends the **XDG** path `get_universal_skills_path()` = `$XDG_CONFIG_HOME/agents/skills` (default `~/.config/agents/skills`) at global (`descriptor.rs:188-189`), and `project_root/.agents/skills` at project (`descriptor.rs:207`).

**Therefore** the classifier MUST be derived from the resolved `get_skills_paths` (= `skill_read_paths`) list — **never** from `capabilities.skills.universal` and **never** from a hardcoded name list.

> **Amp/Kimi @global — LOCKED (not an open question).** At **global** scope, Amp/Kimi's universal read path is the XDG dir (`~/.config/agents/skills`), **not** `~/.agents/skills`. The canonical Master at global is `~/.agents/skills`. Therefore **Amp/Kimi @global classify as NeedsLink** against `~/.agents/skills` and DO get a link — this falls out of the algorithm automatically with no special-casing, and the classifier test pins it. At **project** scope the universal flag appends `project_root/.agents/skills`, which IS the canonical, so Amp/Kimi @project ARE NativeReaders. (Relocating their global Master to the XDG path is a separate change to `universal_canonical_dir` and is Out of Scope.)

### Install-set computation

`install_universal_layout` partitions each target agent via `classify::classify_agent` for the active scope, against the canonical **skills-dir** (`canonical_skills_dir`, not the skill-dir):

```text
let canonical_skills_dir = universal_canonical_dir(canonical_root)?;  // .agents/skills (SKILLS-DIR)
let canonical = canonical_skills_dir.join(safe_name);                 // .agents/skills/<name> (SKILL-DIR)
for agent in target_agents:
    plan = classify_agent(descriptor, scope, project_root, &canonical_skills_dir)
    match plan.need {
        NativeReader                => results.push(installed:true, error:None)      // no link
        NeedsLink{agent_skills_dir} => symlink_dirs.push(agent_skills_dir)           // link
        Unsupported                 => results.push(installed:false, error:..)       // soft-fail
    }
install_universal(source_root, &canonical, &symlink_dirs, target)  // links to the SKILL-DIR
// fold report.failed into per-agent error rows (Decision 10)
```

The Master stays byte-identical (`EXCLUDE_*` unchanged); only the **link set** shrinks. Lock-write follows Decision 11 (`wrote_master || installed_any`). At global scope the canonical skills-dir is `~/.agents/skills` and the native subset is the smaller `{Codex, OpenCode, Cursor, Cline, Warp}` — discovered automatically from each descriptor's global read closure.

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
    reads_master: bool,    // informational only (NOT used to pick a bucket)
    writes_master: bool,   // informational only
    needs_link: bool,      // == matches!(need, NeedsLink)
    auto_covered: bool,    // == matches!(need, NativeReader)
    supported: bool,       // == !matches!(need, Unsupported)
}
```

**Bucketing invariant.** All five booleans are projected from the single `LinkNeed` 3-state, so exactly one of `{auto_covered, needs_link, !supported}` is true for every agent. `reads_master`/`writes_master` are exposed for diagnostics/UI copy only; **the frontend partitions on `auto_covered` and `needs_link`, never on the read/write booleans**. No agent can be silently dropped.

Handler maps `classify::classify_all` over `registry::ALL_AGENTS`, resolving the canonical **skills-dir** server-side via `universal_canonical_dir(if scope==project { Some(abs_root) } else { None })`. The DTO/endpoint is **per-scope** and requires `project_root` (resolved to absolute) when `scope=project` (mirror `ScopeParams`).

**Option B (booleans on `AgentInfo`) is REJECTED**: `list_agents` passes an empty project_root, so project-scope canonical would be wrong, and it can't express `needs_link` vs `unsupported`. Scope+project_root must be request inputs.

Mount the route in `crates/api/src/lib.rs`; DTO in `crates/api/src/dto/`.

---

## Frontend Changes (`crates/desktop`)

### 1. Delete the install-layout abstraction

- **Delete** `src/lib/install-layout.ts` (`InstallLayout`, `DEFAULT_INSTALL_LAYOUT`, `isUniversalLayout`) and `src/lib/install-layout.test.ts`. With the toggle gone there is nothing to map.
- **`src/pages/sources/index.tsx`**:
    - Remove the `install-layout` import and `ToggleButton`/`ToggleButtonGroup` from `@heroui/react`.
    - Delete the `installLayout`/`setInstallLayout` state and the toggle UI block.
    - In `installFromSource`'s `gitInstall({...})`, remove `universal: isUniversalLayout(installLayout)`.
      (Verify each line range against the current file before editing — the page is large.)
- **`src/components/import-github-skill-panel.tsx`**: `handleInstall` already sends no `universal` — no install-flag change; both surfaces produce the same body shape after the DTO change.

### 2. `GitInstallRequest.universal` — REMOVE the field

The field (`crates/api/src/dto/skill.rs:275-281`, `pub universal: Option<bool>` with `#[serde(default)]`/`#[ts(optional)]`; generated `GitInstallRequest.ts`) is `Option<bool>` where the default (absent/false) meant **copy** — now a banned behavior. Keeping a field whose default requests banned behavior is a footgun.

- **Remove** `pub universal: Option<bool>` + its doc (275-278) + `#[serde(default)]`/`#[ts(optional)]` (279-280).
- Confirm `GitInstallRequest` (`dto/skill.rs:267-282`) has **no `deny_unknown_fields`** (verified: it does not), so a legacy client still sending `"universal": true|false` parses fine (field ignored) — forward-compatible.
- **Regenerate DTOs** per the prettier workflow (MEMORY: `generate:dto` alone shows a spurious 121-file diff): run the ts-rs export, then prettier over `src/generated/`, so the only real diff is the dropped `universal?: boolean`.
- `requests/skills.ts` passes the body straight through — no field reference, safe (verify).

**Rejected (keep-ignored):** leaving a lying `universal?: boolean` in the generated surface invites a future "wire it up" that reintroduces copy.

### 3. i18n

Delete from `src/lib/locales/{en,zh-Hant,zh-Hans}.ts`: `installLayoutLabel`, `installLayoutIsolation`, `installLayoutUniversal`, `installLayoutHint` (grep each file for the keys — line numbers drift; do NOT trust hardcoded line refs here).

Add (same three files): `sourceInstallCoveredTitle`, `sourceInstallCoveredHint`, `sourceInstallLinkTargetsTitle`, `sourceInstallLinkTargetsHint`, `sourceInstallNoLinkTargets`, `agentCoveredBadge`.

### 4. Wire the coverage DTO into both surfaces

- New query options in `requests/agents.ts` + a `useSkillCoverage(scope, projectRoot)` hook joining `AvailableAgent` with the coverage DTO by id; helpers `isAutoCoveredByMaster(cov)` / `needsMasterLink(cov)` in `lib/agent-capabilities.ts`. **Re-query on scope change** (global vs project native sets differ).
- Partition the existing `isUsable && supportsSkillMutation` base set on the **server-derived** `auto_covered`/`needs_link` booleans only:
    ```ts
    const installable = availableAgents.filter(
    	(a) => a.isUsable && supportsSkillMutation(a, scope),
    );
    const autoCovered = installable.filter((a) => coverage[a.id]?.auto_covered);
    const linkTargets = installable.filter((a) => coverage[a.id]?.needs_link);
    ```
- **Import panel** (`import-github-skill-panel.tsx`): `AgentSelector` feeds from `linkTargets` only; default `selectedAgents` = first `linkTargets` (not first installable); **relax** the required-selection rule to valid when ≥1 link target selected OR `linkTargets.length === 0`; render a read-only "Already covered" chip list (or `sourceInstallNoLinkTargets` when empty).
- **Sources page** (`index.tsx`): `installAgentIds` → `linkTargetAgentIds`; an empty list is **no longer an error** (master-only install proceeds) — remove the no-agents early-return/toast on the install path. Add an inline summary "N linked / M auto-covered". The **removal** path `deleteInstalledSkillByName` must keep resolving against the **full installable set**, not the link-target subset (Locked; see Out of Scope).
- `AgentSelector` itself is unchanged; the auto-covered list is plain `Chip`s, NOT a disabled selector (keeps "these are not choices" unambiguous).

### Implementation order (each step compiles)

1. Core: land the classifier + `/skills/coverage` route + the symlink-only primitive (server goes unconditionally-universal). This MUST precede the FE flag removal so there is no window where omitting `universal` means "copy" under the old contract.
2. DTO removal + regen + prettier.
3. FE: delete toggle/state/`install-layout.ts`/test/i18n, drop the `universal:` line, wire coverage.

---

## Test Strategy (incl. Windows-CI junction tests)

### The single biggest fix

`install_layout.rs:276` declares the link test module `#[cfg(all(test, unix))] mod tests`, which keeps **every** link test off Windows CI — the junction path would ship untested. **Split** into a cross-platform base (`#[cfg(test)] mod tests`, asserting via `Linker::is_link` which works on both OSes), a nested `#[cfg(unix)] mod unix_specific` for `read_link`-relative-form assertions (junctions don't preserve relative form), and a `#[cfg(windows)] mod windows_specific` for the junction tests. Windows junctions need **no admin / no Dev Mode**, so the junction tests actually **execute** on `windows-latest` in the 3-platform `just test` gate.

### NO-COPY regression contract (strongest assertions)

- **Compile-time guarantee**: removing `UniversalInstallReport.copied_fallback` and `LinkResult::CopiedFallback` means the type system no longer admits a copy outcome. Sweep all crates (api/cli) for any pattern-match on `copied_fallback`/`CopiedFallback` before deleting — a miss fails the workspace build on all platforms.
- **T-NOCOPY** (cross-platform): after `install_universal`, assert `Linker::is_link(&link) == true`; on Windows assert it's a reparse point (`is_dir()==true && is_link==true`), never a plain dir. Then write a sentinel into the Master **after** linking and read it back **through** the link — proves a true link, not a coincidentally-identical copy.

### Windows junction (runs on windows-latest) — `#[cfg(windows)] mod windows_specific`

- **T-WIN-JUNCTION-CREATE**: install one agent **with a space in the link parent path** (covers the spaces-quoting concern); assert `linked` has it, `Linker::is_link` true, reading `link\assets\a.txt` returns Master content.
- **T-WIN-JUNCTION-DETECT (forces the junction path)**: do NOT rely on `symlink_dir` failing — if the runner has Developer Mode on, `symlink_dir` succeeds and you'd test a symlink instead. Call the extracted `pub(crate) create_junction(abs_target, link)` directly so a junction is always exercised. Assert `Linker::is_link == true` AND `symlink_metadata().file_type().is_symlink() == false` — pins the `0x0400` reparse branch that bare `is_symlink()` misses.
- **T-WIN-JUNCTION-REMOVE**: `Linker::unlink` a junction; assert link gone AND Master + files intact (proves `remove_dir`, not `remove_dir_all`).
- **T-WIN-JUNCTION-ABS-TARGET**: build the junction via `create_junction` with the recomputed **absolute** `abs_target`; assert it resolves — pins that the fallback used an **absolute** target (never `..\..`).
- **T-NONABS-TARGET-ERR** (cross-platform unit test): `install_universal` with a NON-absolute `canonical` returns `Err(LinkError::NonAbsoluteTarget)` — pins Decision 6's precondition (the previously-unasserted absolute-root invariant).

### Hard-error when neither symlink nor junction works

- **T-HARDERR** (`#[cfg(unix)]`): `set_permissions(agent_skills_dir, 0o500)` after `create_dir_all` to force EACCES on the symlink create inside it; assert the report's `failed` has the agent with a `LinkError`, the link path does NOT exist, and the Master IS present (Decision 11 — Master written first, install recorded). Use the `testing-fs-failures` skill technique; **skip under root** (`geteuid()==0`). (There is no copy fallback, so the assertion is "no link created AND Master intact AND `failed` populated" — there is no "nothing copied" to assert.)
- **T-HARDERR-WIN**: junction almost always succeeds on the runner, so prefer a **deterministic unit test** of the decision via an injectable `choose_link_outcome<F,G>(try_symlink, try_junction)` where both fakes return `Err` → `Err(LinkUnsupported)`, proving no third (copy) attempt exists. Document the choice in the test doc-comment.
- **T-HARDERR-NOPARTIAL** (both): after a forced hard error, Master intact (written first), agent link path absent.

### Idempotency / conflict / relative-vs-absolute / native (restructured base+leaf)

- **T-IDEMP** (cross-platform; from `is_idempotent_on_existing_correct_symlink`:332): install twice → second run `already_linked`, empty `linked`, link not re-created (assert stable `modified()` / unix inode). Detection via `Linker::is_link` so a Windows junction is recognized, not re-attempted or mis-flagged Conflict.
- **T-CONFLICT-REALDIR** (cross-platform; from `never_clobbers_an_existing_real_directory`:358): pre-create a real dir; assert `conflicts` has it, content preserved, not a link after.
- **T-CONFLICT-FOREIGN-LINK** (cross-platform, NEW): pre-create a symlink/junction to a DIFFERENT target; assert `conflicts`, not clobbered, still resolves to the foreign target.
- **T-REL-PROJECT / T-ABS-GLOBAL** (`#[cfg(unix)]`; keep `relative_links_use_dotdot_global_links_are_absolute`:391): asserts `read_link == "../../.agents/skills/my-skill"` (project) and `== canonical` (global). On Windows the absolute-target requirement is covered by T-WIN-JUNCTION-ABS-TARGET. Keep `relative_path_computes_minimal_dotdot` (`:424`) cross-platform (pure `Path`).
- **T-AGENTS-NATIVE-NO-LINK** (cross-platform, NEW): given a realistic install set, the report's `linked` set never contains a path inside `.agents/skills` (Master never self-links; no native agent dir gets a redundant link). The per-agent matrix lives in **classify.rs unit tests** — see below.

### Classifier unit tests (`linker/classify.rs`)

Per-scope, against **real descriptors** (the most error-prone part — a wrong scope silently drops a needed link). All comparisons against the canonical **skills-dir** (`.agents/skills`), never the skill-dir:

- Global: `{Codex, OpenCode, Cursor, Cline, Warp}` → `NativeReader`; Claude/Gemini/Copilot/Windsurf/RooCode/Mistral/… → `NeedsLink`.
- Project: the broader set (Codex/OpenCode/Cursor/Cline/Copilot/Gemini/Antigravity/Amp/Kimi/Warp) → `NativeReader`.
- **Amp/Kimi @global → `NeedsLink`** against `~/.agents/skills` (their universal read path is the XDG dir, not `~/.agents/skills`) — pins the locked nuance (asserted as truth, not deferred).
- Amp/Kimi @project → `NativeReader` (universal flag appends `project_root/.agents/skills` = canonical).
- **3-state totality**: for every registered agent at every scope, exactly one of `{NativeReader, NeedsLink, Unsupported}` (pins the bucketing invariant).
- Wire these alongside the existing `descriptor_regression.rs` style so classification can never drift from the AGENTS.md-documented native sets.
- macOS `/var`→`/private`: canonicalize BOTH sides in the comparison; add a test on a `tempfile` path under the symlinked `/tmp` root.

### NPX hash / lock golden tests stay GREEN, unchanged

- `crates/skill/tests/hash_parity_golden.rs` (CI-BLOCKING) and `npx_interop.rs` require **zero** edits — the hash is computed over the Master's files, symlink-vs-copy is irrelevant, and `EXCLUDE_FILES`/`EXCLUDE_DIRS`/traversal order are untouched. **Do not modify** `copy_dir_recursive`'s exclude lists.
- **T-MASTER-HASH-STABLE**: hash the Master before and after linking N agents → assert equal (linking must never mutate the Master).
- **T-LOCK-PARITY-LINK-VS-COPY** (NEW): write the install lock for the same skill via the new link-era path and assert the resulting lock JSON (`skillPath`, `source`, hash, schema) is byte-identical to a lock written from the same source folder in the copy-era — pins that round-trip (Decision 7) holds at the full-lock level, not just the folder hash. Since the lock is computed over the SOURCE folder (not the installed dir) in both eras, this should hold; the test makes it a regression guard.

### Removal / discovery / delete-route junction tests (`removal.rs` + `manager/skill.rs` + `discovery.rs` + API)

- Keep the existing `#[cfg(unix)]` symlink tests; add Windows-junction siblings: `execute_removal` on a junction unlinks (single reparse point) and **never** `remove_dir_all`s into the Master.
- **T-REMOVE-SKILL-PATH-JUNCTION** (NEW, `#[cfg(windows)]`): `manager/skill.rs::remove_skill_path` on a junction-referrer removes the junction and leaves the Master intact (pins the previously-missed call site).
- **T-DISCOVERY-JUNCTION-CANONICAL** (NEW, `#[cfg(windows)]`): create a junction install, run `discovery::collect_skills`, assert the discovered `Skill.canonical_path` is set (a junction is recognized as a referrer, not a copy). On Unix the symlink equivalent already passes; this pins the `is_symlink()` → `Linker::is_link` swap at `discovery.rs:49`. **(P0-A.)**
- **T-PLAN-JUNCTION-REFERRER** (NEW, `#[cfg(windows)]`): `plan_symlink_removal` over a targeted **junction** referrer schedules it for unlink (not orphaned) — pins the `removal.rs:248`/`:263` swaps. **(P0-B.)**
- **T-EXTERNAL-JUNCTION-REFERRER** (NEW, `#[cfg(windows)]`): `dir_has_external_referrer` returns `true` for an external **junction** referrer, so a shared Master with a live junction referrer is NOT removed — pins the `removal.rs:364` swap. **(P0-B.)**
- **T-DELETE-BY-PATH-JUNCTION** (NEW, `#[cfg(windows)]`): the `DELETE /skills/by-path` route (`delete_skill_by_path`) on a junction install takes the canonical-layout branch (junction recognized via `Linker::is_link`) and does not orphan it — pins the `skills.rs:380-381` swap. **(P1-E.)**

### Absolute project-root precondition test (P0-C)

- **T-REL-ROOT-ABSOLUTIZED** (cross-platform): drive a project-scope install with a **relative** `project_root` (e.g. `"./proj"`) through the route boundary and assert the resulting canonical / link target is **absolute** (and the install succeeds with no `NonAbsoluteTarget` rows) — proving the boundary absolutizes the root and/or `universal_canonical_dir` returns an absolute path. Complements `T-NONABS-TARGET-ERR`, which pins the _primitive's_ assertion; T-REL-ROOT-ABSOLUTIZED pins that the _caller_ never lets a relative root reach it.

---

## Out of Scope

- **No global setting / config file** for install mode — there is no mode anymore.
- **No retroactive conversion** of existing copy-installs (forward-only). Coverage is a pre-install planning aid; it must not trigger relinking of legacy copies.
- **No copy escape hatch** anywhere on the install path (no hidden flag, no "advanced" toggle, no FE env override).
- **No iflow copy-mode** ported.
- **No `transfer.rs` cross-agent transfer redesign** (Locked Decision 9 — stays copy-based; converting it is deferred).
- **No removal-path redesign** beyond (a) swapping `is_symlink`→`Linker::is_link` in `execute_removal` + `remove_skill_path`, and (b) keeping `deleteInstalledSkillByName` on the full installable set.
- **No Amp/Kimi global-Master relocation** — they link to `~/.agents/skills` at global per the locked classifier; moving their Master to the XDG path is a separate, out-of-scope change.
- **No `cmd.exe`-metacharacter hardening** for project roots containing `&`/`^`/`%`/parens — out of scope (sanitized skill names cannot contain them; pathological roots are unsupported).

---

## Open Questions

1. **`ConfigError::Link(LinkError)`** dedicated variant vs mapping into `ConfigError::Io`? Per Decision 10, link failures are per-agent soft-fails (string-folded into `AgentInstallResult.error`), so a `ConfigError` variant is needed ONLY if a caller chooses to hard-fail `NonAbsoluteTarget` rather than soft-fail it. _Recommend a dedicated `ConfigError::Link(LinkError)` variant for clean UI messaging; default behaviour stays per-agent soft-fail._
2. **Migration shim**: keep `#[deprecated] pub use linker as install_layout;` for one release, or update all call sites in one commit? _Recommend single-commit (in-crate, low risk)._
3. **CLI `--universal`**: hidden no-op for one release (safe) vs remove (breaks `--universal` callers with unknown-arg)? _Recommend hidden no-op._
4. **Attribution placement**: `UPSTREAM.md` MIT-borrow row vs a dedicated `THIRD-PARTY.md`/`NOTICE` — see Attribution section. _Recommend a clearly-labelled third-party-borrow row in `UPSTREAM.md` distinct from the fork-sync ledger._
5. **`classify_all` shape**: caller resolves `master_skills_dir` via `universal_canonical_dir` first (current shape) vs the fn also returning the resolved master / "needs creating" flag. _Recommend caller-resolves (keeps `classify.rs` free of `dirs`/scope-resolution policy)._
6. **`conflicts` bucket**: distinguish foreign-symlink vs real-dir for the UI, or single bucket? _Recommend single bucket for v1; the report separates them internally if needed later._
7. **mklink /J quoting confirmation**: confirm the exact `Command` arg vector (`cmd`, `/C`, `mklink`, `/J`, `<link>`, `<abs target>`) handles spaces (rely on `Command`'s per-arg quoting, NOT a joined string). T-WIN-JUNCTION-CREATE on a spaced path is the regression guard.

> Resolved (no longer open): **manual-create convergence** (Locked Decision 1 + the `add_skill` → `add_skill_universal` rewiring already require manual creation to write a Master + link — no longer an open question; the only residual is the forward-only migration note that legacy agent-local manual-creates are not retroactively converted, per Decision 2), the classifier skills-dir/skill-dir contract (pinned in "The Model" + "Public API"), the hard-error policy (Decision 10) and silent-no-op gap (Decision 11), `remove_skill_path` rewiring, the `transfer.rs` contradiction (Decision 9 / Out of Scope), `install_skill` scope (now IN scope), the absolute-target precondition (Decision 6 + `NonAbsoluteTarget` + the P0-C absolutization requirement), Amp/Kimi @global (locked NeedsLink), the coverage DTO bucketing invariant, the lock-helper survivor question, and the `GitInstallRequest` `deny_unknown_fields` check (verified absent).

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

Record the borrow in `UPSTREAM.md` as an **MIT-attributed third-party code borrow**, explicitly distinct from the `AkaraChen/aghub` fork-sync ledger (this is a third-party MIT borrow, not a fork-sync row). Note the **behaviour change** so it surfaces in release notes: a Windows environment that previously "worked" via the copy fallback (no Dev Mode AND `mklink /J` blocked) now records a **per-agent hard error** (the Master is still materialized, but that agent gets no link). If `UPSTREAM.md`'s structure makes a third-party borrow awkward, a `NOTICE`/`THIRD-PARTY.md` file is the alternative (Open Question 4).
