# desktop-frontend history

Incident history moved out of `crates/desktop/src` code comments. The code
keeps the current rule; each entry here keeps what happened and why.

## MCP search matched nothing

`lib/mcp-search.ts` — commit `b79aeeb9`.

The MCP list searched with Fuse keys `items.0.name` / `items.0.source` /
`items.0.agent`. Fuse splits a key on `.` and, on reaching an array, walks
EVERY element; the literal `0` is then looked up as a property of each element
object and is always `undefined`. So the MCP search box emptied the list on the
first keystroke and never found a server. A QA pass that only searched for a
string nothing should match read that as correct, which is how it survived.

The bug surfaced while extracting the search into a shared seam. That seam
exists because the MCP page had no "open server is outside the results" banner:
the list said "no matching servers" while the right panel kept a fully operable
edit/delete/duplicate surface for a server outside the results. The banner and
the list filter must agree, so they share one options object (the same reason
`skill-search.ts` exists for skills).

Rule: keys are `items.<field>`; walking every member is intended (a merged
group's members differ by agent). Pinned by `lib/mcp-search.test.ts`: "a query
finds an agent that is not the group's first member", "the list filter and the
open-server check agree on every query".

## Preference fallback written back over the stored value

`lib/preference-write.ts` — commit `5971f416`.

`useQuery` hands a failed read back as `data: undefined`, and three preference
hooks substituted a render fallback (`DEFAULT_SIDEBAR_ITEMS`, `[]` for stars,
"the first installed editor"). The next write took that fallback as the user's
current state and saved it: toggling one sidebar item after a failed read wrote
the defaults back over the user's hidden/reordered list; starring one skill
wrote a one-element array over every other star.

Rule: a write's basis is what was actually read (`preferenceWriteBasis`);
`null` means write nothing. The rollback half (a failed `save()` leaves the
value in the Tauri store's memory) is the `src/AGENTS.md` rule "Tauri store:
`set()` mutates memory, only `save()` reaches disk". Pinned by
`lib/preference-write.test.ts`: "a read that did not succeed has no basis,
whatever is cached", "a failed save restores the cache AND rewrites the
previous value".

## Update downloaded twice after switching pages

`lib/app-update.ts` — commit `5570bf46`.

The About panel's update mutations were local to the component, so switching
pages destroyed the observer while the download carried on. Coming back reset
the UI to "check for updates", and pressing it started a SECOND download of the
same update.

Rule: the update phase lives in a provider that never unmounts, and
`canStartUpdateWork` is the one place that decides whether work may start.
Pinned by `lib/app-update.test.ts`: "work cannot start while a check or
download is already running", "work cannot start once an update is installed".

## Migration preview conflated moves and links

`lib/skill-migration.ts` `migrationSummary` — commit `f0536c33`.

A fifty-skill preview repeated the store path and the fused-agent sentence
fifty times, burying the two per-skill facts (the name, and whether it was
refused); those scope-wide facts were hoisted into one sentence. Separately,
`migrating` and `linking` used to be one number, which read as "your skills are
about to move" even when only Referrers were being created or repointed at a
Master that never moved. One level down, a single shared `totalLinks` made the
move sentence claim links that a link-only row contributed; the link counts are
now split per bucket (`migratingLinks` / `linkingLinks`).

Pinned by `lib/skill-migration.test.ts`: "a relinked row is link work, not a
content move", "a reconciled row is link work too", "an already-migrated store
that only lacks two private slots moves nothing".

## Tidy-only repair toasted as migrated

`lib/skill-migration.ts` `migrationToastMessage` — commit `19c5ff64`.

The toast used to be `result.skills.length` as "migrated". A commit that only
detached stale Referrers migrated NOTHING, so it made exactly that false claim.
Once the dialog stopped auto-closing, "Run again" stayed clickable after a
clean commit, and a bulk re-run returns `skills: []`; that case needed its own
"nothing left" key rather than `skillLayoutMigrated` with `count: 0`.

Pinned by `lib/skill-migration.test.ts`: "a pure-tidied COMMIT toasts as
tidied, never as migrated", "a commit that changed nothing toasts as nothing
left, not zero migrated".

## Repair re-run widened to a bulk repair

`lib/skill-migration.ts` `repairScope` and
`components/skill-layout-migration-banner.tsx` — commit `19c5ff64`.

`names: undefined` is a BULK repair (every skill the lock names at the scope).
The button's scope silently widened to that in two ways:

- **Basis.** The selectable set was derived from `shown`, which is the last
  run's RESULT once there is one. Preview `[a, b]`, the user unchecks `b`, the
  commit posts `names: [a]`; afterwards `shown === result.skills === [a]` made
  `pickedNames.length === selectable.length` true, so "Run again" posted no
  names at all and migrated the row the user had explicitly deselected.
- **Empty.** `0 === 0` also read as "everything", so a press with nothing
  checked posted a bulk repair.

Rule: the scope basis is the live preview; the last run's rows are only
subtracted; empty stays empty and the caller disables the button. Pinned by
`lib/skill-migration.test.ts`: "repairScope: the last run's rows must never
become the scope basis", "repairScope: nothing outstanding is not the same as
everything", "repairScope: no selection narrows to nothing; a full one is one
bulk call".

## Skills page stacked three banners

`components/skill-status-strip.tsx` — commit `efb446be`.

`pages/settings/skills.tsx` used to stack three separate banners
(update-all, background-check, layout-migration). They collapsed into one
status strip where each true fact gets one row and the container hides itself
via `:empty` when nothing is left.
