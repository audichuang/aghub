# 04 — Source-staging materializer (mode-aware + symlink containment)

**What to build:** One shared materializer that writes a set of `(path, bytes,
mode)` tree entries into a private staging dir with correct, safe semantics, so
both backends produce a skill folder that is **byte-identical to a clone**. This is
the **Source staging** materializer — distinct from, and must NOT be merged with,
the existing Master materialization (which deliberately dereferences symlinks and
applies npx excludes).

**Blocked by:** None — can start immediately.

**Status:** ready-for-agent

- [ ] Regular files written at the correct skill-root-relative path with raw
      bytes; mode `100755` sets the exec bit on Unix.
- [ ] In-root symlink (`120000`) recreated as a symlink; an **out-of-root /
      absolute / cyclic** symlink target is **REJECTED** — test asserts an error is
      returned **and nothing is written** (exercise the failure path, not only the
      happy path). Gitlink (`160000`) is never written as a file.
- [ ] **Hash-parity golden:** materialize a fixture entry-set, then compare
      **byte-for-byte AND `compute_skill_folder_hash`** against a **real gix clone**
      of the same content (ground truth — not the materializer's own output). Fails
      if any file is dropped/mangled or the hash diverges.
- [ ] An unrepresentable case-collision returns an error (no silent merge).

Spec: `docs/specs/2026-07-17-skill-fetch-partial-github-rest.md` (Two distinct
materializers; Decision 12). Prior art: `crates/skill/tests/hash_parity_golden.rs`.
