# GIT CRATE KNOWLEDGE BASE

**Crate**: `aghub-git` — clone/fetch with credential injection + pluggable
fetch backends (`RepoFetchBackend`: gix shallow / GitHub REST).

**Used by**: `aghub-api`, `aghub-cli`, `skill-update`, `aghub-cc-plugins` (NOT core).

## GOTCHAS

- **`inject_credentials` is HTTPS-only** — every other scheme is rejected
  (`credentials.rs`). Do not "fix" SSH by injecting into scp-style URLs.
- **Tokens never in error strings** — always `redact_url_userinfo` before surfacing.
- **`system_git.rs` shells out to the real `git` binary** when no explicit token
  applies so OS credential helpers (Windows Credential Manager, TFS/Azure DevOps)
  work. Do **not** collapse that path into pure `gix`.
- Auth precedence: explicit `CloneOptions::with_credentials` wins; env
  `GIT_USERNAME` / `GIT_PASSWORD` is the fallback. Token resolution lives in
  the **callers** — cli `source`/`check` map env `GITHUB_TOKEN`; the api
  resolves per-source tokens from its `credentials/` store (no `GITHUB_TOKEN`
  env). Do not add either mapping here.
- **Which backend serves a fetch is decided at RUNTIME, and each caches
  separately.** `GithubRest` yields to `GixShallow` on ANY non-2xx — a
  rate-limited anonymous request included — so a token-less dev machine
  routinely exercises gix while a user with a token exercises REST. A
  fetch/caching change verified on one path says NOTHING about the other.
  Assert on request COUNT through the `HttpTransport` seam, never on
  wall-clock: a timing only measures whichever path that machine happened
  to take.

## WHERE TO LOOK

| Task                            | File                           |
| ------------------------------- | ------------------------------ |
| Clone                           | `src/clone.rs`                 |
| Bare shallow (depth-1) fetch    | `src/fetch.rs`                 |
| Fetch backend trait             | `src/backend.rs`               |
| GitHub REST backend             | `src/github_rest.rs`           |
| Remote refs / branch listing    | `src/remote.rs`                |
| Materialize staged tree entries | `src/stage.rs` + `src/tree.rs` |
| Credential inject               | `src/credentials.rs`           |
| Source/URL normalize            | `src/source.rs`                |
| System-git fallback             | `src/system_git.rs`            |
| Redact                          | `src/redact.rs`                |

`backend.rs` (`RepoFetchBackend`) is the seam `skill-update`'s fallback chain
plugs into: `GithubRest` (github.com only, `HttpTransport` test seam) →
`GixShallow` → system git.

## ANTI-PATTERNS

- NEVER hardcode credentials
- NEVER log or return URLs with userinfo still present
- NEVER hold `TempDir` past the scope that needs the tree (auto-delete on drop)
