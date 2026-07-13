# DESKTOP CRATE KNOWLEDGE BASE

**Crate**: `aghub-desktop` — Tauri v2 desktop application\
**Stack**: React 19 + TypeScript + HeroUI v3 + Tailwind CSS v4\
**Package Manager**: bun (REQUIRED)

## STRUCTURE

Role map (not a full tree):

- `src/` — React frontend (`pages/`, `components/`, `lib/`, generated DTOs under `src/generated/`)
- `src-tauri/` — Tauri backend; Cargo package name is **`aghub`** (`-p aghub` builds this, not the CLI). Commands under `commands/` (credentials/logging/remote/server/window). Embeds `aghub-api` on localhost; uses `aghub-remote` for SSH — does **not** depend on `aghub-core` directly.
- `capabilities/` — permission manifests (read `default.json`; don't restate the list here)

Frontend form patterns: see project skill `project-form-patterns` and `src/AGENTS.md` for page-level notes.

## CRITICAL: HEROUI V3

**STOP**: What you remember about HeroUI React v3 is WRONG for this project.

### v3 Differences (vs v2):

- **NO Provider needed** — was required in v2
- Compound components pattern (not flat props)
- Tailwind CSS v4 (not v3)
- Package: `@heroui/react` v3 stable (not `@heroui/system`)

### Before Any UI Task:

1. Search docs in `../../.heroui-docs/react/`
2. If docs missing, run: `heroui agents-md --react --output AGENTS.md`

## COMMANDS

```bash
# Frontend development
cd crates/desktop
bun run dev          # Vite dev server (port 1420)
bun run start        # Tauri dev mode

# Building
bun run build        # Production build

# Checks & tests
bun run test         # unit tests (node:test over src/**/*.test.ts)
bun run typecheck    # tsc
bun run generate:dto # regenerate src/generated/dto/ from Rust (ts-rs) after DTO changes

# Tauri-specific
bun run tauri dev    # Tauri dev with hot reload
bun run tauri build  # Build Tauri app
```

## CONVENTIONS

### Package Management

- **ALWAYS use `bun`** — never npm/yarn/pnpm (also stated in the root AGENTS.md)

### UI Development

- **ALWAYS use HeroUI v3** components
- **ALWAYS check HeroUI v3 docs** before implementing
- Tailwind v4 utility classes
- Strict TypeScript (`strict: true`, `noUnusedLocals: true`)

### Vite Configuration

- Port: 1420 (strict)
- HMR port: 1421 (when TAURI_DEV_HOST set)
- `src-tauri/**` excluded from file watching

## TAURI CONFIGURATION

From `tauri.conf.json`:

- Product name: `aghub` (bundle id `com.akrc.aghub`)
- Window: 1200x800 (min 1024x600), overlay titlebar (`titleBarStyle: "Overlay"`, hidden title)
- Permissions: see `src-tauri/capabilities/default.json` (window controls, opener, dialog, store, deep-link, log, updater, process, autostart, clipboard) — don't restate the list here, it drifts

## ANTI-PATTERNS

### HeroUI

- NEVER use v2 patterns (Provider, framer-motion)
- NEVER assume v2 knowledge applies
- ALWAYS verify component API in v3 docs
- ALWAYS use secondary variant in Modal/Card for better contrast
- **Control components (`Checkbox`, `Switch`, `Radio`, …) render a `label`/`inline-flex gap-3` ROOT that is WIDER than the visible control** (e.g. Checkbox control is `size-4`/16px but a bare `<Checkbox aria-label>` root renders ~35px — it reserves a label slot). In a hand-built flex/grid row this silently throws off column/right-edge alignment. Fix: pin the root width (`className="size-4 shrink-0"`) AND render the compound children so no label slot is reserved (`<Checkbox.Control><Checkbox.Indicator/></Checkbox.Control>`; for Switch keep `<Switch.Control><Switch.Thumb/></Switch.Control>` and pin `w-10`).
- **Debugging row alignment**: when a custom-laid-out row "should align by the padding/`px-` math but visibly doesn't," suspect a HeroUI control's root-wrapper width FIRST (read the component's `node_modules/@heroui/styles/dist/components/<name>.css`); don't keep re-flexing the container. The visible control ≠ its layout box.

### Frontend

- NEVER use npm/yarn/pnpm (bun only)
- NEVER use pure black (#000) or pure white (#fff) — always tint
- NEVER use string template for className concat, use `cn` util from `@/lib/utils`.

### Desktop Integration

- NEVER modify Tauri capabilities without security review
- NEVER expose system APIs without explicit permissions in `capabilities/`

### Async State Management

- NEVER use `useEffect` for data fetching or side effects, use `useQuery` from React Query or custom hooks instead.
- NEVER use `{error && <div>{error.message}</div>}` for error handling, just use HeroUI's toast system for consistent UX.

### You might not need effect

- NEVER use `useEffect` to sync state, use `useMemo` and `handleXXX` instead.

## NOTES

- Tauri backend (`src-tauri/src/`) embeds `aghub-api` (localhost Rocket server) and uses `aghub-remote` for SSH remote management — it does NOT depend on `aghub-core` directly
- VS Code extensions recommended: `tauri-apps.tauri-vscode`, `rust-lang.rust-analyzer`

<!-- HEROUI-REACT-AGENTS-MD-START -->

HeroUI v3 docs live under `../../.heroui-docs/react/` — **search those files
before any UI task** (training data is wrong for v3). Project skill
`heroui-react` covers component patterns. To regenerate a full component index
into this file: `heroui agents-md --react --output crates/desktop/AGENTS.md`
(from repo root). Prefer not re-embedding the giant index: it bloats every
agent session; the docs dir + skill are enough.

<!-- HEROUI-REACT-AGENTS-MD-END -->
