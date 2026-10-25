---
name: aghub-cli
description: "Installs, links, updates and diagnoses skills across coding agents (Claude Code, Codex, Cursor and others) via the `aghub-cli` binary. Use when an agent cannot see or run a skill, a skill needs syncing to every agent or publishing from its git source, or `doctor` / `.skill-lock.json` / `.agents/skills` state needs reading; not for dual-engine plugins (dual-host-plugin)."
---

# Manage skills with `aghub-cli`

Decide two axes before touching anything. **Scope** says where the Master lives;
**roster** says which agents may read it. Most bad installs come from deciding
only one.

Exact syntax belongs to `aghub-cli <command> --help` — it is unusually complete,
including the destructive-default and token rules. This file carries what the
help cannot: the storage model, which branch to take, and the traps.

Where to read next:

- JSON shapes, every state value, the adoption guard, misleading command names →
  [state semantics](references/state-semantics.md)
- Grant, publish an edit, update, adopt / replace provenance →
  [install and update](references/install-update.md)
- Revoke, delete, clean up leftovers, accept a rename →
  [delete and rename](references/delete-rename.md)
- MCP servers, sub-agents, plugins, inference, `skill-usage`, `agents` →
  [other commands](references/other-commands.md)

## The storage model

One Master per scope at `.aghub/<name>` (`~/.aghub` global, `<root>/.aghub`
project). **No agent reads that directory.** Every grant is a separate Referrer
symlink in an agent's own skills dir, so a healthy, current Master reaches
nobody by itself. Content state and coverage are independent axes.

Which dir an agent uses — and whether it is SHARED — differs per agent **and per
scope**. Ask, never hard-code:

```bash
aghub-cli <SCOPE> coverage    # REFERRER DIR + SHARES WITH, per agent (-a is ignored)
```

Granting to one agent on a shared dir grants to every agent that READS it, and a
shared Referrer cannot be revoked for one agent alone. Read the matrix correctly:

- `SHARES WITH` lists agents that WRITE the same dir, not everyone who reads it.
  At project scope only amp writes `<root>/.agents/skills` (`SHARES WITH` is
  `-`), yet well over a dozen agents read it. Globally cline and warp share
  `~/.agents/skills` (read by roughly eleven agents), and kimi and amp share
  `~/.config/agents/skills`.
- `REFERRER DIR` is where aghub WRITES. Agents READ more dirs than that (cursor
  and opencode also read `~/.agents/skills` for npx interop). `doctor` counts
  every read dir — a Referrer found in a compat dir is `linked`, with the row's
  `path` naming where — and `repair` grants only to agents that can already
  READ the skill, which is why it may link an agent `coverage` never paired
  with that dir.
- The JSON form carries no directory (use the table). `coverage` is static: per-
  skill state comes from `doctor --verify-links`.

**`withheld` is not coverage.** It means the Master is healthy and this agent
holds no Referrer in any dir it reads — a legitimate "installed, not granted"
state that `--fail-on-issues` ignores. It is a pass only for an agent nobody
asked for. **For a requested agent, `linked` is the only pass.**

## 1. Establish the matrix

```bash
aghub-cli --version
```

This file is written against the 2.43 line; on an older build trust `--help`
over it. Below 2.18.0 the Master lived in `.agents/skills` and agents read it
without a Referrer — none of the model above applies. Upgrade before mutating.
`X.Y.Z-N-gSHA` is a clean source build N commits past the tag; a `-dev` suffix
means a dirty build, not a broken install.

Fix four values:

- **scope** — exactly one of `-g` / `-p`. Project scope needs an agent marker or
  `skills-lock.json` at or above the cwd; `.git` alone is not one. The walk-up
  stops at the FIRST marker, and `~/.claude/` is one — so under `$HOME` with no
  closer marker the project root IS `$HOME`, `<root>/.aghub` is the global
  store, default-scope `doctor` lists every global skill again as project
  `untracked`, and a `-p` install writes `~/skills-lock.json`. Do not adopt or
  clean those rows. Before any `-p` command, if `-p coverage`'s REFERRER DIR
  sits directly under `$HOME`, you are not in a project — use `-g` or `cd` to
  the real root.
