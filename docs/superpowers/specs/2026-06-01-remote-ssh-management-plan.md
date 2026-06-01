# Remote SSH Management — Implementation Plan (TDD)

**Date:** 2026-06-01
**Source:** Reconciliation of two independent Opus planning workflows
(subsystem-decomposition + adversarial review; holistic-architect + 3-lens critique).
Companion to `2026-06-01-remote-ssh-management-design.md`.

## Reconciled cross-cutting decisions

1. **Sync Tauri commands.** `test_connection`, `connect_remote`, `disconnect_remote`,
   `remote_status` are **synchronous** `#[tauri::command] fn` (Tauri runs them on its
   worker threadpool; blocking `std::process` is safe). src-tauri tokio is rt-only — do
   **not** add the `process` feature. The FE `invoke()` still returns a Promise.
2. **No new dependencies.** api crate already has tokio `process`; hand-roll arg parsing
   (no clap). Desktop crate: `std::process` only. Connection ids via `crypto.randomUUID()`.
3. **Shared Rust IPC types in `ssh.rs`:** `CommandOutput { status_code: Option<i32>,
   stdout: String, stderr: String }`, `RunError` (plain enum impl `std::error::Error`,
   **no thiserror**), `ChildHandle` (wraps `std::process::Child`, exposes `pid()` + `kill()`).
4. **Connection Rust struct** carries `#[serde(rename_all = "camelCase")]` so the FE
   camelCase payload (`sshTarget`/`remoteAghubPath`) deserializes. Round-trip serde test.
5. **String contract (land W1 first):** the api binary prints `AGHUB_API_PORT=<n>` and
   `aghub-api <semver>`; the desktop parsers consume them. Assert the exact literals in
   **both** crates' tests so drift breaks a test, not production.
6. **Cache isolation:** `queryClient.clear()` AFTER committing the new `baseUrl` on every
   active-connection switch (keys.ts keys are flat, not target-namespaced).
7. **Local connection** (id `"local"`) is synthesized in the provider, **not persisted**.
   The store holds only user remotes. **No store migration / no `CURRENT_VERSION` bump** —
   `store.get("connections") ?? []` handles absence (precedent: `lib/store/projects.ts`).
   (Deliberate deviation from both workflows, justified by the projects.ts precedent and to
   avoid bumping the shared `CURRENT_VERSION`.)
8. **Backward compat:** `ConnectionProvider` replaces `ServerProvider` but keeps rendering
   `<ServerContext value={{port, baseUrl}}>`, so `useServer()`/`useApi()` and every page
   work unchanged. `providers/server.tsx` is deleted (only `App.tsx` imports it).
9. **No capability change.** App-defined commands need no allowlist (`start_server` has
   none); `std::process` spawn is outside Tauri's capability system. Assert default.json
   unchanged.
10. **`find_available_port`** in `commands/server.rs:6` becomes `pub(crate)` and is shared
    by `start_server` and the tunnel local-port pick.

### Security hardening (from the security/lifecycle critics)

- **Remote-shell injection is the top risk.** Argv arrays only protect the *local* ssh/scp
  call; SSH's remote command is a single string re-parsed by the VM login shell. Any
  `remoteAghubPath` / connection id / log path interpolated into the remote `nohup …`
  command MUST be single-quote shell-escaped (`'` → `'\''`) and validated. Dedicated tests:
  a hostile `remoteAghubPath` stays inert in the composed remote command.
- **Tunnel entrance bound to loopback explicitly:** `-L 127.0.0.1:<local>:127.0.0.1:<remote>`
  (never `<local>:…`) regardless of ssh `GatewayPorts`.
- **Host keys:** keep `-o BatchMode=yes` (fails closed; no TOFU auto-accept). Error mapping
  distinguishes "unknown host key → ssh in once manually" from "REMOTE HOST IDENTIFICATION
  CHANGED". Assert `BatchMode=yes` present in a test.
- **Remote log** under a private dir, not predictable `/tmp`:
  `d="${XDG_RUNTIME_DIR:-$HOME/.cache/aghub}"; mkdir -p -m 700 "$d"; log="$d/aghub-api.<id>.log"`
  with file mode 600.
- **Guarded remote kill** (never blind `kill <pid>`):
  `kill -0 <pid> 2>/dev/null && ps -o comm= -p <pid> | grep -q aghub-api && kill <pid>`
  (SIGTERM, idempotent) — defends against PID reuse.
- **PID capture:** start with `nohup sh -c 'exec <bin> --port 0' >"$log" 2>&1 & echo PID=$!`
  so `$!` is the aghub-api process, not a wrapper.
