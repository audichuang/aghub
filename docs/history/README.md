# Incident history

Why a rule in the code exists: what used to happen, the bug or review finding
that produced the rule, and the test that now pins it. Code comments state the
current rule and link here (`See docs/history/<file>.md#<anchor>`); the story
lives in this directory so the comment stays short.

- One file per area (`core-<area>.md`, `<crate>.md`, `desktop-frontend.md`,
  `release.md`). Each entry is a `## <title>` heading — that heading is the
  anchor code links to, so renaming it breaks links; grep for the anchor first.
- Entries are history: they are not rewritten when the code changes. If a rule
  is removed, delete the code comment's link, not the entry.
- Current-state truth is the code; the load-bearing decisions are in
  `docs/adr/`, and the end-to-end reasoning is in the Hindsight knowledge pages.
