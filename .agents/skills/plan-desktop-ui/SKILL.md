---
name: plan-desktop-ui
description: Plan a page-level change to aghub's desktop UI before writing any of it — analyse what the current page does, build a clickable prototype fed with the user's REAL local aghub data, publish it for approval, then write a spec an implementer can follow. Use when a desktop page feels cluttered or confusing, when redesigning or restructuring a page, when adding a screen, or when a desktop UI change is big enough that guessing wrong is expensive; also on 這頁怪怪的 / 版面很亂 / 重新設計這個頁面 / 先給我看看畫面. Not for a one-line style tweak or a visual polish pass (use `polish`), and not for documenting an existing page as-is (use `spec-from-code`).
---

# Planning a desktop UI change

The order that works here: **understand → prototype on real data → get approval
→ spec → implement → verify**. Skipping to code on a page-level change means the
user first sees the design when it is already built, and by then disagreeing is
expensive for both of you.

Verification is the sibling skill: `verify-desktop-ui`.

## 1. Analyse before proposing

Read the page and everything it renders end to end, and trace the real flow —
not the flow the file names suggest. What you are hunting for is structural:

- **State with two owners.** The same fact written from two places (a page
  `useState` and a URL param, a list selection and a route) is where "it feels
  weird" usually lives, and no amount of visual polish fixes it.
- **A control that lies.** A toggle whose two sides show the same thing; a label
  promising a hierarchy the data does not have.
- **Chrome competing for a narrow column.** Count what stacks above the content
  in the 320px list column before the first row appears.
- **Dead ends.** A button that cannot reach the panel it names.

Say what you found and stop there if the user was only asking. A complaint is
not automatically a work order.

## 2. Prototype with the user's real data

This is the step that carries the whole skill. A prototype is only worth
building if the user can _judge_ it, and what makes it judgeable is their own
data — real skill names, real counts, real sources, real failure states.

Pull it with the CLI rather than inventing it:

```bash
aghub-cli get skills -a all -g --json          # every agent's rows
aghub-cli source list --json                   # sources per scope
aghub-cli repair -g --json                     # migration preview (dry-run unless -y)
jq . ~/.agents/.skill-lock.json                # global lock
aghub-cli check skills --online -g --json      # SLOW: ~50s of real network
aghub-cli source diff <owner/repo> -g --json   # a private repo needs GITHUB_TOKEN and
                                               # EXITS 1 with a CLI_ERROR without one
```

The last two cost real time and can fail — start them early, and treat a
credential error as data ("this source reads as unreachable") rather than a
blocker.

Shape it into one JSON blob and inline it into a single HTML file, replacing
every `<` with its `<` escape first — one skill description containing a
closing script tag ends the script block early and the page dies silently.

Real data is not decoration. On this machine it surfaced a state no mock would
have contained: of the skills the page lists, all but the few dozen the lock
records have no source at all, so most of the list can never be checked or
updated. That changed the design. Recompute such numbers from your own run;
never copy them out of this file.

**Match the real app, do not approximate it.** The desktop app is a webview, so
the prototype can use the same web stack rather than a hand-written lookalike.
Reuse the actual oklch tokens from `crates/desktop/src/styles/theme.css`, the
real zh-Hant strings from `crates/desktop/src/lib/locales/zh-Hant.ts`, and the
same two-column frame. Hand-rolled substitutes for HeroUI v3 controls get their
heights, focus rings, disabled states and indicator glyphs wrong, and the user
ends up judging your CSS instead of the design.

**Make it operable, not a picture.** Clicking, switching scope, searching and
filtering should work against the real data; mutations can be simulated in page
memory as long as the page says so. Add a fake address bar showing the URL
state — it is how the user sees that scope and selection actually live in the
URL.

## 3. Hand it over so they can operate it

**Publish it as an Artifact and give them that link.** They can open it
anywhere, and republishing the same file path keeps the same URL, so the
feedback loop costs nothing. Do not stand up a LAN server unless they ask.

One constraint shapes the whole page: an Artifact **cannot make network requests
at all** — fetch, XHR and WebSocket are blocked to every host, localhost
included. The data has to be baked into the file. That is also why the prototype
is one self-contained file rather than the dev server.

**Put the diff inside the page.** A "what's different from today" panel, one
line per change, behind a corner button. This is the highest-value part: the
user is comparing against a page they already know, and without it they judge
the new page in isolation and you both leave with different pictures of what was
agreed. State in the same panel **what is real data and what is simulated** —
which buttons only write to page memory. A user who thinks a simulated install
was real will report a bug that does not exist.

**Ask for the disagreement, not for approval.** 「看看有沒有哪裡怪」gets the
real objection;「這樣可以嗎」gets a yes. Expect a screenshot cropped around the
offending area — that is the highest-signal feedback there is, so treat it as a
defect report. Fix, republish to the same URL, tell them to refresh. Two or
three rounds is normal and each costs minutes.

## 4. Spec from the approved prototype

Only once they say it is right, write `.scratch/<feature>/spec.md` (a tracked
convention here — several already exist). What makes it followable:

- **Why**, with `file:line` for each structural problem from step 1.
- **Target behavior** as tables, not prose: URL params and their rules, what
  each column shows, panel priority.
- **Explicit non-goals**, naming the files that must NOT change. An implementer
  with a refactor in reach will take it otherwise.
- **A fixed decision where two designs are defensible.** "Keep the existing
  variant as the default and add a compact one" beats "split it into two
  layers", which invites a rewrite of a page you said not to touch.
- **Acceptance scenarios carrying the real numbers from step 2** — "the project
  scope shows N skills still on the old layout, global shows M", with N and M
  filled from that run. These become the scenarios `verify-desktop-ui` executes,
  so write them as things you can observe: the URL after a click, what the panel
  shows, a count on screen.
- Link the published prototype, so the implementer sees the artifact the user
  approved rather than your prose summary of it.

## 5. Hand off and verify

The implementer gets the spec path, the prototype link, and
`crates/desktop/AGENTS.md` (which routes HeroUI v3 to the `heroui-react` skill).
Then verify the result yourself with `verify-desktop-ui` — the acceptance
scenarios are already written, so this is mostly running them.

Ask for the numbers back, not a summary: gates run, scenarios observed. "I
confirmed by reading the code path" is the answer that hid a blank list here.
