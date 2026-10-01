# core-removal history

Incidents behind skill removal and resync in `crates/core`
(`skills/removal.rs`, `skills/resync.rs`, `dto/removal.rs`).

## Slot only removal sweeps

Both removal planners (`plan_symlink_removal`, `plan_copy_removal`) and
`dir_has_external_referrer` used to sweep only `<dir>/<sanitized-name>`, which
is where aghub itself installs. `npx skills` and older aghub releases wrote
`<dir>/<folder>` under a different frontmatter `name`, and discovery recurses,
so a grouped layout puts the skill at `<dir>/<team>/<folder>`. The planner then
found nothing while the agent kept reading the skill: `--all-agents` reported a
clean `removed` with the nested copy untouched, and the symlink path turned into
a hard `unsupported_operation` refusal of a legitimate delete. Separately, one
unreadable sibling used to abort the whole scan, collapsing the union to the
slot and making a directory another agent still links into look unreferenced.

Rule: sweep the slot UNION what discovery reads (`candidate_entries`, narrow and
name-matched, for deletes; `referrer_candidates`, wide and identity-matched, for
"does anything resolve here?"), and fail closed on the incompleteness flag.

Tests: `all_agents_delete_reaches_a_nested_copy_in_another_agents_dir`,
`all_agents_delete_works_for_an_npx_folder_name_mismatch`,
`a_broken_sibling_must_not_hide_a_nested_referrer`.
Commits: 1b373c2c, ab0d85fe.

## Unstatable entry dropped from the sweep

`plan_symlink_removal` used to `continue` past an entry whose
`symlink_metadata` failed with anything but NotFound. `delete --all-agents`
then neither counted it as a holder nor unlinked its Referrer, while still
reporting `success: true` with the path silently missing from the JSON.

Rule: NotFound means "not held"; any other stat error is an unknown holder and
keeps the shared Master, exactly as a known one would. Only entries this removal
names are reported in `skipped`.

Tests: `reconcile_skill_will_not_gc_the_master_when_a_holder_is_unreadable`,
`an_unreadable_agent_does_not_block_a_referrer_unlink`.
Commit: 5e5567dd.

## Unreadable peer dir hid an inbound link

In `dir_has_external_referrer`, `Linker::is_link` and
`canonicalize(..).unwrap_or(false)` both answer "no" to EACCES, so an unreadable
peer directory hid a live inbound symlink and the copy-layout removal
`remove_dir_all`'d the directory it pointed at. Verified: identical runs
differing only in the peer dir's mode either skipped the directory (0755) or
deleted it and left the peer's link dangling (0400), both exit 0. The first fix
failed closed on EVERY stat error, which was too blunt: the loop runs over every
agent in the roster, so one odd directory blocked copy-layout deletion of every
skill.

Rule: fail closed only where a referrer could actually be — NotFound /
NotADirectory skip; an unstat-able entry is cleared by a complete name-only
`read_dir` listing that does not contain its leaf (`dir_lists_name`); an
unfinished listing is an unknown referrer.

Commit: 5e5567dd.

## Reconcile refusal hid the agents own read dir

The reconcile preflight used to derive its own refusal message instead of
reading the removal verdict. It listed "who else reads the Master" while staying
silent about the agent's OWN second read dir — the only thing the user could
have acted on.

Rule: `RemovalPlan::still_read_from` is populated only by the one verdict owner
(`read_effect_after`, via `remove_skill_planned`), so `delete`, the API delete
route and `reconcile skill` name the same paths.

Test: `reconcile_skill_refuses_when_the_named_holder_is_the_unreadable_one`.
Commit: 5a14405e.

## Preview and commit duplicated per surface

