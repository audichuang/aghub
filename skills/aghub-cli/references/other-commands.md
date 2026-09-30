# `aghub-cli` other commands

The parts of the CLI that `SKILL.md` does not route: MCP servers, sub-agents,
Claude Code plugins, inference providers and `skill-usage`. Syntax is in
`aghub-cli <command> --help`; this file carries only scope, preview and traps.

## Contents

- [MCP servers](#mcp-servers)
- [Sub-agents](#sub-agents)
- [`transfer` and `reconcile` (MCP, sub-agent)](#transfer-and-reconcile-mcp-sub-agent)
- [`plugin`](#plugin)
- [`inference`](#inference)
- [`skill-usage`](#skill-usage)
- [`agents`](#agents)

## MCP servers

`get|describe|add|update|delete mcps` manage MCP servers in an agent's config;
`enable|disable mcps <NAME>` toggle one (MCP only). An agent that cannot toggle
refuses, naming itself — claude, the `-a` default, is one, so always pass `-a`.
Scope and `-a` apply as everywhere; a comma list fans out for
get/add/update/delete/enable/disable (`describe` takes one agent), and `-a all`
is accepted by `get` only.

`add mcps` takes `-c/--command` (stdio) or `-u/--url` (HTTP/SSE), plus
`-t/--transport` (`streamable-http` default, or `sse`), `--header KEY:VALUE`,
`-e/--env KEY=VALUE` and `--timeout SECONDS`.

- `update mcps` is a patch, and `get mcps --json` has a fixed shape: see
  [state-semantics](state-semantics.md#commands-whose-names-mislead) and
  [`get mcps --json`](state-semantics.md#get-mcps---json).
- `delete mcps` judges sharing against the WHOLE `-a` list.
  `delete mcps X -a claude -p` refuses because copilot reads the same
  `<root>/.mcp.json`; `-a claude,copilot` removes it.

## Sub-agents

`get|describe|add|update|delete sub-agents`: see
[state-semantics](state-semantics.md#sub-agents).

## `transfer` and `reconcile` (MCP, sub-agent)

Both copy normalized resources between agents; neither manages git provenance.

- `transfer mcp|sub-agent --from-agent A --name N --to B ...` writes at once.
- `reconcile mcp|sub-agent --from-agent A --name N --add X --remove Y [--yes]`
  needs at least one `--add`/`--remove` and ignores `-a`. An add-only reconcile
  writes at once (no preview); one that removes previews unless `--yes`.
- An already-present equivalent target is an idempotent success
  (`already_present: true`).
- The preview is a plan echo (`{dry_run, add, remove}`) and does NOT check that
  a `--remove` target ever held the resource: a typo previews as "would remove"
  with exit 0, then fails that row on commit (exit 1).
- `reconcile mcp --remove claude -p` is always refused: copilot shares
  `<root>/.mcp.json`, and the whole roster is protected. Repeat `--remove` for
  each sharing agent (it takes no comma list).

## `plugin`

Claude Code plugins and marketplaces (`list`, `install`, `uninstall`, `update`,
`enable`, `disable`, `prune`, `validate`, `marketplace list|add|remove|update`).

- Ignores `-g`/`-p`/`--all` and `-a`: one shared store. Mutations write at once; only `prune` has a `--dry-run` preview.
- Only `plugin list [--available]` and `plugin marketplace list` emit JSON. Every
  other action REJECTS `--json` (error, non-zero) instead of printing prose.
- `install <PLUGIN_ID>` takes `name@marketplace`; `--scope` is one of `global`
  (default), `project`, `local`, `managed`.
- `update` needs a Claude Code restart to apply.

## `inference`

Provider inventory (`list`, `get`, `add`, `update`, `delete`, `key`) plus keyring
API keys. Ignores scope.

- `add` resolves the key from `--api-key <KEY>`, else `--api-key -` (stdin),
  else `$AGHUB_INFERENCE_API_KEY`; stdin is read only for an explicit `-`.
- `update --api-key -` stores a LITERAL `-`: stdin is honoured on `add` only.
  Pass the key explicitly on `update`.
- `delete` needs `--yes`. `key` prints a masked preview, never the raw key.
- Bindings and routing are desktop/API only; there is no `inference bind`.

## `skill-usage`

Reads Claude Code's own `skillUsage` counter from `~/.claude.json`, least-used
first, read-only. Claude-global only: `-p` and `--all` are refused. Installed
skills never dispatched show 0.

## `agents`

`agents list|enable|disable` sets which agents aghub manages (what `-a all`
fan-outs read); ignores scope and `-a`. See the `agents` bullet in
[state-semantics](state-semantics.md#commands-whose-names-mislead).
