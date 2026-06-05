# Push a local skill to a remote host (Feature #2)

- **Date**: 2026-06-05
- **Status**: **Draft — design NOT finalized.** Captures requirements + a recommended
  approach + open questions, ready for a brainstorming pass before TDD. Do not implement
  straight from this doc; resolve the Open Questions first.
- **Author**: audichuang (with Claude)
- **Related**: `crates/skill` (pack/unpack), `crates/api/src/routes/skills.rs`,
  `crates/desktop/src/providers/connection.tsx` (remote tunnel `baseUrl`),
  [`2026-06-05-remote-force-redeploy-design.md`](./2026-06-05-remote-force-redeploy-design.md)
  (the remote `aghub-api` bring-up this builds on).

## Context & Problem

The desktop can connect to a remote VM, bring up `aghub-api` there, and tunnel to it
(`ConnectionProvider` swaps the API `baseUrl` to the tunnel port). All skill operations then
target the **remote** host. But there is no way to take a skill that exists **only on the local
machine** and push it to the connected remote.

- The existing `POST /skills/transfer` (`transfer_skill_route`, `skills.rs:136`) transfers a skill
  **between agents on the same host** (`source` agent → `destinations` agents). It is **not**
  host-to-host: it never moves bytes across machines.
- The remote install paths that *do* cross machines are **git-sourced**
  (`POST /skills/git/install`, `/skills/install`): they fetch from a repo the remote can reach.
  A skill that lives only in the user's local `~/.claude/skills/foo/` (never pushed to git) cannot
  be installed on the remote at all.

So a user iterating on a local-only skill has no in-app way to get it onto the VM they are
connected to.

## Goal

Add a **"傳到遠端主機 / Send to remote host"** action next to the existing **"新增至代理 / Add to
agent"** affordance. It takes an installed **local** skill, ships its bytes to the **connected
remote** `aghub-api`, and installs it there (for chosen agents/scope) — without requiring the
skill to be in a git repo.

## Non-goals

