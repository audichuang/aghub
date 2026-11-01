# CLI CRATE

`aghub-cli` — the clap binary. **`-p aghub` is the desktop src-tauri package, not
this one**; this crate is `-p aghub-cli`.

`src/main.rs` holds `Cli`/`Commands` + dispatch; `src/commands/` is one file per
subcommand. Authoritative surface: clap (`just start -- --help`). The WHY
behind each flow is in the knowledge pages named per section.

## Surface semantics (this crate is where they are written down)

Aliases: `skills`/`skill`, `mcps`/`mcp`, `sub-agents`/`sub-agent`. Scope: `-a` (one id, a comma-separated
list, or `all`), `-g`/`-p`, `--all`.

- **`update mcps` is a PATCH** over the freshly reloaded server (core
  `McpTransport::apply_edit`, applied under the lock): an absent flag keeps the
  old value, `--header`/`--env` REPLACE the whole map, `-t` switches the remote
  kind (url and headers kept), and `-t`/`--header` on a stdio server without
  `-u` is refused. The API PUT replaces the
  transport on purpose, because the GUI sends the whole pre-filled form and
  omits `headers` to clear them.
- **`source accept-rename`** removes the old name's `.aghub/<old>` Master in the
  same transaction (core `rename::accept_rename`, a second removal pass with the
  synthesized canonical path; the snapshot covers it). A Referrer in a dir only
  disabled agents read keeps it. The CLI preview neither fetches nor names
  agents/paths.
- **Destructive defaults**: `delete` (skills, mcps AND sub-agents; the same
  `outcome` vocabulary), `apply-update`, `prune-lock`,
  `source sync`, `source accept-rename`, reconcile-with-removals → **dry-run
  unless `--yes`**. `apply-update <name>` refuses outright instead of previewing, and
  rejects `--all` from the scope table, i.e. BEFORE the `--yes` refusal.
  `apply-update --outdated` (the CLI's "update all") DOES preview: it runs the
  online check for ONE scope and resyncs every `updateAvailable` row through
  `resync_judged_skills` (`source_group: None`, all Sources in one batch),
  handing each row the commit the check judged — a tip that moved since is
  refused per row (`SKILL_SOURCE_CHANGED_DURING_FETCH`), never installed unseen;
  renamed rows are skipped with an `accept-rename` hint
  and uncheckable rows are reported by name and reason, never presented as
  proof that nothing is outdated
- **`source sync --install-missing` PREVIEW** runs core
  `install_fetched::preflight_fetched_install` per install row — the same
  `precheck_request` + `adoption_guard` the install runs under the lock — so a
  predicted refusal is the row's `error` and the preview exits 1 (after
  `note_answer_on_stdout`). Never re-implement the guard in the surface.
