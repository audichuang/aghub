# `aghub-cli` state semantics

JSON shapes, the full value domains, the adoption guard, and the flags whose
names mislead. Applies to `aghub-cli >= 2.18.0`. The call sites in `SKILL.md`
say when to come here.

## Contents

- [`doctor --json`](#doctor---json) — health, master, and per-agent link states
- [`source diff --json`](#source-diff---json) — scope-nested; the states
- [`repair --json`](#repair---json) — shapes found vs actions taken
- [Git source versus local content](#git-source-versus-local-content) — the adoption guard
- [Commands whose names mislead](#commands-whose-names-mislead)
- [Sub-agents](#sub-agents) — single-agent mutations, `outcome` vocabulary
- [Rename and interoperability edges](#rename-and-interoperability-edges)

## `doctor --json`

One row per installed skill, per scope. The same word means different things in
different fields — always read the field name. `missing` in `master` is "no
Master directory"; `missing` in `linkAudit.agents[].state` is "no Referrer AND
no Master to point at".

Link states only carry values when `--verify-links` is passed.

`master` is a coarse label, not a filesystem proof: ANY `symlink_metadata`
error becomes `missing` (a permissions or I/O failure included), and any
non-link occupant becomes `dir` (a regular file included). So an unreadable
Master can present as `orphan-lock` and invite a prune that is not the fix —
stat the path yourself before acting on that pair.

| Field             | Value                      | Meaning                                                                                                                 | Next action                                                                                                                                                                                                                                                                                                                                                                                                 |
| ----------------- | -------------------------- | ----------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `master`          | `dir` / `link` / `missing` | Master slot shape on disk                                                                                               | `link` and `missing` need a health row below                                                                                                                                                                                                                                                                                                                                                                |
| `health`          | `ok`                       | Valid tracked Master                                                                                                    | Verify the roster links                                                                                                                                                                                                                                                                                                                                                                                     |
| `health`          | `invalid-skill`            | `SKILL.md` missing/invalid, or its name disagrees                                                                       | Inspect or restore content                                                                                                                                                                                                                                                                                                                                                                                  |
| `health`          | `orphan-lock`              | Lock entry with no Master on disk                                                                                       | Un-migrated copy in the SHARED slot → `repair`; content only in a private dir → `repair` can do nothing, and `source sync` will build the Master but refuse to take over that occupied slot (`installed: false`), so move the directory aside first; nothing on disk → restore with a grant while the diff row is still `installedCurrent`, otherwise `prune-lock` and reinstall (see SKILL.md's dead ends) |
| `health`          | `untracked`                | Master with no lock entry                                                                                               | Local branch, or adopt/replace                                                                                                                                                                                                                                                                                                                                                                              |
| `health`          | `master-is-symlink`        | The Master slot is itself a link                                                                                        | Inspect before mutating                                                                                                                                                                                                                                                                                                                                                                                     |
| `linkAudit.state` | `notRequested`             | `--verify-links` was omitted                                                                                            | Re-run with it                                                                                                                                                                                                                                                                                                                                                                                              |
| `linkAudit.state` | `verified`                 | Every row is `linked`, `withheld` or `unsupported` — NOT proof a requested agent is covered                             | —                                                                                                                                                                                                                                                                                                                                                                                                           |
| `linkAudit.state` | `issues`                   | At least one row is not                                                                                                 | Read the rows                                                                                                                                                                                                                                                                                                                                                                                               |
| `agents[].state`  | `linked`                   | Referrer resolves to this Master                                                                                        | The only pass for a requested agent                                                                                                                                                                                                                                                                                                                                                                         |
| `agents[].state`  | `withheld`                 | Master healthy and no Referrer in this agent's write dir OR any dir only it reads                                       | Installed but NOT granted. Fine if unrequested; a gap if requested. A Referrer in a private read-only dir reads `linked` instead; the SHARED slot does not count as one                                                                                                                                                                                                                                     |
| `agents[].state`  | `missing`                  | Empty slot and no Master that RESOLVES                                                                                  | Usually pairs with `orphan-lock` → `source sync --install-missing`. But a Master that is a DANGLING SYMLINK also lands here, and its health reads `master-is-symlink`: check the health field before syncing, because that one needs inspecting, not reinstalling. An agent that HAS a slot reads `masterUnusable` instead; `missing` is for an agent with no slot.                                         |
| `agents[].state`  | `dangling`                 | Either end of the link fails to resolve — so an ABSENT MASTER lands here too, not only a broken symlink                 | `repair` when there is a Master or an adoptable shared copy; otherwise `source sync --install-missing`                                                                                                                                                                                                                                                                                                      |
| `agents[].state`  | `foreignLink`              | Slot links to a different target                                                                                        | Preserve and inspect before replacing                                                                                                                                                                                                                                                                                                                                                                       |
| `agents[].state`  | `realPathConflict`         | A real directory occupies the slot                                                                                      | With a Master present, `repair` compares the fork and quarantines it only when the bytes match. With no Master there is ONE exception — a lock-named copy in the SHARED slot is adoptable, which is the migration case — and otherwise repair cannot compare: inspect and move it aside yourself, no verb will overwrite it. Check ownership first either way; the directory may be another installer's     |
| `agents[].state`  | `inaccessible`             | Slot cannot be stat'd (permissions/IO)                                                                                  | Do NOT overwrite; fix permissions and re-run                                                                                                                                                                                                                                                                                                                                                                |
| `agents[].state`  | `orphanMaster`             | Absent slot beside an UNTRACKED Master                                                                                  | A leftover. No source to relink from; clear it with `delete skills <NAME> --all-agents` (preview, then `--yes`)                                                                                                                                                                                                                                                                                             |
| `agents[].state`  | `chain`                    | Referrer points at another link that ends at the Master                                                                 | `repair` (`source sync` cannot fix it)                                                                                                                                                                                                                                                                                                                                                                      |
| `agents[].state`  | `masterUnusable`           | The Master slot is a link or not a directory, seen from an agent that has a slot; pairs with health `master-is-symlink` | Replace the store entry with a real directory by hand — `repair` refuses                                                                                                                                                                                                                                                                                                                                    |
| `agents[].state`  | `unsupported`              | Agent cannot hold skills in this scope                                                                                  | Drop it from the matrix, or change scope                                                                                                                                                                                                                                                                                                                                                                    |

`linked`, `withheld` and `unsupported` are the three states that are not
issues; `--fail-on-issues` counts every other link state (plus `orphan-lock` and
`invalid-skill` on the health axis) and exits non-zero, which is the way to gate
a script. Not an issue is not the same as covered — for a REQUESTED agent only
`linked` passes.

`doctor` has no per-skill filter — it emits every installed skill × every agent
in `-a` (>120 KB for `-a all` over a full global scope). Keep the roster tight
and pipe when one skill matters:

```bash
aghub-cli <SCOPE> -a <ROSTER> doctor --verify-links --json \
  | jq '.[] | select(.skill == "<NAME>")'
```

## `source diff --json`

Scope-nested — filter `.[].skills[]`, **not** the top level, or every `select`
returns zero rows:

```
[{scope, origin, skills:[{name, state, skillPath, reason?, previousName?, upstreamCommitTime?}]}]
```

`origin` is the repository THAT scope was judged against: a host-blind
`owner/repo` resolves per scope from that scope's own lock, so two scopes can
legitimately point at two different forges.

`state` is one of `notInstalled`, `installedCurrent`, `installedOutdated`,
`renamed`, `removed`, `deprecated`, `uncheckable`.

Two of those are weaker than they read:

- `removed` is "the locked path is not in the fetched tree", not proof upstream
  deleted anything. A rename or move that the CHANGELOG parser did not
  recognise shows up as `removed` plus a `notInstalled` row for the new name —
  so deleting on the strength of `removed` alone can throw away a skill that
  merely moved. Look for the orphan `notInstalled` first. When that `notInstalled` row has
  the SAME name, the skill merely moved and `source sync --skill <NAME>
--install-missing` re-points the lock's `skillPath` to the new location.
- `uncheckable` with `reason: "local"` means hashing the FRESHLY FETCHED source
  directory failed — it is not a statement about your installed copy (failures
  hashing installed roots are filtered out silently while the baseline is
  built).

A missing credential does not reach a row; it fails the whole command. That
failure is not proof of a missing credential either — the same error covers a
repo or ref that the token cannot see or that does not exist.

Classification hashes every installed copy it can find across the agents, not
just the Master, and reports `installedOutdated` when any measured copy differs.
Read `installedCurrent` as the weaker claim it is: a root that could not be
hashed is dropped silently, and when nothing measurable is left it falls back to
the stored lock hash — or, when that is unusable too, returns `installedCurrent`
optimistically.

On a `renamed` row, `name` is the NEW name and `previousName` the old one. The
human table prints only STATE / NAME / SKILL_PATH / SCOPE, so use `--json`
whenever `previousName`, `reason` or `origin` matter.

## `repair --json`

```
{dry_run, scope, skills:[{name, shape, outcome, master, referrers[], unlinked[], quarantined, fused[], dry_run}]}
```

`outcome` is a plain string for `conformant` | `migrated` | `relinked` |
`reconciled` | `tidied`, but an object — `{"refused":{"reason","fix"}}` or
`{"failed":{"reason","fix"}}` — otherwise, so `.outcome == "refused"` never
matches: test `.outcome | type` first. A NAME that has no Master, no lock entry and that
no agent reads from any of its skill dirs at this scope is `{"refused":{…}}`, exit 1;
the fix points at `doctor`.

Read the row rather than predicting it. `shape` is what repair FOUND and
`outcome` what it DID, and the two are not a lookup table: a plan holds one
shape per candidate slot while the row reports a single representative, so the
candidate that caused the outcome may not be the one named. `shape` is also
`null` on a row that failed before classification, and a violation is an
externally tagged object (`{"violation":"foreign_target"}`,
`{"violation":{"chain":{"via":"…"}}}`), not a scalar.

The distinctions that change what you do next:

- `conformant` is the DEFAULT outcome, not a health certificate. With no Master
  only planned `Create`/`Relink` actions become refusals; slots repair meant to
  leave alone stay left alone, so a plan that writes nothing still reports
  `conformant` over a LOCKED skill (orphan-lock) with no Master, or a skill
  whose only copy sits where repair leaves it alone. A bulk run (no NAME) also
  suppresses conformant rows entirely, so their absence proves nothing either.
  Confirm health with `doctor --verify-links`, not with this field.
- `refused` is a DECISION: the next run repeats it. Its `fix` is a HINT, not
  something to paste blind — it can be prose, can carry `<source>` / `<agent>`
  placeholders, and does not always spell the binary as `aghub-cli`.
- `failed` is any per-skill error, not only an OS write failure, so it is not
  reliably transient either — read `reason`.
- Exit code is 1 for either.
- `referrers[]` holds PATHS, not agent ids. Map them back through
  `coverage` if you need agent names.
- `fused[]` names agents left sharing one directory afterwards, but it only
  recognises the `.agents/skills` slot — amp and kimi sharing
  `$XDG_CONFIG_HOME/agents/skills` at global scope are not listed.

A dry run walks the same branches, including the hash comparison, so a preview
that says `reconciled` is a commit that will reconcile.

## Git source versus local content

A git install has a lock entry with normalized source, repo path, commit, and
Master hash. It can take part in `source diff`, `source sync --update`, and
`check --online`.

`add --from` creates local content with no source lock. Re-running it does not
refresh an occupied Master. Refresh it through a backed-up delete and re-add;
move it to git provenance through the adopt/replace branch.

**The adoption guard.** An untracked Master may be adopted in place only when it
is a real directory whose bytes hash exactly like the fetched skill; an
already-correct Referrer is enough coverage to write the new source lock. Before
any mutation it rejects a differing Master hash, any symlink or junction
anywhere in the existing Master tree, and a lock owned by another provider or
canonical host/repo identity. A legacy non-GitHub project lock with no
`sourceUrl` also fails closed, because its original host cannot be proven. `source sync --install-missing` previews run this guard (unlocked, advisory) for
install rows only and report its refusal per row; an error-free preview is not a
guarantee, because update conflicts and per-agent slot refusals still appear only
on `--yes`. Every
intentional source change therefore goes through the backed-up delete/reinstall
branch.

## Commands whose names mislead

- `update skills <name>` edits metadata: absent flags keep; `--tools ''` clears
  allowed-tools; the API PUT uses the same core SkillPatch. It does not pull
  upstream content — that is `source sync --update` or `apply-update`. It
  rewrites the SHARED Master's frontmatter in place, so every agent sees the
  change, not just `-a`. On a git-sourced skill that is the in-place edit the
  authoring branch warns about: the row turns `installedOutdated` /
  `updateAvailable`, and the next `--update` or `apply-update --outdated --yes`
  reverts it. Change a tracked skill's metadata in its git source instead.
- `update mcps <name>` is a patch: flags you omit keep their old value (`-u`
  alone keeps the SSE/HTTP kind and headers; `-c` alone keeps `--env`).
  `--header`/`--env` replace the whole map, `-t` switches the remote kind, and
  `-t` or `--header` on a stdio server needs `-u` (otherwise it is refused,
  `VALIDATION_FAILED`, nothing written).
- `repair` does not take a roster: a scalar `-a` is ignored, a comma list or
  `-a all` is rejected outright. It plans against every supported agent's slot
  and grants a new Referrer only to an agent that can already READ the skill.
  Granting a NEW agent is `source sync --install-missing`.
- `--all` is read-only everywhere EXCEPT `prune-lock`, where it writes — both
  locks when a project root exists, global only (project silently skipped,
  still exit 0) when there is none. It stays lock-only: never deletes skill
  files or edits agent config, and previews unless `--yes`.
- Plain `delete` discovers a skill through the AGENT CONFIGS and never looks up
  `.aghub/<name>` directly, so a Master with no Referrer left anywhere is
  reported `absent` and stays on disk. `delete --all-agents` DOES reach it: the
  preview lists the Master in `paths` and `--yes` removes it (`removed`). A
  committed skill delete also reconciles the whole scope lock against disk —
  read `would_prune_lock_entries` in the preview.
- `outcome: "kept"` in a PREVIEW means the commit will not remove it. For a
  single agent whose directory is shared, the preview is `kept` (`success: true`,
  exit 0, the entity is still there) and the `--yes` run then FAILS
  `UNSUPPORTED_OPERATION` (exit 1) instead of returning `kept`. When a
  single-agent `--yes` delete is refused with `UNSUPPORTED_OPERATION`, the
  refusal names enabled readers of the same directory that are not in `-a`,
  including a private real directory reached through another agent's
  dotfiles-style symlinked skills root
  (disabled agents are not named; a refusal caused by npx-era or compat
  leftovers does not name readers); fix is to list them in the same `-a`
  request. A shared-root real directory that git TRACKS is refused the same way
  (exit 1, and the refusal itself includes its path): keep authoring it there,
  or untrack it with `git rm -r --cached <path>` and delete again. `--all-agents`
  applies the same guard and refuses it too, so it is not a bypass. The same
  refusal covers a directory git cannot be asked about (git missing, unusable
  repository, `dubious ownership`, or a probe over its 10 s limit). This is not what `--all-agents` does with a survivor
  — that asserts "gone everywhere", so a survivor makes the confirmed run ERROR
  instead. And `removed` does not prove the bytes are gone: a private Referrer
  can be removed while another copy survives, listed in `skipped`.
- Read `outcome` (`preview` | `removed` | `absent` | `partial` | `kept`), never
  `dry_run` / `executed` — and for `preview` and `kept`, exit zero is not
  completion. A `-a` list's text tally counts `kept` rows apart
  (`N ok, M kept (nothing removed), K failed`). In a multi-agent skill delete
  (`delete skills <NAME> -a a,b,c`), `-a` order does not affect the verdict: core
  plans shared slots first and attributes prior-row credit so private readers
  listed before shared-slot writers succeed without ordering shared slots first
  or re-running. Whole-batch preflight rejection exits 1, states that nothing was
  written, and lists every refused target; under `--json`, the whole-batch failure
  appears in the top-level `error.code` envelope (`UNSUPPORTED_OPERATION` or `INVALID_CONFIG`
  lives there, not in `results[].code`). In `--json`, every row in `results[]`
  carries `outcome`, a row-level `code` appearing only on row failure (`partial` or config load error,
  null on success; missing skills report no-op success with `outcome: "absent"` and exit 0 without an error code,
  whether completely absent or retaining an in-scope lock entry),
  `still_read_from` (paths still reading the
  skill if kept), batch-level `still_read_by` (agents still holding the skill after the removal, copied onto every row, alongside `still_read_by_managed` and `still_read_by_unmanaged`),
  `master_reclaimed` (on commit, whether Master was removed on disk; in preview `master_reclaimed: false` and, if exhaustive and Master exists, `would_reclaim_master: true`),
  and independent lock prune fields (`pruned_lock_entries`, `would_prune_lock_entries`,
  `prune_error`). Rows preserve the caller's request order. A Master listed under
  `kept (shared with other agents)` is still read by another agent: never `rm`
  it; name every holder in one `-a` list or use `--all-agents`.
- `check` is offline unless `--online`, and its scope spans global + project
  unless narrowed (as do `doctor`, `source list` and `source diff`). Most
  mutating commands default to global, but `source sync` is not one of them —
  it requires an explicit `-g` or `-p`.
- `apply-update <NAME>` applies one locked update and refuses outright without
  `--yes` — it has no preview. `apply-update --outdated` (every
  `updateAvailable` row in one scope) DOES preview without `--yes`. Use
  `source sync` when a subset of one source is useful.
- `transfer` and `reconcile` copy normalized resources between agents; they do
  not manage git provenance. A `reconcile` that REMOVES previews unless `--yes`.
- `enable` / `disable` take MCP servers only — passing `skills` is a parse error
  listing the valid values. To stop one agent seeing a skill, remove that
  agent's Referrer (`delete skills <NAME> -a <AGENT>`, or `reconcile --remove`),
  and check `coverage` first: on a shared directory that removes it for every
  agent sharing it.
- `agents list|enable|disable` configures which agents aghub manages (the
  selection `-a all`, `source sync -a all`, repair, rename, and delete-from-all
  read). `enable` / `disable` is a reversible toggle (no preview needed). The
  first `enable` or `disable` on a machine with no stored selection
  (`agents list --json` shows `configured: false`) freezes today's full agent
  list into an allow-list (`configured` becomes true) — even a no-op
  `agents enable <id>` does this — so an agent added to aghub later is NOT
  managed until you enable it.

## `get mcps --json`

Each row carries `transport{type,url|command,args,env|headers,timeout}` and, on
the all-agents paths (`-a all`, `-a a,b`), `source` and `agent`. The legacy
top-level `type` is kept. The table shows only type and target (url or command),
never headers or env values, which may hold tokens.

## Sub-agents

`get|describe|add|update|delete sub-agents` (alias `sub-agent`) manage an
agent's sub-agent markdown files. `add` needs `--name`, `-d` and `--instruction`;
`update <name>` patches `-d` and/or `--instruction` (absent keeps the old value,
extra frontmatter is preserved); `delete` previews unless `--yes` and reports
`outcome` `preview` | `removed` | `absent`. `get` shows name and description only
(`--json` adds `instruction`). **Mutations take a single agent**: `-a a,b` and
`-a all` are refused before any write; an agent without sub-agent support fails
with `UNSUPPORTED_OPERATION`. `get -a all` / `-a a,b` work.

## Rename and interoperability edges

`source accept-rename` is transactional across the old and new Master,
Referrers, and lock. The old Master is removed in the same transaction (covered by the same
rollback) unless a disabled agent's Referrer still points at it; then it is kept,
silently, and shows up untracked in `doctor`. Its rollback is BEST-EFFORT, though — `--help` says "rolls
back on any failure", but the restore path discards its own errors, so a failure
that also fails to unwind leaves a half-state with no signal. After any failed
rename, inspect with `doctor --verify-links` rather than assuming the disk is
back where it started. Current builds scan the fetched catalog for the new
frontmatter name, so a directory move upstream is supported.

The lock format is npx-compatible. Exotic Unicode filenames can hash differently
across implementations; verify hashes before mixing tools for such skills.
