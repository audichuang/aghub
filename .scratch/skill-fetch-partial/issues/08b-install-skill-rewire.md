# 08b — POST /skills/install rewire (only-selected fetch)

**What to build:** `POST /skills/install` (`install_skill`) currently full-clones
the source repo (`clone_skill_source_to_temp`) then `discover_repo_skills` walks
the whole tree and installs the named skills. Rewire it through `SkillRepository`
so it downloads only the selected skill(s), matching User Story 2 and the spec's
per-workflow selection matrix (this surface was missed by tickets 07/08).

**Blocked by:** 07 (SkillRepository).

**Status:** done

- [ ] `install_skill` resolves one `RepoSnapshot` and fetches ONLY the requested
      skills' folders via `SkillRepository` — no whole-repo clone on the GitHub
      path. Client sends skill **names** (and/or `install_all`); map names→paths via
      the catalog (`list`), then `fetch(Skills([...]))`; `install_all` fetches every
      catalogued skill.
- [ ] A recording-transport test asserts the request set contains only the
      selected skill(s)' blobs (+ tree/commit resolve), not the whole repo — FAILS
      if it over-fetches.
- [ ] Lock records the resolved **commit** OID (not a tree OID); `SkillPath`
      validation applies; non-github / fallback still installs correctly.
- [ ] Existing `install_skill` route tests (`install_skill_returns_per_agent_rows_symlink_only`, `install_skill_relative_project_root_is_absolutized`, …) stay green.

Spec: `docs/specs/2026-07-17-skill-fetch-partial-github-rest.md` (User Story 2;
Per-workflow selection matrix).
