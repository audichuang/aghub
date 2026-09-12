---
name: releasing-aghub
description: Runbook for cutting a desktop + CLI release of this aghub fork (audichuang/aghub) via the tag-driven GitHub Actions pipeline, plus the versioning model and how to verify artifacts and fix the failures it commonly hits. Use when the user wants to cut/ship a release, bump the version, push a release tag, when a Release workflow run fails (macOS `security import` / sccache `Cargo Fetch`), or when verifying release artifacts, `latest.json`, or the Homebrew tap. ALSO use for any aghub version/maintenance question — why a local `aghub-cli --version` reports a `-dev` version, where the version number comes from, keeping the desktop app and CLI on the same version / shipped together, `just bump`, or a `ci.yml` `test` flaking on a test that passes locally.
---

# Releasing aghub (this fork)

## TL;DR — for a normal forward release, run one command

```bash
just release X.Y.Z          # e.g. just release 2.3.8   (add --yes to skip the confirm)
```

`just release` wraps `scripts/release.sh`, which automates the whole mechanical
flow so the model doesn't burn tokens re-deriving it each time and can't fumble
the gotchas: it validates the version, **pushes to `fork` (never origin =
upstream)**, waits for the HEAD commit's `ci.yml` to go **green** before tagging,
tags `vX.Y.Z`, watches `release.yml` (**auto-reruns once** on a transient CI
dispatch flake — jobs stuck `queued` with nothing published), then verifies the
artifacts (`latest.json` URLs, Homebrew cask sha256). You still pick the version
and confirm go/no-go — a tag triggers a real public release.

The rest of this file is the **reference** behind that script — read it to
understand the model, debug a failure the script surfaces, or do something the
script does not cover (notably **re-releasing a botched tag**, which needs a
manual delete + retag — see the last section).

Releases are **tag-driven**: pushing a `v*` tag runs `.github/workflows/release.yml`, which fans out to
`verify-ci` (gate) → `changelog` → `build-tauri` (4 targets) + `build-cli` (4 targets) → `publish-homebrew`.
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
`git push` is gated by a **pre-push hook** (prettier `--check` + clippy `-D warnings` + eslint + tsc) — note it does
**NOT** run tests; that gap is why `just preflight` exists. `just preflight` runs on your platform only and cannot
reproduce macOS/Windows-specific behavior — for that, rely on the CI matrix and write tests that simulate the platform
condition on Linux (e.g. operate through a symlinked temp dir to mimic macOS `/var` → `/private` canonicalize).

**Upstream ports**: `UPSTREAM.md` (repo root) is the complete log of what this fork takes / defers / skips from
`AkaraChen/aghub`. Any release that includes a port MUST add a row there before tagging — that is the durable record,
not just the commit message. (Distinct from the npx `skills` ecosystem upstream tracked by `npx-skills-contract`.)

## Verify after green

```bash
gh release view vX.Y.Z --repo audichuang/aghub --json assets --jq '.assets[].name'
gh release download vX.Y.Z --repo audichuang/aghub --pattern latest.json --output -   # urls must be audichuang/aghub
gh api repos/audichuang/homebrew-tap/contents/Casks/aghub.rb --jq .content | base64 -d | grep -E 'version|sha256'
gh api repos/audichuang/homebrew-tap/contents/Formula/aghub-cli.rb --jq .content | base64 -d | grep -m1 version
```

- Expect dmg (arm+x64), nsis `setup.exe`, msi, AppImage/deb/rpm, 4 CLI archives, `latest.json`.
- `latest.json` urls + signatures must point at **this** repo; cask `sha256` must be non-empty.
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

| Symptom                                                                                                                                                       | Cause                                                                                                                                                                                                        | Fix                                                                                                                                                                                                                                                                                                  |
| ------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| macOS `failed codesign … security import: failed to import keychain certificate`                                                                              | Unset `APPLE_*` secrets resolve to **empty strings**, so Tauri tries to import an empty cert                                                                                                                 | Keep the `APPLE_*` env lines **commented out** in `release.yml`; unsigned dmg builds fine. Only uncomment once real Apple Developer certs + secrets exist.                                                                                                                                           |
| `Cargo Fetch` fails: `sccache: Server startup failed … dns error … Try again`                                                                                 | Transient GitHub infra/DNS flake reaching the cache backend                                                                                                                                                  | Re-run the job: `gh run rerun --failed <run-id> --repo audichuang/aghub`. Not a code issue.                                                                                                                                                                                                          |
| Homebrew job fails on push to tap                                                                                                                             | Missing/expired `HOMEBREW_TAP_TOKEN`                                                                                                                                                                         | Reset the PAT secret; rest of the release is unaffected.                                                                                                                                                                                                                                             |
| `ci.yml` `test` (push to main) fails on a test that passes locally and under `-p <crate>` — and so blocks the release (verify-ci stays red until CI is green) | A test reading `dirs::home_dir()` raced a HOME/XDG-mutating test under `cargo test --workspace` (heavier parallel load than `-p` surfaces the race)                                                          | Serialize it: hold the shared lock (`test_env_lock` in api, `env_lock` in core) in BOTH the HOME/XDG-mutating test AND the home-reading test; `#[cfg(unix)]`-gate unix-only tests; canonicalize both path sides for macOS `/var`→`/private`. Reproduce with `cargo test --workspace`, not just `-p`. |
| `just release` aborts `timed out waiting for ci.yml to go green` while CI is still running / eventually goes green                                            | 3-OS matrix cold compile ran longer than the script's CI-wait window                                                                                                                                         | **Re-run** `just release X.Y.Z --yes` once CI is green — HEAD is already pushed, so it skips the wait and tags immediately. Wait window raised 20→40 min, so this should be rare.                                                                                                                    |
| One `Build Desktop (<target>)` job fails at `Uploading latest.json...` with `Not Found — update-a-release-asset`, everything else green                       | The 4 desktop jobs each read-modify-write the SAME `latest.json` asset (tauri-action `includeUpdaterJson`); two interleaved and one lost the race                                                            | Not a code issue — re-release the tag (below). `release.sh` now asserts the manifest carries all 4 `{os}-{arch}` keys, so a bad one can't pass verification silently. Bit v2.11.1 once in ~13 releases.                                                                                              |
| A platform's desktop app never offers an update, no error anywhere, release run was green                                                                     | Same race, silent shape: a lost update dropped that platform's key from `latest.json`. The endpoint is `releases/latest/download/latest.json`, so the updater is simply told "no update" — nobody reports it | `curl -sL .../releases/download/vX.Y.Z/latest.json \| jq '.platforms \| keys'` — expect 11 keys spanning darwin/linux/windows. Short? Re-release the tag. **Only `just release` runs this check** — a hand-pushed tag (every pre-release) bypasses it, so check by hand there.                       |

## Re-release a botched tag

If a run half-fails and leaves a partial Release, redo the **same** version cleanly:

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
