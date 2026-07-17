# 05 — RepoFetchBackend trait + GixShallow backend (fetch goes shallow)

**What to build:** The backend seam in `aghub-git`, plus a shallow gix backend.
The existing fetch now runs through the trait and is **shallow (depth-1)**,
dropping history everywhere, while preserving the existing gix→system-git +
OS-credential-helper fallback so non-GitHub private hosts (TFS / Azure DevOps /
self-hosted GitLab, authenticated via GCM / Windows Credential Manager) keep
working.

**Blocked by:** 01 (RepoSnapshot), 04 (materializer).

**Status:** ready-for-agent

- [ ] `RepoFetchBackend` trait (`resolve` → `RepoSnapshot`, `read_tree`,
      `read_blobs`, `materialize`) with a `GixShallow` implementation.
- [ ] The existing gix fetch routes through `GixShallow` and is **shallow
      depth-1**; a **local-remote fixture proves a parent commit / its objects are
      UNREACHABLE** after fetch (not merely "install succeeded").
- [ ] `GixShallow` retains the gix→system-git + OS-credential-helper fallback — an
      existing TFS/non-github-style auth test still passes.
- [ ] Materialization goes through the ticket-04 materializer; all existing
      install / check paths remain green.

Spec: `docs/specs/2026-07-17-skill-fetch-partial-github-rest.md` (Fallback;
Decisions 5, 13).
