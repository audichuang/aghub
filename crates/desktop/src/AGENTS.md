# DESKTOP FRONTEND KNOWLEDGE BASE

**Scope**: the React frontend (`crates/desktop/src/`). For build/Tauri/HeroUI **policy**, package manager (`bun`), and the Rust `src-tauri` backend, see the desktop root [`../AGENTS.md`](../AGENTS.md). This file is the frontend-internals map.

**Stack**: React 19 + TypeScript + HeroUI v3 + Tailwind v4 + TanStack Query.

## STRUCTURE

```
src/
├── main.tsx / App.tsx   # Entry + top-level routes/providers
├── generated/dto/       # ts-rs DTOs generated from the Rust API — DO NOT hand-edit
├── requests/            # Data-access layer: per-domain React Query options + mutations
├── lib/                 # Pure helpers (e.g. getMcpMergeKey, install-layout, grouping)
├── hooks/               # Reusable stateful logic
├── contexts/            # Cross-tree state (e.g. agent-availability)
├── providers/           # App-level providers (query client, store, theme)
├── pages/               # Route screens
├── components/          # Reusable UI
├── layouts/ · styles/ · assets/
```

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
- **HeroUI v3 ≠ v2.** Always search the v3 docs before any UI work (see the root CLAUDE.md HeroUI policy + the repo `.heroui-docs/`).
- Grouping/merge utilities live in `lib/` (e.g. `getMcpMergeKey`) — search there before re-implementing a Map-reduce in a page.
- **Pure logic gets a colocated `*.test.ts`** (node:test, run via `bun run test`). When you add a pure helper to `lib/` (or a pure type-contract next to a component), add the test beside it — CI runs the whole `src/**/*.test.ts` glob.
