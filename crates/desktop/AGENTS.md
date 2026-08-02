# DESKTOP CRATE KNOWLEDGE BASE

**Directory**: `crates/desktop` — Tauri v2 desktop app (bun frontend +
`src-tauri` Rust crate; there is no `aghub-desktop` crate)\
**Stack**: React 19 + TypeScript + HeroUI v3 + Tailwind CSS v4

## STRUCTURE

Role map (not a full tree):

- `src/` — React frontend (`pages/`, `components/`, `lib/`, generated DTOs under `src/generated/`)
- `src-tauri/` — Tauri backend; Cargo package name is **`aghub`** (`-p aghub` builds this, not the CLI). Commands under `commands/` (credentials/logging/remote/server/window). Embeds `aghub-api` on localhost; uses `aghub-remote` for SSH — does **not** depend on `aghub-core` directly.
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

### Desktop Integration

- NEVER modify Tauri capabilities without security review
- NEVER expose system APIs without explicit permissions in `capabilities/`
- NEVER do blocking I/O in a sync `#[tauri::command]` — it runs on the main
  thread and freezes the UI (beachball). Make the command `async fn` and wrap
  blocking work in `spawn_blocking` (worked example: `commands/remote.rs`)
