# Host-aware Source grouping

Status: complete (unreleased)

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
- ONE reconstruction decides the clone COORDINATE of an entry that records no
  `sourceUrl` (`sources::reconstruct_source_url`), and the row's advertised
  `source_url`, `diff_source`'s fetch and the bulk apply's `SourceRef` all come
  from it. Membership agreeing is not enough: while the row reconstructed a
  GitLab URL and apply resolved the raw `group/repo` on its own (as GitHub
  shorthand), both halves passed the group check and then fetched different
  forges.
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

## Outcome

All three consequences are addressed by the ONE grouping fix, because diff,
apply, the bulk membership check and CLI `source sync` all select through
`source_matches`:

1. A row's diff and its apply now resolve to the same repository — the row's
   `sourceUrl` covers every entry in it by construction.
2. A foreign-forge entry can no longer be classified `Removed { noPath }` from
   another forge's tree, so the one-click "Clean up removed" cannot reach it.
3. CLI `source sync --update` applies its ONE fetched tree only to entries of
   that origin, so it can no longer overwrite a skill with another forge's
   content. A host-blind argument that spans forges — `source list` prints the
   lock's own host-blind `SOURCE`, so pasting that column back is the ordinary
   way to produce one — is now REFUSED before any fetch rather than resolved to
   whichever entry sorted first (`ResolvedSourceMeta::ambiguous_origins`, shared
   by CLI `source diff`/`sync` and the API `/sources/diff`, which answers
   `SOURCE_AMBIGUOUS`). `source sync` still has no Source assertion of its own —
   it no longer needs one for THIS failure, because selection is now
   origin-exact.

    The bulk apply deliberately does NOT take this guard: each row fetches its own
    entry's coordinate, so a host-blind group merely admits more rows, each still
    updated from its own forge. The guard belongs only where ONE fetched tree is
    applied to MANY entries.

4. A row is one repository, not one TREE — its entries need not share a branch.
   The row's key stays the origin (adding `ref` to it would give two rows one
   `source_url`, which the desktop keys on), but `diff_source` now splits the
   baseline into one cohort per recorded ref and fetches each, so every entry is
   judged against the tree it was installed from. Judged against the row's ref
   instead, a `v1`-pinned skill read as outdated forever — its own apply fetches
   `v1`, so the hash never converged — or as `Removed { noPath }` when its folder
   was absent there, which the desktop deletes in one click. The mutation seam
   already fetched once per `(source, ref)`; this is the same grouping on the
   read side. An explicit `?ref=` still collapses everything into one cohort:
   that IS the caller asking what a single ref would give.

    Only the PRIMARY cohort (the row's own ref) contributes not-installed offers,
    and only for paths no cohort owns — another cohort's skills appear in the
    primary tree too, and they are entries, not offers.

Unplanned benefit: credential bindings are keyed on the clone URL, and a row's
URL now covers all of its entries, so a bound token can no longer be the wrong
forge's for part of a row.

Regression caught in self-review before release: making the row identity an
origin silently broke the "open Sources view" jump from a skill's source group
(it compared an origin against a host-blind lock source — both `string`, so
neither the typechecker nor any test noticed). Fixed with a tested mapping in
`lib/source-identity.ts`; its first implementation matched a partial segment and
its own test caught that.

## The legacy shape this turns on

`sourceType: gitlab|git` with NO `sourceUrl` is live, not hypothetical. Current
aghub cannot originate it on a fresh remote install — `recordable_source_url`
(`crates/skill/src/install.rs`) records a URL for every non-github.com host —
but:

- aghub before `3c1a0601` had no `source_url` field at all while already
  classifying `gitlab.com` as `gitlab` and other hosts as `git`;
- npx `skills` before `1164afa5` did the same;
- every rewrite path preserves it — apply/update, `source sync`, check-updates
  hash healing, prune, and accept-rename rollback all re-emit the surviving
  entry unchanged, and no migration backfills the field;
- `npx skills`'s v1 reader accepts an older entry as-is and its writer re-emits
  the whole lock, so an absent field survives npx round-trips too.

So the reconstruction must be provider-aware, and every consumer of it must use
the SAME one. (`sourceUrl` is also no longer aghub-only: current npx writes it
for `git`/`gitlab` installs — it just never backfills.)

## Still open

- **An unknowable host still silently resolves to GitHub.** `sourceType: git`
  (or a custom type) with no `sourceUrl` keeps its own spelling, and the fetcher
  then reads `owner/repo` as GitHub shorthand. List and apply agree — that is
  what this version fixes — but they can agree on the wrong forge. The end state
  is fail-closed (report the row uncheckable rather than fetch a repository
  nobody installed from); that changes behavior for existing installs, so it is
  not folded into this change.
- **CLI `source diff`/`sync` refuse a multi-ref scope instead of splitting it.**
  Both fetch the repository ONCE and reuse that tree for every entry — and
  `sync --update` also installs from it — so they now bail with the list of refs
  and ask for `--ref`, rather than judging (or overwriting) a `v1` entry with
  `main`'s tree. The API `/sources/diff` does not need the guard: it owns its
  fetches and splits into one cohort per ref. Making the CLI match means holding
  one fetched tree per ref through the install/update flow; that is the same
  unmet "CLI and Desktop share ONE bulk implementation" objective below.
- **A cohort that cannot be fetched fails the whole diff.** `diff_source`
  returns `FetchFailed`/`NeedsCredential`/`UncheckableSource` for the row rather
  than dropping that cohort's entries or judging them against another ref's
  tree — the same answer the row already gave when its only fetch failed. So one
  dead ref (a deleted branch someone was pinned to) makes the whole row
  undiffable. Reporting those entries individually needs a per-skill uncheckable
  state, which `SourceSkillState` does not have.
- **accept-rename can write a non-URL as a present `sourceUrl`.** For a legacy
  GitLab entry it fetches the raw `group/repo` and `recordable_source_url`
  persists that string, since it is non-empty and carries no GitHub host. That
  defeats "a present `sourceUrl` is an authoritative clone URL" everywhere else.
  `rename.rs` lives in `core` and cannot reach `skill_update::sources`, so the
  shared reconstruction has to move down (or the value be validated) first.
- **One SSH-recorded entry can make a whole row uncheckable.** `resolve_source_meta`
  is first-wins on the fetch coordinate, matching `list_sources`. Preferring an
  HTTPS spelling was tried and removed: with a host-blind `want`, `source_matches`
  falls back to `entry_source == want`, so the preference could swap in another
  forge's URL while keeping the first forge's type and ref.
- **`MAX_BATCH_NAMES` is a hard 256 and the desktop chunks to it.** Hoisting the
  Lock read and the agent scan to one-per-batch does not bound the real cost:
  every resolvable row still runs its own transaction, which re-scans every
  agent under the mutation lock because that read has to be fresh.
- CLI `source sync --update` has no Source assertion (the bulk HTTP path does).
  Not needed for the failure above, but the CLI would still act on a stale view.
- `.scratch/source-bulk-sync/spec.md`'s original objective — CLI and Desktop
  sharing ONE bulk implementation — remains unmet.
