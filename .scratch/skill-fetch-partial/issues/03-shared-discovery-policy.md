# 03 — Extract shared discovery policy as a pure function

**What to build:** One pure discovery function over a tree-entry stream that
captures today's scan semantics **exactly**, used first by the current filesystem
walk (and later by both fetch backends). **No behavior change** — this is a
prefactor whose whole risk is a silent semantic drift, so its tests must pin the
behavior, not merely re-pass.

**Blocked by:** None — can start immediately.

**Status:** done

- [ ] A pure fn maps tree entries → discovered skill folders using the existing
      semantics: `full_depth`, `max_depth 10`, and **case-sensitive** dedup by raw
      frontmatter name (`HashSet<String>`, as in `scan.rs` today).
- [ ] **Behavior-pinning fixtures that fail on drift:** (a) two skills whose names
      differ only in case both survive (proves case-sensitive); (b) a nested/depth
      fixture at/beyond `max_depth`; (c) a duplicate-name fixture dedups to one.
- [ ] The **same fixtures run through the old scan and the new pure fn produce
      identical results** (equivalence, not just "both green").
- [ ] Existing discovery/scan tests remain green.

Spec: `docs/specs/2026-07-17-skill-fetch-partial-github-rest.md` (Discovery policy;
Decision 14). Do NOT adopt npx `PRIORITY_PREFIXES`.
