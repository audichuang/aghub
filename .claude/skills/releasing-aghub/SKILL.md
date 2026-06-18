---
name: releasing-aghub
description: Runbook for cutting a desktop + CLI release of this aghub fork (audichuang/aghub) via the tag-driven GitHub Actions pipeline, plus how to verify artifacts and fix the failures it commonly hits. Use when the user wants to cut/ship a release, bump the version, push a release tag, when a Release workflow run fails (macOS `security import` / sccache `Cargo Fetch`), or when verifying release artifacts, `latest.json`, or the Homebrew tap.
---

# Releasing aghub (this fork)

Releases are **tag-driven**: pushing a `v*` tag runs `.github/workflows/release.yml`, which fans out to
`test` (ubuntu/macOS/Windows gate) → `changelog` → `build-tauri` (4 targets) + `build-cli` (4 targets) → `publish-homebrew`.
The `test` job gates everything — a tag whose tests fail on **any** platform produces **no** artifacts (added after a
macOS/Windows-only bug shipped because the build compiled but tests were red). There is no manual build or upload.
This fork ships its **own** independent version line — start ≥ the highest existing tag.

## Cut a release

```bash
# 0. PRE-FLIGHT — never tag a commit whose tests aren't green on all platforms.
just preflight                                   # local: fmt+clippy+typecheck+test+doc (the pre-push hook does NOT run tests)
git push origin main                             # then let CI's 3-OS matrix run
gh run watch <ci-run-id> --repo audichuang/aghub --exit-status   # must be GREEN before step 1

# 1. pick the next version (independent monotonic semver; do NOT hand-edit manifests —
#    CI seds the tag into Cargo.toml / desktop package.json / tauri.conf.json)
git tag vX.Y.Z && git push origin vX.Y.Z

# 2. watch it to completion (grab the run id from the line below)
gh run list  --repo audichuang/aghub --workflow release.yml --limit 1
gh run watch <run-id> --repo audichuang/aghub --exit-status
```

The release `test` gate is a backstop, not a substitute for step 0 — tagging a red commit just wastes a release run.
`git push` is gated by a **pre-push hook** (prettier `--check` + clippy `-D warnings` + eslint + tsc) — note it does
**NOT** run tests; that gap is why `just preflight` exists. `just preflight` runs on your platform only and cannot
reproduce macOS/Windows-specific behavior — for that, rely on the CI matrix and write tests that simulate the platform
condition on Linux (e.g. operate through a symlinked temp dir to mimic macOS `/var` → `/private` canonicalize).

## Verify after green

```bash
gh release view vX.Y.Z --repo audichuang/aghub --json assets --jq '.assets[].name'
gh release download vX.Y.Z --repo audichuang/aghub --pattern latest.json --output -   # urls must be audichuang/aghub
gh api repos/audichuang/homebrew-tap/contents/Casks/aghub.rb --jq .content | base64 -d | grep -E 'version|sha256'
```

- Expect dmg (arm+x64), nsis `setup.exe`, msi, AppImage/deb/rpm, 4 CLI archives, `latest.json`.
- `latest.json` urls + signatures must point at **this** repo; cask `sha256` must be non-empty.
- Install path for users: `brew install --cask audichuang/tap/aghub` (CLI: `audichuang/tap/aghub-cli`).

## Invariants (don't break these)

- **`tauri.conf.json` `pubkey`** (committed, plaintext) pairs with the `TAURI_SIGNING_PRIVATE_KEY` secret and **must never change** once a build ships — otherwise installed apps can't auto-update. `endpoints` must point at this repo.
- Required repo secrets: `TAURI_SIGNING_PRIVATE_KEY`, `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`, `HOMEBREW_TAP_TOKEN` (a PAT with Contents:write on `audichuang/homebrew-tap` — the default `GITHUB_TOKEN` can't reach a separate repo).
- The signing keypair lives only in those secrets (set once); it is not regenerated per release.

## Troubleshooting

| Symptom                                                                          | Cause                                                                                        | Fix                                                                                                                                                        |
| -------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------- |
| macOS `failed codesign … security import: failed to import keychain certificate` | Unset `APPLE_*` secrets resolve to **empty strings**, so Tauri tries to import an empty cert | Keep the `APPLE_*` env lines **commented out** in `release.yml`; unsigned dmg builds fine. Only uncomment once real Apple Developer certs + secrets exist. |
| `Cargo Fetch` fails: `sccache: Server startup failed … dns error … Try again`    | Transient GitHub infra/DNS flake reaching the cache backend                                  | Re-run the job: `gh run rerun --failed <run-id> --repo audichuang/aghub`. Not a code issue.                                                                |
| Homebrew job fails on push to tap                                                | Missing/expired `HOMEBREW_TAP_TOKEN`                                                         | Reset the PAT secret; rest of the release is unaffected.                                                                                                   |

## Re-release a botched tag

If a run half-fails and leaves a partial Release, redo the **same** version cleanly:

```bash
gh run cancel <run-id> --repo audichuang/aghub
gh release delete vX.Y.Z --repo audichuang/aghub --yes --cleanup-tag   # removes Release + remote tag
git tag -d vX.Y.Z
# ...commit the fix, push main, then re-tag:
git tag vX.Y.Z && git push origin vX.Y.Z
```

> Safe while no users have the build. For an already-public version, ship a new patch tag instead.
