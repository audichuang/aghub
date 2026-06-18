# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Aghub manages AI coding agent configurations across **23 agents** (Claude, OpenCode, Cursor, Windsurf, Copilot, RooCode, Cline, Gemini, Codex, Zed, Warp, and more), handling MCP servers, skills, and sub-agents through a unified interface. It is delivered through three surfaces: a CLI (`aghub-cli`), a Rocket HTTP API (`aghub-api`), and a Tauri desktop app (`crates/desktop`). It also manages inference providers, Claude Code plugins, and SSH-based remote deployment. Full agent list: the `AgentType` enum in `crates/agents/src/models.rs` (NOT `crates/core`) or `aghub-cli --help`.

> **Features in flight:** see `docs/specs/` for active designs — e.g. `2026-06-02-sources-and-universal-install.md` (the "Sources" management page + `.agents`-symlink "universal" install mode) and `2026-05-31-skill-management-improvements.md`.

> **Domain language & decisions:** [`CONTEXT.md`](CONTEXT.md) is the glossary (Source hash, Master, Referrer, Relink, …); [`docs/adr/`](docs/adr/) records load-bearing decisions (e.g. transactional skill rename). Deep, reusable knowledge lives in project skills under `.claude/skills/` — `aghub-skills` (skill subsystem invariants), `npx-skills-contract` (npx round-trip contract), `testing-fs-failures` (forcing fs failures in tests), `releasing-aghub` (tag-driven release runbook + troubleshooting). Per-crate `CLAUDE.md` files are one-line `@AGENTS.md` imports of that crate's `AGENTS.md` (not symlinks).

## Common Commands

Use `just` for task running:

```bash
# Build
just dev          # Debug build
just build        # Release build

# Test
just test         # Run all tests
just integration-test  # Run integration tests only
just test-with-validation  # Tests requiring real CLI tools installed (claude, opencode, etc.)

# Lint/Format
just lint         # Run clippy with warnings as errors
just fmt          # Format code

# Run
just start -- --help                    # Run with cargo
just start -- -a claude get skills      # List skills
just start -- -a claude get mcps        # List MCP servers
just install                            # Release build → ~/.cargo/bin/
```

Run a single test: `cargo test --package aghub-core test_name -- --exact`

## Architecture

### Workspace Structure

The workspace has **13 Rust member crates** (root `Cargo.toml`, version `1.1.1`) plus the desktop frontend. Dependency direction: `agents` → `core` → `cli`/`api`/`desktop`; the tool crates are used laterally.

- **`crates/agents`** (`aghub-agents`): **The single source of truth for agent-specific behavior** — `AgentDescriptor` constants, the `AgentType` enum + normalized models (`AgentConfig`, `Skill`, `McpServer`), and the `format/` serializers. `aghub-core` re-exports this crate.
- **`crates/core`** (`aghub-core`): Orchestration layer — re-exports `aghub-agents` and adds `ConfigManager`, the `registry`, adapter dispatch (`adapter.rs`/`adapters/`), skills discovery, and cross-agent `transfer`.
- **`crates/cli`** (`aghub`): CLI binary (`aghub-cli`) with clap-based commands
- **`crates/api`** (`aghub-api`): Rocket v0.5 HTTP API server exposing agent config operations over HTTP (~85 routes under `/api/v1/`)
- **`crates/desktop`** (`aghub-desktop`): Tauri v2 app — React 19 + TypeScript + HeroUI v3 + Tailwind v4 (package manager: **bun**); `src-tauri/` is the Rust backend, embeds `aghub-api` on localhost. See `crates/desktop/CLAUDE.md`.
- **`crates/skill`** (`skill`): `.skill`/zip packaging + npx-compatible lock files (global `~/.agents/.skill-lock.json` v3, project `skills-lock.json` v1); `parse()` auto-detects directory/zip/SKILL.md; `discover_repo_skills`, content hashing. Called by `manager.add_skill_from_path()`.
- **`crates/skills-sh`** (`skills-sh`): HTTP client for the skills.sh registry (`SKILLS_API_URL` env var overrides base URL); search only.
- **`crates/inference`** (`aghub-inference`): Inference-provider management — SQLite metadata + platform keyring for API keys; adapts to Claude/OpenCode/Codex.
- **`crates/remote`** (`aghub-remote`): SSH remote VM management (probe → install → start `aghub-api` → log polling), `~/.ssh/config` parsing; exposed via the desktop Tauri layer (NOT the HTTP API).
- **`crates/cc-plugins`** (`aghub-cc-plugins`): Claude Code plugin lifecycle (install/marketplace).
- **`crates/git`** (`aghub-git`): git clone/fetch with credential injection (`fetch_ref_to_temp`, `resolve_remote_source`, `materialize_tree`).
- **`crates/json`** (`aghub-json`), **`crates/markdown`** (`aghub-markdown`): JSON/JSONC editing + YAML frontmatter parsing helpers.