- **roster** — one id or a comma list. `-a all` only when the user asks for it,
  and it (like every fan-out: repair, update, rename, delete-from-all) expands
  only to agents ENABLED in `aghub-cli agents list`; a disabled agent is silently
  left out.
- **skill name(s)** — the `name:` in the skill's SKILL.md frontmatter, which is
  what install matches. A folder name that disagrees is not found.
- **source string** — what you pass to `source sync`. `doctor` does not print it
  verbatim (`owner/repo` for GitHub, `type:source` elsewhere, `local`, or `—`
  with no lock entry). Take it from `source list --json`.

**Private HTTPS sources need nothing from you** when `git clone` works
unattended: aghub tries `GIT_PASSWORD` (any host), then `GITHUB_TOKEN`
(github.com), then the user's git credential helpers non-interactively. Do NOT
fetch and inject a token yourself; export one only where no helper exists
(headless CI). An env value shadows the helper, so a github.com `auth` failure
while logged in usually means a stale `GIT_PASSWORD`/`GITHUB_TOKEN`. An `ssh://`
or `git@host:` source discards tokens and reports `uncheckable` `reason: "ssh"`
— re-pin it to its HTTPS URL.

## 2. Read the current state

```bash
aghub-cli <SCOPE> -a <ROSTER> doctor --verify-links --json
aghub-cli <SCOPE> source list --json
aghub-cli <SCOPE> source diff <SOURCE> --json     # git sources only; always fetches
```

`doctor` emits a FLAT array (`.[] | select(...)`); `source diff` nests skills
under a per-scope wrapper (`.[].skills[]`). A filter aimed at the wrong level
silently returns nothing.

**Mixed pins** (one source, entries on different refs) are normal residue, and
the commands treat them OPPOSITELY: `source sync` refuses until `--ref` names
one; `source diff` splits entries into per-ref cohorts. Do not carry `--ref` into
`diff` — it collapses the cohorts and manufactures `installedOutdated` rows.
`(default branch)` in sync's refusal is how an entry with NO recorded ref
displays, not a value to pass back; use the repo's real default branch. Pins do
not converge on their own: `--update` re-stamps only hash and `refCommit`, so a
source stays mixed until every entry is reinstalled with one `--ref`.

## 3. Take one branch

**Mutation lock.** Every mutating flow takes one interprocess lock per scope,
`.aghub-mutation.lock` (globally beside the global lock file, so it follows
`XDG_STATE_HOME`; for a project in `<root>/.agents/`). Another aghub holding it —
a second CLI or the desktop app — makes yours wait up to 10 s and then fail with
"another aghub mutation is still in progress, so '<op>' timed out waiting for
the skill mutation lock; retry once it finishes". **Do not delete the file** —
find the other process. Reads are unlocked.

A `doctor` row carries `health`, `master`, and a `state` per agent; the verb
depends on that COMBINATION plus what is on disk plus whether you want the skill
restored or the record dropped. What follows routes the cases with one answer.
When nothing matches cleanly, read [state semantics](references/state-semantics.md)
and look at the disk instead of forcing a verb.

**Ask first: is there something to point at?** A Master at `.aghub/<name>`, or a
lock-named copy in the SHARED slot that repair can adopt. Content in an agent's
private directory does not count — repair leaves that alone.

