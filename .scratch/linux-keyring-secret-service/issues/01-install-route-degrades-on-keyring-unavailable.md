# 01 — `POST /skills/install` degrades silently when the keyring backend is unreachable

**Status:** open (deferred)

**Where:** `crates/api/src/routes/skills.rs:1313`, inside
`install_skill_route_with_repo`:

```rust
let keyring = crate::routes::skills_update::KeyringResolver::load().await;
```

## Root cause

`KeyringResolver::load()` is the NON-fail-closed loader: any read failure —
including "the backend itself is unreachable" (Linux secret-service with no
D-Bus session, a locked keychain, ...) — degrades to an empty snapshot
(`creds: vec![]`, `bindings: default()`), never an error. `install_skill`
uses this loader (not `load_or_unavailable`, which the sibling mutating
routes `apply_skill_update`/`accept_skill_rename`/git-scan all use — see
`crates/api/AGENTS.md` and the round-2 GitHub #15 fixes).

Consequence when the keyring backend is down:

- A **public** source installs anonymously (no credential resolves, same as
  "no credential bound" — usually still works fine for a public repo).
- A **private** source's clone fails with a generic clone/auth error instead
  of a stable, retryable 503 `KEYCHAIN_UNAVAILABLE` — the caller can't tell
  "this source needs a credential I don't have" apart from "the credential
  store itself is broken right now."

## Why this is deferred, not fixed now

Round-2 Codex review flagged this as lower severity than the apply-update/
accept-rename/git-scan cases (P2-3) that got the `load_or_unavailable` fix:
installing a brand-new skill is a materially different operation from
updating/renaming one already on disk — there is no existing installed
state to protect, and letting a public source install anonymously when the
keyring happens to be unreachable is an acceptable, arguably even desirable,
degradation (it still succeeds for the common case). The private-source
failure mode (confusing clone error instead of 503) is a real papercut, but
not a data-safety issue, and fixing it means auditing whether callers of
`install_skill_route_with_repo` / `install_skill_with_repo` already handle a
503 `ApiResult` shape consistently with the other mutating routes' clients
(desktop UI error surfacing) — worth doing as its own pass, not bundled into
this branch's keyring/secret-service fixes.

## Suggested fix (when picked up)

Switch line 1313 to
`crate::routes::skills_update::KeyringResolver::load_or_unavailable().await?`
(same pattern as `apply_skill_update`/`accept_skill_rename` — see
`LazyKeyringFallback` in `crates/api/src/routes/skills_update.rs` for the
forwarded-token-aware variant, since git-scan-style forwarding may also
apply to installs from a remote-forwarded desktop session). Add a
regression test mirroring
`apply_skill_update_route_fails_closed_when_keyring_backend_unreachable`,
using the `crate::credentials::test_hooks::ForceCredentialBackendUnavailable`
injection hook (cross-platform, no DBUS).
