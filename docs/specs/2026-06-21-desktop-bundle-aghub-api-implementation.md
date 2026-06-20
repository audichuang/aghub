# Desktop bundles a version-locked `aghub-api` — implementation design

- **Date**: 2026-06-21
- **Status**: Design — ready for plan
- **Author**: audichuang (with Claude)
- **Refines**: [`2026-06-05-desktop-bundle-aghub-api-design.md`](./2026-06-05-desktop-bundle-aghub-api-design.md)
  (that doc is the "prerequisite" sketch; this is the implementation-ready,
  Codex-reviewed version)

## Context & problem

A **shipped** desktop app cannot deploy `aghub-api` to a remote VM: `remote_install_source()`
(`crates/desktop/src-tauri/src/commands/remote.rs:570`) only resolves a source from a dev env var
(`AGHUB_REMOTE_API_BINARY`) or a git checkout (`git remote get-url origin`). A packaged app has
neither, so remote connect / force-redeploy are dev-tree-only today.

This makes the desktop **release** build ship a `aghub-api` binary inside its bundle, version-locked
to the workspace version, and makes `remote_install_source()` prefer it. The bring-up side is
already ready: `RemoteInstallSource::LocalBinary(PathBuf)` is consumed by `ensure_remote_api`
(connect) and `force_redeploy_remote_api` (redeploy), both gating a cross-platform `LocalBinary`
behind `probe_remote_platform`.

## Settled decisions

1. **CI-only injection.** The committed `tauri.conf.json` is NOT modified. The bundling declaration
   is injected only in release CI, so local `tauri dev` / `tauri build` are completely unaffected.
2. **`bundle.resources`, not `externalBin`.** We need the binary's _file path_ to `scp` it, not to
   _execute_ a sidecar locally. `resources` resolves cleanly via `BaseDirectory::Resource`;
   `externalBin` is meant for the shell-plugin sidecar runner (which this project does not use).
3. **Committed overlay + `--config`.** A small committed overlay JSON is merged by Tauri natively
   via `tauri-action`'s `args: --config <overlay>`. No in-place `sed`/`jq` surgery on the main
   config; `pubkey` / `version` / `bundle` in `tauri.conf.json` stay untouched.
4. **Same-OS/arch only.** This feature guarantees deploy only when the VM's `(os, arch)` equals the
   desktop's (Mac→Mac, Linux→Linux). Cross-OS (e.g. Mac/Windows desktop → Linux VM) is **explicitly
   out of scope**: a packaged build has no git checkout, so there is no `CargoGit` fallback; the
   cross-platform gate refuses the wrong-arch binary and the UI shows the existing manual-install
   hint. This matches the current gate behaviour — we only document it, not change it.

## Components

### 1. Committed overlay config (new file)

`crates/desktop/src-tauri/tauri.bundle.conf.json`:

```json
{
	"bundle": {
		"resources": ["binaries/aghub-api*"]
	}
}
```

- Glob `aghub-api*` covers Unix `aghub-api` and Windows `aghub-api.exe` from one static file.
- `resources` paths resolve relative to the **main** config directory (`src-tauri/`), because
  `--config` is merged as JSON into the main config (via `TAURI_CONFIG`), not evaluated from the
  overlay file's own location. So the pattern means `src-tauri/binaries/aghub-api*`.
- Tauri list resources preserve their relative directory structure, so the runtime resource key is
  `binaries/aghub-api` (Unix) / `binaries/aghub-api.exe` (Windows).

### 2. CI staging (`.github/workflows/release.yml`, `build-tauri` job)

Add one step **after `Sync Version`, before `Build Tauri`** (runs per matrix target):

1. **Clean + build** the sidecar:
    - `rm -rf crates/desktop/src-tauri/binaries && mkdir -p crates/desktop/src-tauri/binaries`
      (clean first so the glob never bundles a stale match).
    - `cargo build -p aghub-api --release --target ${{ matrix.target }}` — built **after**
      `Sync Version`, so it is version-locked to the workspace version.
    - Copy `target/${{ matrix.target }}/release/aghub-api[.exe]` →
      `crates/desktop/src-tauri/binaries/aghub-api[.exe]`.
2. **Version assertions (replaces the fragile `sed`-exit reliance):** after `Sync Version`, grep
   each intended manifest and assert it actually carries the synced version, rather than trusting
   `sed`'s exit code (a no-match `sed` still exits 0). At minimum assert root `Cargo.toml`'s
   `version` line. Note: `crates/desktop/package.json` currently has **no** `"version"` field, so
   that existing `sed` is already a silent no-op. Scope the assertion to the version-relevant source
   only — root `Cargo.toml` (and `tauri.conf.json`, whose `version` line does match) — and leave the
   pre-existing `package.json` `sed` as-is (out of scope for this change); do **not** assert a field
   that does not exist.
