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

## Update — (1) is FIXED, (2) is fixed as a message

Codex round 5 was right that recording (1) as an accepted low risk understated
it: the hash's blind spot was being used as PERMISSION for a destructive
removal. Reproduced — a source with a symlink and a Master without one hashed
EQUAL, the reconcile reported "2 succeeded", and the symlink was gone.

The content proof now refuses rather than certifies when it cannot see the whole
tree (`has_unhashed_entries` in `crates/core/src/transfer.rs`): a symlink, a
`.git` or a `node_modules` directory on either side answers `Unprovable`, and a
removal is never authorised by a proof that could not look. The hash itself is
unchanged — it MUST stay npx-compatible — so what remains deferred is only that
such a skill cannot be reconciled with `--remove` at all until the user copies
it themselves.

(2) is also addressed: `ContentProof` now separates `Differs` from
`Unprovable`, so a hash failure no longer produces a message asserting a content
difference that does not exist.

**Found by:** round-4 workflow (both LOW, confirmed); re-severitied by Codex
round 5 (HIGH) and fixed.
