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
first, and preview and commit share one definition. For skills that seam is
`plan_reconcile_skill` itself — existence, disjointness and the shared-backing
spares check all live in it, so `reconcile_skill_preview` is one call
(`ensure_skill_exists` and `ensure_skill_reconcile_spares` were removed).

MCP and sub-agent reconcile once previewed through a CLI-side chain of public
guards (`ensure_mcp_exists` / `ensure_sub_agent_exists`, `ensure_disjoint`,
`ensure_*_reconcile_spares`), while the commit additionally refused unmanaged
source fields and, inside the batch preflight, a target that cannot hold the
resource (losslessly, when the source is removed). So `reconcile mcp --remove
codex --add cursor` on a Codex server with a timeout previewed clean and
`--yes` refused it. They now mirror the skill shape: `plan_reconcile_mcp` /
`plan_reconcile_sub_agent` plus the plan's per-row `preflight`, run by both the
commit and `reconcile_{mcp,sub_agent}_preview`; the old guards are gone.

Tests: `reconcile_rejects_empty_target_set_and_validates_source_in_preview`,
`reconcile_skill_preview_refuses_what_the_commit_refuses`,
`reconcile_mcp_preview_refuses_what_the_commit_refuses`,
`reconcile_sub_agent_preview_refuses_what_the_commit_refuses`,
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

The depth-bound fix then stopped at `depth >= MAX_DEPTH`, while the hash
refuses only `depth > MAX_DEPTH` — it still lists the directory AT the bound
and skips its symlinks silently. A symlink there was again certified `Landed`
and removed. The private walker had drifted from the hash twice, so it was
deleted: `skill::hash::compute_skill_folder_hash_covering_all` reports every
skip from the SAME walk that computes the digest (`HashError::Uncovered`).
Pinned by `round_five_delta_fixes_stay_closed` case (2) at `MAX_DEPTH`.

Rule: anything the hash cannot see answers `Unprovable`, never `Landed`, and
"what the hash cannot see" is answered by the hash's own traversal — never a
second walker.

Commits: 8267b920 (symlink), e84f4967 (depth bound), fae2ec88 (one walk).

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

