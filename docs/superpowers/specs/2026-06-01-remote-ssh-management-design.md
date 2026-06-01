# Remote SSH Management — Design Spec

**Date:** 2026-06-01
**Status:** Approved (brainstorming) → ready for implementation plan
**Surface:** Desktop (Tauri) app only

## 1. Goal

Let the user manage AI-agent configuration (skills / MCP servers / sub-agents) on a
remote VM from the local desktop app, exactly as they manage the local machine today.
The VM and the local machine are independent targets. The user selects which target
("connection") is active; every existing page then reflects that target.

Out of scope for v1: bi-directional sync/diff between local and VM, password-based SSH
auth, CLI support, parallel multi-host operations, editing `~/.ssh/config` from the app.

## 2. Core decisions (from brainstorming)

| Decision | Choice |
| --- | --- |
| Use case | Remote *management* of the VM's own configs (not sync) |
| Surface | Desktop app |
| Transport | Run `aghub-api` **on the VM**, reach it via an **SSH tunnel**; the frontend swaps `baseUrl` |
| SSH auth | Shell out to the **system `ssh`/`scp`**, reusing `~/.ssh/config`, `ssh-agent`, keys, `known_hosts`. **No credentials stored by the app.** |
| Hosts | **Multiple** connections, with a switcher (Local + N remotes) |

## 3. Why this transport

`useApi()` builds its HTTP client from `useServer().baseUrl`, and clients are cached
per-baseUrl (`requests/client.ts`). Therefore **changing the active `baseUrl` re-points
every existing request at the new target with no page-level changes.** Running the same
`aghub-api`/`aghub-core` on the VM means remote behavior is identical to local by
construction (correct home dir, path resolution, availability detection, writes). SSH is
purely a secure transport + tunnel; no parallel "remote-only" logic is introduced.

## 4. Architecture

### 4.1 Connection model (frontend + Tauri store)

```
Connection = {
  id: string,              // uuid
  label: string,           // user-facing name
  sshTarget: string,       // host or ~/.ssh/config alias (e.g. "my-vm")
  user?: string,           // optional, overrides ssh config
  port?: number,           // optional ssh port
  remoteAghubPath?: string // optional explicit path to aghub-api on the VM
}
```

- Persisted in the Tauri store (same mechanism as `projects` / `disabledAgents`:
  `lib/store/connections.ts`, mirroring `lib/store/agents.ts`).
- An implicit **Local** connection (id `"local"`) always exists and equals current behavior.
- A single **active connection id** is held in app state (React), default `"local"`.

### 4.2 Remote bring-up (Tauri Rust backend)

New module `crates/desktop/src-tauri/src/commands/remote.rs` (+ a `ssh.rs` helper).

A `CommandRunner` trait abstracts process execution so the pure logic is unit-testable
without a real VM:

```rust
trait CommandRunner {
    fn run(&self, program: &str, args: &[String]) -> Result<Output, RunError>;     // blocking, capture
    fn spawn(&self, program: &str, args: &[String]) -> Result<ChildHandle, RunError>; // long-lived (tunnel)
}
struct SystemRunner;   // real ssh/scp
struct MockRunner;     // tests: scripted stdout/stderr/exit per (program,args)
```

Pure, test-first functions (no I/O):

- `build_ssh_args(conn, remote_cmd) -> Vec<String>`
- `build_scp_args(conn, local_path, remote_path) -> Vec<String>`
- `build_tunnel_args(conn, local_port, remote_port) -> Vec<String>`
  (`ssh -N -o BatchMode=yes -o ExitOnForwardFailure=yes -L <local>:127.0.0.1:<remote> <target>`)
- `parse_remote_port(stdout) -> Option<u16>`  — parses `AGHUB_API_PORT=<n>`
- `parse_api_version(stdout) -> Option<String>` + `is_version_compatible(local, remote) -> bool`
- connection state transitions: `Disconnected → Probing → (Deploying) → Starting → Tunneling → Connected | Error`

**Security:** every `ssh`/`scp` invocation passes arguments as an **argv array** (never a
joined shell string) so a hostile `sshTarget`/path cannot inject commands. All ssh runs
non-interactively with `-o BatchMode=yes`. The remote server binds `127.0.0.1` only; the
tunnel forwards over loopback on both ends.

