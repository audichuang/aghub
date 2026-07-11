# REMOTE CRATE KNOWLEDGE BASE

**Crate**: `aghub-remote` — SSH-based remote VM management (deploy + run `aghub-api` on a remote host).

## OVERVIEW

Drives the lifecycle **probe → ensure (install if missing) → start → poll/log**, parses `~/.ssh/config`, and uploads/cleans files over SSH/SCP. Exposed via the **desktop Tauri layer only — NOT the HTTP API** (`crates/api` does not depend on this crate).

## STRUCTURE

```
src/
├── lib.rs          # Public exports + ConnState / ConnectError / RunError
├── ssh.rs          # Pure arg builders: build_ssh_args / build_scp_args / build_tunnel_args; is_version_compatible
├── ssh_config.rs   # Parse ~/.ssh/config (host → connection params)
├── bringup.rs      # probe → ensure_remote_api (install if missing; auto-upgrade on patch drift) → start_remote; force_redeploy_remote_api
├── fs.rs           # Remote dir listing + chunked upload (prepare/cat/finish)
└── test_support.rs # test helpers for asserting on built argv
```

## WHERE TO LOOK

| Task                        | Location                                                                                |
| --------------------------- | --------------------------------------------------------------------------------------- |
| Build an ssh/scp/tunnel cmd | `src/ssh.rs` (`build_*` fns)                                                            |
| Probe / ensure / redeploy   | `src/bringup.rs` (`probe_connection`, `ensure_remote_api`, `force_redeploy_remote_api`) |
| Version wire contract       | `src/ssh.rs` (`is_version_compatible` — major.minor)                                    |
| Parse ssh config            | `src/ssh_config.rs`                                                                     |
| Remote file transfer        | `src/fs.rs` (`build_remote_*_cmd`)                                                      |

## COMMANDS

```bash
cargo test -p aghub-remote
```

## GOTCHAS / ANTI-PATTERNS

- **Command construction is intentionally pure** (`build_*_cmd` / `build_*_args` return argv) and is the test seam — assert on the built command, don't shell out in tests (see `test_support.rs`).
- Only the desktop consumes this crate; do **not** wire it into `crates/api`.
- `ensure_remote_api` gates whether a remote install is needed (and refuses a cross-platform `LocalBinary` install before scp); `force_redeploy_remote_api` stages-then-atomic-mv (no pkill). `is_version_compatible` is the major.minor wire/version contract — keep the API's `--version` / `AGHUB_API_PORT=` output in sync with it.
