# core-transfer history

Incidents behind `crates/core/src/transfer.rs`: batch transfer, reconcile and
shared-backing checks.

## Batch row ok field

`OperationResultView` and `core::batch`'s `AgentOpResultView` both claimed to
be "the single place the wire shape is defined", and both serialize into an
envelope with the same top-level keys (`success_count` / `failed_count` /
`results`) — but one spelled the per-row flag `success` and the other `ok`.
A parser written against `row.ok` read `undefined` for every transfer/reconcile
row and scored successes as failures.

Rule: `OperationResultView` emits both `success` and `ok`; `already_present`
is emitted unconditionally so a mixed-version client can tell `false` from
"not reported".

Commit: b610db98.

## Reconcile preview approved what the commit refused

The reconcile preview (`--dry-run`, and the implicit dry-run of `--remove`
without `--yes`) used to be an echo of argv. `--name totally-absent --remove
opencode` printed a plan and exited 0, and only the `--yes` run reported
`Resource not found`. The same held for `--add opencode --remove opencode`
(`ensure_disjoint` ran only on commit) and for the shared-backing refusal.
`confirm = false` is not a dry-run switch — an add-only reconcile with it still
writes — so the preview needs read-only preflights, not a planner call.

Rule: every refusal the commit can raise has a read-only seam the preview calls
first (skill existence is decided by `reconcile_skill_preview`, which shares
`plan_reconcile_skill` with the commit; `ensure_skill_exists` was removed;
`ensure_mcp_exists`, `ensure_sub_agent_exists`, `ensure_disjoint`, and
`ensure_*_reconcile_spares` remain for their respective domains), and preview
and commit share one definition.

Tests: `reconcile_rejects_empty_target_set_and_validates_source_in_preview`,
`reconcile_skill_preview_refuses_what_the_commit_refuses`,
`ensure_disjoint_rejects_agent_in_both_add_and_remove`.

Commit: b610db98.

## Folder hash blind spots authorised a removal

A removing reconcile proves the copy landed with the npx folder hash before it
deletes the source. That hash skips symlinks, `.git` and `node_modules`. A
source with a symlink and a Master without one hashed equal, the reconcile
reported "2 succeeded", and the symlink was gone.

The first fix, `has_unhashed_entries`, stopped recursing at a private depth of
`32`, so trees 33-64 deep that the hash accepts were refused with a false
"contains symlinks" reason.

Rule: anything the hash cannot see answers `Unprovable`, never `Landed`; the
walker uses the hash's own `skill::hash::MAX_DEPTH`.

Commits: 8267b920 (symlink), e84f4967 (depth bound).

## Shared backing destroyed a resource

The reconcile removal guard used to compare agent ids. When two agents read one
file the copy found an equivalent entry and truthfully reported
`already_present`, the staged gate only asked whether the copy errored, and the
removal rewrote the shared file. Every row reported success and the resource was
gone from everyone. Observed shapes:

- `~/.claude.json` hard-linked to `~/.cursor/mcp.json` (dotfile `cp -l`,
  rdfind/jdupes): a reconcile naming claude as a copy target emptied claude's
  config and reported "2 succeeded, 0 failed". `canonicalize` does not collapse
  hard links, hence `Backing` compares `(dev, ino)` (commit e9f8da9a).
- `reconcile --from-agent claude --remove grok` with `~/.grok -> ~/.claude`
  deleted the source's only copy — the guard protected copy targets only, not
  the source (commit 994e2e6d).
- `~/.gemini -> ~/.claude`: "remove from gemini" deleted claude's private
  skill and exited 0.
- An agent named nowhere in the command: Claude and Copilot both resolve a
  project MCP to `<root>/.mcp.json`, and `reconcile mcp --remove claude`
  rewrote it while copilot lost the server too. Hence the roster protect list
  (commit 4a5efb25).
- `Option<PathBuf>` backing lookups read an unrelated broken `config.toml` on a
  sharing agent as "not a holder" (a `ConfigManager` load parses MCPs too), and
  the roster guard deleted the file both read. Hence `Backed::Unknown`, which
  fails closed (commit 4a5efb25).

Rule: membership is a property of the file, never of the agent id; every agent
in the registry that is not being removed is protected (skills excepted, see
`protected_targets`); an undeterminable backing refuses.

Tests: `reconcile_mcp_refuses_when_an_unnamed_agent_shares_the_file`,
`reconcile_sub_agent_refuses_when_an_unnamed_agent_shares_the_file`,
`reconcile_sub_agent_refuses_when_both_targets_are_one_file`,
`an_undeterminable_protected_backing_refuses_the_removal`.

## Sibling rows sharing one backing

The shared-backing refusal tells the caller to add the sharer to `--remove`,
but that command then failed: the first row took the entry out of the shared
file and the second found nothing, so a reconcile that did exactly what was
asked reported `failed_count: 1` and exited 1.

