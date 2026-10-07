# cli history

Incidents behind the rules in `crates/cli`: command defaults, scope resolution
and output contracts.

## check scope defaults to both

`check` used to follow the plain global default. Run inside a project, it
answered "up to date" from the global lock alone without ever reading the
project's. It now defaults to BOTH scopes like the other read-only diagnostics
(`doctor`, `source list`, `source diff`). Commit: 7b2ee2d4 (help text
corrected in 4538016a). Pinned by: `check_defaults_to_both_scopes`.

## narrowed resource args

`check` and `apply-update` shared the full `ResourceType`, so clap advertised
`[possible values: skills, mcps]` and their long help never said otherwise; the
runtime then bailed with bare prose on stderr and an EMPTY stdout, even under
`--json`. An agent enumerating the surface from `[possible values]` built
`check mcps`. They now take `SkillResource`, so the rejection is a precise
parse error.

`enable`/`disable skills` was a dead command: `set_skill_enabled` had no
success branch for any agent, deliberately — `save()` serializes MCPs only, so
flipping `Skill::enabled` would silently rewrite `.mcp.json` and strip fields
aghub does not model (core calls that "worse than an honest refusal"). But clap
still advertised `skills`, so the only way to learn no agent supported it was
to enumerate agent ids by hand. They now take `McpResource`. Commit: b610db98.

## stderr logger

Nothing in the workspace installed a `log` logger outside tests, so every
`log::warn!` in `aghub-core` / `aghub-skill` / `aghub-git` went to the no-op
logger. Both lock read paths (`skill::lock::io`, `skill::lock::local`) fail
OPEN on an unparseable lock and announce it only through `log::warn!`, so a
corrupt `skills-lock.json` read as "no skills installed" with nothing on either
stream to contradict it. `StderrLogger` in `main.rs` fixes that. Commit:
b610db98.

## json failure envelope

A `--json` caller used to get nothing on stdout and one line of English on
stderr, and every runtime failure — a policy refusal (`apply-update` without
`--yes`), a missing resource, an invalid agent id, a rejected scope
combination, a genuine failed write — was exit 1. The only way to tell them
apart was matching prose, which was neither stable nor consistent: the same
missing resource read `Skill 'x' not found` from `describe` and `Resource not
found: skill 'x'` from `disable`. `report_failure` now prints
`{"error":{code,message,retryable}}` on stdout with `code` from
`aghub_core::error_codes`.

A wrapped failure also used `to_string()`, which returns only the outermost
context: the message read "Failed to load config" with the real cause (file,
parse error, line) stranded in the stderr-only `Caused by:` block. The message
now uses `{:#}`. Commit: b610db98.

## agent id validated up front

`-a` validity used to be checked only after eight early returns, so `-a bogus`
exited 1 on `get`/`check`/`prune-lock`/`delete` but exited 0, silently ignoring
the typo, on `coverage`/`doctor`/`source list`/`skill-usage`. `doctor` and
`doctor --verify-links` — the same subcommand — disagreed about the same bad
id. No cheap read-only probe could check a composed id; the wall came later,
mid-write. `run()` now validates once, before every early dispatch. Commit:
b610db98. Pinned by: `invalid_agent_id_fails_consistently_across_commands`.

## source sync examples

The `source sync` examples used to sit in the `///` doc comment. clap re-wraps
doc paragraphs and joined the two example lines into ONE unrunnable command
(`… --yes aghub-cli -p source sync …`) — the only worked example in the CLI, on
the install entry point. They now live in `SYNC_EXAMPLES`, emitted verbatim via
`after_long_help`. Commit: b610db98. Pinned by: `source_sync_help_renders`.

## doctor fail-on-issues claims

`--fail-on-issues` is opt-in so existing CI users of `doctor` are unaffected;
without it `doctor --verify-links && echo healthy` printed healthy over a
dangling referrer because findings only went to stderr. `untracked` is not
counted because failing CI over a hand-placed skill would only teach users to
append `|| true`.

Its help once claimed a linked master (`master-is-symlink`) makes aghub
"refuse to repair, relink or delete" and that "`source sync` will not act on it
either". Neither held: `verify_shape`'s blockers are a shared `ForkedCopy` and
an `AliasedMaster`, not the master-side violations, so the delete guard never
fires on a linked master; and `verify_shape` is called only from `removal.rs`,
so the resync path has no shape gate — what it does to a linked master is
unpinned, not refused. Only `repair` is pinned (`plan_repair` ->
`Refuse { MasterIsLink }`).

## diff accepts online

