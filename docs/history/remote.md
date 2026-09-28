# remote history

Incident history moved out of `crates/remote` code comments. The code keeps
the current rule; each entry here keeps what happened and why.

## Tunnel joined user ControlMaster

With a user `ControlMaster auto` + `ControlPersist` entry for the host in
`~/.ssh/config`, the tunnel `ssh -L` process did not open its own connection:
it handed the forward to the existing master and exited 0 immediately while the
forward stayed up under the master. Every caller reads the child process as the
tunnel — bring-up treats an early exit as a failed forward, the watcher treats
exit as a disconnect, teardown kills it to close the forward — so bring-up
reported `ssh tunnel exited early (exit status: 0)` and tore down a remote
server whose forward was in fact working. Rule: `build_tunnel_args` passes
`ControlPath=none` and `ControlMaster=no`; one-shot commands may still reuse
the user's master. Commit `e163f941`.

## Remote install hardening

v2.7.3 hardening of remote install (commit `5f7b0cc6`):

- **Version stamp.** A `cargo install --git` checkout has no tag refs, so the
  `aghub-api` build script's `git describe` fallback failed and the binary
  reported the workspace manifest placeholder (`1.1.1`). The desktop never
  treats that as compatible, so it reinstalled forever. The cargo install now
  stamps `AGHUB_RELEASE_VERSION` — the desktop's version, or the pinned tag's
  version when a tag (and no branch) is what gets installed.
- **Staging nonce.** A fixed staging file let two racing installs (an auto
  upgrade overlapping a user "Reinstall") corrupt each other: install A
  validated the staged file while B overwrote it mid-copy, then A renamed B's
  truncated upload into place. `install_nonce` gives every install its own
  `.upload.<nonce>` path.
- **Unset stage variable.** Under `bash -lc`, an early failure (before `stage`
  was assigned) still reached the `rm -f "$stage"` cleanup, and an unset
  `stage` could resolve to whatever the login profile bound to that name (e.g.
  `stage=~/.ssh/authorized_keys`). Scripts now initialize `stage`/`tmp` to
  empty first and guard cleanup on `[ -n ... ]`.
- **Best-effort upgrade.** A failed patch upgrade used to sever an
  already-usable connection; `ensure_remote_api` now falls back to the
  pre-install probe when it was present + compatible.

## Redeploy pkill self-DoS

`force_redeploy_remote_api` used to `pkill` the running server by name/path
before replacing the binary. The redeploying connection had no pid for that
server (it did not start it), so on a shared host the by-path kill also reaped
sibling connections' servers running the same binary. Rule: never kill; the
atomic `mv` swap is safe while the old process holds the previous inode, and
the old server is left as an orphan on its own `--port 0`. Commit `2f2ca513`.
