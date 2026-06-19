# Skill / MCP **Library** + per-project on/off toggle (the "Switchboard")

**Status:** design (brainstormed 2026-06-07/08; not yet implemented)
**Date:** 2026-06-08

## Motivation

A skill or MCP server is only useful in _some_ projects, but today install location
forces an all-or-nothing choice:

- Install **globally** (`~/.claude/skills/`, `~/.claude.json`) → it loads into **every**
  project's context, even where it's irrelevant.
- Install into **one project** → it can't be reused elsewhere without reinstalling.

Both waste context. Skill names+descriptions and MCP tool schemas are injected into the
agent's context for **every** resource it can see, so the only real lever for "best
context per project" is **which resources are visible to the agent in that project**.

### The load-bearing constraint

Claude Code (and every other agent) decides what to load by **what is physically present
in the directories it scans** — `~/.claude/skills/` + `<project>/.claude/skills/` for
skills, `~/.claude.json` + `<project>/.mcp.json` for MCP. It does **not** read any aghub
"enabled" flag. (aghub's existing `Skill.enabled` field is `#[serde(skip)]` and never
persisted — a no-op for skills; MCP `enabled` only round-trips for OpenCode.)

Therefore a real per-project switch can only be built on **presence**: a resource must
leave the always-scanned global location and live in a **Library** that no agent scans,
and "on" must mean "materialised into _this_ project's scanned location".

This is exactly the existing **Master + Referrer** (Universal install) machinery, applied
at **project** scope instead of agent-global scope.

---

## Glossary (proposed additions to `CONTEXT.md`)

**Library**:
An off-pool that **no agent scans** — `~/.aghub/skills/<name>/` for skills, `~/.aghub/mcp.json`
for MCP. A resource in the Library exists on disk but contributes **zero** context to any
project until Activated. A Library skill is a **Master** (per the existing term) whose
Referrers are created lazily, per project, on Activation.
_Avoid_: "store", "cache".

**Resident** (a.k.a. global-always):
A resource installed in the always-scanned global location (`~/.claude/skills/`,
`~/.claude.json`). It loads into every project and cannot be toggled per project. The
user's existing global resources are Resident and are never touched except by an explicit
Demote.
_Avoid_: "global skill" when the per-project distinction matters.

**Activate / Deactivate**:
Turning a Library resource on/off **in a specific project**. Activate = create the project
Referrer (skill symlink) / write the project `.mcp.json` entry. Deactivate = remove it. The
Library Master / catalog entry is untouched by Deactivate.
_Avoid_: "enable/disable" conceptually (those are the CLI verbs and the OpenCode soft-toggle).

**Switchboard**:
The per-project desktop view (`/projects/:id`) listing a project's resources as
Resident (locked on) / Activated / available-in-Library, each with a `Switch`.

**Demote**:
Move a Resident into the Library (`~/.claude/skills/<name>` → `~/.aghub/skills/<name>`),
making it toggleable. Opt-in only; the sole operation that touches existing Resident
resources.

---

## Core model

| State             | On disk                                                                    | Loaded by Claude?      | Source              |
| ----------------- | -------------------------------------------------------------------------- | ---------------------- | ------------------- |
| **Resident**      | `~/.claude/skills/<name>/`, `~/.claude.json`                               | every project          | existing; untouched |
| **Library · off** | `~/.aghub/skills/<name>/`, `~/.aghub/mcp.json`                             | **nobody** → 0 context | aghub install       |
| **Library · on**  | `<project>/.claude/skills/<name>` (symlink), `<project>/.mcp.json` (entry) | this project only      | Activation          |

Principles fixed during brainstorming:

- **Install ≠ active-everywhere.** New installs default into the Library (off everywhere).
- **Implicit state, no manifest, not in git.** aghub manages it directly: skill symlink
  present = on; MCP same-named entry present = on. There is **no** separate enable-list file.
- **Existing Resident resources are never touched** except by an explicit Demote.
- **Claude-first, extensible.** v1 targets Claude (+ Copilot, which shares `~/.claude/skills`).
  The model and API carry an `agents: [..]` field (v1 fixed to `["claude"]`) so adding
  Codex/OpenCode later needs no refactor — only widening the Referrer targets.
- **Desktop-first.** Build order: core → HTTP API → desktop UI (the primary surface) →
  CLI (opportunistic). The Switchboard is where the user lives.