Forgiving that turned out to need a credential, not just a shared backing:

- Copilot's project MCP path falls back to `<root>/.mcp.json` while neither
  that file nor `.github/mcp.json` exists, so `--remove claude --remove
copilot` against an absent file blessed both `ResourceNotFound`s:
  `success_count: 2` for removing something nobody had.
- The skill arm forgave every `ResourceNotFound` unconditionally, so removing
  from two agents that never held the skill exited 0 with the disk untouched.
- Skill credits keyed on write dirs missed the Master (which lives in no
  agent's write dir): an exhaustive removal's first row deleted the Master and
  every later row reported `RESOURCE_NOT_FOUND`.
- `--remove claude --remove claude` deleted once, credited the backing, and
  let row two be forgiven by row one — two successes for one deletion. Rows are
  now deduplicated before any exists.
- `RemovalOutcome::executed` is set for the whole execute branch even when
  every `remove_dir_all` failed with `EACCES`, so a row that left the Master on
  disk reported a deletion and vouched for its siblings. `failed_paths` is the
  truthful half; such a row is an `Err` that is never `ResourceNotFound`.

Rule: `RemovalCredits` resolves backings at preflight (a sub-agent's backing is
the file the first row deletes); only a row that really emptied its backing
credits later rows sharing it; one `sibling_already_took_it` for all three
delete arms.

Tests: `reconcile_mcp_credits_only_the_backing_a_row_emptied`,
`reconcile_mcp_does_not_bless_a_removal_nothing_ever_held`,
`reconcile_skill_does_not_bless_a_removal_nothing_ever_held`,
`reconcile_skill_forgives_the_row_whose_master_a_sibling_took`,
`reconcile_skill_failed_master_delete_credits_no_sibling`,
`transfer_duplicate_targets_are_deduplicated`.

Commit: 4a5efb25.

## Transfer skill pre-check refused genuine no-ops

`transfer_skill` used to guard with `get_skill().is_some()`, which refused the
two genuine no-ops — the target already reads the Master, or already holds a
valid link to it — while `reconcile --add` accepted exactly those. Same
operation, opposite verdict.

Rule: no pre-check; `add_skill_from_path` owns the already-present decision,
and a real foreign occupant is refused by `add_skill_from_path_universal`.

Test: `transfer_skill_already_present_is_an_idempotent_success`.

Commit: 899eb9f6.

## Holder scan reads skill dirs directly

`skill_holders` answers "will anyone still read the Master after this
reconcile?". It used to go through `load_all_agents`, whose full config load
also parses MCPs and sub-agents and gives up on the first error: one
unparseable `.mcp.json` erased an agent's skills from the answer and the Master
was collected out from under a real holder. Treating any load failure as "might
hold it" traded that for the opposite failure — an agent that cannot hold
skills vetoed every removal in the scope, with no override.

Rule: walk the skill read dirs directly; an existing-but-unlistable read dir
counts as a holder (fail closed, leaving at worst a reclaimable
`orphanMaster`); an absent read dir holds nothing (widening that makes every
uninstalled agent a holder and Master GC never happens again).

Tests: `a_broken_mcp_file_of_a_non_holder_does_not_block_master_collection`,
`reconcile_skill_keeps_the_master_when_a_holders_dir_cannot_be_listed`,
`reconcile_skill_will_not_gc_the_master_when_a_holder_is_unreadable`.

Commit: 1b373c2c.

## Naming an unreadable holder

Naming the unreadable agent in `--remove` flipped `exhaustive` true, and the
batch deleted the Master from a readable row while the unreadable agent's own
row was still ahead: its preflight fails open on a config it cannot load and
rows are attempt-all, so ordering saved nothing. The Master was gone and an
opaque copy or Referrer stayed behind.

Rule: refuse before any row runs while any holder is unreadable; fixing the
directory is the way through.

Test: `reconcile_skill_refuses_when_the_named_holder_is_the_unreadable_one`.

Commit: e91583dc.

## Earlier rows credited by position

`earlier_row_removals` used a `take_while` that, for a target absent from the
plan, credited the entire list — counting removals from rows that had not run.
That flipped `shared_master_kept` to false, the preflight green-lit the row,
and the commit refused it: the half-applied reconcile the preflight exists to
prevent.

Rule: credit by position; an absent target credits nothing.

Test: `earlier_rows_are_credited_by_position_not_by_scanning`.

Commit: d6151c23.

## Seventh spelling of slot sharing

`plan_reconcile_skill` ordered shared slots first with an inline reader count —
a seventh independent spelling of slot sharing. It counted the row's own agent
(where `classify::shared_with` excludes self) and matched read dirs by equality
(where the owner uses containment), so a Master under
`.agents/skills/<team>/<name>` counted zero readers.

Rule: ask `skill_dir_readers_outside` with an empty exclusion list.

Test: `reconcile_removes_shared_referrers_before_private_fallback_readers`.

Commit: d6151c23.

## Reconcile delete rows preserve request order

`plan_reconcile_skill` used to sort delete rows shared-first, causing output rows
to diverge from the caller's request order. Delete rows now retain request order
so per-target attribution directly maps back to caller input. Shared-first
execution is handled internally by `remove_skill_batch`.

Rule: preserve request order for delete rows in reconcile plans.

Test: `reconcile_skill_delete_results_follow_request_order`.

## Paired copy undoes the removal

Preflight runs every row before any copy, so it sees a disk where this
reconcile's own copies do not exist yet.

- "add windsurf, remove cursor" on a cursor-private skill passed preflight,
  wrote the Master, deleted cursor's folder, and reported both rows successful
  while cursor still saw the skill.
- Asking only whether the delete target read the Master was half the question.
  Amp and Kimi both read and write `~/.config/agents/skills` at global scope, so
  `--add amp --remove kimi -g` planned a copy whose Referrer slot was the entry
  Kimi's delete then unlinked; Amp, the agent being added, lost the skill.
- Deriving either half from `skill_store_roots` refused `reconcile --add claude
--remove amp -g` outright: that list includes the XDG dir no copy to a
  different agent writes.

Rule: `a_copy_restores_it` compares the dirs the copies leave a readable entry
in (`copy_entry_dirs`, via `agent_link_need`) with the delete target's
`get_skills_paths`, by containment.

Tests: `reconcile_skill_refuses_a_removal_the_paired_copy_would_undo`,
`a_copy_restores_it_asks_the_classifier_not_the_master_root_list`,
`a_copy_restores_it_sees_a_referrer_dir_the_copy_and_the_delete_share`,
`copy_collision_is_checked_when_delete_target_is_initially_absent`.

Commit: 1b373c2c.

## Batch refusal variant was flattened

`delete skills x -a cursor` and `reconcile skills x --remove cursor` raise the
same refusal, but reconcile's batch aggregation flattened it to
`InvalidConfig`: the API answered 400 for one and 422 for the other, and a
client branching on `UNSUPPORTED_OPERATION` saw the domain error in its "bad
parameters" arm.

Rule: when every row refused for the same domain reason, keep that variant.

Commit: 1b373c2c.

## Missing source blocked a removal-only reconcile

In the desktop app, after a skill was removed from Claude, subsequent removal
reconciles retained Claude as the source. `plan_reconcile_skill` called
`load_source_skill` first, which returned `ResourceNotFound` (HTTP 404),
blocking the entire batch before any holder scan or planning ran. The remaining
Referrers and the Master were left behind on disk.

When a reconcile is removal-only (`added` is empty), the source's content is not
needed for copies. If the source agent no longer holds the skill, the planner
falls back to loading the skill from the first agent in `removed` that still
holds it. If the reconcile includes additions or none of the removed agents
holds the skill, it refuses with `InvalidConfig` listing the agents that still
hold it, while keeping everything untouched. If no agent holds the skill at all,
`ResourceNotFound` is returned as before.

When only disabled agents still hold it, the refusal used to say "only disabled
agents still hold it … Refresh the list, or use one of them as the source"
without naming them — but the desktop list hides disabled agents, so neither
step was possible. It now names them as holders (never as readers; `keepers`
still leaves them out) and says what works: include them in the removal, or use
one as the source. Pinned by
`reconcile_skill_refusal_when_only_disabled_agents_hold_it`; the no-holder
removal case by `reconcile_skill_removal_naming_no_holder_refuses_and_names_holders`,
and Master retention through the fallback by
`reconcile_skill_fallback_keeps_master_while_an_unremoved_holder_remains`.

The effective source for protected target checks and deletion attribution
remains the caller's original `source`: fallback only substitutes `plan.skill`
and `plan.source_root`.

Per-row exhaustiveness (`row_exhaustive`): the plan's `exhaustive` flag says the
holder scan finished, which lets a removal prove it orphans nothing. That proof
only covers a row whose agent is actually in `holders`. A stale source that the
caller also lists in `removed` holds nothing, so it must not inherit the batch's
`exhaustive` — the row is `exhaustive && holders.contains(row agent)`. Without
it the stale row either blocks the whole batch or is treated as having removed
something. With it, that row fails alone ("not found") and the rows that do
hold the skill still succeed. The preflight, the dry-run and the commit all go
through `row_exhaustive`, so none can answer differently.

Pinned by `reconcile_skill_stale_source_in_removed_fails_only_its_own_row`.

Tests: `reconcile_skill_removes_remaining_holders_when_source_referrer_is_gone`,
`reconcile_skill_missing_source_with_adds_refuses_and_names_holders`,
`reconcile_skill_preview_allows_removal_when_source_is_gone`,
`reconcile_skill_stale_source_in_removed_fails_only_its_own_row`.
