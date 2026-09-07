# DESKTOP CRATE KNOWLEDGE BASE

**Directory**: `crates/desktop` — Tauri v2 desktop app (bun frontend +
`src-tauri` Rust crate; there is no `aghub-desktop` crate)\
**Stack**: React 19 + TypeScript + HeroUI v3 + Tailwind CSS v4

## STRUCTURE

Role map (not a full tree):

- `src/` — React frontend (`pages/`, `components/`, `lib/`, generated DTOs under `src/generated/`)
- `src-tauri/` — Tauri backend; Cargo package name is **`aghub`** (`-p aghub` builds this, not the CLI). Commands under `commands/` (credentials/logging/remote/server/window). Embeds `aghub-api` on localhost; uses `aghub-remote` for SSH — does **not** depend on `aghub-core` or `aghub-inference` directly; it goes through `aghub-api`, which re-exports the shared app data root (`default_app_data_dir`) and the inference db's file NAME (`INFERENCE_PROVIDERS_FILE`, used only to spell the legacy-db hint's paths). It never opens that db: the provider list comes over HTTP from the embedded API. (The credential wrappers it also imports — `resolve_git_token_for_source` / `list_bound_sources` — DO read the OS keyring in-process.)
- `src-tauri/capabilities/` — permission manifests (read `default.json`; don't restate the list here)

Frontend form patterns: `project-form-patterns` in the universal Master
(`.agents/skills/` — not auto-registered in Claude Code) + `src/AGENTS.md` for
page-level notes.

## CRITICAL: HEROUI V3

**STOP**: what you remember about HeroUI React is WRONG for v3 — never write a
component from memory. The source of truth is the live v3 docs
(`https://v3.heroui.com/docs/react/…mdx`); the project skill **`heroui-react`**
covers fetching them plus the compound-component patterns. A local
`.heroui-docs/react/` cache may exist (gitignored, usually **absent** — do not
assume it is there); when it is missing, fetch, never fall back on memory.
Project increment: stable `@heroui/react` ^3.x — no Provider, compound
components, Tailwind v4.

## GOLDEN RULE

After changing a Rust API DTO, run `bun run generate:dto` — `src/generated/` is
codegen and a stale checkout ships a lying TypeScript contract. Everything else
(`dev`/`start`/`build`/`test`/`typecheck`) is in `package.json`; **bun only**.

## ANTI-PATTERNS

### HeroUI

- Use the **secondary** variant in `Modal` / `Card` — better contrast
- **Control components (`Checkbox`, `Switch`, `Radio`, …) render a `label`/`inline-flex gap-3` ROOT that is WIDER than the visible control** (e.g. Checkbox control is `size-4`/16px but a bare `<Checkbox aria-label>` root renders ~35px — it reserves a label slot). In a hand-built flex/grid row this silently throws off column/right-edge alignment. Fix: pin the root width (`className="size-4 shrink-0"`) AND render the compound children so no label slot is reserved (`<Checkbox.Control><Checkbox.Indicator/></Checkbox.Control>`; for Switch keep `<Switch.Control><Switch.Thumb/></Switch.Control>` and pin `w-10`).
- **Debugging row alignment**: when a custom-laid-out row "should align by the padding/`px-` math but visibly doesn't," suspect a HeroUI control's root-wrapper width FIRST (read the component's `node_modules/@heroui/styles/dist/components/<name>.css`); don't keep re-flexing the container. The visible control ≠ its layout box.

### Frontend

- NEVER use string template for className concat — use `cn` from `@/lib/utils`
- NEVER use `useEffect` for data fetching or to sync state — `useQuery` /
  `useMemo` / `handleXXX` instead
- NEVER surface errors as `{error && <div>{error.message}</div>}` — use HeroUI's
  toast system.
    - **Exception: a persistent cached-data warning.** A toast is for a transient
      event (e.g. a mutation just failed) — it disappears on its own, so it is
      the wrong tool for "a background refetch failed and the rows on screen may
      be stale," which stays true until the user acts. That case gets an inline
      banner instead (e.g. `pages/settings/integrations-panel.tsx`'s credentials
      list), and the banner MUST carry `role="alert"` + `aria-live="polite"` (or
      `"assertive"`) so it is announced without requiring focus to land on it —
      an inline error `<div>` with no live-region semantics is silent to a
      screen reader.
- **`setLocation` does not move nuqs state on the SAME route.** The app pairs
  wouter with `nuqs/adapters/react`: nuqs re-reads the query string on
  `popstate` and on its own emitter, while wouter's navigate dispatches a custom
  `pushState` event. Navigate to a different pathname and the page remounts, so
  it works; navigate to the page you are already on and the address bar changes
  while every `useQueryState` keeps its old value — the URL and the screen
  disagree, which is worse than a dead button. A component that may be rendered
  by the page owning those params (e.g. `SkillDetail` inside
  `pages/settings/skills.tsx`) takes a callback and lets that page use its own
  nuqs setter; `setLocation` stays as the cross-route fallback.
- **Several ListBoxes sharing one selection must each receive only their own
  keys.** Hand a per-group `ListBox` the global `selectedKeys` and React Aria
  echoes the other groups' names back on every toggle, so the handler reads one
  of THOSE as the row that was clicked — clicking a row in one group selects a
  row in another. `useMultiSelect`'s `createGroupedSelectionHandler` is
  necessary but NOT sufficient: also pass the intersection
  (`plugin-list.tsx` and `skill-list.tsx` both show the shape). A single ListBox
  owning the whole list keeps the plain `createSelectionHandler`.

### Desktop Integration

- NEVER modify Tauri capabilities without security review
- NEVER expose system APIs without explicit permissions in `capabilities/`
- NEVER do blocking I/O in a sync `#[tauri::command]` — it runs on the main
  thread and freezes the UI (beachball). Make the command `async fn` and wrap
  blocking work in `spawn_blocking` (worked example: `commands/remote.rs`)