3. **Host-arch-aware sidecar smoke test:** only **execute** `aghub-api --version` when the target
   arch equals the runner's host arch (both `aarch64-apple-darwin` and `x86_64-apple-darwin` build
   on the same arm64 `macos-latest` runner — running the x86_64 binary there relies on Rosetta,
   which is **not** guaranteed). When the binary is native, parse its `major.minor` and assert it
   equals the synced workspace version. When non-native, verify by comparing the source
   `Cargo.toml` version + confirming the staged file exists (do not execute it).
4. Change tauri-action's `args` from `--target ${{ matrix.target }}` to
   `--target ${{ matrix.target }} --config src-tauri/tauri.bundle.conf.json`
   (path relative to `projectPath: crates/desktop`).
5. **Remove the `|| true`** on the `tauri.conf.json` version `sed` so a real failure is loud — but
   note this alone does not prove a match (see #2; that's why the explicit assertion exists).

`build-cli` is unchanged (the CLI does not bundle the API).

### 3. Runtime resolution (`crates/desktop/src-tauri/src/commands/remote.rs`)

Add a **packaged-first** branch at the top of `remote_install_source`:

```rust
fn remote_install_source(app: &AppHandle) -> Option<RemoteInstallSource> {
    if let Some(path) = bundled_api_path(app) {
        return Some(RemoteInstallSource::LocalBinary(path));
    }
    // ... existing env / cargo-git fallback unchanged ...
}

/// Resolve the bundled, version-locked `aghub-api` shipped as a Tauri resource,
/// or `None` in a dev build where it was never bundled.
fn bundled_api_path(app: &AppHandle) -> Option<PathBuf> {
    let name = if cfg!(windows) { "binaries/aghub-api.exe" } else { "binaries/aghub-api" };
    let path = app.path().resolve(name, BaseDirectory::Resource).ok()?;
    path.exists().then_some(path)
}
```

- `app.path().resolve(.., BaseDirectory::Resource)` is core-Tauri (no `tauri-plugin-fs`/shell
  needed). Import `tauri::path::BaseDirectory`.
- The `.exists()` check is what keeps dev builds on the env/cargo-git fallback: in dev the resource
  was never bundled, so resolve→`exists()` is false and we fall through.
- Executable bit is **not** handled here: the remote `finish_remote_api_upload` does
  `mv` → `chmod 755` → `--version` (`crates/remote/src/ssh.rs:375-382`), so a resource copy losing
  `+x` locally is harmless.
- **Thread `&AppHandle`** into `remote_install_source` and add `app: AppHandle` to the
  `remote_install_source_available` command. Call sites: `bring_up` and `force_redeploy_remote`
  already have `app`; the `remote_install_source_available` command currently takes no args —
  adding the injected `AppHandle` is transparent to the frontend (it invokes with no payload).

### 4. macOS codesigning

Currently unsigned (`APPLE_*` secrets intentionally commented out in `release.yml`). Resources in
the `.app` are covered by the app bundle's deep signature **when** signing exists — so nothing to do
now. Acceptance for this change: confirm the unsigned bundle still builds with the embedded resource.
When real Apple certs land, the deep signature covers `Resources/binaries/` automatically — a
future note, no action here.

### 5. `.gitignore` + `.prettierignore`

Add `crates/desktop/src-tauri/binaries/` to **both** so a locally/CI-staged binary never gets
committed or dirties the prettier/lint/test gates (`.prettierignore` currently ignores only Tauri
`gen/` and `target/`).

## Testing / acceptance criteria

- **Dev untouched**: local `cd crates/desktop && bun run tauri build` (no `--config` overlay)
  still succeeds and needs no staged sidecar.
- **Packaged**: a CI build embeds `binaries/aghub-api[.exe]` per target; on a native-arch target the
  bundled `aghub-api --version` `major.minor` equals the synced workspace version (CI assertion);
  `remote_install_source()` returns the bundled `LocalBinary` path in a packaged build and falls
  back to env/cargo-git in dev.
- **Same-OS deploy works; cross-OS shows the manual hint** (out of scope, documented).
- CI-path correctness (`--config` overlay resource resolution, per-target naming, host-arch smoke)
  cannot be validated locally — iterate via tag pushes using the `releasing-aghub`
  "re-release a botched tag" flow.

## Out of scope / follow-ups

- Cross-OS/arch VM deploy from a packaged build (no `CargoGit` fallback without a git checkout).
- Bundling multiple remote-platform sidecars and selecting by `probe_remote_platform`.
- Delta/compressed uploads.
- Signed/notarized macOS bundles (blocked on Apple certs).

## Risks (confirm on first tag push)

1. ~~`--config` overlay resource path base~~ — **confirmed** by Codex against Tauri 2.11 /
   tauri-utils 2.9.1 / tauri-build 2.6.1: merged via `TAURI_CONFIG`, base is `src-tauri/`.
2. Windows `.exe` naming handled in both CI staging and runtime resolution (`cfg!(windows)`).
3. Host-arch smoke must not execute a non-native macOS target binary (P0 — addressed in §2.3).
