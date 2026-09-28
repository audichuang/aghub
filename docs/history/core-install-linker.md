# core-install-linker history

Incident history moved out of `crates/core` code comments. The code keeps the
current rule; each entry here keeps what happened and why.

## Doctor listed the quarantine as a skill

`repair` keeps forks aside under `.aghub/.quarantine/<name>/<stamp>/`.
`top_level_skill_dirs` never saw it (one level deep, needs a root `SKILL.md`),
but `doctor` enumerated the store directly and listed `.quarantine` as an
`invalid-skill`, which permanently reddened `--fail-on-issues` for anybody who
had ever migrated. Rule: every enumerator of the store applies
`is_store_bookkeeping` (dot-prefixed entries are aghub's own).

- Commit: 623acace

## Native reader classification removed

Before the Master moved into the `.aghub` store, some agents read the Master
directly, and `classify` had a `LinkNeed::NativeReader` variant, plus
`reads_master` / `writes_master` booleans on `AgentLinkPlan` and a
`master_skills_dir` parameter on `agent_link_need`. Once no agent reads the
store, all of it was structurally constant:

- The variant was deleted rather than left unreachable: a variant still
  constructed but never produced draws no dead-code warning and silently kills
  every `matches!` arm testing for it.
- The booleans would have shipped hard-coded `false` to the UI dressed as facts;
  `shared_with` replaced them (who else a grant reaches).
- The parameter would have let a caller pass the OLD master path and resurrect
  the deleted behaviour.

Before `shared_with` existed, install attribution, doctor rows, `transfer`'s
protect set and the desktop checkbox group each keyed on the Referrer path and
conflated its sharers.

- Pinning test: `the_former_native_readers_now_get_their_own_referrer_dirs`
  (`skills/linker/classify.rs`)

## Delete preview hid the lock prune

A committed `delete` reconciles the WHOLE scope's lock against disk, so it also
drops entries for OTHER skills that are already gone. The preview did not
disclose that: `pruned_lock_entries` appeared only on the committed payload,
while `prune-lock`, which performs the same GC, gates it behind its own `--yes`.
`preview_prune_for_removal` was added, excluding the paths the delete will take
(still present before the delete, so a plain `preview_prune` would omit the
target's own key).

The first cut compared excluded paths as raw strings and named "macOS
/private/var, a symlinked HOME" as a hypothetical upgrade path. It was not
hypothetical: on a macOS runner the plan carried the Master as
`/private/var/...` and the agent dirs as `/var/...`, they never matched, and an
`--all-agents` preview omitted the key it was certain to drop. Fix: compare
through `normalize_ancestors` (never a full `canonicalize`, which would follow a
Referrer to its Master and prune a live key).

- Commits: 04b6bb70 (preview disclosure), 4fdef951 (ancestor comparison)
- Pinning tests: `remove_skill_planned_dry_run_discloses_prune_without_writing`,
  `preview_of_a_shadowing_copy_removal_discloses_the_lock_prune`

## Prune preview trusted an unreadable lock

`locked_keys` reads the lock through the fail-OPEN readers, so an unreadable
lock yielded an empty key set, and an empty `would_prune_lock_entries` means
"the scan ran and found no orphans" — a clean bill of health from a scan that
saw nothing, while `--yes` on the same file (fail-CLOSED modify seam) refused
outright. Rule: previews read the lock through `locked_keys_checked` and
degrade to `NotRun` / `UnreadableLock`.

- Commit: 75d2452d

## Prune scan missed the Master store

After the Master moved into `.aghub` (which no agent reads), the prune disk set
was still the union of agent dirs only, which hold nothing but Referrers; and
`top_level_skill_dirs` tested `is_dir()`, which is false for a symlink. Together
every installed skill read as an orphan, and since `delete --yes` prunes its
scope's lock as a side effect, the first delete after upgrading would have wiped
the lock's provenance. Rule: `scope_skill_dirs` always includes the store, and
the scan accepts symlinks (the `SKILL.md` probe follows them, so a dangling one
still fails).

- Commit: 3ef017e5

## Install attribution vs rollback receipts

Once `LinkNeed::NativeReader` was deleted, every agent reading the shared
`.agents/skills` slot at project scope became a Referrer of the SAME directory:
one reported `Linked` and the rest `AlreadyLinked`. Reporting those as
`installed: false, error: None` was a first-install failure with no error
attached, so `installed` started folding in `already_linked`. That split two
questions that used to share one answer:

- Attribution ("can this agent read it") — `installed`, includes
  `already_linked`.
- Rollback / lock-write signal ("did THIS call create it") —
  `created_referrer_dirs` from the linker's own `linked` set, never
  `already_linked`: rolling back a link this call did not create would remove a
  grant that was already there. Keying the lock write on readability instead
  made an idempotent re-run rewrite the lock.

The shared slot's dirs are also deduplicated before linking, or the sharers
report `AlreadyLinked` against work the same call just did.

- Commits: 005b1783 (creation receipts), 2d94db50 (lock-write signal)