`source diff --online` used to be a clap exit 2 whose "to pass '--online' as a
value, use '-- --online'" tip read like a quoting problem. Since `check
--online` exists, callers reasonably tried the same flag, so `diff` accepts it
as a hidden no-op. Pinned by: `agent_facing_message_and_flag_fixes`.

## malformed config is not missing

`run_for_agent` used to decide "tolerate a load failure" on the command alone,
so a config that EXISTS but does not parse was tolerated exactly like an absent
one. `delete --yes` then took `config().is_none()` as "already gone" and
reported `{success:true, executed:false}` on exit 0 while the entry stayed in
the file — a silent failed removal. The error kind now decides: only NotFound
is tolerated. The same failure used to be wrapped with `anyhow!("… {}", e)`,
which stringified the `ConfigError` and degraded the `--json` code to
`CLI_ERROR` instead of `JSON_PARSE_ERROR`.

Tightening that made a broken `.mcp.json` fail `check skills` and
`prune-lock`, which never read agent config — so both now dispatch before any
adapter is built. Pinned by:
`malformed_agent_config_fails_delete_instead_of_reporting_success`.

## all agents message

`handle_all_agents` used to reject non-`get` commands with "supports only
'get'", contradicting both the `-a` help (`all` also works with `doctor
--verify-links` and `source sync`) and the behaviour; its suggested remedy (a
comma-separated list) was wrong too — lists are REJECTED by check, describe,
coverage, prune-lock and apply-update. Pinned by:
`agent_facing_message_and_flag_fixes`.

## add from with name

`add skills --from <path> --name <n>` used to import, then rename with
`update_skill`. That released the mutation lock between the halves and
stranded the imported skill whenever the rename failed. The install now writes
the requested name directly, and a conflicting name is an error refused before
anything is written (which also retired a `&& !renamed` correction on
`already_installed`).

The post-add "already covered" note was replaced by the shared-slot note: it
described the leak as a feature — agents got the skill because storing it
granted it, with no opt-out. Now they get it only when their slot is written,
and the agents sharing that slot are named.

## manual add reports disk

The manual (`--name`, no `--from`) branch used to build its view from the
request and hard-code `already_installed: false`, on a comment claiming a
manual add always errors on a duplicate. It does not: two of
`add_skill_universal`'s branches are idempotent no-ops, so a re-add with a
changed `--description` printed "added skill", echoed the NEW description, and
left the Master alone. It now serializes what the manager reports on disk.

## lock snapshot fails closed

The lock read paths fail OPEN to an empty lock, deliberately, so one corrupt
file does not break every query. But `check` and `doctor` answered `[]` on exit
0 with an empty stderr for a `skills-lock.json` full of entries they could not
parse, and `doctor` went on to classify the still-present skills `untracked`
and recommend deleting them. A first fix was a predicate-only probe, which left
each command to read the file a second time through the fail-open reader; a
non-aghub writer (an editor, `npx skills`) truncating it between the two reads
put the empty answer back. `LockSnapshot` now reads once and is consumed
directly. Commit: 7b2ee2d4. Pinned by:
`unreadable_lock_fails_the_commands_that_report_it`.

## doctor link audit

`doctor --verify-links` used to derive each referrer's verdict itself
(`symlink_metadata` -> `is_link` -> two `canonicalize`s -> compare). A two-hop
chain resolves to the Master, so endpoint equality certified it `Linked` while
`plan_repair` scheduled a `Relink` for the same directory — doctor said clean,
repair said fix. The verdict now comes from `classify_shape`.

Other shapes of the same drift, each now closed:

- `LinkAudit` reported `verified` while its own rows said `missing`.
- One blanket note offered `source sync --install-missing` for every issue;
  when `chain` and `master-unusable` arrived they inherited it though it fixes
  neither (a chain's endpoint IS the Master, so linking reports
  `AlreadyLinked`). Notes are now one per `Remedy`.
- Orphan masters (what `delete --yes` keeps when another agent still reads it)
  fell in that bucket too, so doctor told the caller to reinstall what they had
  just removed while another note told them to delete it.
- The reinstall note was not runnable: `source sync` needs a `<SOURCE>`
  positional and `--yes`.
- The remedy's scope flag came from the command (default: both scopes), so a
  project-only fault printed `-g`.
- `--fail-on-issues` was derived from `link_audit` alone, so without
  `--verify-links` it exited 0 over an `invalid-skill`; and its failure message
  always named the link audit, even when it had not run.
- `master-is-symlink` was excused as "a SUPPORTED layout, as the NativeReader
  branch says" — a branch since deleted, and a claim core contradicts
  (`Violation(MasterIsLink)`, refused by `plan_repair`). Excusing it while the
  link audit called it an issue had doctor's two columns answer one fact both
  ways.
- A dangling master symlink downgraded an absent slot out of `missing` because
  `master_state` uses `symlink_metadata`; the check now uses `exists()`.
- `.quarantine` (from `repair`) listed as an `invalid-skill` row forever.
- `AgentLinkState` docs kept saying `autoCovered` / "reads the master
  directly" long after that state was renamed `withheld` and the concept
  deleted.

Pinned by: `doctor_points_a_chain_at_repair_not_at_sync`,
`doctor_master_is_symlink_fails_both_axes`,
`doctor_separates_orphan_masters_and_can_gate_on_issues`,
`doctor_linked_master_prints_one_remedy_not_two`,
`second_review_found_gaps_stay_fixed`,
`third_review_sibling_shapes_stay_fixed`.

## source sync yes without action

`source sync <repo> --yes` — the most natural spelling of "install this repo's
skills" — used to fall through to the no-action overview: exit 0, no `dryRun`
key at all, and a third payload shape, so a consumer keyed on
`dryRun == false` (missing is falsy) concluded the install had been applied.
It is now refused before the fetch, so it costs no network round trip and
reports the real problem instead of a credential/network error.

## source command consolidation

`commands/source.rs` used to carry four private scope resolvers and three
hand-copied "no project root found" sentences (five wordings CLI-wide), and
re-assembled the fetch/classify sequence separately in `diff` and `sync` with
branches copied word for word — so the two worded the same refusal two ways.
Scope now comes from `main`'s one resolver, the sequence from
`skill_update::sources`, and refusal wording from `refusal_error`. The source
redactor was also once a private copy here, which is why the update-check log
later grew the same credential-leak hole; it now lives only in
`aghub_git::redact_source_credentials`.

A host-blind `owner/repo` spanning two scopes that resolve to different forges
used to be refused as ambiguous; each scope is now judged against its own
recorded origin, and `DiffScopeView.origin` names it.

## accept rename preview

`source accept-rename`'s degenerate-name guard and lock read used to sit AFTER
the dry-run return, so the preview green-lit renaming a name not in the lock
(or `a -> a`) and the caller hit the wall only on `--yes`. The preview also
`println!`ed prose even under `--json` on exit 0 — a strict parser read a crash
on the success path, a lenient one read "the rename was committed". Pinned
by: `accept_rename_preview_validates_lock_and_honours_json`.

## reconcile without targets

`reconcile` with neither `--add` nor `--remove` used to fall through to
`run(source, [], [], false)`: an empty batch rendered as
`{"success_count":0,"failed_count":0,"results":[]}` on exit 0,
indistinguishable from a real copy for anything keyed on the exit code.
`--agent` is where a caller lands first — clap's "a similar argument exists:
'--agent'" tip for a mistyped `--agents` points there, and the usage line
never mentions `--add`. It is now a usage error.

Its preview also skipped the one check that prevents data loss (no removal
from a copy target or the source), so it green-lit plans `--yes` then refused.
`transfer::install_scope` was once a private resolver re-reading
`cli.global`/`cli.project`. Pinned by:
`reconcile_rejects_empty_target_set_and_validates_source_in_preview`.

## partial removal exits nonzero

A removal payload with `success: false` (`RemovalKind::Partial`: the removal
ran and at least one path could not be deleted) used to exit 0. `delete --yes`
on a read-only directory exited 0 with the skill untouched. It now bails after
printing the report, and suppresses the failure renderer's second document.
Commit: 994e2e6d. Pinned by:
`a_delete_that_removed_nothing_does_not_report_success`.

## all agents sweep can keep all

Since 5437da3c a `delete --all-agents` sweep can finish having taken NOTHING:
a commit whose `blocks` is true still refuses, but the planner's own keep does
not, and reports `kept` with `--yes` given. The single-agent `kept` message
("re-run with --all-agents") is a dead end there, because `--all-agents` is
what just ran, so the renderer shows the skipped list instead.

## unstatable referrer test scope

`a_referrer_we_cannot_stat_still_counts_as_a_referrer` covers the COPY layout
only (`canonical_path` is None, so `plan_symlink_removal` is never entered).
An earlier version of its comment claimed it covered the symlink sweep too,
and reverting the symlink fix left every test there green; the symlink sweep
has its own pair: the two arms of
`an_agent_dir_we_cannot_stat_is_not_one_that_holds_nothing`.

## delete skills -a order independence

`aghub-cli delete skills <name> -a <list>` used to dispatch through
`batch::run_skill_agent_mutation`, executing row-by-row in argv order with
only per-agent capability preflight. When private readers preceded shared-slot
writers in `-a`, the private readers saw the shared slot still populated and kept
their links, leaving partial removals (issue #21).

CLI `delete skills -a` now routes directly through `skills::removal::remove_skill_batch`,
the same shared core entry used by `reconcile skill --remove` (API follows in A5). Whole-batch
preflight checks all targets atomically (exiting 1 with nothing written on refusal),
internal shared-first execution handles prior-row credit regardless of `-a` order,
and results are projected in original request order. `--json` rows carry unified
`outcome`, wire `code`, and attribution fields (`still_read_from`, `still_read_by`,
`master_reclaimed`).

Rule: CLI skill deletions delegate to `remove_skill_batch`; `-a` order does not affect the verdict.

Tests: `test_cli_delete_skills_agent_order_independence`,
`test_cli_delete_skills_whole_batch_preflight_rejection`,
`test_cli_delete_skills_absent_member_exit_zero_and_preview_commit_verdict_parity`,
`test_cli_delete_skills_non_exhaustive_lock_only_absent_member_exit_zero_parity`,
core `test_master_reclaimed_preview_commit_and_surviving_master`.
