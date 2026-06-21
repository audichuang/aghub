# Remote git-credential forwarding (controller-side resolution)

**Status:** design approved & Codex-reviewed; revised per review (2026-06-22); not yet implemented
**Date:** 2026-06-22

> **Revision note (post Codex review).** The original design resolved the token
> via a new unauthenticated HTTP endpoint and forwarded tokens for every source.
> Codex flagged three P1 issues; this revision adopts them:
> 1. **Token resolution moved off HTTP onto a Tauri command** (D2 → B1) — removes
>    the web-reachable raw-token endpoint entirely, and (bonus) removes the need
>    to expose `localBaseUrl` to the renderer.
> 2. **`check-updates` forwards only sources with an explicit binding** (D4
>    refined) — the current `GET /skills/sources` hard-codes `is_private:false`,
>    so the FE cannot otherwise know which sources are private.
> 3. **Remote compatibility needs an explicit feature/protocol gate** — the
>    existing bring-up only checks equal `major.minor` and would silently accept
>    an old remote that ignores the header.

## Motivation

The desktop app supports SSH-remote control: it brings up `aghub-api` on a
remote VM and the frontend's baseUrl is swapped to a loopback SSH tunnel
(`ssh -N -L 127.0.0.1:<local>:127.0.0.1:<remote>`, see
`crates/remote/src/ssh.rs::build_tunnel_args`). **All** API calls in remote mode
therefore execute on the VM's `aghub-api` process.

Git credentials are resolved from the **OS keyring of whatever machine runs the
api process**:

- token store — `keyring::Entry::new("aghub", "github_credentials")`
  (`crates/api/src/routes/credentials.rs`)
- source→credential bindings —
  `keyring::Entry::new("aghub", "skill_source_bindings")`
  (`crates/api/src/credentials/resolve.rs`)
- resolution — `resolve_token_for_source(...)`, called from
  `sources.rs::diff_source`, `skills_update.rs::check_updates`, and the git
  scan path (`skills.rs::git_scan_skills`).

In remote mode this resolves against the **VM's** keyring, which produces two
failure modes:

1. **Token on the wrong machine.** A credential bound in the Mac app lives in the
   Mac keyring; the VM keyring is empty, so a private source check returns
   `needs_credential: true` → UI shows "此來源為私有且無可用憑證．請綁定憑證後再檢查".
2. **The VM may have no keyring backend.** The Linux `keyring` crate uses the
   Secret Service (D-Bus / gnome-keyring). A headless VM often has none, so
   `set_password()` itself fails (`KEYCHAIN_ERROR`) — the user cannot even bind a
   credential while remote.

This was never designed for "controller machine ≠ executor machine". This spec
makes the **controller (Mac) the single source of truth** for credentials and
**forwards a per-source token over the SSH tunnel per request**, used in-memory
by the VM and never persisted there.

---

## Understanding summary

- **What:** in remote mode, resolve git credentials on the controller (Mac) and
  forward them to the VM `aghub-api` per request; the VM uses them in-memory for
  the fetch and never persists them.
- **Why:** the VM keyring is empty (and may not exist), so credentialed git
  fetches on the VM always fail today.
- **Who:** users driving one or more remote VMs from the Mac desktop app.
- **Truth source:** credentials + source→credential bindings stay in the **Mac
  keyring** (existing `/credentials` + source-bindings); the remote stores
  nothing new.
- **Non-goals (YAGNI):** no VM-side file-backed credential store (option B); no
  SSH-agent forwarding (option C); no change to the API's overall no-auth
  posture; no change to the CLI `GIT_PASSWORD` / `GITHUB_TOKEN` env flow.

---

## Decision log