### Why `~/.aghub/` and not `~/.agents/`

`~/.agents/skills` is the npx **Universal** master location that universal-capable agents
(Codex, OpenCode, Cursor, …) **auto-scan** — anything there is _always on_ for them, which
breaks "off by default" the moment we extend beyond Claude. `~/.aghub/` is scanned by **no**
agent, so the toggle semantics stay uniform across all agents, and the npx `.agents`
round-trip contract is left untouched. (`~/.aghub` is currently unused; the desktop bundle id
`com.akrc.aghub` is unrelated.)

### Relationship to the existing Universal install

Both use **Master + Referrer + symlink**, but differ in _where_ and _when_:

|                                  | Master location          | Referrer location     | Created                                     |
| -------------------------------- | ------------------------ | --------------------- | ------------------------------------------- |
| **Universal install** (existing) | `.agents/skills/<name>`  | agent **global** dir  | at install → always-on for universal agents |
| **Library** (new)                | `~/.aghub/skills/<name>` | **project** agent dir | on **Activation** → off until then          |

So the Library reuses the `install_layout.rs` primitive with: canonical dir =
`~/.aghub/skills`, Referrer target = a **project** agent skills dir, link created on
Activate rather than install.

---

## Part A — Skills: Library + per-project Activation

### Install into the Library

`copy_dir_recursive` the parsed skill into `~/.aghub/skills/<safe_name>/` (reusing the
existing copy primitive, excluding `.git`/`__pycache__`). No Referrer is created anywhere —
the skill is off in every project. This is the **default** install destination (§Part C).

### Activate (turn on in a project)

Create an **absolute** symlink `<project>/.claude/skills/<name>` → `~/.aghub/skills/<name>`,
via the existing `link_one()` (idempotent / conflict-safe / Windows copy-fallback). Absolute
(not relative) because the Master lives in `$HOME`, outside the project tree.

- **Idempotent**: an existing correct symlink → no-op (`AlreadyLinked`).
- **Conflict**: a real dir / foreign symlink already at `<project>/.claude/skills/<name>` is
  **never clobbered** — Activation aborts and reports the conflict.
- **Windows fallback**: if symlink creation fails, copy the master in and record
  `CopiedFallback` so Deactivate knows to delete the copy (not just unlink).
- Requires a resolvable **project root** (an agent marker such as `.claude/`). If cwd / the
  selected project has none, Activation errors "not in a project".

### Deactivate (turn off in a project)

Remove **only** the project Referrer (`remove_skill` already detects `canonical_path` → a
symlink → unlinks and leaves the Master intact). On a Windows `CopiedFallback`, remove the
copied directory instead.

### Containment & Library removal

- Add `~/.aghub/skills` to `allowed_skill_roots()` so the containment guard permits
  removal/relink of Library Referrers and Masters (and rejects symlink-escape as before).
- `library remove <name>`: if any project still Activates it, **detect the Referrers and
  block/warn** rather than orphaning symlinks.

### `enabled` field

Activation state is **presence**, not the `Skill.enabled` flag. The existing no-op
`enabled` field plays no role here (it can stay as-is for now; removing it is out of scope).

---

## Part B — MCP: Library catalog + per-project Activation

### Catalog format — `~/.aghub/mcp.json`

A normalized, agent-agnostic catalog using aghub's internal `McpServer` model (not any
agent's on-disk shape):

```jsonc
{
	"version": 1,
	"servers": {
		"filesystem": {
			"transport": {
				"type": "stdio",
				"command": "npx",
				"args": ["-y", "@mcp/server-fs", "/data"],
				"env": { "LOG": "info" },
			},
			"timeout": 30000,
		},
		"github": {
			"transport": {
				"type": "streamable_http",
				"url": "https://api.../mcp/",
				"headers": { "Authorization": "${GH_PAT}" },
			},
		},
	},
}
```

Auto-created on first Library write; global-home only; never committed to a project.
`transport` uses the existing `#[serde(tag = "type")]` snake_case variants
(`stdio` / `sse` / `streamable_http`).

### Identity & "positioning" — name-based, implicit

MCP's only identity key is `name` (no UUID/hash); aghub does **not** dedup MCP across scopes.
We embrace that:

- A Library MCP is **on in project P** ⟺ `<project>/.mcp.json` has an entry with the **same
  name** as a Library catalog entry.
