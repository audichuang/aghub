# Remote `aghub-api` version mismatch: force-redeploy

- **Date**: 2026-06-05
- **Status**: Design (revised after strict review)
- **Author**: audichuang (with Claude)
- **Prerequisite**: [`2026-06-05-desktop-bundle-aghub-api-design.md`](./2026-06-05-desktop-bundle-aghub-api-design.md) (bundling). This feature ships first behind the **dev fallback** install source and picks up the bundled binary automatically once the prerequisite lands.
- **Related**: `crates/remote`, `crates/desktop/src-tauri/src/commands/remote.rs`

## Context & Problem

The desktop brings up `aghub-api` on a remote VM over SSH and tunnels to it. Before using a
remote binary it **probes the version** and enforces compatibility:

- `probe_connection` (`crates/remote/src/bringup.rs:197`) parses the remote `aghub-api <semver>`
  banner (`parse_api_version`, `ssh.rs:425`) and sets `compatible` via `is_version_compatible`
  (`ssh.rs:439`), whose v1 rule is **equal `major.minor`**.
- In the live path, `bring_up` (`commands/remote.rs:263`) calls `ensure_remote_api`. When a binary
  is **present**, `ensure_remote_api` **early-returns `Ok(first)` regardless of compatibility**
  (`bringup.rs:291-293`). `bring_up` then checks `if !test.compatible` and constructs
  `RemoteError::Incompatible { remote_version }` **inline** (`commands/remote.rs:277-280`).
    > Note: `decide_deploy` (`bringup.rs:396`) is **not on this path** — it has zero production call
    > sites (only unit tests) and is dead code from the desktop's perspective. The auto-install only
    > fires for an **absent** binary, never for present-but-incompatible.

A user running their **own fork** of `aghub-api` (e.g. `1.1.1`) against a desktop enforcing
`2.0.1` therefore gets `aghub-api 1.1.1 (incompatible with 2.0.1)` and is **blocked** with no
in-app remedy.

**Decision (user):** detect this and offer a **user-triggered, confirmed "force redeploy" button**
that overwrites the remote binary with the **desktop's bundled `aghub-api`** (version-locked, so
`major.minor` matches by construction), then restarts and re-probes. Until bundling lands
(prerequisite spec), force-redeploy uses the existing **dev fallback** source
(`AGHUB_REMOTE_API_BINARY` / cargo-git via `remote_install_source()`).

## Goals

1. Turn the existing `Incompatible` detection into an **actionable** UI: show the version mismatch
   and a force-redeploy button.
2. Force-redeploy a **version-locked local `aghub-api`** over the incompatible remote one
   (overwrite), then restart + re-probe + connect.
3. Explicitly **user-triggered, confirmed, and same-platform-gated** (it overwrites the user's
   fork and a wrong-arch binary would not run).

## Non-goals

- **Bundling the binary** — separate prerequisite spec. This feature works behind the dev fallback
  until then.
- **Automatic redeploy** — `decide_deploy`'s (unused) auto policy is untouched; force-redeploy is
  explicit and confirmed.
- **Cross-platform redeploy** — a binary built for the desktop's `(os, arch)` will not run on a
  different remote; that case disables the button with guidance.
- **cargo-git "redeploy my fork" source** — deferred; the dev fallback env path remains.

## Architecture

```
crates/remote (aghub-remote)   ← probe_remote_platform (uname) + normalization;
                                 force_redeploy_remote_api orchestration entrypoint;
                                 build_remote_pkill_cmd; install to the probe-resolved path
crates/desktop/src-tauri
  commands/remote.rs           ← force_redeploy_remote command; shared finish_bring_up helper;
                                 RemoteError::CrossPlatformRedeploy; resolve install source
  lib.rs                       ← register force_redeploy_remote in generate_handler!
crates/desktop (frontend)
  providers/connection.tsx     ← branch the isError screen on kind==='incompatible';
                                 force-redeploy mutation + queryClient.setQueryData
```

Transport/bring-up logic stays in the tauri-free `aghub-remote` crate (unit-tested with
`MockRunner`); the Tauri command is thin glue, per the existing module.

## Components

### 1. Remote-platform probe + normalization (`crates/remote`)

- The local binary is built for the desktop's `(os, arch)`. The desktop reads its own platform via
  `std::env::consts::{OS, ARCH}` (e.g. `("macos","aarch64")`, as in `commands/logging.rs:129-131`).
- Add `probe_remote_platform<R: CommandRunner>(runner, conn) -> Option<(String, String)>` that runs
  `uname -sm` over SSH and **normalizes** to the `std::env::consts` vocabulary:
  `Darwin→macos`, `Linux→linux`; `arm64→aarch64`, `x86_64→x86_64`. Lock the mapping with unit tests.
- `same_platform = (desktop_os, desktop_arch) == normalized_remote`. **Unparseable or failed
  `uname` ⇒ `same_platform = false`** (button disabled + manual hint) — never a hard error.