Tauri commands (registered in `lib.rs invoke_handler`):

- `test_connection(connection) -> TestResult` — runs `ssh <target> <aghub-api> --version`,
  returns `{ reachable, apiPresent, apiVersion, compatible, message }`. Never mutates.
- `connect_remote(connection) -> u16` — full bring-up, returns the **local** tunnel port.
- `disconnect_remote(connectionId)` — kill tunnel child + remote server pid.
- `remote_status(connectionId) -> Status` — current state for the UI indicator.

Bring-up sequence inside `connect_remote`:

1. **Probe**: `ssh <target> <aghub-api> --version`. Determine presence + compatibility.
2. **Deploy (best-effort, same-platform only)**: if absent and the desktop bundles a
   matching-platform `aghub-api` binary (detected via `ssh <target> uname -s -m`),
   `scp` it to `~/.local/bin/aghub-api` (or `remoteAghubPath`). If platforms differ or no
   bundled binary, return a structured `RemoteApiMissing` error containing the install
   command (`cargo install --path crates/api` / `just install`). v1 primary path is
   detect-or-instruct; auto-deploy is the same-arch convenience.
3. **Start**: `ssh <target> 'nohup <aghub-api> --port 0 >/tmp/aghub-api.<id>.log 2>&1 & echo PID=$!'`.
   Capture the PID. Poll `ssh <target> 'cat /tmp/aghub-api.<id>.log'` until
   `AGHUB_API_PORT=<n>` appears (bounded timeout, e.g. 10 s). This is the VM-side port.
4. **Tunnel**: spawn `ssh -N -L <freeLocalPort>:127.0.0.1:<remotePort> <target>` as a
   managed child; `freeLocalPort` chosen via the existing `127.0.0.1:0` bind trick.
5. Store `RemoteHandle { tunnel_child, remote_pid, connection_id }` in app state; return
   `freeLocalPort`.

Lifecycle: `RemoteState { handles: Mutex<HashMap<String, RemoteHandle>> }` in `AppState`.
`disconnect_remote` and app-exit cleanup kill the tunnel child and `ssh <target> kill <pid>`.

### 4.3 `aghub-api` binary change (`crates/api/src/main.rs`)

The standalone binary must support ephemeral-port selection and report it:

- Parse args: `--port <n>` (default `0`), `--version` (prints `aghub-api <semver>` and exits).
- When `--port 0`: pick a free port via `TcpListener::bind("127.0.0.1:0")` (mirrors the
  desktop helper), then start Rocket on it.
- Print `AGHUB_API_PORT=<port>` to stdout immediately before `launch()` so the SSH caller
  can parse it from the log.

Test-first targets: arg parsing, free-port selection helper, version string. (The Rocket
launch itself stays integration-tested.)

### 4.4 Frontend connection layer

- `providers/server.tsx` → evolves into `providers/connection.tsx` (`ConnectionProvider`).
  It owns: connection list (from store), active connection id, per-connection status, and
  the resolved `baseUrl`.
  - Local active → ensures local server via existing `start_server`, `baseUrl = http://localhost:<localPort>/api/v1`.
  - Remote active → calls `invoke("connect_remote", { connection })`, `baseUrl = http://localhost:<tunnelPort>/api/v1`.
- **Backward compatibility:** `ConnectionProvider` still supplies the existing
  `ServerContext` value `{ port, baseUrl }`, so `useApi()` / `useServer()` keep working
  unchanged. A new `useConnection()` exposes the richer API (list, active, status,
  `connect`, `disconnect`, `addConnection`, `updateConnection`, `removeConnection`,
  `setActive`).
- **Cache isolation on switch:** on active-connection change, call
  `queryClient.clear()` (or `removeQueries`) so VM data never bleeds into Local view and
  vice-versa. This avoids touching the `queryKeys` factory. `AgentAvailabilityProvider`
  re-fetches against the new `baseUrl` automatically (availability is per-target by design;
  `disabledAgents` remains a global user preference for v1).

### 4.5 Connection switcher UI

- A switcher in the sidebar header (HeroUI v3 `Dropdown`/`Select`): lists Local + remotes,
  shows the active one + a status dot (idle / connecting / connected / error).
