# Remote SSH Management — Manual Integration Checklist

**Date:** 2026-06-01

The pure logic (W1–W3, W5/W6 pure modules) is unit-tested and the Rust/TS both
compile + lint clean. The pieces below cannot be unit-tested without a real VM
and a running desktop build; verify them by hand once.

## Prerequisites

- A reachable VM you can `ssh <target>` into **non-interactively** (key in
  `~/.ssh`, host in `~/.ssh/known_hosts`, ssh-agent loaded). `ssh -o BatchMode=yes
<target> true` must succeed from the desktop machine.
- `aghub-api` installed on the VM and on `PATH` (v1 is detect-or-instruct):
  build/install it on the VM (`cargo install --path crates/api` or `just install`).
  Confirm `ssh <target> aghub-api --version` prints `aghub-api 1.1.1`.
- Desktop app built and running (`bun run tauri dev`).

## Checklist

1. **Add a connection.** Sidebar → connection switcher → "Manage connections…" →
   add `{ label, sshTarget }` (+ optional user/port/remoteAghubPath). Save.
2. **Test connection.** Click _Test connection_. Expect a success toast with
   `reachable=true, apiPresent=true, compatible=true`. Then test a bad host →
   expect a failure toast carrying ssh stderr (and that it does **not** hang on
   an unknown host key — BatchMode fails closed).
3. **Connect (happy path).** Select the VM in the switcher. Status → connecting →
   connected. The Skills / MCP / Sub-agents pages now show the **VM's** resources
   (cross-check against `ssh <target> ls ~/.claude/skills`).
4. **Edit on the VM.** Add/disable a skill or MCP while the VM is active; confirm
   the change lands on the VM's files (`ssh <target> cat …`), not locally.
5. **Switch back to Local.** Pages show local resources again; no VM data bleeds
   through (cache cleared on switch).
6. **`aghub-api` missing.** On a VM without `aghub-api`, connect → expect a
   `remoteApiMissing` error surfaced with the install hint.
7. **Tunnel death.** While connected, `ssh <target> pkill -f 'aghub-api --port'`
   (or drop the network). Expect the `remote-disconnected` toast and the UI
   returning to a safe state (Local).
8. **Disconnect.** Disconnect the VM; confirm `ssh <target> pgrep -af aghub-api`
   shows the remote server gone (guarded kill) and no `ssh -L` tunnel lingers
   locally (`pgrep -af 'ssh -N -L'`).
9. **App exit cleanup.** Connect, then quit the app. Confirm no orphaned remote
   `aghub-api` on the VM and no orphaned local tunnel process.
10. **Two VMs.** Add a second connection; switch between both and Local; confirm
    each shows its own data and only one tunnel per active host exists.

## Known v1 limitations (documented, not bugs)

- **Detect-or-instruct only** — no auto-`scp` deploy yet (needs a bundled
  matching-platform `aghub-api`); the Scp decision branch exists + is unit-tested.
- **Same-network/SSH assumptions** — fresh `ssh` per bring-up step (no
  ControlMaster multiplexing); compatibility is `major.minor` equality.
- `disabledAgents` is a global preference (not per-connection).
- Windows desktop is untested (no guaranteed system `ssh`); v1 targets Linux/macOS.
- If the network is down at quit time, the remote guarded-kill can't run and may
  leave the VM `aghub-api` until its next manual cleanup.
