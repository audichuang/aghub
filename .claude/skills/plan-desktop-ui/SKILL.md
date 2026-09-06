---
name: plan-desktop-ui
description: Plan a change to aghub's desktop UI before writing any of it — analyse what the current page actually does, build a clickable single-file prototype fed with the user's REAL local data, get it approved, then turn it into a spec an implementer can follow. Use when the user says a desktop page feels wrong, cluttered, confusing or "something is off", when redesigning or restructuring a page, when adding a screen, or when a UI change is big enough that guessing wrong is expensive. Not for a one-line style tweak.
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
aghub-cli check skills --online -g --json      # real update / uncheckable states
aghub-cli source diff <owner/repo> -g --json   # per-source install state
aghub-cli repair -g --json                     # migration preview counts
jq . ~/.agents/.skill-lock.json                # global lock
```

Shape it into one JSON blob and inline it into a single HTML file, replacing
every `<` with its `\u003c` escape first — one skill description containing a
closing script tag ends the script block early and the page dies silently.

Real data is not decoration. Here it surfaced three things a mock would have
hidden: 1187 skills with no lock entry (94% of the list), a source whose whole
diff was "needs credential", and a token estimate that showed Claude Code
silently dropping 28 skill descriptions. The user's decisions changed because of
all three.

**Steal the app's own look** so the judgement is about structure, not paint:
copy the oklch tokens out of `crates/desktop/src/styles/theme.css`, keep the
sidebar and two-column frame, use the real zh-Hant strings from
`src/lib/locales/zh-Hant.ts`.

**Make it operable, not a picture.** Clicking, switching scope, searching and
filtering should work against the real data; mutations can be simulated in page
memory as long as you say so. Add a fake address bar showing the URL state — it
is how the user sees that scope and selection actually live in the URL.

Publish it so they can click it on any device (an Artifact, or a static server
on the LAN). Ask what is wrong, change it, republish. Cheap loops here are worth
far more than a careful first draft.

## 3. Spec from the approved prototype

Once they approve, write `.scratch/<feature>/spec.md`. What makes it followable:

- **Why**, with `file:line` for each structural problem from step 1.
- **Target behavior** as tables, not prose: URL params and their rules, what
  each column shows, panel priority.
- **Explicit non-goals**, naming the files that must NOT change. An implementer
  with a refactor in reach will take it otherwise.
- **A fixed decision where two designs are defensible.** "Keep the existing
  variant as the default and add a compact one" beats "split it into two
  layers", which invites a rewrite of a page you said not to touch.
- **Acceptance scenarios with real expected numbers** from step 2's data (this
  project shows 22 legacy skills, global shows 42). These become the scenarios
  the `verify-desktop-ui` harness runs, so write them as things you can observe:
  URL after a click, what the panel shows, a count on screen.
- Escape `|` inside markdown table cells as `\|`, or the table silently breaks.

## 4. Hand off and verify

Implementer gets the spec path, the prototype path, `crates/desktop/AGENTS.md`
and the reminder that HeroUI v3 must not be written from memory. Then verify the
result yourself with `verify-desktop-ui` — the acceptance scenarios are already
written, so this is mostly running them.

Ask for the numbers back, not a summary: gates run, scenarios observed. "I
confirmed by reading the code path" is the answer that hid a blank list here.
