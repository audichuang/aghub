# Auto-deploy on connect: bundled `aghub-api` + connect-path upgrade — implementation design

- **Date**: 2026-06-21
- **Status**: Design — ready for plan
- **Author**: audichuang (with Claude)
- **Refines / supersedes**: [`2026-06-05-desktop-bundle-aghub-api-design.md`](./2026-06-05-desktop-bundle-aghub-api-design.md)
  (the "prerequisite" sketch) and the earlier bundling-only revision of this
  file. This version expands scope to deliver **zero-manual auto-deploy on
  connect** for the same-platform case.

## Context & problem

Connecting a **shipped** desktop app to a VM that already runs an _older,
incompatible_ `aghub-api` currently dead-ends: the desktop shows
"Remote aghub-api is incompatible … Auto-deploy isn't available in this build —
install aghub-api on the VM manually." The user must SSH in by hand. Two
independent gaps cause this:

- **Gap A — no install source in a packaged build.** `remote_install_source()`
  (`crates/desktop/src-tauri/src/commands/remote.rs:570`) only resolves a source
  from a dev env var (`AGHUB_REMOTE_API_BINARY`) or a git checkout
  (`git remote get-url origin`). A packaged app has neither, so it returns
  `None` → "Auto-deploy isn't available", and even the manual "Force redeploy"
  button reports `RemoteApiMissing`.
- **Gap B — connect never upgrades a present-but-incompatible binary.**
  `ensure_remote_api` (`crates/remote/src/bringup.rs:287`) short-circuits with
  `if first.api_present { return Ok(first); }` — it returns as soon as _any_
  `aghub-api` is found, **without checking compatibility**. So a VM with an old
  `1.1.x` binary is never auto-upgraded on connect; `bring_up` then sees
  `!compatible` and surfaces the "Incompatible" screen, forcing a manual click.

This design closes both: ship a version-locked `aghub-api` inside the desktop
bundle (Gap A), and make the connect path auto-upgrade a present-but-incompatible
binary when a source is available and the platform matches (Gap B). End state:
on the same platform (here, Linux desktop → Ubuntu VM), **reconnecting silently
upgrades the VM to the matching version and connects — no SSH, no button.**

The bring-up plumbing already exists: `RemoteInstallSource::LocalBinary(PathBuf)`
is consumed by `ensure_remote_api` (connect) and `force_redeploy_remote_api`
(redeploy); the remote finish step does `mv → chmod 755 → --version`
(`crates/remote/src/ssh.rs:375-382`).

## Settled decisions

1. **Bundle via `bundle.resources`, not `externalBin`.** We need the binary's
   _file path_ to `scp` it, not to _execute_ a sidecar locally. `resources`
   resolves cleanly via `BaseDirectory::Resource` (core Tauri v2.11, no
   fs/shell plugin); `externalBin` targets the shell-plugin sidecar runner this
   project does not use.
2. **Injected via a committed overlay + `--config`** — never by editing the
   committed `tauri.conf.json`. A small overlay JSON
   (`crates/desktop/src-tauri/tauri.bundle.conf.json`) is merged by Tauri
   natively (`--config` → `TAURI_CONFIG`), so `pubkey` / `version` / the main
   `bundle` block stay untouched, and plain `tauri dev` / `cargo build` are
   unaffected.
3. **Available in CI _and_ via a local `just` recipe** (distribution). The
   overlay + a build-and-stage step run in the release `build-tauri` job **and**
   in a new local `just` recipe, so an installable desktop with the embedded
   sidecar can be produced **now**, without waiting for a tag/CI release. The
   committed config stays clean either way — only the recipe / CI pass
   `--config`.
4. **Auto-upgrade on connect** (Gap B). `ensure_remote_api` is changed so a
   present-but-_incompatible_ binary triggers a deploy/upgrade when a source is
   available, instead of returning early. This is a remote **mutation**, so it
   stays gated: only when a source exists AND (for a `LocalBinary` source) the
   remote `(os, arch)` matches the desktop's. `CargoGit` (compiles on the VM) is
   un-gated. Manual "Force redeploy" remains as an explicit override.
5. **Same-OS/arch only for the bundled binary.** A packaged build has no
   `CargoGit` fallback (no git checkout), so cross-OS (e.g. Mac/Windows desktop
   → Linux VM) stays **out of scope**: the platform gate refuses the wrong-arch
   binary and the UI shows the manual-install hint. The target case (Linux
   desktop → Linux VM) is fully covered.

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

- Glob `aghub-api*` covers Unix `aghub-api` and Windows `aghub-api.exe` from one
  static file.