- **Pulling** a remote skill down to local (reverse direction) — separate follow-up.
- **Syncing/diffing** — this is a one-shot push (overwrite-or-create), not a continuous sync.
- **Transferring across two remotes** — only local → the single currently-connected remote.
- **Re-using the git path** — git-sourced skills already have `/skills/git/install`; this feature
  is specifically for **local-only** (non-git) skills. (A git-backed skill *may* still use this
  path; it just isn't the motivating case.)

## Recommended approach (to validate in brainstorming)

Reuse the existing `.skill` zip format as the transport unit:

1. **Pack** the local skill folder into a `.skill` zip (`skill::pack`, `package.rs:87` — already
   excludes `__pycache__`/`node_modules`/`.git`/root `tests/`).
2. **Upload** the bytes to a **new endpoint on the remote `aghub-api`** over the existing tunnel
   `baseUrl` (so it inherits the SSH tunnel's transport; no new ports/auth).
3. **Remote unpack + install**: the remote `aghub-api` writes the bytes to a temp file,
   `skill::unpack` (`package.rs:180`) into a temp dir, then installs via the same path the local
   "add from path" uses (`ConfigManager::add_skill_from_path`, `core/manager/skill.rs:383`) for the
   requested agents/scope, and records a lock entry (`source_type: "local"` → no `refCommit`).

This keeps the wire format consistent with `.skill` packaging the codebase already trusts, and the
remote install reuses audited code rather than a bespoke writer.

## Architecture (sketch)

```
crates/skill            pack/unpack already exist — reused as-is.
crates/api              NEW endpoint to RECEIVE an uploaded .skill and install it
                        (runs on the REMOTE aghub-api; the desktop talks to it via the tunnel).
crates/core             add_skill_from_path already exists — reused for the remote install.
crates/desktop (FE)     "Send to remote host" action: pack request → POST to remote baseUrl →
                        report per-agent results. Disabled/hidden when on the Local connection.
crates/desktop src-tauri Likely a Tauri command to pack the local skill folder into bytes
                        (the FE cannot read arbitrary local files / run zip itself).
```

### Components

1. **Remote receive+install endpoint (`crates/api`).** A new
   `POST /skills/upload` (name TBD) accepting the `.skill` bytes plus `{ agents, scope,
   project_root? , name? }`. Decode → temp file → `skill::unpack` → validate
   (`skill::validate`, path-traversal guard) → `add_skill_from_path` per agent → write a
   `source_type: "local"` lock entry. Returns an `OperationBatchResponse`-shaped per-agent result
   (mirror `transfer_skill_route`).
   - **Wire encoding**: multipart vs base64-in-JSON — see Open Questions. Rocket supports both; the
     rest of the API is JSON, so base64-in-JSON is the lower-friction default for small skills.
   - **Size guard**: cap the accepted body (skills are small; reject multi-MB uploads).
2. **Local pack (`crates/desktop` src-tauri).** A Tauri command `pack_skill(source_path) ->
   bytes/base64` that resolves the local skill folder (tilde-expand, the `skill_root` logic already
   in `connection`/`skills_update`) and calls `skill::pack` into a temp file, returning the bytes.
   (The webview can't zip a local directory itself.)
3. **FE action (`crates/desktop` src).** A "Send to remote host" entry next to "Add to agent" in
   the skill detail / list. Flow: pick target agents+scope (reuse the existing agent picker) →
   `invoke("pack_skill", …)` → `POST {remoteBaseUrl}/skills/upload` (the active connection's
   `baseUrl`, NOT the local one) → toast per-agent results. **Only enabled when the active
   connection is remote** (`activeConnection.id !== LOCAL_CONNECTION.id`).

## Data flow

1. User on a **remote** connection opens a **local** skill, clicks "Send to remote host".
2. Picks agents + scope (global/project; project needs a remote `project_root`).
3. `pack_skill` zips the local folder → bytes.
4. FE `POST`s bytes + targets to the **remote** `aghub-api` via the tunnel `baseUrl`.
5. Remote unpacks, installs per agent, writes lock entries, returns per-agent success/failure.
6. FE invalidates the remote skill queries and toasts the outcome.

## Open questions (resolve in brainstorming BEFORE TDD)

1. **Where does the local skill come from?** The "active connection" is the *remote* while
   connected, so the local skill list isn't what the remote API serves. Do we (a) require the user
   to be on Local to pick the skill then choose a remote target, or (b) keep a separate "local
   skills" source available even while connected? This shapes the whole UX and which `baseUrl`
   each request hits.
2. **Wire format**: base64-in-JSON (simple, fits existing JSON API, ~33% overhead) vs Rocket
   multipart form (streamed, larger-friendly). Lean base64 unless skills can be large.
3. **Overwrite semantics** on the remote: if the skill name already exists remotely, replace
   (stage+swap like `apply-update`) or refuse? Confirm dialog?
4. **Scope/agent targeting**: reuse the existing agent picker + scope resolver; for `project`
   scope, how is the **remote** `project_root` chosen (the remote directory picker already exists:
   `list_remote_directories`)?
5. **Lock entry shape**: `source_type: "local"` with `source` = original local path? That path is
   meaningless on the remote — store a sentinel (e.g. `transferred-from-desktop`) so `check`
   reports `Uncheckable{Local}` cleanly.
6. **Auth/size/validation**: max upload size; reject `..`/abs paths (already in `skill::validate`);
   the tunnel is the trust boundary (no extra token).
7. **Does this belong on `aghub-api` generally, or guard it** so it only accepts uploads in the
   remote/desktop context?

## Testing strategy (once design is settled)

- **`crates/skill`**: pack→unpack round-trip already covered; add a fixture asserting a packed
  local skill unpacks to a byte-identical tree.
- **`crates/api`**: the new endpoint — decode + unpack + per-agent install into a temp `TestConfig`
  (isolated `.claude`/`.opencode`), assert lock entry (`source_type: "local"`, no `refCommit`),
  reject oversized / path-traversal payloads. Pure, no network.
- **`crates/desktop` src-tauri**: `pack_skill` resolves the right folder and returns non-empty
  bytes (network-free).
- **FE**: action hidden/disabled on Local; mutation posts to the remote `baseUrl`; per-agent toast.
  (No TDD seam at the FE; verify visually.)

## Notes

- This composes with the remote bring-up/force-redeploy work: it only makes sense once a remote
  `aghub-api` is connected, and it reuses the tunnel `baseUrl` for transport.
- Prefer reusing `skill::pack`/`unpack` + `add_skill_from_path` over any bespoke copy logic, so the
  feature inherits the existing exclusion/validation/lock behavior.
