# REMOTE CRATE KNOWLEDGE BASE

**Crate**: `aghub-remote` — SSH remote VM lifecycle (probe → ensure → start →
poll/log). **Desktop Tauri only — not the HTTP API** (`crates/api` must not
depend on this crate).

## WHERE TO LOOK

| Task                      | Location                                                                            |
| ------------------------- | ----------------------------------------------------------------------------------- |
| ssh/scp/tunnel argv       | `ssh.rs` (`build_*` pure builders — the test seam)                                  |
| Probe / ensure / redeploy | `bringup.rs` (`probe_connection`, `ensure_remote_api`, `force_redeploy_remote_api`) |
| Version wire contract     | `ssh.rs` `is_version_compatible` (major.minor)                                      |
| Parse `~/.ssh/config`     | `ssh_config.rs`                                                                     |
| Remote file transfer      | `fs.rs`                                                                             |

## GOTCHAS / ANTI-PATTERNS

- **Command construction is pure** (`build_*` return argv) — assert on argv in
  tests; don't shell out (see `test_support.rs`)
- Only desktop consumes this crate — do **not** wire into `crates/api`
- `ensure_remote_api` gates install (refuses cross-platform `LocalBinary` before
  scp); `force_redeploy_remote_api` stages-then-atomic-mv (**no pkill**)
- Keep remote API `--version` / `AGHUB_API_PORT=` output in sync with
  `is_version_compatible`

```bash
cargo test -p aghub-remote
```