> `skills-ref` is an **external git dependency** (`AkaraChen/skills-ref`), not a local crate.

### Key Design Patterns

**Adapter Pattern**: `AgentAdapter` trait in `crates/core/src/adapters/mod.rs`. All agents are dispatched through `create_adapter(agent_type)` → `registry::get(agent_type)` → `&'static AgentDescriptor`, which implements `AgentAdapter` via `adapter.rs`. There are no hand-wired adapter structs — behavior is entirely driven by function pointers on each descriptor.

**Normalized Model**: `AgentConfig` in `models.rs` provides a unified representation with:

- `Vec<Skill>`: Skills with frontmatter metadata (name, description, author, version, tools)
- `Vec<McpServer>`: MCP servers with `McpTransport` (Stdio, Sse, StreamableHttp variants)

**ConfigManager**: Central abstraction in `manager.rs` that coordinates adapter operations and provides CRUD methods for resources.

### Agent-Specific Behavior

Agent behavior is defined entirely in `crates/agents/src/agents/<name>.rs` descriptor constants (NOT in `crates/core`). Key notes:

- **Claude**: skills are NOT stored in JSON; discovered from `~/.claude/skills/` SKILL.md files. URL-based MCPs silently skipped on serialize.
- **OpenCode**: uses native format with `mcp` object key (not `mcp_servers` array). SSE and StreamableHttp transports are unified as `"type": "remote"` — SSE transport identity is lost on roundtrip. Reads skills from its own dir (`.opencode/skills` project / `~/.config/opencode/skills` global) **plus** the universal `.agents/skills` master — never another agent's private dir.
- **Codex/Mistral**: TOML config format.
- **Universal-master reads** (`.agents/skills`): an agent reads the universal Master only where its descriptor maps that scope's skills dir to `.agents/skills` — this is **per-agent and per-scope, not a blanket rule**. At **project** scope, `<root>/.agents/skills` is read by the npx-interop set: Codex, OpenCode, Cursor, Cline, Copilot, Gemini, Antigravity, Amp, Kimi, Warp. At **global** scope, `~/.agents/skills` is read by a smaller subset — **Codex, OpenCode, Cursor, Cline, Warp**; the rest (Claude, Gemini, Copilot, Kiro, Windsurf, Trae, RooCode, Mistral, Pi, KiloCode, …) read only their own per-agent global dir. The load-bearing invariant this branch enforces: each agent reads ONLY its own dir + the Master where mapped, and must never read another agent's private dir (e.g. Cursor/OpenCode do **not** read `.claude/skills` or `.codex/skills`). Separately, only **Amp** and **Kimi** set `capabilities.skills.universal: true`, which additionally appends the XDG `$XDG_CONFIG_HOME/agents/skills` (default `~/.config/agents/skills`) — note that is the XDG path, **not** `~/.agents/skills` (so Amp/Kimi do not read the global `~/.agents/skills` Master).
- **Copilot**: shares `~/.claude/skills/` as its skills path (same as Claude).
- **`registry::get()` fallback**: returns Claude's descriptor silently if the requested agent ID is not found.

