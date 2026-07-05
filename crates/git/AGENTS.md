# GIT CRATE KNOWLEDGE BASE

**Crate**: `aghub-git` — Git clone/fetch with credential injection\
**Used by**: `aghub-api`, `aghub-cli`, `skill-update`, `aghub-cc-plugins` (NOT `aghub-core`)

## STRUCTURE

```
crates/git/src/
├── lib.rs           # Public API surface (clone/fetch/resolve/redact exports)
├── clone.rs         # CloneOptions builder, clone_to_temp(), clone_to_path()
├── fetch.rs         # fetch_ref_to_temp() — treeless fetch (skill-update's workhorse)
├── credentials.rs   # read_credentials(), inject_credentials() (HTTPS-ONLY — rejects
│                    #   every other scheme), Credentials struct
├── source.rs        # resolve_remote_source() — shorthand/URL normalization; preserves
│                    #   ssh/scp forms, strips userinfo/passwords
├── system_git.rs    # fallback to the system `git` binary + OS credential helpers
│                    #   (Windows Credential Manager / GCM — the TFS/Azure DevOps path)
├── remote.rs        # RemoteOptions, resolve_ref_oid, remote-ref discovery
├── redact.rs        # redact_url_userinfo() — tokens never leak into errors
├── tree.rs          # materialize_tree()
└── error.rs         # GitError enum (thiserror)
```

## WHERE TO LOOK

| Task                    | File                 |
| ----------------------- | -------------------- |
| Clone a repo            | `src/clone.rs`       |
| Treeless fetch one ref  | `src/fetch.rs`       |
| Inject credentials      | `src/credentials.rs` |
| Normalize a source/URL  | `src/source.rs`      |
| System-git/OS-cred path | `src/system_git.rs`  |
| ls-refs / URL rewriting | `src/remote.rs`      |

## USAGE

```rust
// Via environment variables (preferred)
std::env::set_var("GIT_USERNAME", "user");
std::env::set_var("GIT_PASSWORD", "token");
let temp = clone_to_temp(CloneOptions::new("https://github.com/user/repo.git"))?;

// Or explicit credentials
let temp = clone_to_temp(
    CloneOptions::new("https://github.com/user/repo.git")
        .with_credentials("user", "token")
)?;
// temp dir auto-cleaned on drop
```

## ENV VARS

- `GIT_USERNAME` — Git username for auth
- `GIT_PASSWORD` — Git password or personal access token

## DEPENDENCIES

Uses `gix` (pure Rust git) for token-based flows. Features: `blocking-network-client`, `worktree-mutation`, `blocking-http-transport-reqwest-native-tls`. **`system_git.rs` deliberately shells out to the system `git` binary** when no explicit token applies, so OS credential helpers (Windows Credential Manager, NTLM/Kerberos for TFS/Azure DevOps) keep working — don't "simplify" that path into gix.

## ANTI-PATTERNS

- NEVER hardcode credentials — always read from env or `CloneOptions.with_credentials()`
- NEVER hold `TempDir` beyond the scope where cloned files are needed (auto-deleted on drop)
