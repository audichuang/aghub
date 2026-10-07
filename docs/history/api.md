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

Rule: refuse only a request that names SOME of the location's readers, and let
a request covering the whole set through. The verdict is owned by core's
`single_agent_keep_reason` (the route no longer hand-writes the OR of
`skill_dir_readers_outside` and `dir_has_external_referrer`), so CLI, by-name
and by-path answer identically; this includes a private directory co-read
through another agent's dotfiles-style symlinked skills root. A real directory
inside `.aghub` is kept, and so is a shared-root directory git tracks. A
GitTracked response includes the exact `git rm -r --cached <path>` escape in its
`error` field. The kept answer goes through the `RemovalView` seam
(`outcome: "kept"`); it used to be a hand-built `success: true`, and the desktop
dialog closed on it as if deleted.

Releasing the directory also unlinks the links the requested agents own in their
private skills dirs, or a dangling link would stay behind. The route does not
assemble that list: it calls core's `plan_owned_inbound_links`, the same function
the planners use (a first version copied the loop and drifted by one condition).
See `core-removal.md#disabled-agent-blocked-a-single-agent-delete`. The by-path
`..` rule below still runs first, so a root, `<root>/SKILL.md` or `<child>/..`
never reaches the keep rule.

Pinned by: `delete_by_path_keeps_shared_slot_read_by_other_agents`,
`delete_by_path_keeps_shared_slot_referenced_by_another_agent_symlink`,
`delete_by_path_removes_shared_slot_when_every_reader_is_in_the_request`,
`delete_by_path_full_group_keeps_shared_slot_with_legacy_named_referrer`,
`delete_by_path_release_also_unlinks_requested_agents_private_link`,
`delete_by_path_git_tracked_is_kept_untracked_is_deleted`,
`dotfiles_shared_private_dir_is_kept_by_name_and_by_path_for_either_reader`
(`crates/api/src/routes/skills.rs` tests).

`delete_by_path_rejects_aghub_store_path_before_the_keep_rule` pins the earlier
per-agent path validation only; it deliberately does not claim to reach or pin
the core keep rule.

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

## delete-by-path target must be a skill root

`DELETE /skills/by-path` took the directory a `source_path` named (or the parent
of a non-directory) and, when `SKILL.md` did not parse, fell back to the FOLDER
name as the skill name. So `<slot>/team` (a category folder with skills only
underneath) looked like a skill called `team` and `remove_dir_all` took every
skill under it; `<slot>/z/scripts` and `<slot>/z/scripts/run.sh` removed a
subdirectory (or `scripts`), and a missing file such as `<slot>/z/gone.txt`
resolved to its parent, the whole skill `z`. A symlink loop (`ELOOP`) made
`is_dir()` false, so the target silently became the slot root and the refusal
echoed that path (`skipped`) although the request never named it. The same
route also hand-built its copy-removal plan with `needs_confirm: false`, while
the by-name planner reports `true` for releasing a real directory from a shared
root.

Rule, owned by core (`removal::by_path_skill_dir` / `by_path_skill_name`):

1. A directory is taken as is; a file must be a `SKILL.md` (its parent is the
   skill); anything else is `NotSkillRoot`. A missing `SKILL.md` still yields
   its parent so the already-gone answer is unchanged.
2. Any metadata error other than not-found (ELOOP, EACCES, a non-directory
   ancestor) is `Unresolvable`: the shape is unknown, so nothing is derived
   from it. Both refusals are fixed strings with no filesystem path.
3. The directory must hold its OWN parsable `SKILL.md`; the name is the
   frontmatter's, and the folder must be that name's slot name (a `folder-v2`
   with `name: real` is refused). A nested skill under a category folder
   (`<slot>/team/a`) is fine; the category folder itself is not.
4. The copy plan comes from `assemble_copy_release_plan`, the producer the
   by-name planner uses, so `needs_confirm` is one answer. It is a report
   field here: the route still deletes only when `confirm: true`.

The folder must also be the slot name the frontmatter name sanitizes to
(`sanitize_name`). Two older pinned tests used a differing folder (a legacy-named
Referrer, npx-era links with another name) as if it were a deletable target; they
now keep the differing name on the Referrer / Master side and give the target
folder its skill's name. Pinned by
`by_path_skill_name_needs_its_own_parsable_skill_md`.

Cost: a skill whose `SKILL.md` no longer parses, or whose folder differs from
its name, can no longer be deleted by path.