| #   | Decision                                                                              | Alternatives considered                                  | Why                                                                                                                |
| --- | ------------------------------------------------------------------------------------- | -------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------ |
| D1  | **Controller-side resolution + per-request forwarding** over the tunnel               | B: persist on VM (+file store); C: SSH-agent forwarding  | Keeps the VM stateless, single source of truth, sidesteps headless-keyring, fits the `TokenResolver` seam.          |
| D2  | **(revised) Resolve via a Tauri command**, renderer attaches the forward header       | A: HTTP `resolved-token` endpoint; A′: HTTP + session secret; B2: Tauri proxy / native request | Codex P1: an unauthenticated, all-origin, `credentials:true` localhost API must not expose a raw-token HTTP endpoint (a local web page that scans the port could exfiltrate it). A Tauri command is not web-reachable, needs no bespoke API auth, and is a minimal delta. B2 (token never in renderer) was rejected as not worth duplicating request logic in Rust for a renderer that runs the app's own trusted code. |
| D3  | **Cover all git-auth remote paths** (sources/diff, check-updates, git scan/install/sync) | sources/diff only                                        | A half-fix where diff works but "install" fails is worse UX than no fix.                                            |
| D4  | **(refined) Minimal-privilege / per-source**, and for `check-updates` forward **only sources with an explicit binding** | Forward the whole creds+bundle; forward for every source incl. host-fallback | Codex P1: the FE cannot identify private sources from the current sources list. Only explicit bindings are enumerable controller-side, and limiting to them is the strictest least-privilege.    |
| D5  | **Dedicated header** `X-Aghub-Git-Tokens` (base64 JSON `{source: token}`) added to the CORS allowlist | Reuse `Authorization: Bearer …`                          | Avoids colliding with a future `ApiAuth` on `Authorization`; one-line CORS change, intentful. (Still required: the renderer→remote request carries this custom header.) |
| D6  | **Explicit per-request injection** in the request layer (source is known)             | Global `ky` `beforeRequest` interceptor                  | An interceptor couples endpoint allowlist / source param into the client factory and is hard to test.              |
| D7  | **Remote feature/protocol gate** — forwarding only engaged against a remote known to support it | Rely on existing `major.minor` compat                    | Codex P1: existing compat accepts equal `major.minor`; an old patch-level remote would silently ignore the header. Gate on a capability/min-protocol-version, or force a minor bump. |
| D8  | **Host-pin to normalized origin** `(scheme, host, port)`, generalized from github.com-only | Reuse port-agnostic `same_host`                          | Codex P2: self-hosted GitLab on a non-default port must not be conflated; pin the forwarded token to the source's resolved clone-URL origin. |

---

## Architecture & data flow

Three elements (names are the implementation proposal):

1. **Tauri command `resolve_git_token(source) → Option<{token, origin}>`**
   (desktop `src-tauri`). It calls a new public
   `aghub_api::resolve_git_token_for_source(source)` that wraps the existing
   `load_source_bindings()` + `load_credentials()` + `resolve_token_for_source()`
   against the **local (Mac) keyring**. The embedded api is in-process, so this
   is a direct Rust call — **no HTTP, no port, not web-reachable.** A companion
   `list_bound_sources() → Vec<source>` command reads the bindings store
   **in-process** (NOT an HTTP call to the local api — that would reintroduce a
   localBaseUrl dependency) so the FE can enumerate explicitly-bound sources for
   `check-updates`.
   - **Origin-aware (Codex 2nd-pass P2):** the return must carry the source's
     resolved clone-URL **origin** `(scheme, host, port)`, not just a host
     string. Today `keychain_host_for_source()` returns host-only and the
     host-fallback in `resolve_token_for_source` matches `cred.name == host`
     (`crates/api/src/credentials/resolve.rs`); D8 pinning needs origin, so this
     wrapper introduces an origin-aware resolved-token type rather than reusing
     the host-only helper verbatim.
2. **Forward header** `X-Aghub-Git-Tokens: <base64(JSON {source: token})>`,
   attached by the renderer to the request sent to the **remote** api. One
   uniform mechanism: single-source routes carry one entry, `check-updates`
   carries the (bound-only) map.
3. **Remote `ForwardedTokenResolver`** (parsed by a Rocket request guard).
   Resolver routes use `ChainResolver(forwarded, KeyringResolver)` — header
   first, keyring fallback. The git scan path uses a present forwarded token
   directly as its credential.

### Flow — `sources/diff` (remote active)

1. FE wants the diff for source `S`.
2. FE invokes the Tauri command `resolve_git_token(S)` → `{token, host}` (from
   the Mac keyring binding), or nothing.
