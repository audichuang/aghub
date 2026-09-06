# Upstream Sync Log

A living, **complete** record of how this fork has handled every upstream-only
commit — ported, already-present, deferred, skipped, or N/A — so nothing is
silently forgotten. Maintain this on every sync. Provenance also lives in commit
messages, but this is the discoverable, durable home.

## ⚠️ Two different "upstreams" — don't confuse them

- **Fork upstream — [`AkaraChen/aghub`](https://github.com/AkaraChen/aghub)**: the
  repo this fork descends from. **This file tracks it.** On this clone its `git
remote` is named **`origin`** (NOT `upstream`); our own publish remote is
  **`fork`** — push commits and release tags to `fork`, never to `origin`.
- **Skills-ecosystem upstream — vercel-labs `npx skills` package**: the skill CLI we
  stay round-trip compatible with. Documented by the `npx-skills-contract` and
  `upstream-skills-flow` skills + `CONTEXT.md` ("Skill folder hash" = its GitHub tree
  SHA). **NOT this file.** When those skills say "upstream" they mean the npx package.

## Fork baseline

- This fork ships its **own** version line (currently `v2.1.x`), independent of upstream.
- `git remote`: `origin = https://github.com/AkaraChen/aghub.git` (upstream);
  `fork = https://github.com/audichuang/aghub.git` (ours — push here).
- **merge-base** with upstream: `ca48d93`.
- **Last full review**: upstream `main` @ `c1801ede` (2026-07-16) — **97 further
  upstream-only commits** since `01bf2d57`. Tally: **1 ported** (TS7, shipped
  v2.7.0) · ~75 skipped
  (desktop "resource-list interaction v2" + library-page redesign — built on the
  agent-hub-revamp #282 UI this fork never adopted; `@dnd-kit/core`,
  `use-list-selection`, `use-resource-groups`, `lib/store/groups`,
  `agent-coverage-matrix` etc. don't exist here) · ~19 dependency bumps · 1 CI
  bump (`actions/upload-artifact` 6→7) · 1 TS7 upgrade. **Zero Rust-backend
  commits** in the whole batch — the range touches only `crates/desktop` +
  lockfiles + one `.github` action. Notes: the CI bump is **N/A here** (our
  `release.yml` uploads via `softprops/action-gh-release@v3`, never uses
  `actions/upload-artifact`); the **TS7 upgrade is the only cleanly portable item**
  (independent of the UI, swaps `tsc`→native `@typescript/native` tsgo). UI is a
  rewrite against the #282 architecture, not a port (product call, not a sync).
  Borrowable UI _ideas_ (reimplement, don't cherry-pick): agent-coverage-matrix
  view + right-click context menus — our `use-multi-select` already does
  shift-range/bulk selection, so those parts aren't new.
- **Prior review**: upstream `main` @ `01bf2d57` (2026-07) — **55 further
  upstream-only commits** since `714b971`. Tally of that batch:
  2 ported (backend fixes below) · 1 deferred (usage-monitoring feature, product
  call) · ~22 skipped (desktop UI built on the agent-hub-revamp #282 architecture
  this fork never adopted — `global-search`/`agent-overview-card`/
  `resource-page-toolbar` don't exist here) · ~30 dependency bumps.
- **Earlier review**: upstream `main` @ `714b971` — 52 upstream-only commits since
  `ca48d93` (2026-06). Tally: 8 ported · 5 already-present · 2 deferred (security)
  · 3 skipped (product) · 2 upstream-tests · 1 partial · 2 upstream-internal · 29
  dependency bumps. Every one is dispositioned below.

## ✅ Ported from upstream (active backport work)

| Upstream           | Our commit  | Crate      | What                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                              |
| ------------------ | ----------- | ---------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `3ad9f1c`          | `b5a7857`   | core       | copy-mode skill import preserves the full source tree (scripts/refs/assets/body), not just a synthesized `SKILL.md`                                                                                                                                                                                                                                                                                                                                                                                               |
| `52a938c`          | `37ca1e7`   | cc-plugins | tarball extraction path validation (zip-slip / `..` / absolute / symlink / hardlink)                                                                                                                                                                                                                                                                                                                                                                                                                              |
| `ffeec65`          | `a1ee462`   | agents     | Codex sub-agent I/O hardening (O_NOFOLLOW read + staging-temp/rename write) — **codex `.toml` reader only** (`agents/codex/sub_agent.rs`); the generic `.md` sub-agent path was hardened separately (see PR #209 row)                                                                                                                                                                                                                                                                                             |
| `91bd12d` (subset) | `1ef6980`   | api        | `/skills/content` + `/skills/tree` constrained to allow-listed roots (delete-by-path already covered by our `assert_contained`)                                                                                                                                                                                                                                                                                                                                                                                   |
| `2f13f0c`          | `30856f7`   | api        | git-scan credentials restricted to `github.com`                                                                                                                                                                                                                                                                                                                                                                                                                                                                   |
| `398c7e8`          | `31da70c`   | ci         | validate Homebrew release tag format in `release.yml` before touching the tap (also fixes the prior `aghub vv<tag>` commit-message double-v)                                                                                                                                                                                                                                                                                                                                                                      |
| `1858167`          | `b87566b0`  | cc-plugins | marketplace `github_owner_repo` only resolves owner/repo when the source host is genuinely GitHub (filter on `RemoteSourceType::Github`) — a non-github plugin URL no longer mis-resolves to a github raw-manifest fetch. Ports verbatim; our `aghub-git` already exposes `source_type`                                                                                                                                                                                                                           |
| `9ba3a64`          | `b87566b0`  | desktop    | deep-link MCP import review hardening — per-field transport review (command/args/env or url/headers/timeout) + a warning Alert and a mandatory consent checkbox that gates Install for stdio (executable) MCPs. `aghub://` links are an external attack surface                                                                                                                                                                                                                                                   |
| `PR #209` (open)   | `9b54de54`  | agents     | generic `.md` sub-agent I/O symlink hardening (`sub_agents.rs`: Claude/OpenCode path) — reject symlinked path components + non-regular files on read, staging-temp + `create_new` + rename on write. Ported verbatim (portable `symlink_metadata`, not `a1ee462`'s unix-only O_NOFOLLOW). **Not a main commit** — surfaced by upstream-PR review, so not in the 52-commit tally                                                                                                                                   |
| `b95e1f61`         | (this sync) | agents     | correct config paths for trae / jetbrains-ai / augmentcode — the descriptors declared MCP/skill files these agents never read (invented home dotfolders). augmentcode → real `~/.augment/settings.json` (global only, no project file, drop `.augmentcode` marker); trae → drop unattested global MCP/skills, keep project `.trae/`, `global_data_dir` = OS config dir; jetbrains-ai → MCP unsupported (GUI-only). Our descriptors were still upstream's pre-fix state; cherry-picked clean with its test updates |
| `e7628f2a`         | (this sync) | core       | `ConfigManager::validate` spawns the agent CLI via `validate_command`; from the console-less desktop app on Windows that flashed a console window. Set `CREATE_NO_WINDOW` (new `pub(crate)` const in `lib.rs`, `#[cfg(windows)]`), mirroring the api/cc-plugins/skills-linker convention. Cherry-picked clean                                                                                                                                                                                                     |
| `25a18462`         | `2e636c90`  | desktop    | TS7 native compiler — desktop `build`/`typecheck` swap `tsc` → `@typescript/native` (npm alias `typescript@^7`, Go-based tsgo); root peerDep `typescript` → `^7`. Hand-applied (not cherry-picked); parity vs classic tsc verified. Shipped in v2.7.0                                                                                                                                                                                                                                                             |

## ✅ Already present in this fork (implemented earlier, independent of the v2.1.3/2.1.4 work)

Verified by code inspection (`git show` on both sides), not titles. These are NOT
exact patch-id matches — they were re-implemented in our own commits:

| Upstream  | Our commit | What                                                                            |
| --------- | ---------- | ------------------------------------------------------------------------------- |
| `bcba0fd` | `2d420a1`  | Windows tray + autostart (tray setup + autostart plugin + UI toggle)            |
| `1cf2386` | `bff88ba`  | stop console windows flashing on Windows subprocess spawns (`CREATE_NO_WINDOW`) |
| `99d9b9d` | `ac7a455`  | market table installs header single-line                                        |
| `da760f4` | `fd07e07`  | show real install counts + sort market by installs                              |
| `426628e` | `199a280`  | sort enabled plugins on top of the plugin list                                  |

## ✅ Our hardening beyond upstream (found via review / CI, not in upstream)

| Our commit          | Crate           | What                                                                                                                                                                                                                                                                                                                             |
| ------------------- | --------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `cac06b6`           | cc-plugins      | build tarball target from the **canonical** root — fixes macOS/Windows `/var`→`/private` canonicalize-prefix bug that broke extraction (Linux `/tmp` isn't a symlink, so it passed locally)                                                                                                                                      |
| `a455f4c`           | cc-plugins      | component-wise, symlink-rejecting extraction dir creation (follow-up to `37ca1e7`)                                                                                                                                                                                                                                               |
| `408c787` `be9c465` | api/core/agents | accept 403-or-404 for refused reads; `#[cfg(unix)]` tests simulating the macOS canonicalize-prefix gotcha on Linux (symlinked tmp dir)                                                                                                                                                                                           |
| `b53fb2f` `e187cff` | ci              | release 3-platform test gate + per-tag concurrency group; `just preflight`                                                                                                                                                                                                                                                       |
| `9b54de54`          | core            | `transfer.rs` batch copy-delete now routes `remove_dir_all` through a containment guard (`allowed_skill_roots` + `assert_contained`), mirroring `manager::skill` removal — closes an unguarded `remove_dir_all` (canonicalize-escape). Inspired by upstream PR #216's principle (its literal "reconcile" fn does not exist here) |

## ⏸️ Deferred — security-relevant, intentionally not yet

| Upstream                                                              | Why not yet                                                                                                                                                                                       |
| --------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `43acac8` API token auth (`ApiAuth`) + `109e343` `TrustedLocalOrigin` | Collides with this fork's multi-connection / SSH-remote server model (token injection over the SSH tunnel). The API is same-host-trusted for now — see `crates/api/AGENTS.md` CORS note. Revisit. |
| (CORS tightening — part of the same change)                           | Tied to the token-auth work above; deferred together.                                                                                                                                             |

## ⏭️ Skipped — product choices (revisit per roadmap)

| Upstream                                          | Note                                                                         |
| ------------------------------------------------- | ---------------------------------------------------------------------------- |
| `324b2ba` inference: agent provider model routing | Feature; self-contained but not a product priority here.                     |
| `3af4a98` desktop: PostHog analytics              | We don't want bundled analytics by default (no PostHog symbols in our tree). |
| `e8f94ab` desktop: 'auto-check updates' setting   | Optional UX; skipped.                                                        |

## 🧪 Upstream tests — covered by our own, not separately ported

| Upstream                                | Note                                                                                    |
| --------------------------------------- | --------------------------------------------------------------------------------------- |
| `f04f142` route-level mutation coverage | We wrote our own route-level tests alongside the ported fixes (`crates/api` mod tests). |
| `f34a95c` harden route mutation tests   | Same — superseded by our test coverage for the ported routes.                           |

## 🟡 Partially present — revisit if needed

| Upstream                                                    | Status                                                                                                         |
| ----------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------- |
| `88dc33e` keep starred skills on top across ungrouped lists | Only partially similar; our code still keeps single-item / unknown ungrouped lists separate. Not a full match. |

## 🔧 Upstream-internal — N/A to this fork

| Upstream                        | Note                                                  |
| ------------------------------- | ----------------------------------------------------- |
| `b014ccb` init: skill           | Upstream repo housekeeping.                           |
| `e1080c2` chore: release v1.2.2 | Upstream's own release commit (our line is separate). |

## 📦 Dependency bumps — not individually tracked (29)

27 `chore(deps*)` version bumps + `d5668ed` (KIT-43 catalog centralization) +
`5b44a26` (align react-dom with react). We bump dependencies on our own cadence;
not mirrored one-for-one. Re-evaluate in bulk during a sync if a security advisory
or a needed feature lands upstream.

## Fork-only (not present in upstream AkaraChen/aghub)

- **Hermes agent** (2026-07-14): Nous Research Hermes Agent — global-only, YAML
  MCP at `~/.hermes/config.yaml`. Spec:
  `docs/specs/2026-07-14-hermes-agent-support.md`. Does not exist upstream.
- **Oh My Pi (`omp`) agent** (2026-09-06): `can1357/oh-my-pi`, a fork of pi.
  `json_map` MCP on the default `type` tag with a native `enabled` bool, at
  `~/.omp/agent/mcp.json` / `.omp/mcp.json`; skills at `.omp/agent/skills` +
  `~/.agents/skills` (global) and `.omp/skills` + `.agents/skills` (project).
  Does not exist upstream.
- **Coverage matrix page + MCP bulk-manage-agents** (2026-07-16, v2.7.0):
  desktop `/coverage` grid (skill·mcp × agent, click-to-reconcile, name filter)
  and generalizing the skill bulk-manage-agents dialog to MCP. Spec:
  `docs/specs/2026-07-16-desktop-coverage-matrix.md`. Fork-original — the
  upstream `agent-coverage-matrix.tsx` is a different thing (≈ our existing skill
  bulk dialog), deliberately NOT ported (see the c1801ede review above).

## Fork-divergence adaptations (why our ports are not verbatim copies)

- **Reuse, don't duplicate**: ported the _protection_, but reused our existing
  `install_layout` (copy) and `aghub_core::skills::removal::{allowed_skill_roots,
assert_contained}` instead of upstream's parallel `copy_dir_recursive` / `canonical_*`
  helpers — one definition, no drift.
- **Universal-install symlinks**: content/tree must still list `.claude/skills/foo →
.agents/skills/foo` symlinks (a real feature here), so we containment-check the symlink
  target against the allow-listed roots instead of upstream's blanket reject.
- **Host-scoped credentials**: the github.com guard applies to the _explicit_ credential
  path + a session same-host check, NOT a blanket rule — preserves this fork's host-scoped
  private-repo scanning (`resolve_token_for_source`).

## Doing the next sync

```bash
git fetch origin main
git log --no-merges --oneline ca48d93..origin/main       # full upstream-only list + count
git log --no-merges --cherry-pick --right-only ca48d93...origin/main     # not-yet-equivalent
git show <sha>                                            # inspect a candidate
# For "is it already here?" check our own history by content, not by title:
git log --oneline -S '<distinctive snippet>' -- <path>
# Port (reuse our infra where it diverges) → `just preflight` → push → CI green
# on all 3 platforms → then update this file:
#   1. add a row to ✅ Ported (upstream SHA ↔ our commit) OR ✅ Already-present
#   2. remove it from the other sections
#   3. bump "Last full review" SHA + the tally count above
```

### Triage judgment (how to spend the time, learned from prior syncs)

- **Filter deps first, then bucket by crate.** Drop the `chore(deps*)` bumps (~half
  the batch) up front. Of what's left, the high-value ports are the ones touching
  the **Rust backend** (`agents`/`core`/`api`/`cli`); `fix(desktop)` commits are
  usually not.
- **Our desktop has diverged hard — check the file exists before porting UI.** Most
  upstream `fix(desktop)`/`feat(desktop)` work builds on the agent-hub-revamp (#282)
  UI this fork never adopted. Before attempting a desktop port, confirm the target
  file/component even exists here (`ls`/grep the path from the diff); if it doesn't,
  it's a rewrite against a different UI, not a port — skip unless we want the feature.
- **Confirm "already fixed here?" by reading OUR current code, not the title.** A
  clean `git cherry-pick -n <sha>` that auto-merges is strong evidence our side was
  still the pre-fix state; if it conflicts, our code already diverged — inspect before
  resolving.
- **Port mechanics:** `git cherry-pick -n -x <sha>` (the `-x` records provenance) →
  run the affected test suites (the fix's own test updates come along and should go
  green) → **split into one commit per upstream SHA** by passing explicit file lists
  to `git commit`, so each port keeps its own provenance line.
- **Two standing gotchas:** push to **`fork`**, never `origin` (= upstream); and any
  `#[cfg(windows)]` item must carry the same `cfg` as its use site, or Windows CI
  clippy goes red on merge (unused-const / dead-code) even though local Linux is green.
