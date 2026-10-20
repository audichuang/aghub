# Install and update branches

The install-side branches `SKILL.md` routes to. Every command takes the scope
and roster fixed in `SKILL.md` step 1.

## Contents

- [Grant an agent](#grant-an-agent)
- [Publish an edit to a skill you author](#publish-an-edit-to-a-skill-you-author)
- [Update git-sourced content](#update-git-sourced-content)
- [Adopt an untracked Master or replace provenance](#adopt-an-untracked-master-or-replace-provenance)

## Grant an agent

Granting is an install action, so it goes through `source sync`, not `repair`:

```bash
aghub-cli <SCOPE> -a <ROSTER> source sync <SOURCE> --skill <NAMES> --install-missing --json
aghub-cli <SCOPE> -a <ROSTER> source sync <SOURCE> --skill <NAMES> --install-missing --yes --json
```

Preview first and read `targetAgents`; check `coverage` first if the roster
touches a shared dir. An install row's `error` in the PREVIEW is a predicted
adoption-guard refusal (exit 1) — not something to push through with `--yes`.
The converse does not hold: an update row's `SKILL_UPDATE_CONFLICT`, a per-agent
slot refusal (`installed: false`) and an unreadable lock surface only on
`--yes`, so read the committed row too. Naming an already-installed skill is
idempotent, which makes this the branch for a Referrer that should exist and
does not.

**The trap: `--install-missing` plans only `notInstalled` rows, plus explicitly
named `installedCurrent` and `uncheckable` (`local`) ones.** An
`installedOutdated` skill is filtered out, so granting it produces an EMPTY plan
— exit 0, nothing written, the agent still `withheld`. Adding `--update` does not
rescue it: an update resyncs the Master and EXISTING Referrers, links no new
agent, and its row has no `agents` key (`.agents` reads `null`). When the row is
outdated run TWO commands: update it to `installedCurrent`, then grant.

The same silence hides a wrong name: an unknown `--skill` value (a folder name
that is not the frontmatter `name`) is also an empty plan with exit 0; the "has
no skill named" warning goes to stderr only.

In the committed run each `actions[].agents[]` entry is `{agent, installed,
error?}`. An agent ALREADY correctly linked reports `installed: true`, so every
`installed: false` carries an `error`; a false-with-no-error row is a false
pass. The preview omits `agents` entirely.

**Cheaper offline grant:** for a skill linked to at least one agent — including
a local `add --from` skill, which `source sync` cannot grant — `transfer skill
--from-agent <LINKED_AGENT> --name <NAME> --to <AGENT>` (repeat `--to`) links
the EXISTING Master. It writes at once (no preview), never fetches (so the
outdated trap does not apply), and grants the Master as-is, stale content
included, and never touches the lock: an untracked Master stays untracked.
Across scopes (`-g` source, `-p` target or the reverse) it copies the Master
and records the source's owner and stored Source hash in the target's lock,
so a locally edited source still reads as modified there — when the source's
host is known. A legacy URL-less entry whose host is ambiguous (an old npx
`gitlab:group/repo`, possibly self-hosted) is copied untracked instead and
compares no owner: `doctor` shows it untracked unless the target already had
an entry of that name; re-install from the real URL to track it. `transfer`
and `reconcile --add` refuse when the target scope's same-named Master is
untracked and differs; a carried (host-known) copy also refuses a different
owner. A copy onto an owned Master grants it as-is. `source
accept-rename` refuses an ambiguous entry before fetching: re-install it from
its real URL first.

## Publish an edit to a skill you author

The Master is a materialized copy. Editing it in place is overwritten by the
next `--update`, and the changed hash makes the install path's adoption guard
refuse it later (`refusing to adopt it`). Edit the git source instead:

```bash
# in the source repo checkout
git add <skill-dir> && git commit && git push
# then, per scope
aghub-cli <SCOPE> source sync <SOURCE> --skill <NAME> --update --json
aghub-cli <SCOPE> source sync <SOURCE> --skill <NAME> --update --yes --json
```

`source diff` fetches the remote, so an unchanged diff right after an edit means
the fetched ref does not carry your change — usually an unpushed commit, but
check which ref the entry is pinned to. If the Master was already edited in
place, `--update --yes` restores it from upstream and the local edit is lost.

Before publishing a source, check for frontmatter-name collisions. Discovery
scans up to folder depth 10 (installed locks hold paths five segments deep,
inside `plugins/` and `.agents/`), dedups **first-seen wins** (the later one is
silently invisible) and is case-SENSITIVE. It walks the git tree and compares
PARSED values, so ask git and strip quotes (`name: foo` and `name: "foo"`
collide):

```bash
git ls-files '*SKILL.md' | xargs -r grep -h '^name:' \
  | sed 's/^name:[[:space:]]*//; s/^["'"'"']//; s/["'"'"']$//' | sort | uniq -d
```

## Update git-sourced content

For ONE source, the two `--update` commands above, selecting the
`installedOutdated` rows from `source diff`. `--update` is scope-wide for the
Master and existing Referrers; `-a` narrows only install/relink actions. Follow
with the grant branch if the roster should also grow.

For EVERY outdated skill across all sources ("update everything", the desktop's
update-all button):

```bash
aghub-cli <SCOPE> apply-update skills --outdated --json         # preview
aghub-cli <SCOPE> apply-update skills --outdated --yes --json
```

It runs `check --online` for that one scope and resyncs every `updateAvailable`
row in one batch, so its target list is the online check's, not `source
diff`'s. Read the preview:

- **"Update available" includes a local edit.** An edited Master is on the list
  and `--yes` restores it from upstream. Compare `skills[]` with what the user
  edited on purpose; that edit belongs in the publish branch.
- **One scope per run.** `--all` is refused; run `-g` and `-p` separately.
- **`renamed` rows are skipped** (listed under `renamed[]` — the accept-rename
  branch), and `uncheckable` rows are listed under `uncheckable[]` with a
  `reason`. An empty `skills[]` beside a non-empty `uncheckable[]` is not proof
  everything is current.

Judge the committed run by `results[].success` per row: exit 1 means at least
one row failed and the others still ran. A failed row carries `error`, `code`
and `retryable` (`true` only for `SKILL_MUTATION_LOCK_BUSY`). A batch preflight
failure (an unreadable lock, for one) fails the whole command before any row
runs: no `results[]`, the error is the top-level `error.code`. `--yes` with
nothing to update prints `dryRun: false`, `results: []`, exit 0.

## Adopt an untracked Master or replace provenance

Back up outside aghub-managed directories first, then preview the normal
`--install-missing` grant. Adoption succeeds only under narrow conditions (exact
hash match, no symlink in the tree, no conflicting lock owner — the adoption
guard in [state semantics](state-semantics.md#git-source-versus-local-content));
when it refuses, that refusal is the answer. The preview runs the same guard: a
refusal is that row's `error` ("refusing to adopt it" / "already owned by
source" / "contains a link"), the preview exits 1, and `--yes` would refuse
identically before writing.

Local content that must survive goes into the git source first (publish
branch). For a deliberate source swap: preview `delete skills <NAME>
--all-agents`, read every path (it includes the Master) and
`would_prune_lock_entries` — a committed skill delete also drops OTHER skills'
lock entries that have no Master on disk, including an `orphan-lock` you could
still restore (restore or prune those first). `--yes` it, then install from the
new source with an explicit `-a` roster. Refusals of that delete:
[delete and rename](delete-rename.md).

A local `add --from` skill has no upstream-update branch: refresh it through
the same backed-up delete plus a re-`add` with an explicit comma roster
([state semantics](state-semantics.md#git-source-versus-local-content)).