Rule: no pre-check; `install_local_skill` owns the already-present decision
and refuses a real foreign occupant. (Written for `add_skill_from_path`, since
deleted — see
[below](#transfer-and-reconcile-copies-share-the-local-install).)

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

Test: `test_internal_accumulation_credits_later_row` (and `reconcile_skill_never_holder_fails_its_own_row_only`).

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

`transfer_mcp` was the last entry still outside `batch_preflight_error`: it
flattened every preflight refusal to `InvalidConfig` and dropped
`rejected_targets`. It now shares the batch shape; pinned by
`transfer_mcp_all_unsupported_targets_keep_unsupported_variant`.

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

## Reconcile delete rows mapped the verdict twice

The delete half of `reconcile_skill` used to answer the verdict in two places:
`preflight_delete` and the commit's Delete arm. The commit's removal was a lazy
memo filled by whichever delete row ran first. That row re-ran the post-copy
spare check for every row in order to choose the batch's agent list, and the
two mappings disagreed on a missing row. The commit arm's `_ => Ok(false)` also
hid which verdicts counted as success.

Rule: the runner's Delete arm only re-checks spares. `remove_skill_batch` runs
once, after the runner, for the cleared rows, and every row is attributed in
place through `delete_row_result`. A `Partial` verdict with no error still
counts as success; that is an open decision, because `to_batch_view` reports it
as `ok=false`.

Tests: `reconcile_skill_delete_rows_carry_outcome_and_surviving_holders`,
`reconcile_skill_first_delete_row_spared_out_still_removes_the_rest`,
`delete_row_result_maps_each_verdict`.

## Lowercase skill marker resolved to the file

"Where is this skill's folder" was spelled six times: `transfer::skill_root_unchecked`
and `removal::skill_root` (byte-identical, each documented as "the one"),
`skills::skill_source_root`, and the API by-path read route's `get_skill_root`.
Every copy stepped up from a file named exactly `SKILL.md`, while discovery's
marker rule (`enumerate::skill_marker`) and the parser also accept a root
`skill.md`. Discovered skills were never affected, because `convert_skill` and
enumerate record `<dir>/SKILL.md` whatever the real marker is. A path the user
names was affected: `install_local_skill` on `<dir>/skill.md` took the FILE as
the source root and failed hashing it with "Not a directory".

Rule: `enumerate::SKILL_MARKERS` is the one marker list. `skill_source_root`
reads it, and `removal::skill_root` is the only model→root resolver, which
transfer calls. A missing path resolves lexically
(`skill_root_returns_nonexistent_dir_as_is` pins that).

Follow-up (review): stepping up from any `skill.md` name made the resolver
non-idempotent on a skill FOLDER literally named `skill.md` (`sanitize_name`
keeps `.`, so the name is reachable). Callers that already hold a root —
`install_local` on a folder, transfer re-resolving `plan.source_root` inside
the copy install and `prove_content_landed`, `sources.rs` hashing scanned
dirs — then copied or hashed the PARENT, and a reconcile move could delete the
real source after a "proven" copy of the wrong tree. `skill_source_root` now
steps up only when the path is not an existing directory. Residual: a MISSING
folder named `skill.md` still resolves to its parent, the same exposure
`SKILL.md` always had.

Tests: `install_local_skill_from_lowercase_skill_md_installs_its_folder`,
`skill_source_root_resolves_lowercase_skill_md_to_parent`,
`skill_source_root_keeps_directory_named_like_a_marker`,
`install_local_skill_from_folder_named_skill_md_keeps_that_folder`.

## Transfer and reconcile copies share the local install

`transfer skill` and `reconcile skill --add` copied through
`ConfigManager::add_skill_from_path`, a second local install beside
`install_local_skill` (`add --from`, the API import). It wrote no lock entry,
ran no adoption guard, rolled back differently, and silently REUSED a
pre-existing same-named Master whose content differed. Nobody chose that
difference. A transferred skill was invisible to `check` / `apply-update`, and
a plain transfer could grant a target stale content without a word
(`prove_content_landed` guards only a reconcile that also removes).

Rule: every skill copy goes through `install_local_skill`, one agent per call
(so batch rows keep their own outcome), and `add_skill_from_path{,_universal}`
are deleted. The trap in a naive reroute: that install records the local
path as the owner, and the destination lock already names `github:…` for the
common same-scope grant. Under a root spelled through a symlink (macOS `/var`)
the source path (discovery canonicalizes a Referrer to its Master) no longer
equals the Master's spelling, so the guard's regrant shortcut misses and the
grant is refused "already owned". So the copy says what to do with the lock
(`LocalSkillInstallRequest::lock`, `LocalInstallLock`):

- Same scope (one lock file; project roots compared by resolved location):
  `Untouched`, a pure regrant. The owner is neither compared nor written.
  Carrying the entry and comparing it with itself was review round 1's fix,
  and it refused an npx `gitlab`/`git` project entry (no `sourceUrl`;
  `same_source_owner` accepts that only for github/local), and it stamped an
  untracked Master with a `local:<root>/.aghub/<name>` entry pointing at
  itself. The Master content and link-free guards still run.
- Across scopes: `Carry` the source entry — `source`, type, ref, `skillPath`,
  `refCommit` and its STORED Source hash. Re-hashing the copied content wrote
  a locally edited source's hash as the baseline, so `check` took the
  fast path and called the copy current while the source scope said
  modified. The hash is recomputed only when none is stored (empty or
  placeholder).
  The source entry is read while BOTH scopes' mutation locks are held, taken
  in one ordered acquire (`skills::lock::write_scopes_guard`): which scope
  orders first depends on the paths, so taking them one by one inverts in one
  direction. Pinned across processes by
  `a_cross_scope_transfer_reads_the_source_entry_under_its_lock`
  (`tests/mutation_lock_flows.rs`); the in-process
  `transfer_skill_cross_scope_locks_both_scopes_in_either_direction` pins only
  that neither direction trips the inversion refusal.
- Across scopes with no source entry: `Local`, recorded at the source root
  (for a Referrer, the Master discovery resolved it to).

Project → global used to carry `sourceUrl: ""` (project locks omit
reconstructable URLs). A blank global URL made `EntryIdentity::of_global_entry`
unresolvable, so `describes` accepted ANY repository and the API's git-sync
could install repo B's bytes under transferred skill A; accept-rename fetched a
hostless gitlab `group/repo` as GitHub. Now one provider-aware rule,
`skills::source_coordinate::entry_clone_source` (moved from skill-update), is
used by the carried GLOBAL write when the rebuild is certain
(an ambiguous owner is never carried, see below; github is byte-identical to a
fetched install), by `of_global_entry` (legacy blank entries already on disk) and by
rename's global arm — and, since a URL-less legacy gitlab PROJECT entry had
the same GitHub misread, by `of_project_entry` and rename's project arm too
(capture and verify both use it, so a github project entry still compares
equal to itself).

Those readers then rebuilt a host for AMBIGUOUS entries too: a URL-less
`gitlab:group/sub/repo` (self-hosted under npx 1.5.11) made accept-rename
fetch `gitlab.com/group/sub/repo` — an unrelated repo that, holding the new
name, would replace the skill — and a valid git-sync from the real
`gitlab.corp/...` host was refused `SKILL_SOURCE_MISMATCH`. Rule: the readers
use `source_coordinate::known_clone_source` (the one "is the host known" home,
also behind transfer's carry decision): recorded URL, else a rebuild only when
unambiguous, else AMBIGUOUS — `EntryIdentity` keeps the raw recorded value
(global: the blank URL; project: `source`) and accept-rename refuses before
fetching. A two-segment URL-less `gitlab:o/r` is ambiguous too, so the earlier
rename tests flipped to the refusal. Tests:
`project_rename_source_refuses_an_ambiguous_url_less_entry`,
`global_rename_source_rebuilds_only_a_provable_blank_url`,
`rename_refuses_an_ambiguous_url_less_gitlab_entry_before_the_fetch`
(skill-update),
`git_sync_permits_its_own_self_hosted_host_for_an_ambiguous_global_entry`
(api).

A URL-less `gitlab` entry does NOT name gitlab.com. npx `skills` 1.5.11
records ANY non-github `/-/tree/` host as `type: "gitlab"`, strips the host to
`group/repo` and writes no `sourceUrl` (newer npx, e.g. 1.5.22, does write
`sourceUrl` for git/gitlab, but old entries stay on disk). One round rebuilt
both sides in `same_source_owner` through `entry_clone_source`; that accepted
a gitlab.com claimant for a self-hosted entry (any registrant of the same
`group/repo` on gitlab.com could take it over) and conflated a namespace
literally named `gitlab.com` (`gitlab.com/team/repo`) with `team/repo`.

A narrower follow-up accepted two URL-less entries with the same spelling as
one owner (for a retry or a second agent's grant). That still merged two
self-hosted hosts' `gitlab:group/repo`, and carrying such an owner to GLOBAL
with a blank URL let `prepare_locked_resync` and `EntryIdentity` both rebuild
gitlab.com and agree — `apply-update` would replace the content from a guessed
host (resync never asks `same_source_owner`).

Rule: a cross-scope copy carries the owner ONLY when its host is known — a
recorded URL, or `source_coordinate::reconstruction_is_unambiguous` (github,
local, or a source that spells its own host). An ambiguous legacy owner is
copied UNTRACKED (`LocalInstallLock::Untouched`: no lock entry, no owner
comparison, Master guards still run), which is what transfer did before this
range for every copy: `doctor` shows it untracked, and re-installing from the
real URL tracks it. `same_source_owner` is back to its original rules and
never rebuilds a coordinate: a URL-less non-github entry fails closed, the
same spelling included. Tests:
`transfer_skill_cross_project_ambiguous_owner_is_copied_untracked`,
`transfer_skill_ambiguous_owner_to_global_is_copied_untracked`,
`a_url_less_gitlab_entry_refuses_a_full_gitlab_com_url`,
`url_less_gitlab_entries_with_one_spelling_are_not_one_owner`,
`a_dotted_gitlab_namespace_is_not_its_host_stripped_twin`,
`a_url_less_gitlab_entry_refuses_another_repo`,
`a_url_less_custom_remote_still_fails_closed`.

Behaviour change: a destination Master with different content and no lock
owner is now REFUSED ("different content") instead of reused — by `transfer`
and by `reconcile --add` alike. A copy onto an OWNED Master still grants it
as-is: the hash check runs only for an untracked Master. A carried remote
owner once made `adoption_guard` treat the copy as a FETCHED install and
byte-compare every existing Master; the copier drops `__pycache__` while the
raw hash counts it, so every second agent and every retry of a Python skill
was refused. The guard now takes `fetched` from its caller (only a remote
checkout is fetched); pinned by
`transfer_skill_carried_owner_with_python_cache_can_be_retried_and_regranted`.

Tests: `transfer_skill_same_scope_regrant_keeps_the_lock_owner`,
`transfer_skill_same_scope_leaves_a_url_less_remote_owner_alone`,
`transfer_skill_same_scope_keeps_an_untracked_master_untracked`,
`transfer_skill_cross_scope_carries_the_stored_hash_of_an_edited_source`,
`transfer_skill_cross_scope_untracked_records_the_master_not_the_referrer`,
`transfer_skill_cross_scope_locks_both_scopes_in_either_direction`,
`global_rename_source_rebuilds_only_a_provable_blank_url`,
`a_blank_global_url_still_refuses_a_different_repo`,
`project_rename_source_refuses_an_ambiguous_url_less_entry`,
`git_sync_refuses_a_different_repo_for_a_blank_url_global_entry`; the reuse pin
`add_skill_from_path_universal_does_not_overwrite_existing_canonical` was
deleted (the refusal is pinned by
`install_local_skill_refuses_to_adopt_untracked_master_when_content_differs`).
