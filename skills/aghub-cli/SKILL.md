---
name: aghub-cli
description: "Installs, relinks, updates and diagnoses skills managed by the `aghub-cli` binary — keeping a skill's Master content and its per-agent links in sync across coding agents (Claude Code, Codex, Cursor, and others). Use when a skill is installed but one agent cannot see or run it, when a skill needs linking or syncing to every agent, when publishing an authored edit to its git source so all agents pick it up, or when `doctor` / `.skill-lock.json` / `.agents/skills` state needs reading. Not for making a plugin loadable by a second engine (dual-host-plugin)."
---

# Manage skills with `aghub-cli`

Decide two axes before touching anything. **Scope** says where the Master lives;
**roster** says which agents may read it. Most bad installs come from deciding
only one.

Exact syntax belongs to `aghub-cli <command> --help` — it is unusually complete,
including the destructive-default and token rules. This file carries what the
help cannot: the storage model, which branch to take, and the traps.

For the commands this file does not route — MCP servers, sub-agents, Claude Code
plugins, inference providers, skill-usage — see
[other commands](references/other-commands.md).

## The storage model

One Master per scope at `.aghub/<name>` (`~/.aghub` global, `<root>/.aghub`
project). **No agent reads that directory.** Every grant is a separate Referrer
symlink in an agent's own skills dir, so a healthy, current Master reaches
nobody by itself. Content state and coverage are independent axes.

Which dir an agent uses — and whether that dir is SHARED — differs per agent
**and per scope**. Ask, never hard-code:

```bash
aghub-cli <SCOPE> coverage    # REFERRER DIR + SHARES WITH, per agent (-a is ignored)
```

The sharing is the footgun: granting to one agent on a shared dir grants to
every agent that READS that dir, and a shared Referrer cannot be revoked for one
agent alone. Ask the matrix, do not remember it — but read it correctly:
`SHARES WITH` lists the agents that WRITE the same dir, not everyone who reads
it. At project scope only amp writes `<root>/.agents/skills` (`SHARES WITH` is
`-`), yet well over a dozen agents read it; globally cline and warp write
`~/.agents/skills` and roughly eleven agents read it. So one grant there reaches
all of those readers, and `doctor` still calls the ones without their own
Referrer `withheld` — `withheld` therefore does not mean "cannot see it". The
JSON form carries no directory (use the table for paths). `coverage` is static:
it names no skills; per-skill state comes from `doctor --verify-links`.

Its `REFERRER DIR` column is where aghub WRITES. Several agents READ more places
than that — cursor and opencode also read `~/.agents/skills` for npx interop —
which is why `repair` can hand a Referrer to an agent `coverage` never paired
with that directory.

### `withheld` is not coverage