- This is a **dedicated probe called only from the force-redeploy command**, so the read-only
  `test_connection` / `probe_connection` do not pay an extra SSH round-trip.

### 2. Force-redeploy entrypoint (`crates/remote/src/bringup.rs`)

```
fn force_redeploy_remote_api<R: CommandRunner>(
    runner: &R, conn: &Connection, local_version: &str, source: &RemoteInstallSource,
) -> Result<TestResult, ConnectError>
```

Steps:

1. **Kill the stale process first** (best-effort): a new `build_remote_pkill_cmd` issuing
   `pkill -x aghub-api || true`. This terminates the untracked incompatible process (whose pid we
   never captured) and avoids `ETXTBSY` on the overwrite.
2. **Resolve the install target to the path the probe will resolve.** The probe resolves via
   `command -v aghub-api` → `~/.cargo/bin` → `~/.local/bin` (`ssh.rs` `default_api_path_script` /
   `build_remote_probe_cmd`). The default install target is `~/.local/bin`, which can differ from a
   `~/.cargo/bin` binary that is earlier on `PATH`. Force-redeploy must install to the
   **probe-resolved** path (resolve it first, or require `remoteAghubPath` to be set), or the
   re-probe will run the OLD binary and report "still incompatible".
3. **Overwrite via atomic rename.** Adjust the finish step to `mv` the uploaded file onto the
   target then `chmod 755` (rename(2) avoids `ETXTBSY` even if the old inode is still mapped),
   rather than `install -m 755` in place (`ssh.rs:336-344`).
4. **Re-probe** (`probe_connection`) and return the new `TestResult`.

Returns `Result<TestResult, ConnectError>`. **No new `ConnectError` variant is added** — the
`From<ConnectError> for RemoteError` impl (`commands/remote.rs:96-113`) is an exhaustive match, so
adding one would require a new arm; instead all new error shaping happens in the Tauri layer.

> The existing auto path (`ensure_remote_api`) intentionally does not install over a present
> binary; `force_redeploy_remote_api` is the explicit override that does.

### 3. `force_redeploy_remote` Tauri command (`commands/remote.rs` + `lib.rs`)

- **Register in `tauri::generate_handler![…]`** in `lib.rs` (alongside `connect_remote` etc.). No
  `capabilities/` entry is needed for custom app commands (only plugin permissions live there).
- Resolve the install source: bundled binary when packaged (per the prerequisite spec), else the
  dev fallback `remote_install_source()`; if `None`, return `RemoteApiMissing { install_hint }`.
- Compute `same_platform` via Component 1. If `false`, return a new
  **`RemoteError::CrossPlatformRedeploy { remote_platform, hint }`** (Tauri-layer variant) carrying
  the manual `install_hint()` — before any mutation.
- Run `force_redeploy_remote_api`. On a **compatible** re-probe, continue the bring-up and connect;
  on failure, return a structured `RemoteError` (`DeployFailed`, etc.) and leave the remote as-is
  (uploads use a temp path + finish step; a partial failure does not brick the remote).
- **Lifecycle/concurrency (must match `connect_remote`):** claim the `connecting` dedup slot and
  release it on every path (`commands/remote.rs:186-199`). Force-redeploy is only reachable from
  the failed/incompatible state, so **no live `RemoteHandle` should exist** for this id; if one
  unexpectedly does, **refuse with `AlreadyConnecting`** rather than tearing down a working
  connection. On success, **insert the resulting `RemoteHandle` into `state.handles`** so
  disconnect/exit can reach the tunnel + remote process.
- **Extract `finish_bring_up(app, connection, started) -> Result<RemoteHandle, RemoteError>`** from
  the tail of `bring_up` (`commands/remote.rs:283-356`: `start_remote` → `find_available_port` →
  tunnel spawn → `TUNNEL_SETTLE` `try_wait` with orphan-kill guard → `spawn_tunnel_watcher` →
  `RemoteHandle`). Both `connect_remote` and `force_redeploy_remote` call it, so the
  "every early return must guarded-kill the started remote" invariant (`:291`) is written once.

### 4. Frontend: actionable incompatibility (`crates/desktop/src/providers/connection.tsx`)

Today the error state is a **full-screen early-return rendered before `<ConnectionContext>`**
(`connection.tsx:215-224`): `serverQuery.isError` → a `<div>` with a single danger `<p>`. There is
no error-kind branch and no slot for a button, and children using the context are not mounted.

- **Refactor the `isError` branch** to detect `kind === 'incompatible'` (the payload already
  carries `remoteVersion`, surfaced around `connection.tsx:55/77`) and render a **dedicated
  incompatibility screen**: remote version, desktop version (`aghub_api::VERSION`), and a
  **「強制重新部署」button** (enabled only when same-platform; cross-platform → disabled + tooltip
  pointing at the manual `cargo install` hint).
- Confirm dialog (HeroUI v3 `alert-dialog`): _"這會用 desktop 內建的 aghub-api 覆蓋遠端現有的
  （包含你的 fork）。繼續？"_.