- `resources` paths resolve relative to the **main** config dir (`src-tauri/`),
  because `--config` merges as JSON into the main config — not relative to the
  overlay file's own location. So the pattern means `src-tauri/binaries/aghub-api*`.
- Tauri list resources preserve their relative directory structure, so the
  runtime resource key is `binaries/aghub-api` (Unix) / `binaries/aghub-api.exe`
  (Windows).

### 2. CI staging (`.github/workflows/release.yml`, `build-tauri` job)

One step **after `Sync Version`, before `Build Tauri`** (per matrix target):

1. **Clean + build** the sidecar:
   `rm -rf crates/desktop/src-tauri/binaries && mkdir -p …`, then
   `cargo build -p aghub-api --release --target <triple>` (built **after**
   `Sync Version`, so version-locked), then copy
   `target/<triple>/release/aghub-api[.exe]` → `…/binaries/aghub-api[.exe]`.
2. **Post-sync version assertion:** grep root `Cargo.toml` (and `tauri.conf.json`,
   whose `version` line does match) and assert the synced version is actually
   present — `sed` exits 0 on no-match, so this is the real guard.
   `crates/desktop/package.json` has **no** `"version"` field; leave its
   pre-existing no-op `sed` alone and do not assert a field that does not exist.
3. **Host-arch-aware sidecar smoke test:** only **execute** `aghub-api --version`
   when the target arch == the runner host arch (both `aarch64-apple-darwin` and
   `x86_64-apple-darwin` build on the same arm64 `macos-latest` runner — running
   the x86_64 binary relies on Rosetta, **not** guaranteed). Native → parse
   `major.minor` and assert == synced workspace version. Non-native → verify via
   the `Cargo.toml` version string + file existence; never execute it.
4. tauri-action `args`: `--target <triple>` → `--target <triple> --config src-tauri/tauri.bundle.conf.json`
   (relative to `projectPath: crates/desktop`).
5. **Remove the `|| true`** on the `tauri.conf.json` version `sed` (the explicit
   assertion in step 2 is what proves a match).

`build-cli` is unchanged (the CLI does not bundle the API).

### 3. Local staging recipe (`justfile`) — distribution

A new recipe (e.g. `desktop-bundle`) lets a developer produce an installable
build with the embedded sidecar locally, mirroring CI:

- Detect the host triple (`rustc -vV` → `host:` line).
- `rm -rf` + `mkdir -p crates/desktop/src-tauri/binaries`.
- `cargo build -p aghub-api --release` (host target) and copy to
  `…/binaries/aghub-api[.exe]`.
- Run the desktop bundle build passing `--config src-tauri/tauri.bundle.conf.json`
  (via `bun run tauri build`), so the same overlay is applied locally.

This keeps the committed config clean (plain `bun run dev` / `tauri dev` /
`cargo build` are untouched) while giving a one-command path to a usable build
today.

### 4. Runtime resolution (`crates/desktop/src-tauri/src/commands/remote.rs`)

A **packaged-first** branch at the top of `remote_install_source`:

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
	let name = if cfg!(windows) {
		"binaries/aghub-api.exe"
	} else {
		"binaries/aghub-api"
	};
	let path = app.path().resolve(name, BaseDirectory::Resource).ok()?;
	path.exists().then_some(path)
}
```

- `app.path().resolve(.., BaseDirectory::Resource)` is core Tauri (import
  `tauri::path::BaseDirectory`); no fs/shell plugin needed.
- The `.exists()` check keeps dev builds on the env/cargo-git fallback (the
  resource was never bundled → falls through), so `bun run start` from the repo
  still uses `CargoGit`.
- Executable bit is **not** handled here — the remote `finish_remote_api_upload`
  chmods (`crates/remote/src/ssh.rs:375-382`).
- **Thread `&AppHandle`** into `remote_install_source` and add `app: AppHandle`
  to the `remote_install_source_available` command. `bring_up` and
  `force_redeploy_remote` already have `app`; the command is invoked from the
  frontend with no payload, so the injected handle is transparent.

### 5. Connect-path auto-upgrade (`crates/remote/src/bringup.rs`) — Gap B

Restructure `ensure_remote_api` so "present" no longer means "done":

```rust
let first = probe_connection(runner, conn, local_version);
if !first.reachable {
	return Err(ConnectError::Unreachable { stderr: first.message });
}
// Present AND compatible → nothing to do.
if first.api_present && first.compatible {
	return Ok(first);
}

// Absent, or present-but-incompatible: try to install/upgrade.
let Some(source) = source else {
	// No source: absent → RemoteApiMissing; present-but-incompatible →
	// return the probe so the caller surfaces the Incompatible screen
	// (unchanged behaviour for that case).
	return if first.api_present {
		Ok(first)
	} else {
		Err(ConnectError::RemoteApiMissing { install_hint: install_hint() })
	};
};