- **`get|describe|add|update|delete sub-agents`** are thin adapters over core
  `add_sub_agent` / `update_sub_agent(SubAgentPatch)` / `remove_sub_agent_planned`;
  the wire view is core `dto::SubAgentView` (shared with the API, since
  `SubAgent`'s serde skips `instruction`; the table never prints it). `add`
  requires `--name`, `-d` and `--instruction`; `update` is a patch (absent keeps,
  unmodeled frontmatter survives, no rename flag); MCP-/skill-only flags (incl. `--universal`) are an
  error naming the flag; an agent without sub-agent support is refused
  (`UNSUPPORTED_OPERATION`) by get/describe/delete preview alike, via core
  `ConfigManager::ensure_sub_agent_supported` (also the API's gate). **Sub-agent mutations take a single agent**: an `-a`
  list is refused before any write (core has no sub-agent batch policy) and
  `-a all` is refused as for every mutation; `get` accepts both.
- **`delete`'s JSON carries `outcome`**: `preview` | `removed` | `absent` |
  `partial` | `kept` (`success: true` but THE ENTITY IS STILL THERE; the API
  adds an api-only `failed`). Read `outcome`, never `dry_run`/`executed` —
  `executed: true` is set for the whole execute branch even when every delete
  failed (`partial`), and `absent` outranks the caller's intent. `kept` covers
  two situations: a shared Master another agent still reads (when confirmed with
  `--yes`, a single agent blocked by shared-slot readers gets an error listing
  the blocking agent ids), and an `--all-agents` sweep that took NOTHING because
  it could not prove nothing still holds the skill (`skipped` names what it could
  not read). The second never reaches commit and runs no lock prune — fix what
  it could not read and re-run; no flag overrides it. A preview carries
  `would_prune_lock_entries`, deliberately separate from the committed
  `pruned_lock_entries`.
  Why: knowledge page `技能移除與 Master 回收`
- **`doctor`'s `health` covers lock ↔ Master only** (computed by `aghub_core::skills::health::report`; the CLI only renders and gates) — per-agent referrer state
  needs `--verify-links`, and `linkAudit.state` is `verified` ONLY when every
  agent row is healthy. The per-agent verdict is `skills::shape::classify_shape`'s,
  renamed; doctor derives none of its own, so `chain` and `masterUnusable` must
  not be folded back into the sync note, `orphanMaster` must NOT be offered
  `source sync --install-missing` (there is no source), and `master-is-symlink`
  ALSO fails `--fail-on-issues` — the two axes must never answer one fact
  differently. On the health axis only `untracked` is excused; on the link axis
  `withheld` / `unsupported` / `linked` are not issues either. Issues alone exit
  0; `--fail-on-issues` opts into a non-zero one.
  States, remedies and the four buckets: knowledge page `技能連結形狀與 repair 鏈`
- **`repair`** also DETACHES stale Referrers in read-only compat dirs (outcome
  `tidied`, printed `unlinked:`) under four guards that must not be loosened,
  and REFUSES a real directory that git TRACKS (the refusal prints
  `git rm -r --cached <path>`). Same page. A NAMED skill that is in no lock, has no
  Master and is read by no agent (any skill dir, compat included) is `refused` (core `repair_skill` → `RepairPlan::finds_nothing`),
  never `conformant`; the API route answers the same through `repair_all`. A
  locked name with nothing on disk stays `conformant` — bulk worklists are lock
  names and must not start refusing.
- **`check` is offline by default** (`checked: false`): a source nothing could
  fetch keeps its permanent reason (`local` / `ssh` / `unsupportedScheme`),
  everything else reports `network` = "we did not look", so `--online` is only
  ever suggested for rows it would really answer. Offline hashes NO skill
  folder. Scope defaults to BOTH, like `doctor` / `source list` / `source diff`
- **"Update available" includes a LOCAL edit**, not just an upstream move — and
  `apply-update --yes` OVERWRITES that edit. The row's two digests are
  comparison hashes and deliberately need NOT equal the lock's `computedHash`.
  Mechanics: `crates/skill-update/AGENTS.md`
- **`check --online` heals GLOBAL `refCommit` only** (`projection::write_auto_healed_hashes`,
  shared with `apply-update --outdated`, its preview included): an `upToDate`
  row's `refCommit` advances to the judged tip, or every later check re-fetches
  the group. The content baseline is NOT healed (unlike the API): a
  `heal_hash` on an `updateAvailable` row would record a local edit as the
  baseline and hide it from the next check, and would clear npx's
  `skillFolderHash`. Offline and the project lock are never written; a busy
  mutation lock is a stderr warning, never a failed check.
- **`--write-result` is an UNGUARDED write, so it needs its own guard.** The
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
- **`transfer` / `reconcile`**: cross-agent copy and reconcile. The preview runs the commit's own plan and refuses what `--yes` would; removal protects the whole roster, and direct `delete` judges sharing against the whole `-a` list. Read the `cli-surface-rules` skill before changing them.
- **`skill-usage`**: Claude-global only; rejects project/`--all`.
  **`coverage`**: rejects `--all`, scope `-g` or `-p` only, and is a static
  CAPABILITY matrix — no skill names, no counts (use `doctor --verify-links`)
- **Narrowed resource args**: `check`/`apply-update` take skills ONLY,
  `enable`/`disable` take mcps ONLY — their own clap value_enums, so the
  rejection is a parse error naming the valid values
- **`inference`**: provider inventory + keyring keys. Bindings/routing are
  desktop/API-only — there is no `inference bind` here. `--api-key -` reads the
  key from stdin on `inference add` ONLY — `update` stores a literal `-`
- **`agents list|enable|disable`** reads/writes core `agent_settings` (the
  selection every fan-out reads: `-a all`, `source sync -a all`, repair, rename,
  delete-from-all); ids parse through the ONE `AgentSelection::parse`; ignores
  scope and `-a`; an unreadable agents.json is a failure for list AND for
  enable/disable (never overwritten); a toggle is reversible so there is no
  preview; `list --json` is `{configured, agents:[{id,display_name,managed}]}`
  in roster order, `configured:false` means nobody chose yet so every agent is
  managed
- **`get mcps` wire view is core `dto::McpView`**, shared with the API's
  `McpResponse`; do not add a field in only one place. `--json` rows also keep a
  legacy top-level `type` (`McpTransport::kind()`); the table shows type and
  target only, never header/env values
- Skill install is **always symlink-only**; `--universal` is a hidden no-op
- Source creds: `GIT_PASSWORD` (any host) / `GITHUB_TOKEN` (github.com https-only),
  then the user's git credential helper (`git credential fill`, https only) —
  never ask an agent to inject a token the helper already holds

## Two dispatch funnels — keep both halves in step

**Scope.** Every command is classified in one table (`scope_policy`); one that reads scope goes through one resolver (`resolve_scope`) and one sealed value (`Scope`), and `Scope::write_scope()` is the one answer to "which store does this write?". Scope flags are mutually exclusive, checked by hand in `main()`, so that rejection exits 1, not clap's 2. `write_scope()` errors on anything but one store. Never add a `_ => Global` catch-all: it turns a scope the table lets through into a silent write to the global lock. Read the `cli-surface-rules` skill before adding a subcommand.

**Early dispatch.** `check`, `repair`, `prune-lock`, `plugin`, `source`,
`apply-update`, `inference`, `agents`, `transfer`,
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
`commands/source.rs`) serves a local dir as the fetched repo. `check skills`'s JSON
shape is pinned by `check_skills_outputs_json_array`; its heal and the request
count it buys by `check_online_heals_ref_commit_so_the_next_check_skips_the_fetch`
(the hook also answers the tip preflight and logs `resolve` / `fetch` to
`AGHUB_TEST_SOURCE_FETCH_LOG`). Its commit oid must follow the served content:
a constant tip lets a test that edits the fetch root preflight-skip and read
`upToDate`.

## Anti-patterns

- **Don't** `println!` diagnostics — use `eprintln_verbose!`
- **Don't** hardcode agent id strings — use `AgentType`
