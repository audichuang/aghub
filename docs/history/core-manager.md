# core-manager history

Incidents behind `crates/core/src/manager`: the ConfigManager skill mutations
and their attribution.

## Re-add reported requested metadata

`add_skill_universal`'s idempotent branches used to return a bare `Ok(())`.
`aghub add skills -n <existing>` then printed "added skill", serialized
`already_installed: false`, and echoed the REQUESTED `--description` /
`--version` / `--tools` on exit 0 while the Master on disk kept its old
content. Re-running `add` with corrected metadata is a standard scripted
repair, and it silently did nothing. The same defect existed in
`add_skill_from_path_universal`, where `add --from` printed the source file's
frontmatter while disk still held the old Master, and the from-path path did
not even warn that the Master was preserved.

Rule: every add returns a `SkillAdd` whose `skill` is what is ON DISK
(`master_on_disk`), with `already_installed` set on the no-op; an unparsable
preserved Master is an error, never a fallback to the caller's input.

Pinning tests: `add_skill_universal_idempotent_readd_is_noop`,
`add_skill_from_path_universal_does_not_overwrite_existing_canonical`.
Commit: b610db98.

## Skill mutations rewrote MCP config

`save()` / `save_current()` serialize MCPs and nothing else, but skill
add / update / remove / enable used to call `save_current()` anyway. That
cannot persist a skill, and as a side effect it rewrote the agent's MCP config
from the normalized model, stripping per-server fields aghub does not model
from `.mcp.json`. `set_skill_enabled` was the starkest form: flipping
`Skill::enabled` in memory was dropped on the floor while `.mcp.json` was
damaged.