`doctor --verify-links` gives one state per agent. The one that misleads is
**`withheld`**: the Master is healthy and this agent holds no Referrer anywhere
it privately reads. That is a legitimate resting state ("installed, deliberately
not granted"), and `--fail-on-issues` correctly ignores it — but it is a PASS
only for an agent nobody asked for. **For a requested agent, `linked` is the
only pass.** Reading `withheld` as "fine" reports success for an agent that
cannot see the skill.

"Anywhere it privately reads" is load-bearing, because an agent reads more dirs
than it writes. A Referrer in one of its own PRIVATE read-only dirs counts as
`linked` — that is how an install made before an agent moved its write slot
stays covered instead of auditing as a `withheld` the user cannot act on. The
SHARED slot is deliberately excluded from that: sitting in `.agents/skills` with
no Referrer of your own IS the withheld state. Getting out of a read-only compat
dir is `repair`'s job, and it works because repair's grant roster asks READ
paths while its candidate slots come from WRITE dirs.

## 1. Establish the matrix

```bash
aghub-cli --version    # this file describes >= 2.18.0
```

Below 2.18.0 the Master lived in `.agents/skills` and agents could read it
without a Referrer — none of the model above applies. Upgrade before mutating.
`X.Y.Z-N-gSHA` is a clean source build N commits past the tag, and a `-dev`
suffix is a dirty one.

Fix four values before continuing:

- **scope** — exactly one of `-g` / `-p`. Project scope needs an agent marker or
  `skills-lock.json` at or above the cwd; `.git` alone is not one. The walk-up
  stops at the FIRST marker, and `~/.claude/` is one — so from a directory under
  `$HOME` with no closer marker the project root IS `$HOME`, and project scope's
  `<root>/.aghub` is the global store: default-scope `doctor` lists every global
  skill a second time as project `untracked`, and a `-p` install writes
  `~/skills-lock.json`. Do not adopt or clean those rows. Before any `-p`
  command read `-p coverage`'s REFERRER DIR: if it sits directly under `$HOME`
  you are not in a project — use `-g`, or run from the real project root.
- **roster** — one id or a comma list. `-a all` only when the user asks for it.
- **skill name(s)** — the `name:` in the skill's SKILL.md frontmatter, which is
  what install matches. A folder name that disagrees will not be found.

A source repo is scanned for `SKILL.md` well below its root — up to folder
depth 10, and installed locks really do hold source paths five segments deep,
inside `plugins/` and `.agents/`. Two files sharing one frontmatter name are
deduped **first-seen wins**, so the later one is silently invisible rather than
overwriting anything, and the dedup is case-SENSITIVE (`Foo` and `foo` are two
skills). Before publishing a source, check for collisions on the frontmatter
name rather than the directory name, and ask git rather than the filesystem
because discovery walks the git tree. Strip the quotes before comparing —
aghub compares PARSED values, so `name: foo` and `name: "foo"` collide while a
raw grep shows two different strings:

```bash
git ls-files '*SKILL.md' | xargs -r grep -h '^name:' \
  | sed 's/^name:[[:space:]]*//; s/^["'"'"']//; s/["'"'"']$//' | sort | uniq -d
```

- **source string** — what you will pass to `source sync`. `doctor` does not
  print it verbatim (`owner/repo` for GitHub, `type:source` for other hosts,
  `—` when there is no lock entry). Take it from `source list --json`.

For a private **HTTPS** source, export the token before step 2. aghub reads the
ENVIRONMENT, so a working `git push` to that same repo proves nothing — for
github.com the environment is the only mechanism; other HTTPS hosts have a
system-`git` fallback that may pick up a credential helper, which is why the
same failure appears on some hosts and not others. An `ssh://` or `git@host:`
source discards tokens entirely and reports `uncheckable` with `reason: "ssh"` —
re-pin it to its HTTPS URL instead of hunting for a credential.

`GIT_PASSWORD` is read for ANY HTTPS host and wins over `GITHUB_TOKEN` even on
github.com. So export `GIT_PASSWORD` for a non-GitHub host, and when a github.com
fetch fails with `auth` although `GITHUB_TOKEN` is set, look for a stale
`GIT_PASSWORD` in the environment first.

```bash
export GITHUB_TOKEN="$(gh auth token)"   # github.com https sources
```

## 2. Read the current state

```bash
aghub-cli <SCOPE> -a <ROSTER> doctor --verify-links --json
aghub-cli <SCOPE> source list --json
aghub-cli <SCOPE> source diff <SOURCE> --json     # git sources only; always fetches
```

Their JSON shapes differ, and a `jq` filter aimed at the wrong level silently
returns nothing: `doctor` emits a FLAT array of skill rows (`.[] | select(...)`),
while `source diff` nests skills under a per-scope wrapper (`.[].skills[]`).
Shapes, full value domains and per-skill filters are in
[state semantics](references/state-semantics.md).

Mixed pins — one source whose entries sit on different refs — are the normal
residue of installs made at different times, and the two commands treat them
OPPOSITELY. **`source sync` refuses** until `--ref` names one. **`source diff`
does not**: it splits the entries into per-ref cohorts and reports each row
against its own ref. So do not carry `--ref` into `diff` out of habit — it
collapses the cohorts onto one tree and manufactures `installedOutdated` rows
for skills that are legitimately pinned elsewhere. `source list` reads the lock
only and is unaffected.

`sync`'s refusal prints the pin set. `(default branch)` in that list is how a
lock entry with NO recorded ref displays — not a value you can pass back; use
the repo's real default branch name. An install records the ref it resolved
(from `--ref`, else the entry's existing lock ref) and records nothing when
there is neither, while `--update` re-stamps only hash and `refCommit`. So the
pin set does not converge on its own: a source stays mixed until every entry has
been reinstalled.

## 3. Take one branch

Every mutating flow first takes one interprocess lock per scope, named
`.aghub-mutation.lock`. Globally it sits beside the global lock file, so its
directory follows `XDG_STATE_HOME`; for a project it is `<root>/.agents/`,
deliberately NOT beside `skills-lock.json`. Do not hard-code either. Another
aghub holding it — a second CLI, or the desktop app — blocks yours. **Do not
delete that file to get past it.** Find the other process instead: two syncs
writing one Master at the same time is precisely what it prevents. Read paths
are unlocked, so a `doctor` or `diff` never waits.

A `doctor` row is not one state. It carries `health`, `master`, and a `state`
per agent, and the verb depends on that COMBINATION plus what is actually on
disk plus whether you want the skill restored or the record dropped. What
follows routes the cases that have one answer. It deliberately does not route
every combination — some have no single-verb fix and one has no fix at all. When
nothing here matches cleanly, read
[state semantics](references/state-semantics.md) and look at the disk instead of
forcing a verb.

**Ask first: is there something to point at?** A Master at `.aghub/<name>`, or a
lock-named copy in the SHARED slot that repair can adopt. Content in an agent's
private directory does not count — repair leaves that alone.

- Something to point at, slot OCCUPIED BY THE WRONG THING (`dangling`,
  `foreignLink`, `realPathConflict`) → [Repair](#repair-the-layout). A squatting
  real directory is compared against the Master and quarantined only when the
  bytes match. **Do not assume the Master is the correct side**: that directory
  may belong to a different installer, so look for an ownership record (a plugin
  manifest, an `installed.json`) before you let anything move it. Content the
  Master LACKS belongs in the git source — take it there through the authoring
  branch. Hand-editing the Master to absorb it is the same materialized-copy
  mistake `--update` later discards.
- Something to point at, slot EMPTY → that is `withheld`, not damage. Repair
  leaves it alone; granting it is [Grant](#grant-an-agent).
- Nothing to point at → repair refuses or reports `conformant` (exit 0) and
  writes nothing. Only a name that nothing holds at all — unlocked, no Master, no
  copy anywhere — is refused outright. A LOCKED name with nothing on disk, or an
  unlocked name whose only copy sits in an agent's private dir or the shared slot,
  reports `conformant`. [Grant](#grant-an-agent) is the branch, because it
  fetches.

| What step 2 showed                                                                                  | Branch                                                                                                                                                      |
| --------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------- |
| A REQUESTED agent is `withheld`, or the skill is `notInstalled`                                     | [Grant an agent](#grant-an-agent)                                                                                                                           |
| You changed the skill's content yourself                                                            | [Publish an edit](#publish-an-edit-to-a-skill-you-author)                                                                                                   |
| `installedOutdated` in `source diff`, `updateAvailable` in `check --online`, or "update everything" | [Update content](#update-git-sourced-content)                                                                                                               |
| `renamed`                                                                                           | [Accept a rename](#accept-an-upstream-rename)                                                                                                               |
| `health: untracked` — a Master with no lock entry                                                   | [Adopt or replace](#adopt-an-untracked-master-or-replace-provenance). An exact-byte match is adopted in place; that is the same branch, not a different one |
| `unsupported` for an agent you asked about                                                          | Nothing to run — that agent cannot hold skills in this scope. Drop it from the matrix or change scope                                                       |

**Dead ends. These are disclosed, not routed — do not pick a verb for them:**

- An `orphan-lock` with NOTHING on disk restores only while `source diff` still
  calls it `installedCurrent`. If the stored hash makes the row
  `installedOutdated`, `--install-missing` skips it and `--update` refuses
  because there is no installed copy to resync; a `renamed` row refuses for the
  same reason. Drop the stale entry with `prune-lock` and install it fresh, or
  accept that the entry is stale and just prune it.
- `orphanMaster` — a Master with no lock entry and no slot for this agent — has
  no source to relink from. Wanting it tracked again is the adopt branch, and
  adoption only succeeds on an exact byte match. Not wanting it is
  [Clean up](#clean-up-leftovers).
- A `realPathConflict` with NO Master anywhere — the content exists only as that
  private directory — is the one repair cannot resolve: it has nothing to
  compare the fork against, so it cannot quarantine it. `source sync` will
  materialise the Master but still refuses to take over the occupied slot,
  reporting `installed: false` with "a real directory or a foreign link already
  occupies this skill slot". Compare that directory against the fetched skill
  yourself, move it aside, then grant. (With a Master present this is NOT a dead
  end — that is the repair case above.)
- `health: master-is-symlink`, `invalid-skill`, `inaccessible`, and the
  `removed` / `deprecated` / `uncheckable` diff rows are inspect-first —
  [state semantics](references/state-semantics.md). Never reinstall over
  `master-is-symlink`.

### Repair the layout

`repair` is one verb for every non-conformant on-disk shape — un-migrated copy,
symlink chain, Referrer pointing elsewhere, npx-clobbered fork. Layout only: it
never fetches. Naming a skill repairs it whether or not the lock knows it, so
you can point it at something you cannot otherwise diagnose; a bulk run (no
NAME) only walks the names the lock holds. The lock gates ADOPTION, not
attention.

```bash
aghub-cli <SCOPE> repair [NAME]           # preview; omit NAME for every locked skill
aghub-cli <SCOPE> repair [NAME] --yes
```

It is built to be read by an agent: `--json` carries the shape it found, every
path it touched, and for a refusal a `fix` hint. Exit 1 means something was
refused or failed. Read the preview's `referrers[]` before `--yes` — that list,
not this file, is the authority on what the run will write.

A **`failed`** row is the exception, deliberately: it carries no `shape`, an
empty `master`, no `referrers` and no `quarantined`, because the write may have
landed partly and naming a path would claim it landed. So do not read that row's
empty paths as "nothing was written" — read its `reason`, fix what it names, then
re-run. Repair is idempotent and picks the skill up in whatever state it now
holds, but a re-run alone is not guaranteed to clear a `failed` row.

- **`-a` is not a narrowing knob here.** A scalar `-a` is ignored; a comma
  roster or `-a all` is REJECTED outright. Repair plans against every supported
  agent's slot, and an agent with no slot is granted one only when it can
  already READ the skill (read paths, not write dirs — see the `coverage` note
  above). So a preview naming more agents than you expected is usually correct:
  once the Master leaves a shared slot, the agents that were reading it there
  need Referrers of their own or they lose the skill silently.
- **What repair can and cannot adopt.** It adopts a real copy into the Master
  only from the recognised SHARED slot, and only for a skill the lock names.
  The same content sitting in an agent's PRIVATE directory is left alone, so
  repair can legitimately do nothing about an `orphan-lock` whose content is
  visibly on disk. With no Master and nothing adoptable it refuses — that is the
  grant branch below, not a repair. (`dangling` reaching here at all surprises
  people; [state semantics](references/state-semantics.md) says why.)

Do not predict the outcome from the state you observed; the same state reaches
different outcomes depending on what else is on disk. Run the preview and read
what it says it will do.

And `conformant` is not proof the layout is healthy. A NAME that
exists nowhere is refused; a locked skill with no Master, or one whose only copy
repair leaves alone, can still report conformant. When there is no Master, only
the slots repair had planned to CREATE or RELINK turn into refusals — a
slot it was going to leave alone stays left alone, so a plan that touches
nothing reports `conformant` while the skill is still unreachable. A bulk run
(no NAME) then suppresses that row entirely. Name the skill, and confirm with
`doctor --verify-links` rather than with repair's own verdict.

### Grant an agent

Granting is an install action, so it goes through `source sync`, not `repair`:

```bash
aghub-cli <SCOPE> -a <ROSTER> source sync <SOURCE> --skill <NAMES> --install-missing --json
aghub-cli <SCOPE> -a <ROSTER> source sync <SOURCE> --skill <NAMES> --install-missing --yes --json
```

Preview first and read `targetAgents`; check `coverage` before committing if the
roster touches a shared dir. An install row's `error` in the PREVIEW is a
predicted refusal (exit 1), so it is not something to push through with `--yes`.
The converse does not hold: only the install rows' adoption guard is predicted.
An update row's `SKILL_UPDATE_CONFLICT`, a per-agent slot refusal
(`installed: false`) and an unreadable lock still surface only on `--yes`, so
read the committed row too. Naming a skill that is already installed is
idempotent, which makes this the branch for a Referrer that should exist and
does not.

**The trap: `--install-missing` plans only `notInstalled` rows, plus explicitly
named `installedCurrent` ones.** An `installedOutdated` skill is filtered out,
so granting an agent a skill that has an upstream update produces an EMPTY plan
— exit 0, nothing written, the agent still `withheld`.

**Adding `--update` to the same command does not rescue it.** The outdated row
goes to the update branch instead, and an update resyncs the Master and the
Referrers that already exist — it links no new agent, and its row carries no
`agents` key at all (the field is dropped when empty, so `.agents` reads `null`,
not `[]`). So check the row's state in `source diff` first, and when it is
outdated run TWO commands: update it to `installedCurrent`, then grant.

The same silence hides a wrong name: an unknown `--skill` value — a folder name
that is not the frontmatter `name` — is also an empty plan with exit 0, and the
"has no skill named" warning goes to stderr only.

In the committed run each `actions[].agents[]` entry is
`{agent, installed, error?}`. An agent whose slot was ALREADY correctly linked
reports `installed: true` — it can read the skill, which is what the field
means — so every `installed: false` you see carries an `error`. Do not treat a
false-with-no-error row as idempotent success; it is a false pass. The preview
omits `agents` entirely.

To revoke for ONE agent, delete that agent's Referrer (`delete skills <NAME>
-a <AGENT>`, or `reconcile --remove`) — but only when that agent has a PRIVATE
dir. aghub will not revoke a shared Referrer on an agent's behalf: the
single-agent delete previews `outcome: "kept"` (exit 0, nothing to remove) and
the `--yes` run then fails `UNSUPPORTED_OPERATION` (exit 1). A shared slot goes
only when EVERY agent that reads it is in the `-a` list, and `coverage` does not
list those readers (see above). To remove the skill everywhere use `delete
skills <NAME> --all-agents`.

An agent that already holds a linked copy is a cheaper grant source: for a skill
linked to at least one agent — including an untracked `add --from` skill, which
`source sync` cannot grant — `transfer skill --from-agent <LINKED_AGENT> --name
<NAME> --to <AGENT>` (repeat `--to`) links the EXISTING Master offline. It
writes at once — no preview, no `--yes`. It never fetches, so the
`installedOutdated` trap above does not apply, and for the same reason it grants
the Master as-is, stale content included.

### Publish an edit to a skill you author

The Master is a materialized copy. Editing it in place is overwritten by the
next `--update`, and the changed hash makes the adoption guard in the INSTALL
path refuse to adopt it later (`refusing to adopt it` — that is `source sync`,
not `repair`). Edit the git source instead:

```bash
# in the source repo checkout
git add <skill-dir> && git commit && git push
# then, per scope
aghub-cli <SCOPE> source sync <SOURCE> --skill <NAME> --update --json
aghub-cli <SCOPE> source sync <SOURCE> --skill <NAME> --update --yes --json
```

`source diff` fetches the remote, so an unchanged diff right after an edit means
the ref it fetched does not carry your change — usually an unpushed commit, but
check which ref the entry is pinned to before assuming that. If a Master was already edited in place,
`--update --yes` restores it from upstream and the local edit is lost — which is
why the edit belongs in the source.

### Update git-sourced content

For ONE source, the same two commands as above, selecting the
`installedOutdated` rows from `source diff`. `--update` is scope-wide for the
Master and existing Referrers; `-a` narrows only install/relink actions. Follow
with the grant branch if the roster should also grow.

For EVERY outdated skill across all sources — "update everything", the desktop's
update-all button — there is one verb:

```bash
aghub-cli <SCOPE> apply-update skills --outdated --json         # preview
aghub-cli <SCOPE> apply-update skills --outdated --yes --json
```

It runs `check --online` for that one scope and resyncs every `updateAvailable`
row in one batch, so its target list IS the online check's, not `source diff`'s
(the two disagree legitimately — section 4). Three things the command will not
tell you unless you read the preview:

- **"Update available" includes a local edit.** An edited Master is on the list
  and `--yes` restores it from upstream. Compare the preview's `skills[]` with
  what the user edited on purpose; that edit belongs in the authoring branch.
- **One scope per run.** `--all` is refused; run `-g` and `-p` separately.
- **`renamed` rows are skipped**, listed under `renamed[]` — that is the
  accept-rename branch. `uncheckable` rows are not targets either; they are listed
  under `uncheckable[]` with a `reason`, so an empty `skills[]` beside a
  non-empty `uncheckable[]` is not proof everything is current.

Judge the committed run by `results[].success` per row; exit 1 means at least
one row failed and the others still ran. Missing from `apply-update --help` on
an older build: fall back to one `source sync <SOURCE> --update --yes` per row
of `source list --json`.

### Adopt an untracked Master or replace provenance

Back up outside aghub-managed directories first, then preview the normal
`--install-missing` branch. Adoption succeeds only under narrow conditions
(exact hash match, no symlink in the tree, no conflicting lock owner) —
[state semantics](references/state-semantics.md) has them, and when the guard
refuses, that refusal is the answer. The preview runs the same guard: a refusal
shows up as that row's `error` ("refusing to adopt it" / "already owned by
source" / "contains a link") and the preview exits 1; `--yes` would refuse
identically before writing anything.

Local content that must survive belongs in the git source first (the authoring
branch). For a deliberate source swap: preview `delete skills <NAME>
--all-agents`, read every path in the preview — it includes the Master — and
read `would_prune_lock_entries` too: a committed skill delete also drops the lock
entries of OTHER skills that have no Master on disk, including an `orphan-lock`
you could still have restored with a grant (restore or prune those first).
`--yes` it, then install from the new source with an explicit `-a` roster.

If that delete refuses with "Read only by disabled agent(s)", those agents were
disabled after being granted the skill. aghub never sweeps a disabled agent's
dirs unless that agent is the `-a` target — and `-a` defaults to `claude`, so a
disabled claude IS swept when you omit `-a` — and its link keeps the Master
alive. Run `aghub-cli agents list` to see which agents
are unmanaged and `aghub-cli agents enable <id>` to turn one back on (note that
`-a all` and `source sync -a all` skip unmanaged agents), or unlink exactly the
entries the message lists, then retry — do not widen the roster to `-a all`.

If a single-agent delete refuses with "Also read there by agents not in this
request", the skill lives in a shared slot still read by other agents. Only
ENABLED agents are named (disabled agents never block a single-agent delete).
Include the named agents in the same `-a` request, or delete for every agent
(`--all-agents`, which also unlinks it for them).
The same rule applies when the shared-slot entry is a real directory (not a
link): it is deleted once every ENABLED agent reading that slot is in the `-a`
list (disabled agents never count), and refused, naming the readers left out,
otherwise. A symlink to it from an agent NOT in the `-a` list also blocks it: the
delete exits 1 and names that link and agent (for example
`.claude/skills/<name>` when claude was left out of the request). A symlink from
an agent that IS in the `-a` list does not block it; that agent's own row
unlinks it, and the verdict is the same in whatever order `-a` lists the agents
(preview and `--yes` agree). A real directory inside `.aghub` is never deleted by
a single-agent delete. `reconcile --remove` and the desktop's manage-agents /
bulk dialogs apply this same rule to their removal list. A real directory has no
`.aghub` Master behind it, so deleting it deletes the content and the lock entry
is pruned too. Local edits are gone for good; if the skill originally came from a
source, `source sync <repo>` can install a fresh copy of the source's version.
If no other Referrer remains, the Master content is deleted too (the removed
paths include `.aghub/<name>`), so re-enabling a disabled agent later does not bring
the skill back; it has to be reinstalled.

An `add --from` install is intentionally untracked and has no upstream-update
branch; refresh it through the same backed-up delete plus a re-`add` with an
explicit comma roster.

### Accept an upstream rename

Confirm `source diff` reports `renamed`, preview `source accept-rename`, then
repeat with `--yes`. Both arguments come from that row (`--help` names which
field is which). The transaction resolves the new frontmatter name even when the
repo directory moved. The preview names no paths and no agents. The commit grants
the new name to every MANAGED agent that could READ the old one — shared-slot
readers get their own Referrer, a disabled agent gets nothing — and the committed
`paths` can list a shared slot more than once, so dedupe before counting. The commit removes the old name everywhere, its `.aghub/<old>`
Master included. The one exception is an agent you disabled (`agents list`) that
still holds a Referrer to it: the old Master is kept for that agent and shows up
untracked in `doctor` (enable the agent or unlink that entry, then `delete skills
<OLD> --all-agents`). The preview does not fetch or check the disk, so a wrong
name, a new name that already exists in the scope (`TargetExists`) and a locked
old name with no installed copy (`NoInstalledCopy`) all preview as success (exit 0) and fail only on `--yes` (exit 1). Run `doctor --verify-links` afterwards as the completion check.

### Clean up leftovers

An `orphanMaster` is typically what an earlier `delete` left when it spared a
Master another agent still read. Plain `delete skills <NAME>` cannot clear it:
it discovers a skill through the AGENT CONFIGS, so with no Referrer left anywhere
it reports a successful `absent` while the Master stays on disk. `delete skills
<NAME> --all-agents` does reach it: the preview lists `.aghub/<name>` in `paths`,
and `--yes` removes it (`outcome: "removed"`). Do not `rm` the directory by hand.

Check the other agents' rows first — an untracked Master can still have a live
`linked` Referrer, and removing it would dangle that link. A stale LOCK entry is
a separate job: `prune-lock`.

**Completion criterion for every branch**: exit zero is not enough. Read the
verb's own verdict — `outcome` for `delete`, `outcome` per row for `repair`, and
for `source sync` the row's own `error` FIRST — an install that fails outright
emits no `agents` at all, so a caller that only walks `agents[]` sees an empty
list and calls it success — then, for an install/grant row, each agent's
`installed`/`error`. An update row never carries `agents`; judge it by
`applied`/`error`. Treat `preview`, `kept`,
`refused` and any partial result as open work rather than as done. `absent` is
the one that depends on what you asked: from `delete` it means the skill was
already gone and retrying will not help, but on an orphan Master a plain `delete`
(without `--all-agents`) reports it because deletion never found the thing you
were trying to remove.

## 4. Prove the matrix

```bash
aghub-cli <SCOPE> -a <ROSTER> doctor --verify-links --json
aghub-cli <SCOPE> source diff <SOURCE> --json
aghub-cli <SCOPE> check skills --online --json
```

For a git install require `health: "ok"`, `updatable: true`, the expected
source, `installedCurrent` in the diff, `status: "upToDate"` in the online
check — and **`linked` for every agent in the requested roster**. For a local
install `untracked` and `updatable: false` are expected, but the roster bar is
the same.

`updatable` is a conjunction — fetchable source type AND a recorded `skillPath`
AND a valid Master — so `health: "ok"` with `updatable: false` is real, usually
a lock entry with no `skillPath`. Provenance is intact; in-place update is not
available.

Two results to read rather than retry:

- `uncheckable` is never a pass. `checked` is copied straight from whether you
  passed `--online`, so `checked: true` says the run was allowed to go online,
  NOT that this row's fetch was attempted — local, ssh and unsupported-scheme
  sources end in the precheck, and a credential-backend failure stops the row
  later but still before any fetch. Read
  `reason`: `auth` is a missing or rejected credential (export the token and
  re-run, do not reinstall), `ssh`/`local` are permanent for that source, as are
  `unsupportedScheme` and `noPath` (the lock has no `skillPath`). `network` with
  `checked: false` means you omitted `--online`; `network`/`timeout` online are
  transient — retry.
  `updateAvailable` / `renamed` route back to section 3.
- The two commands hash different things, so they disagree legitimately.
  `source diff` hashes **every installed copy it can find across the agents** —
  not just the Master — and reports `installedOutdated` if any of them differs.
  The converse is weaker than it looks: a copy it could not hash is dropped
  silently, and with no usable hash left it falls back to the lock's, or to
  `installedCurrent` when even that is unusable. So `installedCurrent` means
  "nothing it could measure disagreed", not "every copy matches".
  `check --online` compares the hash recorded in the LOCK
  (`contentHash` globally, `computedHash` in a project `skills-lock.json` —
  assert on the right one). Read a disagreement as a question about WHICH copy,
  and answer it by looking:
    - diff `installedCurrent` + check `updateAvailable` → every copy diff could
      MEASURE matches upstream and the lock hash is stale. Refresh it with
      `apply-update skills <NAME> --yes` (no preview mode — it refuses without
      `--yes`); `source sync --update` only prints `Nothing to do` here.
    - diff `installedOutdated` + check `upToDate` → the lock matches upstream and
      at least one copy on disk does not. That is an edited Master, OR a stale
      private/forked copy in some agent's own directory that diff also hashed.
      `doctor --verify-links` can show you that a private real directory occupies
      a slot, but it never hashes anything, so it cannot tell you WHICH copy
      drifted — compare the bytes yourself. `apply-update` would overwrite an
      edited Master, so look before running it. If the edit is wanted, take the
      authoring branch instead. With a forked private copy, `source sync
--update --yes` refuses that row (`errorCode: "SKILL_UPDATE_CONFLICT"`,
      "different installed copy alongside its Master", exit 1) and `repair`
      refuses too; the row stays `installedOutdated` until you move the fork
      aside.

If the user needs proof of runtime invocation rather than file discovery, run a
smoke prompt inside each requested agent — no aghub command can show that.
