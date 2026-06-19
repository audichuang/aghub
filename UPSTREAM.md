# Upstream Sync Log

A living record of what this fork ports (and deliberately skips) from its **fork
upstream**. Append a row per sync — provenance also lives in commit messages, but
this is the discoverable, durable home.

## ⚠️ Two different "upstreams" — don't confuse them

- **Fork upstream — [`AkaraChen/aghub`](https://github.com/AkaraChen/aghub)**: the
  repo this fork descends from. **This file tracks it.**
- **Skills-ecosystem upstream — vercel-labs `npx skills` package**: the skill CLI we
  stay round-trip compatible with. Documented by the `npx-skills-contract` and
  `upstream-skills-flow` skills + `CONTEXT.md` ("Skill folder hash" = its GitHub tree
  SHA). **NOT this file.** When those skills say "upstream" they mean the npx package.

## Fork baseline

- This fork ships its **own** version line (currently `v2.1.x`), independent of upstream.
- `git remote`: `upstream = https://github.com/AkaraChen/aghub.git`.
- **merge-base** with upstream: `ca48d93`. Upstream `main` at last review: `714b971` (2026-06).

## Ported from AkaraChen/aghub

| When                   | Upstream           | Our commit | Crate      | What                                                                                                                            |
| ---------------------- | ------------------ | ---------- | ---------- | ------------------------------------------------------------------------------------------------------------------------------- |
| 2026-06 (v2.1.3/2.1.4) | `3ad9f1c`          | `b5a7857`  | core       | copy-mode skill import preserves the full source tree (scripts/refs/assets/body), not just a synthesized `SKILL.md`             |
| 2026-06                | `52a938c`          | `37ca1e7`  | cc-plugins | tarball extraction path validation (zip-slip / `..` / absolute / symlink / hardlink)                                            |
| 2026-06                | `ffeec65`          | `a1ee462`  | agents     | Codex sub-agent I/O hardening (O_NOFOLLOW read + staging-temp/rename write)                                                     |
| 2026-06                | `91bd12d` (subset) | `1ef6980`  | api        | `/skills/content` + `/skills/tree` constrained to allow-listed roots (delete-by-path already covered by our `assert_contained`) |
| 2026-06                | `2f13f0c`          | `30856f7`  | api        | git-scan credentials restricted to `github.com`                                                                                 |

## Our hardening beyond upstream (found via review / CI, not in upstream)

| Our commit          | Crate           | What                                                                                                                                                                                        |
| ------------------- | --------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `cac06b6`           | cc-plugins      | build tarball target from the **canonical** root — fixes macOS/Windows `/var`→`/private` canonicalize-prefix bug that broke extraction (Linux `/tmp` isn't a symlink, so it passed locally) |
| `a455f4c`           | cc-plugins      | component-wise, symlink-rejecting extraction dir creation (follow-up to `37ca1e7`)                                                                                                          |
| `408c787` `be9c465` | api/core/agents | accept 403-or-404 for refused reads; `#[cfg(unix)]` tests simulating the macOS canonicalize-prefix gotcha on Linux (symlinked tmp dir)                                                      |
| `b53fb2f` `e187cff` | ci              | release 3-platform test gate + per-tag concurrency group; `just preflight`                                                                                                                  |

## Fork-divergence adaptations (why our port is not a verbatim copy)

- **Reuse, don't duplicate**: ported the _protection_, but reused our existing
  `install_layout` (copy) and `aghub_core::skills::removal::{allowed_skill_roots,
assert_contained}` instead of upstream's parallel `copy_dir_recursive` /
  `canonical_*` helpers — one definition, no drift.
- **Universal-install symlinks**: content/tree must still list `.claude/skills/foo →
.agents/skills/foo` symlinks (a real feature here), so we containment-check the
  symlink target against the allow-listed roots instead of upstream's blanket reject.
- **Host-scoped credentials**: the github.com guard applies to the _explicit_
  credential path + session same-host check, NOT a blanket rule — preserves this
  fork's host-scoped private-repo scanning (`resolve_token_for_source`).

## Deliberately NOT ported (and why)

| Upstream                                                                                                             | Why skipped                                                                                                                                                                                        |
| -------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `43acac8` API token auth (`ApiAuth`) + `109e343` `TrustedLocalOrigin`                                                | Collides with this fork's multi-connection / SSH-remote server model (token injection over the SSH tunnel). Deferred — see `crates/api/AGENTS.md` CORS note. The API is same-host-trusted for now. |
| CORS tightening                                                                                                      | Tied to the token-auth work above; deferred together.                                                                                                                                              |
| `1858167` marketplace manifest host validation                                                                       | Lower priority; not yet ported.                                                                                                                                                                    |
| `9ba3a64` deep-link MCP import consent UI                                                                            | Not yet ported.                                                                                                                                                                                    |
| `324b2ba` inference model routing, `3af4a98` PostHog, `e8f94ab` auto-check updates, `bcba0fd` Windows tray/autostart | Product choices — intentionally skipped, revisit per roadmap.                                                                                                                                      |
| dependency bumps                                                                                                     | Handled separately from feature/security syncs.                                                                                                                                                    |

## Doing the next sync

```bash
git fetch upstream main
git log --oneline ca48d93..upstream/main          # what's new upstream
git show <sha>                                     # inspect a candidate
# port (reuse our infra where it diverges), test on 3 platforms via CI,
# then append a row above + update merge-base/last-review SHAs.
```
