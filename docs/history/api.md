# api history

Incidents behind the rules in `crates/api`: the skill routes and the
forwarded-credential guards.

## delete-by-path absent body

`DELETE /skills/by-path` used to answer an already-absent skill with a
hand-built body: `deleted_path: Some(skill_dir)` alongside `executed: false`
(contradicting that field's contract, "only when `executed`; null otherwise",
and telling the desktop a never-existing path was deleted), and `dry_run`
derived from `!confirm`, so a confirmed delete of an absent skill reported a
dry-run.

Rule: the absent branch answers through `routes::noop_removal_response`, the
same `outcome: "absent"` shape every other already-gone path uses.

Commit: b610db98.

## delete-by-path shared slot guard

The by-path route's non-link (copy) branch bypasses `plan_removal`'s referrer
sweep, so it re-applies its own guard. The sweep alone was not enough: a shared
slot such as `.agents/skills` is read by scanning the directory, so up to ten
project agents could read it with ZERO links pointing at it. The route then
`remove_dir_all`'d it and reported `removed`, while
`DELETE /agents/<a>/skills/<n>` and the CLI `delete` refused the same request —
this was the one delete surface that never came through
`remove_skill_planned`.

A first fix asked "is this a Master?". A Master is shared by construction, so
that answered yes for every one and made the desktop's per-LOCATION delete
unable to drop any Master at all. The dialog groups installs by exact
`source_path` and sends every agent installed there, which means "drop this
location" with nobody left to surprise.

Rule: refuse only a request that names SOME of the location's readers — ask
`skill_dir_readers_outside` who is left over (plus `dir_has_external_referrer`
for links), and let a request covering the whole set through. The kept answer
goes through the `RemovalView` seam (`outcome: "kept"`); it used to be a
hand-built `success: true`, and the desktop dialog closed on it as if deleted.

Pinned by: `delete_by_path_keeps_shared_slot_read_by_other_agents`,
`delete_by_path_keeps_shared_slot_referenced_by_another_agent_symlink`,
`delete_by_path_removes_shared_slot_when_every_reader_is_in_the_request`,
`delete_by_path_full_group_keeps_shared_slot_with_legacy_named_referrer`
(`crates/api/src/routes/skills.rs` tests).

Commit: 1b373c2c.

## delete-by-path hand-built outcome

The by-path route used to assemble its `RemovalOutcome` by hand and drifted from
the manager's twice: the preview hard-coded `PruneStatus::NotRun`, and the
commit hard-coded `failed_paths` empty, which made `partial` unreachable there.

Rule: preview and commit both go through the core producers
`RemovalOutcome::preview` / `RemovalOutcome::commit`.

Commit: 6a30ab8b.

## removal response dry-run inference

`routes::removal_response` used to infer `dry_run` from `!outcome.executed`. That
reported `dry_run: true` for a CONFIRMED delete whose target was already absent
— reading as "your request was not carried out" when it was already satisfied —
and serialized an already-gone resource identically to a refused preview.

Rule: the caller passes `requested_dry_run` (`!confirm`) explicitly.

Commit: b610db98.

## log fairing redaction test binary

`ApiLogFairing` must never log headers (`X-Aghub-Git-Tokens` carries raw git
tokens). Its guard used to live inside the lib test binary. `log::set_logger`
succeeds once per PROCESS, so a capturing logger sharing a binary with any test
that builds a Rocket first loses the race and records nothing: the buffer was
empty, every `!logs.contains(secret)` assertion was vacuously true, and dumping
the whole header map plus the token by name still passed.

Rule: the guard lives in its own binary, `tests/log_fairing_redaction.rs`
(`api_log_fairing_never_logs_the_forwarded_token_header`), built via
`build_rocket_for_tests`. Do not move it back into the lib.

Commit: 6971c515.

## keyring read cache

The OS credential store is not a cheap read: on macOS it serializes a process's
concurrent access and can block for seconds on the first touch after the
keychain locks. Startup issues several credential reads (the credentials route,
the update check, a source diff); paying that once per read made a single
check-updates spend 22.9s in credential resolution. Splitting
`load_credentials` / `load_source_bindings` into two concurrent tasks was tried
and measured worse (5.3s became 22.9s) — more contenders only deepened the
keychain queue. The fix was fewer round trips: a 30s read cache in
`KeyringJson` (commit 13bce81e), refreshed on every write through it.

A dismissed macOS authorization dialog (`User canceled the operation`) was then
re-opened by each independent startup read: one cancel became three prompts and
~10s of waiting. Failed reads are now remembered for 5s, replayed with their
original `Unavailable` / `Other` classification (commit cd0da800).

Pinned by: `a_replayed_failure_keeps_its_original_classification`,
`the_failure_window_is_far_shorter_than_the_success_window`
(`crates/api/src/credentials/mod.rs`).

## apply-update keyring fail-closed

`apply_skill_update` once loaded a permissive keyring snapshot that degraded ANY
read failure — including an unreachable backend — to an empty snapshot. For a
MUTATING route that meant a keyring outage silently resolved "no credential"
and the request failed later with a confusing error instead of a stable,
retryable 503. git-scan's host-scoped fallback (no explicit `credential_id`) had
the same bug via `.ok()?` / `.unwrap_or_default()`, letting a private source
proceed as public (GitHub #15, found in Codex review).

The first tests forced the outage via `DBUS_SESSION_BUS_ADDRESS`, which only
affects Linux secret-service; on macOS/Windows runners they silently observed a
non-503 result. They now use
`crate::credentials::test_hooks::ForceCredentialBackendUnavailable`.

Pinned by: `apply_skill_update_route_fails_closed_when_keyring_backend_unreachable`,
`git_scan_host_fallback_fails_closed_when_keyring_backend_unreachable`,
`install_fails_closed_when_keyring_backend_unreachable`,
`delete_route_fails_closed_when_keyring_backend_unreachable` (inference).
Complement: `apply_update_forwarded_token_succeeds_even_when_keyring_backend_unreachable`.

Commit: 58b06365.
