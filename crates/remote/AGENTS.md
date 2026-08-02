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
| Capability probe          | `ssh.rs` `CAP_GIT_CREDENTIAL_FORWARDING` + `bringup.rs` `--capabilities` probe      |
| Parse `~/.ssh/config`     | `ssh_config.rs`                                                                     |
| Remote dir browsing       | `fs.rs` (VM project picker; scp argv lives in `ssh.rs` `build_scp_args`)            |

## GOTCHAS / ANTI-PATTERNS

- **Command construction is pure** (`build_*` return argv) — assert on argv in
  tests; don't shell out (see `test_support.rs`)
- Only desktop consumes this crate — do **not** wire into `crates/api`
- `ensure_remote_api` gates install (refuses cross-platform `LocalBinary` before
  scp — `CargoGit` / `ReleaseDeb` build/download **on the VM** and are un-gated)
  and best-effort auto-upgrades on version mismatch;
  `force_redeploy_remote_api` stages-then-atomic-mv (**no pkill**)
- The remote-API wire contract is `--version` / `AGHUB_API_PORT=` /
  `--capabilities` — keep output in sync with `is_version_compatible`, and note
  `CAP_GIT_CREDENTIAL_FORWARDING` **mirrors** the `aghub_api::cli` constant
  (deliberately no api dependency); probe is fail-safe-false and additive