Rule: skill mutations never call `save_current()`; `enable_skill` /
`disable_skill` refuse with `unsupported_operation` (not-found still wins, for
the API's 404).

Pinning test: `disable_skill_refuses_and_leaves_the_mcp_config_untouched`.
Commit: ccf20cc7.

## Update skill withholds the re-read

`update_skill` is the only guarded `ConfigManager` mutation that takes
`mutation_guard` without `guard_and_reload`. Re-reading config under the lock
regressed two rename tests on macOS ONLY ("the referrer must resolve to the
master" — the relink landed somewhere that does not resolve). The paths the
flow renames come from the reloaded entry, and on macOS a filesystem-resolved
path is `/private/var/...` where the caller's root is `/var/...`; something in
that pair breaks the relink. It could not be reproduced on Linux (a project
root that is itself a symlink still passes), so the re-read was withheld until
the macOS behaviour is understood rather than guessed at. The stale-view window
found in review therefore stays OPEN here while it is closed in add /
add-from-path / remove / remove-planned; see
`docs/specs/2026-07-29-skill-mutation-interprocess-lock.md`.

Commit: c62868e3.

## Remove skill ate a shared Master

The public `remove_skill` seam ends its `!is_link` branch in `remove_dir_all`,
and the entry it deletes comes from discovery. When agents read
`.agents/skills` directly, the shared Master was discovered with
`canonical_path = None`, so the seam deleted a shared Master and returned Ok.
A first fix checked only `dir_has_external_referrer`, which let a Master with
no symlinks but ~10 other direct readers through — silent data loss with no
dangling link left behind to notice it by.

Rule: before the copy-layout delete, `remove_skill` asks
`single_agent_keep_reason` — the same rule `plan_copy_removal` uses, shared
verbatim rather than restated — and refuses, pointing at
`remove_skill_planned`, the only layout-aware seam allowed to take a Master.

Pinning test: `remove_skill_refuses_master_another_agent_links_to`
(`manager::skill::tests`).

Later: `single_agent_keep_reason` now takes `scope` and the requested agents and
decides a real directory in a shared slot by its ENABLED readers (see
`core-removal.md#disabled-agent-blocked-a-single-agent-delete`). The planned
seam passes the batch's agents, so naming every enabled reader deletes; the
plain `remove_skill` seam passes none, so it stays strict and still refuses.
Commit: d7d1ea91.

## Nested broken link reached commit

`remove_skill_planned_inner` returns a preview (`kept`) when the planner's own
sweep finished with nothing to take and something in `skipped`. `blocks` cannot
always see that: `read_effect_after` stops at a directory whose root
`SKILL.md` parses (`collect_skills`) while the planner recurses into it
(`collect_entry_paths`), so a broken link NESTED inside an unrelated healthy
skill folder made the planner keep the Master with `effect.incomplete` false
and `survivors` empty. That fell through to `commit`, which reported `kept`
with `executed: true` AND ran a scope-wide lock GC the preview had promised
would not run (`RemovalOutcome::preview` gates its prune disclosure on that
flag) — dropping an unrelated skill's source provenance on a delete that
removed nothing. Earlier still, `commit` on such a run serialized
`outcome: "removed"` with the skill on disk.

Rule: `spared_everything || (all_agents && shared_master_kept &&
paths.is_empty())` returns `RemovalOutcome::preview(plan, true, …)` with
`shared_master_kept = true`, never `commit`.

Pinning test: `all_agents_keep_previews_instead_of_running_an_undisclosed_prune`.
Commit: 29116f38.

## Store checks skipped for a linked Master

In `skill_for_planned_removal`, the duplicate-Master refusal and the
fail-closed store read used to sit after the caller's own hit and a peer
agent's, so they ran only for a Master no agent linked. Link either of two
same-named Masters into any agent and `--all-agents --yes` deleted one of
them, reported `removed` and pruned the lock key while the other stayed on
disk.

Rule: the store scan runs BEFORE either early return. It fails closed on an
unreadable store entry (its frontmatter name is unknown, so it may be this
skill under another folder name) and refuses two Masters under one name rather
than picking by `read_dir` order. The first store with exactly one match wins,
so a GLOBAL duplicate cannot refuse a project removal.

Pinning tests: `linked_masters_do_not_bypass_exhaustive_store_validation`,
`two_masters_under_one_name_are_refused_not_picked_by_read_dir_order`,
`an_unreadable_store_entry_fails_closed_instead_of_guessing`.
Commit: 06d1f284.

## Add as name was import then rename

CLI `add --from <path> --name <new>` used to be import followed by an
`update_skill` rename. That released the mutation lock between the two halves
(another process could swap the Master out from under the rename) and stranded
the imported skill when the rename failed.

Rule: `add_skill_from_path_universal(path, as_name)` applies the name BEFORE
the duplicate check, so the install is one step in one lock span; an explicit
name never takes the idempotent no-op, and it refuses to rewrite a Master this
call did not create.

Pinning tests:
`add_skill_from_path_universal_refuses_to_rename_onto_a_foreign_master`,
`rename_import_refuses_when_the_target_name_is_taken` (`crates/cli/tests`).
Commit: d7d1ea91.

## Name based idempotence deleted the source

The from-path add once decided "already installed" by skill NAME. For an agent
holding an unrelated same-named skill it reported `already_installed`, and
paired with `reconcile --remove` — whose gate only asks whether the copy
ERRORED — the copy did nothing, the delete ran, and the content was gone.

Rule: idempotence is decided by the RESOLVED link target (the agent slot
canonicalizes to the Master), never by the name.

Commit: 75d2452d.

## Both scope load_failed was always false

`load_both_annotated` merges project and global and deliberately fails open —
a broken project config must not hide the global results — which is right for
a listing and wrong for a decision. Without a second return value
`AgentResources::load_failed` was a constant `false` in `Both` scope, so its
own contract ("callers that decide something must read this flag") could not
be honoured there, and the `load_failed: true` arm in `all_agents` was dead
code implying otherwise.

Rule: `load_both_annotated_checked` returns whether either scope failed to
load, and `Both`-scope callers that decide read it.

Commit: 355afb61.
