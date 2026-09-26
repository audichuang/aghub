# CLI CRATE

`aghub-cli` — the clap binary. **`-p aghub` is the desktop src-tauri package, not
this one**; this crate is `-p aghub-cli`.

`src/main.rs` holds `Cli`/`Commands` + dispatch; `src/commands/` is one file per
subcommand. Authoritative surface: clap (`just start -- --help`). This crate
OWNS the user-facing semantics below — the root AGENTS.md no longer repeats
them, and the WHY behind each flow is in the knowledge pages named per section.

## Surface semantics (this crate is where they are written down)

Aliases: `skills`/`skill`, `mcps`/`mcp`. Scope: `-a` (one id, a comma-separated
list, or `all`), `-g`/`-p`, `--all`.

- **Destructive defaults**: `delete`, `apply-update`, `prune-lock`,
  `source sync`, `source accept-rename`, reconcile-with-removals → **dry-run
  unless `--yes`**. `apply-update <name>` refuses outright instead of previewing, and
  rejects `--all` from the scope table, i.e. BEFORE the `--yes` refusal.
  `apply-update --outdated` (the CLI's "update all") DOES preview: it runs the
  online check for ONE scope and resyncs every `updateAvailable` row through
  `resync_locked_skills` (`source_group: None`, all Sources in one batch);
  renamed rows are skipped with an `accept-rename` hint
  and uncheckable rows are reported by name and reason, never presented as
  proof that nothing is outdated
- **`delete`'s JSON carries `outcome`**: `preview` | `removed` | `absent` |
  `partial` | `kept` (`success: true` but THE ENTITY IS STILL THERE; the API
  adds an api-only `failed`). Read `outcome`, never `dry_run`/`executed` —
  `executed: true` is set for the whole execute branch even when every delete
  failed (`partial`), and `absent` outranks the caller's intent. `kept` covers
  two situations: a shared Master another agent still reads, and an
  `--all-agents` sweep that took NOTHING because it could not prove nothing
  still holds the skill (`skipped` names what it could not read). The second
  never reaches commit and runs no lock prune — fix what it could not read and
  re-run; no flag overrides it. A preview carries `would_prune_lock_entries`,
  deliberately separate from the committed `pruned_lock_entries`.
  Why: knowledge page `技能移除與 Master 回收`
- **`doctor`'s `health` covers lock ↔ Master only** — per-agent referrer state
  needs `--verify-links`, and `linkAudit.state` is `verified` ONLY when every
  agent row is healthy. The per-agent verdict is `skills::shape::classify_shape`'s,
  renamed; doctor derives none of its own, so `chain` and `masterUnusable` must
  not be folded back into the sync note, `orphanMaster` must NOT be offered
  `source sync --install-missing` (there is no source), and `master-is-symlink`
  ALSO fails `--fail-on-issues` — the two axes must never answer one fact
  differently. On the health axis only `untracked` is excused; on the link axis
  `withheld` / `unsupported` / `linked` are not issues either. Default exit is
  unchanged;
  `--fail-on-issues` opts into a non-zero one.
  States, remedies and the four buckets: knowledge page `技能連結形狀與 repair 鏈`
- **`repair`** also DETACHES stale Referrers in read-only compat dirs (outcome
  `tidied`, printed `unlinked:`) under four guards that must not be loosened,
  and REFUSES a real directory that git TRACKS (the refusal prints
  `git rm -r --cached <path>`). Same page.
- **`check` is offline by default** (`checked: false`): a source nothing could
  fetch keeps its permanent reason (`local` / `ssh` / `unsupportedScheme`),
  everything else reports `network` = "we did not look", so `--online` is only
  ever suggested for rows it would really answer. Offline hashes NO skill
  folder. Scope defaults to BOTH, like `doctor` / `source list` / `source diff`
- **"Update available" includes a LOCAL edit**, not just an upstream move — and
  `apply-update --yes` OVERWRITES that edit. The row's two digests are
  comparison hashes and deliberately need NOT equal the lock's `computedHash`.
  Mechanics: `crates/skill-update/AGENTS.md`
- **`check` never writes; `--write-result` is why that needs a guard.** The
  sidecar path is arbitrary, so the write is refused by file NAME
  (`.skill-lock.json`, `skills-lock.json`, `.aghub-mutation.lock`), by an
  `.aghub` path SEGMENT, and by `.agents/skills` as adjacent SEGMENTS, BEFORE
  any resolved-path comparison — `-g` resolves no project root at all, so the
  project lock one `../` away is invisible to a resolved-path check. Normalize
  with `skill::lock::resolve_existing`. Why each round of "simplify this":
  knowledge page `check --write-result 的受管狀態守衛`
- **`source diff` ALWAYS fetches** (no offline mode); `--online` is accepted as
  a no-op alias so the `check` habit is not a clap error. It judges each read
  scope against the origin THAT scope's lock records; ambiguity within ONE scope
  is still a refusal. The API's `?scope=all` answers differently on purpose —
  see `crates/api/AGENTS.md`
- **`transfer` / `reconcile`**: cross-agent copy / reconcile of
  skills·mcps·sub-agents. `reconcile` needs at least one `--add`/`--remove`;
  `-a/--agent` is ignored. An already-present target is an idempotent success
  (`already_present: true`) — for an MCP/sub-agent only when the existing value
  is EQUIVALENT. Removing a skill refuses an end state that cannot exist (the
  agent would read it from the same set of places afterwards), and
  `reconcile skill` refuses BEFORE the first write so the disk is untouched.
  The preview is a plan echo (`{dry_run, add, remove}`, no per-row results) and
  does NOT check that a `--remove` target ever held the thing: a typo previews as
  "would remove" and exits 0, then fails that row on commit and exits 1.
  `reconcile mcp` / `reconcile sub-agent` protect the whole ROSTER, not just the
  agents you named, so `reconcile mcp --remove claude -p` is always refused —
  the message names copilot (both resolve `<root>/.mcp.json`; repeat `--remove`,
  it takes no comma list). Following that remedy exits 0. A `--remove` naming an
  agent whose backing no row emptied FAILS that row. That refusal is
  `INVALID_CONFIG` / HTTP 400; the SKILL removal refusal is the other code —
  `delete` and `reconcile skill` both report `UNSUPPORTED_OPERATION` / HTTP 422.
  Batching is transport and must not relabel either as bad parameters.
  Direct `delete mcps <name> -a claude -p` uses the same shared-reader guard:
  it refuses before preview or commit when copilot would lose the server.
  Why:
  knowledge page `多目標突變的 scope 閘門與批次歸因`
- **`skill-usage`**: Claude-global only; rejects project/`--all`.
  **`coverage`**: rejects `--all`, scope `-g` or `-p` only, and is a static
  CAPABILITY matrix — no skill names, no counts (use `doctor --verify-links`)
- **Narrowed resource args**: `check`/`apply-update` take skills ONLY,
  `enable`/`disable` take mcps ONLY — their own clap value_enums, so the
  rejection is a parse error naming the valid values
- **`inference`**: provider inventory + keyring keys. Bindings/routing are
  desktop/API-only — there is no `inference bind` here. `--api-key -` reads the
  key from stdin on `inference add` ONLY — `update` stores a literal `-`
- Skill install is **always symlink-only**; `--universal` is a hidden no-op
- Source creds: `GIT_PASSWORD` (any host) / `GITHUB_TOKEN` (github.com https-only)

## Two dispatch funnels — keep both halves in step

**Scope.** Scope flags are mutually exclusive, enforced MANUALLY in `main()`
before every dispatch (a clap `ArgGroup` does not propagate to `global = true`
args), so that rejection is exit **1**, not clap's exit 2. ONE table
(`scope_policy`), ONE resolver (`resolve_scope`), ONE resolved value (`Scope`). `scope_policy` is **exhaustive** over `Commands` and
over `SourceAction`, so a new subcommand does not COMPILE until it is
classified — it used to end in `_ => AllowBoth` and rely on a comment. `None`
means the command ignores scope entirely (`inference`, `plugin`) and must not
reach the resolver.

Command modules receive a `Scope`, never `cli.global/project/all`; its fields
are private **to `mod scope`**, so there is nothing left for them to re-derive.
The module matters: Rust privacy reaches every DESCENDANT of the defining
module, so a `Scope` declared in the crate root would still let
`commands::source` forge `Scope { ProjectOnly, None }` and skip the table.
(`aghub_core::paths::find_project_root` stays re-importable by anyone — the
seal is on CONSTRUCTING a resolved scope, not on finding a root.) That is what
stops `source`/`coverage`/`transfer` regrowing private resolvers with their own
wording of the project-root bail (there were five). The bail covers reads too
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
`transfer::install_scope` maps the scope itself rather than calling
`write_target()`.

**`Scope::write_target()`** is the ONE answer to "which store does this write?"
(`Some(root)` = project, `None` = global) and it ERRORS on anything else.
`source`'s `write_scope`, `accept-rename`'s `RenameScope` and `transfer`'s
`install_scope` each used to close that match with `_ => …::Global`, so a scope
the table let through became a silent write to the GLOBAL lock. Never reopen
one of those matches with a catch-all.

The classification itself is compile-forced but not compile-CHECKED: only review
catches a command classified under the wrong policy. What the test suite adds is
that a new subcommand cannot escape the case table —
`every_subcommand_has_a_policy_case` asks clap for the subcommand list rather
than a second hand-written one.

**Early dispatch.** `check`, `repair`, `prune-lock`, `plugin`, `source`,
`apply-update`, `inference`, `transfer`,
`reconcile`, `coverage`, `doctor` and `skill-usage` run BEFORE any adapter or
`ConfigManager` exists, so a missing or malformed agent config cannot block a
command that never needed one. The `unreachable!()` arms in `run_for_agent`'s
match are that contract — adding an early dispatch without its arm (or the
reverse) is how it rots.

**Multi-agent.** `AgentSelection` (re-exported by `aghub_core::models`, defined
in `crates/agents/src/models.rs`) is the ONE `-a` parser and `core/src/batch.rs`
owns the envelope. Never re-parse `-a` per command; never hand-roll the envelope.

## Commands stay thin

`transfer` / `reconcile` / `coverage` / `inference` are adapters over core (and
`inference::cascade`). Anything a second surface also needs belongs in core —
this crate is a surface, not a home for policy.

## Failure output

`main` is a thin wrapper: `run(cli)` returns `Result`, and `report_failure`
renders it. Under `--json` the error goes to **stdout** as
`{"error":{code,message,retryable}}` with `code` from
`aghub_core::error_codes` (shared with the API) — so raise a `ConfigError` where
one fits instead of an ad-hoc `bail!`, or the code degrades to `CLI_ERROR`.

A command that has ALREADY printed its full answer and returns `Err` only to set
the exit code (the batch envelope, `transfer`/`reconcile`, a partial
`prune-lock`) must call `note_answer_on_stdout()` first, or stdout ends up
holding two JSON documents and every parse of it fails.

## Tests

`tests/cli_tests.rs` (`assert_cmd`). `source sync` e2e need no network:
`AGHUB_TEST_SOURCE_FETCH_ROOT` (a `#[cfg(debug_assertions)]` hook in
`commands/source.rs`) serves a local dir as the fetched repo. `check skills` is
read-only — it never mutates a lock — and its JSON shape is pinned by
`check_skills_outputs_json_array`.

## Anti-patterns

- **Don't** `println!` diagnostics — use `eprintln_verbose!`
- **Don't** hardcode agent id strings — use `AgentType`
