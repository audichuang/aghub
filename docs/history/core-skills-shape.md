# core-skills-shape history

Incident history moved out of `crates/core` code comments. The code keeps the
current rule; each entry here keeps what happened and why.

## Candidate referrers were once shape derived

`candidate_referrers` used to be the union of "links that already resolve to
the Master" and "agents that natively read `.agents/skills`". The first half
admitted only paths that were already conformant, so a dangling link, a foreign
target and npx's real directory were filtered out before anything could report
them. The second half returned a variant carrying no path, so cursor / codex /
opencode lost their private dirs — the agents the per-agent-Referrer decision
exists to serve.

Rule: derive every candidate from the agent's own WRITE dir (private where it
has one, the shared slot where it does not), whether or not anything is there.

Pinned by: `global_candidates_prefer_private_dirs_over_the_shared_slot`.
Commit: d67ab33f.

## Aliased store dropped shared slot agents

`skill_write_dir` once went through `agent_link_need`, whose `NativeReader`
arm (since deleted) carried no path and was reached whenever the agent's dir
resolved to the store. With `.agents/skills` symlinked into `.aghub` (stow, or
a hand-fixed layout) every shared-slot agent classified as a native reader and
dropped out of the candidate set, so `repair` silently did nothing for the
eight agents that most needed it.

Rule: ask the adapter for the write dir directly.

Pinned by: `an_aliased_master_refuses_the_whole_plan`. Commit: c0c90e2c.

## Legacy shape variant offered user dirs for adoption

`SkillShape` once had a `Legacy` variant. The spec defines legacy as "the LOCK
names it, a real directory serves it, and no Master exists", but
`classify_shape` cannot see the lock: it classified a user's hand-placed
`.cursor/skills/<n>`, and even a regular file, as legacy and offered it for
adoption as the Master, which D5 forbids.

Rule: `classify_shape` is observation only; whether an `UnmigratedCopy` may be
adopted is `plan_repair`'s call, where the lock and slot identity are known.

Pinned by: `an_unlocked_directory_is_never_adopted`,
`a_regular_file_at_the_slot_is_refused_not_adopted`. Commit: fc7dc3f1.

## Repair quarantined category directories

Several agents group their own skills under a category directory
(`~/.hermes/skills/research/` holds fourteen sub-skills and a
`DESCRIPTION.md`). When aghub managed a skill also called `research`, the
category dir classified as a forked copy and `repair`'s `fix:` line told the
user to "keep the one you want" — quarantining it would move somebody's whole
skill collection aside.

Rule: a real directory with no root `SKILL.md` is `ForeignDir`, left strictly
alone, and is asked BEFORE the fork / unmigrated split.

Pinned by: `a_directory_that_is_not_a_skill_is_foreign_not_a_fork`.
Commit: 6b299366.

## Readers of counted bare existence

`readers_of` once counted any entry at the skill's name. A same-named category
directory then put every agent sharing that compat dir into `grant_to`, which
turned an absent shared row into `Create` — a managed skill granted to readers
that never read it.

Rule: a read counts only for a root `SKILL.md` (`SkillMarker::Present`).

Pinned by: `readers_of_ignores_a_same_named_category_dir_with_no_root_skill_md`,
`readers_of_ignores_a_same_named_regular_file`. Commit: 6b299366.

## One bool marker served two callers

The "does this entry hold a root `SKILL.md`" probe was a bool. `chmod 000` on a
same-named collision directory made the probe unreadable; the bool said "yes"
(fail-open, correct for `classify_shape`), and `readers_of` read that same yes
as "this agent reads the skill" and linked it into the shared `.agents/skills`
slot. Found by running `repair` against that state on the CLI.

Rule: `SkillMarker` is three-state; `classify_shape` folds `Unknown` to
`Present` (stays loud, routes to a refusal), `readers_of` folds it to not
present (no grant without evidence).

Pinned by: `an_unreadable_same_named_dir_never_seeds_an_implicit_create`,
`readers_of_treats_an_unreadable_same_named_dir_as_not_a_reader`,
`an_unreadable_directory_is_not_mistaken_for_a_foreign_one`. Commit: 19c5ff64.

## Dangling referrer rescue

The marker rule dropped a dangling Referrer: `has_skill_marker` asks about
`<entry>/SKILL.md`, which fails `NotFound` once the link target is gone, so a
stranded Referrer (`.agent/skills/<name>` after a migration or a hand-deleted
shared slot — the family `agents/antigravity.rs` documents) looked like an
agent never granted the skill, and repair lost its only way back. The first
fix counted any `is_link`, which was too wide: a link to an existing directory
with no `SKILL.md` counted as a prior grant, and `repair --yes` created
`.agents/skills/<name>`, handing the skill to all eight shared-slot agents.
Verified by running.

Rule: count a link only when its target is definitively `NotFound`
(`is_dangling_link`); EACCES / ELOOP / a dead mount is unknown and seeds no
grant. `Linker::is_link` (lossy) is used, not `is_link_checked`, so an
unreadable parent reads as "not a reader".

Pinned by: `readers_of_counts_a_dangling_compat_referrer_as_a_reader`,
`readers_of_counts_only_a_definitively_dangling_link`,
`readers_of_still_excludes_an_unreadable_compat_directory`. Commit: deefc56b.

## Migration created no per agent referrers

`plan_repair` once decided actions in the same pass that observed shapes, and
gated `Create` on `master_exists` alone. During a migration the Master does not
exist yet (it is about to be adopted out of the shared slot), so a migration
created no per-agent Referrer: the Master moved into the store and every agent
kept reading it through the one shared link — D6 failing closed.