`RemovalOutcome::preview` / `commit` used to have a second hand-written copy in
the API by-path delete route. It drifted twice: the route hard-coded
`PruneStatus::NotRun`, so its preview under-reported the lock cleanup its own
commit performed; and it hard-coded `failed_paths` empty, so
`RemovalKind::Partial` was unreachable there — a delete where every
`remove_dir_all` returned EACCES reported `outcome: "removed"` with the skill
still on disk, and the desktop closes its dialog on `removed`. The CLI also
shipped a preview that printed a plan the commit then refused, because the
shape check ran at the call sites rather than inside the producer.

Rule: `preview` and `commit` are the only producers of a skill
`RemovalOutcome`, and both run `verify_shape` themselves.

Tests: `delete_preview_discloses_the_lock_entries_it_would_prune`,
`a_run_whose_deletes_all_failed_is_not_reported_as_removed`.
Commit: 6a30ab8b.

## Partial removals reported as removed

The execute branch sets `executed: true` unconditionally and folds failed
deletes into `plan.skipped`, so a run where every delete failed reported
`executed: true` and, once the three-way outcome existed, `outcome: "removed"`
for files that were all still there. After `RemovalKind::Partial` was added,
`RemovalView::success` was still hard-coded `true`, so a `delete --yes` that
deleted nothing because of EACCES exited 0 while the variant's own doc said
"do not read this as success".

Rule: `RemovalOutcome::failed_paths` carries the failures separately;
`Partial` is checked before `Removed`, and `success` is false for `Partial`.

Test: `a_run_whose_deletes_all_failed_is_not_reported_as_removed`.
Commits: bb39d10a, 994e2e6d.

## Absent was indistinguishable from preview

`dry_run` used to be derived from `!outcome.executed`, and an "already gone"
outcome and an unconfirmed preview both had `executed: false`. They serialized
identically — the same md5, byte for byte, for `delete skills nope -y` and
`delete skills nope` — while the human renderer told them apart ("nothing to
remove" vs "would remove … re-run with --yes"). A confirmed caller whose target
no longer existed was told `dry_run: true`, read it as "my confirmation was
ignored", and retried forever.

Rule: `dry_run` is the caller's intent (`RemovalView::from_outcome` takes it
explicitly; there is no `From<&RemovalOutcome>`), `RemovalOutcome::absent` is
set only by `noop()`, and `RemovalKind::Absent` outranks the caller's intent.

Tests: `preview_and_absent_are_machine_distinguishable`,
`delete_json_distinguishes_preview_removed_and_absent`,
`delete_yes_on_an_absent_resource_does_not_ask_for_yes_again`.
Commit: b610db98.

## Kept outcome for a shared Master

A single-agent removal that resolves to a shared Master takes nothing away, so
an executing call refuses with `unsupported_operation`. Its dry-run used to
report `preview` — "re-run with --yes" pointed straight at a guaranteed error —
and the API reported it as plain `success: true`, which is why the desktop's
delete dialog closed on a skill that was still installed. `plan.shared_master_kept`
already existed and was read by the manager and transfer, but never by the wire
view. Later the same hint reappeared for an npx-era Referrer beside its Master:
the plan had a path to unlink, the preview said `preview`, and `--yes` answered
`unsupported_operation`.

Rule: `RemovalKind::Kept` when `shared_master_kept && (paths.is_empty() ||
!executed)`; it outranks every other answer.

Tests: `a_kept_shared_master_is_not_a_removal_or_a_preview`,
`a_preview_the_commit_will_refuse_reports_kept_not_preview`,
`a_kept_shared_master_preview_promises_no_prune`.
Commits: 899eb9f6, 1b373c2c.

## Resync failure codes diverged per surface

The CLI's `source sync` used to render a `StaleFetch` resync failure as free
text with no code at all, while the API answered 409 +
`SOURCE_CHANGED_DURING_FETCH` for the very same condition.

Rule: `resync` owns ONE machine-code classification read by every surface;
surfaces own only the wording.

Commit: fd106912.

## All agents delete asked only the initiator