- Something to point at, slot OCCUPIED BY THE WRONG THING (`dangling`,
  `foreignLink`, `realPathConflict`, `chain`) → [Repair](#repair-the-layout).
  **Do not assume the Master is the correct side**: a squatting directory may
  belong to another installer — look for an ownership record (a plugin manifest,
  an `installed.json`) first. Content the Master LACKS belongs in the git source
  (the publish branch), never hand-edited into the Master.
- Something to point at, slot EMPTY → `withheld`, not damage. Grant it.
- Nothing to point at → repair writes nothing: a locked name, or one whose only
  copy sits where repair leaves it, reports `conformant` (exit 0); only a named,
  unlocked name nothing holds anywhere is refused. Grant is the branch, because
  it fetches.

| What step 2 showed                                                                                  | Branch                                                                                                                              |
| --------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------- |
| A REQUESTED agent is `withheld`, or the skill is `notInstalled`                                     | [Grant](references/install-update.md#grant-an-agent)                                                                                |
| You changed the skill's content yourself                                                            | [Publish an edit](references/install-update.md#publish-an-edit-to-a-skill-you-author)                                               |
| `installedOutdated` in `source diff`, `updateAvailable` in `check --online`, or "update everything" | [Update](references/install-update.md#update-git-sourced-content)                                                                   |
| `renamed`                                                                                           | [Accept a rename](references/delete-rename.md#accept-an-upstream-rename)                                                            |
| `health: untracked` — a Master with no lock entry                                                   | [Adopt or replace](references/install-update.md#adopt-an-untracked-master-or-replace-provenance) (exact-byte match adopts in place) |
| Revoke for one agent, remove a skill, or an `orphanMaster` leftover                                 | [Delete and clean up](references/delete-rename.md)                                                                                  |
| `unsupported` for an agent you asked about                                                          | Nothing to run — that agent cannot hold skills in this scope. Drop it or change scope                                               |

**Dead ends — disclosed, not routed; do not pick a verb for them:**

- An `orphan-lock` with NOTHING on disk restores with `source sync <SOURCE>
--skill <NAME> --install-missing --yes` while `source diff` calls it
  `uncheckable` (`local`). If upstream moved (`installedOutdated`),
  `--install-missing` skips it and `--update` refuses (no installed copy), and
  `renamed` refuses likewise: `prune-lock` and install fresh, or just prune.
- `orphanMaster` has no source to relink from. Tracked again = the adopt branch
  (exact byte match only); not wanted = delete with `--all-agents`.
- A `realPathConflict` with NO Master anywhere: repair has nothing to compare
  against. `source sync` builds the Master but refuses the occupied slot
  (`installed: false`, "a real directory or a foreign link already occupies this
  skill slot"). Compare the directory with the fetched skill yourself, move it
  aside, then grant.
- `health: master-is-symlink`, `invalid-skill`, `inaccessible`, and the
  `removed` / `deprecated` / `uncheckable` diff rows are inspect-first
  ([state semantics](references/state-semantics.md)). Never reinstall over
  `master-is-symlink`.
- Upstream's root `SKILL.md` is a symlink (or any non-regular file): an
  install (`source sync --install-missing`, preview included) refuses with
  `VALIDATION_FAILED`, an update (`--update`, `apply-update`) with
  `SKILL_PARSE_FAILED` — both say "the skill's SKILL.md is not a regular file"
  and nothing is written. No flag gets past it — the fix is in the git source:
  commit a regular `SKILL.md`, then sync again.

### Repair the layout

`repair` is one verb for every non-conformant on-disk shape — un-migrated copy,
symlink chain, Referrer pointing elsewhere, npx-clobbered fork. Layout only: it
never fetches. A named skill is repaired whether or not the lock knows it; a
bulk run (no NAME) walks only lock names and suppresses `conformant` rows.

```bash
aghub-cli <SCOPE> repair [NAME] --json        # preview
aghub-cli <SCOPE> repair [NAME] --yes --json
```

- **Read the whole preview before `--yes`**: `referrers[]` (links created or
  repointed), `unlinked[]` (stale Referrers it will DELETE from read-only compat
  dirs), `quarantined`, and `fused[]` (agents left sharing one dir). That preview,
  not this file, is the authority; do not predict the outcome from the state you
  observed. Exit 1 means something was refused or failed.
- **`-a` is not a narrowing knob.** A scalar `-a` is ignored; a comma list or
  `-a all` is rejected. Repair plans against every ENABLED agent's slot
  (disabled agents are neither linked, unlinked nor listed), so a preview naming
  more agents than expected is usually right: once the Master leaves a shared
  slot, its readers need Referrers of their own.
- A squatting real directory is quarantined only when its bytes match the
  Master. A **`failed`** row deliberately names no paths (the write may have
  landed partly): read `reason`, fix it, re-run, and do not read the empty paths
  as "nothing was written".
- `conformant` is not proof of health: with no Master, a slot repair would leave
  alone stays unreachable yet reports `conformant`, and a bulk run hides the
  row. Name the skill and confirm with `doctor --verify-links`.

## Completion criterion for every branch

Exit zero is not enough. Read the verb's own verdict:

- `delete` → `outcome` (per `results[]` row for an `-a` list); `repair` →
  `outcome` per row.
- `source sync` → the row's own `error` FIRST (a failed install emits no
  `agents`), then each `agents[]` entry's `installed`/`error`; an update row has
  no `agents` and is judged by `applied`/`error`.
- `preview`, `kept`, `refused` and any partial result are open work. `absent`
  from `delete` means already gone — except that a plain `delete` on an orphan
  Master also says `absent` because it never found the Master.

## 4. Prove the matrix

```bash
aghub-cli <SCOPE> -a <ROSTER> doctor --verify-links --fail-on-issues --json
aghub-cli <SCOPE> source diff <SOURCE> --json
aghub-cli <SCOPE> check skills --online --json
```

`--fail-on-issues` makes the exit code count every bad link state plus health
`orphan-lock` / `invalid-skill` / `master-is-symlink`, but not `withheld`, so it
does not replace the roster bar. For a git install require `health: "ok"`,
`updatable: true`, the expected source, `installedCurrent` in the diff,
`status: "upToDate"` online — and **`linked` for every requested agent**. For a
local install `untracked` and `updatable: false` are expected; the roster bar is
the same.

`updatable` is a conjunction (fetchable source type AND a recorded `skillPath`
AND a valid Master), so `ok` with `updatable: false` is real — usually a lock
entry with no `skillPath`.

Read rather than retry:

- `uncheckable` is never a pass, and `checked: true` only says the run was
  allowed online, not that this row was fetched. By `reason`: `auth` = missing
  or rejected credential (fix it, do not reinstall); `ssh`, `local`,
  `unsupportedScheme`, `noPath` = permanent for that source; `ambiguousHost` =
  a URL-less `gitlab` lock entry (npx strips a self-hosted host), never fetched
  from a guessed gitlab.com — reinstall from the real URL (`apply-update` and
  `source diff`/`sync` refuse it too); `network` with
  `checked: false` = you omitted `--online`; `network`/`timeout` online =
  transient, retry.
- `source diff` and `check --online` share one verdict and agree row for row on
  the same scope (equivalences in [state semantics](references/state-semantics.md#source-diff---json));
  a disagreement means the disk changed between runs — re-run both.
- When the enabled agents' copies disagree (a forked private copy beside the
  Master's Referrer), there is no local baseline: the row is `uncheckable`
  (`local`) while the lock hash still matches upstream, but `installedOutdated` /
  `updateAvailable` once upstream moved — and then `--update` /
  `apply-update` refuses it with `SKILL_UPDATE_CONFLICT`. Move the fork aside.
  A lone private copy with no other managed copy IS the baseline.
- A `source sync --update --yes` row with `SKILL_SOURCE_CHANGED_DURING_FETCH` or
  `SKILL_SOURCE_MISMATCH` wrote nothing; re-run `source diff`, then retry.

If the user needs proof of runtime invocation rather than file discovery, run a
smoke prompt inside each requested agent — no aghub command can show that.