### CLI Command Surface

```
aghub-cli [-a <agent>] [-g|--global] [-p|--project] [--all] [-v|--verbose] <command>

Commands:
  get    <skills|mcps>               # list resources
  add    <skills|mcps>               # --name, --from PATH, --command, --url, --transport,
                                     #   --header KEY:VALUE, --env KEY=VAL, --description,
                                     #   --author, --version, --tools,
                                     #   --universal (skills: write a .agents master + symlink the
                                     #     target agent; default is an isolated copy that never touches .agents)
  update <skills|mcps> <name>        # same flags as add
  delete <skills|mcps> <name>        # --all-agents, --dry-run (default), --yes to actually remove
  enable/disable <skills|mcps> <name> # soft toggle; only meaningful for OpenCode
  describe <skills|mcps> <name>      # JSON output for a single resource (handled inline in main.rs)
  check                              # offline: list installed skills that have updates (read-only)
  apply-update                       # apply a locked skill update
  prune-lock                         # drop lock entries with no on-disk skill (dry-run by default; --yes)
  plugin <list|install|uninstall|update|enable|disable|prune|validate>  # Claude Code plugins
  plugin marketplace <add|remove|update|list>
  interactive                        # step-by-step wizard
```

Resource type aliases: `skills`/`skill`, `mcps`/`mcp`.

### Skills Discovery

Skills are loaded from directories containing `SKILL.md` files. The adapter parses YAML frontmatter (between `---` markers) to extract metadata like name, description, author, and version. The `source_path: Option<String>` field on `Skill` records the file path where the skill was loaded from.

### Adding/Removing Agents

Touch all of these when adding or removing an agent (note: descriptors live in `crates/agents`, the registry in `crates/core`):

