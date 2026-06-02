# Sources management + universal (`.agents`) skill-install mode

**Status:** in progress (backend complete & verified; desktop UI in progress)
**Date:** 2026-06-02

## Motivation

Users who install skills from git repos (à la `npx skills`) want to:

1. **See the sources (repos) they've already installed from** without re-pasting
   the URL, and
2. for a given source, **see which of its skills they have NOT installed yet**
   (e.g. "the repo offered 5, I installed 2, now I want one more") and install the
   new ones — instead of running CLI commands and guessing.

Separately, users coming from `npx skills` expect the **`.agents` master + per-agent
symlink** on-disk layout, but want control over *which* agents can see a skill.

This work delivers both, as two linked features.

---

## Part A — Install layout: isolation (default) vs universal (opt-in)

### The problem with a blanket `.agents` store

`~/.agents/skills` (global) and `<project>/.agents/skills` (project) are **read
paths for universal-capable agents** (codex, opencode, and many at project scope —
see each `crates/agents/src/agents/*.rs`). So writing a skill into `.agents/skills`
makes it visible to **all** those agents, regardless of which agent the user picked.
"Install only for Claude" must therefore **not** touch `.agents`.

Conversely, the `npx skills` single-source-of-truth model (real file once in
`.agents`, symlinks elsewhere) *requires* `.agents`. These two goals are
fundamentally in tension for a *selective* install, so we support **two modes**:

| Mode | Behaviour | Default? |
| --- | --- | --- |
| **Isolation** | Copy the skill into each selected agent's own skills dir; **never** touch `.agents`. | ✅ default |
| **Universal** | Write the real file once into `.agents/skills/<name>`; symlink each selected agent whose dir doesn't already read `.agents` (e.g. Claude). Mirrors `npx skills`. | opt-in |

Notes:
- Default is **copy** (isolation). Universal is explicitly opt-in (CLI `--universal`,
  API `universal: true`, future desktop toggle). (This is the *opposite* of an early
  assumption that symlink should be the default — the `.agents` leak makes copy the
  safe default.)
- Universal symlinks: **relative** for project scope (portable across machines /
  git), **absolute** for global scope.
- If a platform can't create a symlink (e.g. Windows without privilege) the install
  **falls back to a real copy** and records it — it never fails the install.
- Existing correct symlinks are left as-is (idempotent); a conflicting real
  file/dir or foreign symlink is **never clobbered** — it's reported.
- The removal side was already symlink-aware
  (`crates/core/src/skills/removal.rs`, `Layout::Symlink`); this work adds the
  missing "create the symlink" half and sets `canonical_path` so removal recognises
  the layout.

### npx-lock compatibility (must not break)

The lock files round-trip with `npx skills`. We only ever touch the additive
`contentHash` (global) / `computedHash` (project), keep `skillFolderHash` empty,
**never bump the lock versions** (global v3 / project v1), and use `skillPath`
(repo-relative `<dir>/SKILL.md`) as the identity key.

---

## Part B — Sources feature

A new top-level desktop page **"Sources"** (`🌐`, `/sources`).

- **Overview** — a single list aggregating sources from the **global** lock **and
  each project's** lock (projects come from the desktop store via `useProjects`),
  each row tagged with its scope (global / project:name), a skill count, and a
  credential-availability chip.
- **Per-source diff** — selecting a source fetches the repo once and reports each
  of its skills in one of: `notInstalled` / `installedCurrent` / `installedOutdated`
  / `uncheckable`. This **absorbs** the old "check-updates" (new skills + updates in
  one place). Private sources with no usable credential return `needsCredential` so
  the UI can offer to bind one.
- **Install** reuses the proven git scan → install flow; the user is prompted to
  choose target agents on each install (and may pick isolation or universal).

### New HTTP API

| Method + path | Purpose |
| --- | --- |
| `GET /skills/sources?scope=&project_root=` | Offline, lock-only. Group installed skills by source per scope → `SourcesListResponse`. |
| `GET /skills/sources/diff?source=&scope=&project_root=&git_ref=` | Fetch the source once, list all its skills, diff vs lock → `SourceDiffResponse` (3-state + `needsCredential`). |

DTOs: `SourceSummaryResponse`, `SourcesListResponse`, `SourceSkillDiff`,
`SourceDiffResponse`, `CredentialStatus` (`crates/api/src/dto/sources.rs`).

The session-based install endpoint `POST /skills/git/install` gained a
`universal: bool` field (default false). The CLI `add` gained `--universal`.

Cross-project aggregation is driven by the **frontend** (the project list only
lives in the desktop store); the backend exposes single-scope endpoints and the UI
loops projects with `useQueries`. The `/skills/sources/diff` is read-only (no
auto-heal of the lock during a background scan).

---

## File map (what changed)

**Core (`crates/core`)**
- `src/skills/install_layout.rs` *(new)* — universal-mode primitive:
  `universal_canonical_dir`, `install_universal`, `link_agents_to_canonical`
  (relative/absolute links, Windows copy-fallback, conflict-safe, idempotent).
- `src/manager/skill.rs` — `add_skill_universal` / `add_skill_from_path_universal`
  (default `add_skill` copy behaviour unchanged).

**API (`crates/api`)**
- `src/routes/sources.rs` *(new)* — `list_sources`, `diff_source`.
- `src/dto/sources.rs` *(new)* — the DTOs above.
- `src/routes/skills.rs` — `install_git_skill_universal` + `universal` branch in
  `git_install_skills`; `src/dto/skill.rs` — `GitInstallRequest.universal`.
- `src/routes/skills_update.rs` — `GitFetcher` made `pub(crate)` for reuse.
- `src/lib.rs`, `src/bin/export-dto.rs` — route mount + DTO export registration.

**CLI (`crates/cli`)** — `add --universal` flag (`main.rs`, `commands/add.rs`).

**Desktop (`crates/desktop`)** — `lib/api.ts` (`getSources`/`diffSource`),
`requests/sources.ts`, `requests/keys.ts`, `lib/sidebar-navigation.ts` +
`lib/store/types.ts` (nav item), `pages/sources/*`, `App.tsx` route, and i18n keys
in `lib/locales/{en,zh-Hant,zh-Hans}.ts`.

---

## Implementation status

- [x] Core symlink primitive + unit tests
- [x] Wire universal into `add_skill` / `git_install` / CLI `--universal`
- [x] Sources DTOs + ts-rs export
- [x] `GET /skills/sources` (offline aggregation)
- [x] `GET /skills/sources/diff` (3-state)
- [ ] Desktop page (overview + 3-state detail + install entry + i18n) — in progress

Backend verified: `cargo build`/`cargo check`, `cargo clippy -D warnings`,
`cargo fmt`, and unit tests all green.

## Known follow-ups / not yet done

- Cross-request fetch cache for "auto-check all on open" (currently relies on
  React Query `staleTime`; a process-level `(source, ref)` cache in `AppState` is a
  planned optimisation).
- `/skills/sources/diff` returns `sessionId: null`; the UI re-scans via the existing
  git flow to install. Returning a reusable scan session is a future optimisation.
- `credentialStatus`/`isPrivate` in the offline list are best-effort (private is
  only authoritatively known at fetch time).
- Legacy lock entries with an empty baseline hash are reported `installedCurrent`
  (we don't have a hash to compare) rather than false "update available".
