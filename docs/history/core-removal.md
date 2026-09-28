# core-removal history

Incident history moved out of `crates/core` code comments. The code keeps the
current rule; each entry here keeps what happened and why.

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