- **Activate** = convert the normalized entry to the target agent's shape (Claude
  `mcpServers`) and write it into `<project>/.mcp.json` (existing `add_mcp` @ ProjectOnly). A
  same-named **different** MCP already present → **block + warn**; same-named identical → idempotent.
- **Deactivate** = remove that named entry from `<project>/.mcp.json` (existing `delete_mcp` @
  ProjectOnly); other entries and non-MCP keys are preserved by `save_mcps_to_file`.
- The name is **preserved as-is** (no `lib_` prefix) so Claude shows the real name.
- Accepted trade-off: a hand-added entry that happens to share a Library name is treated as
  "the Library one, on". For MCP (name = identity) this is acceptable.

### Secrets / env

env (incl. API keys) is plaintext JSON. Policy:

- Library catalog stores values **as entered** (plaintext, in `$HOME` — same exposure as
  today's `~/.claude.json`).
- On Activation into `<project>/.mcp.json`, **detect if the file is inside a git repo and
  warn** + offer to add it to `.gitignore` (non-blocking).
- **`${VAR}` placeholders are supported**: stored and written verbatim; Claude expands at
  runtime. Security-conscious users avoid plaintext-in-repo by entering `${MY_KEY}`.
- Keyring integration (aghub already ships a platform keyring) is a **future** follow-up.

### Copy semantics (no auto-propagate)

MCP Activation **copies** the definition (JSON has no symlink). Editing a Library entry does
**not** update already-Activated projects. A "re-sync to projects" action is a future
follow-up; v1 does not auto-propagate.

### Transport fidelity

The normalized catalog preserves the exact transport (Stdio/Sse/StreamableHttp), so Claude
round-trips losslessly. OpenCode's `sse`→`streamable_http` identity loss only matters at the
(future) OpenCode Activation boundary; noted, not a v1 concern.

---

## Part C — Desktop UI (the Switchboard)

| Surface                                | Role                                                                                                                                                                                              |
| -------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **`/projects/:id`** (Switchboard)      | Per-project on/off. Lists **Resident + Activated** only; each row a `Switch`. **"+ Activate from Library"** opens a search/multi-select picker. Switch off → row returns to Library.              |
| Resident rows                          | `Switch` shown **on + locked** (disabled) with a "Resident" chip + tooltip ("loads in every project; can't disable here"); each offers an opt-in **Demote** action.                               |
| **`/skills`, `/mcp`** (global pages)   | Demoted to **Library + Resident management** (install / remove / update the actual artifacts). Library MCP list is keyed by **name** (the global MCP page keeps its existing transport grouping). |
| **Install entry** (incl. Sources page) | New installs **default to the Library** (off everywhere); "Resident" is an explicit opt-in choice.                                                                                                |

Notes:

- One `Switch` per row = "active in this project", v1 targeting Claude (+ Copilot). The
  API/model carry `agents: ["claude"]`; a later per-agent expansion turns the row into a
  per-agent sub-control without backend changes.
- `Switch` is a new HeroUI component usage here (not currently used in skills/MCP pages).
- The Switchboard extends the existing `/projects/:id` `UnifiedResourceList` (`scope: "all"`)
  by adding Library-available items via the picker and a per-row state.
- Concept clarification surfaced in UI copy: **Universal** = `.agents` master + agent-global
  symlinks (always-on); **Library** = `~/.aghub` master + per-project symlinks (off until
  Activated). The primary install choice is destination (**Library / Resident**);
  isolation-vs-universal is a sub-detail of the Resident path's cross-agent layout.

### Part D — Demote (Resident → Library)

Move `~/.claude/skills/<name>` → `~/.aghub/skills/<name>` **and auto-Activate it in the
current project**. Net effect in the project being viewed: unchanged; effect elsewhere: now
off (toggleable). Without the auto-Activate, Demote would silently turn the skill off in the
very project the user is looking at. (MCP Demote: move the `~/.claude.json` entry into the
catalog + write it into the current `<project>/.mcp.json`.)

---

## HTTP API

Follow existing conventions (ScopeParams `scope`/`project_root`; `{error, code}` envelope;
ts-rs DTOs in `crates/desktop/src/generated/dto`; separate `enable`/`disable` routes already
exist). Proposed:

| Method + path                                                            | Purpose                                                            |
| ------------------------------------------------------------------------ | ------------------------------------------------------------------ |
| `POST /skills/library`, `POST /mcps/library`                             | Install into the Library (off everywhere).                         |
| `GET /skills/library`, `GET /mcps/library`                               | List the Library pool.                                             |
| `DELETE /skills/library/<name>`, `DELETE /mcps/library/<name>`           | Remove from Library (blocks if Referrers exist).                   |
| `POST /agents/<agent>/skills/<name>/activate?project_root=` (+ `mcps`)   | Activate in a project (create Referrer / write entry).             |
| `POST /agents/<agent>/skills/<name>/deactivate?project_root=` (+ `mcps`) | Deactivate in a project.                                           |
| `POST /skills/<name>/demote?project_root=` (+ `mcps`)                    | Demote a Resident into the Library + auto-Activate in the project. |

The list endpoints (or the project `scope: "all"` list) gain a `state` field per resource:
`resident` / `project_on` / `library_off`, so the Switchboard renders three states from one
query. (Naming: `activate`/`deactivate` are the API verbs to avoid colliding with the
existing MCP soft-toggle `enable`/`disable`.)

## CLI (opportunistic)

- `aghub -p enable|disable skill|mcp <name>` → Activate/Deactivate in the current project.
  (For skills this currently is a no-op; repurposing fixes the dead toggle.)
- `aghub skill|mcp library add|list|remove|import <…>` — manage the pool; `import` = Demote.

---

## File map (anticipated)

**Core (`crates/core`)**

- `src/skills/install_layout.rs` — reuse `install_universal`/`link_one` with canonical dir =
  `~/.aghub/skills` and project Referrer targets; add `~/.aghub/skills` to
  `allowed_skill_roots()`.
- `src/skills/library.rs` _(new)_ — Library install/list/remove/import; Activation/Deactivation
  for skills; Demote.
- `src/mcp/library.rs` _(new)_ — `~/.aghub/mcp.json` catalog read/write (normalized model);
  Activate (→ `add_mcp` ProjectOnly with format conversion + git/secret checks), Deactivate
  (→ `delete_mcp` ProjectOnly); Demote.
- `src/manager/{skill,mcp}.rs` — wire the above; 3-state annotation in list/load.

**Agents (`crates/agents`)** — a normalized↔Claude MCP conversion path for catalog entries
(reuse `json_map` serialize for the project-file side).

**API (`crates/api`)** — new routes + DTOs above; `state` field on skill/MCP responses; ts-rs export.

**CLI (`crates/cli`)** — repurpose `enable`/`disable` @ project scope; `skill|mcp library *`.

**Desktop (`crates/desktop`)** — Switchboard on `/projects/:id` (Switch + Library picker +
Resident/Demote); global pages → Library management; install-destination choice (default
Library); `requests/*` + `lib/api.ts` methods + query keys; i18n `lib/locales/{en,zh-Hant,zh-Hans}`.

---

## Edge cases & invariants

- **Conflict-safe**: never clobber a real dir / foreign symlink / different same-named MCP;
  report instead.
- **Idempotent**: re-Activating an already-on resource is a no-op.
- **Library removal guard**: refuse/warn if Referrers exist.
- **Windows**: symlink → copy fallback; Deactivate removes the copy.
- **Resident vs Library same name**: Resident wins display as "Resident"; flagged if ambiguous.
- **Project resolution**: Activation requires an agent-marker project root.
- **npx `.agents` contract untouched**: the Library is `~/.aghub/`, separate from `.agents`.

## Out of scope / future

- Sub-agents (only skills + MCP in v1).
- Per-agent expansion of the Switch (model already carries `agents: []`).
- MCP "re-sync to projects" after a Library edit.
- Keyring-backed MCP secrets.
- OpenCode `sse`→`http` fidelity at a future OpenCode Activation boundary.

## Implementation status

- [ ] Core: Library install + Activate/Deactivate (skills) reusing `install_layout`
- [ ] Core: `~/.aghub/mcp.json` catalog + MCP Activate/Deactivate + secret/git checks
- [ ] Core: Demote (skills + MCP) with auto-Activate
- [ ] API: library + activate/deactivate/demote routes; `state` field; DTO export
- [ ] Desktop: Switchboard (Switch + picker + Resident/Demote); install-destination default
- [ ] CLI (opportunistic): repurpose enable/disable; `library *`
- [ ] `CONTEXT.md` glossary additions; consider an ADR for the `~/.aghub` Library decision