3. FE → **remote** api `GET /skills/sources/diff?source=S` with
   `X-Aghub-Git-Tokens: {S: T}`.
4. The remote `diff_source` resolver returns `T` (from the header) instead of
   reading the VM keyring → fetch succeeds.

### Flow — `check-updates` (multi-source, bound-only)

`check-updates` scans a whole scope and resolves a token **per lock entry** on
the server, so a single token header cannot cover it, and the FE cannot tell
which sources are private (`GET /skills/sources` hard-codes `is_private:false`).
The FE therefore enumerates the **explicitly bound** sources (controller-side),
resolves a token per bound source via the Tauri command, assembles the map, and
sends it once. The remote resolver looks each source up in the map reusing the
existing `lookup_keys` / `binding_keys_match_lookup` cross-URL-form matching.
Unbound sources are not forwarded (least-privilege; they fetch unauthenticated
as today and surface `needs_credential` if private).

---

## Feasibility of the remote-interaction path (verified)

The FE → tunnel → remote-api header path is feasible. After the D2 revision only
**two** adjustments remain (the `localBaseUrl` exposure is no longer needed —
resolution is a Tauri command, not a local HTTP call):

1. **CORS must allow the forward header (backend, required).**
   `crates/api/src/lib.rs` `allowed_headers` is `["Authorization","Accept","Content-Type"]`.
   The Tauri webview calling `localhost:<port>` is cross-origin, so
   `X-Aghub-Git-Tokens` triggers a preflight and is blocked unless allow-listed.
   With `allow_credentials:true` + `AllOrSome::All`, rocket_cors reflects the
   origin (verified against the local rocket_cors source), so the header passes
   preflight once listed.
2. **The remote api must actually support the feature (operational, gated by D7).**
   Compatibility today is equal `major.minor` only (`crates/remote/src/ssh.rs`
   version check; `commands/remote.rs` rejects only *incompatible* remotes). An
   old same-`major.minor` remote would be accepted and **silently ignore** the
   header. Forwarding must be engaged only against a remote that advertises the
   capability — extend the existing SSH `aghub-api --version` probe and carry the
   marker through `TestResult` / `connect_remote` (see §G) — **or** the release
   must force a minor bump so the compat check excludes pre-feature remotes.

Transport itself is transparent: the tunnel is a raw TCP loopback forward, so
HTTP headers pass through unchanged.

---

## Backend changes (`aghub-api` + desktop `src-tauri`)

- **A. Resolution (no HTTP endpoint).** Add `pub fn
  resolve_git_token_for_source(source: &str) -> Option<ResolvedToken>` to
  `aghub-api` wrapping the existing resolver; `ResolvedToken` carries
  `{ token, origin: (scheme, host, port) }` (origin-aware — see §E / Codex
  2nd-pass P2), not a bare host. The existing resolver internals stay
  crate-private; only this wrapper is `pub`. Add Tauri commands
  `resolve_git_token` and `list_bound_sources` in `src-tauri`, both reading the
  Mac keyring / bindings store **in-process**. **No new HTTP route, no raw token
  over HTTP, no localBaseUrl dependency.**
- **B. Forwarding mechanism (shared, remote side):**
  - `ForwardedGitTokens` request guard — parse `X-Aghub-Git-Tokens` (base64 JSON
    map); absent → empty map; malformed → empty map + `warn!` (graceful, no 400).
  - `ForwardedTokenResolver` impl `skill_update::TokenResolver` — match `source`
    in the map via the existing `lookup_keys` / `binding_keys_match_lookup`.
  - `ChainResolver(forwarded, KeyringResolver)` — header first, keyring fallback.
- **C. Resolver routes:** swap the injected resolver from `KeyringResolver` to
  `ChainResolver` in `sources.rs::diff_source`,
  `skills_update.rs::check_updates`, and the single-source apply resolve
  (`skills_update.rs:~536`).
- **D. Git scan path** (`skills.rs::git_scan_skills`): if the forwarded map has a
  token for `req.url`'s source, use it as the clone credential (alongside / in
  place of the `credential_id` keyring lookup). The token is cached in the scan
  session as today, so **branch rescans / branch listing** that reuse the
  session token inherit it. NB (Codex P3): `install`/`sync` mostly reuse the
  already-cloned temp dir, so they do not independently re-resolve — the gain is
  on scan + branch operations, not a blanket "install/sync auto-inherit".
