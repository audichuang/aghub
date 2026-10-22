---
name: releasing-aghub
description: Runbook for cutting, verifying and repairing a desktop + CLI release of the audichuang/aghub fork (tag-driven release.yml, `just release`, `latest.json`, Homebrew tap) and its versioning model. Use when shipping or tagging a release (發版), when a Release run fails or a main `ci.yml` flake blocks the tag, or on version questions like a `-dev` `aghub-cli --version` or `just bump`.
---

# Releasing aghub (this fork)

## Normal forward release: one command

```bash
just release X.Y.Z          # add --yes to skip the confirm
just release verify X.Y.Z   # re-check a published release (read-only)
```

**Before running it** (the script does neither): run `just preflight` (the
pre-push hook runs no tests). `preflight` is Linux-only, so while the last
review round runs, push the branch to `fork` and run
`gh workflow run ci.yml --repo audichuang/aghub --ref <branch>` (a feature
branch, never `main`: a dispatch there can cancel the push run `release.sh`
waits on) to get the macOS/Windows test matrix early. That run does not count
for the release — `release.sh` waits for the push run on `main` — but a
Windows-only failure otherwise surfaces only during that wait, one CI cycle
later. If the release includes an upstream
(AkaraChen/aghub) port, add its row to `UPSTREAM.md` and bump "Last full
review" — that log, not the commit message, is the durable record.

You pick the version and confirm go/no-go — a tag is a real public release
(Homebrew + the auto-update endpoint). `scripts/release.sh` then: validates the
version, **pushes to `fork` (origin = upstream AkaraChen — never)**, waits up to
40 min for HEAD's `ci.yml` to go green, tags `vX.Y.Z`, watches `release.yml`
(when no job concluded `failure` — macOS jobs that no hosted runner acquired
are cancelled after ~15 min under a capacity squeeze — it reruns the unfinished
jobs with `--failed`, pausing 15 min between up to 6 tries; tune with
`RELEASE_FLAKE_RETRIES` / `RELEASE_FLAKE_PAUSE_SECS`), then runs
`scripts/verify-release.sh` in full mode.

**Interrupted? Re-run the same command.** The release runs on GitHub; killing the
script only stops the local watch. Once `vX.Y.Z` is on the fork at HEAD — or at
an ancestor of HEAD, when main moved on during a long wait (those newer commits
are not in the release) — `just release X.Y.Z --yes` skips push/CI/tag and
resumes at watch + verify.
Never hand-tag to "finish" it.

## Pipeline

`v*` tag → `verify-ci` → `changelog` (creates the Release as a **draft**) →
`build-tauri` + `build-cli` (3 targets each: macOS arm64, Linux x64, Windows x64; upload into the draft) → `publish`
(`verify-release.sh --pre-publish` on the draft, then makes it public/latest) →
`publish-homebrew` (bumps the `aghub` cask and `aghub-cli` formula together).

- **Nothing is public until `publish` passes**, so a failed build leaves a draft
  and users keep the previous version. Every `softprops/action-gh-release` step
  must pass `draft: true` — without it, finding the draft makes softprops
  publish it. Why: `release.yml` header comment.
- **`verify-ci` does not re-run tests**: it requires a green **push**-event
  `ci.yml` run for the tagged commit (PR runs are not trusted — path filters can
  skip `test`). A tag on a commit not green on main fails fast with no artifacts.
- Checks belong in `verify-release.sh` (shared by `publish` and `release.sh`),
  never as a one-off grep in a runbook — prose checks get skipped by the next
  hand-pushed tag.
- Users install with `brew install --cask audichuang/tap/aghub`
  (CLI: `audichuang/tap/aghub-cli`).

## Versioning

**One version, two artifacts, always shipped together** — `publish` needs both
build jobs, and the cask + formula are bumped in one job. Never ship one without
the other.

- **Release builds**: the tag is the truth. CI seds it into `Cargo.toml`,
  `crates/desktop/package.json`, `crates/desktop/src-tauri/tauri.conf.json` and
  exports `AGHUB_RELEASE_VERSION` so the CLI self-reports exactly `X.Y.Z`
  (a smoke test asserts it). Do NOT hand-edit manifests for a release.
- **Local builds**: `crates/cli/build.rs` uses `AGHUB_RELEASE_VERSION`, else
  `git describe --tags --dirty=-dev` (`2.1.6`, `2.1.6-3-gabc1234`, `2.1.6-dev`
  when dirty), else `CARGO_PKG_VERSION`. No `--always`, so a bare SHA never
  shadows the version.
- **The committed manifest version is a placeholder** that lags. `just bump <ver>`
  only syncs the three manifests locally (e.g. before a desktop dev run); it does
  not drive releases.
- **Choosing the number**: independent monotonic semver, ≥ the highest fork tag.
  A changed `/api/v1` response SHAPE (not an added optional field) needs a MINOR
  bump: `aghub_remote::ssh::is_version_compatible` pairs any equal major.minor,
  so a patch would let an older desktop talk to a newer VM aghub-api.
  `git cliff --unreleased` marks such commits `[**breaking**]`.

## Manual cut (only when `just release` can't be used)

```bash
# 0. Same prerequisites as above (UPSTREAM.md row, just preflight).
just preflight
SHA=$(git rev-parse HEAD)   # pin it: main may move while CI runs
git push fork "$SHA:refs/heads/main"
gh run watch <ci-run-id> --repo audichuang/aghub --exit-status   # MUST be green
git tag vX.Y.Z "$SHA" && git push fork vX.Y.Z
gh run list  --repo audichuang/aghub --workflow release.yml --limit 1
gh run watch <run-id> --repo audichuang/aghub --exit-status
just release verify X.Y.Z
```

`just preflight` covers your platform only; macOS/Windows behaviour comes from
the CI matrix, or from a Linux test that simulates it (e.g. a symlinked temp dir
for macOS `/var` → `/private`).

## Invariants

- **`tauri.conf.json` `pubkey`** pairs with the `TAURI_SIGNING_PRIVATE_KEY`
  secret and must never change once a build ships (installed apps could no
  longer auto-update); `endpoints` must point at this repo. The keypair is set
  once, never regenerated.
- Required secrets: `TAURI_SIGNING_PRIVATE_KEY`,
  `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`, `HOMEBREW_TAP_TOKEN` (PAT with
  Contents:write on `audichuang/homebrew-tap`; `GITHUB_TOKEN` can't reach it).
- **Tap layout: `Formula/aghub-cli.rb`, `Casks/aghub.rb`.** Homebrew ignores a
  formula at the tap root and silently keeps users on their installed version.

## When something fails

A run failed, `just release` aborted, `publish` / `publish-homebrew` / a desktop
build is red, `preflight` dies on a stray `.git`, or a tag must be redone: read
[references/troubleshooting.md](references/troubleshooting.md).