`remove_skill_planned` once asked discovery "did that removal take anything
away?" over the INITIATING agent's read dirs only. That let an `all_agents`
delete report a clean `removed` while a second agent went on discovering the
skill from a layout the planner had missed. Separately,
`transfer::reconcile_skill` used to keep its own copy of the verdict and
refuse shapes that the CLI `delete` and the API delete routes — which come
through `remove_skill_planned` — reported as `removed`.

Rule: `remove_skill_planned` is the ONE home for the verdict, and with
`all_agents` it reads every agent's dirs — the same dirs the planner just
swept, so the two cannot disagree unless the planner really left something
behind.

Commit: 1b373c2c.

## Disabled agent blocked a single-agent delete

`skill_dir_readers_outside` walked the full `AgentType::ALL`, so an unmanaged
agent blocked `delete skills x -a cline` with "Also read there by agents not in
this request: cursor (disabled)" and the only escape was `--all-agents` or
re-enabling.

Rule: disabled = unmanaged, excluded from unselected readers; Master retention
still full roster.

Tests:
`manager::skill::tests::single_agent_remove_skill_shared_slot_succeeds_when_other_reader_disabled`,
`single_agent_remove_skill_shared_slot_refused_when_other_reader_enabled`,
cli test `delete_single_agent_ignores_disabled_shared_slot_reader`.

Follow-up (review finding B1): the `< 2` "only the initiator reads this slot"
shortcut in `unselected_reader_needs_referrer` had inherited the filter. With a
DISABLED initiator (`-a <disabled>` is not rejected by the CLI) plus one enabled
reader it counted 1, returned "nobody needs it", and deleted the shared Referrer
and Master an enabled, unselected reader still used. The shortcut now counts the
full roster (`readers_outside(.., include_disabled = true)`); only the set that
names/blocks stays filtered.

Resolved (was a known gap): when a real directory is read by more than one
agent, the CLI/core single-agent delete must protect every enabled reader even
if the directory is outside a shared slot. The original fix handled a real
directory in `.agents/skills`, but API by-path then exposed a second data-loss
case: `.claude/skills/x` was a real directory while `.cursor/skills` was a
dotfiles-style symlink to `.claude/skills`; deleting by either agent alone
removed the bytes from both. The verdict now lives in
`single_agent_keep_reason` alone, in this order: (1) inside the `.aghub` store
-> keep unconditionally (no agent reads it, so "no reader outside" would
misfire and delete a Master); (2) a symlink resolving to it -> keep, EXCEPT a
link in a requested agent's own private skills dir that no unrequested enabled
agent also reads (below); (3) with non-empty `requested`, any real directory is
kept iff an ENABLED reader is outside the request (disabled agents are
unmanaged and never count); an empty request fails closed only inside a shared
Referrer root; (4) a shared-root directory git tracks is kept too (below); (5)
a private copy with no outside enabled reader -> delete.
It is stricter than the link rule: no `slot_reader_count < 2` shortcut and no
"the other reader has another copy" release, because deleting a real directory
deletes content. There is no `.aghub` Master behind it, so the lock entry is
pruned too: local edits cannot come back, though a skill that originally came
from a source can be reinstalled fresh with `source sync <repo>`. The API
by-path route, `reconcile` (and so the desktop's manage-agents / bulk dialogs,
through `requested_removals`) call the same function. `--all-agents` is
unchanged.

Order independence (the verdict must not depend on `-a` order): step (2) used to
see only the disk as it is when a row is planned, but a batch executes rows
sequentially. In the npx layout (`.agents/skills/x` a real directory,
`.claude/skills/x` a link to it) `-a claude,cursor,opencode` previewed cursor as
`kept` while `--yes` ran claude's row first, which unlinked the link, so cursor's
row then found no inbound link and deleted the only copy; `-a
cursor,opencode,claude` kept it. Now a link that lives in a REQUESTED agent's
private skills dir (and that no unrequested enabled agent also reads) is not an
external referrer, because that agent's own row removes it; the row that
releases the directory also plans those links (`plan_owned_inbound_links`, the
one assembly shared by the copy-layout planner, the symlink-layout planner and
the API by-path route), so no dangling link is left in either order and the
preview's paths equal what executes (as a set across the batch). A link in a
shared root, in an unrequested agent's dir, or in an unmanaged dir an enabled
reader also uses stays an external referrer: the delete exits 1 and names it,
exactly as base did. A link that lives in a directory no agent owns is outside
the scan altogether (as in base): it is neither named nor unlinked, and it
dangles once the directory is deleted.

