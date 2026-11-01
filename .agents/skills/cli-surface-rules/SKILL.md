---
name: cli-surface-rules
description: How aghub-cli resolves scope and what `transfer`, `reconcile` and multi-agent `delete` promise. Use when changing those commands, adding a subcommand or touching `scope_policy` in crates/cli; not for operating the CLI as a user (aghub-cli).
---

# CLI surface rules

A semantics change also edits `skills/aghub-cli/` in the same commit.

## Scope funnel

**Scope.** Scope flags are mutually exclusive, enforced MANUALLY in `main()`
before every dispatch (a clap `ArgGroup` does not propagate to `global = true`
args), so that rejection is exit **1**, not clap's exit 2. ONE table
(`scope_policy`), ONE resolver (`resolve_scope`), ONE resolved value (`Scope`). `scope_policy` is **exhaustive** over `Commands` and
over `SourceAction`, so a new subcommand does not COMPILE until it is
classified. `None`
means the command ignores scope entirely (`inference`, `plugin`, `agents`) and
must not reach the resolver.

Command modules receive a `Scope`, never `cli.global/project/all`; its fields
are private **to `mod scope`**, so there is nothing left for them to re-derive.
The module matters: Rust privacy reaches every DESCENDANT of the defining
module, so a `Scope` declared in the crate root would still let
`commands::source` forge `Scope { ProjectOnly, None }` and skip the table.
(`aghub_core::paths::find_project_root` stays re-importable by anyone — the
seal is on CONSTRUCTING a resolved scope, not on finding a root.) That is what
stops `source`/`coverage`/`transfer` regrowing private resolvers with their own
wording of the project-root bail. The bail covers reads too
(`-p get skills` must not answer `[]` from a non-project dir) and every
rejection runs BEFORE the cwd is touched — `-g` and the plain global default
resolve no project root at all, because a deleted cwd must not kill a
global-only command.

**One policy opts out** (`rootless_project_passthrough`, only
`TRANSFER_SCOPE`): `transfer`/`reconcile` never resolved a root in the CLI, so
a rootless `-p` stays `ProjectOnly` with no root and core's source lookup fails
with a typed `ResourceNotFound`. Bailing early instead rewrites `--json`'s
`error.code` to `CLI_ERROR` with nothing else visibly different — pinned by
`rootless_project_transfer_keeps_resource_not_found_code`. That is also why
`transfer::resolve_transfer_scope` maps the scope itself rather than calling
`write_scope()`.

**`Scope::write_scope()`** is the ONE answer to "which store does this write?"
(`WriteScope::project(root)` = project, `WriteScope::Global` = global) and it
ERRORS on anything else. Core's skill mutation entries take that `WriteScope`
and carry no `Both` / missing-root rejection of their own — this resolver is
where they are refused.
`source`'s `write_scope`, `accept-rename`'s `WriteScope` and `transfer`'s
`resolve_transfer_scope` stay exhaustive matches: a `_ => …::Global` catch-all turns any
scope the table lets through into a silent write to the GLOBAL lock.

The classification itself is compile-forced but not compile-CHECKED: only review
catches a command classified under the wrong policy. What the test suite adds is
that a new subcommand cannot escape the case table —
`every_subcommand_has_a_policy_case` asks clap for the subcommand list rather
than a second hand-written one.

## `transfer`, `reconcile` and multi-agent `delete`

**`transfer` / `reconcile`**: cross-agent copy / reconcile of
skills·mcps·sub-agents. `reconcile` needs at least one `--add`/`--remove`;
`-a/--agent` is ignored. An already-present target is an idempotent success
(`already_present: true`) — for an MCP/sub-agent only when the existing value
is EQUIVALENT. Removing a skill refuses an end state that cannot exist (the
agent would read it from the same set of places afterwards), and
`reconcile skill` refuses BEFORE the first write so the disk is untouched;
`reconcile skill --remove` refuses a plugin-owned skill with `MANAGED_RESOURCE`
like `delete`, in preview and commit alike.

### Skill copies and the lock

A SKILL copy (transfer, reconcile
`--add`) is core `install_local_skill`, the `add --from` install. The CLI always
passes ONE scope (the cross-scope case below is reached only from the
API/desktop route). Within one scope it never touches the lock (an untracked Master stays untracked);
across scopes it writes the target's entry carrying the source's owner and
STORED Source hash — only when the source's host is known (a recorded URL,
github, local, or a source spelling its host) and it carries no credentials;
an ambiguous legacy entry (npx 1.5.11 `gitlab:group/repo`, which may be
self-hosted) or a credential-bearing one (`https://user:TOKEN@…` — a project
lock gets committed) is copied
untracked: `doctor` shows it untracked unless the target already had an
entry of that name; re-install from the real URL to track. A copy is refused
when the target's same-named Master is untracked and differs, and when the
target's entry names a different owner (an untracked copy still compares it,
and an ambiguous URL-less owner matches no entry, not even its own spelling); a CARRIED copy also refuses when the target scope tracks that source on
another ref (it would leave a mixed cohort `source sync` refuses; run
`source sync --ref` there first). A copy onto an OWNED Master grants it as-is, stale content
included (a carried owner does not make the copied content remote, so the
Master is not byte-compared). Accept-rename refuses such an ambiguous entry
before fetching.

### Preview and roster protection

The preview runs the commit's own core plan and per-row preflight
(`reconcile_{skill,mcp,sub_agent}_preview`), so it refuses what `--yes` would,
with the same code — including an MCP/sub-agent copy that would lose a field
once the source is removed. It then echoes the plan (`{dry_run, add, remove}`,
no per-row results) and does NOT check that a `--remove` target ever held the thing: a typo previews as
"would remove" and exits 0, then fails that row on commit and exits 1.
`reconcile mcp` / `reconcile sub-agent` protect the whole ROSTER, not just the
agents you named, so `reconcile mcp --remove claude -p` is always refused —
the message names copilot (both resolve `<root>/.mcp.json`; repeat `--remove`
or pass a comma list). Following that remedy exits 0. A `--remove` naming an
agent whose backing no row emptied FAILS that row. The roster refusal is
`INVALID_CONFIG` / HTTP 400; the SKILL removal refusal is the other code —
`delete` and `reconcile skill` both report `UNSUPPORTED_OPERATION` / HTTP 422.
Batching is transport and must not relabel either as bad parameters.

### Direct `delete`

Direct `delete` judges sharing against the WHOLE `-a` list (core's
`requested`), not each row alone: `delete mcps <name> -a claude -p`
refuses before preview or commit because copilot reads the same
`<root>/.mcp.json`, and `-a claude,copilot` removes it (exit 0). Same for
skills: a shared Referrer goes only when every reader of that slot is in the
list. The API's by-name delete routes take the same set as `?agents=a,b`.
Why:
knowledge page `多目標突變的 scope 閘門與批次歸因`