Rule: two passes — shapes first, then actions gated on `will_have_master`
(exists OR is being adopted).

Pinned by: `an_agent_that_reads_it_today_is_granted_an_explicit_referrer`.
Commit: c4035c17.

## Private copy won adoption by registry order

Adoption was once decided by registry order, which let an agent's PRIVATE copy
become the Master while the real Master-to-be in the shared slot was planned
for `Relink` — the one action that destroys a directory.

Rule: exactly one adoption, and only from the shared slot of a lock-named
skill.

Pinned by: `a_private_copy_never_wins_adoption_over_the_shared_slot`.
Commit: fc7dc3f1.

## Repair migrated git tracked source B6

Bulk `repair` migrated every lock-named skill, including real directories git
tracks (authored in place). Both moving actions (`AdoptAsMaster`,
`CompareThenQuarantine`) rename the directory into the ignored store, so the
skill's source left version control: 39 deletions in `git status`, exit 0,
doctor green. Shape cannot tell authored source from a pre-2.18 install (both
`UnmigratedCopy`), and neither can the lock (aghub's own repo has 22 hand-edited
skills in `skills-lock.json`). Reported against a linked worktree. Root
`AGENTS.md`'s D7 "migration deliberately leaves alone" was only true of the
lazy path.

Rule: a moving action on a git-tracked directory refuses (`GitTrackedSource`,
all tracked paths on one refusal); an unanswerable probe inside a repo refuses
(`GitTrackingUndecided`). Deliberately not "is it inside a repo", and
scope-blind (dotfiles repos hold `~/.agents/skills`).

Pinned by: `a_git_tracked_shared_slot_refuses_instead_of_migrating`,
`an_untracked_shared_slot_inside_a_repo_still_migrates`,
`a_git_tracked_fork_is_not_quarantined`,
`an_unanswerable_git_probe_refuses_rather_than_migrating`. Commit: 486ebb61.

## Compat sweep unlinked the physical shared slot

An earlier compat-Referrer sweep compared a compat dir with the write slots by
`==` on the constructed `PathBuf`s. A compat dir reached through a symlinked
ancestor (`.agent/skills` -> `.agents/skills`) is a different string from the
slot it aliases, so the sweep planned an `Unlink` for the physical shared slot
itself.

Rule: compare by `entry_identity`, never by path string.

Pinned by: `an_aliased_compat_dir_never_schedules_the_shared_slot_for_unlink`.
Commit: 19c5ff64.

## Compat probe folded permission errors

The compat unlink test once probed with `Linker::is_link`, which folds every
I/O error to `false`. A `PermissionDenied` probe read as "nothing here", so a
row never actually removed still reported itself removed.

Rule: `compat_unlink_permitted` fails closed via `Linker::is_link_checked`;
only `NotFound` reads as nothing to detach.

Pinned by: `a_compat_entry_that_changed_since_planning_is_never_reported_as_unlinked`,
`an_unreadable_compat_dir_refuses_the_plan_instead_of_a_silent_no_op`.
Commit: 19c5ff64.

## Compat sweep asked only the first reader

The "nobody's write slot" guard once also protected agents still READING a
shared dir. That only worked while every shared dir happened to have a writer —
an accident of the roster that ran out at project scope, where `.agents/skills`
is amp's alone against a dozen readers. Separately, the coverage check asked
"is the descriptor I am looping over covered?" and detached on the first
`true` — an existential test authorizing a global action on a possibly-shared
entry; every other reader was `continue`d before it was recorded, so an agent
whose only way in was that entry lost the skill without a vote.

Rule: guard 4 (`compat_unlink_authorized`) quantifies over EVERY reader of the
entry, matched by `entry_identity`, and an entry nobody was observed reading is
not detachable. It is ANDed with the write-slot guard, never substituted. It is
pure (roster and coverage as data) because the real roster has no dir read by
two agents and written by none, and `set_skills_path_override` cannot make one.

Pinned by: `a_shared_compat_entry_is_spared_unless_every_reader_is_covered`,
`the_compat_sweep_never_takes_what_it_must_not`. Commit: ff013fe3.

## Compat sweep skipped the unreadable probe

The sweep once `continue`d early for an uncovered agent, which skipped the
fallible probe — the only thing that notices an unreadable compat dir. `repair`
reported `ok` with a stale referrer sitting there, and `UnreadableCompatDir`
only refused when some other agent's write slot happened to be covered.
Verified by running.

Rule: every agent's read dirs are probed; coverage gates only the destructive
half, inside `compat_unlink_authorized`.

Pinned by: `an_unreadable_compat_dir_refuses_even_with_no_covered_slot`.
Commit: ff013fe3.

## Adopt source coverage cost a second run

The coverage table once counted only `Create` / `Relink` / conformant `Leave`
rows. An agent whose write slot IS the adopt source (antigravity at project
scope, where its write dir and the shared slot are the same directory) was not
covered, so detaching its compat link needed a second `repair` run.

Rule: an `AdoptAsMaster` row covers its slot's agents, because step 5 swaps it
for a link to the Master.

Pinned by: `a_migration_detaches_the_compat_referrer_in_the_same_run`.
Commit: 5a14405e.

## Verify shape printed two shapes in one detail

`verify_shape` once matched on the action and fell through a `_` arm for the
detail text, printing "a foreign link target … something that is neither a link
nor a directory" — two different shapes in one sentence.

Rule: match the blocking shapes exhaustively so a wrong detail is unreachable.

Pinned by: `commit_refuses_a_forked_copy_before_deleting_anything`.
Commit: 74aa044a.