- **E. Host pinning (D8):** generalize `require_github_credential_url`
  (github.com-only) and the port-agnostic `same_host` to pin the forwarded token
  to the source's resolved clone-URL **origin** `(scheme, host, port)`. github.com
  behavior unchanged; self-hosted GitLab on a custom port handled correctly.
- **F. CORS + logging:** add `X-Aghub-Git-Tokens` to `lib.rs` `allowed_headers`;
  ensure the header is in log redaction. No widening of origins/methods.
- **G. Capability advertisement (D7) — concrete location (Codex 2nd-pass).**
  There is **no** existing HTTP health/version route to hang a field on. The
  right seam is the **existing SSH `aghub-api --version` probe** in bring-up
  (`crates/remote/src/ssh.rs` version parse; `commands/remote.rs`): extend it to
  also read a protocol/capability marker (a bumped `--version` semantic, or a new
  `aghub-api --capabilities` line), and **carry the result through `TestResult` /
  `RemoteHandle` and the `connect_remote` return** (which today returns only the
  tunnel port). The desktop stores "this remote supports forwarding" and only
  attaches the header when it does. Surfacing it to TS means adding a field to the
  generated `TestResult` DTO.

---

## Frontend changes (`crates/desktop/src`)

- **No `localBaseUrl` needed** — token resolution is the `resolve_git_token`
  Tauri command, not a local HTTP call. (Removes the original prerequisite 2 and
  the React Query `gcTime` concern.)
- **Forwarding helpers** (`requests/` or a small `lib/` module):
  `resolveForwardedTokens(sources[]) → Record<source, token>` (invokes the Tauri
  command per source, skips nulls) and `encodeGitTokensHeader(map) → base64(JSON)`.
- **Centralized injection (Codex P2):** the typed `createApi` methods do not
  currently accept per-request headers, and some components call
  `api.skills.gitScan` directly. Either extend the relevant API methods with an
  optional header argument, or route git-auth calls through one
  forwarding-aware request module and migrate the direct callers. Inject only
  where the source is known (single-source) or enumerated (check-updates).
- **Gating (D7 + remote-only):** attach the header only when (a) the active
  connection is remote, (b) the remote advertises the capability, and (c) only on
  the remote client — never the local one. Local mode is unchanged.

---

## Security, error handling, edge cases

**Security**

- **No raw token over HTTP** — resolution is an in-process Tauri command, not a
  web-reachable endpoint (closes Codex P1-a / the localhost-web-exfil class).
- Residual: the token transits the renderer briefly (set as a header by `ky`).
  Acceptable under Tauri's own-renderer trust model; only renderer XSS could
  reach it. B2 (token never in renderer) was considered and rejected (D2).
  **Renderer hygiene (Codex 2nd-pass):** the forwarded token must stay out of
  React Query state, persisted stores, logs, devtools-visible state, and error
  objects — resolve it transiently right before the request and never cache it.
- Token travels to the VM only over the SSH-encrypted loopback tunnel; the VM
  uses it in-memory and discards it — never written to disk or log (gix strips
  URL userinfo; the header is added to log redaction).
- Host pinning is origin-level `(scheme, host, port)` (D8), so a token is never
  attached to a different origin.
- CORS adds only the one header.

**Error handling (graceful degradation throughout)**

- Resolution returns nothing (no binding) → map omits that source → remote falls
  back to keyring → still none ⇒ `needs_credential` (as today). No hard error.
- Remote lacks the capability (D7) → forwarding not engaged; behaves as today
  with a clear message rather than a silent failure.
- `check-updates` binding enumeration failure → degrade to no forwarding.

**Edge cases**

- Source bound on both Mac and VM → header (Mac) wins via `ChainResolver` order
  (controller is the truth source).
- Multiple remotes → each gets only the tokens for the sources it is asked about.
- Token rotation → resolved fresh per request from the Mac keyring; no stale VM
  copy.
