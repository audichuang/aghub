# Delete, clean up and rename

The removal-side branches `SKILL.md` routes to. `delete` previews unless
`--yes`; `delete skills --help` restates the `kept` / `git rm` / `--all-agents`
rules in short.

## Contents

- [Revoke for one agent](#revoke-for-one-agent)
- [Delete refusals](#delete-refusals)
- [Reading the result](#reading-the-result)
- [Clean up leftovers](#clean-up-leftovers)
- [Accept an upstream rename](#accept-an-upstream-rename)

## Revoke for one agent

Delete that agent's Referrer (`delete skills <NAME> -a <AGENT>`, or `reconcile
skill --remove`) — but only when the agent has a PRIVATE dir. aghub will not
revoke a shared Referrer on one agent's behalf: the single-agent delete previews
`outcome: "kept"` (exit 0) and the `--yes` run then fails
`UNSUPPORTED_OPERATION` (exit 1). A shared slot goes only when every ENABLED
agent that reads it is in the `-a` list, and `coverage` does not list those
readers. To remove the skill everywhere: `delete skills <NAME> --all-agents`.

`enable` / `disable` take MCP servers only; they cannot hide a skill.

## Delete refusals

- **"Also read there by agents not in this request"** — a shared slot (link or
  real directory) still read by other ENABLED agents; disabled agents never
  block a single-agent or comma-list delete. Add the named agents to the same
  `-a` list, or use `--all-agents`. The same applies to a private real directory
  co-read because another agent's skills root is a dotfiles-style symlink to it.
  A symlink to it from an agent NOT in `-a` (a disabled agent's included) blocks
  too; that refusal lists paths, not agent names (e.g. `.claude/skills/<name>`
  when claude was left out). A link from an agent that IS in `-a` does not
  block: its own row unlinks it. `-a` order never changes the verdict, and
  preview and `--yes` agree. A refusal caused by npx-era or compat leftovers
  names no readers.
- **"Read only by disabled agent(s)"** — those agents were disabled after being
  granted the skill. aghub never sweeps a disabled agent's dirs unless that agent
  is NAMED (the `-a` target or list, or a `reconcile --remove` row; `-a`
  defaults to `claude`, so a disabled claude IS swept when you omit `-a`), and an
  unnamed one's link keeps the Master alive. Either `aghub-cli agents enable
<id>` (see `agents list`), unlink exactly the listed entries, or name the
  holders (`reconcile skill --remove a,b`, or the desktop delete dialog's "also
  remove these agents' links") — naming is the consent. Do not widen to `-a all`.
- **A real directory that git TRACKS in a shared slot** is refused (exit 1) by
  single-agent and `--all-agents` delete alike, as is one git cannot answer for
  (git missing, unusable repository, `dubious ownership`, or a probe over its
  10 s limit). The refusal prints the exact `git rm -r --cached <path>`; untrack
  deliberately, or keep authoring it there. `reconcile --remove` and the desktop
  dialogs apply the same rule.
- A real directory inside `.aghub` is never deleted by a single-agent delete.

A real directory has no Master behind it, so deleting it deletes the content and
its lock entry for good; `source sync` can only install a fresh copy of the
source's version. When no other Referrer remains, the Master goes too (paths
include `.aghub/<name>`), so re-enabling a disabled agent later does not bring
the skill back.

## Reading the result

Read `outcome` (`preview` | `removed` | `absent` | `partial` | `kept`), never
`dry_run` / `executed`. `kept` in a preview means the commit will not remove it;
`removed` does not prove the bytes are gone (a survivor is listed in
`skipped`). `--all-agents` asserts "gone everywhere", so a survivor makes the
confirmed run ERROR instead of `kept`. A Master listed under `kept (shared with
other agents)` is still read by another agent: never `rm` it.

JSON shape depends on the roster:

- **Single agent** (no comma list): one top-level object with `outcome`,
  `paths`, `skipped`, `type`, `name` and the prune fields — no `still_read_*`,
  no `master_reclaimed`.
- **`-a a,b` list**: an envelope whose `results[]` rows are `{agent, ok,
outcome, code, output, error}` in request order. `code` appears only on a
  row-level failure (`partial` or a config load error); a missing skill is a
  no-op `absent` with no code, exit 0. Everything else sits in
  `results[].output`: `still_read_from` (paths still reading it),
  `still_read_by` / `still_read_by_managed` / `still_read_by_unmanaged`,
  `master_reclaimed` (preview: `false`, plus `would_reclaim_master: true` when the
  run would remove the Master), and the prune fields. Filter
  `.results[].output.still_read_by`, not `.results[].still_read_by`. The text
  tally counts `kept` apart (`N ok, M kept (nothing removed), K failed`).
- **Whole-batch preflight rejection**: exit 1, nothing written, every refused
  target named; under `--json` it is the top-level `error.code`
  (`UNSUPPORTED_OPERATION`, `INVALID_CONFIG` or `MANAGED_RESOURCE`), not a row
  `code`.
- Prune status is independent: `would_prune_lock_entries` (preview),
  `pruned_lock_entries`, `prune_error`. A committed skill delete reconciles the
  whole scope lock against disk.

## Clean up leftovers

An `orphanMaster` is typically what an earlier `delete` left when it spared a
Master another agent still read. Plain `delete skills <NAME>` discovers skills
through the AGENT CONFIGS, so with no Referrer left it reports `absent` while
the Master stays. `delete skills <NAME> --all-agents` reaches it: the preview
lists `.aghub/<name>` in `paths`, `--yes` removes it (`removed`). Do not `rm` it
by hand, and check the other agents' rows first — an untracked Master can still
have a live `linked` Referrer. A stale LOCK entry is a separate job:
`prune-lock`.

## Accept an upstream rename

Confirm `source diff` reports `renamed` (`name` = new, `previousName` = old),
preview `source accept-rename`, then repeat with `--yes`; `--help` names which
argument is which. The new frontmatter name is resolved even when the repo
directory moved.

- The preview names no paths and no agents, and refuses without fetching a
  degenerate rename, an old name not in the lock, and a new name that already
  exists in the scope (`RENAME_TARGET_EXISTS`) — exit 1 before `--yes`. A locked
  old name with no installed copy (`NoInstalledCopy`) previews as success and
  fails only on `--yes`. Under `--json` the `error.code` matches the desktop/API.
- The commit grants the new name to every MANAGED agent that could READ the old
  one (shared-slot readers get their own Referrer, a disabled agent gets
  nothing); its `paths` can list a shared slot more than once, so dedupe before
  counting. It removes the old name everywhere, `.aghub/<old>` included — unless
  a disabled agent still holds a Referrer to it; then the old Master is kept and
  shows up `untracked` in `doctor` (enable the agent or unlink that entry, then
  `delete skills <OLD> --all-agents`).
- Rollback is BEST-EFFORT: `--help` says "rolls back on any failure", but the
  restore path discards its own errors. After any failed rename, inspect with
  `doctor --verify-links` rather than assuming the disk is back. Run it after a
  successful one too, as the completion check.
