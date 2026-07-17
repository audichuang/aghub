# 01 — RepoSnapshot & commit/tree OID separation

**What to build:** A correct repository-identity model so that later GitHub-REST
support (whose trees API returns a _tree_ OID) can never poison the lock.
Introduce `RepoSnapshot { commit_oid, tree_oid, commit_time }`; every path that
records upstream identity in a lock writes the **commit** OID, never a tree OID.
The existing gix fetch populates the snapshot. **No change to what is fetched** in
this ticket — identity model only.

**Blocked by:** None — can start immediately.

**Status:** done

- [ ] `RepoSnapshot` carries distinct `commit_oid`, `tree_oid`, `commit_time`.
- [ ] Given a **crafted snapshot where `commit_oid` ≠ `tree_oid`**, the written
      lock (global `refCommit` / project) records `commit_oid` — a test that
      **FAILS if `tree_oid` is written** (not "existing tests still pass").
- [ ] `commit_time` is documented as best-effort author time (may be `None`),
      matching the existing model.
- [ ] All existing install / check / apply-update / rename tests remain green;
      fetch scope and behavior are unchanged in this ticket.

Spec: `docs/specs/2026-07-17-skill-fetch-partial-github-rest.md` (Decision 8).
