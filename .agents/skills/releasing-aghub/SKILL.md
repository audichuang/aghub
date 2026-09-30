---
name: releasing-aghub
description: Runbook for cutting a desktop + CLI release of this aghub fork (audichuang/aghub) via the tag-driven GitHub Actions pipeline, plus the versioning model and how to verify artifacts and fix the failures it commonly hits. Use when the user wants to cut/ship a release, bump the version, push a release tag, when a Release workflow run fails (macOS `security import` / sccache `Cargo Fetch`), or when verifying release artifacts, `latest.json`, or the Homebrew tap. ALSO use for any aghub version/maintenance question — why a local `aghub-cli --version` reports a `-dev` version, where the version number comes from, keeping the desktop app and CLI on the same version / shipped together, `just bump`, or a `ci.yml` `test` flaking on a test that passes locally.
---

# Releasing aghub (this fork)

## TL;DR — for a normal forward release, run one command

```bash
just release X.Y.Z          # e.g. just release 2.3.8   (add --yes to skip the confirm)
just release verify X.Y.Z   # re-check a published release (read-only)
```

**Interrupted? Re-run the same command.** Killing the script (a timeout, another
agent's `pkill`) never affects the release — `release.yml` runs on GitHub. Once
`vX.Y.Z` is on the fork at HEAD, `just release X.Y.Z --yes` skips push/CI/tag
and RESUMES at "watch the run, then verify". Never tag by hand to "finish" it.

`just release` wraps `scripts/release.sh`, which automates the whole mechanical
flow so the model doesn't burn tokens re-deriving it each time and can't fumble
the gotchas: it validates the version, **pushes to `fork` (never origin =
upstream)**, waits for the HEAD commit's `ci.yml` to go **green** before tagging,
tags `vX.Y.Z`, watches `release.yml` (**auto-reruns once** on a transient CI
dispatch flake — jobs stuck `queued` with nothing published), then runs
`scripts/verify-release.sh`: assets, `latest.json` (version, 4 signed
platforms, URLs on this tag), `releases/latest`, the live updater endpoint, and
the Homebrew cask + formula (version AND sha256 equal to GitHub's asset
digests). You still pick the version and confirm go/no-go — a tag triggers a
real public release.

The rest of this file is the **reference** behind that script — read it to
understand the model, debug a failure the script surfaces, or do something the
script does not cover (notably **re-releasing a botched tag**, which needs a
manual delete + retag — see the last section).

Releases are **tag-driven**: pushing a `v*` tag runs `.github/workflows/release.yml`, which fans out to
`verify-ci` (gate) → `changelog` (creates the Release as a **draft**) → `build-tauri` (4 targets) + `build-cli`
(4 targets), all uploading into the draft → `publish` (runs `verify-release.sh --pre-publish` on the draft, then
makes it public / latest) → `publish-homebrew`.

**Nothing is public until `publish` passes.** Before this, the Release went public — and became
`releases/latest`, which is the updater endpoint — before any asset existed: on v2.30.0 the endpoint 404'd for
~15 min, then served a `latest.json` with only one platform for ~13 more, and a failed build left it like that. Now
a failed build, a red macOS signature check, or a `latest.json` that lost a platform to the upload race leaves a
**draft**: users keep getting the previous version. Every `softprops/action-gh-release` step must pass
`draft: true` — without it, finding the existing draft makes softprops publish it.
`verify-ci` gates everything: it asserts the tagged commit already has a **green push-to-main `ci.yml` run** (ci.yml's
`test` job runs the ubuntu/macOS/Windows suite unconditionally on every push to main). It does **not** re-run the suite
— a tag whose CI isn't green, or that never landed on main, produces **no** artifacts and fails fast at `verify-ci`.
The safety property it protects (added after a bug that compiled but had red tests shipped on macOS/Windows) is intact
— a green CI run means all 3 test legs passed — while dropping the old gate's ~26 min re-run of tests the tagged commit
already passed on its push to main. There is no manual build or upload. This fork ships its **own** independent version
line — start ≥ the highest existing tag.

## Versioning model & app/CLI sync

**One version, two artifacts, always shipped together.** The desktop app and the
CLI are never released independently: `build-tauri` and `build-cli` both
`needs: verify-ci`, and `publish-homebrew` `needs: [build-tauri, build-cli]` — so a
release publishes only when BOTH built from the SAME tag. The Homebrew tap's
`aghub` cask and `aghub-cli` formula are bumped to the same version in that one
job. Never ship one without the other; never let their versions diverge.

**Where the version comes from:**

- **Release builds** — the git tag is the source of truth. CI `sed`s `vX.Y.Z`
  (minus the `v`) into `Cargo.toml`, `crates/desktop/package.json`, and
  `crates/desktop/src-tauri/tauri.conf.json` at build time, and exports
  `AGHUB_RELEASE_VERSION` to `build-cli` so the binary self-reports exactly
  `X.Y.Z` (the smoke test asserts it). Without that env the sed dirties the
  tree and `git describe --dirty=-dev` stamped every release binary
  `X.Y.Z-dev` — shipped that way up to v2.5.4. Do NOT hand-edit the manifests
  for a release.
- **Local source builds** — `crates/cli/build.rs` stamps the binary from
  `git describe --tags --dirty=-dev` (leading `v` stripped), so
  `aghub-cli --version` self-reports a real version: `2.1.6` on a clean tag,
  `2.1.6-3-gabc1234` a few commits past it, `2.1.6-dev` with a dirty tree. It
  falls back to `CARGO_PKG_VERSION` when no tag is reachable (source
  tarball). `--always` is deliberately NOT used, so a bare commit SHA can
  never shadow the release version.
- **The committed manifest version is a placeholder** that lags the release
  line — don't read it as "the version". Trust the tag (releases) or
  `aghub-cli --version` (local). `just bump <ver>` only syncs the three
  manifests locally (handy before a desktop dev run); it does NOT drive
  releases. It uses `perl -i` so it works on Linux and macOS alike (the old
  `sed -i ''` was BSD/macOS-only and errored on Linux).

## Cut a release

```bash
# 0. PRE-FLIGHT — never tag a commit whose tests aren't green on all platforms.
#    If this release includes any port from the fork upstream (AkaraChen/aghub),
#    FIRST append a row to UPSTREAM.md (repo root) — upstream SHA ↔ our commit ↔
#    crate — and bump its "Last full review" SHA. Keep the sync log complete.
just preflight                                   # local: fmt+clippy+typecheck+test+doc (the pre-push hook does NOT run tests)
git push fork main                               # then let CI's 3-OS matrix run (origin = upstream — never push there)
gh run watch <ci-run-id> --repo audichuang/aghub --exit-status   # MUST be GREEN before step 1 — verify-ci hard-blocks otherwise

# 1. pick the next version (independent monotonic semver; do NOT hand-edit manifests —
#    CI seds the tag into Cargo.toml / desktop package.json / tauri.conf.json)
git tag vX.Y.Z && git push fork vX.Y.Z

# 2. watch it to completion (grab the run id from the line below)
gh run list  --repo audichuang/aghub --workflow release.yml --limit 1
gh run watch <run-id> --repo audichuang/aghub --exit-status
```

Step 0 is mandatory, not advisory: `verify-ci` fails the release if the tagged commit has no green push-to-main CI
run — it does not re-test. `just release` already waits for green before tagging; a manual tag must too.
`git push` is gated by a **pre-push hook** (oxfmt `--check` + clippy `-D warnings` + oxlint + tsc) — note it does
**NOT** run tests; that gap is why `just preflight` exists. `just preflight` runs on your platform only and cannot
reproduce macOS/Windows-specific behavior — for that, rely on the CI matrix and write tests that simulate the platform
condition on Linux (e.g. operate through a symlinked temp dir to mimic macOS `/var` → `/private` canonicalize).

**Upstream ports**: `UPSTREAM.md` (repo root) is the complete log of what this fork takes / defers / skips from
`AkaraChen/aghub`. Any release that includes a port MUST add a row there before tagging — that is the durable record,
not just the commit message. (Distinct from the npx `skills` ecosystem upstream tracked by `npx-skills-contract`.)

## Verify after green

```bash
just release verify X.Y.Z    # = bash scripts/verify-release.sh vX.Y.Z
```

One script, two call sites: `release.yml`'s `publish` job runs it with
`--pre-publish` on the draft (assets + `latest.json`), and `release.sh` runs the
full mode after the run (plus `releases/latest`, the live updater endpoint and
the Homebrew cask + formula — version and sha256, compared against GitHub's own
per-asset `digest`, so nothing is downloaded). Add a check THERE, never as a
one-off grep in a runbook: a check that lives only in prose is skipped by the
next hand-pushed tag.

- Install path for users: `brew install --cask audichuang/tap/aghub` (CLI: `audichuang/tap/aghub-cli`).

## Invariants (don't break these)

- **App + CLI are one release at one version.** `publish-homebrew` needs both
  `build-tauri` and `build-cli`; the tap's `aghub` cask and `aghub-cli` formula
  must always carry the same version, and the CLI binary's self-reported
  `git describe` version must match the tag too. A release that built only one
  of the two, or bumped one formula without the other, is broken — re-release.
- **`tauri.conf.json` `pubkey`** (committed, plaintext) pairs with the `TAURI_SIGNING_PRIVATE_KEY` secret and **must never change** once a build ships — otherwise installed apps can't auto-update. `endpoints` must point at this repo.
- Required repo secrets: `TAURI_SIGNING_PRIVATE_KEY`, `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`, `HOMEBREW_TAP_TOKEN` (a PAT with Contents:write on `audichuang/homebrew-tap` — the default `GITHUB_TOKEN` can't reach a separate repo).
- The signing keypair lives only in those secrets (set once); it is not regenerated per release.
- **The tap formula lives at `Formula/aghub-cli.rb`** (cask at `Casks/aghub.rb`).
  Modern Homebrew ignores formulae at the tap ROOT — `brew` then silently
  resolves the installed keg's cached formula, so users stay pinned at their
  installed version forever while `brew update` reports up-to-date (bit us up
  to v2.5.4: Macs stuck on 2.3.11). Never write the formula to the tap root.

## Troubleshooting

| Symptom                                                                                                                                                       | Cause                                                                                                                                                                                           | Fix                                                                                                                                                                                                                                                                                                  |
| ------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| macOS `failed codesign … security import: failed to import keychain certificate`                                                                              | Unset `APPLE_*` secrets resolve to **empty strings**, so Tauri tries to import an empty cert                                                                                                    | Never add `APPLE_*` secrets to `release.yml` until real Apple Developer certs exist; ad-hoc signing (`APPLE_SIGNING_IDENTITY: "-"`) builds fine. Switch-over steps: `docs/history/release.md#macos-ad-hoc-codesign`.                                                                                 |
| `Cargo Fetch` fails: `sccache: Server startup failed … dns error … Try again`                                                                                 | Transient GitHub infra/DNS flake reaching the cache backend                                                                                                                                     | Re-run the job: `gh run rerun --failed <run-id> --repo audichuang/aghub`. Not a code issue.                                                                                                                                                                                                          |
| Homebrew job fails on push to tap                                                                                                                             | Missing/expired `HOMEBREW_TAP_TOKEN`                                                                                                                                                            | Reset the PAT secret; rest of the release is unaffected.                                                                                                                                                                                                                                             |
| `ci.yml` `test` (push to main) fails on a test that passes locally and under `-p <crate>` — and so blocks the release (verify-ci stays red until CI is green) | A test reading `dirs::home_dir()` raced a HOME/XDG-mutating test under `cargo test --workspace` (heavier parallel load than `-p` surfaces the race)                                             | Serialize it: hold the shared lock (`test_env_lock` in api, `env_lock` in core) in BOTH the HOME/XDG-mutating test AND the home-reading test; `#[cfg(unix)]`-gate unix-only tests; canonicalize both path sides for macOS `/var`→`/private`. Reproduce with `cargo test --workspace`, not just `-p`. |
| `just release` aborts `timed out waiting for ci.yml to go green` while CI is still running / eventually goes green                                            | 3-OS matrix cold compile ran longer than the script's CI-wait window                                                                                                                            | **Re-run** `just release X.Y.Z --yes` once CI is green — HEAD is already pushed, so it skips the wait and tags immediately. Wait window raised 20→40 min, so this should be rare.                                                                                                                    |
| One `Build Desktop (<target>)` job fails at `Uploading latest.json...` with `Not Found — update-a-release-asset`, everything else green                       | The 4 desktop jobs each read-modify-write the SAME `latest.json` asset (tauri-action `includeUpdaterJson`); two interleaved and one lost the race                                               | Nothing shipped — the release is still a draft. `gh run rerun <run-id> --failed`, then resume with `just release X.Y.Z --yes`. Bit v2.11.1 once in ~13 releases.                                                                                                                                     |
| `publish` fails with `latest.json has no signed '<platform>' entry`, every build green                                                                        | Same race, silent shape: a lost update dropped that platform's key. It used to ship like that (the updater just says "no update" forever); `publish` now blocks it while the release is a draft | Rerun that platform's `Build Desktop` job (`gh run view <run-id>` → rerun it from the UI or `gh run rerun <run-id> --job <job-id>`), then rerun `publish`. Holds for hand-pushed tags and pre-releases too — the check is in the workflow now, not only in `just release`.                           |
| `publish-homebrew` fails with `no usable sha256 for release asset …`                                                                                          | An expected asset is missing or has no digest. It used to `wget … \|\| echo`, and a failed download shipped the empty-file sha256 `e3b0c442…` to the tap (v2.3.0)                               | Check the release's asset list; rerun the job once the asset exists. Never hand-write a checksum.                                                                                                                                                                                                    |
| Local `just preflight` fails first thing: `'/…/.git' exists above the test temp dir`                                                                          | A stray `.git` in an ancestor of `$TMPDIR` makes test fixtures look like they sit in a broken repo, and the skill repair tests fail (an empty `/tmp/.git` did this on 2026-09-26)               | Remove it if stray (`rmdir` when empty), or `TMPDIR=/var/tmp just preflight` — the TMPDIR must sit OUTSIDE `/tmp`; a subdirectory of it is still under the stray `.git` and fails the same guard.                                                                                                    |

## Re-release a botched tag

A failed run now leaves a **draft**, which usually needs no delete at all: fix,
`gh run rerun <run-id> --failed`, then `just release X.Y.Z --yes` resumes. Delete
and retag only when the tagged commit itself must change. If a run half-fails and
leaves a partial Release, redo the **same** version cleanly:

```bash
gh run cancel <run-id> --repo audichuang/aghub
gh release delete vX.Y.Z --repo audichuang/aghub --yes --cleanup-tag   # removes Release + remote tag
git tag -d vX.Y.Z
# ...commit the fix, push main, then re-tag:
git tag vX.Y.Z && git push fork vX.Y.Z
```

> Safe while no users have the build. For an already-public version, ship a new patch tag instead.

Check that precondition rather than assuming it — a release can be minutes old and already pulled:

```bash
gh release view vX.Y.Z --repo audichuang/aghub \
  --json assets --jq '[.assets[].downloadCount] | add'   # 0 → deleting is safe
```