// Same-platform gate for a LocalBinary source — now covers BOTH the absent
// and the upgrade path. CargoGit compiles on the VM, so it is un-gated.
if let RemoteInstallSource::LocalBinary(_) = source {
	let local = (std::env::consts::OS, std::env::consts::ARCH);
	let remote = probe_remote_platform(runner, conn);
	let same = remote.as_ref().map(|(o, a)| o == local.0 && a == local.1).unwrap_or(false);
	if !same {
		let remote_platform = remote
			.map(|(o, a)| format!("{o}/{a}"))
			.unwrap_or_else(|| "unknown".to_string());
		// Cross-platform: cannot deploy. Present-but-incompatible → surface the
		// probe (Incompatible screen + manual hint); absent → CrossPlatformDeploy.
		return if first.api_present {
			Ok(first)
		} else {
			Err(ConnectError::CrossPlatformDeploy { remote_platform })
		};
	}
}

let bin = resolved_path(conn);
install_remote_api(runner, conn, &bin, source)?; // overwrites an old binary
let second = probe_connection(runner, conn, local_version)
	.with_install_result(true, "aghub-api installed/upgraded".to_string());
if !second.reachable {
	return Err(ConnectError::Unreachable { stderr: second.message });
}
if !second.api_present {
	return Err(ConnectError::DeployFailed(format!(
		"Automatic install ran, but aghub-api is still unavailable: {}",
		second.message
	)));
}
Ok(second)
```

Net effect: with bundling (Component 4) giving a packaged build a `LocalBinary`
source, connecting to a same-platform VM with an old binary now **auto-overwrites
it with the version-locked binary and connects**, no manual step. The connect
path no longer needs the user to find and press "Force redeploy" for the common
case; the button remains as an explicit override. `install_remote_api` already
does stage→finish (`mv` + `chmod`), so the upgrade overwrites cleanly.

### 6. macOS codesigning

Unsigned today (`APPLE_*` commented out). Resources are covered by the app's
deep signature **when** signing exists — nothing to do now; just confirm the
unsigned bundle still builds with the embedded resource. A future note when
certs land.

### 7. `.gitignore` + `.prettierignore`

Add `crates/desktop/src-tauri/binaries/` to **both** so a CI- or recipe-staged
binary is never committed and never dirties the prettier/lint/test gates
(`.prettierignore` currently ignores only Tauri `gen/` and `target/`).

## Testing / acceptance criteria

- **Dev untouched:** `cd crates/desktop && bun run tauri build` (no overlay) and
  `bun run dev` still work and need no staged sidecar.
- **Local bundle:** the new `just` recipe produces an installable desktop whose
  `Resources/binaries/aghub-api` exists and whose `--version` matches the
  workspace version.
- **Gap A:** in a packaged/locally-bundled build, `remote_install_source()`
  returns the bundled `LocalBinary`; in a repo `bun run start`, it falls back to
  `CargoGit`.
- **Gap B (the headline):** connecting to a same-platform VM running an
  incompatible `aghub-api` auto-upgrades it and connects with **no manual step**;
  with no source (and no bundled binary) the behaviour is unchanged (Incompatible
  screen). Unit-test the new `ensure_remote_api` branches with `MockRunner`
  (present+incompatible+LocalBinary+same-platform → installs; present+incompatible
  +no-source → returns Ok(first); present+incompatible+LocalBinary+cross-platform
  → Ok(first); absent+cross-platform → CrossPlatformDeploy).
- **CI-only behaviour** (overlay resource resolution, per-target naming,
  host-arch smoke) is validated by a tag push via the `releasing-aghub`
  re-release-a-botched-tag flow.

## Out of scope / follow-ups

- Cross-OS/arch VM deploy from a packaged build (no `CargoGit` fallback).
- Bundling multiple remote-platform sidecars and selecting by `probe_remote_platform`.
- Delta/compressed uploads; signed/notarized macOS bundles (blocked on Apple certs).

## Risks (confirm on first tag push)

1. ~~`--config` overlay resource path base~~ — confirmed (Tauri 2.11 / tauri-utils
   2.9.1 / tauri-build 2.6.1): merged via `TAURI_CONFIG`, base is `src-tauri/`.
2. Windows `.exe` naming handled in CI staging, the `just` recipe, and runtime
   resolution (`cfg!(windows)`).
3. Host-arch smoke must not execute a non-native macOS target binary.
4. Gap B changes a connect into a silent remote mutation. Mitigated by the
   source + same-platform gates and the version-locked bundled binary; the
   no-source path is unchanged.
