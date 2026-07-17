# 07 — SkillRepository + rewire CLI/update surfaces + root-level preflight

**What to build:** The skill-aware composite (in `skill-update`) that owns snapshot
pinning and the **single** REST→gix fallback route, wired into the CLI/update
surfaces so each fetches only the affected skills; plus the root-level
whole-folder + size-refusal policy.

**Blocked by:** 02 (SkillPath), 05 (GixShallow), 06 (GithubRest).

**Status:** done

- [ ] `SkillRepository` with `resolve` / `list` / `fetch`; `list` and `fetch`
      **share one immutable `RepoSnapshot`**. A caller that knows its skill paths
      fetches directly, without a full `list`.
- [ ] `check_updates`, CLI `source` (install/sync/diff), `apply-update`, and
      `accept-rename` fetch **only the selected/affected skills** — a recording-seam
      test asserts non-selected skills' blobs are not fetched. `source sync/diff`
      may `list` the whole catalog to classify, but content-fetches only affected.
- [ ] **Snapshot isolation**: with the branch advanced between `resolve` and
      `fetch`, the **pinned commit** is used and recorded (test).
- [ ] Fallback routing (REST → gix) is decided in **one place** (SkillRepository /
      backend composite), not re-decided per surface. The existing
      `skill-update::Fetcher` gains a selection-carrying signature (or is replaced);
      it is NOT an unchanged thin adapter.
- [ ] **Root-level skill**: fetches the whole root folder within the reused Source
      bounds (`MAX_SKILL_FILES` / `MAX_SKILL_BYTES`); an over-bound tree is
      **refused** with `ROOT_SKILL_TOO_LARGE` and **no blobs are downloaded** (not a
      silent shallow fallback).

Spec: `docs/specs/2026-07-17-skill-fetch-partial-github-rest.md` (Architecture;
Root-level; Decisions 3, 11).