- An "Add / manage connections" dialog (HeroUI v3 `Modal`): form for label / sshTarget /
  user / port / remoteAghubPath, a **Test connection** button (calls `test_connection`),
  and edit/remove of existing connections.
- All built per the desktop HeroUI v3 rules (compound components, `cn` util, no `useEffect`
  for data fetching, toast for errors).

## 5. Data flow

```
select VM → ConnectionProvider.setActive(vm)
          → invoke connect_remote(vm)
              → ssh probe/deploy/start (VM-side aghub-api on 127.0.0.1:R)
              → spawn ssh -L L:127.0.0.1:R vm   (tunnel)
              → returns L
          → baseUrl = http://localhost:L/api/v1
          → queryClient.clear()
          → every page re-fetches via useApi() against the VM
edits → POST/PUT/DELETE through the tunnel → VM's aghub-core writes the VM's files
```

## 6. Error handling

- SSH unreachable / auth / unknown host key (BatchMode fails) → structured error surfaced
  via toast + connection status `error`, including `ssh` stderr. v1 does **not**
  auto-accept unknown host keys; the message tells the user to `ssh` in once manually.
- `RemoteApiMissing` / incompatible version → actionable message with the install command.
- Port line not seen within timeout → kill the started process, report `start failed`.
- Tunnel child dies → status flips to `disconnected`; UI offers reconnect.
- `connect_remote` timeout (overall) bounded; partial bring-up is cleaned up on failure.

## 7. Testing strategy

- **Rust unit (TDD, real passing tests):** `build_ssh_args` / `build_scp_args` /
  `build_tunnel_args` argv shape; `parse_remote_port`; `parse_api_version` +
  compatibility; state-machine transitions driven through `MockRunner`; cleanup on
  failure paths. `crates/api`: arg parsing + free-port helper + version string.
- **Frontend unit:** `connections.ts` store CRUD; `ConnectionProvider` reducer/state
  transitions (active switch clears cache; status derivation). `tsc` + `eslint` clean.
- **Manual integration (needs a real VM — the user's):** end-to-end connect, list/add/edit
  skills+MCP on the VM, disconnect, reconnect, error cases. Documented as a checklist; not
  run in CI (no VM available there).

## 8. Files touched (anticipated)

- `crates/api/src/main.rs` — args, ephemeral port, `AGHUB_API_PORT=` line, `--version`.
- `crates/desktop/src-tauri/src/commands/ssh.rs` — `CommandRunner`, `SystemRunner`, arg builders, parsers (new).
- `crates/desktop/src-tauri/src/commands/remote.rs` — connection commands + state machine (new).
- `crates/desktop/src-tauri/src/commands/mod.rs` — re-exports.
- `crates/desktop/src-tauri/src/lib.rs` — register commands, add `RemoteState` to `AppState`, app-exit cleanup.
- `crates/desktop/src/lib/store/connections.ts` — store CRUD (new).
- `crates/desktop/src/lib/store.ts` — re-export.
- `crates/desktop/src/contexts/connection.tsx` — context type (new).
- `crates/desktop/src/providers/connection.tsx` — provider (replaces/wraps `server.tsx`).
- `crates/desktop/src/hooks/use-connection.ts` — hook (new); keep `use-server.ts` shim.
- `crates/desktop/src/components/connection-switcher.tsx` + `manage-connections-dialog.tsx` (new).
- `crates/desktop/src/App.tsx` — swap `ServerProvider` → `ConnectionProvider`; mount switcher.
- `crates/desktop/src-tauri/capabilities/*` — permissions for spawning processes if required.

## 9. Risks / open items

- **Cross-arch deploy:** auto-`scp` only works VM↔desktop same OS+arch. v1 falls back to
  detect-or-instruct. (User case: Linux desktop → Linux x86_64 VM = supported.)
- **SSH connection churn:** each step is a fresh `ssh` (re-auth). Acceptable with
  ssh-agent/keys; `ControlMaster` multiplexing is a later optimization.
- **`tauri_plugin_process` / capability scope** for spawning `ssh` must be confirmed; may
  need a `tauri-plugin-shell` allowlist entry restricted to `ssh`/`scp`.
- **Windows** desktop lacks a guaranteed system `ssh`; v1 targets the user's Linux desktop,
  Windows handled later.