- Surfaced ssh stderr to FE toasts must not echo the full quoted remote command line.

### Concurrency / lifecycle

- **Never hold the `Mutex<HashMap>` guard across the seconds-long ssh work** in
  `connect_remote`. Pattern: lock briefly to claim a per-id in-progress slot → do ssh work
  unlocked → re-lock to insert the handle; kill-and-bail if the slot was lost/duplicated.
- **Dead-tunnel detection:** per-tunnel `std::thread` (not a tokio task — rt is
  single-thread) blocking on `child.wait()`, emitting Tauri event
  `remote-disconnected { connectionId }` on exit; the FE flips that connection's status to
  `error`/`disconnected` and offers reconnect.
- **App-exit cleanup:** restructure `lib.rs` tail from `.run(generate_context!())` to
  `let app = builder.build(generate_context!())?; app.run(|app, event| match event {
  RunEvent::ExitRequested { .. } | RunEvent::Exit => cleanup_all_remotes(app), _ => {} })`.
- **Status projection:** Rust `ConnState` (Disconnected/Probing/Deploying/Starting/
  Tunneling/Connected/Error) projects to a 4-state FE status (idle/connecting/connected/
  error). Own the mapping in the provider.

### Deferred (YAGNI for v1, documented)

- Auto-`scp` requires bundling a matching-platform `aghub-api` into the Tauri app, which is
  not yet configured. v1 path is **detect-or-instruct**: `decide_deploy` is implemented and
  unit-tested (Skip | Scp | InstructInstall), the Scp branch locates the binary via a
  bundled-resource path or local `which aghub-api` (works in dev / same-platform), and
  otherwise returns `RemoteApiMissing { install_hint }`. SHA-256 verify, ControlMaster
  multiplexing, per-connection `disabledAgents`, and the availability-Spinner-flash polish
  are v2.
- **FE test runner is absent** (only `tsc`+`eslint`; no vitest/jest/jsdom, bun missing).
  Pure FE logic is extracted into testable `.ts` modules; executable tests use Node's
  built-in `node --test` (zero deps) where Node supports it, otherwise they are
  design-of-record. `tsc` + `eslint` are the mandatory FE gates.

## Ordered work items (TDD: RED → GREEN → verify per item)

> Verifier per Rust item: `cargo test -p <crate>` + `cargo clippy -p <crate> --all-targets`
> + `cargo fmt --check`. Package name for the desktop crate is **`aghub`** (not aghub-desktop).
> FE items: `./node_modules/.bin/tsc --noEmit` + `./node_modules/.bin/eslint <files>`
> (+ `node --test` for extracted logic where available).

### W1 — `aghub-api` binary CLI (crate `aghub-api`) — *no deps; land first*

Files: `crates/api/src/main.rs` (+ optional `crates/api/src/cli.rs` exported from lib for
unit tests).
- Pure `parse_args(args) -> Result<Config{port:u16(default 0), version:bool}, ParseError>`
  supporting `--port <n>`, `--port=<n>`, `--version`/`-V`.
- `version_string() -> String` = `format!("aghub-api {}", env!("CARGO_PKG_VERSION"))` (1.1.1).
- `pick_free_port() -> io::Result<u16>` (`TcpListener::bind("127.0.0.1:0")`).
- Thin `main`: version → print & exit; resolve port (0 → pick_free_port); print
  `AGHUB_API_PORT=<port>` + **flush stdout** before `start(ApiOptions{port, app_data_dir:None})`.
- RED tests: arg parse happy/equals/version/-V/error; version_string format == env semver;
  pick_free_port > 0 and re-bindable; assert exact literal `AGHUB_API_PORT=` prefix.

### W2 — SSH foundation `commands/ssh.rs` (crate `aghub`) — *no deps; parallel with W1*

- `Connection` (snake_case fields, `#[serde(rename_all="camelCase")]`), `CommandOutput`,
  `RunError`, `ChildHandle`, `trait CommandRunner { run; spawn }`, `SystemRunner`
  (`std::process`), `MockRunner` (`#[cfg(test)]`, scripted by (program,args), records calls).
- Pure: `build_ssh_args`, `build_scp_args` (`-P` uppercase), `build_tunnel_args`
  (`-N -o BatchMode=yes -o ExitOnForwardFailure=yes -L 127.0.0.1:<l>:127.0.0.1:<r> target`),
  `shell_quote_single`, `build_remote_start_cmd` (uses shell_quote on path+log+id),
  `parse_remote_port`, `parse_pid`, `parse_api_version`, `is_version_compatible`
  (major.minor equality).
