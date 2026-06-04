# REMOTE CRATE KNOWLEDGE BASE

**Crate**: `aghub-remote` — SSH-based remote VM management (deploy + run `aghub-api` on a remote host).

## OVERVIEW

Drives the lifecycle **probe → decide → install → start → poll/log**, parses `~/.ssh/config`, and uploads/cleans files over SSH/SCP. Exposed via the **desktop Tauri layer only — NOT the HTTP API** (`crates/api` does not depend on this crate).

## STRUCTURE

```
src/
├── lib.rs          # Public exports + ConnState / ConnectError / RunError
├── ssh.rs          # Pure arg builders: build_ssh_args / build_scp_args / build_tunnel_args
├── ssh_config.rs   # Parse ~/.ssh/config (host → connection params)
├── bringup.rs      # probe → decide_deploy → install_remote_api → ensure_remote_api; version compat
└── fs.rs           # Remote dir listing + chunked upload (prepare/cat/finish)
```

## WHERE TO LOOK

| Task                        | Location                                                    |
| --------------------------- | ----------------------------------------------------------- |
| Build an ssh/scp/tunnel cmd | `src/ssh.rs` (`build_*` fns)                                |
| Decide install vs reuse     | `src/bringup.rs` (`decide_deploy`, `is_version_compatible`) |
| Parse ssh config            | `src/ssh_config.rs`                                         |
| Remote file transfer        | `src/fs.rs` (`build_remote_*_cmd`)                          |

## COMMANDS

```bash
cargo test -p aghub-remote
```

## GOTCHAS / ANTI-PATTERNS

- **Command construction is intentionally pure** (`build_*_cmd` / `build_*_args` return argv) and is the test seam — assert on the built command, don't shell out in tests (see `test_support.rs`).
- Only the desktop consumes this crate; do **not** wire it into `crates/api`.
- `decide_deploy` / `is_version_compatible` gate whether a remote install is needed — change them together when bumping the wire/version contract.
