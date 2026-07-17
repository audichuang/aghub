# 06 — GithubRest backend (the optimization)

**What to build:** For `github.com` / `*.github.com`, fetch only the selected
skill's **latest** files via the GitHub REST API — no clone, no history, no other
files. This is the core optimization.

**Blocked by:** 01 (RepoSnapshot), 03 (discovery policy), 04 (materializer).

**Status:** done

> **Shipped divergences (code + spec win over this ticket — do NOT "re-do" it as
> written):** (1) host gate shipped as **exact `github.com` only**
> (`is_github_com_host`, `crates/git/src/github_rest.rs`) — `*.github.com`
> subdomains and GHES fall back to git, deliberately; (2) a mid-operation
> `truncated` tree ships a **clean error, NOT a gix re-route** (gix 0.84
> `with_ref_name(<OID>)` panics; a branch re-fetch would break snapshot
> pinning). See the spec's Decision Log / Known limitations.

- [ ] `resolve()` returns `commit_oid` + `tree_oid` via REST; `read_tree` lists via
      the recursive git trees API and feeds the ticket-03 discovery policy;
      `read_blobs` downloads via the git blobs API using the **raw media type**.
- [ ] **Token-first auth**: a resolved token is sent up front; anonymous only when
      no token; the existing unauthenticated-first `fetch_source_with_resolver` is
      updated to match.
- [ ] **No over-fetch / no history (recording HTTP seam + canned fixtures):** for a
      repo with unrelated large blobs, the recorded request set contains **only**
      the selected skill's blob OIDs (and, for `list`, only the discovered
      `SKILL.md` blobs) — asserts unrelated blobs and any commits/history endpoint
      were **never requested**.
- [ ] Each **fallback trigger** (tree `truncated`, 403 rate-limit, 401,
      network/unexpected-shape) routes to gix; a **security-validation failure does
      NOT** silently fall back.
- [ ] **Hash-parity**: a REST-materialized skill equals the gix-materialized skill
      equals a clone (identical `compute_skill_folder_hash`).
- [ ] Concurrency default 6; per-run request/byte budget computed from tree
      metadata before download; an absolute deadline is honored (not reliant on the
      caller's outer `spawn_blocking` timeout).
- [ ] Host gate: exact `github.com`/`*.github.com` → `api.github.com` via an
      explicit trusted mapping; GHES (custom domain) is not treated as GitHub.

Spec: `docs/specs/2026-07-17-skill-fetch-partial-github-rest.md` (GitHub REST
fast-path; Decisions 1, 4, 8). Reference mechanism: `vercel_npx_skill/src/blob.ts`.
