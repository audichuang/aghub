# 09 — Integration & round-trip verification (the whole is correct)

**What to build:** Prove the assembled feature end-to-end. The per-ticket tests
verify units; this ticket promises that the **combination** delivers the
Problem-Statement guarantee and never breaks the npx round-trip. No new product
behavior — verification only.

**Blocked by:** 01, 02, 03, 04, 05, 06, 07, 08 (all).

**Status:** done

- [ ] **End-to-end "only the skill"**: installing a small skill from a large,
      multi-skill repo via **each surface** (CLI `source`, `POST /skills/install`,
      desktop) fetches **only that skill's blobs and zero history** (recording seam,
      or a `#[ignore = "network"]` E2E) — the core Problem-Statement promise.
- [ ] **Round-trip parity**: the Master bytes, Source hash, and lock entry produced
      by the aghub REST path are **identical** to a gix clone of the same skill; a
      lock written by npx `skills` still round-trips (hash matches, no state wiped).
- [ ] **Fallback equivalence**: a non-GitHub source and a rate-limited GitHub
      source each install a result **identical** to the REST/clone result.
- [ ] **Cross-surface consistency**: the same skill installed via CLI vs desktop
      yields the same lock entry and Master.
- [ ] `just preflight` is green (fmt + clippy + typecheck + tests + doc tests).

Spec: `docs/specs/2026-07-17-skill-fetch-partial-github-rest.md` (Testing
Decisions; Round-trip contract).
