# aghub

aghub manages AI coding agent configurations — MCP servers, skills, and sub-agents — across many agents, through a CLI, an HTTP API, and a desktop app. This glossary fixes the language that recurs across those surfaces.

## Language

### Skill hashing & locks

**Source hash**:
The SHA-256 aghub computes over a skill's source folder, used to decide whether an installed skill is up to date. It is stored under different keys depending on the lock format (the global lock's `contentHash`, the project lock's `computedHash`), but it is one concept.
_Avoid_: "content hash" / "computed hash" when speaking conceptually — those are the storage keys, not the term.

**Skill folder hash**:
The upstream GitHub tree SHA for a skill folder, written by the npx `skills` toolchain. In aghub's own global lock it is deliberately kept empty; the Source hash supersedes it. Setting a Source hash and clearing the Skill folder hash always happen together — they are never both populated.
_Avoid_: conflating with Source hash.

**Lock entry**:
One skill's record in a lock file. The global lock (npx-compatible v3) and the project lock (v1, intentionally timestamp-free to avoid merge conflicts) carry different fields for the same skill.
_Avoid_: "lock row".

### Skill layout & install

**Universal install**:
A layout where a skill lives once as a shared Master and each agent's skills directory holds a symlink pointing at it. Contrast with an Isolated-copy install, where every agent gets its own independent copy and there is no Master.
_Avoid_: "symlink mode" / "linked skill" as the canonical name.

**Master**:
The single `.agents/skills/<name>` directory that a Universal install's per-agent symlinks resolve to. Renaming or removing it is the operation that must account for every Referrer.
_Avoid_: "canonical dir" (that is the storage key), "source".

**Master GC (on removal)**:
Removing a skill deletes the Master **only** when the removal targets the view that reads the Master directly **and** no other view still references it. Removing one linking agent's Referrer (the common `delete -a <agent>` case) unlinks that Referrer but **keeps** the Master, because `.agents/skills` is itself in the swept agent-dir union — every project NativeReader (Codex, OpenCode, Cursor, Cline, Warp, …) reads the Master directly, so it counts as a live reference. The Master is GC'd only once the last such reference goes (e.g. deleting via the NativeReader whose skills dir _is_ `.agents/skills`).
_Avoid_: "removing the last referrer GCs the Master" — that overstates it; a lone linker removal does not, and the `.agents/skills` reader always counts as a referrer. Two authoritative rules, both in `crates/core/src/skills/removal.rs`: `plan_symlink_removal` for a Referrer-layout entry (`canonical_path` set), and `single_agent_keep_reason` for a Master discovered as a plain dir — the latter is shared verbatim by `plan_copy_removal` and by the `ConfigManager::remove_skill` seam, so no caller of the copy rule can take a Master — the GC above happens through `remove_skill_planned` (or `--all-agents`), never through the plain `remove_skill` seam.

**Referrer**:
An agent's skills entry that is a symlink resolving to a Master. When the Master is renamed or removed, its Referrers must be re-pointed or pruned. For GC purposes a project NativeReader that reads `.agents/skills` directly (no symlink) also counts as a referrer that keeps the Master alive.
_Avoid_: "link", "alias".

**Relink**:
Re-pointing a Master's Referrers after the Master moves: unlink the old-name symlinks and recreate symlinks at the new name. A failed Relink leaves dangling Referrers and is the failure a transactional rename must roll back.

**Fetched Source**:
A commit-pinned, selectively materialized upstream tree kept alive long enough to drive an install or Resync. Its staged root and commit identity are one unit; it is mutation input, never a Master.
_Avoid_: "clone", "temp dir", or "source directory" as the canonical name.

**Resync (an installed skill)**:
Replacing an installed skill's on-disk content from a freshly-fetched source folder and updating its Source hash in the lock — **without** changing the install layout. For a Universal install the Master is swapped and the symlink Referrers are left untouched (symlink targets are skipped). The single operation behind `aghub apply-update`, `source sync --update`, and the API apply-update / git-sync routes. Distinct from an install (which creates the layout) and from Relink (which re-points Referrers after a rename). Every installed target is swapped before the lock advances: any per-target swap failure aborts and leaves the lock unchanged, because a partial swap with an advanced lock would let a later update-check read a stale target as up-to-date (differing per-agent hashes are dropped as ambiguous, so the lock is the sole baseline).
_Avoid_: "update" as the canonical name — it collides with the update-check orchestrator (`skill-update`).

**Multi-target mutation**:
One logical command applied to an ordered set of agent targets: predictable capability failures reject the whole command before writes, then runtime failures are attributed per target while every target is attempted. It is not a transaction and must not replace Resync's lock-advancement rules.
_Avoid_: "batch" as the canonical name — that describes transport shape, not the mutation policy.

## Remote / VM orchestration

**Remote connection (VM)**:
A saved SSH target the desktop can bring `aghub-api` up on and tunnel to on localhost, so every existing feature (skills, MCPs, sub-agents, …) runs against the VM through the same UI. Switching the active connection re-points the API base URL — there are no per-feature remote code paths.
_Avoid_: "remote server" for the saved connection (the server is the `aghub-api` process the bring-up starts on the VM).

**Bring-up**:
The connect sequence `probe → ensure_remote_api (install if missing) → start_remote → ssh -L tunnel → switch active`. Teardown kills the local tunnel child via a cross-platform `Child::kill()` and guarded-kills the remote `aghub-api` by pid.
_Avoid_: "deploy" for the whole flow — install/deploy is only the `ensure_remote_api` / `force_redeploy_remote_api` step.

**Desktop-only boundary (by design)**:
Remote/VM orchestration is exposed **only** through the desktop (Tauri commands + React). There is intentionally **no** CLI subcommand and **no** `/api/v1` route, and `crates/cli`/`crates/api` do not depend on `aghub-remote`. The boundary is deliberate: the HTTP API is the artifact being deployed and tunneled to, so it cannot orchestrate its own deployment. The tauri-free `aghub-remote` crate (probe/ensure/start/cleanup over an injected `CommandRunner`) is shaped so a thin `aghub-cli remote` shell _could_ drive it later if headless management is ever wanted.
_Avoid_: assuming remote management is scriptable/headless today — it is not.

**Auto-deploy vs pre-installed (shipped builds)**:
Connecting to a VM that already has a compatible `aghub-api` works in any build (`ensure_remote_api` returns early on `api_present` before reading an install source). **Auto-install on first connect** and **force-redeploy** need a resolvable install source, which a packaged desktop currently lacks (the version-locked-binary bundling prerequisite has not landed), so those two conveniences are dev-tree-only and the UI gates the redeploy button off when no source is available.
_Avoid_: "the desktop deploys aghub-api to any VM" — only a dev tree (or a pre-installed VM) does today.
