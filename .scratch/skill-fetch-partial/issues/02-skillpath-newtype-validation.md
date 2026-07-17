# 02 — SkillPath newtype + entry-point validation

**What to build:** Every fetch/install entry point accepts only a validated skill
path, so a malicious or malformed path cannot escape the staging/target root.
Introduce `SkillPath` (repo-relative, POSIX, no `..`, no absolute, no leading `/`,
no prefix escape; the empty path denotes the repo-root skill). Thread it into the
CLI `source` install, the API `POST /skills/install`, and the desktop
`git_install` path (which today raw-`join`s a client-supplied string).

**Blocked by:** None — can start immediately.

**Status:** ready-for-agent

- [ ] `SkillPath` construction rejects `..`, absolute paths, leading `/`, and
      prefix-escape inputs; accepts normal sub-folder paths and the empty
      (repo-root) path.
- [ ] At **each surface**, a traversal path is rejected **before any filesystem
      write** — a test asserts nothing is written outside the target and that
      **FAILS if the raw string is used** instead of `SkillPath`.
- [ ] Normal sub-folder and root installs still succeed unchanged.

Spec: `docs/specs/2026-07-17-skill-fetch-partial-github-rest.md` (FetchSelection;
Codex Critical #6).