Pinned by: `delete_by_path_refuses_category_folder_and_keeps_every_skill_under_it`,
`delete_by_path_refuses_skill_subdirectory_and_stray_file`,
`delete_by_path_symlink_loop_refuses_without_leaking_paths`,
`delete_by_path_needs_confirm_matches_core_plan_for_shared_slot`
(`crates/api/src/routes/skills.rs`);
`by_path_skill_dir_accepts_dir_and_skill_md_refuses_other_files`,
`by_path_skill_dir_refuses_a_symlink_loop_as_unresolvable`,
`by_path_skill_name_needs_its_own_parsable_skill_md`
(`crates/core/src/skills/removal.rs`).

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

## delete-by-path parent-dir rule

`DELETE /skills/by-path` derives the skill directory from `source_path`: the path
itself when it is a directory, else its `parent()`. Two inputs made that land on
the skills ROOT instead of a skill: a path ending in `..` (`Path::file_name()` is
`None`, so nothing pins the last segment to a skill), and a non-directory
directly under the root such as `<slot>/SKILL.md` or `<slot>/<missing-name>`
(its `parent()` is the root). When the request's `agents` were exactly the
slot's readers (per-agent validation passes, and the shared-slot guard sees no
outside reader), main deleted the slot directory itself, or every skill inside
it, depending on the shape: the root itself, `<slot>/`, `<slot>/SKILL.md`,
`<slot>/<missing-name>` and `<slot>/%2e%2e` removed the slot directory;
`<slot>/y/..` removed every skill inside it but left the slot directory in
place; `<slot>/.` emptied its contents (reported as partial). `agents = ALL`
was only safe because per-agent validation rejected it.

Rule, in two layers:

1. The core removal containment check is STRICT (`assert_strictly_contained`):
   the root itself is never a target, and not even when one root is nested in
   another (a target equal to ANY root is refused).
2. A `..` component is refused only in the part of `source_path` AFTER the
   matching agent skills-root prefix. The first fix refused any `..` anywhere,
   which broke legitimate requests: `project_root` is free text (remote
   connections, raw HTTP) and `absolutize_root` returns an absolute path
   un-normalized, so a project at `~/x/../proj` produces list `source_path`s
   that contain `..` the user never typed. The prefix comes from the same
   un-normalized `project_root`, so a lexical `strip_prefix` lines up. If no
   skills root strips, the request is refused (fail closed). `y/..`, `y/../z`
   and cross-slot `<P>/.cursor/skills/../../.agents/skills/y` all leave a `..`
   in the remainder. The refusal text carries no filesystem path.

The category-folder gap that used to be listed here is closed by
[delete-by-path target must be a skill root](#delete-by-path-target-must-be-a-skill-root).

Pinned by: `delete_by_path_rejects_trailing_dotdot_project_agents_slot`,
`delete_by_path_rejects_trailing_dotdot_project_cursor_slot`,
`delete_by_path_rejects_trailing_dotdot_global_agents_slot`,
`delete_by_path_rejects_dotdot_in_middle_of_path`,
`delete_by_path_rejects_dotdot_before_skills_root_with_no_strippable_prefix`,
`delete_by_path_rejects_dotdot_before_skills_root_without_trailing_dotdot`,
`delete_by_path_rejects_skills_root_itself`,
`delete_by_path_rejects_skill_md_directly_under_the_skills_root`,
`delete_by_path_rejects_child_then_dotdot`,
`delete_by_path_rejects_dotdot_into_aghub_store`,
`delete_by_path_rejects_cross_slot_dotdot`,
`delete_by_path_accepts_dotdot_in_project_root` (`crates/api/src/routes/skills.rs`);
`assert_strictly_contained_rejects_inner_root_nested_in_another_root`
(`crates/core/src/skills/removal.rs`).

## by-name delete and reconcile removal rows wire fields

`DELETE /agents/<agent>/skills/<name>` and `POST /skills/reconcile` deletion rows now share the core batch removal entry (`remove_skill_batch`).
Removal rows in both endpoints now project the core-decided `outcome` and partition surviving holders into `still_read_by_managed` and `still_read_by_unmanaged` (while deriving `still_read_by` for compatibility).
In addition, whole-batch preflight rejection preserves the HTTP 422 status and `UNSUPPORTED_OPERATION` code, while providing a structured `rejected_targets: [{agent, reason}]` payload; configuration or planner errors similarly preserve HTTP 400 `INVALID_CONFIG` with structured `rejected_targets`.

Pinned by: `reconcile_skill_removal_row_reports_outcome_and_managed_holders`,
`delete_by_name_preview_and_commit_carry_outcome_and_managed_holders`,
`delete_by_name_preflight_rejection_carries_structured_rejected_targets`,
`delete_by_name_preflight_rejection_on_planner_error_carries_rejected_targets`
(`crates/api/src/routes/skills.rs`).
