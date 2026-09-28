# Incident history

Why a rule in the code exists: what used to happen, and the bug or review
finding that produced the rule. Code comments state the current rule and link
here (`See docs/history/<file>.md#<anchor>`); the story lives in this directory
so the comment stays short.

- One file per area (`core-<area>.md`, `<crate>.md`, `desktop-frontend.md`,
  `release.md`). Each entry is a `## <title>` heading — that heading is the
  anchor code links to, so renaming it breaks links; grep for the anchor first.
- An entry includes `Pinned by: <test>` when a test pins the rule; not every
  entry has one.
- Entries are history: they are not rewritten when the code changes. If a rule
  is removed, delete the code comment's link, not the entry.
- `node scripts/check-history-links.mjs` (repo root) fails on a link to a
  missing file or heading, and on a duplicate heading within one file.
- Current-state truth is the code; the load-bearing decisions are in
  `docs/adr/`, and the end-to-end reasoning is in the Hindsight knowledge pages.
  Per-agent descriptor research (vendor paths, binary forensics) lives in the
  sibling [`docs/descriptors/`](../descriptors/).

## Index

- [agents.md](agents.md) — `crates/agents`: descriptors, roster, MCP `format/`
- [api.md](api.md) — `crates/api`: skill routes, credentials
- [cli.md](cli.md) — `crates/cli`: command surface
- [core-install-linker.md](core-install-linker.md) — `crates/core/src/skills`:
  `install_fetched`, `linker/`, `prune`
- [core-manager.md](core-manager.md) — `crates/core/src/manager`
- [core-removal.md](core-removal.md) — `crates/core`: `skills/removal`,
  `skills/resync`, `dto/removal`
- [core-repair-rename.md](core-repair-rename.md) — `crates/core/src/skills`:
  `repair`, `rename`, `discovery`
- [core-skills-shape.md](core-skills-shape.md) — `crates/core/src/skills/shape`
- [core-transfer.md](core-transfer.md) — `crates/core/src/transfer.rs`
- [desktop-frontend.md](desktop-frontend.md) — `crates/desktop/src`
- [desktop-tauri.md](desktop-tauri.md) — `crates/desktop/src-tauri`
- [inference.md](inference.md) — `crates/inference`
- [release.md](release.md) — `.github/workflows/release.yml`
- [remote.md](remote.md) — `crates/remote`
- [skill.md](skill.md) — `crates/skill` (lock I/O)
- [skill-update.md](skill-update.md) — `crates/skill-update`
- [tooling.md](tooling.md) — `justfile`, cargo, local environment