- RED tests: every builder argv shape; **hostile `sshTarget`/`remoteAghubPath` stays one
  argv element**; `BatchMode=yes` present; remote-start cmd neutralizes injection;
  `-P` vs `-p`; user via `-l` (ssh) / `user@target` (scp); all parsers + compatibility;
  MockRunner returns scripted output and records calls.

### W3 — Remote bring-up logic `commands/remote.rs` (crate `aghub`) — *depends on W2*

- `ConnState`, `TestResult`, `ConnectError` (`RemoteApiMissing{install_hint}`,
  `Unreachable{stderr}`, `StartTimeout`, `TunnelFailed`) — all `Serialize`.
- Generic over `<R: CommandRunner>` so it runs under MockRunner with no real ssh:
  `probe_connection`, `decide_deploy(test_result, same_platform, has_bundled_binary) ->
  DeployDecision`, `start_remote` (issue start cmd, **bounded-attempts** poll of the log via
  `parse_remote_port`, injectable attempts+delay; on timeout issue guarded kill →
  `StartTimeout`), `cleanup_remote` (guarded kill).
- RED tests: probe (ok / unreachable-stderr / command-not-found); decide_deploy 3 branches;
  start_remote success; start_remote timeout **issues a guarded kill** (assert via Mock);
  cleanup issues guarded kill.

### W4 — Tauri commands + state + lifecycle wiring (crate `aghub`) — *depends on W1,W2,W3*

Files: `commands/remote.rs` (command layer), `commands/mod.rs`, `commands/server.rs`
(`find_available_port` → `pub(crate)`), `lib.rs`.
- `RemoteState { handles: Mutex<HashMap<String, RemoteHandle>>, inProgress }` in `AppState`.
- Sync commands `test_connection`, `connect_remote`, `disconnect_remote`, `remote_status`;
  connect = probe → (decide_deploy/scp|instruct) → start_remote → pick local free port →
  spawn tunnel (ChildHandle) → spawn watcher thread (emits `remote-disconnected`) → store
  handle (lock-discipline per the concurrency rule) → return local port.
- Register all four in `generate_handler!`; restructure builder tail to `.build()?.run(|app,
  event| …)` with `cleanup_all_remotes` on exit.
- Tests: command-level state transitions via injected MockRunner where feasible;
  serde round-trip of `Connection`; assert `capabilities/default.json` unchanged.

### W5 — FE connection layer (crate `aghub` desktop FE) — *depends on W4 signatures*

Files: `lib/store/connections.ts` (CRUD, `?? []`, `crypto.randomUUID`), `lib/store.ts`
re-export, `lib/connection-logic.ts` (pure: status projection, baseUrl derivation, Local
merge — node-testable), `contexts/connection.tsx`, `providers/connection.tsx`,
`hooks/use-connection.ts`, `App.tsx` (swap provider, listen for `remote-disconnected`),
delete `providers/server.tsx`. Keep `hooks/use-server.ts`/`use-api.ts` unchanged.
- Honor the no-`useEffect`-for-side-effects rule: drive `start_server`/`connect_remote`
  via `useMutation`/`useQuery`, not raw `useEffect` (the old ServerProvider's `useEffect`
  is removed).
- Tests (`node --test` where possible): store CRUD; status projection; baseUrl derivation;
  Local synthesis; cache-clear-on-switch logic.

### W6 — Connection switcher UI (crate `aghub` desktop FE) — *depends on W5,W4*

Files: `components/connection-switcher.tsx` (HeroUI v3 **Dropdown** with single-selection
connection rows + a non-selectable "Manage connections…" action row + status dot), mounted
in `components/app-sidebar.tsx` inside `<aside>` before `<nav>`;
`components/manage-connections-dialog.tsx` (HeroUI v3 Modal: label/sshTarget/user/port/
remoteAghubPath form + **Test connection** button → `test_connection` + edit/remove);
locale keys added to **all three** `lib/locales/{en,zh-Hans,zh-Hant}.ts`.
- Consult HeroUI v3 docs via the `heroui-react` skill (`.heroui-docs/react` does not exist).
- Use `cn` util; errors via Toast; ids via `crypto.randomUUID`.
- Tests: pure form-validation fn (`node --test`); rendering is manual-only (no runner).

## Build order

`W1 ∥ W2` → `W3` → `W4` (integration; verify `cargo test -p aghub` + `-p aghub-api`,
clippy, fmt) → `W5` → `W6` (verify `tsc` + `eslint`). Manual integration checklist
(needs the user's real VM) executed last.