- The force-redeploy **`useMutation`** and the success handler
  **`queryClient.setQueryData(['server', activeId], port)`** (to flip `serverQuery` error→success
  and open the tunnel) must live **inside `ConnectionProvider`** (or a self-contained component
  instantiated in the error branch), because the context boundary is not open in the error state.
- This persistent, actionable banner is a **deliberate exception** to the project's "errors via
  toast" rule (`crates/desktop/CLAUDE.md`); transient sub-errors during redeploy still use toast.
- No `useEffect` for the call (mutation only), per project conventions.

## Data flow

1. Connect → `bring_up` → `ensure_remote_api` finds present-but-`!compatible` →
   `RemoteError::Incompatible { remote_version }`.
2. UI shows the mismatch screen + 「強制重新部署」(enabled iff same-platform).
3. Confirm → `force_redeploy_remote`: resolve source → `probe_remote_platform` → same-platform OK →
   `force_redeploy_remote_api` (`pkill` → resolve target path → upload → atomic `mv`+`chmod` →
   re-probe) → `compatible: true` → `finish_bring_up` (start → tunnel → handle) → return port.
4. UI: `queryClient.setQueryData(['server', activeId], port)` → tunnel `baseUrl` active.

## Error handling & edge cases

| Case                                         | Behavior                                                                                                                                                                            |
| -------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Cross-platform / unparseable/failed `uname`  | `same_platform = false` → button disabled + tooltip; command returns `CrossPlatformRedeploy` + manual `install_hint()`.                                                             |
| No resolvable source (dev build, no env/git) | `RemoteApiMissing { install_hint }` (existing).                                                                                                                                     |
| Old incompatible process running             | `pkill -x aghub-api` (best-effort) before overwrite; atomic `mv` avoids `ETXTBSY` regardless. New server binds a fresh port (`start_remote` uses `--port 0`), so no port collision. |
| Install-target ≠ probe-resolved path         | Install to the probe-resolved path (or require `remoteAghubPath`) so the re-probe runs the new binary — otherwise "still incompatible after redeploy".                              |
| Upload / `mv` / re-probe failure             | `DeployFailed`; do not loop. If the overwrite already happened, the message states redeploy succeeded but start/probe failed.                                                       |
| Concurrent connect + redeploy for same id    | `connecting` dedup slot serializes them; an unexpected pre-existing live handle → refuse with `AlreadyConnecting` (do not clobber a working connection).                            |

## Security considerations

- Force-redeploy overwrites an executable on a host the user controls — gated behind an explicit,
  confirmed action and a same-platform check.
- SSH/scp commands are built as **structured `Vec<String>` argv** (never shell-joined), and
  `remoteAghubPath` is neutralized via `shell_quote_single` (`ssh.rs`); there is no
  credential/token in `Connection` to leak. (There is no "redaction" in `crates/remote`; the
  safety property is structured-argv + quoting, not redaction.)
- Registering a new command does not change Tauri capabilities (custom commands route via
  `generate_handler!`).

## Implementation notes (visibility)

- `install_hint` (`bringup.rs:411`) is currently private → make it `pub(crate)` or carry the hint
  string in the `CrossPlatformRedeploy` error.
- `resolved_path` is private in both `bringup.rs:177` and `commands/remote.rs:140` → expose
  `pub(crate)` or have `force_redeploy_remote_api` accept a pre-resolved target path from the
  command layer.
- `MockRunner` is `#[cfg(test)] pub(crate)` in `crates/remote` → unit coverage stops at that crate
  boundary; desktop-level command tests need a separate seam or are out of scope.

## Testing strategy

- **`crates/remote`** (with `MockRunner`): - `probe_remote_platform` normalization: `Darwin/arm64 → macos/aarch64`, `Linux/x86_64`,
  unparseable → `None`. - `force_redeploy_remote_api` argv sequence: `pkill → (resolve target) → scp upload → mv+chmod →
probe`; returns a compatible `TestResult` when the mock reports a matching version. - re-probe still incompatible → error surfaced, no start. - install-target resolution picks the probe-resolved path. - existing `parse_api_version` / `is_version_compatible` tests unchanged; update the
  `install_remote_api`/finish-step argv test for the `mv`+`chmod` change.
- **`crates/desktop` (frontend)**: incompatible screen renders with version info; button
  enabled/disabled by same-platform; confirm→mutation→`setQueryData` path; transient errors → toast.
- **Command layer**: `finish_bring_up` extraction preserves the orphan-kill guard (the existing
  bring-up tests should still pass); a redeploy inserts a `RemoteHandle` reachable by
  `disconnect_remote`.

## Known limitations / out of scope

- Cross-platform redeploy (needs per-target binaries or cargo-git) — deferred.
- "Redeploy my fork" (cargo-git source in the UI) — deferred; env path remains.
- Backing up the overwritten remote binary — not done; the confirm dialog is the warning.
- Windows remotes (no `uname`) — fall into the `same_platform = false` safe default.
