# Issue tracker: GitHub

Issues and specs (a spec is also known as a PRD) for this repo live as GitHub
issues on **the fork, `audichuang/aghub`**. Use the `gh` CLI for all operations.

> **Always pass `-R audichuang/aghub`.** This clone has two remotes: `fork`
> (live main, `audichuang/aghub`) and `origin` (upstream `AkaraChen/aghub`).
> `gh repo set-default audichuang/aghub` is set locally, but that lives in
> `.git/config` and is absent in a fresh clone or worktree — without `-R`, `gh`
> may open the issue on the upstream repo. Never file on `AkaraChen/aghub`.

Older specs and tickets from before the switch remain as markdown under
`.scratch/<feature>/`; leave them where they are.

## Conventions

- **Create an issue**: `gh issue create -R audichuang/aghub --title "..." --body-file <file>` (or a heredoc).
- **Read an issue**: `gh issue view <number> -R audichuang/aghub --comments`.
- **List issues**: `gh issue list -R audichuang/aghub --state open --json number,title,body,labels,comments --jq '[.[] | {number, title, body, labels: [.labels[].name], comments: [.comments[].body]}]'` with appropriate `--label` and `--state` filters.
- **Make an issue a sub-issue of a parent**: `gh issue create -R audichuang/aghub --parent <parent> ...`, or `gh issue edit <parent> -R audichuang/aghub --add-sub-issue <child>` afterwards.
- **Comment on an issue**: `gh issue comment <number> -R audichuang/aghub --body "..."`
- **Apply / remove labels**: `gh issue edit <number> -R audichuang/aghub --add-label "..."` / `--remove-label "..."`
- **Close**: `gh issue close <number> -R audichuang/aghub --comment "..."`

Write issue bodies in 繁體中文; keep identifiers, commands and domain terms from
`CONTEXT.md` in their original form.

## Pull requests as a triage surface

**PRs as a request surface: no.** _(Set to `yes` if this repo treats external PRs as feature requests; `/triage` reads this flag.)_

## When a skill says "publish to the issue tracker"

Create a GitHub issue on `audichuang/aghub`.

## When a skill says "fetch the relevant ticket"

Run `gh issue view <number> -R audichuang/aghub --comments`.

## Wayfinding operations

Used by `/wayfinder`. The **map** is a single issue with **child** issues as tickets.

- **Map**: a single issue labelled `wayfinder:map`, holding the Notes / Decisions-so-far / Fog body.
- **Child ticket**: a GitHub sub-issue of the map. Labels: `wayfinder:<type>` (`research`/`prototype`/`grilling`/`task`). Once claimed, the ticket is assigned to the driving dev.
- **Blocking**: GitHub native issue dependencies — `gh api --method POST repos/audichuang/aghub/issues/<child>/dependencies/blocked_by -F issue_id=<blocker-db-id>`, where `<blocker-db-id>` is `gh api repos/audichuang/aghub/issues/<n> --jq .id` (not the `#number`). A ticket is unblocked when every blocker is closed.
- **Frontier query**: the map's open sub-issues with no open blocker and no assignee; first in map order wins.
- **Claim**: `gh issue edit <n> -R audichuang/aghub --add-assignee @me`, the session's first write.
- **Resolve**: comment the answer, close, then append a context pointer (gist + link) to the map's Decisions-so-far.
