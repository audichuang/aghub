# GIT CRATE KNOWLEDGE BASE

**Crate**: `aghub-git` — clone/fetch with credential injection.

**Used by**: `aghub-api`, `aghub-cli`, `skill-update`, `aghub-cc-plugins` (NOT core).

## GOTCHAS

- **`inject_credentials` is HTTPS-only** — every other scheme is rejected
  (`credentials.rs`). Do not "fix" SSH by injecting into scp-style URLs.
- **Tokens never in error strings** — always `redact_url_userinfo` before surfacing.
- **`system_git.rs` shells out to the real `git` binary** when no explicit token
  applies so OS credential helpers (Windows Credential Manager, TFS/Azure DevOps)
  work. Do **not** collapse that path into pure `gix`.
- Preferred auth: env `GIT_USERNAME` / `GIT_PASSWORD`, or `CloneOptions::with_credentials`.

## WHERE TO LOOK

| Task                     | File                 |
| ------------------------ | -------------------- |
| Clone                    | `src/clone.rs`       |
| Treeless fetch (updates) | `src/fetch.rs`       |
| Credential inject        | `src/credentials.rs` |
| Source/URL normalize     | `src/source.rs`      |
| System-git fallback      | `src/system_git.rs`  |
| Redact                   | `src/redact.rs`      |

## ANTI-PATTERNS

- NEVER hardcode credentials
- NEVER log or return URLs with userinfo still present
- NEVER hold `TempDir` past the scope that needs the tree (auto-delete on drop)
