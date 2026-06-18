# Desktop bundles a version-locked `aghub-api` (P0 prerequisite)

- **Date**: 2026-06-05
- **Status**: Design — prerequisite for force-redeploy
- **Author**: audichuang (with Claude)
- **Blocks**: [`2026-06-05-remote-force-redeploy-design.md`](./2026-06-05-remote-force-redeploy-design.md)

## Context & Problem

The desktop can deploy `aghub-api` to a remote VM, but only when an install **source** is
configured. `remote_install_source()` (`crates/desktop/src-tauri/src/commands/remote.rs:359`)
resolves a source from, in order:

1. `AGHUB_REMOTE_API_BINARY` env → `RemoteInstallSource::LocalBinary(path)`, else
2. `AGHUB_REMOTE_INSTALL_GIT_URL`/`git remote get-url origin` → `RemoteInstallSource::CargoGit`.

A **shipped desktop app has neither** (no dev env var, no git checkout), so it cannot deploy at
all. Verified: `tauri.conf.json` has no `externalBin`/`resources` (the `bundle` block at lines
34-62 holds only `active`/`targets`/`icon`/`macOS.dmg`/`createUpdaterArtifacts`); `build.rs` is
`fn main() { tauri_build::build() }`; the `build-tauri` CI job (`.github/workflows/release.yml`)
never builds or stages `aghub-api` before `tauri-action`.

This prerequisite makes the desktop **ship a matching `aghub-api` binary inside its bundle**, so
remote deploy (and the planned force-redeploy) can use a **version-locked** local binary whose
`major.minor` equals `aghub_api::VERSION` — the version the desktop enforces
(`commands/remote.rs:39`, `LOCAL_VERSION`).

## Goal

Ship `aghub-api` as a bundled sidecar with the desktop, version-locked to the desktop build, and
make `remote_install_source()` prefer it. No behavior change to remote connect itself.

## Non-goals

- Cross-compiling `aghub-api` for platforms other than each desktop release target.
- Changing the remote bring-up flow (handled by the force-redeploy spec).

## Components

1. **`tauri.conf.json` → `bundle.externalBin`.** Declare `aghub-api` as an external binary.
   Tauri requires the on-disk file to carry the **target-triple suffix**, e.g.
   `binaries/aghub-api-aarch64-apple-darwin`, `…-x86_64-unknown-linux-gnu`,
   `…-x86_64-pc-windows-msvc.exe`. The config references the base path `binaries/aghub-api`; Tauri
   resolves the suffixed file per target at bundle time.
2. **Staging the binary in CI** (`.github/workflows/release.yml`, `build-tauri` job). Add, **after
   the `Sync Version` step and before `tauri-action`**:
    - `cargo build -p aghub-api --release --target ${{ matrix.target }}`
    - copy the result to `crates/desktop/src-tauri/binaries/aghub-api-${{ matrix.target }}`
      (`.exe` on Windows).
      Building from the same checkout after `Sync Version` is what makes the binary version-locked.
3. **Runtime resolution** (`commands/remote.rs`). When packaged, resolve the sidecar via the Tauri
   path API (`app.path().resolve("aghub-api", BaseDirectory::Resource)` / the sidecar resolver)
   and return it as `RemoteInstallSource::LocalBinary`. **Dev fallback unchanged**: if the bundled
   binary is not resolvable, fall back to the existing `remote_install_source()` (env / cargo-git).
4. **macOS codesigning.** External binaries in the bundle must be signed with the app's identity
   and covered by the entitlements; verify the `build-tauri` signing step covers `binaries/`.
5. **Version-lock smoke test (MANDATED, not deferred).** In `build-tauri`, after building the
   sidecar, assert `aghub-api --version` (its `parse_api_version`-compatible banner) reports the
   same `major.minor` as the workspace version. Also **remove the `|| true`** on the
   `tauri.conf.json` version `sed` in `release.yml` so a version-sync failure fails loudly.

## Version semantics (important)

The correctness-relevant version is the **Cargo workspace version** (`Cargo.toml:20`,
inherited by `aghub-api` via `version.workspace = true`, surfaced as `aghub_api::VERSION` and used
as `LOCAL_VERSION`). The **Tauri app version** in `tauri.conf.json` is a separate track (currently
`1.2.1`, diverging from the workspace `1.1.1`) and is **not** what `is_version_compatible` checks.
The bundled `aghub-api` must match the workspace version, not the Tauri app version.

## Testing / acceptance criteria

- The packaged app contains `aghub-api` (per-target suffixed) in its resources.
- The bundled `aghub-api --version` matches `aghub_api::VERSION` (`major.minor`) — enforced by the
  CI smoke test.
- In a packaged build, `remote_install_source()` returns the bundled `LocalBinary` path; in a dev
  build it falls back to env/cargo-git as today.
- macOS: the bundled binary is signed (no Gatekeeper rejection).

## Out of scope / follow-ups

- Using the bundled binary to also fix the _absent_-binary auto-deploy path (today `decide_deploy`
  is unused in production; wiring it is separate).
- Delta/compressed uploads.
