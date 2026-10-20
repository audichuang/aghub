# Lock entry readers

A flow that starts WRITING a lock entry it did not write before (or writes a
new shape of one) hands that entry to every reader below. The baseline is the
entry a fetched install (`skill-update` `fetched_install` → core
`install_fetched_skill_and_lock`) writes for the same repo, ref and skill.
Fill the table: for each row, does the new entry make the reader behave
differently from the baseline? Every "differs" row is a finding to fix or to
justify before review.

| Reader                                              | Where                                                                             | What it reads off the entry                                    |
| --------------------------------------------------- | --------------------------------------------------------------------------------- | -------------------------------------------------------------- |
| `check` online / offline, tip preflight skip        | `skill-update` `projection.rs`, `lib.rs` (`preflight_can_skip`, `locally_intact`) | Source hash vs local copy; `refCommit`; clone coordinate       |
| `apply-update` / resync                             | `skill-update` `mutation.rs`, core `skills::resync`                               | clone coordinate, `ref`, `skillPath`; `EntryIdentity` re-check |
| `source sync` (ref cohort, `--install-missing`)     | `skill-update` `sources.rs` (`plan_source_sync`, `recorded_refs`)                 | Source membership (`source_matches`), the set of recorded refs |
| `source diff` / `source list`                       | `skill-update` `sources.rs`                                                       | Source membership, origin grouping, clone coordinate           |
| `accept-rename`                                     | core `skills::rename` (`rename_source_from_lock`)                                 | known clone coordinate; refuses an ambiguous host              |
| API git-sync / `EntryIdentity`                      | core `skills::lock` (`of_global_entry`, `of_project_entry`, `describes`)          | source / URL / type, `skillPath`                               |
| ownership on install                                | core `skills::adoption` (`same_source_owner`, `adoption_guard`)                   | owner fields; Master hash on adoption                          |
| delete lock-prune, `prune-lock`, `repair`, `doctor` | core `skills::removal`, `prune`, `repair`, `health`                               | membership by name; Master presence and health                 |
| transfer / reconcile copy, `add --from`             | core `skills::transfer` (`owner_is_carriable`), `skills::install_local`           | owner, ref, hash carried into the other scope's lock           |
| API raw lock routes                                 | api `GET /skills/lock/global`, `/skills/lock/project`                             | every field, sources through `redact_source_credentials`       |
| desktop skill detail / check-updates                | `crates/desktop` via the api DTOs and the raw lock routes                         | the API's projection of the same entry                         |
| npx `skills`                                        | global v3 / project v1 (see npx-skills-contract)                                  | every field; unknown ones must round-trip                      |

The traps that each cost a round, so check them first:

- **Host**: a URL-less non-github entry may be self-hosted (npx 1.5.11 records
  any non-github `/-/tree/` host as `gitlab`). Write or compare a host only
  when `source_coordinate::known_clone_source` proves it; otherwise keep the
  value the entry already recorded. A blank `sourceUrl` resolves to nothing, and `describes`
  then accepts any repository.
- **Credentials**: a recorded URL may embed `user:TOKEN@`. A project lock gets
  committed; error text reaches logs and API responses. Pass sources through
  `aghub_git::redact_source_credentials` before printing them, and never
  carry a credential-bearing source into another lock.
- **Hash**: an unknown stored hash stays the placeholder
  (`skill::lock_hash_unknown`). Re-hashing a copied folder turns a local edit
  into the upstream baseline. The copier drops `__pycache__` and the raw hash
  counts it, so byte-compare a Master only against content fetched from a
  remote checkout.
- **Ref cohort**: one source on two refs in one scope makes `source sync` and
  fetched installs refuse that source; refuse the write before it lands.
- **Source spelling**: `github.com` is the only host the `owner/repo`
  shorthand rebuilds to. Record a subdomain such as `gist.github.com`.
