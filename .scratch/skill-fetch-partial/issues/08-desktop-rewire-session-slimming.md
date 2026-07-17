# 08 — Desktop scan/install rewire + session slimming

**What to build:** Desktop "Import from GitHub" browses via `list` (no whole-repo
clone) and installs via `fetch` (only the selected skill); the scan session
becomes a commit-pinned `SkillSource` handle with no whole-repo `TempDir` on the
GitHub path.

**Blocked by:** 07 (SkillRepository).

**Status:** done

- [ ] Scan lists a repo's skills via `SkillRepository::list` with **no full clone**
      on the GitHub path — the recording seam shows no repo-wide blob download
      during scan (only `SKILL.md` blobs for the catalog).
- [ ] Install fetches only the selected skill(s) via `fetch`.
- [ ] `GitCloneSession` becomes a `SkillSource` handle pinning `commit_oid`;
      `TempDir` is not leaked through the type; a whole-repo clone is held **only**
      on the gix-fallback path.
- [ ] **TOCTOU**: the branch advances between scan and install → the **scanned
      commit** is installed and recorded (test).

Spec: `docs/specs/2026-07-17-skill-fetch-partial-github-rest.md` (Desktop session
slimming; Decision 3).
