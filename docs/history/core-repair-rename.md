# core-repair-rename history

Incidents behind `crates/core/src/skills` repair, rename and discovery.

## Repair batch loop lived in each surface

The CLI and the API route each had their own copy of the bulk repair loop
(same worklist, same fail-closed lock read, same "stay quiet about conformant
skills" rule) and they had drifted: the route took no outer bulk guard, so a
fifty-skill desktop migration ran as fifty independently racing mutations.
Both loops also used `?`, so an EACCES on skill 25 of 50 discarded the report
for the 24 that had already migrated — the disk was fine (every step is
crash-safe and idempotent) but the user was told nothing.

The guard must also come BEFORE the lock read, because the lock decides which
directory may be ADOPTED as a Master: this run reads `demo` as locked, another
aghub deletes that entry and releases, npx drops a fresh real
`.agents/skills/demo` in place, and this run resumes with a stale
`in_lock = true` and adopts content nobody authorized. Dry runs take no guard
(they write nothing), so a preview still reads outside it — deliberate, and
the reason a preview is authority for nothing.

Rule: `repair::repair_all` is the one home for the batch, under ONE outer
mutation guard taken before the lock read; a failing skill becomes a
`RepairOutcome::Failed` row instead of aborting.

- Tests: `skills::repair::tests::one_failing_skill_does_not_throw_away_the_rest_of_the_batch`,
  `skills::repair::tests::a_second_bulk_run_reports_nothing_left_to_do`
- Commit: 9eb531d7

## Compat unlink recheck and the fourth guard

`execute_repair` step 6 used to record an `Unlink` row's path BEFORE
rechecking it, so an entry that was left untouched still reported itself as
removed. It also used `Linker::unlink`, which folds `NotFound` into success:
an entry another process removed between the recheck and the call was
reported as unlinked by THIS run.

Rule: re-ask `compat_unlink_permitted` (link-only, resolves to this master or
the adopt source, nobody else's write slot) at write time, and record only
what `Linker::unlink_reporting` says this call removed.

The fourth guard ("the write slot covers it afterwards") was once decided only
in `plan_repair`, against the plan: `RepairPlan` carried no scope/root, so a
non-aghub actor breaking the covering slot between plan and step 6 (the mutation
lock only serializes aghub against aghub) left repair detaching the agent's last
link. Closed (D8, #65): `RepairPlan` carries `scope` / `project_root`, and step 6
re-asks `compat_unlink_authorized` with coverage read from the disk (each
reader's own slot Conformant). The check → remove pair is still two syscalls;
narrowing that needs `unlinkat` against a directory fd.

- Tests: `skills::repair::tests::a_compat_entry_that_changed_since_planning_is_never_reported_as_unlinked`,
  `skills::repair::tests::a_covering_slot_broken_after_planning_keeps_the_compat_referrer`,
  `skills::repair::tests::the_compat_sweep_never_takes_what_it_must_not`
- Commits: 19c5ff64 (recheck + fourth-guard gap), deefc56b (`unlink_reporting`)

## Rename snapshot missed grouped entries

`snapshot_old_skill` backed up `<dir>/<sanitized-name>` only, while step 8's
`plan_removal` deletes everything discovery finds under the old name —
including a differently-named or grouped entry such as `<dir>/team/legacy`.
Step 8 could delete a path step 6 never backed up, and a later failure then
rolled back into a permanently missing old skill.

Rule: the snapshot enumerates the SAME candidate set `plan_removal` will
delete (`removal::candidate_entries`), and an incomplete listing aborts before
any mutation.

- Test: `skills::rename::tests::snapshot_covers_the_nested_entries_removal_will_delete`
- Commit: ab0d85fe

## Discovery read unreadable as empty

Several discovery paths turned "cannot read" into "nothing there":

- `load_skills_from_dir` returned the same empty list for an absent and an
  unreadable directory. `chmod 000` on an agent's skills dir made `get skills`
  print `[]` on exit 0, and because `load_all_agents` sets `load_failed` only
  from an `Err`, the agent became invisible to `transfer::skill_holders`.
- `collect_skills` aborted on the first failure, discarding every skill already
  found, so one unreadable sibling made a whole agent dir look empty.
- Per-entry errors went through `flatten()` + `is_dir()`, both answering "no";
  with mode 0400 on a skills dir `read_dir` succeeds and every stat fails.
- A blanket `Err(_)` on `parse_skill_dir` recursed into a directory whose
  `SKILL.md` existed but could not be read, found only files and returned
  `Ok`: `transfer::skill_holders` counted a real reader as a non-reader and the
  shared master was deleted — exit 0, "N succeeded, 0 failed". In a copy layout
  the same truncation widened a single-agent removal into a sweep that deleted
  an UNTARGETED agent's skill directory (commit 5e5567dd).

Two over-corrections followed and were walked back:

- A non-directory path was recorded as a read failure, so `skill_holders`
  counted a provably-empty path as an unverifiable holder and the exhaustive
  guard refused a safe collection (commit ab0d85fe).
- Every `SkillError::Io` was propagated, but `read_to_string` also raises
  `InvalidData` (a non-UTF-8 `SKILL.md`) and `IsADirectory`. One bad file then
  failed every command for that agent — including the `delete` that would have
  removed it (commit 09d068a5). An unparsable entry also set `unlisted`, which
  let one broken skill keep every other agent's directory alive.

A `_partial` twin of `load_master_skills` also existed briefly; it bought an
unrelated delete's convenience with three false answers and was removed —
an unopenable store `SKILL.md` has an unknown name, so a partial store list
cannot answer "not there".

Rule: absent and non-directory are complete answers; any other read failure is
recorded (first failure wins) while the walk continues; only real read errors
on `SKILL.md` count, and they do not set `unlisted`.

- Tests: `transfer::tests::reconcile_skill_will_not_gc_the_master_when_a_holder_is_unreadable`,
  `manager::skill::tests::an_unreadable_store_entry_fails_closed_instead_of_guessing`
- Commits: 355afb61, 29116f38, 5e5567dd, 09d068a5, ab0d85fe
