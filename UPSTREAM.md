# Upstream Sync Log

A living, **complete** record of how this fork has handled every upstream-only
commit — ported, deferred, skipped, or N/A — so nothing is silently forgotten.
Append/maintain this on every sync. Provenance also lives in commit messages, but
this is the discoverable, durable home.

## ⚠️ Two different "upstreams" — don't confuse them

- **Fork upstream — [`AkaraChen/aghub`](https://github.com/AkaraChen/aghub)**: the
  repo this fork descends from. **This file tracks it.** `git remote` name: `upstream`.
- **Skills-ecosystem upstream — vercel-labs `npx skills` package**: the skill CLI we
  stay round-trip compatible with. Documented by the `npx-skills-contract` and
  `upstream-skills-flow` skills + `CONTEXT.md` ("Skill folder hash" = its GitHub tree
  SHA). **NOT this file.** When those skills say "upstream" they mean the npx package.

## Fork baseline

- This fork ships its **own** version line (currently `v2.1.x`), independent of upstream.
- `git remote`: `upstream = https://github.com/AkaraChen/aghub.git`.
- **merge-base** with upstream: `ca48d93`.
- **Last full review**: upstream `main` @ `714b971` — **50 upstream-only commits** since
  `ca48d93` (2026-06). Every one is dispositioned below.

## ✅ Ported

| When                   | Upstream           | Our commit | Crate      | What                                                                                                                            |
| ---------------------- | ------------------ | ---------- | ---------- | ------------------------------------------------------------------------------------------------------------------------------- |
| 2026-06 (v2.1.3/2.1.4) | `3ad9f1c`          | `b5a7857`  | core       | copy-mode skill import preserves the full source tree (scripts/refs/assets/body), not just a synthesized `SKILL.md`             |
| 2026-06                | `52a938c`          | `37ca1e7`  | cc-plugins | tarball extraction path validation (zip-slip / `..` / absolute / symlink / hardlink)                                            |
| 2026-06                | `ffeec65`          | `a1ee462`  | agents     | Codex sub-agent I/O hardening (O_NOFOLLOW read + staging-temp/rename write)                                                     |
| 2026-06                | `91bd12d` (subset) | `1ef6980`  | api        | `/skills/content` + `/skills/tree` constrained to allow-listed roots (delete-by-path already covered by our `assert_contained`) |
| 2026-06                | `2f13f0c`          | `30856f7`  | api        | git-scan credentials restricted to `github.com`                                                                                 |

## ✅ Our hardening beyond upstream (found via review / CI, not in upstream)

| Our commit          | Crate           | What                                                                                                                                                                                        |
| ------------------- | --------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `cac06b6`           | cc-plugins      | build tarball target from the **canonical** root — fixes macOS/Windows `/var`→`/private` canonicalize-prefix bug that broke extraction (Linux `/tmp` isn't a symlink, so it passed locally) |
| `a455f4c`           | cc-plugins      | component-wise, symlink-rejecting extraction dir creation (follow-up to `37ca1e7`)                                                                                                          |
| `408c787` `be9c465` | api/core/agents | accept 403-or-404 for refused reads; `#[cfg(unix)]` tests simulating the macOS canonicalize-prefix gotcha on Linux (symlinked tmp dir)                                                      |
| `b53fb2f` `e187cff` | ci              | release 3-platform test gate + per-tag concurrency group; `just preflight`                                                                                                                  |

## ⏸️ Deferred — security-relevant, intentionally not yet

| Upstream                                                              | Why not yet                                                                                                                                                                                       |
| --------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `43acac8` API token auth (`ApiAuth`) + `109e343` `TrustedLocalOrigin` | Collides with this fork's multi-connection / SSH-remote server model (token injection over the SSH tunnel). The API is same-host-trusted for now — see `crates/api/AGENTS.md` CORS note. Revisit. |
| (CORS tightening — part of the same change)                           | Tied to the token-auth work above; deferred together.                                                                                                                                             |
| `1858167` validate GitHub marketplace manifests (cc-plugins)          | Low blast radius; not yet ported. Reasonable next port.                                                                                                                                           |
| `9ba3a64` harden deep-link MCP import review (desktop consent UI)     | Frontend UX guard; not yet ported.                                                                                                                                                                |

## ⏭️ Skipped — product choices (revisit per roadmap)

| Upstream                                          | Note                                                     |
| ------------------------------------------------- | -------------------------------------------------------- |
| `324b2ba` inference: agent provider model routing | Feature; self-contained but not a product priority here. |
| `3af4a98` desktop: PostHog analytics              | We don't want bundled analytics by default.              |
| `e8f94ab` desktop: 'auto-check updates' setting   | Optional UX; skipped.                                    |
| `bcba0fd` Windows tray + autostart                | Platform UX; skipped.                                    |

## 🧪 Upstream tests — covered by our own, not separately ported

| Upstream                                | Note                                                                                    |
| --------------------------------------- | --------------------------------------------------------------------------------------- |
| `f04f142` route-level mutation coverage | We wrote our own route-level tests alongside the ported fixes (`crates/api` mod tests). |
| `f34a95c` harden route mutation tests   | Same — superseded by our test coverage for the ported routes.                           |

## 🖥️ Desktop / release polish — not ported (low priority; verify before porting, some may already exist independently)

| Upstream                                                             |
| -------------------------------------------------------------------- |
| `88dc33e` keep starred skills on top across ungrouped lists          |
| `1cf2386` stop console windows flashing on Windows subprocess spawns |
| `99d9b9d` keep market table installs header single-line              |
| `da760f4` show real install counts + sort market by installs         |
| `426628e` sort enabled plugins on top of plugin list                 |
| `398c7e8` validate homebrew release tag (CI)                         |

## 🔧 Upstream-internal — N/A to this fork

| Upstream                        | Note                                                  |
| ------------------------------- | ----------------------------------------------------- |
| `b014ccb` init: skill           | Upstream repo housekeeping.                           |
| `e1080c2` chore: release v1.2.2 | Upstream's own release commit (our line is separate). |

## 📦 Dependency bumps — not individually tracked

The remaining ~25 commits are `chore(deps*)` version bumps plus `d5668ed`
(KIT-43 catalog centralization) and `5b44a26` (align react-dom with react). We
bump dependencies on our own cadence; not mirrored one-for-one. Re-evaluate in bulk
during a sync if a security advisory or a needed feature lands upstream.

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
git fetch upstream main
git log --no-merges --oneline ca48d93..upstream/main     # full upstream-only list
git show <sha>                                            # inspect a candidate
# Port (reuse our infra where it diverges) → run `just preflight` → push → CI green
# on all 3 platforms → then:
#   1. add a row to the ✅ Ported table (upstream SHA ↔ our commit)
#   2. move/adjust its row out of the other sections
#   3. bump "Last full review" + merge-base SHAs above
```