- **Header size (Codex P2):** even bound-only, a scope with many bound sources
  repeats a token per source and could approach 8–16KB header limits. If hit,
  switch `check-updates` to POST + body (or a token-table + source-reference
  encoding). YAGNI until measured.

---

## Testing strategy

**Backend (Rust unit + Rocket local-client)**

- `resolve_git_token_for_source` (pure wrapper) + `ForwardedTokenResolver`
  matching (reuse `lookup_keys`); `ChainResolver` precedence (header beats
  keyring; no header → keyring fallback).
- `ForwardedGitTokens` guard parsing: valid base64 JSON → map; absent → empty;
  malformed → empty + warn.
- skill-update orchestrator/`diff_source` tests: a recording fetcher asserts the
  resolver's token reaches the fetch.
- `sources/diff` route: with header → uses token; without → keyring (reuse the
  `AGHUB_TEST_SOURCE_FETCH_ROOT` seed).
- Host pinning: source-origin-A token not attached to an origin-B request
  (including a same-host-different-port case).
- `git scan`: forwarded token used for the clone, host-origin guard holds.

**Frontend (`node --test`, pure functions, like `connection-logic.ts`)**

- `resolveForwardedTokens` (mock the Tauri `invoke`): skips nulls, includes
  resolved.
- `encodeGitTokensHeader` round-trip.
- Gating: header built only when active ≠ local **and** capability advertised.
- `check-updates` bound-source enumeration → map assembly.

**Integration / manual**

- The real Mac→tunnel→VM round-trip is **not** in CI (no real VM). Manual
  checklist: private-source diff → install → check-updates all green on a remote;
  **plus an old-remote test** that confirms forwarding is not engaged and the
  user sees a clear "upgrade remote" message (Codex P1-b).

---

## Open items deferred

- `check-updates` POST+body fallback if the token map exceeds header limits.
- Real private/credential classification on `GET /skills/sources` (would let
  forwarding cover host-fallback-credential sources too, beyond explicit
  bindings) — out of scope here; tracked separately.
- Tightening the embedded API's overall no-auth + all-origin posture (the
  fork-wide `ApiAuth` question) — independent of this feature now that no secret
  is exposed over HTTP.

---

## Codex review (2026-06-22) — incorporated

- **P1 raw-token endpoint** → D2 revised to a Tauri command (no HTTP endpoint).
- **P1 remote upgrade overstated** → D7 capability/protocol gate + old-remote test.
- **P1 `check-updates` can't identify private sources** → D4 refined to
  bound-sources-only forwarding.
- **P2 header size** → documented bound + POST fallback.
- **P2 `localBaseUrl` via query cache (gcTime)** → moot; resolution is a Tauri
  command, `localBaseUrl` no longer needed.
- **P2 centralized FE injection** → extend API methods / forwarding-aware module.
- **P2 host pinning** → origin-level `(scheme, host, port)` (D8).
- **P3 install/sync reuse wording** → corrected (temp-dir reuse; gain is on scan
  + branch ops).

### Second pass (2026-06-22) — no new P1; tightened

Codex re-reviewed the revised spec: **no new P1**, three prior P1s confirmed
closed (raw-token fully; D7 and bound-only closed with implementation caveats).
Incorporated:

- **New P2 — D2/D8 origin mismatch:** the wrapper returned `{token, host}` via the
  host-only `keychain_host_for_source`, but D8 needs origin `(scheme,host,port)`.
  → §A/architecture now specify an **origin-aware** `ResolvedToken`.
- **D7 marker under-specified** → §G now names the concrete seam (extend the SSH
  `aghub-api --version` probe; carry through `TestResult` / `connect_remote`;
  no HTTP health route exists).
- **`list_bound_sources` ambiguity** ("reuse local GET" vs "no localBaseUrl") →
  resolved to an **in-process** Tauri command reading the bindings store.
- **Renderer hygiene** → added: keep the token out of React Query state, stores,
  logs, and error objects.
- **Accepted UX gap:** a private source that is unbound and relies on host-fallback
  credentials will still fail unauthenticated under bound-only forwarding (D4);
  the real fix (private classification on `GET /skills/sources`) is a deferred item.
