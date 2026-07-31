# Source bulk sync

Status: complete

## Objective

Deepen Sources resync so the Desktop HTTP adapter and CLI adapter share one
Rust implementation for updating several locked skills from one Source.

## Contract

- Resolve every requested Lock entry and its `EntryIdentity` before fetching.
- Reject predictable preflight failures before any installed skill is changed.
- Require every Lock entry to still belong to the Source represented by the
  caller; reject a stale Source view before credential resolution or fetching.
- Group requested skills by effective Source + ref and fetch each group once.
  Multiple refs from the same Source remain valid and are fetched once per ref.
- Preserve request order in the per-skill results.
- Attempt every resync after a successful preflight/fetch; one runtime failure
  does not suppress later skills.
- Reuse an already-fetched Source in CLI `source sync --update`; do not add a
  second fetch there.
- Expose a batch HTTP adapter while retaining the single-update route.
- Make Desktop `apply all` call the batch adapter once rather than looping over
  single-update requests.

## Surface responsibilities that remain separate

- Desktop: progress/toasts, query invalidation, forwarded credential header.
- CLI: dry-run/confirmation, text/JSON formatting, exit status, env credentials.
- HTTP: request validation, error projection, blocking-pool scheduling.
- Shared module: Lock observation, fetch selection, compare-after-fetch,
  attempt-all resync policy, and ordered results.

## Verification

- A shared-seam test proves two skills from one Source cause exactly one fetch,
  both installed folders change, and both Lock entries receive the fetched
  commit identity.
- Failure tests prove stale Source views and missing fetched skills stop before
  any fetch/write, while a runtime stale-entry failure does not suppress later
  rows.
- Handler and adapter tests prove one HTTP batch fetch, ordered error rows, and
  Desktop one-request wiring.
- Existing single-update, CLI source sync, mutation-lock, and Source tests stay
  green.
- Workspace fmt/clippy/test/doc-test gates and the Desktop
  test/typecheck/lint/format/build gates pass.