The second order-dependence (review finding M-1): a requested agent that has its
OWN link into the directory (claude and cursor both linking to
`.agents/skills/x`, only claude/cursor/opencode enabled) takes the
symlink-layout planner, whose `other_refs` scan saw the other requested agent's
link and the real directory as survivors and judged that row `blocks`, so 3 of
the 6 `-a` orders exited 1 and 3 exited 0 over identical final disks
(`reconcile skill --remove` likewise). `plan_removal_for_agents` now asks
`single_agent_keep_reason` first for such a row, when the canonical is a real
directory in a shared slot and the request is non-empty; on release it plans the
owned links plus the directory exactly like the copy layout, and on any keep it
falls through to the unchanged symlink planner, so a single-agent delete of
just that agent's link (another agent still linking) keeps working. Rows that
run after the directory is gone end as the existing `absent` noop (the CLI's
`plan_or_noop` maps `ResourceNotFound`), exit 0. A refused batch is not atomic:
every row is attempted, so a requested agent's own row may already have
unlinked its link when another row refuses (identical in either order).

Git-tracked directories: a real directory git TRACKS is authored source, and
deleting it for "some agents" would remove it from version control. Like
`repair` (`GitTrackedSource`, reusing `shape::git_tracked`), the release step
refuses it (`KeepReason::GitTracked`; the warning prints
`git rm -r --cached <path>` as the escape). Divergence on purpose: `repair`
also refuses when tracking cannot be decided (git missing, unusable
repository); a delete treats that as untracked and goes ahead, because git being
absent must not make a skill undeletable. Only the shared-slot real-directory
release is gated; `--all-agents` and the link layout are not. The refusal
message itself carries the tracked path and `git rm -r --cached <path>` escape;
the current `--all-agents` path can still delete such a directory and is not a
supported bypass. `git_tracked` clears inherited `GIT_DIR`, `GIT_WORK_TREE`,
`GIT_INDEX_FILE`, `GIT_COMMON_DIR`, `GIT_OBJECT_DIRECTORY` and `GIT_NAMESPACE`
before spawning git, so a hook or parent process cannot make the probe answer
for another repository.

A released real directory under a shared Referrer root keeps the symlink
planner's `needs_confirm: true` wire contract. The shared assembly helper had
temporarily reported `false`; ordinary private copies remain `false` and retain
their original no-extra-confirm behavior.

