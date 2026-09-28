# inference history

Incidents behind the rules in `crates/inference`: provider metadata and keyring
storage.

## Provider delete stale key rollback

GitHub #15 P2-5 (found in Codex review). `delete_provider_cascade` read the
provider's API key once, in its own precondition check, and threaded that
value into `store.delete` as a `known_api_key` parameter for rollback. That
read ran OUTSIDE the store's lock, so a concurrent `set_api_key` that
committed a newer key between the read and `store.delete` taking its lock
could be clobbered: an SQL-delete failure rolled the keyring back to the
stale precondition value. The same race existed between two concurrent
`set_api_key` calls on one provider — both read the same previous key, and one
rollback stomped the other's committed key, leaving the DB (where only one
UPDATE wins) out of sync with the keyring.

Rules it produced:

- `credential_lock_for(app_data_dir)` serializes every keyring+SQL sequence
  (`create`/`update`/`delete`/`set_api_key`/`delete_api_key`). It is keyed by
  app data dir rather than held on the store, because the API constructs a
  fresh `InferenceProviderStore` per request.
- `store.delete` reads the current key itself, under that lock; it is the only
  read rollback may use. The cascade's precondition read is discarded.
- The in-lock read is fail-hard (GitHub #15 P1c). A best-effort read that
  degrades to `None` was considered and rejected: it would silently disable
  rollback on a transient read failure, worse than failing for a
  security-sensitive value.

Pinned by `cascade::tests::cascade_delete_rollback_never_restores_stale_precondition_value`
and the `store.rs` regression tests for P1c / P2-5. Commit `58b06365`.

## Provider delete keyring probe

GitHub #15 P1a (found in Codex review). `delete_provider_cascade` read the API
key (needed for OpenCode's `(base_url, api_key)` match) only AFTER the Claude
and Codex bindings had been torn down, so an unreachable credential backend
(Linux secret-service with no D-Bus session, a locked keychain) produced a
half-finished delete. The key read now runs first, before any adapter is built
or any row touched, so an unreachable backend returns `KeyringUnavailable` with
the whole delete a no-op. Pinned by
`cascade::tests::cascade_fails_closed_before_any_mutation_when_backend_unreachable`.
Commit `58b06365`.
