# Host-aware Source grouping

Status: in progress (v2.9.2)

## Problem

A Sources row is keyed on the lock's `source` field, which is host-blind: for
every non-`git@` URL it normalizes to `owner/repo`
(`aghub_git::ResolvedRemoteSource::lock_source`). Two hosts serving the same
`owner/repo` — a mirror, an internal forge, a fork — therefore collapse into ONE
row, whose `sourceUrl` is whichever entry the grouping map happened to insert
first.

Three consequences, all pre-existing (present in v2.9.0), all one root cause:

1. **The diff judges against a different repository than the apply installs
   from.** `/sources/diff` fetches only the row's first matching `source_url`,
   and `source_matches` pulls in the other host's entries anyway. So skill B
   (on host B) is compared against host A's tree — it reads as outdated because
   the bytes differ — and applying then fetches from B's own coordinate. The
   badge never clears, because the comparison it is based on can never be
   satisfied.
2. **It can DELETE installed skills.** A host-B entry whose `skillPath` is
   absent from host A's tree is classified `Removed { reason: "noPath" }`, and
   the Desktop's "Clean up removed" is a one-click, no-confirmation delete.
3. **CLI `source sync --update` overwrites with the wrong content.** It has no
   Source assertion at all and applies ONE fetched tree to every matched row.

The bulk-update Source assertion added in v2.9.1 is deliberately aligned to this
same grouping predicate, so it inherits the resolution limit rather than fixing
it (a stricter assertion there rejected rows the UI correctly showed, with an
error no refresh could clear — see `.scratch/source-bulk-sync/spec.md`).

## Contract

- Group Sources by normalized clone ORIGIN (scheme-insensitive host + port +
  repository path), not by the host-blind `owner/repo`. Two hosts serving the
  same path are two rows.
- ONE predicate decides membership, and diff, apply and the bulk assertion all
  use it — a row's diff and its apply must resolve to the same repository.
- An npx-written entry (no `sourceUrl`, `source` = `owner/repo` shorthand) keeps
  matching its reconstructed GitHub origin: the shorthand IS a GitHub coordinate.
- The lock file's own fields do not change. Grouping is a view concern; the
  `source` / `sourceUrl` shape stays npx-compatible (`npx-skills-contract`).

## Also in this version

- Hoist the per-name lock read out of the bulk resync loop (one read, then map
  lookups) and make dedupe linear, so `MAX_BATCH_NAMES` can rise or go. Today a
  Source with >500 outdated skills cannot use "Update all" at all — a limit
  v2.9.1 introduced, since the old per-skill loop had none.
- Tighten the bulk request's Source vocabulary: `LockedSkillsResyncRequest.source`
  currently carries a GROUPING key while its name and `SourceChanged` suggest a
  repository identity, which promises more than the check delivers.

## Expected fallout

- Sources UI shows two rows where it showed one (correct, but user-visible).
- The two mixed-host tests added in v2.9.1
  (`one_source_row_spanning_two_hosts_updates_each_from_its_own_host` and its
  global twin) assert the PRE-FIX arrangement — one row spanning two hosts. They
  must be rewritten as two rows, and their docstrings currently do not say they
  describe a transitional state.
- CLI `source list` / `diff` / `sync` row sets change for mixed-host locks.