1. `crates/agents/src/agents/<name>.rs` — create or delete the descriptor constant (`codex` is a subdirectory, not a single `.rs` file)
2. `crates/agents/src/agents/mod.rs` — add/remove `pub mod <name>;`
3. `crates/agents/src/agents/factory.rs` — add/remove the dispatch arm
4. `crates/agents/src/models.rs` — add/remove the `AgentType` enum variant, `ALL` array entry, `as_str()` arm, `from_str()` arm
5. `crates/core/src/registry/mod.rs` — add/remove `&agents::<name>::DESCRIPTOR` from `ALL_AGENTS` (the cross-crate step that's easy to miss)

### Testing

Integration tests in `crates/core/tests/integration_tests.rs` use a `TestConfig` helper to create isolated temp directories with `.claude/` or `.opencode/` structures.

For test isolation, `TestConfig` uses `crate::adapter::set_skills_path_override(agent_id, path)` (per-agent thread-local).

Additional test files:

- `crates/core/tests/mcp_tests.rs`: MCP-specific behavior, transport types, deduplication
- `crates/core/tests/test_agent_paths.rs`: XDG-compliant skills path configuration per agent
- `crates/cli/tests/cli_tests.rs`: End-to-end CLI tests via `assert_cmd`

## Release & Packaging

- Releases are **tag-driven**: pushing a `v*` tag runs `.github/workflows/release.yml` → builds desktop bundles (macOS/Windows/Linux via `tauri-action`) + CLI, generates `latest.json`, updates the Homebrew tap. No manual build/upload.
- **Version comes from the git tag** — CI `sed`s it into `Cargo.toml`, `crates/desktop/package.json`, `tauri.conf.json`. Don't hand-bump for a release; `just bump <ver>` only syncs those three manifests locally.
- **Tauri updater**: the committed `tauri.conf.json` `pubkey` must pair with the `TAURI_SIGNING_PRIVATE_KEY` repo secret, and `endpoints` must point at _this_ repo's releases. The pubkey must never change once a build ships, or installed apps can't auto-update.
- **Gotcha**: unset `APPLE_*` secrets resolve to empty strings and break the macOS build (`security import` on an empty cert). Keep them commented out in `release.yml` until real Apple certs exist — unsigned dmg builds fine otherwise.
- The Homebrew tap is a **separate repo** written via the `HOMEBREW_TAP_TOKEN` PAT (the default `GITHUB_TOKEN` can't reach it).
- `git push` is gated by a **pre-push hook**: prettier `--check` + clippy `-D warnings` + eslint + tsc.

## Configuration Paths Reference

| Agent    | Global Config                      | Project Config            | Skills Path         |
| -------- | ---------------------------------- | ------------------------- | ------------------- |
| Claude   | `~/.claude.json`                   | `.mcp.json`               | `~/.claude/skills/` |
| OpenCode | `~/.config/opencode/opencode.json` | `.opencode/settings.json` | -                   |

Project root is detected by walking up directories looking for agent markers (`.claude/`, `.opencode/`, `.cursor/`, `.mcp.json`, etc.). `.git` alone is NOT sufficient — the directory must also contain at least one agent marker.

<!-- HEROUI-REACT-AGENTS-MD-START -->

[HeroUI React v3 Docs Index]|root: ./.heroui-docs/react|STOP. What you remember about HeroUI React v3 is WRONG for this project. Always search docs and read before any task.|If docs missing, run this command first: heroui agents-md --react --output AGENTS.md|components/(buttons):{button-group.mdx,button.mdx,close-button.mdx,toggle-button-group.mdx,toggle-button.mdx}|components/(collections):{dropdown.mdx,list-box.mdx,tag-group.mdx}|components/(colors):{color-area.mdx,color-field.mdx,color-picker.mdx,color-slider.mdx,color-swatch-picker.mdx,color-swatch.mdx}|components/(controls):{slider.mdx,switch.mdx}|components/(data-display):{badge.mdx,chip.mdx,table.mdx}|components/(date-and-time):{calendar.mdx,date-field.mdx,date-picker.mdx,date-range-picker.mdx,range-calendar.mdx,time-field.mdx}|components/(feedback):{alert.mdx,meter.mdx,progress-bar.mdx,progress-circle.mdx,skeleton.mdx,spinner.mdx}|components/(forms):{checkbox-group.mdx,checkbox.mdx,description.mdx,error-message.mdx,field-error.mdx,fieldset.mdx,form.mdx,input-group.mdx,input-otp.mdx,input.mdx,label.mdx,number-field.mdx,radio-group.mdx,search-field.mdx,text-area.mdx,text-field.mdx}|components/(layout):{card.mdx,separator.mdx,surface.mdx,toolbar.mdx}|components/(media):{avatar.mdx}|components/(navigation):{accordion.mdx,breadcrumbs.mdx,disclosure-group.mdx,disclosure.mdx,link.mdx,pagination.mdx,tabs.mdx}|components/(overlays):{alert-dialog.mdx,drawer.mdx,modal.mdx,popover.mdx,toast.mdx,tooltip.mdx}|components/(pickers):{autocomplete.mdx,combo-box.mdx,select.mdx}|components/(typography):{kbd.mdx}|components/(utilities):{scroll-shadow.mdx}|getting-started/(handbook):{animation.mdx,colors.mdx,composition.mdx,styling.mdx,theming.mdx}|getting-started/(overview):{design-principles.mdx,quick-start.mdx}|getting-started/(ui-for-agents):{agent-skills.mdx,agents-md.mdx,llms-txt.mdx,mcp-server.mdx}|releases:{v3-0-0-alpha-32.mdx,v3-0-0-alpha-33.mdx,v3-0-0-alpha-34.mdx,v3-0-0-alpha-35.mdx,v3-0-0-beta-1.mdx,v3-0-0-beta-2.mdx,v3-0-0-beta-3.mdx,v3-0-0-beta-4.mdx,v3-0-0-beta-6.mdx,v3-0-0-beta-7.mdx,v3-0-0-beta-8.mdx,v3-0-0-rc-1.mdx,v3-0-0.mdx}|demos/accordion:{basic.tsx,custom-indicator.tsx,custom-render-function.tsx,custom-styles.tsx,disabled.tsx,faq.tsx,multiple.tsx,surface.tsx,without-separator.tsx}|demos/alert-dialog:{backdrop-variants.tsx,close-methods.tsx,controlled.tsx,custom-animations.tsx,custom-backdrop.tsx,custom-icon.tsx,custom-portal.tsx,custom-trigger.tsx,default.tsx,dismiss-behavior.tsx,placements.tsx,sizes.tsx,statuses.tsx,with-close-button.tsx}|demos/alert:{basic.tsx}|demos/autocomplete:{allows-empty-collection.tsx,asynchronous-filtering.tsx,controlled-open-state.tsx,controlled.tsx,custom-indicator.tsx,default.tsx,disabled.tsx,email-recipients.tsx,full-width.tsx,location-search.tsx,multiple-select.tsx,required.tsx,single-select.tsx,tag-group-selection.tsx,user-selection-multiple.tsx,user-selection.tsx,variants.tsx,with-description.tsx,with-disabled-options.tsx,with-sections.tsx}|demos/avatar:{basic.tsx,colors.tsx,custom-styles.tsx,fallback.tsx,group.tsx,sizes.tsx,variants.tsx}|demos/badge:{basic.tsx,colors.tsx,dot.tsx,placements.tsx,sizes.tsx,variants.tsx,with-content.tsx}|demos/breadcrumbs:{basic.tsx,custom-render-function.tsx,custom-separator.tsx,disabled.tsx,level-2.tsx,level-3.tsx}|demos/button-group:{basic.tsx,disabled.tsx,full-width.tsx,orientation.tsx,sizes.tsx,variants.tsx,with-icons.tsx,without-separator.tsx}|demos/button:{basic.tsx,custom-render-function.tsx,custom-variants.tsx,disabled.tsx,full-width.tsx,icon-only.tsx,loading-state.tsx,loading.tsx,outline-variant.tsx,ripple-effect.tsx,sizes.tsx,social.tsx,variants.tsx,with-icons.tsx}|demos/calendar:{basic.tsx,booking-calendar.tsx,controlled.tsx,custom-icons.tsx,custom-styles.tsx,default-value.tsx,disabled.tsx,focused-value.tsx,international-calendar.tsx,min-max-dates.tsx,multiple-months.tsx,read-only.tsx,unavailable-dates.tsx,with-indicators.tsx,year-picker.tsx}|demos/card:{default.tsx,horizontal.tsx,variants.tsx,with-avatar.tsx,with-form.tsx,with-images.tsx}|demos/checkbox-group:{basic.tsx,controlled.tsx,custom-render-function.tsx,disabled.tsx,features-and-addons.tsx,indeterminate.tsx,on-surface.tsx,validation.tsx,with-custom-indicator.tsx}|demos/checkbox:{basic.tsx,controlled.tsx,custom-indicator.tsx,custom-render-function.tsx,custom-styles.tsx,default-selected.tsx,disabled.tsx,form.tsx,full-rounded.tsx,indeterminate.tsx,invalid.tsx,render-props.tsx,variants.tsx,with-description.tsx,with-label.tsx}|demos/chip:{basic.tsx,statuses.tsx,variants.tsx,with-icon.tsx}|demos/close-button:{default.tsx,interactive.tsx,variants.tsx,with-custom-icon.tsx}|demos/color-area:{basic.tsx,controlled.tsx,custom-render-function.tsx,disabled.tsx,space-and-channels.tsx,with-dots.tsx}|demos/color-field:{basic.tsx,channel-editing.tsx,controlled.tsx,custom-render-function.tsx,disabled.tsx,form-example.tsx,full-width.tsx,invalid.tsx,on-surface.tsx,required.tsx,variants.tsx,with-description.tsx}|demos/color-picker:{basic.tsx,controlled.tsx,with-fields.tsx,with-sliders.tsx,with-swatches.tsx}|demos/color-slider:{alpha-channel.tsx,basic.tsx,channels.tsx,controlled.tsx,custom-render-function.tsx,disabled.tsx,rgb-channels.tsx,vertical.tsx}|demos/color-swatch-picker:{basic.tsx,controlled.tsx,custom-indicator.tsx,custom-render-function.tsx,default-value.tsx,disabled.tsx,sizes.tsx,stack-layout.tsx,variants.tsx}|demos/color-swatch:{accessibility.tsx,basic.tsx,custom-render-function.tsx,custom-styles.tsx,shapes.tsx,sizes.tsx,transparency.tsx}|demos/combo-box:{allows-custom-value.tsx,asynchronous-loading.tsx,controlled-input-value.tsx,controlled.tsx,custom-filtering.tsx,custom-indicator.tsx,custom-render-function.tsx,custom-value.tsx,default-selected-key.tsx,default.tsx,disabled.tsx,full-width.tsx,menu-trigger.tsx,on-surface.tsx,required.tsx,with-description.tsx,with-disabled-options.tsx,with-sections.tsx}|demos/date-field:{basic.tsx,controlled.tsx,custom-render-function.tsx,disabled.tsx,form-example.tsx,full-width.tsx,granularity.tsx,invalid.tsx,on-surface.tsx,required.tsx,variants.tsx,with-description.tsx,with-prefix-and-suffix.tsx,with-prefix-icon.tsx,with-suffix-icon.tsx,with-validation.tsx}|demos/date-picker:{basic.tsx,controlled.tsx,custom-render-function.tsx,disabled.tsx,form-example.tsx,format-options-no-ssr.tsx,format-options.tsx,international-calendar.tsx,with-custom-indicator.tsx,with-validation.tsx}|demos/date-range-picker:{basic.tsx,controlled.tsx,custom-render-function.tsx,disabled.tsx,form-example.tsx,format-options-no-ssr.tsx,format-options.tsx,input-container.tsx,international-calendar.tsx,with-custom-indicator.tsx,with-validation.tsx}|demos/description:{basic.tsx}|demos/disclosure-group:{basic.tsx,controlled.tsx}|demos/disclosure:{basic.tsx,custom-render-function.tsx}|demos/drawer:{backdrop-variants.tsx,basic.tsx,controlled.tsx,navigation.tsx,non-dismissable.tsx,placements.tsx,scrollable-content.tsx,with-form.tsx}|demos/dropdown:{controlled-open-state.tsx,controlled.tsx,custom-trigger.tsx,default.tsx,long-press-trigger.tsx,single-with-custom-indicator.tsx,with-custom-submenu-indicator.tsx,with-descriptions.tsx,with-disabled-items.tsx,with-icons.tsx,with-keyboard-shortcuts.tsx,with-multiple-selection.tsx,with-section-level-selection.tsx,with-sections.tsx,with-single-selection.tsx,with-submenus.tsx}|demos/error-message:{basic.tsx,with-tag-group.tsx}|demos/field-error:{basic.tsx}|demos/fieldset:{basic.tsx,on-surface.tsx}|demos/form:{basic.tsx,custom-render-function.tsx}|demos/input-group:{default.tsx,disabled.tsx,full-width.tsx,invalid.tsx,on-surface.tsx,password-with-toggle.tsx,required.tsx,variants.tsx,with-badge-suffix.tsx,with-copy-suffix.tsx,with-icon-prefix-and-copy-suffix.tsx,with-icon-prefix-and-text-suffix.tsx,with-keyboard-shortcut.tsx,with-loading-suffix.tsx,with-prefix-and-suffix.tsx,with-prefix-icon.tsx,with-suffix-icon.tsx,with-text-prefix.tsx,with-text-suffix.tsx,with-textarea.tsx}|demos/input-otp:{basic.tsx,controlled.tsx,disabled.tsx,form-example.tsx,four-digits.tsx,on-complete.tsx,on-surface.tsx,variants.tsx,with-pattern.tsx,with-validation.tsx}|demos/input:{basic.tsx,controlled.tsx,full-width.tsx,on-surface.tsx,types.tsx,variants.tsx}|demos/kbd:{basic.tsx,inline.tsx,instructional.tsx,navigation.tsx,special.tsx,variants.tsx}|demos/label:{basic.tsx}|demos/link:{basic.tsx,custom-icon.tsx,custom-render-function.tsx,icon-placement.tsx,underline-and-offset.tsx,underline-offset.tsx,underline-variants.tsx}|demos/list-box:{controlled.tsx,custom-check-icon.tsx,custom-render-function.tsx,default.tsx,multi-select.tsx,virtualization.tsx,with-disabled-items.tsx,with-sections.tsx}|demos/meter:{basic.tsx,colors.tsx,custom-value.tsx,sizes.tsx,without-label.tsx}|demos/modal:{backdrop-variants.tsx,close-methods.tsx,controlled.tsx,custom-animations.tsx,custom-backdrop.tsx,custom-portal.tsx,custom-trigger.tsx,default.tsx,dismiss-behavior.tsx,placements.tsx,scroll-comparison.tsx,sizes.tsx,with-form.tsx}|demos/number-field:{basic.tsx,controlled.tsx,custom-icons.tsx,custom-render-function.tsx,disabled.tsx,form-example.tsx,full-width.tsx,on-surface.tsx,required.tsx,validation.tsx,variants.tsx,with-chevrons.tsx,with-description.tsx,with-format-options.tsx,with-step.tsx,with-validation.tsx}|demos/pagination:{basic.tsx,controlled.tsx,custom-icons.tsx,disabled.tsx,simple-prev-next.tsx,sizes.tsx,with-ellipsis.tsx,with-summary.tsx}|demos/popover:{basic.tsx,custom-render-function.tsx,interactive.tsx,placement.tsx,with-arrow.tsx}|demos/progress-bar:{basic.tsx,colors.tsx,custom-value.tsx,indeterminate.tsx,sizes.tsx,without-label.tsx}|demos/progress-circle:{basic.tsx,colors.tsx,custom-svg.tsx,indeterminate.tsx,sizes.tsx,with-label.tsx}|demos/radio-group:{basic.tsx,controlled.tsx,custom-indicator.tsx,custom-render-function.tsx,delivery-and-payment.tsx,disabled.tsx,horizontal.tsx,on-surface.tsx,uncontrolled.tsx,validation.tsx,variants.tsx}|demos/range-calendar:{allows-non-contiguous-ranges.tsx,basic.tsx,booking-calendar.tsx,controlled.tsx,default-value.tsx,disabled.tsx,focused-value.tsx,international-calendar.tsx,invalid.tsx,min-max-dates.tsx,multiple-months.tsx,read-only.tsx,three-months.tsx,unavailable-dates.tsx,with-indicators.tsx,year-picker.tsx}|demos/scroll-shadow:{custom-size.tsx,default.tsx,hide-scroll-bar.tsx,orientation.tsx,visibility-change.tsx,with-card.tsx}|demos/search-field:{basic.tsx,controlled.tsx,custom-icons.tsx,custom-render-function.tsx,disabled.tsx,form-example.tsx,full-width.tsx,on-surface.tsx,required.tsx,validation.tsx,variants.tsx,with-description.tsx,with-keyboard-shortcut.tsx,with-validation.tsx}|demos/select:{asynchronous-loading.tsx,controlled-multiple.tsx,controlled-open-state.tsx,controlled.tsx,custom-indicator.tsx,custom-render-function.tsx,custom-value-multiple.tsx,custom-value.tsx,default.tsx,disabled.tsx,full-width.tsx,multiple-select.tsx,on-surface.tsx,required.tsx,variants.tsx,with-description.tsx,with-disabled-options.tsx,with-sections.tsx}|demos/separator:{basic.tsx,custom-render-function.tsx,manual-variant-override.tsx,variants.tsx,vertical.tsx,with-content.tsx,with-surface.tsx}|demos/skeleton:{animation-types.tsx,basic.tsx,card.tsx,grid.tsx,list.tsx,single-shimmer.tsx,text-content.tsx,user-profile.tsx}|demos/slider:{custom-render-function.tsx,default.tsx,disabled.tsx,range.tsx,vertical.tsx}|demos/spinner:{basic.tsx,colors.tsx,sizes.tsx}|demos/surface:{variants.tsx}|demos/switch:{basic.tsx,controlled.tsx,custom-render-function.tsx,custom-styles.tsx,default-selected.tsx,disabled.tsx,form.tsx,group-horizontal.tsx,group.tsx,label-position.tsx,render-props.tsx,sizes.tsx,with-description.tsx,with-icons.tsx,without-label.tsx}|demos/table:{async-loading.tsx,basic.tsx,column-resizing.tsx,custom-cells.tsx,empty-state.tsx,pagination.tsx,secondary-variant.tsx,selection.tsx,sorting.tsx,tanstack-table.tsx,virtualization.tsx}|demos/tabs:{basic.tsx,custom-render-function.tsx,custom-styles.tsx,disabled.tsx,secondary-vertical.tsx,secondary.tsx,vertical.tsx,with-separator.tsx}|demos/tag-group:{basic.tsx,controlled.tsx,custom-render-function.tsx,disabled.tsx,selection-modes.tsx,sizes.tsx,variants.tsx,with-error-message.tsx,with-list-data.tsx,with-prefix.tsx,with-remove-button.tsx}|demos/textarea:{basic.tsx,controlled.tsx,full-width.tsx,on-surface.tsx,rows.tsx,variants.tsx}|demos/textfield:{basic.tsx,controlled.tsx,custom-render-function.tsx,disabled.tsx,full-width.tsx,input-types.tsx,on-surface.tsx,required.tsx,textarea.tsx,validation.tsx,with-description.tsx,with-error.tsx}|demos/time-field:{basic.tsx,controlled.tsx,custom-render-function.tsx,disabled.tsx,form-example.tsx,full-width.tsx,invalid.tsx,on-surface.tsx,required.tsx,with-description.tsx,with-prefix-and-suffix.tsx,with-prefix-icon.tsx,with-suffix-icon.tsx,with-validation.tsx}|demos/toast:{callbacks.tsx,custom-indicator.tsx,custom-queue.tsx,custom-toast.tsx,default.tsx,placements.tsx,promise.tsx,simple.tsx,variants.tsx}|demos/toggle-button-group:{attached.tsx,basic.tsx,controlled.tsx,disabled.tsx,full-width.tsx,orientation.tsx,selection-mode.tsx,sizes.tsx,without-separator.tsx}|demos/toggle-button:{basic.tsx,controlled.tsx,disabled.tsx,icon-only.tsx,sizes.tsx,variants.tsx}|demos/toolbar:{basic.tsx,custom-styles.tsx,vertical.tsx,with-button-group.tsx}|demos/tooltip:{basic.tsx,custom-render-function.tsx,custom-trigger.tsx,placement.tsx,with-arrow.tsx}

<!-- HEROUI-REACT-AGENTS-MD-END -->
