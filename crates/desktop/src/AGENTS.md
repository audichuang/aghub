# DESKTOP FRONTEND KNOWLEDGE BASE

**Scope**: the React frontend (`crates/desktop/src/`). For build/Tauri/HeroUI **policy**, package manager (`bun`), and the Rust `src-tauri` backend, see the desktop root [`../AGENTS.md`](../AGENTS.md). This file is the frontend-internals map.

**Stack**: React 19 + TypeScript + HeroUI v3 + Tailwind v4 + TanStack Query.

## STRUCTURE

Role map:

- `generated/dto/` — ts-rs from Rust API; **never hand-edit**
- `requests/` — single data-access seam (query/mutation options + invalidate helpers)
- `lib/` — helpers (mostly pure; also the api client, i18n, and `store/`
  persist); `hooks/` / `contexts/` / `providers/` — React wiring
  (`providers/` is connection/theme/agent-availability context — **not** QueryClient;
  QueryClient lives in `App.tsx`; persist store under `lib/store`)
- `pages/` · `components/` · `layouts/` · `styles/` · `assets/`

## WHERE TO LOOK

| Task                         | Location                                                                     |
| ---------------------------- | ---------------------------------------------------------------------------- |
| Call the API / cache a query | `requests/<domain>.ts` (queryOptions + mutationOptions + invalidate helpers) |
| API response/request shapes  | `generated/` (regenerate from Rust; never edit by hand)                      |
| Shared transform/util        | `lib/` (check before writing a new one)                                      |
| Add a screen                 | `pages/` + a route in `App.tsx`                                              |

## GOTCHAS / ANTI-PATTERNS

- **`generated/` is codegen (ts-rs from the Rust API).** Never hand-edit; change the Rust type + regenerate. Pages consume these DTOs directly — that coupling is intentional, not a layer to wrap.
- **`requests/` is the single data-access seam.** Reuse its `queryOptions` / `mutationOptions` and the `invalidate*Queries` helpers (e.g. `invalidateSkillQueries`) — don't hand-roll `useQuery` keys per page or duplicate optimistic-cache logic.
- Grouping/merge and bulk plan/diff utilities live in `lib/` (e.g.
  `getMcpMergeKey`, `group-agent-plan.ts`) — search there before
  re-implementing a Map-reduce or bulk-diff in a page.
- **`!isLoading` is not "the data is trustworthy".** A failed query settles with
  `data` undefined, which every `?? []` / `?? {}` turns into "the server says
  there is nothing" — indistinguishable from a real empty answer. Anything that
  PERSISTS a decision from query data (a store write, a seed, a "handled" flag)
  must gate on `isSuccess`; a transient failure otherwise records an empty world
  as fact and the recovery looks like a first run.
- **Tauri store: `set()` mutates memory, only `save()` reaches disk.** A failed
  `save()` leaves the new value in the store's in-memory map, so the next
  `save()` from ANYWHERE else in the app (autostart toggle, a settings field)
  flushes the edit that just failed, and a re-read hands back the unpersisted
  value. Snapshot the previous value and restore it in the catch — do not
  recover by re-reading.
- **Pure logic gets a colocated `*.test.ts`** (node:test, run via `bun run test`). When you add a pure helper to `lib/` (or a pure type-contract next to a component), add the test beside it — CI runs the whole `src/**/*.test.ts` glob.