Known edge: when a link from a DISABLED agent's private dir points at the
directory, the refusal path lists only the path, not the agent. Not changed.
The two other edges that used to be listed here are fixed:
[a failed directory delete](#dir-delete-failure-left-links-unlinked) and the
reconcile "still read by" clause naming disabled agents (below).

Tests:
`manager::skill::tests::dotfiles_shared_private_dir_obeys_the_complete_requested_reader_set`,
`manager::skill::tests::real_dir_shared_slot_single_agent_remove_succeeds_when_other_readers_disabled`,
`skills::removal::tests::plan_removal_copy_single_agent_removes_only_targeted_copy`,
`manager::skill::tests::real_dir_shared_slot_kept_when_enabled_reader_not_in_request`,
`manager::skill::tests::real_dir_shared_slot_removed_when_request_names_every_enabled_reader`,
`manager::skill::tests::real_dir_batch_verdict_is_independent_of_order`,
`manager::skill::tests::real_dir_batch_with_own_links_is_order_independent`,
`manager::skill::tests::real_dir_keeps_and_refuses_when_link_belongs_to_unrequested_agent`,
`manager::skill::tests::real_dir_with_unrequested_enabled_linker_refuses_the_direct_reader_row`,
`manager::skill::tests::real_dir_empty_request_fails_closed`,
`manager::skill::tests::real_dir_git_tracked_single_agent_delete_is_refused`,
`manager::skill::tests::real_dir_untracked_in_git_repo_single_agent_delete_is_allowed`,
`skills::removal::tests::single_agent_keep_reason_git_tracked_refuses_untracked_allows`,
`skills::removal::tests::single_agent_keep_reason_protects_private_dir_shared_by_dotfiles_layout`,
`skills::shape::git_env_tests::git_tracked_ignores_inherited_repository_environment`,
`skills::removal::tests::single_agent_keep_reason_aghub_store_real_dir_is_kept_when_everyone_is_requested`,
cli tests `dotfiles_shared_private_dir_delete_uses_the_complete_agent_list`,
`real_dir_delete_*`, `real_dir_npx_layout_*`,
`delete_real_dir_with_own_links_is_order_independent`,
`reconcile_skill_remove_real_dir_with_own_links_is_order_independent`,
`delete_real_dir_with_unrequested_enabled_linker_is_refused_any_order`,
`delete_real_dir_git_tracked_is_refused_untracked_allowed`, api tests
`dotfiles_shared_private_dir_is_kept_by_name_and_by_path_for_either_reader`,
`delete_by_path_release_also_unlinks_requested_agents_private_link`,
`delete_by_name_removes_real_shared_dir_when_request_names_every_enabled_reader`,
`manager::skill::tests::single_agent_remove_skill_refused_when_initiator_disabled_and_other_reader_enabled`,
`manager::skill::tests::single_agent_remove_skill_project_scope_refused_when_initiator_disabled_and_other_reader_enabled`,
cli test `delete_single_agent_disabled_initiator_refuses_enabled_shared_slot_reader`.

## Reconcile delete order needs the full roster

After disabled agents were dropped from `skill_dir_readers_outside`, reconcile's
delete sort key (which used it with an empty exclusion) tied shared and private
slots when the non-enabled agents were disabled; a private row could run before
the shared row and preflight refused ("still served ... from .agents/skills").
The key now uses `slot_reader_count` (full roster).

Rule: slot sharing is structural, independent of management; delete ordering
counts the full roster.

Tests:
`transfer::tests::reconcile_orders_shared_referrers_first_when_other_agents_disabled`.

## Dir delete failure left links unlinked

A real-directory release planned `[owned inbound links..., directory]`, and
`execute_removal` runs a plan in order. When `remove_dir_all` then failed (a
read-only parent, mode 555: the children go, the final `rmdir` is refused) the
requested agents' links were already gone and nothing restored them, so a
failed delete still revoked the grants.

The order is now `[directory, links...]`, from the one producer
`removal::plan_dir_release_paths` (the manager's copy-release assembly and the
API by-path route both call it), and `execute_removal` skips a link that
resolves into a directory whose removal failed, reporting it in `skipped`
beside the directory in `failed`. Restoring unlinked links was rejected: it
needs a link constructor per platform (junctions on Windows) and can fail too.
Residual: `remove_dir_all` is not atomic, so the directory may be emptied
while the links stay; the surfaces report the directory as failed, and a
re-run deletes the leftovers.

The reconcile refusal's "the shared master is still read by ..." clause
(`ReconcileSkillPlan::keepers`) comes from `skill_holders`, which walks the FULL
roster because it also answers the Master-GC question (`exhaustive`). The
clause now drops disabled agents: they still keep a Master alive but are not
readers, so the message must not name them. The refusal VERDICT is untouched
(`read_effect_after`).

Tests:
`manager::skill::tests::real_dir_delete_failure_keeps_the_requested_agents_links`,
`transfer::tests::reconcile_refusal_does_not_name_a_disabled_agent_as_a_reader`.
