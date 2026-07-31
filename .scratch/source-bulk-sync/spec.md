# Source bulk sync

Status: complete

## Objective

Let the Desktop update every outdated skill of one Source in a single request,
over one shared Rust seam, without giving up per-skill attribution or the
per-skill install/lock transaction.

## Contract

- Resolve every requested Lock entry and its `EntryIdentity` from ONE entry
  read, before that entry's Source is fetched.
- Assert every entry still belongs to the Source the caller named, before
  credential resolution or fetching. `source: None` skips the assertion for a
  caller that has no independent Source view — asserting one would mean reading
  the entry twice and failing on a repoint that landed between the two reads.
- Group resolvable skills by effective Source + ref and fetch each group once.
  Multiple refs from the same Source remain valid, fetched once per ref.
- Finish every group's fetch BEFORE the first write, so a fetch failure cannot
  leave a half-updated batch.
- Return one ordered row per unique requested name. A repeated name is attempted
  once (twice against one captured identity would manufacture a phantom
  concurrent-change on the second attempt).
- Every per-skill failure is a row: an unresolvable entry, a repointed Source, a
  skill missing upstream, its group's fetch, or its own transaction. No single
  skill's failure suppresses another's update. This deliberately does NOT use
  `aghub_core::batch`'s all-or-nothing preflight: that policy exists for ONE
  resource fanned out to many agents, where a partial batch leaves the agents
  inconsistent. The named skills here are independent, and each row's own
  install+lock swap is already transactional under the mutation lock.
- Only a request that cannot produce rows at all (unsupported scope, no names)
  fails as a whole.
- Expose a batch HTTP adapter while retaining the single-update route; the
  single route keeps escalating a credential-backend failure to 503, while a
  batch row never escalates (it would erase every other row's attribution).
- Make Desktop `apply all` call the batch adapter once rather than looping over
  single-update requests.

## Surface responsibilities that remain separate

- Desktop: toasts, query invalidation, forwarded credential header.
- CLI: dry-run/confirmation, text/JSON formatting, exit status, env credentials.
- HTTP: request validation, error projection, blocking-pool scheduling.
- Shared module: Lock observation, Source assertion, fetch grouping,
  compare-after-fetch, attempt-all row policy, ordered results.

## Original objective NOT met

This spec's stated objective was ONE Rust bulk implementation shared by the
Desktop HTTP adapter and the CLI adapter. That was not achieved: CLI
`source sync --update` still runs its OWN bulk loop
(`capture_scope_identities` + `apply_update_row`), and the two share only
`resync_fetched_source` — which they already shared BEFORE this work. Their row
policies differ (the CLI treats a missing pre-fetch identity as one failed row
and continues; this seam rejects that row for a different reason).

Recorded as unmet rather than reworded into a "known gap": an earlier revision
of this file downgraded the objective to match the implementation, which is
documentation bending to code. Shipped in v2.9.1 with the objective open.

Mitigating context, not a justification: the CLI has ONE source and ONE fetched
tree, so it needs no grouping, and the transactional core is already shared. The
un-shared part is the thin attempt-all ordering layer. Folding the CLI on needs
this seam to accept an already-fetched Source so the CLI does not fetch twice.

## Batch failure policy — approved decision

Per-row attribution (a predictable failure fails only its own row) is the
APPROVED behavior, decided by the maintainer after v2.9.1 shipped it. The
alternative considered and rejected was the original fail-closed contract
(any predictable failure ⇒ nothing written for the whole batch): the named
skills are independent and each row's own install+lock swap is already
transactional, so aborting buys no atomicity while letting one unresolvable
entry cost every other skill its update.

## Verification

- A shared-seam test proves two skills from one Source cause exactly one fetch,
  both installed folders change, and both Lock entries receive the fetched
  commit identity; a second proves one fetch per Source+ref group.
- Failure tests prove: a stale Source view fails its rows without any fetch
  (`PanicFetcher`); a skill missing upstream, an unlocked name, and one group's
  fetch failure each fail only their own row while the others still update; a
  runtime stale entry does not suppress later rows; a repeated name yields one
  row. The fetch-failure test's failing group is the SECOND one fetched, and its
  fetcher asserts no installed copy has been swapped yet — so a regression to a
  lazy per-row fetch is caught.
- Each new guard was proven to fail on its own regression: dedupe removed,
  one group's fetch failure tainting every row, the Source assertion disabled,
  and a write moved ahead of the remaining fetch.
- Handler and adapter tests prove one HTTP batch fetch, ordered rows, per-row
  `KEYCHAIN_UNAVAILABLE` attribution, and Desktop one-request wiring.
- Workspace fmt/clippy/test/doc-test gates and the Desktop
  test/typecheck/lint/format/build gates pass.
