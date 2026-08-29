# 06 — The npx folder hash ignores symlinks and caps at MAX_FILES

**Status:** open · pre-existing · accepted risk for v2.15.0

`crates/skill/src/hash.rs` skips symlinks (:120,125) and refuses above
`MAX_FILES = 10_000` (:13). Two consequences for the content-landing proof that
now gates a destructive removal (`crates/core/src/transfer.rs`):

1. Two folders differing only in symlinked content hash EQUAL, so the proof can
   certify a copy that did not carry that difference.
2. A skill folder over the cap makes `compute_skill_folder_hash` fail, and
   `skill_folders_match` answers `false` on any error — so a legitimate move is
   refused with a message asserting a content difference that does not exist.

**Why accepted:** (1) is bounded by npx-compat — the hash MUST stay
npx-compatible (root `CONTEXT.md`), so changing what it covers is a
compatibility decision, not a bug fix. (2) fails CLOSED, which is the correct
direction for a guard on a destructive step; the cost is a confusing message.

**Fix direction for (2):** distinguish "hashes differ" from "could not hash" and
say which.

**Found by:** round-4 workflow (both LOW, confirmed).
